//! Deterministic fuzzing: hostile and mangled input is thrown at every parser and decoder, and at the HTTP API,
//! and the properties that must always hold are checked (no panic, no wrong data, no unbounded work, nothing written
//! outside the destination).
//!
//! Every iteration derives its own generator from (POWERDB_FUZZ_SEED, iteration), so a failure prints the numbers
//! that replay it exactly: `POWERDB_FUZZ_SEED=.. POWERDB_FUZZ_START=.. POWERDB_FUZZ_ITERS=1 cargo test --release --test fuzz <target>`.
//! The default is a few seconds per target; raise POWERDB_FUZZ_ITERS (for example 2000000) for a long local run.
//!
//! This is not coverage-guided: it mutates known-good inputs and grammar pieces rather than steering by code coverage.
//! The `fuzz/` directory holds libFuzzer targets for that, which need a nightly toolchain.

use axum::body::Body;
use axum::http::{Method, Request, header};
use http_body_util::BodyExt;
use powerdb::api::{self, Config};
use powerdb::model::Record;
use powerdb::security::{expr, magic};
use powerdb::server;
use powerdb::storage::chunker::ChunkSizes;
use powerdb::util::new_id;
use powerdb::{Db, Durability, Options};
use serde_json::json;
use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use tower::ServiceExt;

// ---- harness ----

/// Refuses any single allocation over 1 GiB, and says who asked and for what input before aborting.
/// Linux refuses absurd allocations at once (the process aborts), while macOS grants the address space lazily, so without
/// this a decoder that trusts a damaged length passes on one and kills the process on the other.
struct HugeAllocGuard;

