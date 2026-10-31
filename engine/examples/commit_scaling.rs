use powerdb::model::{CollectionDef, Rules};
use powerdb::storage::{Db, Durability, Options};
use serde_json::{json, Map, Value};
use std::time::Instant;

fn main() {
    let sync = std::env::args().nth(1).as_deref() != Some("nosync");
    for threads in [1usize, 4, 16] {
        let dir = std::env::temp_dir().join(format!("pdb-cs-{}-{threads}", std::process::id()));
        let db = Db::open(&dir, Options { durability: if sync { Durability::Commit } else { Durability::Never }, ..Options::default() }).unwrap();
        db.create_collection(CollectionDef { name: "c".into(), fields: vec![], rules: Rules::default(), encrypt: false, created_at: 0 }).unwrap();
        let per = 600 / threads.min(4).max(1) * if sync { 1 } else { 40 };
        let t = Instant::now();
        let hs: Vec<_> = (0..threads).map(|_| { let db = db.clone(); std::thread::spawn(move || { for _ in 0..per { let f: Map<String, Value> = json!({}).as_object().unwrap().clone(); db.create("c", f, vec![]).unwrap(); } }) }).collect();
        for h in hs { h.join().unwrap(); }
        let n = per * threads;
        println!("sync={sync} threads={threads:>2}: {:>8.0} commits/s  ({n} commits in {:.2}s)", n as f64 / t.elapsed().as_secs_f64(), t.elapsed().as_secs_f64());
        drop(db);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
