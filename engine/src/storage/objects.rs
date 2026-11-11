//! Buckets and objects: named containers and the files in them, with no record involved.
//!
//! An object is a file reached by `bucket` and `key`. Its bytes are stored exactly as a record's files are (chunked, content-addressed,
//! deduplicated, optionally encrypted), and it counts toward that file's reference count, so the garbage collector, compaction, backup and
//! `powerdb check` treat records and objects alike. What this module adds is the catalog: which key names which file, and the versions.
//!
//! * `buckets`: name -> `BucketDef`.
//! * `objects`: `bucket 0x00 key 0x00 version id` -> `ObjectMeta`. Version ids are time-ordered, so the newest version of a key is the last
//!   entry under its prefix. A key cannot contain a control character, so the separators are unambiguous.
//! * A bucket without versioning keeps one entry per key: a put replaces it, a delete removes it for good.
//! * A bucket with versioning keeps every put. A delete without a version adds a delete marker (the key reads as absent); a delete naming a
//!   version removes that version for good. Versioning can be switched on but not off, because turning it off would have to decide what to do
//!   with the versions that exist.
//!
//! Every change is one atomic batch together with its reference-count changes, through the same code a record's commit uses.

use super::{Db, PendingFile};
use crate::error::{Error, Result};
use crate::util::{from_hex_32, hex, new_id, now_ms};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