unsafe impl std::alloc::GlobalAlloc for HugeAllocGuard {
    unsafe fn alloc(&self, l: std::alloc::Layout) -> *mut u8 {
        if l.size() > 1 << 30 {
            let input = INPUT
                .try_with(|c| c.try_borrow().map(|s| s.clone()).unwrap_or_default())
                .unwrap_or_default();
            eprintln!(
                "\nHUGE ALLOCATION: {} bytes requested\n  input: {input}\n{}",
                l.size(),
                std::backtrace::Backtrace::force_capture()
            );
            std::process::abort();
        }
        unsafe { std::alloc::System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: std::alloc::Layout) {
        unsafe { std::alloc::System.dealloc(p, l) }
    }
    unsafe fn alloc_zeroed(&self, l: std::alloc::Layout) -> *mut u8 {
        if l.size() > 1 << 30 {
            return unsafe { self.alloc(l) };
        }
        unsafe { std::alloc::System.alloc_zeroed(l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: std::alloc::Layout, n: usize) -> *mut u8 {
        if n > 1 << 30 {
            return unsafe {
                self.alloc(std::alloc::Layout::from_size_align_unchecked(n, l.align()))
            };
        }
        unsafe { std::alloc::System.realloc(p, l, n) }
    }
}

#[global_allocator]
static GUARD: HugeAllocGuard = HugeAllocGuard;

struct Rng(u64);
impl Rng {
    fn new(seed: u64, i: u64) -> Self {
        let mut r = Rng(seed ^ i.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03);
        for _ in 0..4 {
            r.next();
        }
        r
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0 | 1;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
    fn pct(&mut self, p: u64) -> bool {
        self.next() % 100 < p
    }
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }
    /// Random bytes, of a random length below `max`.
    fn rand(&mut self, max: usize) -> Vec<u8> {
        let n = self.below(max);
        self.bytes(n)
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}

thread_local! { static INPUT: RefCell<String> = const { RefCell::new(String::new()) }; }
/// Records the current input so a failure can show it.
fn note(s: impl Into<String>) {
    INPUT.with(|c| *c.borrow_mut() = s.into());
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn run(name: &str, default_iters: u64, mut f: impl FnMut(&mut Rng)) {
    let seed = env_u64("POWERDB_FUZZ_SEED", 0x5EED_0001);
    let start = env_u64("POWERDB_FUZZ_START", 0);
    let iters = env_u64("POWERDB_FUZZ_ITERS", default_iters);
    eprintln!("fuzz `{name}`: {iters} iterations from {start}, seed {seed}");
    for i in start..start + iters {
        let mut rng = Rng::new(seed, i);
        note("");
        if let Err(e) = catch_unwind(AssertUnwindSafe(|| f(&mut rng))) {
            let msg = e
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            let input = INPUT.with(|c| c.borrow().clone());
            panic!(
                "fuzz target `{name}` failed at iteration {i} (seed {seed}): {msg}\n  input: {input}\n  replay: POWERDB_FUZZ_SEED={seed} POWERDB_FUZZ_START={i} POWERDB_FUZZ_ITERS=1 cargo test --release --test fuzz {name}"
            );
        }
    }
}

/// Mutates bytes the way fuzzers do: flips, interesting values, inserts, deletes, duplicated ranges, truncation.
fn mutate(r: &mut Rng, b: &mut Vec<u8>) {
    for _ in 0..1 + r.below(4) {
        match r.below(7) {
            0 if !b.is_empty() => {
                let i = r.below(b.len());
                b[i] ^= 1 << r.below(8);
            }
            1 if !b.is_empty() => {
                let i = r.below(b.len());
                b[i] = *r.pick(&[
                    0u8, 0xff, 0x7f, 0x80, b'"', b'\\', b'(', b')', b'.', b'@', 0x0a,
                ]);
            }
            2 => {
                let i = r.below(b.len() + 1);
                let n = 1 + r.below(8);
                let ins = r.bytes(n);
                b.splice(i..i, ins);
            }
            3 if !b.is_empty() => {
                let i = r.below(b.len());
                let n = 1 + r.below((b.len() - i).min(16));
                b.drain(i..i + n);
            }
            4 if !b.is_empty() => {
                let i = r.below(b.len());
                let n = 1 + r.below((b.len() - i).min(32));
                let piece = b[i..i + n].to_vec();
                let at = r.below(b.len() + 1);
                b.splice(at..at, piece);
            }
            5 if !b.is_empty() => {
                let n = r.below(b.len());
                b.truncate(n);
            }
            _ => {
                let n = r.below(64);
                b.extend(std::iter::repeat_n(0u8, n));
            }
        }
    }
}

fn show(b: &[u8]) -> String {
    let s: String = b.iter().take(300).map(|c| format!("{c:02x}")).collect();
    format!("{} bytes, hex {s}", b.len())
}

struct Dir(PathBuf);
impl Dir {
    fn new(tag: &str) -> Self {
        Dir(std::env::temp_dir().join(format!("powerdb-fuzz-{tag}-{}", new_id())))
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn small_opts() -> Options {
    Options {
        durability: Durability::Never,
        segment_bytes: 1 << 20,
        chunk: ChunkSizes {
            min: 8 << 10,
            avg: 32 << 10,
            max: 128 << 10,
        },
        verify_reads: true,
        cache_bytes: None,
    }
}

// ---- expression language (rules and filters) ----

const EXPR_CORPUS: &[&str] = &[
    "@request.auth.id != \"\"",
    "@request.auth.id == owner_id",
    "status = 'paid' and amount > 1000",
    "(status == \"paid\" || status == \"open\") && !archived",
    "not (amount < 5)",
    "client ~ \"acme\"",
    "@request.body.owner_id == @request.auth.id",
    "record.tags.0 == \"a\"",
    "amount >= 1 && amount <= 100 && client != null",
    "",
];
const EXPR_TOKENS: &[&str] = &[
    "(",
    ")",
    "&&",
    "||",
    "!",
    "and",
    "or",
    "not",
    "==",
    "=",
    "!=",
    ">",
    "<",
    ">=",
    "<=",
    "~",
    "!~",
    "@request.auth.id",
    "@request.method",
    "@auth.role",
    "status",
    "amount",
    "id",
    "owner_id",
    "'x'",
    "\"y\"",
    "0",
    "-1",
    "3.5",
    "1e308",
    "null",
    "true",
    "false",
    ".",
    ",",
    "[",
    "]",
    "\\",
    "\"",
    "'",
    " ",
    "\t",
    "é",
    "𝄞",
    "\0",
];

fn random_expr(r: &mut Rng) -> String {
    match r.below(3) {
        0 => {
            let mut b = r.pick(EXPR_CORPUS).as_bytes().to_vec();
            mutate(r, &mut b);
            String::from_utf8_lossy(&b).into_owned()
        }
        1 => (0..1 + r.below(40))
            .map(|_| *r.pick(EXPR_TOKENS))
            .collect::<Vec<_>>()
            .join(if r.pct(50) { " " } else { "" }),
        _ => {
            // deep nesting and very long chains: the limits must hold
            let n = 1 + r.below(200);
            match r.below(3) {
                0 => format!("{}1 = 1{}", "(".repeat(n), ")".repeat(n)),
                1 => vec!["a = 1"; n].join(" && "),
                _ => format!("{}x", "!".repeat(n)),
            }
        }
    }
}

#[test]
fn expressions_never_panic_and_stay_cheap() {
    let record: Record = serde_json::from_value(json!({
        "id": "r1", "collection": "invoices", "created_at": 1, "updated_at": 2,
        "fields": {"status": "paid", "amount": 1500, "owner_id": "u1", "tags": ["a", "b"], "client": "Acme"}, "files": {}
    }))
    .unwrap();
    let auth = json!({"id": "u1", "role": "user"});
    let body = json!({"owner_id": "u1", "amount": "12"});
    run("expressions_never_panic_and_stay_cheap", 40_000, |r| {
        let src = random_expr(r);
        note(format!("{src:?}"));
        let t = std::time::Instant::now();
        let parsed = expr::parse(&src);
        if let Ok(e) = parsed {
            for ctx in [
                expr::Ctx {
                    record: Some(&record),
                    auth: Some(&auth),
                    body: Some(&body),
                    method: "POST",
                },
                expr::Ctx {
                    record: None,
                    auth: None,
                    body: None,
                    method: "GET",
                },
            ] {
                let a = e.eval(&ctx);
                assert_eq!(a, e.eval(&ctx), "evaluation is not deterministic");
                let _ = (e.matches(&ctx), e.uses_record());
            }
            // parse again: same tree
            assert_eq!(e, expr::parse(&src).unwrap());
        }
        assert!(
            t.elapsed() < std::time::Duration::from_millis(250),
            "took {:?}",
            t.elapsed()
        );
    });
}

#[test]
fn an_expression_longer_than_the_limit_is_refused() {
    let long = format!("a = 1{}", " && a = 1".repeat(expr::MAX_LEN));
    assert!(expr::parse(&long).is_err());
}

// ---- upload type checking ----

#[test]
fn magic_sniffing_never_panics_and_never_lets_an_executable_through() {
    let headers: &[&[u8]] = &[
        b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR",
        b"%PDF-1.7\n",
        b"\xff\xd8\xff\xe0\0\x10JFIF",
        b"GIF89a",
        b"PK\x03\x04",
        b"RIFF\0\0\0\0WEBP",
        b"\x7fELF\x02\x01\x01",
        b"MZ\x90\0\x03",
        b"#!/bin/sh\n",
        b"\xcf\xfa\xed\xfe",
        b"\xca\xfe\xba\xbe",
        b"<?php echo 1;",
        b"<html>",
        b"hello",
    ];
    let mimes = [
        "",
        "application/octet-stream",
        "image/png",
        "application/pdf",
        "text/plain",
        "IMAGE/PNG; charset=x",
        "x/y/z",
        "\0",
        "image/*",
    ];
    run(
        "magic_sniffing_never_panics_and_never_lets_an_executable_through",
        60_000,
        |r| {
            let mut h = r.pick(headers).to_vec();
            if r.pct(80) {
                mutate(r, &mut h);
            }
            if r.pct(30) {
                let tail = r.rand(600);
                h.extend(tail);
            }
            let mime = *r.pick(&mimes);
            note(format!("declared {mime:?}, {}", show(&h)));
            let sn = magic::sniff(&h);
            let verdict = magic::check(mime, &h);
            if sn.kind == magic::Kind::Executable {
                assert!(verdict.is_err(), "an executable was accepted");
            }
            let _ = magic::must_download(mime);
            if let Ok(stored) = verdict {
                assert!(!stored.is_empty());
            }
        },
    );
}

// ---- stored formats ----

#[test]
fn stored_formats_reject_garbage_and_round_trip_what_they_accept() {
    use powerdb::storage::model::Manifest;
    run(
        "stored_formats_reject_garbage_and_round_trip_what_they_accept",
        80_000,
        |r| {
            let mut b = if r.pct(50) {
                // a valid manifest's size is exactly the sum of its chunk lengths
                let chunks: Vec<([u8; 32], u32)> = (0..r.below(5))
                    .map(|_| ([r.next() as u8; 32], (r.next() >> 40) as u32))
                    .collect();
                let size = chunks.iter().map(|(_, l)| *l as u64).sum();
                Manifest { size, chunks }.to_bytes()
            } else {
                r.rand(300)
            };
            mutate(r, &mut b);
            note(show(&b));
            if let Some(m) = Manifest::from_bytes(&b) {
                assert_eq!(m.to_bytes(), b, "accepted bytes do not round-trip");
                let _ = (m.file_id(), m.unique_keys());
            }
        },
    );
}

// ---- tokens ----

#[test]
fn signed_urls_accept_only_what_this_server_signed() {
    use powerdb::security::secrets::Secrets;
    use powerdb::security::signed_url::UrlSigner;
    let d = Dir::new("urls");
    std::fs::create_dir_all(&d.0).unwrap();
    let signer = UrlSigner::new(&Secrets::load_or_create(&d.0).unwrap());
    let scope = "invoices/r1/document/abc";
    let good = signer.sign(scope, 2_000_000_000);
    assert!(signer.verify(scope, &good, 1_000).is_ok());
    assert!(
        signer.verify(scope, &good, 2_000_000_001).is_err(),
        "expired link accepted"
    );
    assert!(
        signer
            .verify("invoices/r2/document/abc", &good, 1_000)
            .is_err(),
        "link works for another scope"
    );
    run(
        "signed_urls_accept_only_what_this_server_signed",
        60_000,
        |r| {
            let mut t = good.clone().into_bytes();
            if r.pct(30) {
                t = r.rand(80);
            } else {
                mutate(r, &mut t);
            }
            let token = String::from_utf8_lossy(&t).into_owned();
            note(format!("{token:?}"));
            if signer.verify(scope, &token, 1_000).is_ok() {
                // base64 can carry spare bits in its last character, so a few variants decode to the same bytes; those must still be the real MAC
                let raw = |s: &str| {
                    use base64::Engine;
                    base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .decode(s)
                        .ok()
                };
                assert_eq!(
                    raw(&token),
                    raw(&good),
                    "a token that was never signed verified"
                );
            }
        },
    );
}

#[test]
fn session_tokens_accept_only_what_this_server_issued() {
    let d = Dir::new("jwt");
    let (st, _) = server::build_state(&d.0, small_opts(), Config::default()).unwrap();
    st.auth
        .register(
            "a@example.com",
            "correct horse battery",
            powerdb::auth::Role::User,
        )
        .unwrap();
    let (token, _) = st
        .auth
        .login("a@example.com", "correct horse battery")
        .unwrap();
    let who = st.auth.verify_token(&token).unwrap();
    let id = |p: &powerdb::auth::Principal| format!("{:?}", p.auth_value());
    run(
        "session_tokens_accept_only_what_this_server_issued",
        40_000,
        |r| {
            let mut t = token.clone().into_bytes();
            match r.below(4) {
                0 => t = r.rand(200),
                1 => {
                    // swap or drop a segment
                    let parts: Vec<&str> = token.split('.').collect();
                    let mut p: Vec<String> = parts.iter().map(|s| s.to_string()).collect();
                    let i = r.below(3);
                    p[i] = String::from_utf8_lossy(&r.rand(40)).into_owned();
                    t = p.join(".").into_bytes();
                }
                _ => mutate(r, &mut t),
            }
            let s = String::from_utf8_lossy(&t).into_owned();
            note(format!("{s:?}"));
            if let Ok(p) = st.auth.verify_token(&s) {
                assert_eq!(
                    id(&p),
                    id(&who),
                    "a forged token was accepted as someone else"
                );
            }
        },
    );
}

// ---- schema validation ----

#[test]
fn record_validation_never_panics_on_any_json() {
    use powerdb::model::{CollectionDef, FieldDef, FieldKind, Rules};
    let f = |name: &str, kind, required, max: Option<u64>, mime: Vec<&str>| FieldDef {
        name: name.into(),
        kind,
        required,
        max,
        mime: mime.into_iter().map(String::from).collect(),
        relation: None,
    };
    let def = CollectionDef {
        name: "t".into(),
        fields: vec![
            f("a", FieldKind::Text, true, Some(10), vec![]),
            f("n", FieldKind::Number, false, None, vec![]),
            f("doc", FieldKind::File, false, Some(1000), vec!["image/*"]),
        ],
        rules: Rules::default(),
        encrypt: false,
        created_at: 0,
    };
    let leaves = [
        json!(null),
        json!(true),
        json!(1),
        json!(-1.5),
        json!(1e300),
        json!(u64::MAX),
        json!(""),
        json!("x"),
        json!("é".repeat(40)),
        json!([]),
        json!({}),
        json!([1, [2]]),
    ];
    run("record_validation_never_panics_on_any_json", 60_000, |r| {
        let mut m = serde_json::Map::new();
        for k in ["a", "n", "doc", "zzz", "", "__proto__", "a.b"] {
            if r.pct(50) {
                m.insert(k.into(), r.pick(&leaves).clone());
            }
        }
        note(format!("{m:?}"));
        let files = std::collections::BTreeMap::new();
        let _ = def.validate(&m, &files, r.pct(50));
        for name in [
            "",
            "a",
            "A",
            "a-b",
            "x".repeat(200).as_str(),
            "é",
            "..",
            "a\0b",
        ] {
            let _ = powerdb::model::valid_name(name);
        }
    });
}

// ---- configuration ----

#[test]
fn settings_files_never_panic_the_parser() {
    run("settings_files_never_panic_the_parser", 30_000, |r| {
        let mut b = powerdb::config::TEMPLATE.as_bytes().to_vec();
        if r.pct(30) {
            b = r.rand(200);
        }
        mutate(r, &mut b);
        let s = String::from_utf8_lossy(&b).into_owned();
        note(format!("{s:?}"));
        let _ = powerdb::config::FileConfig::parse(&s);
    });
}

// ---- damaged storage ----

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let (a, b) = (e.path(), to.join(e.file_name()));
        if e.file_type().unwrap().is_dir() {
            copy_dir(&a, &b);
        } else if e.file_name() != "LOCK" {
            std::fs::copy(&a, &b).unwrap();
        }
    }
}

fn files_under(root: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(root).unwrap() {
        let e = e.unwrap();
        if e.file_type().unwrap().is_dir() {
            files_under(&e.path(), out);
        } else if e.file_name() != "LOCK" {
            out.push(e.path());
        }
    }
}

/// The property that matters most for a store: damage may make a read fail, but it may never make it return different bytes.
#[test]
fn damaged_storage_fails_loudly_and_never_returns_wrong_data() {
    use powerdb::model::{CollectionDef, FieldDef, FieldKind, Rules};
    use std::io::Cursor;
    let pristine = Dir::new("pristine");
    let mut expected: Vec<(String, Vec<u8>)> = vec![];
    {
        let (st, _) = server::build_state(&pristine.0, small_opts(), Config::default()).unwrap();
        let db = st.db.clone();
        db.create_collection(CollectionDef {
            name: "t".into(),
            fields: vec![FieldDef {
                name: "doc".into(),
                kind: FieldKind::File,
                required: false,
                max: None,
                mime: vec![],
                relation: None,
            }],
            rules: Rules::default(),
            encrypt: true,
            created_at: 0,
        })
        .unwrap();
        for seed in 0..4u64 {
            let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
            let bytes: Vec<u8> = (0..300_000)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    (x >> 24) as u8
                })
                .collect();
            let p = db
                .stage_file(
                    "t",
                    "application/octet-stream",
                    Some("f"),
                    Cursor::new(bytes.clone()),
                )
                .unwrap();
            let rec = db
                .create("t", serde_json::Map::new(), vec![("doc".into(), p)])
                .unwrap();
            expected.push((rec.id, bytes));
        }
        db.flush().unwrap();
    }
    let mut targets = vec![];
    files_under(&pristine.0, &mut targets);
    targets.retain(|p| !p.ends_with("secret.key"));
    targets.sort(); // read_dir order differs between file systems; the same iteration must pick the same file everywhere
    run(
        "damaged_storage_fails_loudly_and_never_returns_wrong_data",
        150,
        |r| {
            let work = Dir::new("work");
            copy_dir(&pristine.0, &work.0);
            let rel = r
                .pick(&targets)
                .strip_prefix(&pristine.0)
                .unwrap()
                .to_path_buf();
            let victim = work.0.join(&rel);
            let mut bytes = std::fs::read(&victim).unwrap();
            let before = bytes.len();
            mutate(r, &mut bytes);
            std::fs::write(&victim, &bytes).unwrap();
            note(format!(
                "{} {before} -> {} bytes",
                rel.display(),
                bytes.len()
            ));

            // A damaged secret would make every encrypted read fail, which is also loud; the same code path covers it.
            let opened = catch_unwind(AssertUnwindSafe(|| {
                server::build_state(&work.0, small_opts(), Config::default())
            }));
            let Ok(Ok((st, _))) = opened else { return }; // refusing to open, even by panicking inside the storage library, is not silent corruption
            for (id, want) in &expected {
                let got = st.db.get("t", id).and_then(|rec| match rec {
                    Some(rec) => st
                        .db
                        .read_file(&powerdb::util::from_hex_32(&rec.files["doc"].id).unwrap()),
                    None => Err(powerdb::Error::NotFound("gone".into())),
                });
                if let Ok(bytes) = got {
                    assert_eq!(
                        &bytes,
                        want,
                        "damage to {} produced different bytes without an error",
                        rel.display()
                    );
                }
            }
            let _ = st.db.verify(true);
        },
    );
}

// ---- backups ----

#[test]
fn a_tampered_backup_never_writes_outside_its_destination() {
    use powerdb::ops;
    let src = Dir::new("bsrc");
    ops::init(&src.0).unwrap();
    let backup = Dir::new("bak");
    ops::backup(&src.0, &backup.0, false).unwrap();
    let manifest = std::fs::read(backup.0.join("BACKUP.json")).unwrap();
    let evil = [
        "../escape",
        "..\\escape",
        "/abs/escape",
        "a/../../escape",
        "",
        "a//b",
        "C:\\x",
        "meta/../../escape",
    ];
    let parent = Dir::new("parent");
    std::fs::create_dir_all(&parent.0).unwrap();
    run(
        "a_tampered_backup_never_writes_outside_its_destination",
        120,
        |r| {
            let work = Dir::new("bwork");
            copy_dir(&backup.0, &work.0);
            let mut m = manifest.clone();
            if r.pct(50) {
                // point one manifest path at somewhere unsafe
                let text = String::from_utf8_lossy(&m).into_owned();
                if let Some(i) = text.find("\"path\": \"") {
                    let start = i + 9;
                    let end = start + text[start..].find('"').unwrap();
                    m = format!("{}{}{}", &text[..start], r.pick(&evil), &text[end..]).into_bytes();
                }
            } else {
                mutate(r, &mut m);
            }
            std::fs::write(work.0.join("BACKUP.json"), &m).unwrap();
            note(String::from_utf8_lossy(&m).into_owned());
            let dest = parent.0.join(format!("restored-{}", new_id()));
            let _ = ops::restore(&work.0, &dest);
            let mut entries: Vec<String> = std::fs::read_dir(&parent.0)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            entries.retain(|n| !n.starts_with("restored-"));
            assert!(
                entries.is_empty(),
                "restore wrote outside its destination: {entries:?}"
            );
            assert!(!parent.0.parent().unwrap().join("escape").exists());
            let _ = std::fs::remove_dir_all(&dest);
        },
    );
}

// ---- the HTTP API ----

#[test]
fn random_http_requests_never_panic_and_never_cause_a_server_error() {
    let d = Dir::new("http");
    let (st, boot) = server::build_state(
        &d.0,
        small_opts(),
        Config {
            login_attempts_per_minute: 1_000_000,
            ..Config::default()
        },
    )
    .unwrap();
    let admin = boot.admin_key.unwrap();
    let app = api::router(st.clone());
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let call = |method: &str,
                uri: &str,
                token: Option<&str>,
                ct: Option<&str>,
                body: Vec<u8>,
                extra: Vec<(String, String)>| {
        let mut b =
            Request::builder().method(Method::from_bytes(method.as_bytes()).unwrap_or(Method::GET));
        match uri.parse::<axum::http::Uri>() {
            Ok(u) => b = b.uri(u),
            Err(_) => return None, // not a request a client could even send
        }
        if let Some(t) = token {
            b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
        }
        if let Some(c) = ct {
            b = b.header(header::CONTENT_TYPE, c);
        }
        for (k, v) in extra {
            b = b.header(k, v);
        }
        let req = b.body(Body::from(body)).ok()?;
        Some(rt.block_on(async {
            let r = app.clone().oneshot(req).await.unwrap();
            let status = r.status();
            let _ = r.into_body().collect().await;
            status
        }))
    };
    // a collection with a mixture of open and closed rules, and one record to find
    let def = json!({"name": "docs", "rules": {"list": "", "view": "", "create": "@request.auth.id != \"\"", "update": "@request.auth.id == owner_id"},
        "fields": [{"name": "title", "kind": "text", "required": true}, {"name": "owner_id", "kind": "text"}, {"name": "n", "kind": "number"},
                   {"name": "file", "kind": "file", "max": 100000, "mime": ["image/*", "application/pdf"]}]});
    assert_eq!(
        call(
            "POST",
            "/api/v1/collections",
            Some(&admin),
            Some("application/json"),
            def.to_string().into_bytes(),
            vec![]
        )
        .unwrap()
        .as_u16(),
        201
    );
    call(
        "POST",
        "/api/v1/collections/docs/records",
        Some(&admin),
        Some("application/json"),
        br#"{"title":"seed","owner_id":"u1","n":5}"#.to_vec(),
        vec![],
    );

    let paths = [
        "/api/v1/collections",
        "/api/v1/collections/docs",
        "/api/v1/collections/docs/records",
        "/api/v1/collections/nope/records",
        "/api/v1/users",
        "/api/v1/keys",
        "/api/v1/stats",
        "/api/v1/metrics",
        "/api/v1/healthz",
        "/api/v1/auth/login",
        "/api/v1/auth/register",
        "/api/v1/auth/me",
        "/api/v1/files/docs/x/file",
        "/api/v1/maintenance/gc",
        "/",
        "/index.html",
        "/assets/x.js",
        "/%00",
        "/api/v1/%2e%2e/%2e%2e/etc/passwd",
        "/api/v1/collections/docs/records/",
    ];
    let methods = [
        "GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "TRACE", "BREW",
    ];
    let params = [
        "filter", "sort", "page", "per_page", "perPage", "expand", "fields", "token", "expires",
        "q",
    ];
    let cts = [
        "application/json",
        "multipart/form-data; boundary=zz",
        "multipart/form-data",
        "text/plain",
        "application/x-www-form-urlencoded",
        "",
    ];
    let jsons = [
        r#"{"title":"t"}"#,
        r#"{"email":"a@b.co","password":"x"}"#,
        r#"{"name":"x","fields":[]}"#,
        r#"[1,2"#,
        r#"{"title":{"a":[1,2,{"b":null}]}}"#,
        "null",
        "",
    ];
    let mp = b"--zz\r\nContent-Disposition: form-data; name=\"data\"\r\nContent-Type: application/json\r\n\r\n{\"title\":\"m\"}\r\n--zz\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.png\"\r\nContent-Type: image/png\r\n\r\n\x89PNG\r\n\x1a\n0000\r\n--zz--\r\n";

    run(
        "random_http_requests_never_panic_and_never_cause_a_server_error",
        4_000,
        |r| {
            let mut uri = r.pick(&paths).to_string();
            if r.pct(60) {
                uri.push('?');
                let n = 1 + r.below(3);
                let q: Vec<String> = (0..n)
                    .map(|_| {
                        let v = if r.pct(60) {
                            random_expr(r)
                        } else {
                            String::from_utf8_lossy(&r.rand(12)).into_owned()
                        };
                        let enc: String = v.bytes().map(|c| format!("%{c:02X}")).collect();
                        format!("{}={enc}", r.pick(&params))
                    })
                    .collect();
                uri.push_str(&q.join("&"));
            }
            let mut body = if r.pct(50) {
                r.pick(&jsons).as_bytes().to_vec()
            } else {
                mp.to_vec()
            };
            if r.pct(60) {
                mutate(r, &mut body);
            }
            let token = match r.below(4) {
                0 => Some(admin.clone()),
                1 => Some(
                    String::from_utf8_lossy(&r.rand(40))
                        .into_owned()
                        .replace(['\r', '\n'], ""),
                ),
                _ => None,
            };
            let ct = *r.pick(&cts);
            let method = *r.pick(&methods);
            note(format!(
                "{method} {uri} ct={ct:?} token={} body={}",
                token.is_some(),
                show(&body)
            ));
            // an administrator may legitimately change or remove things, so the destructive routes are only tried anonymously
            let destructive = method == "DELETE"
                || uri.contains("maintenance")
                || uri.contains("/keys")
                || uri.contains("/users");
            let token = if destructive && token.as_deref() == Some(admin.as_str()) {
                None
            } else {
                token
            };
            let Some(status) = call(
                method,
                &uri,
                token.as_deref(),
                (!ct.is_empty()).then_some(ct),
                body,
                vec![],
            ) else {
                return;
            };
            assert!(!status.is_server_error(), "{status} for {method} {uri}");
            if token.is_none() {
                let writes = matches!(method, "POST" | "PUT" | "PATCH" | "DELETE")
                    && !uri.contains("/auth/");
                assert!(
                    !(writes && status.is_success()),
                    "an anonymous write succeeded: {status}"
                );
            }
        },
    );
    // the server is still healthy and the seed record is intact after everything above
    assert_eq!(
        call("GET", "/api/v1/healthz", None, None, vec![], vec![])
            .unwrap()
            .as_u16(),
        200
    );
    let db: &Db = &st.db;
    assert!(db.verify(true).unwrap().problems.is_empty());
}
