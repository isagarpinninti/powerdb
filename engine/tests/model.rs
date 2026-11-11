//! A model-based test of the storage engine.
//!
//! Random creates, updates, deletes, garbage collection, compaction and reopens are applied to the real database and to a plain
//! in-memory model of what it should contain. After every step the database must agree with the model, and `verify` (which recomputes the
//! reference counts from the records) must find nothing wrong. A miscounted reference ends in deleting data that is still in use, so this is
//! the test that has to fail if the counting ever drifts.
//!
//! The seeds are fixed so a failure repeats. `POWERDB_MODEL_SEEDS` raises the number of seeds (the default is small enough for every push).

use powerdb::model::{CollectionDef, FieldDef, FieldKind, Rules};
use powerdb::storage::PendingFile;
use powerdb::storage::chunker::ChunkSizes;
use powerdb::util::{from_hex_32, new_id};
use powerdb::{Db, Durability, Options};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Cursor;
use std::path::PathBuf;

struct Dir(PathBuf);
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// File content is named by a (base, variant) pair, so the model never holds bytes and the test can still regenerate them.
/// Variant 0 is the base itself (a repeat is a duplicate file); variants 1 to 3 change a few bytes in the middle, so they share most
/// chunks with the base but not all: that is what makes the chunk counts interesting.
type Content = (u8, u8);

