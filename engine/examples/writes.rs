use std::fs::OpenOptions;
use std::os::unix::fs::FileExt;
use std::time::Instant;

fn main() {
    let dir = std::env::temp_dir().join(format!("pdb-w-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let chunk = vec![0x5Au8; 1 << 20];
    for (label, total_mib) in [("256 MiB, 1 MiB pwrite, no fsync", 256usize), ("256 MiB again (file already exists, overwritten)", 256)] {
        let f = OpenOptions::new().read(true).write(true).create(true).truncate(label.contains("no fsync")).open(dir.join("a.bin")).unwrap();
        let t = Instant::now();
        for i in 0..total_mib { f.write_all_at(&chunk, (i as u64) << 20).unwrap(); }
        let w = t.elapsed().as_secs_f64();
        let t2 = Instant::now();
        f.sync_data().unwrap();
        println!("{label}: {:.0} MB/s to the page cache ({:.2}s), then fdatasync {:.2}s", total_mib as f64 * 1.048576 / w, w, t2.elapsed().as_secs_f64());
    }
    // appending with an incrementing offset and a fresh file each time, like the segment log
    let f = OpenOptions::new().read(true).write(true).create(true).truncate(true).open(dir.join("b.bin")).unwrap();
    let t = Instant::now();
    for i in 0..256u64 { f.write_all_at(&chunk, i << 20).unwrap(); }
    println!("fresh file append 256 MiB: {:.0} MB/s", 256.0 * 1.048576 / t.elapsed().as_secs_f64());
    std::fs::remove_dir_all(&dir).unwrap();
}
