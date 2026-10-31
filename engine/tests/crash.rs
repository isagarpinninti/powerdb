//! Process-crash safety: a writer is killed with SIGKILL-equivalent force in the middle of committing, over and over,
//! and every write it had acknowledged must still be there, with a structurally consistent store.
//!
//! This covers the process dying, not the machine losing power: a power cut also depends on the disk honoring fsync,
//! which a test on one machine cannot prove.

use powerdb::model::{CollectionDef, FieldDef, FieldKind, Rules};
use powerdb::storage::chunker::ChunkSizes;
use powerdb::util::new_id;
use powerdb::{Db, Durability, Options};
use serde_json::json;
use std::collections::HashSet;
use std::io::{BufRead, BufReader, Cursor, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

fn opts() -> Options {
    Options {
        durability: Durability::Commit,
        segment_bytes: 2 << 20,
        chunk: ChunkSizes {
            min: 16 << 10,
            avg: 64 << 10,
            max: 256 << 10,
        },
        verify_reads: true,
        cache_bytes: None,
    }
}

fn schema() -> CollectionDef {
    let f = |name: &str, kind, max| FieldDef {
        name: name.into(),
        kind,
        required: false,
        max,
        mime: vec![],
        relation: None,
    };
    CollectionDef {
        name: "docs".into(),
        fields: vec![
            f("n", FieldKind::Number, None),
            f("blob", FieldKind::File, Some(8 << 20)),
        ],
        rules: Rules::default(),
        encrypt: false,
        created_at: 0,
    }
}

fn bytes(len: usize, seed: u64) -> Vec<u8> {
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

/// Runs only as the child of `kill_the_writer_repeatedly`; a normal test run returns at once.
#[test]
fn crash_writer_child() {
    let Ok(dir) = std::env::var("POWERDB_CRASH_DIR") else {
        return;
    };
    let db = Db::open(&dir, opts()).unwrap();
    if db.collection("docs").is_err() {
        db.create_collection(schema()).unwrap();
    }
    let mut out = std::io::stdout();
    let mut previous: Option<String> = None;
    for i in 0u64.. {
        // every third file repeats earlier content so deduplication and refcounts are exercised under the kill too
        let seed = if i % 3 == 0 { i % 6 } else { i };
        let file = db
            .stage_file(
                "docs",
                "application/octet-stream",
                Some("x.bin"),
                Cursor::new(bytes(200_000, seed)),
            )
            .unwrap();
        let rec = db
            .create(
                "docs",
                json!({"n": i}).as_object().unwrap().clone(),
                vec![("blob".into(), file)],
            )
            .unwrap();
        writeln!(out, "ACK {}", rec.id).unwrap();
        out.flush().unwrap();
        if i % 4 == 3 {
            if let Some(p) = previous.take() {
                // announced first: a kill between the delete and its acknowledgement leaves it legitimately either way
                writeln!(out, "DELETING {p}").unwrap();
                out.flush().unwrap();
                db.delete("docs", &p).unwrap();
                writeln!(out, "DEL {p}").unwrap();
                out.flush().unwrap();
            }
        }
        previous = Some(rec.id);
    }
}

#[test]
fn kill_the_writer_repeatedly() {
    let dir = std::env::temp_dir().join(format!("powerdb-crash-{}", new_id()));
    let mut acked = HashSet::new();
    let mut deleted = HashSet::new();
    let mut maybe_deleted = HashSet::new();
    for round in 0..5u64 {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "crash_writer_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("POWERDB_CRASH_DIR", &dir)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        std::thread::sleep(Duration::from_millis(400 + round * 130));
        child.kill().unwrap(); // SIGKILL on Unix, TerminateProcess on Windows: no cleanup code runs
        child.wait().unwrap();
        reader.join().unwrap();
        for line in rx.try_iter() {
            if let Some(id) = line.strip_prefix("ACK ") {
                acked.insert(id.to_string());
            } else if let Some(id) = line.strip_prefix("DELETING ") {
                maybe_deleted.insert(id.to_string());
            } else if let Some(id) = line.strip_prefix("DEL ") {
                deleted.insert(id.to_string());
            }
        }

        let db = Db::open(&dir, opts())
            .unwrap_or_else(|e| panic!("round {round}: reopening after a kill failed: {e}"));
        for id in acked.difference(&maybe_deleted) {
            assert!(
                db.get("docs", id).unwrap().is_some(),
                "round {round}: acknowledged record {id} is missing after a kill"
            );
        }
        for id in &deleted {
            assert!(
                db.get("docs", id).unwrap().is_none(),
                "round {round}: deleted record {id} came back"
            );
        }
        let report = db.verify(true).unwrap();
        assert!(
            report.problems.is_empty(),
            "round {round}: {:?}",
            report.problems
        );
    }
    assert!(
        acked.len() >= 5,
        "the writer made almost no progress ({} acknowledged)",
        acked.len()
    );
    let _ = std::fs::remove_dir_all(&dir);
}
