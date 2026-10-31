use powerdb::model::{CollectionDef, FieldDef, FieldKind, Rules};
use powerdb::storage::chunker::ChunkSizes;
use powerdb::util::{from_hex_32, new_id};
use powerdb::{Db, Durability, Options};
use serde_json::{json, Map, Value};
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

struct Dir(PathBuf);
impl Dir {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("powerdb-test-{}", new_id()));
        std::fs::create_dir_all(&p).unwrap();
        Dir(p)
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Deterministic pseudo-random bytes (xorshift): every test file differs and no accidental repetition hides a bug.
fn data(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

fn small_opts() -> Options {
    Options { durability: Durability::Never, segment_bytes: 4 << 20, chunk: ChunkSizes { min: 16 << 10, avg: 64 << 10, max: 256 << 10 }, verify_reads: true, cache_bytes: None }
}

fn schema() -> CollectionDef {
    let field = |name: &str, kind| FieldDef { name: name.into(), kind, required: false, max: None, mime: vec![], relation: None };
    CollectionDef {
        name: "invoices".into(),
        fields: vec![
            FieldDef { required: true, ..field("client", FieldKind::Text) },
            field("amount", FieldKind::Number),
            field("status", FieldKind::Text),
            field("owner_id", FieldKind::Text),
            FieldDef { max: Some(64 << 20), mime: vec!["application/pdf".into(), "image/*".into(), "application/octet-stream".into()], ..field("document", FieldKind::File) },
        ],
        rules: Rules::default(),
        encrypt: false,
        created_at: 0,
    }
}

fn open(dir: &Dir) -> Db {
    let db = Db::open(&dir.0, small_opts()).unwrap();
    if db.collection("invoices").is_err() {
        db.create_collection(schema()).unwrap();
    }
    db
}

fn fields(client: &str, amount: i64) -> Map<String, Value> {
    json!({"client": client, "amount": amount, "status": "paid", "owner_id": "u1"}).as_object().unwrap().clone()
}

fn stage(db: &Db, bytes: &[u8]) -> powerdb::storage::PendingFile {
    db.stage_file("invoices", "application/octet-stream", Some("doc.bin"), Cursor::new(bytes.to_vec())).unwrap()
}

fn read(db: &Db, rec: &powerdb::model::Record, field: &str) -> Vec<u8> {
    db.read_file(&from_hex_32(&rec.files[field].id).unwrap()).unwrap()
}

#[test]
fn a_record_and_its_file_round_trip() {
    let d = Dir::new();
    let db = open(&d);
    let bytes = data(3 * (1 << 20) + 12345, 1);
    let rec = db.create("invoices", fields("Acme", 4500), vec![("document".into(), stage(&db, &bytes))]).unwrap();
    let got = db.get("invoices", &rec.id).unwrap().unwrap();
    assert_eq!(got, rec);
    assert_eq!(got.fields["client"], "Acme");
    assert_eq!(got.files["document"].size, bytes.len() as u64);
    assert_eq!(read(&db, &got, "document"), bytes);
    assert!(db.get("invoices", "nope").unwrap().is_none());
}

#[test]
fn records_survive_a_reopen() {
    let d = Dir::new();
    let (id, bytes) = {
        let db = open(&d);
        let bytes = data(700_000, 2);
        let r = db.create("invoices", fields("Persist", 1), vec![("document".into(), stage(&db, &bytes))]).unwrap();
        db.flush().unwrap();
        (r.id, bytes)
    };
    let db = open(&d);
    let r = db.get("invoices", &id).unwrap().unwrap();
    assert_eq!(read(&db, &r, "document"), bytes);
}

#[test]
fn the_schema_is_enforced() {
    let d = Dir::new();
    let db = open(&d);
    let bad = |f: Map<String, Value>| db.create("invoices", f, vec![]).unwrap_err().to_string();
    assert!(bad(json!({"amount": 1}).as_object().unwrap().clone()).contains("required"));
    assert!(bad(json!({"client": ""}).as_object().unwrap().clone()).contains("not empty"), "a required text field cannot be the empty string");
    assert!(bad(json!({"client": "   "}).as_object().unwrap().clone()).contains("not empty"));
    assert!(bad(json!({"client": 5}).as_object().unwrap().clone()).contains("must be"));
    assert!(bad(json!({"client": "x", "nope": 1}).as_object().unwrap().clone()).contains("no field"));
    assert!(bad(json!({"client": "x", "id": "forged"}).as_object().unwrap().clone()).contains("no field"));
    let big = db.stage_file("invoices", "text/html", None, Cursor::new(vec![1u8; 10])).unwrap();
    let e = db.create("invoices", fields("x", 1), vec![("document".into(), big)]).unwrap_err().to_string();
    assert!(e.contains("allowed"), "{e}");
}

#[test]
fn identical_files_are_stored_once() {
    let d = Dir::new();
    let db = open(&d);
    let bytes = data(2 << 20, 3);
    db.create("invoices", fields("a", 1), vec![("document".into(), stage(&db, &bytes))]).unwrap();
    let after_one = db.stats().unwrap().log_bytes;
    let p = stage(&db, &bytes);
    assert_eq!(p.new_chunks(), 0, "every chunk was already stored");
    db.create("invoices", fields("b", 2), vec![("document".into(), p)]).unwrap();
    assert_eq!(db.stats().unwrap().log_bytes, after_one);
}

#[test]
fn updating_a_large_file_writes_only_the_chunks_that_changed() {
    let d = Dir::new();
    let db = open(&d);
    let v1 = data(24 << 20, 4);
    let rec = db.create("invoices", fields("big", 1), vec![("document".into(), stage(&db, &v1))]).unwrap();
    let before = db.stats().unwrap().log_bytes;
    // insert 100 bytes in the middle: fixed-size blocks would shift every later block; content-defined chunks only touch the neighbourhood
    let mut v2 = v1.clone();
    let at = v2.len() / 2;
    v2.splice(at..at, data(100, 99));
    let p = stage(&db, &v2);
    assert!(p.new_bytes() < 1 << 20, "wrote {} bytes for a 100-byte edit of a {} byte file", p.new_bytes(), v1.len());
    let r2 = db.update("invoices", &rec.id, Map::new(), vec![("document".into(), p)], &[]).unwrap();
    assert!(db.stats().unwrap().log_bytes - before < 1 << 20);
    assert_eq!(read(&db, &r2, "document"), v2);
    assert!(r2.updated_at >= rec.updated_at);
}

#[test]
fn update_merges_fields_and_null_removes_one() {
    let d = Dir::new();
    let db = open(&d);
    let r = db.create("invoices", fields("c", 10), vec![]).unwrap();
    let r2 = db.update("invoices", &r.id, json!({"status": "archived", "amount": null}).as_object().unwrap().clone(), vec![], &[]).unwrap();
    assert_eq!(r2.fields["status"], "archived");
    assert!(!r2.fields.contains_key("amount"));
    assert_eq!(r2.fields["client"], "c");
    assert_eq!(db.get("invoices", &r.id).unwrap().unwrap(), r2);
    assert!(db.update("invoices", "missing", Map::new(), vec![], &[]).is_err());
}

#[test]
fn delete_hides_the_record_at_once_and_gc_gives_the_space_back() {
    let d = Dir::new();
    let db = open(&d);
    let keep = data(1 << 20, 5);
    let r_keep = db.create("invoices", fields("keep", 1), vec![("document".into(), stage(&db, &keep))]).unwrap();
    let mut ids = vec![];
    for i in 0..6 {
        let r = db.create("invoices", fields("tmp", i), vec![("document".into(), stage(&db, &data(2 << 20, 100 + i as u64)))]).unwrap();
        ids.push(r.id);
    }
    let full = db.stats().unwrap();
    assert!(full.segments >= 3, "the test needs several segments");
    for id in &ids {
        assert!(db.delete("invoices", id).unwrap());
        assert!(db.get("invoices", id).unwrap().is_none(), "gone immediately");
    }
    assert!(!db.delete("invoices", &ids[0]).unwrap());
    assert_eq!(db.stats().unwrap().gc_pending, 6);
    let (g, c) = db.collect(0.0).unwrap();
    assert_eq!(g.files_released, 6);
    assert!(c.bytes_reclaimed > 8 << 20, "reclaimed only {}", c.bytes_reclaimed);
    let after = db.stats().unwrap();
    assert!(after.log_bytes < full.log_bytes / 2, "{} -> {}", full.log_bytes, after.log_bytes);
    assert_eq!(after.gc_pending, 0);
    // the survivor is intact, including after the segment it lived in was rewritten
    assert_eq!(read(&db, &r_keep, "document"), keep);
}

#[test]
fn a_shared_file_survives_until_its_last_record_is_deleted() {
    let d = Dir::new();
    let db = open(&d);
    let bytes = data(1 << 20, 6);
    let a = db.create("invoices", fields("a", 1), vec![("document".into(), stage(&db, &bytes))]).unwrap();
    let b = db.create("invoices", fields("b", 2), vec![("document".into(), stage(&db, &bytes))]).unwrap();
    db.delete("invoices", &a.id).unwrap();
    db.collect(0.0).unwrap();
    assert_eq!(read(&db, &b, "document"), bytes);
    db.delete("invoices", &b.id).unwrap();
    db.collect(0.0).unwrap();
    assert!(db.manifest(&from_hex_32(&b.files["document"].id).unwrap()).is_err());
}

#[test]
fn a_file_re_attached_before_gc_runs_is_not_lost() {
    let d = Dir::new();
    let db = open(&d);
    let bytes = data(1 << 20, 7);
    let a = db.create("invoices", fields("a", 1), vec![("document".into(), stage(&db, &bytes))]).unwrap();
    db.delete("invoices", &a.id).unwrap();
    // same content comes back while the old copy is still queued for release
    let again = db.create("invoices", fields("a2", 1), vec![("document".into(), stage(&db, &bytes))]).unwrap();
    db.collect(0.0).unwrap();
    assert_eq!(read(&db, &again, "document"), bytes);
}

#[test]
fn staged_but_never_committed_files_leave_only_garbage_that_compaction_removes() {
    let d = Dir::new();
    let db = open(&d);
    db.create("invoices", fields("anchor", 1), vec![("document".into(), stage(&db, &data(5 << 20, 8)))]).unwrap();
    let before = db.stats().unwrap().log_bytes;
    drop(stage(&db, &data(6 << 20, 9))); // a crash between staging and commit looks exactly like this
    let after = db.stats().unwrap().log_bytes;
    assert!(after > before + (5 << 20));
    assert_eq!(db.count("invoices").unwrap(), 1);
    // push the orphans into sealed segments, then compact them away
    db.create("invoices", fields("more", 1), vec![("document".into(), stage(&db, &data(6 << 20, 10)))]).unwrap();
    let c = db.compact(0.0).unwrap();
    assert!(c.bytes_reclaimed >= 5 << 20, "{c:?}");
}

#[test]
fn a_torn_tail_in_the_log_is_cut_off_on_reopen() {
    let d = Dir::new();
    let (id, bytes) = {
        let db = open(&d);
        let bytes = data(900_000, 11);
        let r = db.create("invoices", fields("t", 1), vec![("document".into(), stage(&db, &bytes))]).unwrap();
        db.flush().unwrap();
        (r.id, bytes)
    };
    // simulate a power cut in the middle of an append: half an entry at the end of the newest segment
    let seg = std::fs::read_dir(d.0.join("blobs")).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).max().unwrap();
    let clean = std::fs::metadata(&seg).unwrap().len();
    let mut f = std::fs::OpenOptions::new().append(true).open(&seg).unwrap();
    use std::io::Write;
    f.write_all(b"PDB1\x01\x02\x03 garbage that is not a whole entry").unwrap();
    drop(f);
    let db = open(&d);
    assert_eq!(std::fs::metadata(&seg).unwrap().len(), clean, "the torn bytes were cut");
    let r = db.get("invoices", &id).unwrap().unwrap();
    assert_eq!(read(&db, &r, "document"), bytes);
    // and the log accepts new entries afterwards
    let r2 = db.create("invoices", fields("after", 2), vec![("document".into(), stage(&db, &data(300_000, 12)))]).unwrap();
    assert_eq!(read(&db, &r2, "document").len(), 300_000);
}

