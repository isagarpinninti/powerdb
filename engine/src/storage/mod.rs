//! The hybrid storage engine: records and indexes in an LSM tree, file bytes in a content-addressed chunk log, tied together by one atomic commit.
//! See docs/DESIGN.md for the reasoning and the crash story.

pub mod chunker;
pub mod format;
pub mod gc;
pub mod model;
pub mod segment;
pub mod snapshot;
pub mod verify;

use crate::error::{Error, Result};
use crate::util::{from_hex_32, hex, new_id, now_ms};
use chunker::{ChunkSizes, Chunker};
use fjall::{Database, Keyspace, KeyspaceCreateOptions, PersistMode};
use model::{AttachedFile, CollectionDef, FileRef, Manifest, Record};
use segment::{ChunkPtr, SegmentLog};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

/// Turns a chunk into the bytes stored in the log and back. The identity codec stores chunks in the clear; `security::crypto` provides an authenticated cipher.
pub trait ChunkCodec: Send + Sync {
    /// Stored in each log entry so a chunk can always be decoded by the codec that wrote it. 0 is reserved for the identity codec.
    fn flags(&self) -> u8;
    fn encode(&self, key: &[u8; 32], raw: &[u8]) -> Result<Vec<u8>>;
    fn decode(&self, key: &[u8; 32], stored: &[u8]) -> Result<Vec<u8>>;
}

pub struct PlainCodec;
impl ChunkCodec for PlainCodec {
    fn flags(&self) -> u8 {
        0
    }
    fn encode(&self, _key: &[u8; 32], raw: &[u8]) -> Result<Vec<u8>> {
        Ok(raw.to_vec())
    }
    fn decode(&self, _key: &[u8; 32], stored: &[u8]) -> Result<Vec<u8>> {
        Ok(stored.to_vec())
    }
}

/// When a write is on disk, not just in the operating system's buffers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Durability {
    /// Every commit is fsynced before it is acknowledged: the log first, then the metadata journal. Safe against power loss. The journal admits one fsync at a time, so
    /// this costs one disk flush per commit (about 3 ms on the SSD this was measured on, so roughly 330 commits per second) however many writers there are.
    Commit,
    /// A commit is acknowledged once the operating system has it, and a background thread fsyncs every this many milliseconds. A crash of the process loses nothing; a power
    /// cut or kernel panic can lose up to this long of the most recent commits (never older ones, and never leaves the database inconsistent).
    Interval(u64),
    /// No fsync until shutdown. For benchmarks and throwaway data.
    Never,
}

#[derive(Clone, Debug)]
pub struct Options {
    pub durability: Durability,
    pub segment_bytes: u64,
    pub chunk: ChunkSizes,
    /// Re-hash every unencrypted chunk read in full and compare it with its key.
    pub verify_reads: bool,
    /// Block cache for the LSM tree.
    pub cache_bytes: Option<u64>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            durability: Durability::Commit,
            segment_bytes: 256 << 20,
            chunk: ChunkSizes::default(),
            verify_reads: true,
            cache_bytes: None,
        }
    }
}

#[derive(Clone)]
pub(crate) struct Spaces {
    pub collections: Keyspace,
    pub records: Keyspace,
    pub files: Keyspace,
    pub file_refs: Keyspace,
    pub chunks: Keyspace,
    pub chunk_refs: Keyspace,
    pub gc_queue: Keyspace,
    pub seg_dead: Keyspace,
}

pub(crate) struct Inner {
    pub dir: std::path::PathBuf,
    /// Held while a backup runs and while compaction rewrites or deletes segments: the two must not overlap.
    pub maint: Mutex<()>,
    /// Holds the data directory's exclusive lock for as long as this database is open.
    pub _lock: std::fs::File,
    pub store: Database,
    pub ks: Spaces,
    pub log: SegmentLog,
    pub opts: Options,
    pub commit: Mutex<()>,
    pub codecs: RwLock<HashMap<u8, Arc<dyn ChunkCodec>>>,
    pub pins: Mutex<HashMap<[u8; 32], u32>>,
    pub gc_seq: AtomicU64,
}

