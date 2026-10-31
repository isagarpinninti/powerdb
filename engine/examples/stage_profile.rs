use powerdb::model::{CollectionDef, Rules};
use powerdb::storage::{Db, Durability, Options};
use std::io::Cursor;
use std::time::Instant;

fn main() {
    let mut x = 88172645463325252u64;
    let data: Vec<u8> = (0..256usize << 20).map(|_| { x ^= x << 13; x ^= x >> 7; x ^= x << 17; (x >> 24) as u8 }).collect();
    for (label, sync, seg) in [("sync off, 256 MiB segments", false, 256u64 << 20), ("sync off, 1 GiB segments (no roll during the file)", false, 1 << 30), ("sync on, 1 GiB segments", true, 1 << 30)] {
        let dir = std::env::temp_dir().join(format!("pdb-sp-{}-{}", std::process::id(), label.len()));
        let db = Db::open(&dir, Options { durability: if sync { Durability::Commit } else { Durability::Never }, segment_bytes: seg, ..Options::default() }).unwrap();
        db.create_collection(CollectionDef { name: "c".into(), fields: vec![], rules: Rules::default(), encrypt: false, created_at: 0 }).unwrap();
        let t = Instant::now();
        let p = db.stage_file("c", "x/y", None, Cursor::new(&data)).unwrap();
        println!("{label}: stage 256 MiB = {:.2} s = {:.0} MB/s ({} chunks)", t.elapsed().as_secs_f64(), 268.435456 / t.elapsed().as_secs_f64(), p.total_chunks());
        drop(p); drop(db);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