#[test]
fn range_reads_cross_chunk_boundaries_correctly() {
    let d = Dir::new();
    let db = open(&d);
    let bytes = data(3 << 20, 13);
    let r = db.create("invoices", fields("r", 1), vec![("document".into(), stage(&db, &bytes))]).unwrap();
    let m = db.manifest(&from_hex_32(&r.files["document"].id).unwrap()).unwrap();
    assert!(m.chunks.len() > 8);
    let mut edges = vec![0u64];
    let mut off = 0;
    for (_, l) in &m.chunks {
        off += *l as u64;
        edges.push(off);
    }
    let mut ranges = vec![(0, 1), (0, bytes.len() as u64), (5, 5), (bytes.len() as u64 - 1, bytes.len() as u64), (1000, 2_000_000)];
    for e in &edges[1..4] {
        ranges.push((e - 1, e + 1));
        ranges.push((*e, e + 10));
        ranges.push((e - 10, *e));
    }
    for (s, e) in ranges {
        let mut got = Vec::new();
        db.read_file_range(&m, s, e, &mut |b| {
            got.extend_from_slice(b);
            Ok(())
        })
        .unwrap();
        assert_eq!(got, &bytes[s as usize..e as usize], "range {s}..{e}");
    }
    // past the end is clamped, not an error
    let mut got = Vec::new();
    db.read_file_range(&m, bytes.len() as u64 - 3, bytes.len() as u64 + 100, &mut |b| {
        got.extend_from_slice(b);
        Ok(())
    })
    .unwrap();
    assert_eq!(got, &bytes[bytes.len() - 3..]);
}