fn bytes_of((base, variant): Content) -> Vec<u8> {
    let mut x = (base as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut v: Vec<u8> = (0..300_000)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect();
    if variant > 0 {
        let at = 100_000 + variant as usize * 37_000;
        for b in &mut v[at..at + 64] {
            *b = b.wrapping_add(variant);
        }
    }
    v
}

fn options() -> Options {
    Options {
        durability: Durability::Never,
        segment_bytes: 2 << 20, // small, so segments seal and compaction has work
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
    let f = |name: &str, kind| FieldDef {
        name: name.into(),
        kind,
        required: false,
        max: None,
        mime: vec![],
        relation: None,
    };
    CollectionDef {
        name: "docs".into(),
        fields: vec![
            f("n", FieldKind::Number),
            f("a", FieldKind::File),
            f("b", FieldKind::File),
        ],
        rules: Rules::default(),
        encrypt: false,
        created_at: 0,
    }
}

fn open(dir: &Dir) -> Db {
    let db = Db::open(&dir.0, options()).unwrap();
    if db.collection("docs").is_err() {
        db.create_collection(schema()).unwrap();
    }
    db
}

fn stage(db: &Db, c: Content) -> PendingFile {
    db.stage_file(
        "docs",
        "application/octet-stream",
        Some("f.bin"),
        Cursor::new(bytes_of(c)),
    )
    .unwrap()
}

fn pick(rng: &mut Rng) -> Content {
    // a small pool, so the same content comes up again and again
    (rng.below(4) as u8, rng.below(4) as u8)
}

#[derive(Clone, Debug)]
struct Model {
    n: i64,
    files: BTreeMap<String, Content>,
}

fn fields(n: i64) -> Map<String, Value> {
    json!({ "n": n }).as_object().unwrap().clone()
}

/// The database must hold exactly the model: the same records, the same fields, the same file bytes, and nothing else.
fn agree(db: &Db, model: &HashMap<String, Model>, ctx: &str) {
    assert_eq!(
        db.count("docs").unwrap(),
        model.len() as u64,
        "{ctx}: the number of records"
    );
    for (id, m) in model {
        let rec = db
            .get("docs", id)
            .unwrap()
            .unwrap_or_else(|| panic!("{ctx}: record {id} is missing"));
        assert_eq!(
            rec.fields.get("n"),
            Some(&json!(m.n)),
            "{ctx}: field n of {id}"
        );
        assert_eq!(
            rec.files.keys().collect::<Vec<_>>(),
            m.files.keys().collect::<Vec<_>>(),
            "{ctx}: the files of {id}"
        );
        for (field, content) in &m.files {
            let got = db
                .read_file(&from_hex_32(&rec.files[field].id).unwrap())
                .unwrap_or_else(|e| panic!("{ctx}: reading {id}.{field}: {e}"));
            assert!(
                got == bytes_of(*content),
                "{ctx}: the bytes of {id}.{field} differ"
            );
        }
    }
}

fn check(db: &Db, model: &HashMap<String, Model>, ctx: &str, drained: bool) {
    agree(db, model, ctx);
    let r = db.verify(true).unwrap();
    assert!(
        r.problems.is_empty(),
        "{ctx}: verify found {:?}",
        r.problems
    );
    if drained {
        // once the release queue is empty, a stored file that no record uses is a leak
        let distinct: HashSet<Content> = model
            .values()
            .flat_map(|m| m.files.values().copied())
            .collect();
        assert_eq!(
            r.files,
            distinct.len() as u64,
            "{ctx}: stored files differ from the distinct files in use (a leak, or a premature release)"
        );
    }
}

fn drain(db: &Db) {
    while db.gc_step(100).unwrap().files_released > 0 {}
}

fn run(seed: u64, steps: usize) {
    let dir = Dir(std::env::temp_dir().join(format!("powerdb-model-{}", new_id())));
    std::fs::create_dir_all(&dir.0).unwrap();
    let mut db = Some(open(&dir));
    let mut rng = Rng(seed.wrapping_mul(0x2545_F491_4F6C_DD1D) | 1);
    let mut model: HashMap<String, Model> = HashMap::new();
    let mut counter = 0i64;

    for step in 0..steps {
        let d = db.as_ref().unwrap();
        let ids: Vec<String> = {
            let mut v: Vec<String> = model.keys().cloned().collect();
            v.sort();
            v
        };
        let mut drained = false;
        let op;
        match rng.below(100) {
            // create, with zero, one or two files
            0..=29 => {
                counter += 1;
                let mut files = Vec::new();
                let mut mfiles = BTreeMap::new();
                for field in ["a", "b"] {
                    if rng.below(3) > 0 {
                        let c = pick(&mut rng);
                        files.push((field.to_string(), stage(d, c)));
                        mfiles.insert(field.to_string(), c);
                    }
                }
                let rec = d.create("docs", fields(counter), files).unwrap();
                model.insert(
                    rec.id,
                    Model {
                        n: counter,
                        files: mfiles,
                    },
                );
                op = "create";
            }
            // replace or add a file
            30..=49 if !ids.is_empty() => {
                let id = &ids[rng.below(ids.len())];
                let field = if rng.below(2) == 0 { "a" } else { "b" };
                let c = pick(&mut rng);
                d.update(
                    "docs",
                    id,
                    Map::new(),
                    vec![(field.into(), stage(d, c))],
                    &[],
                )
                .unwrap();
                model.get_mut(id).unwrap().files.insert(field.into(), c);
                op = "replace file";
            }
            // drop a file
            50..=59 if !ids.is_empty() => {
                let id = &ids[rng.below(ids.len())];
                let field = if rng.below(2) == 0 { "a" } else { "b" };
                d.update("docs", id, Map::new(), vec![], &[field.to_string()])
                    .unwrap();
                model.get_mut(id).unwrap().files.remove(field);
                op = "drop file";
            }
            // change a field only
            60..=69 if !ids.is_empty() => {
                let id = &ids[rng.below(ids.len())];
                counter += 1;
                d.update("docs", id, fields(counter), vec![], &[]).unwrap();
                model.get_mut(id).unwrap().n = counter;
                op = "update field";
            }
            // delete
            70..=84 if !ids.is_empty() => {
                let id = ids[rng.below(ids.len())].clone();
                assert!(d.delete("docs", &id).unwrap());
                model.remove(&id);
                op = "delete";
            }
            85..=89 => {
                d.gc_step(1 + rng.below(5)).unwrap();
                op = "gc some";
            }
            90..=94 => {
                drain(d);
                d.compact(0.0).unwrap();
                drained = true;
                op = "drain and compact";
            }
            95..=97 => {
                // a stage that is never committed: its chunks must stay unreferenced and harmless
                let _abandoned = stage(d, pick(&mut rng));
                op = "abandoned stage";
            }
            _ => {
                drop(db.take()); // closes the database
                db = Some(open(&dir));
                op = "reopen";
            }
        }
        let d = db.as_ref().unwrap();
        // verify reads the whole log, so it runs every few steps; the cheap model comparison runs every step
        let ctx = format!("seed {seed} step {step} ({op})");
        if step % 5 == 4 || drained || op == "reopen" {
            check(d, &model, &ctx, drained);
        } else {
            agree(d, &model, &ctx);
        }
    }
    let d = db.as_ref().unwrap();
    drain(d);
    d.compact(0.0).unwrap();
    check(d, &model, &format!("seed {seed} end"), true);
    // and everything must still be there after a restart
    drop(db);
    let d = open(&dir);
    check(&d, &model, &format!("seed {seed} after restart"), true);
}

fn seeds() -> u64 {
    std::env::var("POWERDB_MODEL_SEEDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(6)
}

#[test]
fn random_operations_agree_with_the_model_and_keep_the_counts_exact() {
    for seed in 1..=seeds() {
        run(seed, 160);
    }
}

#[test]
fn deleting_everything_frees_every_file() {
    let dir = Dir(std::env::temp_dir().join(format!("powerdb-model-{}", new_id())));
    std::fs::create_dir_all(&dir.0).unwrap();
    let db = open(&dir);
    let mut ids = Vec::new();
    for i in 0..12u8 {
        let rec = db
            .create(
                "docs",
                fields(i as i64),
                vec![
                    ("a".into(), stage(&db, (i % 3, i % 4))),
                    ("b".into(), stage(&db, (i % 2, 0))),
                ],
            )
            .unwrap();
        ids.push(rec.id);
    }
    for id in &ids {
        db.delete("docs", id).unwrap();
    }
    drain(&db);
    let r = db.verify(true).unwrap();
    assert!(r.problems.is_empty(), "{:?}", r.problems);
    assert_eq!(
        r.files, 0,
        "a file survived the deletion of every record that used it"
    );
    // (chunks in the segment still being written stay indexed until it is sealed and compacted, so they are not counted here)
}

/// Regression: a file queued for release twice (deleted, re-attached before the collector ran, deleted again) was released twice in
/// one collector pass, so every chunk it shares with a file that is still in use was counted down twice. The count then reached zero
/// while the other file still needed the chunk, and compaction deleted data that was in use.
#[test]
fn a_file_queued_for_release_twice_is_released_once() {
    let dir = Dir(std::env::temp_dir().join(format!("powerdb-model-{}", new_id())));
    std::fs::create_dir_all(&dir.0).unwrap();
    let db = open(&dir);
    // g shares most of its chunks with f, and stays alive throughout
    let f: Content = (0, 0);
    let g: Content = (0, 1);
    let keeper = db
        .create("docs", fields(1), vec![("a".into(), stage(&db, g))])
        .unwrap();
    let first = db
        .create("docs", fields(2), vec![("a".into(), stage(&db, f))])
        .unwrap();
    db.delete("docs", &first.id).unwrap(); // f is queued for release
    let again = db
        .create("docs", fields(3), vec![("a".into(), stage(&db, f))])
        .unwrap(); // the same file is attached again before the collector runs
    db.delete("docs", &again.id).unwrap(); // and queued a second time
    drain(&db);
    let r = db.verify(true).unwrap();
    assert!(r.problems.is_empty(), "{:?}", r.problems);
    // push the data through compaction, which is what turns a wrong count into lost bytes
    db.compact(0.0).unwrap();
    let rec = db.get("docs", &keeper.id).unwrap().unwrap();
    let got = db
        .read_file(&from_hex_32(&rec.files["a"].id).unwrap())
        .unwrap();
    assert!(got == bytes_of(g), "the file that stayed in use lost data");
}
