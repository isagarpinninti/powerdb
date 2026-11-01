//! A consistent copy of a running database.
//!
//! The metadata is read through one cross-keyspace snapshot, so it shows every commit that finished before the
//! snapshot and none that finished after. The blob log is append-only and a commit appends its chunks before it
//! publishes its record, so every chunk the snapshot refers to lies below the segment lengths read right after
//! the snapshot. Copying each segment up to that length therefore yields a log that holds everything the copied
//! metadata points at; whatever follows (later writes, a half-written entry) is simply not copied.
//! Compaction is held off meanwhile, because it deletes and rewrites segments.

use super::Db;
use super::segment::seg_path;
use crate::error::Result;
use fjall::{Database, KeyspaceCreateOptions, PersistMode, Readable};
use std::path::Path;

#[derive(Debug, Clone, Copy)]
pub struct SnapshotCopy {
    pub keys: u64,
    pub segments: usize,
    pub blob_bytes: u64,
}

const BATCH: usize = 5_000;

impl Db {
    /// Writes `dest/meta` and `dest/blobs`, a point-in-time image that can be opened as a database. Writers are not blocked.
    pub fn snapshot_to(&self, dest: &Path) -> Result<SnapshotCopy> {
        let _m = self.inner.maint.lock().unwrap_or_else(|e| e.into_inner());
        self.inner.log.sync()?;
        let snap = self.inner.store.snapshot();
        let lengths = self.inner.log.lengths()?; // read after the snapshot, never before: see the module comment

        let out = Database::builder(dest.join("meta")).open()?;
        let mut keys = 0u64;
        for name in self.inner.store.list_keyspace_names() {
            let src = self
                .inner
                .store
                .keyspace(name.as_ref(), KeyspaceCreateOptions::default)?;
            let dst = out.keyspace(name.as_ref(), KeyspaceCreateOptions::default)?;
            let mut batch = out.batch();
            let mut n = 0;
            for g in snap.iter(&src) {
                let (k, v) = g.into_inner()?;
                batch.insert(&dst, k.to_vec(), v.to_vec());
                n += 1;
                keys += 1;
                if n == BATCH {
                    batch.commit()?;
                    batch = out.batch();
                    n = 0;
                }
            }
            batch.commit()?;
        }
        out.persist(PersistMode::SyncAll)?;
        drop(out);

        let mut blob_bytes = 0;
        for (id, len) in &lengths {
            self.inner
                .log
                .copy_prefix(*id, *len, &seg_path(&dest.join("blobs"), *id))?;
            blob_bytes += len;
        }
        Ok(SnapshotCopy {
            keys,
            segments: lengths.len(),
            blob_bytes,
        })
    }
}