#[test]
fn empty_and_tiny_files_work() {
    let d = Dir::new();
    let db = open(&d);
    for (i, len) in [0usize, 1, 100].into_iter().enumerate() {
        let b = data(len, 20 + i as u64);
        let r = db.create("invoices", fields("e", 1), vec![("document".into(), stage(&db, &b))]).unwrap();
        assert_eq!(read(&db, &r, "document"), b);
    }
}

#[test]
fn a_flipped_byte_in_the_log_is_detected_not_returned() {
    let d = Dir::new();
    let db = open(&d);
    let bytes = data(400_000, 30);
    let r = db.create("invoices", fields("c", 1), vec![("document".into(), stage(&db, &bytes))]).unwrap();
    db.flush().unwrap();
    let seg = std::fs::read_dir(d.0.join("blobs")).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).max().unwrap();
    let mut raw = std::fs::read(&seg).unwrap();
    raw[200] ^= 0xFF; // inside the first chunk's payload
    std::fs::write(&seg, raw).unwrap();
    let e = db.read_file(&from_hex_32(&r.files["document"].id).unwrap()).unwrap_err().to_string();
    assert!(e.contains("no longer match"), "{e}");
}

#[test]
fn readers_never_see_a_record_without_its_whole_file_while_writers_run() {
    let d = Dir::new();
    let db = open(&d);
    let stop = Arc::new(AtomicBool::new(false));
    let mut writers = vec![];
    for w in 0..4u64 {
        let db = db.clone();
        writers.push(std::thread::spawn(move || {
            for i in 0..12u64 {
                let b = data(200_000 + (w * 50_000) as usize, w * 1000 + i);
                let p = db.stage_file("invoices", "application/octet-stream", None, Cursor::new(b)).unwrap();
                let r = db.create("invoices", fields(&format!("w{w}"), i as i64), vec![("document".into(), p)]).unwrap();
                if i % 3 == 0 {
                    db.delete("invoices", &r.id).unwrap();
                }
            }
        }));
    }
    let reader = {
        let db = db.clone();
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut checked = 0u64;
            while !stop.load(Ordering::Relaxed) {
                let mut recs = vec![];
                db.scan("invoices", true, |r| {
                    recs.push(r);
                    Ok(true)
                })
                .unwrap();
                for r in recs {
                    // a record may be deleted between the scan and the read, but while it exists its file must be complete and valid
                    if let Some(f) = r.files.get("document") {
                        match db.manifest(&from_hex_32(&f.id).unwrap()) {
                            Ok(m) => {
                                if db.get("invoices", &r.id).unwrap().is_some() {
                                    let mut n = 0u64;
                                    db.read_file_range(&m, 0, m.size, &mut |b| {
                                        n += b.len() as u64;
                                        Ok(())
                                    })
                                    .expect("a listed record's file must read back completely");
                                    assert_eq!(n, f.size);
                                    checked += 1;
                                }
                            }
                            Err(_) => assert!(db.get("invoices", &r.id).unwrap().is_none(), "record exists but its manifest does not"),
                        }
                    }
                }
            }
            checked
        })
    };
    for w in writers {
        w.join().unwrap();
    }
    stop.store(true, Ordering::Relaxed);
    let checked = reader.join().unwrap();
    assert!(checked > 0);
    assert_eq!(db.count("invoices").unwrap(), 4 * 12 - 4 * 4);
}

