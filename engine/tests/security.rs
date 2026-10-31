use powerdb::model::{CollectionDef, FieldDef, FieldKind, Record, Rules};
use powerdb::security::crypto::ChaChaCodec;
use powerdb::security::expr::{parse, Ctx};
use powerdb::security::magic::{check, must_download, sniff, Kind};
use powerdb::security::secrets::Secrets;
use powerdb::security::signed_url::UrlSigner;
use powerdb::storage::chunker::ChunkSizes;
use powerdb::storage::ChunkCodec;
use powerdb::util::{from_hex_32, new_id};
use powerdb::{Db, Durability, Options};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::io::Cursor;
use std::sync::Arc;

fn rec(fields: Value) -> Record {
    Record { id: "r1".into(), collection: "invoices".into(), created_at: 1000, updated_at: 2000, fields: fields.as_object().unwrap().clone(), files: BTreeMap::new() }
}

fn eval(src: &str, r: &Record, auth: Option<Value>) -> bool {
    let e = parse(src).unwrap_or_else(|e| panic!("{src}: {e}"));
    e.matches(&Ctx { record: Some(r), auth: auth.as_ref(), body: None, method: "GET" })
}

#[test]
fn filters_read_naturally_in_both_spellings() {
    let r = rec(json!({"status": "paid", "amount": 4500, "client": "Acme Corp", "tags": ["a", "b"], "owner_id": "u1"}));
    for src in [
        r#"status == "paid" && amount >= 1000"#,
        r#"(status='paid' and amount>1000)"#,
        r#"status = 'paid' AND amount > 1000"#,
        r#"amount >= 1000 && !(status == "void")"#,
        r#"client ~ "acme""#,
        r#"status in ["paid", "sent"]"#,
        r#"tags ~ "b""#,
        r#"not status == "void""#,
        r#"id == "r1" && created_at < 1500 && record.owner_id == "u1""#,
        r#"missing == null"#,
        r#"amount == 4500.0"#,
        r#"status != "paid" || amount == 4500"#,
    ] {
        assert!(eval(src, &r, None), "{src}");
    }
    for src in [r#"status == "void""#, r#"amount < 1000"#, r#"client !~ "acme""#, r#"status in ["sent"]"#, r#"missing == "x""#, r#"amount == "4500""#, r#"status > 5"#, r#"nope"#] {
        assert!(!eval(src, &r, None), "{src}");
    }
}

#[test]
fn precedence_is_not_then_and_then_or() {
    let r = rec(json!({"a": true, "b": false, "c": true}));
    assert!(eval("a || b && b", &r, None)); // a || (b && b)
    assert!(!eval("(a || b) && b", &r, None));
    assert!(eval("!b && c", &r, None));
    assert!(!eval("!(b || c)", &r, None));
}

