//! Buckets and objects: behaviour, and a random-operations model test like the one for records.

use powerdb::model::{CollectionDef, FieldDef, FieldKind, Rules};
use powerdb::storage::chunker::ChunkSizes;
use powerdb::storage::objects::{BucketDef, ObjectMeta};
use powerdb::util::{from_hex_32, hex, new_id};
use powerdb::{Db, Durability, Error, Options};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Cursor;
use std::path::PathBuf;

struct Dir(PathBuf);
impl Dir {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("powerdb-obj-{}", new_id()));
        std::fs::create_dir_all(&p).unwrap();
        Dir(p)
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

type Content = (u8, u8);

fn bytes_of((base, variant): Content) -> Vec<u8> {
    let mut x = (base as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut v: Vec<u8> = (0..200_000)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect();
    if variant > 0 {
        let at = 50_000 + variant as usize * 20_000;
        for b in &mut v[at..at + 64] {
            *b = b.wrapping_add(variant);
        }
    }
    v
}

fn options() -> Options {
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

fn open(d: &Dir) -> Db {
    Db::open(&d.0, options()).unwrap()
}

fn bucket(db: &Db, name: &str, versioning: bool) {
    db.create_bucket(BucketDef {
        name: name.into(),
        versioning,
        encrypt: false,
        created_at: 0,
    })
    .unwrap();
}

fn put(db: &Db, b: &str, key: &str, bytes: Vec<u8>) -> ObjectMeta {
    let f = db
        .stage_object(
            b,
            "application/octet-stream",
            Some("f.bin"),
            Cursor::new(bytes),
        )
        .unwrap();
    db.put_object(b, key, f, BTreeMap::new()).unwrap()
}

fn read(db: &Db, m: &ObjectMeta) -> Vec<u8> {
    db.read_file(&from_hex_32(&m.file_id).unwrap()).unwrap()
}

fn drain(db: &Db) {
    while db.gc_step(100).unwrap().files_released > 0 {}
}

fn clean(db: &Db) {
    let r = db.verify(true).unwrap();
    assert!(r.problems.is_empty(), "{:?}", r.problems);
}

#[test]
fn an_object_round_trips_with_its_metadata_and_checksum() {
    let d = Dir::new();
    let db = open(&d);
    bucket(&db, "docs", false);
    let bytes = bytes_of((1, 0));
    let f = db
        .stage_object(
            "docs",
            "application/pdf",
            Some("Contract (final).pdf"),
            Cursor::new(bytes.clone()),
        )
        .unwrap();
    let meta: BTreeMap<String, String> = [("customer".to_string(), "acme".to_string())].into();
    let m = db
        .put_object("docs", "contracts/2026/acme.pdf", f, meta.clone())
        .unwrap();
    assert_eq!(m.size, bytes.len() as u64);
    assert_eq!(m.mime, "application/pdf");
    assert_eq!(m.filename.as_deref(), Some("Contract (final).pdf"));
    assert_eq!(
        m.checksum,
        hex(blake3::hash(&bytes).as_bytes()),
        "the checksum is the BLAKE3 of the content"
    );
    assert_eq!(m.metadata, meta);
    let got = db
        .get_object("docs", "contracts/2026/acme.pdf", None)
        .unwrap();
    assert_eq!(got, m);
    assert_eq!(read(&db, &got), bytes);
    assert!(matches!(
        db.get_object("docs", "nope", None),
        Err(Error::NotFound(_))
    ));
    assert!(matches!(
        db.get_object("nobucket", "x", None),
        Err(Error::NotFound(_))
    ));
    clean(&db);
}

#[test]
fn files_of_any_kind_are_accepted_as_opaque_bytes() {
    let d = Dir::new();
    let db = open(&d);
    bucket(&db, "misc", false);
    for (key, bytes) in [
        ("empty", vec![]),
        ("one", vec![7u8]),
        ("exe-named-png.png", b"MZ\x90\0\x03\0\0\0".to_vec()),
        ("zeros", vec![0u8; 100_000]),
        ("unicode/\u{65e5}\u{672c}\u{8a9e}.bin", bytes_of((3, 0))),
    ] {
        let m = put(&db, "misc", key, bytes.clone());
        assert_eq!(read(&db, &m), bytes, "{key}");
    }
    clean(&db);
}

#[test]
fn bucket_and_key_names_are_checked() {
    let d = Dir::new();
    let db = open(&d);
    for bad in ["", "Upper", "-lead", "has space", "a/b", &"x".repeat(64)] {
        assert!(
            matches!(
                db.create_bucket(BucketDef {
                    name: bad.into(),
                    versioning: false,
                    encrypt: false,
                    created_at: 0
                }),
                Err(Error::Invalid(_))
            ),
            "{bad:?}"
        );
    }
    bucket(&db, "ok-bucket_1", false);
    assert!(matches!(
        db.create_bucket(BucketDef {
            name: "ok-bucket_1".into(),
            versioning: false,
            encrypt: false,
            created_at: 0
        }),
        Err(Error::Conflict(_))
    ));
    for bad in [
        "",
        "/lead",
        "a/../b",
        "./a",
        "a\0b",
        "a\nb",
        &"k".repeat(1025),
    ] {
        let f = db
            .stage_object(
                "ok-bucket_1",
                "text/plain",
                None,
                Cursor::new(b"x".to_vec()),
            )
            .unwrap();
        assert!(
            matches!(
                db.put_object("ok-bucket_1", bad, f, BTreeMap::new()),
                Err(Error::Invalid(_))
            ),
            "{bad:?}"
        );
    }
    // metadata limits
    let f = db
        .stage_object(
            "ok-bucket_1",
            "text/plain",
            None,
            Cursor::new(b"x".to_vec()),
        )
        .unwrap();
    let too_many: BTreeMap<String, String> =
        (0..33).map(|i| (format!("k{i}"), "v".into())).collect();
    assert!(matches!(
        db.put_object("ok-bucket_1", "k", f, too_many),
        Err(Error::Invalid(_))
    ));
    // the long key that is allowed
    put(&db, "ok-bucket_1", &"k".repeat(1024), b"x".to_vec());
    clean(&db);
}

#[test]
fn a_bucket_with_objects_cannot_be_deleted_and_an_empty_one_can() {
    let d = Dir::new();
    let db = open(&d);
    bucket(&db, "b", true);
    put(&db, "b", "k", b"x".to_vec());
    assert!(matches!(db.delete_bucket("b"), Err(Error::Conflict(_))));
    db.delete_object("b", "k", None).unwrap(); // a delete marker is still an entry
    assert!(matches!(db.delete_bucket("b"), Err(Error::Conflict(_))));
    for v in db.object_versions("b", "k").unwrap() {
        assert!(db.delete_object("b", "k", Some(&v.version_id)).unwrap());
    }
    db.delete_bucket("b").unwrap();
    assert!(matches!(db.bucket("b"), Err(Error::NotFound(_))));
    drain(&db);
    clean(&db);
}

#[test]
fn without_versioning_a_put_replaces_and_a_delete_removes_for_good() {
    let d = Dir::new();
    let db = open(&d);
    bucket(&db, "b", false);
    let first = put(&db, "b", "k", bytes_of((1, 0)));
    let second = put(&db, "b", "k", bytes_of((2, 0)));
    assert_eq!(
        first.id, second.id,
        "the object keeps its id across a replace"
    );
    assert_ne!(first.version_id, second.version_id);
    assert_eq!(
        db.object_versions("b", "k").unwrap().len(),
        1,
        "one version only"
    );
    assert_eq!(
        read(&db, &db.get_object("b", "k", None).unwrap()),
        bytes_of((2, 0))
    );
    drain(&db);
    assert_eq!(
        db.verify(false).unwrap().files,
        1,
        "the replaced file was released"
    );
    assert!(db.delete_object("b", "k", None).unwrap());
    assert!(
        !db.delete_object("b", "k", None).unwrap(),
        "deleting again changes nothing"
    );
    assert!(matches!(
        db.get_object("b", "k", None),
        Err(Error::NotFound(_))
    ));
    drain(&db);
    assert_eq!(db.verify(false).unwrap().files, 0);
    clean(&db);
}

#[test]
fn identical_content_is_stored_once_however_many_keys_name_it() {
    let d = Dir::new();
    let db = open(&d);
    bucket(&db, "b", false);
    for i in 0..5 {
        put(&db, "b", &format!("copy-{i}"), bytes_of((4, 0)));
    }
    let r = db.verify(false).unwrap();
    assert_eq!((r.objects, r.files), (5, 1));
    for i in 0..4 {
        db.delete_object("b", &format!("copy-{i}"), None).unwrap();
    }
    drain(&db);
    db.compact(0.0).unwrap();
    let m = db.get_object("b", "copy-4", None).unwrap();
    assert_eq!(
        read(&db, &m),
        bytes_of((4, 0)),
        "the last key still has its bytes"
    );
    clean(&db);
}

#[test]
fn versioning_keeps_history_marks_deletes_and_restores() {
    let d = Dir::new();
    let db = open(&d);
    bucket(&db, "b", true);
    let v1 = put(&db, "b", "doc", bytes_of((1, 0)));
    let v2 = put(&db, "b", "doc", bytes_of((1, 1)));
    let v3 = put(&db, "b", "doc", bytes_of((2, 0)));
    let versions = db.object_versions("b", "doc").unwrap();
    assert_eq!(
        versions
            .iter()
            .map(|m| m.version_id.clone())
            .collect::<Vec<_>>(),
        vec![
            v3.version_id.clone(),
            v2.version_id.clone(),
            v1.version_id.clone()
        ],
        "newest first"
    );
    assert!(
        versions.iter().all(|m| m.id == v1.id),
        "one object id across its versions"
    );
    assert_eq!(
        read(
            &db,
            &db.get_object("b", "doc", Some(&v1.version_id)).unwrap()
        ),
        bytes_of((1, 0))
    );
    assert_eq!(
        read(&db, &db.get_object("b", "doc", None).unwrap()),
        bytes_of((2, 0))
    );

    // a delete hides the key but keeps the data
    assert!(db.delete_object("b", "doc", None).unwrap());
    assert!(
        !db.delete_object("b", "doc", None).unwrap(),
        "already deleted"
    );
    assert!(matches!(
        db.get_object("b", "doc", None),
        Err(Error::NotFound(_))
    ));
    assert!(db.list_objects("b", "", None, 10).unwrap().items.is_empty());
    assert_eq!(
        db.object_versions("b", "doc").unwrap().len(),
        4,
        "three versions and a marker"
    );
    drain(&db);
    db.compact(0.0).unwrap();

    // restore an old version: it becomes the newest, and the history is intact
    let r = db
        .restore_object_version("b", "doc", &v2.version_id)
        .unwrap();
    assert_ne!(r.version_id, v2.version_id);
    assert_eq!(
        read(&db, &db.get_object("b", "doc", None).unwrap()),
        bytes_of((1, 1))
    );
    assert_eq!(db.list_objects("b", "", None, 10).unwrap().items.len(), 1);
    assert!(matches!(
        db.restore_object_version("b", "doc", "nope"),
        Err(Error::NotFound(_))
    ));
    clean(&db);

    // removing a named version is permanent, and frees its file once nothing else uses it
    assert!(db.delete_object("b", "doc", Some(&v3.version_id)).unwrap());
    assert!(!db.delete_object("b", "doc", Some(&v3.version_id)).unwrap());
    assert!(matches!(
        db.get_object("b", "doc", Some(&v3.version_id)),
        Err(Error::NotFound(_))
    ));
    drain(&db);
    db.compact(0.0).unwrap();
    assert_eq!(
        read(
            &db,
            &db.get_object("b", "doc", Some(&v1.version_id)).unwrap()
        ),
        bytes_of((1, 0))
    );
    clean(&db);
}

#[test]
fn restoring_needs_a_versioned_bucket_and_a_real_version() {
    let d = Dir::new();
    let db = open(&d);
    bucket(&db, "plain", false);
    let m = put(&db, "plain", "k", b"x".to_vec());
    assert!(matches!(
        db.restore_object_version("plain", "k", &m.version_id),
        Err(Error::Invalid(_))
    ));
    bucket(&db, "v", true);
    put(&db, "v", "k", b"x".to_vec());
    db.delete_object("v", "k", None).unwrap();
    let marker = db.object_versions("v", "k").unwrap().remove(0);
    assert!(marker.delete_marker);
    assert!(matches!(
        db.restore_object_version("v", "k", &marker.version_id),
        Err(Error::Invalid(_))
    ));
}

#[test]
fn versioning_can_be_turned_on_but_not_off() {
    let d = Dir::new();
    let db = open(&d);
    bucket(&db, "b", false);
    put(&db, "b", "k", b"one".to_vec());
    db.set_bucket_versioning("b", true).unwrap();
    put(&db, "b", "k", b"two".to_vec());
    assert_eq!(db.object_versions("b", "k").unwrap().len(), 2);
    assert!(matches!(
        db.set_bucket_versioning("b", false),
        Err(Error::Invalid(_))
    ));
    assert!(db.bucket("b").unwrap().versioning);
}

#[test]
fn listing_pages_through_keys_in_order_with_prefixes() {
    let d = Dir::new();
    let db = open(&d);
    bucket(&db, "b", true);
    let mut keys: Vec<String> = Vec::new();
    for i in 0..23 {
        let k = format!("{}/item-{i:02}", if i % 2 == 0 { "even" } else { "odd" });
        put(&db, "b", &k, vec![i as u8; 10]);
        put(&db, "b", &k, vec![i as u8 + 100; 10]); // two versions: still one listing entry
        keys.push(k);
    }
    put(&db, "b", "zz", b"z".to_vec());
    keys.push("zz".into());
    keys.sort();
    db.delete_object("b", "even/item-04", None).unwrap();
    keys.retain(|k| k != "even/item-04");

    let page_all = |prefix: &str, limit: usize| -> Vec<String> {
        let mut out = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let p = db
                .list_objects("b", prefix, after.as_deref(), limit)
                .unwrap();
            out.extend(p.items.iter().map(|m| m.key.clone()));
            match p.next {
                Some(n) => after = Some(n),
                None => break,
            }
        }
        out
    };
    assert_eq!(
        page_all("", 5),
        keys,
        "every key once, in order, whatever the page size"
    );
    assert_eq!(page_all("", 1), keys);
    assert_eq!(page_all("", 1000), keys);
    let odd: Vec<String> = keys
        .iter()
        .filter(|k| k.starts_with("odd/"))
        .cloned()
        .collect();
    assert_eq!(page_all("odd/", 4), odd);
    assert!(page_all("none/", 10).is_empty());
    // every listed entry is the newest version
    for m in db.list_objects("b", "odd/", None, 100).unwrap().items {
        assert_eq!(
            m.version_id,
            db.object_versions("b", &m.key).unwrap()[0].version_id
        );
    }
}

#[test]
fn a_record_and_an_object_can_share_a_file_and_each_keeps_it_alive() {
    let d = Dir::new();
    let db = open(&d);
    db.create_collection(CollectionDef {
        name: "docs".into(),
        fields: vec![FieldDef {
            name: "f".into(),
            kind: FieldKind::File,
            required: false,
            max: None,
            mime: vec![],
            relation: None,
        }],
        rules: Rules::default(),
        encrypt: false,
        created_at: 0,
    })
    .unwrap();
    bucket(&db, "b", false);
    let bytes = bytes_of((6, 0));
    let f = db
        .stage_file(
            "docs",
            "application/octet-stream",
            None,
            Cursor::new(bytes.clone()),
        )
        .unwrap();
    let rec = db
        .create("docs", Default::default(), vec![("f".into(), f)])
        .unwrap();
    let obj = put(&db, "b", "same", bytes.clone());
    assert_eq!(
        db.verify(false).unwrap().files,
        1,
        "one stored file for both"
    );
    // delete the record: the object must keep the bytes through collection and compaction
    db.delete("docs", &rec.id).unwrap();
    drain(&db);
    db.compact(0.0).unwrap();
    assert_eq!(read(&db, &obj), bytes);
    clean(&db);
    // and the other way round
    let f = db
        .stage_file(
            "docs",
            "application/octet-stream",
            None,
            Cursor::new(bytes.clone()),
        )
        .unwrap();
    let rec = db
        .create("docs", Default::default(), vec![("f".into(), f)])
        .unwrap();
    db.delete_object("b", "same", None).unwrap();
    drain(&db);
    db.compact(0.0).unwrap();
    let got = db.get("docs", &rec.id).unwrap().unwrap();
    assert_eq!(
        db.read_file(&from_hex_32(&got.files["f"].id).unwrap())
            .unwrap(),
        bytes
    );
    clean(&db);
}

#[test]
fn objects_survive_a_restart_and_a_backup() {
    let d = Dir::new();
    let copy = Dir::new();
    let (v1, v2) = {
        let db = open(&d);
        bucket(&db, "b", true);
        let a = put(&db, "b", "k", bytes_of((1, 0)));
        let b = put(&db, "b", "k", bytes_of((1, 2)));
        db.delete_object("b", "other", None).unwrap();
        db.snapshot_to(&copy.0).unwrap();
        (a, b)
    };
    for dir in [&d, &copy] {
        let db = open(dir);
        assert_eq!(
            read(&db, &db.get_object("b", "k", None).unwrap()),
            bytes_of((1, 2))
        );
        assert_eq!(
            read(&db, &db.get_object("b", "k", Some(&v1.version_id)).unwrap()),
            bytes_of((1, 0))
        );
        assert_eq!(
            db.object_versions("b", "k").unwrap()[0].version_id,
            v2.version_id
        );
        clean(&db);
    }
}

// ---- model test ----

#[derive(Clone, Debug)]
struct Ver {
    vid: String,
    content: Option<Content>, // None: a delete marker
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

fn agree(db: &Db, model: &HashMap<(String, String), Vec<Ver>>, ctx: &str) {
    for b in ["plain", "vers"] {
        let versioned = b == "vers";
        let mut visible: Vec<String> = Vec::new();
        for ((mb, key), vs) in model {
            if mb != b {
                continue;
            }
            let got: Vec<(String, bool)> = db
                .object_versions(b, key)
                .unwrap()
                .into_iter()
                .map(|m| (m.version_id, m.delete_marker))
                .collect();
            let want: Vec<(String, bool)> = vs
                .iter()
                .rev()
                .map(|v| (v.vid.clone(), v.content.is_none()))
                .collect();
            assert_eq!(got, want, "{ctx}: the versions of {b}/{key}");
            if !versioned {
                assert!(
                    vs.len() <= 1,
                    "{ctx}: the model itself must keep one version in a plain bucket"
                );
            }
            match vs.last() {
                Some(Ver {
                    content: Some(c), ..
                }) => {
                    visible.push(key.clone());
                    let m = db
                        .get_object(b, key, None)
                        .unwrap_or_else(|e| panic!("{ctx}: {b}/{key}: {e}"));
                    assert!(
                        read(db, &m) == bytes_of(*c),
                        "{ctx}: the bytes of {b}/{key}"
                    );
                }
                _ => assert!(
                    matches!(db.get_object(b, key, None), Err(Error::NotFound(_))),
                    "{ctx}: {b}/{key} should read as absent"
                ),
            }
            for v in vs {
                if let Some(c) = v.content {
                    let m = db.get_object(b, key, Some(&v.vid)).unwrap();
                    assert!(
                        read(db, &m) == bytes_of(c),
                        "{ctx}: version {} of {b}/{key}",
                        v.vid
                    );
                }
            }
        }
        visible.sort();
        let mut listed = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let p = db.list_objects(b, "", after.as_deref(), 7).unwrap();
            listed.extend(p.items.into_iter().map(|m| m.key));
            match p.next {
                Some(n) => after = Some(n),
                None => break,
            }
        }
        assert_eq!(listed, visible, "{ctx}: the listing of {b}");
    }
}

fn run(seed: u64, steps: usize) {
    let d = Dir::new();
    let mut db = Some(open(&d));
    bucket(db.as_ref().unwrap(), "plain", false);
    bucket(db.as_ref().unwrap(), "vers", true);
    let mut rng = Rng(seed.wrapping_mul(0x2545_F491_4F6C_DD1D) | 1);
    let mut model: HashMap<(String, String), Vec<Ver>> = HashMap::new();
    let keys = ["a", "b", "dir/c", "dir/d", "e"];
    for step in 0..steps {
        let dbr = db.as_ref().unwrap();
        let b = if rng.below(2) == 0 { "plain" } else { "vers" };
        let key = keys[rng.below(keys.len())].to_string();
        let slot = (b.to_string(), key.clone());
        let op;
        match rng.below(100) {
            0..=44 => {
                let c: Content = (rng.below(8) as u8, rng.below(3) as u8);
                let m = put(dbr, b, &key, bytes_of(c));
                let vs = model.entry(slot).or_default();
                if b == "plain" {
                    vs.clear();
                }
                vs.push(Ver {
                    vid: m.version_id,
                    content: Some(c),
                });
                op = "put";
            }
            45..=59 => {
                let changed = dbr.delete_object(b, &key, None).unwrap();
                let vs = model.entry(slot.clone()).or_default();
                if b == "plain" {
                    assert_eq!(changed, !vs.is_empty());
                    vs.clear();
                } else {
                    let live = matches!(
                        vs.last(),
                        Some(Ver {
                            content: Some(_),
                            ..
                        })
                    );
                    assert_eq!(changed, live, "step {step}: delete changed-flag");
                    if live {
                        let id = dbr.object_versions(b, &key).unwrap()[0].version_id.clone();
                        vs.push(Ver {
                            vid: id,
                            content: None,
                        });
                    }
                }
                op = "delete";
            }
            60..=69 => {
                let vs = model.entry(slot).or_default();
                if !vs.is_empty() {
                    let i = rng.below(vs.len());
                    let v = vs.remove(i);
                    assert!(dbr.delete_object(b, &key, Some(&v.vid)).unwrap());
                }
                op = "delete version";
            }
            70..=77 => {
                if b == "vers" {
                    let vs = model.entry(slot).or_default();
                    let candidates: Vec<Ver> =
                        vs.iter().filter(|v| v.content.is_some()).cloned().collect();
                    if !candidates.is_empty() {
                        let v = &candidates[rng.below(candidates.len())];
                        let m = dbr.restore_object_version(b, &key, &v.vid).unwrap();
                        vs.push(Ver {
                            vid: m.version_id,
                            content: v.content,
                        });
                    }
                }
                op = "restore";
            }
            78..=86 => {
                dbr.gc_step(1 + rng.below(4)).unwrap();
                op = "gc some";
            }
            87..=93 => {
                drain(dbr);
                dbr.compact(0.0).unwrap();
                op = "drain and compact";
            }
            _ => {
                drop(db.take());
                db = Some(open(&d));
                op = "reopen";
            }
        }
        let dbr = db.as_ref().unwrap();
        let ctx = format!("seed {seed} step {step} ({op})");
        agree(dbr, &model, &ctx);
        if step % 6 == 5 || op == "drain and compact" || op == "reopen" {
            let r = dbr.verify(true).unwrap();
            assert!(
                r.problems.is_empty(),
                "{ctx}: verify found {:?}",
                r.problems
            );
        }
    }
    let dbr = db.as_ref().unwrap();
    drain(dbr);
    dbr.compact(0.0).unwrap();
    let r = dbr.verify(true).unwrap();
    assert!(r.problems.is_empty(), "seed {seed} end: {:?}", r.problems);
    // once the release queue is empty, the stored files are exactly the distinct files the live versions name
    let distinct: HashSet<Content> = model
        .values()
        .flat_map(|vs| vs.iter().filter_map(|v| v.content))
        .collect();
    assert_eq!(
        r.files,
        distinct.len() as u64,
        "seed {seed}: stored files differ from the files in use (a leak, or a premature release)"
    );
    assert_eq!(
        r.objects,
        model.values().map(|vs| vs.len() as u64).sum::<u64>(),
        "seed {seed}: object entries"
    );
}

#[test]
fn random_object_operations_agree_with_the_model() {
    let n: u64 = std::env::var("POWERDB_MODEL_SEEDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(6);
    for seed in 1..=n {
        run(seed, 150);
    }
}