#[test]
fn ids_sort_in_creation_order_and_scan_directions_agree() {
    let d = Dir::new();
    let db = open(&d);
    let ids: Vec<String> = (0..50).map(|i| db.create("invoices", fields("o", i), vec![]).unwrap().id).collect();
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(ids, sorted);
    let mut newest = vec![];
    db.scan("invoices", true, |r| {
        newest.push(r.id);
        Ok(newest.len() < 10)
    })
    .unwrap();
    assert_eq!(newest, ids.iter().rev().take(10).cloned().collect::<Vec<_>>());
}

#[test]
fn collection_names_and_duplicates_are_checked() {
    let d = Dir::new();
    let db = open(&d);
    let mut c = schema();
    assert!(db.create_collection(c.clone()).is_err(), "duplicate");
    c.name = "Bad Name".into();
    assert!(db.create_collection(c).is_err());
    assert!(db.collection("missing").is_err());
}

#[test]
fn interval_durability_acknowledges_at_once_and_still_survives_a_clean_reopen() {
    use powerdb::Durability;
    let d = Dir::new();
    let opts = Options { durability: Durability::Interval(20), ..small_opts() };
    let id = {
        let db = Db::open(&d.0, opts.clone()).unwrap();
        db.create_collection(schema()).unwrap();
        let t = std::time::Instant::now();
        let bytes = data(300_000, 40);
        let r = db.create("invoices", fields("fast", 1), vec![("document".into(), stage(&db, &bytes))]).unwrap();
        assert!(t.elapsed() < std::time::Duration::from_millis(500), "an acknowledged write does not wait for a disk flush");
        std::thread::sleep(std::time::Duration::from_millis(80)); // let the background fsync run at least once
        r.id
    }; // dropped: the syncer thread holds only a weak reference, so nothing keeps the database open
    let db = Db::open(&d.0, opts).unwrap(); // would fail on a held lock if the syncer still owned the old instance
    assert!(db.get("invoices", &id).unwrap().is_some());
}