pub const MAX_KEY_BYTES: usize = 1024;
pub const MAX_METADATA_ENTRIES: usize = 32;
pub const MAX_METADATA_KEY: usize = 64;
pub const MAX_METADATA_VALUE: usize = 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BucketDef {
    pub name: String,
    /// Keep every version of an object. Can be turned on, not off.
    #[serde(default)]
    pub versioning: bool,
    /// Encrypt the bucket's file chunks at rest (ChaCha20-Poly1305), as a collection's `encrypt` does.
    #[serde(default)]
    pub encrypt: bool,
    #[serde(default)]
    pub created_at: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ObjectMeta {
    /// Names the object across its versions: the same for every version of one key, new if the key is created again after all versions were removed.
    pub id: String,
    pub bucket: String,
    pub key: String,
    pub version_id: String,
    /// A version that says "this key is deleted". It has no file.
    #[serde(default)]
    pub delete_marker: bool,
    /// Hex id of the file (the hash of its manifest). Empty for a delete marker.
    #[serde(default)]
    pub file_id: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub mime: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    /// BLAKE3 of the content, hex.
    #[serde(default)]
    pub checksum: String,
    /// When this version was written.
    pub created_at: i64,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

/// One page of a listing: the newest version of each key (delete markers left out), in key order.
#[derive(Clone, Debug, Default)]
pub struct ObjectPage {
    pub items: Vec<ObjectMeta>,
    /// Pass as `after` to get the next page; `None` when this was the last.
    pub next: Option<String>,
}

pub fn valid_bucket_name(n: &str) -> bool {
    let b = n.as_bytes();
    !b.is_empty()
        && b.len() <= 63
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-' || *c == b'_')
}

/// A key is any text of up to 1024 bytes without control characters, not starting with `/`, and with no `.` or `..` segment. Keys are labels:
/// they are never turned into a path on disk, so these rules are about keeping them unambiguous, not about protecting a file system.
pub fn valid_key(k: &str) -> bool {
    !k.is_empty()
        && k.len() <= MAX_KEY_BYTES
        && !k.starts_with('/')
        && !k.chars().any(|c| c.is_control())
        && !k.split('/').any(|s| s == "." || s == "..")
}

fn check_metadata(m: &BTreeMap<String, String>) -> Result<()> {
    if m.len() > MAX_METADATA_ENTRIES {
        return Err(Error::Invalid(format!(
            "at most {MAX_METADATA_ENTRIES} metadata entries"
        )));
    }
    for (k, v) in m {
        if k.is_empty()
            || k.len() > MAX_METADATA_KEY
            || !k
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'.' || c == b'-')
        {
            return Err(Error::Invalid(format!(
                "metadata name \"{k}\" must be 1 to {MAX_METADATA_KEY} letters, digits, `_`, `.` or `-`"
            )));
        }
        if v.len() > MAX_METADATA_VALUE || v.chars().any(|c| c.is_control()) {
            return Err(Error::Invalid(format!(
                "metadata value of \"{k}\" must be at most {MAX_METADATA_VALUE} bytes without control characters"
            )));
        }
    }
    Ok(())
}

fn prefix_of(bucket: &str, key: &str) -> Vec<u8> {
    let mut k = Vec::with_capacity(bucket.len() + key.len() + 2);
    k.extend_from_slice(bucket.as_bytes());
    k.push(0);
    k.extend_from_slice(key.as_bytes());
    k.push(0);
    k
}

fn entry_key(bucket: &str, key: &str, version: &str) -> Vec<u8> {
    let mut k = prefix_of(bucket, key);
    k.extend_from_slice(version.as_bytes());
    k
}

fn file_id_of(m: &ObjectMeta) -> Result<Option<[u8; 32]>> {
    if m.delete_marker {
        return Ok(None);
    }
    from_hex_32(&m.file_id)
        .map(Some)
        .ok_or_else(|| Error::Corrupt(format!("object {}/{} has a bad file id", m.bucket, m.key)))
}

impl Db {
    // ---- buckets ----

    pub fn create_bucket(&self, mut def: BucketDef) -> Result<BucketDef> {
        if !valid_bucket_name(&def.name) {
            return Err(Error::Invalid(format!(
                "bucket name \"{}\" must be lowercase letters, digits, `-` and `_`, starting with a letter or digit (at most 63)",
                def.name
            )));
        }
        def.created_at = now_ms();
        let _g = self.inner.commit.lock().unwrap_or_else(|e| e.into_inner());
        if self.inner.ks.buckets.contains_key(def.name.as_bytes())? {
            return Err(Error::Conflict(format!(
                "bucket \"{}\" already exists",
                def.name
            )));
        }
        let mut batch = self.inner.store.batch();
        batch.insert(
            &self.inner.ks.buckets,
            def.name.as_bytes().to_vec(),
            serde_json::to_vec(&def)?,
        );
        self.finish_batch(batch)?;
        Ok(def)
    }

    pub fn bucket(&self, name: &str) -> Result<BucketDef> {
        match self.inner.ks.buckets.get(name.as_bytes())? {
            Some(v) => Ok(serde_json::from_slice(&v)?),
            None => Err(Error::NotFound(format!("bucket \"{name}\""))),
        }
    }

    pub fn buckets(&self) -> Result<Vec<BucketDef>> {
        let mut out = Vec::new();
        for g in self.inner.ks.buckets.iter() {
            let (_, v) = g.into_inner()?;
            out.push(serde_json::from_slice(&v)?);
        }
        Ok(out)
    }

    /// Turns versioning on. Turning it off is refused: the versions that exist would have nowhere to go.
    pub fn set_bucket_versioning(&self, name: &str, on: bool) -> Result<BucketDef> {
        let _g = self.inner.commit.lock().unwrap_or_else(|e| e.into_inner());
        let mut def = self.bucket(name)?;
        if def.versioning == on {
            return Ok(def);
        }
        if !on {
            return Err(Error::Invalid(
                "versioning cannot be turned off once it is on".into(),
            ));
        }
        def.versioning = true;
        let mut batch = self.inner.store.batch();
        batch.insert(
            &self.inner.ks.buckets,
            name.as_bytes().to_vec(),
            serde_json::to_vec(&def)?,
        );
        self.finish_batch(batch)?;
        Ok(def)
    }

    /// Removes an empty bucket. A bucket that still holds any version of any key (a delete marker counts) is refused.
    pub fn delete_bucket(&self, name: &str) -> Result<()> {
        let _g = self.inner.commit.lock().unwrap_or_else(|e| e.into_inner());
        self.bucket(name)?;
        let mut p = name.as_bytes().to_vec();
        p.push(0);
        if self.inner.ks.objects.prefix(&p).next().is_some() {
            return Err(Error::Conflict(format!(
                "bucket \"{name}\" is not empty; delete its objects first"
            )));
        }
        let mut batch = self.inner.store.batch();
        batch.remove(&self.inner.ks.buckets, name.as_bytes().to_vec());
        self.finish_batch(batch)
    }

    // ---- objects ----

    /// Stages an object's bytes in the bucket's chunk store (encrypted if the bucket is). Nothing refers to them until `put_object`.
    pub fn stage_object(
        &self,
        bucket: &str,
        mime: &str,
        filename: Option<&str>,
        reader: impl std::io::Read,
    ) -> Result<PendingFile> {
        let def = self.bucket(bucket)?;
        self.stage_with(def.encrypt, mime, filename, reader)
    }

    fn versions_of(&self, bucket: &str, key: &str) -> Result<Vec<ObjectMeta>> {
        let mut out = Vec::new();
        for g in self.inner.ks.objects.prefix(prefix_of(bucket, key)) {
            let (_, v) = g.into_inner()?;
            out.push(serde_json::from_slice(&v)?);
        }
        Ok(out) // oldest first: version ids sort in time order
    }

    /// Writes a new version of `key` (the only version, in a bucket without versioning) pointing at an already staged file.
    pub fn put_object(
        &self,
        bucket: &str,
        key: &str,
        file: PendingFile,
        metadata: BTreeMap<String, String>,
    ) -> Result<ObjectMeta> {
        if !valid_key(key) {
            return Err(Error::Invalid(format!(
                "object key \"{}\" must be 1 to {MAX_KEY_BYTES} bytes, without control characters, not starting with `/`, and with no `.` or `..` segment",
                key.escape_debug()
            )));
        }
        check_metadata(&metadata)?;
        let _g = self.inner.commit.lock().unwrap_or_else(|e| e.into_inner());
        let def = self.bucket(bucket)?; // read under the lock: a bucket cannot be deleted between here and the write
        let existing = self.versions_of(bucket, key)?;
        let meta = ObjectMeta {
            id: existing.last().map(|m| m.id.clone()).unwrap_or_else(new_id),
            bucket: bucket.to_string(),
            key: key.to_string(),
            version_id: new_id(),
            delete_marker: false,
            file_id: hex(&file.file_id),
            size: file.size(),
            mime: file.mime.clone(),
            filename: file.name.clone(),
            checksum: hex(&file.content_hash),
            created_at: now_ms(),
            metadata,
        };
        let mut batch = self.inner.store.batch();
        let mut delta: HashMap<[u8; 32], i64> = HashMap::new();
        *delta.entry(file.file_id).or_default() += 1;
        if !def.versioning {
            // one version per key: the old one goes, and releases its file
            for old in &existing {
                batch.remove(
                    &self.inner.ks.objects,
                    entry_key(bucket, key, &old.version_id),
                );
                if let Some(f) = file_id_of(old)? {
                    *delta.entry(f).or_default() -= 1;
                }
            }
        }
        self.apply_file_deltas(&mut batch, &delta, &[&file])?;
        batch.insert(
            &self.inner.ks.objects,
            entry_key(bucket, key, &meta.version_id),
            serde_json::to_vec(&meta)?,
        );
        self.finish_batch(batch)?;
        Ok(meta)
    }

    /// The version named, or the newest. A delete marker reads as not found.
    pub fn get_object(&self, bucket: &str, key: &str, version: Option<&str>) -> Result<ObjectMeta> {
        self.bucket(bucket)?;
        let found = match version {
            Some(v) => self
                .inner
                .ks
                .objects
                .get(entry_key(bucket, key, v))?
                .map(|b| serde_json::from_slice::<ObjectMeta>(&b))
                .transpose()?,
            None => self.versions_of(bucket, key)?.pop(),
        };
        match found {
            Some(m) if !m.delete_marker => Ok(m),
            Some(_) => Err(Error::NotFound(format!(
                "object \"{key}\" in bucket \"{bucket}\" (deleted)"
            ))),
            None => Err(Error::NotFound(format!(
                "object \"{key}\" in bucket \"{bucket}\""
            ))),
        }
    }

    /// Every version of a key, newest first, delete markers included.
    pub fn object_versions(&self, bucket: &str, key: &str) -> Result<Vec<ObjectMeta>> {
        self.bucket(bucket)?;
        let mut v = self.versions_of(bucket, key)?;
        v.reverse();
        Ok(v)
    }

    /// Deletes an object. Without `version`: in a bucket without versioning the key is removed for good; in a versioned bucket a delete marker
    /// is added (the data stays and can be restored). With `version`: that version is removed for good. Returns whether anything changed.
    pub fn delete_object(&self, bucket: &str, key: &str, version: Option<&str>) -> Result<bool> {
        let _g = self.inner.commit.lock().unwrap_or_else(|e| e.into_inner());
        let def = self.bucket(bucket)?;
        let existing = self.versions_of(bucket, key)?;
        let mut batch = self.inner.store.batch();
        let mut delta: HashMap<[u8; 32], i64> = HashMap::new();
        let mut remove = |batch: &mut fjall::OwnedWriteBatch, m: &ObjectMeta| -> Result<()> {
            batch.remove(
                &self.inner.ks.objects,
                entry_key(bucket, key, &m.version_id),
            );
            if let Some(f) = file_id_of(m)? {
                *delta.entry(f).or_default() -= 1;
            }
            Ok(())
        };
        match version {
            Some(v) => {
                let Some(m) = existing.iter().find(|m| m.version_id == v) else {
                    return Ok(false);
                };
                remove(&mut batch, m)?;
            }
            None if !def.versioning => {
                if existing.is_empty() {
                    return Ok(false);
                }
                for m in &existing {
                    remove(&mut batch, m)?;
                }
            }
            None => {
                match existing.last() {
                    None => return Ok(false),
                    Some(m) if m.delete_marker => return Ok(false), // already deleted
                    Some(last) => {
                        let marker = ObjectMeta {
                            id: last.id.clone(),
                            bucket: bucket.to_string(),
                            key: key.to_string(),
                            version_id: new_id(),
                            delete_marker: true,
                            file_id: String::new(),
                            size: 0,
                            mime: String::new(),
                            filename: None,
                            checksum: String::new(),
                            created_at: now_ms(),
                            metadata: BTreeMap::new(),
                        };
                        batch.insert(
                            &self.inner.ks.objects,
                            entry_key(bucket, key, &marker.version_id),
                            serde_json::to_vec(&marker)?,
                        );
                    }
                }
            }
        }
        self.apply_file_deltas(&mut batch, &delta, &[])?;
        self.finish_batch(batch)?;
        Ok(true)
    }

    /// Makes an old version the newest again, as a new version that shares its file. Only in a bucket with versioning.
    pub fn restore_object_version(
        &self,
        bucket: &str,
        key: &str,
        version: &str,
    ) -> Result<ObjectMeta> {
        let _g = self.inner.commit.lock().unwrap_or_else(|e| e.into_inner());
        let def = self.bucket(bucket)?;
        if !def.versioning {
            return Err(Error::Invalid(format!(
                "bucket \"{bucket}\" does not keep versions"
            )));
        }
        let existing = self.versions_of(bucket, key)?;
        let src = existing
            .iter()
            .find(|m| m.version_id == version)
            .ok_or_else(|| Error::NotFound(format!("version {version} of \"{key}\"")))?;
        let Some(file) = file_id_of(src)? else {
            return Err(Error::Invalid(
                "that version is a delete marker; there is nothing to restore".into(),
            ));
        };
        let meta = ObjectMeta {
            version_id: new_id(),
            created_at: now_ms(),
            ..src.clone()
        };
        let mut batch = self.inner.store.batch();
        let mut delta: HashMap<[u8; 32], i64> = HashMap::new();
        *delta.entry(file).or_default() += 1;
        self.apply_file_deltas(&mut batch, &delta, &[])?;
        batch.insert(
            &self.inner.ks.objects,
            entry_key(bucket, key, &meta.version_id),
            serde_json::to_vec(&meta)?,
        );
        self.finish_batch(batch)?;
        Ok(meta)
    }

    /// The newest version of each key that starts with `prefix`, in key order, after the key `after`, at most `limit`. Delete markers hide their key.
    pub fn list_objects(
        &self,
        bucket: &str,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<ObjectPage> {
        self.bucket(bucket)?;
        let limit = limit.clamp(1, 1000);
        let mut start = bucket.as_bytes().to_vec();
        start.push(0);
        let base = start.clone();
        match after {
            // every entry of `after` sorts below `after` + 0x01, and keys cannot contain a control character, so this skips exactly its versions
            Some(a) if a >= prefix => {
                start.extend_from_slice(a.as_bytes());
                start.push(1);
            }
            _ => start.extend_from_slice(prefix.as_bytes()),
        }
        let mut want = base;
        want.extend_from_slice(prefix.as_bytes());

        let mut page = ObjectPage::default();
        let mut current: Option<ObjectMeta> = None;
        let emit = |m: Option<ObjectMeta>, page: &mut ObjectPage| {
            if let Some(m) = m
                && !m.delete_marker
            {
                page.items.push(m);
            }
        };
        for g in self.inner.ks.objects.range(start..) {
            let (k, v) = g.into_inner()?;
            if !k.starts_with(&want) {
                break;
            }
            let m: ObjectMeta = serde_json::from_slice(&v)?;
            match &current {
                Some(c) if c.key == m.key => current = Some(m), // a newer version of the same key
                _ => {
                    emit(current.take(), &mut page);
                    if page.items.len() >= limit {
                        page.next = page.items.last().map(|m| m.key.clone());
                        return Ok(page);
                    }
                    current = Some(m);
                }
            }
        }
        emit(current.take(), &mut page);
        Ok(page)
    }

    /// How many object versions exist (for statistics and tests).
    pub fn count_object_versions(&self) -> Result<u64> {
        let mut n = 0;
        for g in self.inner.ks.objects.iter() {
            g.key()?;
            n += 1;
        }
        Ok(n)
    }
}
