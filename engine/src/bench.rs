//! `powerdb bench`: measures the embedded storage engine on this machine. No network, HTTP, authentication or rule evaluation is involved, so these numbers
//! are the engine's own cost per operation, not what a client sees.

use powerdb::model::{CollectionDef, FieldDef, FieldKind, Rules};
use powerdb::storage::{Db, Durability, Options};
use powerdb::util::{from_hex_32, new_id};
use serde_json::{Map, Value, json};
use std::io::Cursor;
use std::time::Instant;

fn pct(v: &mut [u64], p: f64) -> f64 {
    v.sort_unstable();
    v[((v.len() as f64 * p) as usize).min(v.len() - 1)] as f64 / 1000.0
}

fn report(name: &str, lat: &mut [u64], total_s: f64) {
    let n = lat.len();
    println!(
        "  {name:<34} {:>9.0} ops/s   p50 {:>8.1} us   p99 {:>8.1} us   max {:>9.1} us   ({n} ops)",
        n as f64 / total_s,
        pct(lat, 0.5),
        pct(lat, 0.99),
        pct(lat, 1.0)
    );
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

pub fn run(args: &[String]) -> i32 {
    let get = |name: &str, d: usize| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .and_then(|v| v.parse().ok())
            .unwrap_or(d)
    };
    let n = get("--records", 20_000);
    let big_mib = get("--big-mib", 256);
    let durability = if args.iter().any(|a| a == "--sync") {
        Durability::Commit
    } else if args.iter().any(|a| a == "--interval") {
        Durability::Interval(100)
    } else {
        Durability::Never
    };
    let dir = std::env::temp_dir().join(format!("powerdb-bench-{}", new_id()));
    let db = match Db::open(
        &dir,
        Options {
            durability,
            ..Options::default()
        },
    ) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("powerdb: {e}");
            return 1;
        }
    };
    let field = |name: &str, kind| FieldDef {
        name: name.into(),
        kind,
        required: false,
        max: None,
        mime: vec![],
        relation: None,
    };
    db.create_collection(CollectionDef {
        name: "items".into(),
        fields: vec![
            field("title", FieldKind::Text),
            field("n", FieldKind::Number),
            field("tag", FieldKind::Text),
            field("blob", FieldKind::File),
        ],
        rules: Rules::default(),
        encrypt: false,
        created_at: 0,
    })
    .expect("create collection");
    println!(
        "PowerDB storage engine, MEASURED on this machine ({} cores), embedded: no network, HTTP, auth or rules. Durability: {}",
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1),
        match durability {
            Durability::Commit => "fsync before every acknowledgement",
            Durability::Interval(_) => "background fsync every 100 ms",
            Durability::Never =>
                "OS buffers only (a power cut can lose recent commits); --sync or --interval for the safer modes",
        }
    );
    println!("records, one writer thread:");

    let fields = |i: usize| -> Map<String, Value> {
        let tag = ["a", "b", "c"][i % 3];
        json!({"title": format!("item number {i}"), "n": i, "tag": tag})
            .as_object()
            .unwrap()
            .clone()
    };
    let mut ids = Vec::with_capacity(n);
    let mut lat = Vec::with_capacity(n);
    let t = Instant::now();
    for i in 0..n {
        let s = Instant::now();
        let r = db.create("items", fields(i), vec![]).expect("create");
        lat.push(s.elapsed().as_nanos() as u64);
        ids.push(r.id);
    }
    report("create (small record)", &mut lat, t.elapsed().as_secs_f64());

    let mut lat = Vec::with_capacity(n);
    let t = Instant::now();
    for i in 0..n {
        let id = &ids[(i * 7919) % n];
        let s = Instant::now();
        let r = db.get("items", id).expect("get");
        lat.push(s.elapsed().as_nanos() as u64);
        assert!(r.is_some());
    }
    report("read by id", &mut lat, t.elapsed().as_secs_f64());

    let mut lat = Vec::new();
    let t = Instant::now();
    for _ in 0..2000 {
        let s = Instant::now();
        let mut got = 0;
        db.scan("items", true, |_| {
            got += 1;
            Ok(got < 20)
        })
        .expect("scan");
        lat.push(s.elapsed().as_nanos() as u64);
    }
    report("list newest 20", &mut lat, t.elapsed().as_secs_f64());

    let mut lat = Vec::with_capacity(n);
    let t = Instant::now();
    for (i, id) in ids.iter().enumerate() {
        let s = Instant::now();
        db.update(
            "items",
            id,
            json!({"tag": "updated", "n": i + 1})
                .as_object()
                .unwrap()
                .clone(),
            vec![],
            &[],
        )
        .expect("update");
        lat.push(s.elapsed().as_nanos() as u64);
    }
    report(
        "update (patch two fields)",
        &mut lat,
        t.elapsed().as_secs_f64(),
    );

    let mut lat = Vec::with_capacity(n);
    let t = Instant::now();
    for id in &ids {
        let s = Instant::now();
        db.delete("items", id).expect("delete");
        lat.push(s.elapsed().as_nanos() as u64);
    }
    report("delete (tombstone)", &mut lat, t.elapsed().as_secs_f64());

    println!("records with a 100 KiB file, one writer thread:");
    let m = 1000.min(n);
    let mut lat = Vec::with_capacity(m);
    let mut fids = Vec::new();
    let t = Instant::now();
    for i in 0..m {
        let bytes = data(100 << 10, i as u64);
        let s = Instant::now();
        let p = db
            .stage_file(
                "items",
                "application/octet-stream",
                None,
                Cursor::new(bytes),
            )
            .expect("stage");
        let r = db
            .create("items", fields(i), vec![("blob".into(), p)])
            .expect("create");
        lat.push(s.elapsed().as_nanos() as u64);
        fids.push(r.files["blob"].id.clone());
    }
    report(
        "create with file (stage + commit)",
        &mut lat,
        t.elapsed().as_secs_f64(),
    );
    let mut lat = Vec::with_capacity(m);
    let t = Instant::now();
    for f in &fids {
        let s = Instant::now();
        let v = db.read_file(&from_hex_32(f).unwrap()).expect("read file");
        lat.push(s.elapsed().as_nanos() as u64);
        assert_eq!(v.len(), 100 << 10);
    }
    report("read a 100 KiB file", &mut lat, t.elapsed().as_secs_f64());

    println!("one large file ({big_mib} MiB):");
    let big = data(big_mib << 20, 7);
    let t = Instant::now();
    let p = db
        .stage_file("items", "application/octet-stream", None, Cursor::new(&big))
        .expect("stage big");
    let staged = t.elapsed().as_secs_f64();
    let new_chunks = p.new_chunks();
    let r = db
        .create("items", fields(0), vec![("blob".into(), p)])
        .expect("create big");
    let total = t.elapsed().as_secs_f64();
    println!(
        "  ingest: {:.0} MB/s ({:.2} s: chunking, hashing, log write {:.2} s; commit {:.4} s), {new_chunks} chunks",
        big.len() as f64 / 1e6 / total,
        total,
        staged,
        total - staged
    );
    let m = db
        .manifest(&from_hex_32(&r.files["blob"].id).unwrap())
        .unwrap();
    let t = Instant::now();
    let mut n_read = 0u64;
    db.read_file_range(&m, 0, m.size, &mut |b| {
        n_read += b.len() as u64;
        Ok(())
    })
    .expect("read big");
    println!(
        "  sequential read (warm OS cache, chunks verified by hash): {:.0} MB/s",
        n_read as f64 / 1e6 / t.elapsed().as_secs_f64()
    );
    let mut lat = Vec::new();
    let t = Instant::now();
    for i in 0..500u64 {
        let off = (i * 2_654_435_761) % (m.size - 4096);
        let s = Instant::now();
        db.read_file_range(&m, off, off + 4096, &mut |_| Ok(()))
            .expect("range");
        lat.push(s.elapsed().as_nanos() as u64);
    }
    report(
        "  4 KiB range read at random offset",
        &mut lat,
        t.elapsed().as_secs_f64(),
    );
    let t = Instant::now();
    let mut edited = big.clone();
    let at = edited.len() / 2;
    edited.splice(at..at, data(1000, 3));
    let p = db
        .stage_file(
            "items",
            "application/octet-stream",
            None,
            Cursor::new(&edited),
        )
        .expect("stage edit");
    println!(
        "  edit 1000 bytes in the middle and re-store: wrote {:.2} MiB of {big_mib} MiB ({} of {} chunks new) in {:.2} s",
        p.new_bytes() as f64 / 1048576.0,
        p.new_chunks(),
        p.total_chunks(),
        t.elapsed().as_secs_f64()
    );
    drop(p);

    let s = db.stats().unwrap();
    println!(
        "log: {:.1} MiB in {} segment(s)",
        s.log_bytes as f64 / 1048576.0,
        s.segments
    );
    drop(db);
    let _ = std::fs::remove_dir_all(&dir);
    0
}
