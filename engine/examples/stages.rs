use powerdb::storage::chunker::{ChunkSizes, Chunker};
use std::io::Cursor;
use std::time::Instant;

fn main() {
    let mut x = 88172645463325252u64;
    let data: Vec<u8> = (0..256usize << 20).map(|_| { x ^= x << 13; x ^= x >> 7; x ^= x << 17; (x >> 24) as u8 }).collect();
    let mb = data.len() as f64 / 1e6;
    let t = Instant::now();
    let h = blake3::hash(&data);
    println!("blake3 single call over 256 MiB: {:.0} MB/s ({})", mb / t.elapsed().as_secs_f64(), &h.to_hex()[..8]);
    let t = Instant::now();
    let mut n = 0usize; let mut total = 0usize;
    for c in Chunker::new(Cursor::new(&data), ChunkSizes::default()) { let c = c.unwrap(); n += 1; total += c.len(); }
    println!("fastcdc chunking only: {:.0} MB/s ({n} chunks, {total} bytes)", mb / t.elapsed().as_secs_f64());
    let t = Instant::now();
    let mut hasher_bytes = 0;
    for c in Chunker::new(Cursor::new(&data), ChunkSizes::default()) { let c = c.unwrap(); let _ = blake3::hash(&c); hasher_bytes += c.len(); }
    println!("chunk + blake3 per chunk: {:.0} MB/s ({hasher_bytes})", mb / t.elapsed().as_secs_f64());
}
