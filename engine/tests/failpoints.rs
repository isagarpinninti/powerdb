//! Recovery after a crash at every named point in the commit, collection and compaction paths.
//!
//! Run with `cargo test --release --features failpoints --test failpoints`. A child process runs a fixed workload and is aborted at a chosen point
//! (see `src/failpoint.rs`). The parent then reopens the directory and requires, for each point:
//!   * `verify` (every chunk read and hashed, every reference count recomputed) finds nothing wrong;
//!   * everything the child acknowledged is present with the right bytes (a write in flight may be present or absent, never half there);
//!   * the database still works: it accepts a write, collects and compacts, and is still consistent afterwards.
//!
//! A point the workload never reaches fails the test, so a point cannot silently stop being covered.

use powerdb::model::{CollectionDef, FieldDef, FieldKind, Rules};
use powerdb::storage::chunker::ChunkSizes;
use powerdb::util::{from_hex_32, new_id};
use powerdb::{Db, Durability, Options};
use serde_json::{Map, json};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Cursor, Write};
use std::process::{Command, Stdio};

type Content = (u8, u8);

fn bytes_of((base, variant): Content) -> Vec<u8> {
    let mut x = (base as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut v: Vec<u8> = (0..150_000)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect();
    if variant > 0 {
        let at = 40_000 + variant as usize * 20_000;
        for b in &mut v[at..at + 64] {
            *b = b.wrapping_add(variant);
        }
    }
    v
}

fn options() -> Options {
    Options {
        // every commit is fsynced: the property under test is that an acknowledged write survives
        durability: Durability::Commit,
        segment_bytes: 512 << 10, // small, so segments seal and compaction has work
        chunk: ChunkSizes {
            min: 8 << 10,
            avg: 32 << 10,
            max: 128 << 10,
        },
        verify_reads: true,
        cache_bytes: None,
    }
}

fn schema() -> CollectionDef {
    CollectionDef {
        name: "docs".into(),
        fields: vec![
            FieldDef {
                name: "n".into(),
                kind: FieldKind::Number,
                required: false,
                max: None,
                mime: vec![],
                relation: None,
            },
            FieldDef {
                name: "a".into(),
                kind: FieldKind::File,
                required: false,
                max: None,
                mime: vec![],
                relation: None,
            },
        ],
        rules: Rules::default(),
        encrypt: false,
        created_at: 0,
    }
}

fn stage(db: &Db, c: Content) -> powerdb::storage::PendingFile {
    db.stage_file(
        "docs",
        "application/octet-stream",
        Some("f.bin"),
        Cursor::new(bytes_of(c)),
    )
    .unwrap()
}

fn fields(n: i64) -> Map<String, serde_json::Value> {
    json!({ "n": n }).as_object().unwrap().clone()
}

/// Runs only as the child of `every_failpoint_recovers`; a normal run returns at once.
#[test]
fn workload_child() {
    let Ok(dir) = std::env::var("POWERDB_FP_DIR") else {
        return;
    };
    let db = Db::open(&dir, options()).unwrap();
    db.create_collection(schema()).unwrap();
    db.create_bucket(powerdb::storage::objects::BucketDef {
        name: "b".into(),
        versioning: false,
        encrypt: false,
        created_at: 0,
    })
    .unwrap();
    {
        use powerdb::types::Type;
        use powerdb::types::schema::{Change, TableDef};
        let mut t = TableDef::new("tt").unwrap();
        for (n, ty) in [("k", Type::I64), ("v", Type::Text)] {
            t = t
                .apply(
                    &Change::AddColumn {
                        name: n.into(),
                        ty,
                        nullable: false,
                        default: None,
                    },
                    true,
                )
                .unwrap()
                .0;
        }
        t.set_primary_key(&["k"]).unwrap();
        db.create_table(t).unwrap();
        // a child table: a unique tag, and a foreign key to `tt` that cascades, so a delete of a `tt` row also removes (and un-indexes) its child
        let mut c = TableDef::new("tc").unwrap();
        for (n, ty, nullable) in [
            ("k", Type::I64, false),
            ("tk", Type::I64, true),
            ("tag", Type::Text, true),
            ("doc", Type::File, true),
        ] {
            c = c
                .apply(
                    &Change::AddColumn {
                        name: n.into(),
                        ty,
                        nullable,
                        default: None,
                    },
                    true,
                )
                .unwrap()
                .0;
        }
        c.set_primary_key(&["k"]).unwrap();
        db.create_table(c).unwrap();
        db.alter_table(
            "tc",
            &Change::AddUnique {
                name: "tc_tag".into(),
                columns: vec!["tag".into()],
            },
        )
        .unwrap();
        db.alter_table(
            "tc",
            &Change::AddIndex {
                name: "tc_tk_tag".into(),
                columns: vec!["tk".into(), "tag".into()],
            },
        )
        .unwrap();
        db.alter_table(
            "tc",
            &Change::AddForeignKey {
                name: "tc_tt".into(),
                columns: vec!["tk".into()],
                ref_table: "tt".into(),
                ref_table_id: 0,
                ref_pk_types: vec![],
                on_delete: powerdb::types::schema::OnDelete::Cascade,
            },
        )
        .unwrap();
    }
    let mut out = std::io::stdout();
    let mut say = |line: String| {
        writeln!(out, "{line}").unwrap();
        out.flush().unwrap();
    };
    let mut live: Vec<String> = Vec::new();
    for i in 0..45u8 {
        let c: Content = (i % 13, i % 3);
        let rec = db
            .create("docs", fields(i as i64), vec![("a".into(), stage(&db, c))])
            .unwrap();
        say(format!("ACK create {} {} {}", rec.id, c.0, c.1));
        live.push(rec.id);
        if i % 3 == 2 && live.len() > 2 {
            let id = live.remove(0);
            say(format!("TRY delete {id}"));
            db.delete("docs", &id).unwrap();
            say(format!("ACK delete {id}"));
        }
        if i % 4 == 3 {
            let id = live[live.len() / 2].clone();
            let nc: Content = ((i + 5) % 13, (i + 2) % 3);
            say(format!("TRY update {id} {} {}", nc.0, nc.1));
            db.update(
                "docs",
                &id,
                Map::new(),
                vec![("a".into(), stage(&db, nc))],
                &[],
            )
            .unwrap();
            say(format!("ACK update {id} {} {}", nc.0, nc.1));
        }
        // a typed table row: inserted, then changed, then deleted a few iterations later
        {
            use powerdb::types::Value as V;
            let k = i as i64;
            say(format!("TRY trow {k} a"));
            db.insert_row("tt", &[("k", V::I64(k)), ("v", V::Text("a".into()))])
                .unwrap();
            say(format!("ACK trow {k} a"));
            // each child holds a file (four distinct contents between them, so some are shared), which goes with the child
            let file = db
                .stage_table_file(
                    "tc",
                    "application/octet-stream",
                    None,
                    Cursor::new(bytes_of(((k % 4) as u8, 0))),
                )
                .unwrap();
            db.insert_row_with(
                "tc",
                &[
                    ("k", V::I64(k)),
                    ("tk", V::I64(k)),
                    ("tag", V::Text(format!("t{k}"))),
                    ("doc", V::File(file.file_value())),
                ],
                &[&file],
            )
            .unwrap();
            if i % 3 == 2 {
                let j = k - 1;
                say(format!("TRY tupd {j} b"));
                db.update_row("tt", &[V::I64(j)], &[("v", Some(V::Text("b".into())))])
                    .unwrap();
                say(format!("ACK tupd {j} b"));
            }
            if i % 5 == 4 {
                let j = k - 3;
                say(format!("TRY tdel {j}"));
                db.delete_row("tt", &[V::I64(j)]).unwrap();
                say(format!("ACK tdel {j}"));
            }
        }
        // an object: the key is reused, so puts replace and deletes release
        let key = format!("k{}", i % 6);
        let oc: Content = ((i + 2) % 13, i % 3);
        say(format!("TRY put {key} {} {}", oc.0, oc.1));
        let f = db
            .stage_object(
                "b",
                "application/octet-stream",
                None,
                Cursor::new(bytes_of(oc)),
            )
            .unwrap();
        db.put_object("b", &key, f, Default::default()).unwrap();
        say(format!("ACK put {key} {} {}", oc.0, oc.1));
        if i % 4 == 1 {
            let dk = format!("k{}", (i + 3) % 6);
            say(format!("TRY delobj {dk}"));
            db.delete_object("b", &dk, None).unwrap();
            say(format!("ACK delobj {dk}"));
        }
        if i % 5 == 4 {
            db.gc_step(10).unwrap();
        }
        if i % 7 == 6 {
            db.compact(0.0).unwrap();
        }
    }
}

#[derive(Default)]
struct Acked {
    /// the contents a live record's file may legitimately hold (an update in flight adds a second possibility)
    live: HashMap<String, Vec<Content>>,
    maybe_deleted: HashSet<String>,
    /// objects: the contents a key may hold, and whether it may be absent
    objects: HashMap<String, (Vec<Content>, bool)>,
    /// table rows: the values a key may hold, and whether it may be absent
    trows: HashMap<i64, (Vec<String>, bool)>,
    /// keys whose deletion was acknowledged: they must be gone
    tdeleted: HashSet<i64>,
}

fn parse(lines: &[String]) -> Acked {
    let mut a = Acked::default();
    for l in lines {
        let p: Vec<&str> = l.split(' ').collect();
        let c = |i: usize| -> Option<Content> {
            Some((p.get(i)?.parse().ok()?, p.get(i + 1)?.parse().ok()?))
        };
        match p.as_slice() {
            ["ACK", "create", id, ..] => {
                if let Some(c) = c(3) {
                    a.live.insert(id.to_string(), vec![c]);
                }
            }
            ["TRY", "delete", id] => {
                a.maybe_deleted.insert(id.to_string());
            }
            ["ACK", "delete", id] => {
                a.live.remove(*id);
                a.maybe_deleted.remove(*id);
            }
            ["TRY", "update", id, ..] => {
                if let (Some(c), Some(v)) = (c(3), a.live.get_mut(*id)) {
                    v.push(c);
                }
            }
            ["ACK", "update", id, ..] => {
                if let Some(c) = c(3) {
                    a.live.insert(id.to_string(), vec![c]);
                }
            }
            ["TRY", "put", key, ..] => {
                if let Some(c) = c(3) {
                    // a put that was started but never acknowledged: if the key had never existed, it may still not
                    a.objects
                        .entry(key.to_string())
                        .or_insert((vec![], true))
                        .0
                        .push(c);
                }
            }
            ["ACK", "put", key, ..] => {
                if let Some(c) = c(3) {
                    a.objects.insert(key.to_string(), (vec![c], false));
                }
            }
            ["TRY", "delobj", key] => {
                if let Some(e) = a.objects.get_mut(*key) {
                    e.1 = true;
                }
            }
            ["ACK", "delobj", key] => {
                a.objects.remove(*key);
            }
            ["TRY", "trow", k, v] => {
                if let Ok(k) = k.parse::<i64>() {
                    a.trows.insert(k, (vec![v.to_string()], true));
                }
            }
            ["ACK", "trow", k, v] => {
                if let Ok(k) = k.parse::<i64>() {
                    a.trows.insert(k, (vec![v.to_string()], false));
                }
            }
            ["TRY", "tupd", k, v] => {
                if let Some(e) = k.parse::<i64>().ok().and_then(|k| a.trows.get_mut(&k)) {
                    e.0.push(v.to_string());
                }
            }
            ["ACK", "tupd", k, v] => {
                if let Some(e) = k.parse::<i64>().ok().and_then(|k| a.trows.get_mut(&k)) {
                    *e = (vec![v.to_string()], false);
                }
            }
            ["TRY", "tdel", k] => {
                if let Some(e) = k.parse::<i64>().ok().and_then(|k| a.trows.get_mut(&k)) {
                    e.1 = true;
                }
            }
            ["ACK", "tdel", k] => {
                if let Ok(k) = k.parse::<i64>() {
                    a.trows.remove(&k);
                    a.tdeleted.insert(k);
                }
            }
            _ => {} // a line cut off by the crash
        }
    }
    a
}

fn check_acked(db: &Db, a: &Acked, ctx: &str) {
    for (id, allowed) in &a.live {
        match db.get("docs", id).unwrap() {
            None => assert!(
                a.maybe_deleted.contains(id),
                "{ctx}: acknowledged record {id} is gone"
            ),
            Some(rec) => {
                let got = db
                    .read_file(&from_hex_32(&rec.files["a"].id).unwrap())
                    .unwrap_or_else(|e| panic!("{ctx}: reading the file of {id}: {e}"));
                assert!(
                    allowed.iter().any(|c| got == bytes_of(*c)),
                    "{ctx}: the file of {id} is not any of the contents it may hold"
                );
            }
        }
    }
}

fn check_rows(db: &Db, a: &Acked, ctx: &str) {
    use powerdb::types::Value as V;
    for (k, (allowed, absent_ok)) in &a.trows {
        match db.get_row("tt", &[V::I64(*k)]).unwrap() {
            None => assert!(*absent_ok, "{ctx}: acknowledged table row {k} is gone"),
            Some(row) => {
                let got = row
                    .iter()
                    .find(|(n, _)| n == "v")
                    .and_then(|(_, v)| v.clone());
                assert!(
                    allowed.iter().any(|s| got == Some(V::Text(s.clone()))),
                    "{ctx}: table row {k} holds {got:?}, which is none of {allowed:?}"
                );
            }
        }
    }
    for k in &a.tdeleted {
        assert!(
            db.get_row("tt", &[V::I64(*k)]).unwrap().is_none(),
            "{ctx}: table row {k} came back after its deletion was acknowledged"
        );
    }
}

fn check_objects(db: &Db, a: &Acked, ctx: &str) {
    for (key, (allowed, may_be_absent)) in &a.objects {
        match db.get_object("b", key, None) {
            Err(powerdb::Error::NotFound(_)) => {
                assert!(*may_be_absent, "{ctx}: acknowledged object {key} is gone");
            }
            Err(e) => panic!("{ctx}: object {key}: {e}"),
            Ok(m) => {
                let got = db.read_file(&from_hex_32(&m.file_id).unwrap()).unwrap();
                assert!(
                    allowed.iter().any(|c| got == bytes_of(*c)),
                    "{ctx}: object {key} holds bytes that are none of the contents it may hold"
                );
            }
        }
    }
}

fn crash_at(point: &str, nth: u64) {
    let dir = std::env::temp_dir().join(format!("powerdb-fp-{}", new_id()));
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "workload_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("POWERDB_FP_DIR", &dir)
        .env("POWERDB_FAILPOINT", format!("{point}:{nth}"))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let lines: Vec<String> = BufReader::new(child.stdout.take().unwrap())
        .lines()
        .map_while(Result::ok)
        .collect();
    let status = child.wait().unwrap();
    assert!(
        !status.success(),
        "failpoint {point}:{nth} was never reached: the workload finished normally ({} lines)",
        lines.len()
    );
    let ctx = format!("after a crash at {point}:{nth}");
    let acked = parse(&lines);

    // 1. it reopens, and is consistent
    let db =
        Db::open(&dir, options()).unwrap_or_else(|e| panic!("{ctx}: it would not reopen: {e}"));
    let r = db.verify(true).unwrap();
    assert!(
        r.problems.is_empty(),
        "{ctx}: verify found {:?}",
        r.problems
    );
    // 2. nothing acknowledged was lost
    check_acked(&db, &acked, &ctx);
    check_objects(&db, &acked, &ctx);
    check_rows(&db, &acked, &ctx);
    // 3. it still works, and stays consistent through collection and compaction
    let rec = db
        .create("docs", fields(999), vec![("a".into(), stage(&db, (9, 2)))])
        .unwrap();
    while db.gc_step(100).unwrap().files_released > 0 {}
    db.compact(0.0).unwrap();
    let r = db.verify(true).unwrap();
    assert!(
        r.problems.is_empty(),
        "{ctx}: verify found {:?} after recovery work",
        r.problems
    );
    check_acked(&db, &acked, &format!("{ctx} (after collection)"));
    check_objects(&db, &acked, &format!("{ctx} (after collection)"));
    check_rows(&db, &acked, &format!("{ctx} (after collection)"));
    let got = db
        .read_file(&from_hex_32(&db.get("docs", &rec.id).unwrap().unwrap().files["a"].id).unwrap())
        .unwrap();
    assert!(
        got == bytes_of((9, 2)),
        "{ctx}: the new record's file differs"
    );
    drop(db);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn every_failpoint_recovers() {
    // (point, which hit to crash on): an early hit and a later one for the paths that run many times
    for (point, nth) in [
        ("stage.after_append", 3),
        ("stage.after_append", 30),
        ("stage.after_sync", 3),
        ("stage.after_sync", 30),
        ("commit.before_batch", 1),
        ("commit.before_batch", 9),
        ("commit.before_batch", 40),
        ("commit.after_batch", 1),
        ("commit.after_batch", 9),
        ("commit.after_batch", 40),
        ("commit.after_persist", 9),
        ("gc.before_commit", 2),
        ("gc.after_commit", 2),
        ("gc.before_commit", 6),
        ("compact.after_copy", 1),
        ("compact.before_commit", 1),
        ("compact.after_commit", 1),
        ("compact.after_remove", 1),
        ("compact.before_commit", 3),
    ] {
        crash_at(point, nth);
    }
}

// ---- a cascading delete is all or nothing, whichever commit the crash lands on ----

fn cascade_tables(db: &Db) {
    use powerdb::types::Type;
    use powerdb::types::schema::{Change, OnDelete, TableDef};
    let make = |name: &str, cols: &[(&str, Type, bool)]| {
        let mut t = TableDef::new(name).unwrap();
        for (n, ty, nullable) in cols {
            t = t
                .apply(
                    &Change::AddColumn {
                        name: (*n).into(),
                        ty: ty.clone(),
                        nullable: *nullable,
                        default: None,
                    },
                    true,
                )
                .unwrap()
                .0;
        }
        t.set_primary_key(&["k"]).unwrap();
        db.create_table(t).unwrap();
    };
    make("cp", &[("k", Type::I64, false)]);
    make(
        "cc",
        &[
            ("k", Type::I64, false),
            ("pk", Type::I64, true),
            ("tag", Type::Text, true),
        ],
    );
    db.alter_table(
        "cc",
        &Change::AddUnique {
            name: "cc_tag".into(),
            columns: vec!["tag".into()],
        },
    )
    .unwrap();
    db.alter_table(
        "cc",
        &Change::AddForeignKey {
            name: "cc_cp".into(),
            columns: vec!["pk".into()],
            ref_table: "cp".into(),
            ref_table_id: 0,
            ref_pk_types: vec![],
            on_delete: OnDelete::Cascade,
        },
    )
    .unwrap();
}

/// Runs only as the child of `a_cascading_delete_survives_a_crash_at_every_commit`.
#[test]
fn cascade_workload_child() {
    let Ok(dir) = std::env::var("POWERDB_FP_DIR") else {
        return;
    };
    use powerdb::types::Value as V;
    let db = Db::open(&dir, options()).unwrap();
    cascade_tables(&db);
    for i in 0..8i64 {
        db.insert_row("cp", &[("k", V::I64(i))]).unwrap();
        for j in 0..2i64 {
            let k = i * 10 + j;
            db.insert_row(
                "cc",
                &[
                    ("k", V::I64(k)),
                    ("pk", V::I64(i)),
                    ("tag", V::Text(format!("t{k}"))),
                ],
            )
            .unwrap();
        }
        // the parent and both its children go together, in one commit
        db.delete_row("cp", &[V::I64(i)]).unwrap();
    }
}

#[test]
fn a_cascading_delete_survives_a_crash_at_every_commit() {
    for (crashed, nth) in (1..=400u64).enumerate() {
        let dir = std::env::temp_dir().join(format!("powerdb-fp-{}", new_id()));
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "cascade_workload_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("POWERDB_FP_DIR", &dir)
            .env("POWERDB_FAILPOINT", format!("commit.after_batch:{nth}"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        if status.success() {
            // the workload ran to its end before the nth commit: every commit has been tried
            assert!(crashed > 30, "only {crashed} commits were crashed on");
            let _ = std::fs::remove_dir_all(dir);
            return;
        }
        let db = Db::open(&dir, options()).unwrap_or_else(|e| {
            panic!("after the crash at commit {nth}: it would not reopen: {e}")
        });
        // `verify` finds a child whose parent is gone, an index entry without its row, a row without its entry
        let r = db.verify(true).unwrap();
        assert!(
            r.problems.is_empty(),
            "after the crash at commit {nth}: {:?}",
            r.problems
        );
        // and a parent that was deleted took its children with it: none are left with no parent
        let rows = match db.scan_rows("cc", None, 1000) {
            Ok(rows) => rows,
            Err(powerdb::Error::NotFound(_)) => vec![], // the crash was during setup: no such table yet
            Err(e) => panic!("after the crash at commit {nth}: {e}"),
        };
        for row in rows {
            let parent = row
                .iter()
                .find(|(n, _)| n == "pk")
                .and_then(|(_, v)| v.clone());
            if let Some(p) = parent {
                assert!(
                    db.get_row("cp", std::slice::from_ref(&p))
                        .unwrap()
                        .is_some(),
                    "after the crash at commit {nth}: a child refers to the missing parent {p:?}"
                );
            }
        }
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
    }
    panic!("the workload never finished");
}
