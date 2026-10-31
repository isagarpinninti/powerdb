//! The blob log: append-only segment files holding chunk payloads.
//!
//! Entry layout:  magic "PDB1" (4) | chunk key (32) | raw length (4) | stored length (4) | flags (1) | payload (stored length)
//! Entries are written once and never modified. A segment is sealed when the next one starts; space comes back only by compaction (see `gc.rs`).
//! A crash can leave a torn entry at the end of the newest segment; `open` finds the last whole entry and truncates there.

use crate::error::{Error, Result};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

const MAGIC: [u8; 4] = *b"PDB1";
pub const HEADER: u64 = 4 + 32 + 4 + 4 + 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChunkPtr {
    pub seg: u32,
    pub off: u64,
    pub stored_len: u32,
    pub raw_len: u32,
    pub flags: u8,
}

impl ChunkPtr {
    pub const ENCODED: usize = 4 + 8 + 4 + 4 + 1;
    pub fn to_bytes(&self) -> [u8; Self::ENCODED] {
        let mut b = [0u8; Self::ENCODED];
        b[0..4].copy_from_slice(&self.seg.to_le_bytes());
        b[4..12].copy_from_slice(&self.off.to_le_bytes());
        b[12..16].copy_from_slice(&self.stored_len.to_le_bytes());
        b[16..20].copy_from_slice(&self.raw_len.to_le_bytes());
        b[20] = self.flags;
        b
    }
    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        if b.len() != Self::ENCODED {
            return None;
        }
        Some(Self {
            seg: u32::from_le_bytes(b[0..4].try_into().ok()?),
            off: u64::from_le_bytes(b[4..12].try_into().ok()?),
            stored_len: u32::from_le_bytes(b[12..16].try_into().ok()?),
            raw_len: u32::from_le_bytes(b[16..20].try_into().ok()?),
            flags: b[20],
        })
    }
    /// Bytes this chunk occupies in its segment, header included.
    pub fn entry_len(&self) -> u64 {
        HEADER + self.stored_len as u64
    }
}

/// One entry found while scanning a segment.
pub struct ScannedEntry {
    pub key: [u8; 32],
    pub ptr: ChunkPtr,
}

struct Active {
    id: u32,
    file: Arc<File>,
    len: u64,
}

pub struct SegmentLog {
    dir: PathBuf,
    max_bytes: u64,
    active: Mutex<Active>,
    readers: RwLock<HashMap<u32, Arc<File>>>,
}

fn seg_path(dir: &Path, id: u32) -> PathBuf {
    dir.join(format!("seg-{id:06}.pdb"))
}

fn parse_header(h: &[u8; HEADER as usize]) -> Option<([u8; 32], u32, u32, u8)> {
    if h[0..4] != MAGIC {
        return None;
    }
    let key: [u8; 32] = h[4..36].try_into().ok()?;
    let raw = u32::from_le_bytes(h[36..40].try_into().ok()?);
    let stored = u32::from_le_bytes(h[40..44].try_into().ok()?);
    Some((key, raw, stored, h[44]))
}

/// Reads whole entries from the start of a segment until one is missing or malformed. Returns the offset just past the last good entry.
fn scan_file(file: &File, seg: u32, mut visit: impl FnMut(ScannedEntry) -> Result<()>) -> Result<u64> {
    let total = file.metadata()?.len();
    let mut off = 0u64;
    let mut h = [0u8; HEADER as usize];
    while off + HEADER <= total {
        if file.read_exact_at(&mut h, off).is_err() {
            break;
        }
        let Some((key, raw_len, stored_len, flags)) = parse_header(&h) else { break };
        let end = off + HEADER + stored_len as u64;
        if end > total {
            break;
        }
        visit(ScannedEntry { key, ptr: ChunkPtr { seg, off, stored_len, raw_len, flags } })?;
        off = end;
    }
    Ok(off)
}