#[test]
fn access_rules_see_the_signed_in_user() {
    let r = rec(json!({"owner_id": "u1", "public": false}));
    let me = Some(json!({"id": "u1", "role": "member"}));
    let other = Some(json!({"id": "u2"}));
    assert!(eval("@request.auth.id == record.owner_id", &r, me.clone()));
    assert!(eval("@auth.id == owner_id", &r, me.clone()));
    assert!(!eval("@request.auth.id == record.owner_id", &r, other.clone()));
    assert!(!eval("@request.auth.id == record.owner_id", &r, None), "no user: auth.id is null, owner_id is not");
    assert!(eval(r#"@request.auth.id != """#, &r, other), "a signed-in user passes the 'anyone signed in' rule");
    assert!(!eval(r#"@request.auth.id != """#, &r, None), "an anonymous caller must NOT pass the 'anyone signed in' rule");
    assert!(!eval(r#"@request.auth.id != null"#, &r, None));
    assert!(eval(r#"@request.auth.id == """#, &r, None), "and can ask whether the caller is anonymous");
    assert!(eval("public || @request.auth.role == 'member'", &r, me));
    // an unauthenticated caller must not satisfy a rule just because both sides are missing: a record without an owner is not everybody's record
    let r2 = rec(json!({}));
    assert!(!eval("@request.auth.id == record.owner_id", &r2, None));
    assert!(!eval("record.owner_id == @request.auth.id", &r2, None));
    assert!(eval("@request.auth.id != record.owner_id", &r2, None));
    assert!(!eval("@request.auth.id == record.owner_id", &r2, Some(json!({"id": "u1"}))), "a real user does not own an ownerless record either");
    assert!(eval("owner_id == ''", &r2, None) && eval("owner_id == null", &r2, None), "missing means unset");
}

#[test]
fn hostile_text_is_data_never_syntax() {
    let r = rec(json!({"name": "x\" || true || \"", "n": 1}));
    // a quote inside a string stays inside the string
    assert!(!eval(r#"name == "x\" || true || \"y""#, &r, None));
    assert!(eval(r#"name == "x\" || true || \"""#, &r, None));
    for bad in ["status == ", "status ==== 1", "(a", "a)", "a && ", "\"unterminated", "a ; b", "a == 1 2", "@", "@.x", "a.", "[1,", "a in [b]", "\u{0}", "a == '\\q'", "x = 1; DROP TABLE users", ""] {
        assert!(parse(bad).is_err(), "should reject {bad:?}");
    }
}

#[test]
fn expressions_are_bounded() {
    assert!(parse(&format!("a == '{}'", "x".repeat(3000))).is_err(), "too long");
    assert!(parse(&format!("{}a{}", "(".repeat(100), ")".repeat(100))).is_err(), "too deep");
    assert!(parse(&format!("{}a{}", "(".repeat(20), ")".repeat(20))).is_ok());
    assert!(parse(&format!("{}a", "!".repeat(100))).is_err(), "too many negations");
    let wide = (0..600).map(|i| format!("a=={i}")).collect::<Vec<_>>().join(" || ");
    assert!(parse(&wide).is_err(), "too many nodes");
}

#[test]
fn signed_urls_expire_and_cannot_be_moved() {
    let s = UrlSigner::new(&Secrets::from_bytes([7; 32]));
    let scope = "invoices/r1/document/abc";
    let t = s.sign(scope, 1_000_060);
    assert!(s.verify(scope, &t, 1_000_000).is_ok());
    assert!(s.verify(scope, &t, 1_000_060).is_ok());
    assert!(s.verify(scope, &t, 1_000_061).is_err(), "expired");
    assert!(s.verify("invoices/r2/document/abc", &t, 1_000_000).is_err(), "other record");
    assert!(s.verify("invoices/r1/document/abd", &t, 1_000_000).is_err(), "other file");
    let other_key = UrlSigner::new(&Secrets::from_bytes([8; 32]));
    assert!(other_key.verify(scope, &t, 1_000_000).is_err(), "other server secret");
    // extending the expiry by editing the token breaks the MAC
    use base64::Engine;
    let mut raw = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(&t).unwrap();
    raw[7] = raw[7].wrapping_add(200);
    assert!(s.verify(scope, &base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw), 1_000_000).is_err());
    for junk in ["", "x", "AAAA", &"A".repeat(200), "!!!!"] {
        assert!(s.verify(scope, junk, 0).is_err());
    }
}

#[test]
fn the_cipher_authenticates_what_it_decrypts() {
    let c = ChaChaCodec::new(&Secrets::from_bytes([1; 32]));
    let key = [9u8; 32];
    let msg = b"the quick brown fox jumps over the lazy dog".repeat(100);
    let ct = c.encode(&key, &msg).unwrap();
    assert!(ct.len() > msg.len() && !ct.windows(20).any(|w| w == &msg[..20]));
    assert_eq!(c.decode(&key, &ct).unwrap(), msg);
    assert_ne!(c.encode(&key, &msg).unwrap(), ct, "a fresh nonce each time");
    let mut bad = ct.clone();
    bad[40] ^= 1;
    assert!(c.decode(&key, &bad).is_err(), "tampered");
    assert!(c.decode(&[8u8; 32], &ct).is_err(), "bound to its chunk key");
    assert!(ChaChaCodec::new(&Secrets::from_bytes([2; 32])).decode(&key, &ct).is_err(), "wrong secret");
    assert!(c.decode(&key, &ct[..20]).is_err(), "truncated");
}

#[test]
fn magic_bytes_block_disguised_files() {
    let png = [&[0x89u8, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A][..], &[0u8; 64]].concat();
    let elf = [&b"\x7fELF"[..], &[2, 1, 1, 0], &[0u8; 60]].concat();
    let exe = [&b"MZ"[..], &[0x90, 0, 3, 0], &[0u8; 60]].concat();
    let pdf = b"%PDF-1.7\n1 0 obj\n<<>>\nendobj\n".to_vec();
    let zip = [&b"PK\x03\x04"[..], &[0u8; 30]].concat();
    let junk: Vec<u8> = (0..200u32).map(|i| (i * 37 % 251) as u8 | 0x80).collect();

    assert_eq!(check("image/png", &png).unwrap(), "image/png");
    assert_eq!(check("IMAGE/PNG; charset=binary", &png).unwrap(), "image/png");
    assert!(check("image/png", &exe).unwrap_err().to_string().contains("executable"));
    assert!(check("image/png", &elf).is_err());
    assert!(check("application/octet-stream", &elf).is_err(), "executables are refused whatever they are called");
    assert!(check("text/plain", b"#!/bin/sh\nrm -rf /\n").is_err(), "scripts with a shebang");
    assert!(check("image/png", &pdf).unwrap_err().to_string().contains("content looks like application/pdf"));
    assert!(check("application/pdf", &png).is_err());
    assert_eq!(check("application/pdf", &pdf).unwrap(), "application/pdf");
    assert!(check("image/jpeg", &junk).is_err(), "bytes that are no image at all");
    assert_eq!(check("application/vnd.openxmlformats-officedocument.wordprocessingml.document", &zip).unwrap(), "application/vnd.openxmlformats-officedocument.wordprocessingml.document");
    assert_eq!(check("application/octet-stream", &png).unwrap(), "image/png", "an undeclared type is filled in from the content");
    assert_eq!(check("application/json", br#"{"a": 1}"#).unwrap(), "application/json");
    assert!(check("application/json", &png).is_err());
    assert!(check("text/csv", &junk).is_err(), "binary declared as text");
    assert_eq!(check("application/x-custom", &junk).unwrap(), "application/x-custom", "a type we have no signature for is trusted once the executable gate has passed");
    assert!(check("image/png", b"").is_err(), "an empty file is not a PNG");
    assert_eq!(check("text/plain", b"").unwrap(), "text/plain");
    assert_eq!(sniff(b"<svg xmlns='http://www.w3.org/2000/svg'></svg>").kind, Kind::Text);
    assert!(must_download("image/svg+xml") && must_download("text/html") && must_download("application/x-custom"));
    assert!(!must_download("image/png") && !must_download("application/pdf"));
}

// ---- encryption at rest, end to end ----

struct Dir(std::path::PathBuf);
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn private_db(dir: &Dir, secret: u8) -> Db {
    let o = Options { durability: Durability::Never, segment_bytes: 8 << 20, chunk: ChunkSizes { min: 16 << 10, avg: 64 << 10, max: 256 << 10 }, verify_reads: true, cache_bytes: None };
    let db = Db::open(&dir.0, o).unwrap();
    db.register_codec(Arc::new(ChaChaCodec::new(&Secrets::from_bytes([secret; 32]))));
    let f = |name: &str, kind| FieldDef { name: name.into(), kind, required: false, max: None, mime: vec![], relation: None };
    for (name, encrypt) in [("secrets", true), ("pubdocs", false)] {
        if db.collection(name).is_err() {
            db.create_collection(CollectionDef { name: name.into(), fields: vec![f("doc", FieldKind::File), f("note", FieldKind::Text)], rules: Rules::default(), encrypt, created_at: 0 }).unwrap();
        }
    }
    db
}

#[test]
fn private_collections_store_only_ciphertext_and_still_dedup() {
    let dir = Dir(std::env::temp_dir().join(format!("powerdb-sec-{}", new_id())));
    let marker = b"TOP-SECRET-MARKER-0123456789-TOP-SECRET-MARKER".to_vec();
    let mut plain = Vec::new();
    while plain.len() < 600_000 {
        plain.extend_from_slice(&marker);
        plain.extend_from_slice(&(plain.len() as u64).to_le_bytes());
    }
    let db = private_db(&dir, 5);
    let stage = |c: &str| db.stage_file(c, "application/octet-stream", None, Cursor::new(plain.clone())).unwrap();
    let a = db.create("secrets", Map::new(), vec![("doc".into(), stage("secrets"))]).unwrap();
    let before = db.stats().unwrap().log_bytes;
    let again = stage("secrets");
    assert_eq!(again.new_chunks(), 0, "encrypted chunks deduplicate too");
    drop(again);
    let b = db.create("pubdocs", Map::new(), vec![("doc".into(), stage("pubdocs"))]).unwrap();
    assert!(db.stats().unwrap().log_bytes > before + 500_000, "the same bytes under the other codec are a different stored object");
    db.flush().unwrap();

    // the log holds the plaintext exactly once (the public copy), never in the private one
    let mut hits = 0;
    for e in std::fs::read_dir(dir.0.join("blobs")).unwrap() {
        let raw = std::fs::read(e.unwrap().path()).unwrap();
        hits += raw.windows(marker.len()).filter(|w| *w == &marker[..]).count();
    }
    assert!(hits > 0, "the public copy is stored in the clear");
    let per_copy = plain.len() / (marker.len() + 8);
    assert!(hits <= per_copy + 8, "{hits} marker hits, a single plaintext copy has about {per_copy}");

    let read = |db: &Db, r: &Record| db.read_file(&from_hex_32(&r.files["doc"].id).unwrap());
    assert_eq!(read(&db, &a).unwrap(), plain);
    assert_eq!(read(&db, &b).unwrap(), plain);
    drop(db);

    // with the wrong secret the private file is unreadable, the public one is not affected
    let db2 = private_db(&dir, 6);
    assert!(read(&db2, &a).unwrap_err().to_string().contains("authentication"));
    assert_eq!(read(&db2, &b).unwrap(), plain);
    // a corrupted private chunk is caught by the cipher's tag
    drop(db2);
    let db3 = private_db(&dir, 5);
    assert_eq!(read(&db3, &a).unwrap(), plain);
}

#[test]
fn a_private_collection_without_a_key_refuses_to_store_files() {
    let dir = Dir(std::env::temp_dir().join(format!("powerdb-sec-{}", new_id())));
    let db = Db::open(&dir.0, Options { durability: Durability::Never, ..Options::default() }).unwrap();
    db.create_collection(CollectionDef { name: "p".into(), fields: vec![FieldDef { name: "doc".into(), kind: FieldKind::File, required: false, max: None, mime: vec![], relation: None }], rules: Rules::default(), encrypt: true, created_at: 0 }).unwrap();
    let e = db.stage_file("p", "x/y", None, Cursor::new(vec![1, 2, 3])).err().unwrap().to_string();
    assert!(e.contains("no codec is registered"), "{e}");
}

#[test]
fn secrets_derive_independent_keys_and_persist() {
    let dir = Dir(std::env::temp_dir().join(format!("powerdb-sec-{}", new_id())));
    let s = Secrets::load_or_create(&dir.0).unwrap();
    let s2 = Secrets::load_or_create(&dir.0).unwrap();
    assert_eq!(s.derive("a"), s2.derive("a"));
    assert_ne!(s.derive("a"), s.derive("b"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.0.join("secret.key")).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the secret file is private to its owner");
    }
}
