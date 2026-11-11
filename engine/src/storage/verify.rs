//! `powerdb check`: an integrity scrub. It proves three things a database must never get wrong:
//!   1. every stored chunk can be read and still matches its hash (or authenticates, for encrypted chunks);
//!   2. every file a record points to exists, and every chunk a file points to exists;
//!   3. the reference counts that decide when space is freed equal the counts recomputed from the records themselves.
//!
//! A miscounted reference is the dangerous kind of bug (it ends in deleting data that is still in use), so (3) rebuilds the counts independently and compares.

use super::{
    Db,
    model::{Manifest, Record},
    objects::ObjectMeta,
    segment::ChunkPtr,
    u64_of,
};
use crate::error::{Error, Result};
use crate::util::{from_hex_32, hex};
use std::collections::HashMap;

#[derive(Debug, Default, Clone)]
pub struct VerifyReport {
    pub records: u64,
    pub objects: u64,
    pub files: u64,
    pub chunks: u64,
    pub chunk_bytes_checked: u64,
    pub problems: Vec<String>,
}

const MAX_PROBLEMS: usize = 100;

impl Db {
    /// With `content` false only the structure and the counts are checked; with true every chunk is also read and verified (reads the whole blob log).
    pub fn verify(&self, content: bool) -> Result<VerifyReport> {
        let ks = &self.inner.ks;
        let mut r = VerifyReport::default();
        let note = |r: &mut VerifyReport, m: String| {
            if r.problems.len() < MAX_PROBLEMS {
                r.problems.push(m);
            } else if r.problems.len() == MAX_PROBLEMS {
                r.problems.push("... more problems not listed".into());
            }
        };

        // the records, and from them the number of records that use each file
        let mut file_uses: HashMap<[u8; 32], u64> = HashMap::new();
        for g in ks.records.iter() {
            let (_, v) = g.into_inner()?;
            let rec: Record = serde_json::from_slice(&v)
                .map_err(|e| Error::Corrupt(format!("a record does not decode: {e}")))?;
            r.records += 1;
            for (field, f) in &rec.files {
                match from_hex_32(&f.id) {
                    Some(id) => {
                        *file_uses.entry(id).or_default() += 1;
                        if !ks.files.contains_key(id)? {
                            note(
                                &mut r,
                                format!(
                                    "record {} field {field}: its file {} is missing",
                                    rec.id, f.id
                                ),
                            );
                        }
                    }
                    None => note(
                        &mut r,
                        format!("record {} field {field}: bad file id", rec.id),
                    ),
                }
            }
        }

        // the objects: each version names a file and counts as one use of it, exactly as a record's file field does
        for g in ks.objects.iter() {
            let (_, v) = g.into_inner()?;
            let m: ObjectMeta = serde_json::from_slice(&v)
                .map_err(|e| Error::Corrupt(format!("an object does not decode: {e}")))?;
            r.objects += 1;
            let what = format!("object {}/{} (version {})", m.bucket, m.key, m.version_id);
            if !ks.buckets.contains_key(m.bucket.as_bytes())? {
                note(&mut r, format!("{what}: its bucket does not exist"));
            }
            if m.delete_marker {
                continue;
            }
            match from_hex_32(&m.file_id) {
                Some(id) => {
                    *file_uses.entry(id).or_default() += 1;
                    match ks.files.get(id)? {
                        None => note(&mut r, format!("{what}: its file {} is missing", m.file_id)),
                        Some(mv) => {
                            if Manifest::from_bytes(&mv).is_some_and(|mf| mf.size != m.size) {
                                note(&mut r, format!("{what}: its size differs from its file's"));
                            }
                        }
                    }
                }
                None => note(&mut r, format!("{what}: bad file id")),
            }
        }

        // the files, their chunks, and from them the number of files that use each chunk
        let mut chunk_uses: HashMap<[u8; 32], u64> = HashMap::new();
        for g in ks.files.iter() {
            let (k, v) = g.into_inner()?;
            r.files += 1;
            let id: [u8; 32] = k
                .as_ref()
                .try_into()
                .map_err(|_| Error::Corrupt("a file key".into()))?;
            let Some(m) = Manifest::from_bytes(&v) else {
                note(
                    &mut r,
                    format!("file {}: its manifest does not decode", hex(&id)),
                );
                continue;
            };
            if m.chunks.iter().map(|(_, l)| *l as u64).sum::<u64>() != m.size {
                note(
                    &mut r,
                    format!("file {}: its chunks do not add up to its size", hex(&id)),
                );
            }
            if m.file_id() != id {
                note(
                    &mut r,
                    format!("file {}: its manifest does not hash to its id", hex(&id)),
                );
            }
            for k in m.unique_keys() {
                *chunk_uses.entry(k).or_default() += 1;
                if !ks.chunks.contains_key(k)? {
                    note(
                        &mut r,
                        format!(
                            "file {}: chunk {} is missing from the index",
                            hex(&id),
                            hex(&k)
                        ),
                    );
                }
            }
            // a file's counter is the number of records using it; zero is fine while it waits in the release queue
            let stored = u64_of(ks.file_refs.get(id)?);
            let actual = file_uses.get(&id).copied().unwrap_or(0);
            if stored != actual {
                note(
                    &mut r,
                    format!(
                        "file {}: counted {stored} records using it, but {actual} do",
                        hex(&id)
                    ),
                );
            }
        }
        // the chunks: counters, then (optionally) contents
        for g in ks.chunks.iter() {
            let (k, v) = g.into_inner()?;
            r.chunks += 1;
            let key: [u8; 32] = k
                .as_ref()
                .try_into()
                .map_err(|_| Error::Corrupt("a chunk key".into()))?;
            let ptr =
                ChunkPtr::from_bytes(&v).ok_or_else(|| Error::Corrupt("a chunk pointer".into()))?;
            let stored = u64_of(ks.chunk_refs.get(key)?);
            let actual = chunk_uses.get(&key).copied().unwrap_or(0);
            if stored != actual {
                note(
                    &mut r,
                    format!(
                        "chunk {}: counted {stored} files using it, but {actual} do",
                        hex(&key)
                    ),
                );
            }
            if content {
                match self.read_chunk_range(&key, 0, ptr.raw_len) {
                    Ok(_) => r.chunk_bytes_checked += ptr.raw_len as u64,
                    Err(e) => note(&mut r, format!("chunk {}: {e}", hex(&key))),
                }
            }
        }
        Ok(r)
    }
}
