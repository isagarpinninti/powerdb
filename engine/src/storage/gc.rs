//! Background space reclamation. Deleting a record is cheap on purpose (see `Db::commit_change`): it only drops the record and one file counter. The work of
//! giving the space back happens here, off the request path, in two steps:
//!
//! 1. `gc_step` releases files whose counter reached zero: it decrements their chunks' counters and marks the bytes of chunks that hit zero as dead.
//! 2. `compact` rewrites a sealed segment whose dead share is high: live chunks are copied forward, the index is repointed in one atomic batch, and the old file is deleted.

use super::{Db, segment::ChunkPtr, u64_of};
use crate::error::{Error, Result};
use std::collections::HashMap;

#[derive(Debug, Default, Clone)]
pub struct GcStats {
    pub files_released: u64,
    pub chunks_dead: u64,
    pub bytes_dead: u64,
}

#[derive(Debug, Default, Clone)]
pub struct CompactStats {
    pub segments_compacted: u32,
    pub bytes_reclaimed: u64,
    pub bytes_copied: u64,
}

impl Db {
    /// Releases up to `max_files` queued files. Returns what it freed; call again until `files_released` is 0 to drain the queue.
    pub fn gc_step(&self, max_files: usize) -> Result<GcStats> {
        let ks = &self.inner.ks;
        let _g = self.inner.commit.lock().unwrap_or_else(|e| e.into_inner());
        let mut batch = self.inner.store.batch();
        let mut stats = GcStats::default();
        let mut chunk_refs: HashMap<[u8; 32], u64> = HashMap::new();
        let mut dead: HashMap<u32, u64> = HashMap::new();
        for (taken, g) in ks.gc_queue.iter().enumerate() {
            if taken >= max_files {
                break;
            }
            let (qk, fid) = g.into_inner()?;
            batch.remove(&ks.gc_queue, qk.to_vec());
            let fid: [u8; 32] = fid
                .as_ref()
                .try_into()
                .map_err(|_| Error::Corrupt("gc queue entry".into()))?;
            if u64_of(ks.file_refs.get(fid)?) > 0 {
                continue; // a new record took the same file before we got here
            }
            let Some(mv) = ks.files.get(fid)? else {
                batch.remove(&ks.file_refs, fid.to_vec());
                continue;
            };
            let m = super::model::Manifest::from_bytes(&mv)
                .ok_or_else(|| Error::Corrupt("file manifest".into()))?;
            for k in m.unique_keys() {
                let cur = match chunk_refs.get(&k) {
                    Some(c) => *c,
                    None => u64_of(ks.chunk_refs.get(k)?),
                };
                let next = cur.saturating_sub(1);
                chunk_refs.insert(k, next);
                batch.insert(&ks.chunk_refs, k.to_vec(), next.to_le_bytes().to_vec());
                if next == 0
                    && let Some(v) = ks.chunks.get(k)?
                {
                    let p = ChunkPtr::from_bytes(&v)
                        .ok_or_else(|| Error::Corrupt("chunk pointer".into()))?;
                    *dead.entry(p.seg).or_default() += p.entry_len();
                    stats.chunks_dead += 1;
                    stats.bytes_dead += p.entry_len();
                }
            }
            batch.remove(&ks.files, fid.to_vec());
            batch.remove(&ks.file_refs, fid.to_vec());
            stats.files_released += 1;
        }
        for (seg, add) in dead {
            let cur = u64_of(ks.seg_dead.get(seg.to_be_bytes())?);
            batch.insert(
                &ks.seg_dead,
                seg.to_be_bytes().to_vec(),
                (cur + add).to_le_bytes().to_vec(),
            );
        }
        batch.commit()?;
        Ok(stats)
    }

    fn is_pinned(&self, key: &[u8; 32]) -> bool {
        self.inner
            .pins
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(key)
    }