/// Stops the background fsync thread, and waits for it, when the last `Db` handle goes away.
/// Without the wait, the thread could still hold the database for a moment after `drop` returned, and the data
/// directory's lock with it, so reopening right away would be refused.
struct Closer {
    stop: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Closer {
    fn drop(&mut self) {
        drop(self.stop.take()); // wakes the thread at once: its wait ends when the sender is gone
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[derive(Clone)]
pub struct Db {
    // Field order matters: `closer` drops first, joining the thread, so `inner` is then released by this thread, not by the syncer.
    _closer: Arc<Closer>,
    pub(crate) inner: Arc<Inner>,
}

pub fn chunk_key(flags: u8, hash: &[u8; 32]) -> [u8; 32] {
    if flags == 0 {
        return *hash;
    }
    let mut h = blake3::Hasher::new_derive_key("powerdb encrypted chunk key v1");
    h.update(&[flags]);
    h.update(hash);
    *h.finalize().as_bytes()
}

pub(crate) fn rec_key(collection: &str, id: &str) -> Vec<u8> {
    let mut k = Vec::with_capacity(collection.len() + 1 + id.len());
    k.extend_from_slice(collection.as_bytes());
    k.push(0);
    k.extend_from_slice(id.as_bytes());
    k
}

pub(crate) fn u64_of(v: Option<fjall::Slice>) -> u64 {
    v.and_then(|s| s.as_ref().try_into().ok())
        .map(u64::from_le_bytes)
        .unwrap_or(0)
}

/// File bytes that have been chunked, hashed and written to the log but are not yet part of any record. Dropping it without committing leaves only
/// unreferenced log entries, which compaction removes.
pub struct PendingFile {
    pub manifest: Manifest,
    pub file_id: [u8; 32],
    pub mime: String,
    pub name: Option<String>,
    pub(crate) new_ptrs: HashMap<[u8; 32], ChunkPtr>,
    guard: PinGuard,
}

impl PendingFile {
    pub fn size(&self) -> u64 {
        self.manifest.size
    }
    pub fn file_ref(&self) -> FileRef {
        FileRef {
            id: hex(&self.file_id),
            size: self.manifest.size,
            mime: self.mime.clone(),
            name: self.name.clone(),
        }
    }
    /// Bytes this file added to the log (chunks not already stored).
    pub fn new_bytes(&self) -> u64 {
        self.new_ptrs.values().map(|p| p.entry_len()).sum()
    }
    pub fn new_chunks(&self) -> usize {
        self.new_ptrs.len()
    }
    pub fn total_chunks(&self) -> usize {
        self.manifest.chunks.len()
    }
    #[allow(dead_code)]
    fn pinned(&self) -> usize {
        self.guard.keys.len()
    }
}

/// Keeps chunks a staged file relies on from being compacted away before it commits.
struct PinGuard {
    inner: Arc<Inner>,
    keys: Vec<[u8; 32]>,
    /// The segment of each chunk this upload appended, released together with the pins.
    staged_segments: Vec<u32>,
}
impl PinGuard {
    fn pin(&mut self, key: [u8; 32]) {
        let mut p = self.inner.pins.lock().unwrap_or_else(|e| e.into_inner());
        *p.entry(key).or_insert(0) += 1;
        self.keys.push(key);
    }
}
impl Drop for PinGuard {
    fn drop(&mut self) {
        let mut p = self.inner.pins.lock().unwrap_or_else(|e| e.into_inner());
        for k in &self.keys {
            if let Some(n) = p.get_mut(k) {
                *n -= 1;
                if *n == 0 {
                    p.remove(k);
                }
            }
        }
        drop(p);
        // After the pins: by now a committed upload's chunks are in the index, so compaction may look at the segment again.
        for seg in &self.staged_segments {
            self.inner.log.release_staged(*seg);
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub segments: usize,
    pub log_bytes: u64,
    pub dead_bytes: u64,
    pub gc_pending: u64,
}

/// Checks the metadata store's version files against their recorded checksums, before the storage library reads them.
///
/// The library parses a keyspace's current version file and sizes allocations from counts inside it without checking the
/// checksum its `current` file records (an upstream TODO), so one damaged byte can make it request tens of gigabytes and
/// abort the process. This turns that into an ordinary error that says what to do. Only the file `current` names is
/// checked: older `v*` files are leftovers nothing reads. A pristine directory always passes, which every test that
/// opens a database exercises, so a library upgrade that changes the format fails loudly instead of locking anyone out.
fn check_meta_files(meta: &Path) -> Result<()> {
    let keyspaces = meta.join("keyspaces");
    let entries = match std::fs::read_dir(&keyspaces) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    for entry in entries {
        let dir = entry?.path();
        let current = match std::fs::read(dir.join("current")) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        let damaged = |what: String| {
            Error::Corrupt(format!(
                "{}: {what}; the metadata store is damaged. Restore from a backup (powerdb restore)",
                dir.display()
            ))
        };
        if current.len() != 25 || current[24] != 0 {
            return Err(damaged("the version pointer is malformed".into()));
        }
        let id = u64::from_le_bytes(current[0..8].try_into().unwrap_or_default());
        let want = u128::from_le_bytes(current[8..24].try_into().unwrap_or_default());
        let file = std::fs::read(dir.join(format!("v{id}")))
            .map_err(|e| damaged(format!("version file v{id} cannot be read ({e})")))?;
        if xxhash_rust::xxh3::xxh3_128(&file) != want {
            return Err(damaged(format!(
                "version file v{id} does not match its checksum"
            )));
        }
    }
    Ok(())
}

/// Version of the on-disk layout. Bump it, and add an entry to `format::MIGRATIONS`, whenever a change would make older data unreadable.
pub const FORMAT_VERSION: u32 = 1;

impl Db {
    pub fn open(path: impl AsRef<Path>, opts: Options) -> Result<Db> {
        let path = path.as_ref();
        std::fs::create_dir_all(path)?;
        // Two processes on one data directory would corrupt it, so the first one holds an exclusive lock and any other is refused.
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path.join("LOCK"))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(Error::Conflict(format!(
                    "{} is in use by another PowerDB process (or backup); stop it first",
                    path.display()
                )));
            }
            Err(std::fs::TryLockError::Error(e)) if e.kind() == std::io::ErrorKind::Unsupported => {
                tracing::warn!(
                    "this file system does not support locking; make sure only one PowerDB process uses {}",
                    path.display()
                )
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(e.into()),
        }
        format::ensure(path, FORMAT_VERSION, format::MIGRATIONS)?;
        check_meta_files(&path.join("meta"))?;
        let mut b = Database::builder(path.join("meta"));
        if let Some(c) = opts.cache_bytes {
            b = b.cache_size(c);
        }
        let store = b.open()?;
        let o = KeyspaceCreateOptions::default;
        let ks = Spaces {
            collections: store.keyspace("collections", o)?,
            records: store.keyspace("records", o)?,
            files: store.keyspace("files", o)?,
            file_refs: store.keyspace("file_refs", o)?,
            chunks: store.keyspace("chunks", o)?,
            chunk_refs: store.keyspace("chunk_refs", o)?,
            gc_queue: store.keyspace("gc_queue", o)?,
            seg_dead: store.keyspace("seg_dead", o)?,
        };
        let log = SegmentLog::open(&path.join("blobs"), opts.segment_bytes)?;
        let next_seq = match ks.gc_queue.last_key_value() {
            Some(g) => {
                let (k, _) = g.into_inner()?;
                u64::from_be_bytes(
                    k.as_ref()
                        .try_into()
                        .map_err(|_| Error::Corrupt("gc queue key".into()))?,
                ) + 1
            }
            None => 0,
        };
        let mut codecs: HashMap<u8, Arc<dyn ChunkCodec>> = HashMap::new();
        codecs.insert(0, Arc::new(PlainCodec));
        let interval = if let Durability::Interval(ms) = opts.durability {
            Some(ms.max(1))
        } else {
            None
        };
        let inner = Arc::new(Inner {
            dir: path.to_path_buf(),
            maint: Mutex::new(()),
            _lock: lock,
            store,
            ks,
            log,
            opts,
            commit: Mutex::new(()),
            codecs: RwLock::new(codecs),
            pins: Mutex::new(HashMap::new()),
            gc_seq: AtomicU64::new(next_seq),
        });
        let mut closer = Closer {
            stop: None,
            thread: None,
        };
        if let Some(ms) = interval {
            // Holds only a weak reference, and is stopped and joined by `Closer`.
            let weak = Arc::downgrade(&inner);
            let (tx, rx) = std::sync::mpsc::channel::<()>();
            closer.stop = Some(tx);
            closer.thread = Some(
                std::thread::Builder::new()
                    .name("powerdb-fsync".into())
                    .spawn(move || {
                        while let Err(std::sync::mpsc::RecvTimeoutError::Timeout) =
                            rx.recv_timeout(std::time::Duration::from_millis(ms))
                        {
                            let Some(i) = weak.upgrade() else { break };
                            let _ = i.log.sync();
                            let _ = i.store.persist(PersistMode::SyncAll);
                        }
                    })?,
            );
        }
        let db = Db {
            _closer: Arc::new(closer),
            inner,
        };
        Ok(db)
    }

    /// Another keyspace in the same database, for modules (accounts, keys) that keep their own records beside the collections.
    /// The data directory this database was opened on.
    pub fn data_dir(&self) -> &Path {
        &self.inner.dir
    }

    pub fn keyspace(&self, name: &str) -> Result<Keyspace> {
        Ok(self
            .inner
            .store
            .keyspace(name, KeyspaceCreateOptions::default)?)
    }

    /// Registers the codec used for encrypted collections (and for decoding any chunk it wrote).
    pub fn register_codec(&self, codec: Arc<dyn ChunkCodec>) {
        self.inner
            .codecs
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(codec.flags(), codec);
    }

    fn codec_by_flags(&self, flags: u8) -> Result<Arc<dyn ChunkCodec>> {
        self.inner.codecs.read().unwrap_or_else(|e| e.into_inner()).get(&flags).cloned().ok_or_else(|| Error::Invalid(format!("no codec is registered for chunk flag {flags}; is the encryption key configured?")))
    }

    // ---- collections ----

    pub fn create_collection(&self, mut def: CollectionDef) -> Result<CollectionDef> {
        def.check_name()?;
        let _g = self.inner.commit.lock().unwrap_or_else(|e| e.into_inner());
        if self
            .inner
            .ks
            .collections
            .contains_key(def.name.as_bytes())?
        {
            return Err(Error::Conflict(format!(
                "collection \"{}\" already exists",
                def.name
            )));
        }
        def.created_at = now_ms();
        self.inner
            .ks
            .collections
            .insert(def.name.as_bytes().to_vec(), serde_json::to_vec(&def)?)?;
        self.persist()?;
        Ok(def)
    }

    pub fn update_collection(&self, mut def: CollectionDef) -> Result<CollectionDef> {
        def.check_name()?;
        let _g = self.inner.commit.lock().unwrap_or_else(|e| e.into_inner());
        let old = self.collection(&def.name)?;
        if old.encrypt != def.encrypt {
            return Err(Error::Invalid("a collection's encryption cannot change after it is created (existing files keep the codec that wrote them)".into()));
        }
        def.created_at = old.created_at;
        self.inner
            .ks
            .collections
            .insert(def.name.as_bytes().to_vec(), serde_json::to_vec(&def)?)?;
        self.persist()?;
        Ok(def)
    }

    pub fn collection(&self, name: &str) -> Result<CollectionDef> {
        match self.inner.ks.collections.get(name.as_bytes())? {
            Some(v) => Ok(serde_json::from_slice(&v)?),
            None => Err(Error::NotFound(format!("collection \"{name}\""))),
        }
    }

    pub fn collections(&self) -> Result<Vec<CollectionDef>> {
        let mut out = Vec::new();
        for g in self.inner.ks.collections.iter() {
            let (_, v) = g.into_inner()?;
            out.push(serde_json::from_slice(&v)?);
        }
        Ok(out)
    }

    pub fn drop_collection(&self, name: &str) -> Result<u64> {
        self.collection(name)?;
        let mut ids = Vec::new();
        self.scan(name, false, |r| {
            ids.push(r.id);
            Ok(true)
        })?;
        for id in &ids {
            self.delete(name, id)?;
        }
        let _g = self.inner.commit.lock().unwrap_or_else(|e| e.into_inner());
        self.inner.ks.collections.remove(name.as_bytes().to_vec())?;
        self.persist()?;
        Ok(ids.len() as u64)
    }

    // ---- staging files ----

    /// Reads `reader` to the end, writing new chunks to the log. Safe to run on many threads at once.
    pub fn stage_file(
        &self,
        collection: &str,
        mime: &str,
        name: Option<&str>,
        reader: impl Read,
    ) -> Result<PendingFile> {
        let def = self.collection(collection)?;
        let codec = if def.encrypt {
            self.codec_by_flags(1)?
        } else {
            self.codec_by_flags(0)?
        };
        let flags = codec.flags();
        let mut guard = PinGuard {
            inner: self.inner.clone(),
            keys: Vec::new(),
            staged_segments: Vec::new(),
        };
        let mut chunks: Vec<([u8; 32], u32)> = Vec::new();
        let mut new_ptrs: HashMap<[u8; 32], ChunkPtr> = HashMap::new();
        let mut size = 0u64;
        for ch in Chunker::new(reader, self.inner.opts.chunk) {
            let data = ch?;
            let hash = *blake3::hash(&data).as_bytes();
            let key = chunk_key(flags, &hash);
            size += data.len() as u64;
            chunks.push((key, data.len() as u32));
            guard.pin(key); // before looking: compaction must not drop it between the lookup and the commit
            if new_ptrs.contains_key(&key) || self.inner.ks.chunks.contains_key(key)? {
                continue;
            }
            let stored = codec.encode(&key, &data)?;
            let ptr = self
                .inner
                .log
                .append_staged(&key, data.len() as u32, flags, &stored)?;
            guard.staged_segments.push(ptr.seg); // released when the guard drops, even if this function fails below
            new_ptrs.insert(key, ptr);
        }
        crate::failpoint!("stage.after_append");
        if self.inner.opts.durability == Durability::Commit && !new_ptrs.is_empty() {
            self.inner.log.sync()?;
        }
        crate::failpoint!("stage.after_sync");
        let manifest = Manifest { size, chunks };
        let file_id = manifest.file_id();
        Ok(PendingFile {
            manifest,
            file_id,
            mime: mime.to_string(),
            name: name.map(str::to_string),
            new_ptrs,
            guard,
        })
    }

    // ---- records ----

    pub fn create(
        &self,
        collection: &str,
        fields: Map<String, Value>,
        files: Vec<(String, PendingFile)>,
    ) -> Result<Record> {
        let def = self.collection(collection)?;
        let attached: BTreeMap<String, AttachedFile<'_>> = files
            .iter()
            .map(|(k, p)| {
                (
                    k.clone(),
                    AttachedFile {
                        size: p.size(),
                        mime: &p.mime,
                    },
                )
            })
            .collect();
        def.validate(&fields, &attached, false)?;
        let now = now_ms();
        let rec = Record {
            id: new_id(),
            collection: collection.to_string(),
            created_at: now,
            updated_at: now,
            fields,
            files: files
                .iter()
                .map(|(k, p)| (k.clone(), p.file_ref()))
                .collect(),
        };
        let pend: Vec<&PendingFile> = files.iter().map(|(_, p)| p).collect();
        self.commit_change(None, Some(&rec), &pend)?;
        Ok(rec)
    }

    pub fn get(&self, collection: &str, id: &str) -> Result<Option<Record>> {
        match self.inner.ks.records.get(rec_key(collection, id))? {
            Some(v) => Ok(Some(serde_json::from_slice(&v)?)),
            None => Ok(None),
        }
    }

    /// Merges `patch` into the record's fields (a JSON null removes a field), attaches `files` (replacing any file in the same field) and detaches `remove_files`.
    pub fn update(
        &self,
        collection: &str,
        id: &str,
        patch: Map<String, Value>,
        files: Vec<(String, PendingFile)>,
        remove_files: &[String],
    ) -> Result<Record> {
        let def = self.collection(collection)?;
        let old = self
            .get(collection, id)?
            .ok_or_else(|| Error::NotFound(format!("record {id} in \"{collection}\"")))?;
        let mut fields = old.fields.clone();
        for (k, v) in &patch {
            if v.is_null() {
                fields.remove(k);
            } else {
                fields.insert(k.clone(), v.clone());
            }
        }
        let mut frefs = old.files.clone();
        for f in remove_files {
            frefs.remove(f);
        }
        for (k, p) in &files {
            frefs.insert(k.clone(), p.file_ref());
        }
        // Validate the whole resulting record: the fields as merged and the files as they will be.
        let attached: BTreeMap<String, AttachedFile<'_>> = frefs
            .iter()
            .map(|(k, f)| {
                (
                    k.clone(),
                    AttachedFile {
                        size: f.size,
                        mime: &f.mime,
                    },
                )
            })
            .collect();
        def.validate(&fields, &attached, false)?;
        let rec = Record {
            id: old.id.clone(),
            collection: old.collection.clone(),
            created_at: old.created_at,
            updated_at: now_ms().max(old.updated_at),
            fields,
            files: frefs,
        };
        let pend: Vec<&PendingFile> = files.iter().map(|(_, p)| p).collect();
        self.commit_change(Some(&old), Some(&rec), &pend)?;
        Ok(rec)
    }

    pub fn delete(&self, collection: &str, id: &str) -> Result<bool> {
        match self.get(collection, id)? {
            None => Ok(false),
            Some(old) => {
                self.commit_change(Some(&old), None, &[])?;
                Ok(true)
            }
        }
    }

    /// Visits records in id order (oldest first) or reverse (newest first) until `f` returns false.
    pub fn scan(
        &self,
        collection: &str,
        newest_first: bool,
        mut f: impl FnMut(Record) -> Result<bool>,
    ) -> Result<()> {
        let mut prefix = collection.as_bytes().to_vec();
        prefix.push(0);
        let it = self.inner.ks.records.prefix(&prefix);
        let mut visit = |g: fjall::Guard| -> Result<bool> {
            let (_, v) = g.into_inner()?;
            f(serde_json::from_slice(&v)?)
        };
        if newest_first {
            for g in it.rev() {
                if !visit(g)? {
                    break;
                }
            }
        } else {
            for g in it {
                if !visit(g)? {
                    break;
                }
            }
        }
        Ok(())
    }

    pub fn count(&self, collection: &str) -> Result<u64> {
        let mut n = 0;
        let mut prefix = collection.as_bytes().to_vec();
        prefix.push(0);
        for g in self.inner.ks.records.prefix(&prefix) {
            g.key()?;
            n += 1;
        }
        Ok(n)
    }

    // ---- the atomic commit ----

    fn persist(&self) -> Result<()> {
        if self.inner.opts.durability == Durability::Commit {
            self.inner.store.persist(PersistMode::SyncAll)?;
        }
        Ok(())
    }

    /// Applies a record change (create, update or delete) together with every reference-count and index change it implies, as one atomic batch.
    fn commit_change(
        &self,
        old: Option<&Record>,
        new: Option<&Record>,
        pend: &[&PendingFile],
    ) -> Result<()> {
        let ks = &self.inner.ks;
        let _g = self.inner.commit.lock().unwrap_or_else(|e| e.into_inner());
        let mut batch = self.inner.store.batch();

        let mut file_delta: HashMap<[u8; 32], i64> = HashMap::new();
        let id_of = |f: &FileRef| {
            from_hex_32(&f.id).ok_or_else(|| Error::Corrupt(format!("bad file id {}", f.id)))
        };
        if let Some(o) = old {
            for f in o.files.values() {
                *file_delta.entry(id_of(f)?).or_default() -= 1;
            }
        }
        if let Some(n) = new {
            for f in n.files.values() {
                *file_delta.entry(id_of(f)?).or_default() += 1;
            }
        }

        let mut chunk_delta: HashMap<[u8; 32], i64> = HashMap::new();
        let mut new_ptrs: HashMap<[u8; 32], ChunkPtr> = HashMap::new();
        for p in pend {
            for (k, v) in &p.new_ptrs {
                new_ptrs.entry(*k).or_insert(*v);
            }
        }
        for (fid, d) in &file_delta {
            if *d == 0 {
                continue;
            }
            let cur = u64_of(ks.file_refs.get(fid)?) as i64;
            let next = cur + d;
            if next < 0 {
                return Err(Error::Corrupt(
                    "a file's reference count would go negative".into(),
                ));
            }
            batch.insert(
                &ks.file_refs,
                fid.to_vec(),
                (next as u64).to_le_bytes().to_vec(),
            );
            if cur == 0 && next > 0 && !ks.files.contains_key(fid)? {
                let p = pend.iter().find(|p| &p.file_id == fid).ok_or_else(|| {
                    Error::Corrupt("a record refers to a file that was not staged".into())
                })?;
                batch.insert(&ks.files, fid.to_vec(), p.manifest.to_bytes());
                for k in p.manifest.unique_keys() {
                    *chunk_delta.entry(k).or_default() += 1;
                }
            }
            if cur > 0 && next == 0 {
                let seq = self.inner.gc_seq.fetch_add(1, Ordering::SeqCst);
                batch.insert(&ks.gc_queue, seq.to_be_bytes().to_vec(), fid.to_vec());
            }
        }

        let mut dead_delta: HashMap<u32, i64> = HashMap::new();
        for (k, d) in &chunk_delta {
            let cur = u64_of(ks.chunk_refs.get(k)?) as i64;
            let next = cur + d;
            match ks.chunks.get(k)? {
                Some(v) => {
                    if cur == 0 && next > 0 {
                        // a chunk that was counted dead is alive again
                        let p = ChunkPtr::from_bytes(&v)
                            .ok_or_else(|| Error::Corrupt("chunk pointer".into()))?;
                        *dead_delta.entry(p.seg).or_default() -= p.entry_len() as i64;
                    }
                }
                None => {
                    let p = new_ptrs.get(k).ok_or_else(|| {
                        Error::Corrupt("a chunk is neither indexed nor staged".into())
                    })?;
                    batch.insert(&ks.chunks, k.to_vec(), p.to_bytes().to_vec());
                }
            }
            batch.insert(
                &ks.chunk_refs,
                k.to_vec(),
                (next as u64).to_le_bytes().to_vec(),
            );
        }
        for (seg, d) in dead_delta {
            let cur = u64_of(ks.seg_dead.get(seg.to_be_bytes())?);
            batch.insert(
                &ks.seg_dead,
                seg.to_be_bytes().to_vec(),
                (cur as i64 + d).max(0).to_le_bytes().to_vec(),
            );
        }

        match (old, new) {
            (_, Some(n)) => batch.insert(
                &ks.records,
                rec_key(&n.collection, &n.id),
                serde_json::to_vec(n)?,
            ),
            (Some(o), None) => batch.remove(&ks.records, rec_key(&o.collection, &o.id)),
            (None, None) => {}
        }
        crate::failpoint!("commit.before_batch");
        batch.commit()?;
        crate::failpoint!("commit.after_batch");
        self.persist()?;
        crate::failpoint!("commit.after_persist");
        Ok(())
    }

    // ---- reading files ----

    pub fn manifest(&self, file_id: &[u8; 32]) -> Result<Manifest> {
        let v = self
            .inner
            .ks
            .files
            .get(file_id)?
            .ok_or_else(|| Error::NotFound("file".into()))?;
        Manifest::from_bytes(&v).ok_or_else(|| Error::Corrupt("file manifest".into()))
    }

    fn read_chunk_range(&self, key: &[u8; 32], from: u32, to: u32) -> Result<Vec<u8>> {
        for attempt in 0..3 {
            let v = self.inner.ks.chunks.get(key)?.ok_or_else(|| {
                Error::Corrupt("a file refers to a chunk that is not stored".into())
            })?;
            let ptr =
                ChunkPtr::from_bytes(&v).ok_or_else(|| Error::Corrupt("chunk pointer".into()))?;
            let r = (|| -> Result<Vec<u8>> {
                if ptr.flags == 0 && !(from == 0 && to == ptr.raw_len) {
                    return self.inner.log.read_slice(&ptr, from, to - from); // an unencrypted chunk can be read in part
                }
                let stored = self.inner.log.read_payload(&ptr)?;
                let raw = self.codec_by_flags(ptr.flags)?.decode(key, &stored)?;
                if raw.len() != ptr.raw_len as usize {
                    return Err(Error::Corrupt("chunk length".into()));
                }
                if ptr.flags == 0
                    && self.inner.opts.verify_reads
                    && blake3::hash(&raw).as_bytes() != key
                {
                    return Err(Error::Corrupt(
                        "a chunk's contents no longer match its hash".into(),
                    ));
                }
                Ok(raw[from as usize..to as usize].to_vec())
            })();
            match r {
                // compaction may have moved and deleted the segment between the lookup and the read: look the pointer up again
                Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound && attempt < 2 => {
                    continue;
                }
                other => return other,
            }
        }
        Err(Error::Corrupt("a chunk could not be read".into()))
    }

    /// Streams bytes `start..end` of a file to `sink`, one piece per chunk touched.
    pub fn read_file_range(
        &self,
        m: &Manifest,
        start: u64,
        end: u64,
        sink: &mut dyn FnMut(&[u8]) -> Result<()>,
    ) -> Result<()> {
        let end = end.min(m.size);
        let mut off = 0u64;
        for (key, len) in &m.chunks {
            let (c0, c1) = (off, off + *len as u64);
            off = c1;
            if c1 <= start {
                continue;
            }
            if c0 >= end {
                break;
            }
            let from = start.max(c0) - c0;
            let to = end.min(c1) - c0;
            sink(&self.read_chunk_range(key, from as u32, to as u32)?)?;
        }
        Ok(())
    }

    pub fn read_file(&self, file_id: &[u8; 32]) -> Result<Vec<u8>> {
        let m = self.manifest(file_id)?;
        // not `m.size` up front: it is read back from disk, and growing as chunks arrive costs nothing real
        let mut out = Vec::with_capacity((m.size as usize).min(64 << 20));
        self.read_file_range(&m, 0, m.size, &mut |b| {
            out.extend_from_slice(b);
            Ok(())
        })?;
        Ok(out)
    }

    pub fn stats(&self) -> Result<Stats> {
        let mut dead = 0;
        for g in self.inner.ks.seg_dead.iter() {
            let (_, v) = g.into_inner()?;
            dead += u64_of(Some(v));
        }
        let mut pending = 0;
        for g in self.inner.ks.gc_queue.iter() {
            g.key()?;
            pending += 1;
        }
        Ok(Stats {
            segments: self.inner.log.segment_ids()?.len(),
            log_bytes: self.inner.log.total_bytes()?,
            dead_bytes: dead,
            gc_pending: pending,
        })
    }

    /// Flushes everything to disk regardless of the `sync` option.
    pub fn flush(&self) -> Result<()> {
        self.inner.log.sync()?;
        self.inner.store.persist(PersistMode::SyncAll)?;
        Ok(())
    }
}