impl SegmentLog {
    pub fn open(dir: &Path, max_bytes: u64) -> Result<Self> {
        fs::create_dir_all(dir)?;
        let mut ids: Vec<u32> = fs::read_dir(dir)?
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_prefix("seg-")?.strip_suffix(".pdb")?.parse().ok()))
            .collect();
        ids.sort_unstable();
        let id = ids.last().copied().unwrap_or(1);
        let path = seg_path(dir, id);
        let file = Arc::new(OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path)?);
        // Drop a torn tail so new entries start at a clean boundary.
        let good = scan_file(&file, id, |_| Ok(()))?;
        if good != file.metadata()?.len() {
            file.set_len(good)?;
            file.sync_all()?;
        }
        let mut readers = HashMap::new();
        readers.insert(id, file.clone());
        Ok(Self { dir: dir.to_path_buf(), max_bytes, active: Mutex::new(Active { id, file, len: good }), readers: RwLock::new(readers) })
    }

    pub fn active_id(&self) -> u32 {
        self.active.lock().unwrap_or_else(|e| e.into_inner()).id
    }

    pub fn segment_ids(&self) -> Result<Vec<u32>> {
        let mut ids: Vec<u32> = fs::read_dir(&self.dir)?
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_prefix("seg-")?.strip_suffix(".pdb")?.parse().ok()))
            .collect();
        ids.sort_unstable();
        Ok(ids)
    }

    pub fn segment_len(&self, id: u32) -> Result<u64> {
        Ok(fs::metadata(seg_path(&self.dir, id))?.len())
    }

    pub fn total_bytes(&self) -> Result<u64> {
        let mut t = 0;
        for id in self.segment_ids()? {
            t += self.segment_len(id)?;
        }
        Ok(t)
    }

    /// Appends one entry and returns where it landed. Rolls to a new segment when the active one is full.
    pub fn append(&self, key: &[u8; 32], raw_len: u32, flags: u8, payload: &[u8]) -> Result<ChunkPtr> {
        if payload.len() > u32::MAX as usize {
            return Err(Error::Invalid("a chunk cannot exceed 4 GiB".into()));
        }
        let entry = HEADER + payload.len() as u64;
        let mut buf = Vec::with_capacity(entry as usize);
        buf.extend_from_slice(&MAGIC);
        buf.extend_from_slice(key);
        buf.extend_from_slice(&raw_len.to_le_bytes());
        buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        buf.push(flags);
        buf.extend_from_slice(payload);

        let mut a = self.active.lock().unwrap_or_else(|e| e.into_inner());
        if a.len > 0 && a.len + entry > self.max_bytes {
            a.file.sync_data()?; // seal the old segment durably before moving on
            let id = a.id + 1;
            let file = Arc::new(OpenOptions::new().read(true).write(true).create(true).truncate(true).open(seg_path(&self.dir, id))?);
            self.readers.write().unwrap_or_else(|e| e.into_inner()).insert(id, file.clone());
            *a = Active { id, file, len: 0 };
        }
        let off = a.len;
        a.file.write_all_at(&buf, off)?;
        a.len += entry;
        Ok(ChunkPtr { seg: a.id, off, stored_len: payload.len() as u32, raw_len, flags })
    }

    /// Makes everything appended so far durable.
    pub fn sync(&self) -> Result<()> {
        let a = self.active.lock().unwrap_or_else(|e| e.into_inner());
        a.file.sync_data()?;
        Ok(())
    }

    fn reader(&self, seg: u32) -> Result<Arc<File>> {
        if let Some(f) = self.readers.read().unwrap_or_else(|e| e.into_inner()).get(&seg) {
            return Ok(f.clone());
        }
        let f = Arc::new(OpenOptions::new().read(true).open(seg_path(&self.dir, seg))?);
        self.readers.write().unwrap_or_else(|e| e.into_inner()).insert(seg, f.clone());
        Ok(f)
    }

    /// The stored (possibly encrypted) payload of one entry.
    pub fn read_payload(&self, p: &ChunkPtr) -> Result<Vec<u8>> {
        let f = self.reader(p.seg)?;
        let mut buf = vec![0u8; p.stored_len as usize];
        f.read_exact_at(&mut buf, p.off + HEADER)?;
        Ok(buf)
    }

    /// A slice of the stored payload, for chunks stored unencrypted.
    pub fn read_slice(&self, p: &ChunkPtr, from: u32, len: u32) -> Result<Vec<u8>> {
        if from as u64 + len as u64 > p.stored_len as u64 {
            return Err(Error::Invalid("slice runs past the chunk".into()));
        }
        let f = self.reader(p.seg)?;
        let mut buf = vec![0u8; len as usize];
        f.read_exact_at(&mut buf, p.off + HEADER + from as u64)?;
        Ok(buf)
    }

    pub fn scan(&self, seg: u32, visit: impl FnMut(ScannedEntry) -> Result<()>) -> Result<u64> {
        let f = self.reader(seg)?;
        scan_file(&f, seg, visit)
    }

    /// Deletes a sealed segment. Readers that already hold the file keep working (the data stays until they let go).
    pub fn remove_segment(&self, seg: u32) -> Result<()> {
        if seg == self.active_id() {
            return Err(Error::Invalid("the active segment cannot be removed".into()));
        }
        self.readers.write().unwrap_or_else(|e| e.into_inner()).remove(&seg);
        match fs::remove_file(seg_path(&self.dir, seg)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}