    /// Compacts every sealed segment whose dead bytes are at least `min_dead_ratio` of its size (0.0 compacts all sealed segments).
    pub fn compact(&self, min_dead_ratio: f64) -> Result<CompactStats> {
        let mut total = CompactStats::default();
        let active = self.inner.log.active_id();
        for seg in self.inner.log.segment_ids()? {
            if seg == active {
                continue;
            }
            let len = self.inner.log.segment_len(seg)?;
            let dead = u64_of(self.inner.ks.seg_dead.get(seg.to_be_bytes())?);
            if len == 0 || (dead as f64 / len as f64) < min_dead_ratio {
                continue;
            }
            let s = self.compact_segment(seg)?;
            total.segments_compacted += 1;
            total.bytes_reclaimed += s.bytes_reclaimed;
            total.bytes_copied += s.bytes_copied;
        }
        Ok(total)
    }

    fn compact_segment(&self, seg: u32) -> Result<CompactStats> {
        let ks = &self.inner.ks;
        let log = &self.inner.log;
        let seg_len = log.segment_len(seg)?;

        // Phase 1, no lock: find the entries the index still points at, and copy the ones that look alive to the active segment.
        let mut indexed: Vec<([u8; 32], ChunkPtr)> = Vec::new();
        log.scan(seg, |e| {
            if let Some(v) = ks.chunks.get(e.key)?
                && ChunkPtr::from_bytes(&v) == Some(e.ptr)
            {
                indexed.push((e.key, e.ptr));
            }
            Ok(())
        })?;
        let mut moved: HashMap<[u8; 32], ChunkPtr> = HashMap::new();
        let mut copied = 0u64;
        for (key, ptr) in &indexed {
            if u64_of(ks.chunk_refs.get(key)?) > 0 || self.is_pinned(key) {
                let payload = log.read_payload(ptr)?;
                let np = log.append(key, ptr.raw_len, ptr.flags, &payload)?;
                copied += np.entry_len();
                moved.insert(*key, np);
            }
        }
        if self.inner.opts.durability == super::Durability::Commit {
            log.sync()?; // the copies must be durable before the index points at them
        }

        // Phase 2, under the commit lock: repoint or delete, atomically.
        {
            let _g = self.inner.commit.lock().unwrap_or_else(|e| e.into_inner());
            let mut batch = self.inner.store.batch();
            for (key, ptr) in &indexed {
                let still_here =
                    ks.chunks.get(key)?.and_then(|v| ChunkPtr::from_bytes(&v)) == Some(*ptr);
                if !still_here {
                    continue;
                }
                if let Some(np) = moved.get(key) {
                    batch.insert(&ks.chunks, key.to_vec(), np.to_bytes().to_vec());
                } else if u64_of(ks.chunk_refs.get(key)?) == 0 && !self.is_pinned(key) {
                    batch.remove(&ks.chunks, key.to_vec());
                    batch.remove(&ks.chunk_refs, key.to_vec());
                } else {
                    // it came back to life after phase 1: copy it now
                    let payload = log.read_payload(ptr)?;
                    let np = log.append(key, ptr.raw_len, ptr.flags, &payload)?;
                    copied += np.entry_len();
                    if self.inner.opts.durability == super::Durability::Commit {
                        log.sync()?;
                    }
                    batch.insert(&ks.chunks, key.to_vec(), np.to_bytes().to_vec());
                }
            }
            batch.remove(&ks.seg_dead, seg.to_be_bytes().to_vec());
            batch.commit()?;
            if self.inner.opts.durability == super::Durability::Commit {
                self.inner.store.persist(fjall::PersistMode::SyncAll)?;
            }
        }
        log.remove_segment(seg)?;
        Ok(CompactStats {
            segments_compacted: 1,
            bytes_reclaimed: seg_len.saturating_sub(copied),
            bytes_copied: copied,
        })
    }

    /// Drains the release queue and compacts segments that are at least `min_dead_ratio` dead. What a maintenance thread would run when the server is idle.
    pub fn collect(&self, min_dead_ratio: f64) -> Result<(GcStats, CompactStats)> {
        let mut g = GcStats::default();
        loop {
            let s = self.gc_step(256)?;
            if s.files_released == 0 {
                break;
            }
            g.files_released += s.files_released;
            g.chunks_dead += s.chunks_dead;
            g.bytes_dead += s.bytes_dead;
        }
        let c = self.compact(min_dead_ratio)?;
        Ok((g, c))
    }
}
