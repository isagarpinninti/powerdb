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
use crate::types::row::decode_row;
use crate::types::schema::TableDef;
use crate::util::{from_hex_32, hex};
use std::collections::HashMap;

#[derive(Debug, Default, Clone)]
pub struct VerifyReport {
    pub records: u64,
    pub objects: u64,
    pub tables: u64,
    pub table_rows: u64,
    pub indexes: u64,
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

        // typed table rows: each file value in a row (a file column, or each element of a list of files) is one use of that file. This reads the
        // rows as stored, so a value left in a row by a dropped column still counts, which is what the stored counts hold.
        for g in ks.rows.iter() {
            let (_, v) = g.into_inner()?;
            if let Ok(cols) = decode_row(&v) {
                for id in super::tables::file_ids(&cols) {
                    *file_uses.entry(id).or_default() += 1;
                    if !ks.files.contains_key(id)? {
                        note(
                            &mut r,
                            format!(
                                "a table row refers to the file {}, which is missing",
                                hex(&id)
                            ),
                        );
                    }
                }
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
        self.verify_tables(&mut r, &note)?;
        Ok(r)
    }

    /// Typed tables: every definition decodes, every row belongs to a table, decodes, fits the table's current schema (types, NOT NULL), and sits
    /// under the key its own primary-key values make.
    fn verify_tables(
        &self,
        r: &mut VerifyReport,
        note: &dyn Fn(&mut VerifyReport, String),
    ) -> Result<()> {
        let ks = &self.inner.ks;
        let mut defs: HashMap<u32, TableDef> = HashMap::new();
        for g in ks.tables.iter() {
            let (k, v) = g.into_inner()?;
            if k.first().is_none_or(|b| *b < b'a') {
                continue; // the next-table-id counter and the sequence counters
            }
            let name = String::from_utf8_lossy(&k).to_string();
            match TableDef::from_bytes(&v) {
                Ok(d) => {
                    r.tables += 1;
                    if d.name != name {
                        note(
                            r,
                            format!("table \"{name}\": its definition says it is \"{}\"", d.name),
                        );
                    }
                    if d.primary_key.is_empty() {
                        note(r, format!("table \"{name}\": it has no primary key"));
                    }
                    if defs.insert(d.id, d).is_some() {
                        note(
                            r,
                            format!("table \"{name}\": its id is used by another table"),
                        );
                    }
                }
                Err(e) => note(r, format!("table \"{name}\": {e}")),
            }
        }
        // what the index entries should be, from the rows alone
        let mut expected: HashMap<Vec<u8>, Vec<u8>> = HashMap::new();
        for g in ks.rows.iter() {
            let (k, v) = g.into_inner()?;
            r.table_rows += 1;
            let Some(id) = k
                .get(..4)
                .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
            else {
                note(r, "a table row has a key that is too short".into());
                continue;
            };
            let Some(def) = defs.get(&id) else {
                note(
                    r,
                    format!("a row belongs to table id {id}, which does not exist"),
                );
                continue;
            };
            let cols = match decode_row(&v) {
                Ok(c) => c,
                Err(e) => {
                    note(
                        r,
                        format!("table \"{}\": a row does not decode: {e}", def.name),
                    );
                    continue;
                }
            };
            if let Err(e) = def.project(&cols) {
                note(
                    r,
                    format!("table \"{}\": a row does not fit the table: {e}", def.name),
                );
            }
            if let Ok(eff) = super::tables::effective(def, &cols) {
                if let Err(e) = self.check_constraints(def, &eff) {
                    note(
                        r,
                        format!("table \"{}\": a row breaks a constraint: {e}", def.name),
                    );
                }
                if let Err(e) = self.check_fks(def, &eff, None) {
                    note(r, format!("table \"{}\": {e}", def.name));
                }
                match super::tables::index_entries(def, &eff, &k[4..]) {
                    Ok(entries) => {
                        for (ik, iv) in entries {
                            if let Some(prev) = expected.insert(ik, iv.clone())
                                && prev != iv
                            {
                                note(
                                    r,
                                    format!(
                                        "table \"{}\": two rows have the same value in a unique constraint",
                                        def.name
                                    ),
                                );
                            }
                        }
                    }
                    Err(e) => note(
                        r,
                        format!(
                            "table \"{}\": a row's index entries cannot be computed: {e}",
                            def.name
                        ),
                    ),
                }
            }
            match def.key_for_row(&cols) {
                Ok(expect) if expect == k[4..] => {}
                Ok(_) => note(
                    r,
                    format!(
                        "table \"{}\": a row is stored under a key that is not its primary key",
                        def.name
                    ),
                ),
                Err(e) => note(
                    r,
                    format!(
                        "table \"{}\": a row's primary key cannot be read: {e}",
                        def.name
                    ),
                ),
            }
        }
        // the stored index entries must be exactly those
        for g in ks.indexes.iter() {
            let (k, v) = g.into_inner()?;
            r.indexes += 1;
            match expected.remove(k.as_ref()) {
                Some(want) if want == v.as_ref() => {}
                Some(_) => note(r, "an index entry points at the wrong row".into()),
                None => note(
                    r,
                    "an index entry belongs to no row (or to a constraint that no longer exists)"
                        .into(),
                ),
            }
        }
        let missing = expected.len();
        if missing > 0 {
            note(
                r,
                format!(
                    "{missing} index entries are missing: rows that a unique constraint or foreign key should have entries for"
                ),
            );
        }
        Ok(())
    }
}
