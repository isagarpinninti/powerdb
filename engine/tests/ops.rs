use powerdb::model::{CollectionDef, FieldDef, FieldKind, Rules};
use powerdb::security::crypto::ChaChaCodec;
use powerdb::security::secrets::Secrets;
use powerdb::storage::chunker::ChunkSizes;
use powerdb::util::{from_hex_32, new_id};
use powerdb::{Db, Durability, Error, Options, api, ops, server};
use serde_json::{Map, Value, json};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

struct Dir(PathBuf);
impl Dir {
    fn new(tag: &str) -> Self {
        Dir(std::env::temp_dir().join(format!("powerdb-ops-{tag}-{}", new_id())))
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn opts() -> Options {
    Options {
        durability: Durability::Never,
        segment_bytes: 4 << 20,
        chunk: ChunkSizes {
            min: 16 << 10,
            avg: 64 << 10,
            max: 256 << 10,
        },
        verify_reads: true,
        cache_bytes: None,
    }
}

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

fn col(name: &str, encrypt: bool) -> CollectionDef {
    CollectionDef {
        name: name.into(),
        fields: vec![
            FieldDef {
                name: "doc".into(),
                kind: FieldKind::File,
                required: false,
                max: None,
                mime: vec![],
                relation: None,
            },
            FieldDef {
                name: "note".into(),
                kind: FieldKind::Text,
                required: false,
                max: None,
                mime: vec![],
                relation: None,
            },
        ],
        rules: Rules::default(),
        encrypt,
        created_at: 0,
    }
}

/// A database with plain and encrypted records, built through the same wiring the server uses (so the encryption codec is registered).
fn populated(dir: &Path) -> (Db, Vec<(String, String, Vec<u8>)>) {
    let (st, _) = server::build_state(dir, opts(), api::Config::default()).unwrap();
    let db = st.db.clone();
    db.create_collection(col("plain", false)).unwrap();
    db.create_collection(col("vault", true)).unwrap();
    let mut made = vec![];
    for (c, seed) in [("plain", 1u64), ("plain", 2), ("vault", 3)] {
        let bytes = data(500_000, seed);
        let p = db
            .stage_file(
                c,
                "application/octet-stream",
                Some("f.bin"),
                Cursor::new(bytes.clone()),
            )
            .unwrap();
        let mut f = Map::new();
        f.insert("note".into(), Value::String(format!("record {seed}")));
        let r = db.create(c, f, vec![("doc".into(), p)]).unwrap();
        made.push((c.to_string(), r.id, bytes));
    }
    (db, made)
}

fn read(db: &Db, c: &str, id: &str) -> Vec<u8> {
    let r = db.get(c, id).unwrap().unwrap();
    db.read_file(&from_hex_32(&r.files["doc"].id).unwrap())
        .unwrap()
}

#[test]
fn a_data_directory_can_be_open_in_only_one_place() {
    let d = Dir::new("lock");
    let a = Db::open(&d.0, opts()).unwrap();
    match Db::open(&d.0, opts()) {
        Err(Error::Conflict(m)) => assert!(m.contains("in use"), "{m}"),
        Err(e) => panic!("wrong error: {e}"),
        Ok(_) => panic!("a second open of the same directory must be refused"),
    }
    drop(a);
    Db::open(&d.0, opts()).expect("the lock is released when the first database is dropped");
}

#[test]
fn init_makes_a_usable_directory_and_never_touches_an_existing_one() {
    let d = Dir::new("init");
    let r = ops::init(&d.0).unwrap();
    assert!(
        r.admin_key
            .as_deref()
            .is_some_and(|k| k.starts_with("pdb_live_"))
    );
    assert!(d.0.join("powerdb.toml").is_file() && d.0.join("secret.key").is_file());
    let cfg = powerdb::config::FileConfig::load(&d.0)
        .unwrap()
        .expect("the settings file parses");
    assert_eq!(cfg.durability.as_deref(), Some("commit"));
    let again = ops::init(&d.0).unwrap_err().to_string();
    assert!(again.contains("not empty"), "{again}");
    // the key it printed works
    let (st, boot) = server::build_state(&d.0, opts(), api::Config::default()).unwrap();
    assert!(
        boot.admin_key.is_none(),
        "a second start does not mint another admin key"
    );
    assert!(
        st.auth
            .verify_key(r.admin_key.as_deref().unwrap())
            .unwrap()
            .is_admin()
    );
}

#[test]
fn backup_then_restore_gives_back_every_record_and_file() {
    let (src, bak, dst) = (Dir::new("src"), Dir::new("bak"), Dir::new("dst"));
    let (db, made) = populated(&src.0);
    db.flush().unwrap();
    drop(db);
    let r = ops::backup(&src.0, &bak.0, true).unwrap();
    assert!(r.includes_secret && r.files > 3);
    assert!(bak.0.join("BACKUP.json").is_file());
    let restored = ops::restore(&bak.0, &dst.0).unwrap();
    assert_eq!(restored.files, r.files);

    let (st, _) = server::build_state(&dst.0, opts(), api::Config::default()).unwrap();
    for (c, id, bytes) in &made {
        assert_eq!(&read(&st.db, c, id), bytes, "{c}/{id}");
    }
    let rep = st.db.verify(true).unwrap();
    assert!(rep.problems.is_empty(), "{:?}", rep.problems);
    assert_eq!(rep.records, 3);
}

#[test]
fn a_backup_leaves_the_secret_out_unless_asked() {
    let (src, bak) = (Dir::new("src"), Dir::new("bak"));
    let (db, _) = populated(&src.0);
    db.flush().unwrap();
    drop(db);
    let r = ops::backup(&src.0, &bak.0, false).unwrap();
    assert!(!r.includes_secret);
    assert!(
        !bak.0.join("secret.key").exists(),
        "a stolen backup must not carry the key to its own encrypted files"
    );
    assert!(bak.0.join("meta").is_dir() && bak.0.join("blobs").is_dir());
}

#[test]
fn backup_refuses_a_database_that_is_running_and_a_destination_that_is_not_empty() {
    let (src, bak) = (Dir::new("src"), Dir::new("bak"));
    let (db, _) = populated(&src.0);
    let e = ops::backup(&src.0, &bak.0, false).unwrap_err().to_string();
    assert!(e.contains("in use"), "{e}");
    drop(db);
    std::fs::create_dir_all(&bak.0).unwrap();
    std::fs::write(bak.0.join("something"), b"x").unwrap();
    assert!(
        ops::backup(&src.0, &bak.0, false)
            .unwrap_err()
            .to_string()
            .contains("not empty")
    );
}

#[test]
fn a_damaged_or_tampered_backup_is_refused_before_anything_is_written() {
    let (src, bak, dst) = (Dir::new("src"), Dir::new("bak"), Dir::new("dst"));
    let (db, _) = populated(&src.0);
    db.flush().unwrap();
    drop(db);
    ops::backup(&src.0, &bak.0, true).unwrap();
    // flip a byte in a blob file
    let blob = std::fs::read_dir(bak.0.join("blobs"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mut raw = std::fs::read(&blob).unwrap();
    raw[100] ^= 0xFF;
    std::fs::write(&blob, raw).unwrap();
    let e = ops::restore(&bak.0, &dst.0).unwrap_err().to_string();
    assert!(e.contains("checksum") || e.contains("damaged"), "{e}");
    assert!(
        !dst.0.exists() || std::fs::read_dir(&dst.0).unwrap().next().is_none(),
        "nothing was restored"
    );

    // a manifest that tries to write outside the target directory
    let man_path = bak.0.join("BACKUP.json");
    let mut m: Value = serde_json::from_slice(&std::fs::read(&man_path).unwrap()).unwrap();
    m["files"][0]["path"] = json!("../escape.txt");
    std::fs::write(&man_path, m.to_string()).unwrap();
    assert!(
        ops::restore(&bak.0, &dst.0)
            .unwrap_err()
            .to_string()
            .contains("unsafe path")
    );
    // not a backup at all
    assert!(
        ops::restore(&src.0, &Dir::new("x").0)
            .unwrap_err()
            .to_string()
            .contains("not a PowerDB backup")
    );
}

#[test]
fn restore_only_into_a_new_directory() {
    let (src, bak) = (Dir::new("src"), Dir::new("bak"));
    let (db, _) = populated(&src.0);
    db.flush().unwrap();
    drop(db);
    ops::backup(&src.0, &bak.0, true).unwrap();
    assert!(
        ops::restore(&bak.0, &src.0)
            .unwrap_err()
            .to_string()
            .contains("not empty"),
        "never over a live database"
    );
}

#[test]
fn the_checker_passes_a_healthy_database_and_finds_injected_damage() {
    let d = Dir::new("check");
    let (db, made) = populated(&d.0);
    let ok = db.verify(true).unwrap();
    assert!(ok.problems.is_empty(), "{:?}", ok.problems);
    assert_eq!((ok.records, ok.files), (3, 3));
    assert!(
        ok.chunks > 5 && ok.chunk_bytes_checked >= 1_500_000,
        "content was really re-read ({} bytes)",
        ok.chunk_bytes_checked
    );

    // a wrong reference count: the dangerous kind of bug
    let refs = db.keyspace("file_refs").unwrap();
    let (c, id, _) = &made[0];
    let rec = db.get(c, id).unwrap().unwrap();
    let fid = from_hex_32(&rec.files["doc"].id).unwrap();
    refs.insert(fid.to_vec(), 5u64.to_le_bytes().to_vec())
        .unwrap();
    let bad = db.verify(false).unwrap();
    assert!(
        bad.problems
            .iter()
            .any(|p| p.contains("counted 5 records using it, but 1 do")),
        "{:?}",
        bad.problems
    );
    refs.insert(fid.to_vec(), 1u64.to_le_bytes().to_vec())
        .unwrap();
    assert!(db.verify(false).unwrap().problems.is_empty());

    // a file that a record points at vanishes
    db.keyspace("files").unwrap().remove(fid.to_vec()).unwrap();
    assert!(
        db.verify(false)
            .unwrap()
            .problems
            .iter()
            .any(|p| p.contains("is missing"))
    );
}

#[test]
fn the_checker_notices_a_flipped_byte_in_a_stored_chunk() {
    let d = Dir::new("flip");
    let (db, _) = populated(&d.0);
    db.flush().unwrap();
    let seg = std::fs::read_dir(d.0.join("blobs"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .min()
        .unwrap();
    let mut raw = std::fs::read(&seg).unwrap();
    raw[300] ^= 0x01;
    std::fs::write(&seg, raw).unwrap();
    let r = db.verify(true).unwrap();
    assert!(!r.problems.is_empty(), "a corrupted chunk must be reported");
    assert!(
        db.verify(false).unwrap().problems.is_empty(),
        "the quick check, which does not re-read content, cannot see it: that is what the full one is for"
    );
}

#[test]
fn encrypted_chunks_are_authenticated_by_the_checker_when_the_key_is_known() {
    let d = Dir::new("enc");
    let (db, _) = populated(&d.0);
    drop(db);
    let db = Db::open(&d.0, opts()).unwrap(); // no codec registered: it cannot read the encrypted chunks
    let without = db.verify(true).unwrap();
    assert!(
        without.problems.iter().any(|p| p.contains("no codec")),
        "{:?}",
        without.problems
    );
    db.register_codec(Arc::new(ChaChaCodec::new(
        &Secrets::load_or_create(&d.0).unwrap(),
    )));
    assert!(db.verify(true).unwrap().problems.is_empty());
}

#[tokio::test]
async fn status_reports_a_running_server_and_an_absent_one() {
    let d = Dir::new("status");
    let (st, _) = server::build_state(&d.0, opts(), api::Config::default()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, api::router(st)).await.unwrap() });
    let up = tokio::task::spawn_blocking(move || ops::status(addr))
        .await
        .unwrap();
    assert!(up.unwrap().contains("is running"));
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let gone = free.local_addr().unwrap();
    drop(free);
    assert!(
        tokio::task::spawn_blocking(move || ops::status(gone))
            .await
            .unwrap()
            .is_err()
    );
    // something that is not PowerDB
    let other = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let oaddr = other.local_addr().unwrap();
    std::thread::spawn(move || {
        if let Ok((mut s, _)) = other.accept() {
            use std::io::{Read, Write};
            let mut b = [0u8; 512];
            let _ = s.read(&mut b);
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\n\r\nhello");
        }
    });
    assert!(
        tokio::task::spawn_blocking(move || ops::status(oaddr))
            .await
            .unwrap()
            .is_err(),
        "a different web server is not a healthy PowerDB"
    );
}

#[test]
fn a_hot_backup_taken_during_heavy_writes_restores_complete_and_consistent() {
    use std::collections::HashSet;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    let src = Dir::new("hot-src");
    let (db, originals) = populated(&src.0);
    let stop = Arc::new(AtomicBool::new(false));
    let acked: Arc<Mutex<Vec<(String, u64)>>> = Arc::new(Mutex::new(vec![]));
    let maybe_deleted: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
    let seeds = Arc::new(AtomicU64::new(1000));
    let mut threads = vec![];

    for _ in 0..3 {
        let (db, stop, acked, seeds) = (db.clone(), stop.clone(), acked.clone(), seeds.clone());
        threads.push(std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let seed = seeds.fetch_add(1, Ordering::Relaxed);
                // every fifth file repeats earlier content, so shared chunks (reference counts above one) are copied too
                let content = data(150_000, if seed % 5 == 0 { 1000 } else { seed });
                let p = db
                    .stage_file(
                        "plain",
                        "application/octet-stream",
                        Some("f.bin"),
                        Cursor::new(content),
                    )
                    .unwrap();
                let r = db
                    .create("plain", Map::new(), vec![("doc".into(), p)])
                    .unwrap();
                acked
                    .lock()
                    .unwrap()
                    .push((r.id, if seed % 5 == 0 { 1000 } else { seed }));
            }
        }));
    }
    {
        let (db, stop, acked, md) = (
            db.clone(),
            stop.clone(),
            acked.clone(),
            maybe_deleted.clone(),
        );
        threads.push(std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(15));
                let victim = {
                    let a = acked.lock().unwrap();
                    a.get(a.len() / 3).map(|(id, _)| id.clone())
                };
                if let Some(id) = victim {
                    md.lock().unwrap().insert(id.clone()); // announced first: a backup may see it either way
                    let _ = db.delete("plain", &id);
                }
            }
        }));
    }
    {
        let (db, stop) = (db.clone(), stop.clone());
        threads.push(std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                db.collect(0.0).unwrap(); // rewrites and deletes segments: must wait for a backup, not corrupt it
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }));
    }

    let out = Dir::new("hot-out");
    let mut taken = vec![];
    for round in 0..3 {
        std::thread::sleep(std::time::Duration::from_millis(250));
        let must: Vec<(String, u64)> = acked.lock().unwrap().clone();
        let dest = out.0.join(format!("b{round}"));
        let report = ops::backup_online(&db, &dest, round == 0, round == 2).unwrap();
        assert!(report.files > 3, "{report:?}");
        assert_eq!(report.includes_secret, round == 0);
        taken.push((dest, must));
    }
    stop.store(true, Ordering::Relaxed);
    for t in threads {
        t.join().unwrap();
    }
    let deleted = maybe_deleted.lock().unwrap().clone();
    let secrets = Secrets::load_existing(&src.0).unwrap().unwrap();

    for (i, (dest, must)) in taken.iter().enumerate() {
        let restored = Dir::new("hot-restored");
        ops::restore(dest, &restored.0).unwrap();
        let copy = Db::open(&restored.0, opts()).unwrap();
        copy.register_codec(Arc::new(ChaChaCodec::new(&secrets)));
        let report = copy.verify(true).unwrap();
        assert!(
            report.problems.is_empty(),
            "backup {i}: {:?}",
            report.problems
        );
        let mut present = 0;
        for (id, seed) in must {
            if deleted.contains(id) {
                continue;
            }
            let r = copy.get("plain", id).unwrap().unwrap_or_else(|| {
                panic!("backup {i}: record {id}, acknowledged before the backup began, is missing")
            });
            let bytes = copy
                .read_file(&from_hex_32(&r.files["doc"].id).unwrap())
                .unwrap();
            assert_eq!(
                bytes,
                data(150_000, *seed),
                "backup {i}: record {id} has the wrong content"
            );
            present += 1;
        }
        assert!(present > 0, "backup {i} checked nothing");
        for (c, id, bytes) in &originals {
            assert_eq!(&read(&copy, c, id), bytes, "backup {i}: original {c}/{id}");
        }
    }
    assert!(
        taken.iter().map(|(_, m)| m.len()).max().unwrap() > taken[0].1.len(),
        "later backups should have seen more writes than the first"
    );
}

#[test]
fn an_online_backup_refuses_an_existing_destination_and_leaves_the_secret_out_by_default() {
    let src = Dir::new("hot-refuse");
    let (db, _) = populated(&src.0);
    let out = Dir::new("hot-refuse-out");
    std::fs::create_dir_all(&out.0).unwrap();
    assert!(matches!(
        ops::backup_online(&db, &out.0, false, false),
        Err(Error::Conflict(_))
    ));
    let dest = out.0.join("ok");
    let r = ops::backup_online(&db, &dest, false, false).unwrap();
    assert!(!r.includes_secret && !dest.join("secret.key").exists());
    assert!(dest.join("BACKUP.json").is_file() && dest.join("FORMAT").is_file());
    assert!(
        !dest.join("LOCK").exists(),
        "the lock file is not part of a backup"
    );
}
