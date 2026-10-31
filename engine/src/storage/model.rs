use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// What a record says about one of its files. The bytes live in the blob log; `id` names the manifest (hex of the manifest hash).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileRef {
    pub id: String,
    pub size: u64,
    pub mime: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    pub collection: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub fields: Map<String, Value>,
    #[serde(default)]
    pub files: BTreeMap<String, FileRef>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FieldKind {
    Text,
    Number,
    Bool,
    Json,
    /// The id of a record in another collection.
    Relation,
    File,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FieldDef {
    pub name: String,
    pub kind: FieldKind,
    #[serde(default)]
    pub required: bool,
    /// Text: maximum length in bytes. File: maximum size in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<u64>,
    /// File: allowed content types, exact (`application/pdf`) or by family (`image/*`). Empty means any.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mime: Vec<String>,
    /// Relation: the collection the id must belong to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relation: Option<String>,
}

/// Access rules, one expression per action (see `security::rules`). An absent rule means only an administrator may do it: collections are locked until opened.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Rules {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CollectionDef {
    pub name: String,
    #[serde(default)]
    pub fields: Vec<FieldDef>,
    #[serde(default)]
    pub rules: Rules,
    /// Encrypt this collection's file chunks at rest (private files). Public collections store chunks in the clear and can be streamed from the log directly.
    #[serde(default)]
    pub encrypt: bool,
    #[serde(default)]
    pub created_at: i64,
}

pub const RESERVED_FIELDS: [&str; 4] = ["id", "collection", "created_at", "updated_at"];

pub fn valid_name(n: &str) -> bool {
    let b = n.as_bytes();
    !b.is_empty() && b.len() <= 63 && b[0].is_ascii_lowercase() && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
}

fn mime_allowed(allowed: &[String], mime: &str) -> bool {
    allowed.is_empty()
        || allowed.iter().any(|a| match a.strip_suffix("/*") {
            Some(family) => mime.split('/').next() == Some(family),
            None => a.eq_ignore_ascii_case(mime),
        })
}

/// A file being attached to a record, as far as validation needs to know.
pub struct AttachedFile<'a> {
    pub size: u64,
    pub mime: &'a str,
}

impl CollectionDef {
    pub fn check_name(&self) -> Result<()> {
        if !valid_name(&self.name) {
            return Err(Error::Invalid(format!("collection name \"{}\" must be lowercase letters, digits and underscores, starting with a letter (at most 63)", self.name)));
        }
        let mut seen = std::collections::HashSet::new();
        for f in &self.fields {
            if !valid_name(&f.name) || RESERVED_FIELDS.contains(&f.name.as_str()) {
                return Err(Error::Invalid(format!("field name \"{}\" is not allowed", f.name)));
            }
            if !seen.insert(&f.name) {
                return Err(Error::Invalid(format!("field \"{}\" is declared twice", f.name)));
            }
        }
        Ok(())
    }

    /// Checks field values and file attachments against the schema. With `partial` (an update) required fields may be absent.
    pub fn validate(&self, fields: &Map<String, Value>, files: &BTreeMap<String, AttachedFile<'_>>, partial: bool) -> Result<()> {
        for k in fields.keys().chain(files.keys()) {
            if !self.fields.iter().any(|f| &f.name == k) {
                return Err(Error::Invalid(format!("collection \"{}\" has no field \"{k}\"", self.name)));
            }
        }
        for f in &self.fields {
            let v = fields.get(&f.name);
            let file = files.get(&f.name);
            if f.kind == FieldKind::File {
                if v.is_some() {
                    return Err(Error::Invalid(format!("\"{}\" is a file field: send the file itself, not a value", f.name)));
                }
                match file {
                    None if f.required && !partial => return Err(Error::Invalid(format!("file field \"{}\" is required", f.name))),
                    None => {}
                    Some(a) => {
                        if let Some(max) = f.max
                            && a.size > max {
                                return Err(Error::Invalid(format!("\"{}\" is {} bytes; the limit is {max}", f.name, a.size)));
                            }
                        if !mime_allowed(&f.mime, a.mime) {
                            return Err(Error::Invalid(format!("\"{}\" is {}; allowed: {}", f.name, a.mime, f.mime.join(", "))));
                        }
                    }
                }
                continue;
            }
            if file.is_some() {
                return Err(Error::Invalid(format!("\"{}\" is not a file field", f.name)));
            }
            match v {
                None | Some(Value::Null) => {
                    if f.required && !partial {
                        return Err(Error::Invalid(format!("field \"{}\" is required", f.name)));
                    }
                }
                Some(v) => {
                    let ok = match f.kind {
                        // a required text field means something was written: the empty string does not count
                        FieldKind::Text | FieldKind::Relation => v.as_str().is_some_and(|s| f.max.is_none_or(|m| s.len() as u64 <= m) && !(f.required && s.trim().is_empty())),
                        FieldKind::Number => v.is_number(),
                        FieldKind::Bool => v.is_boolean(),
                        FieldKind::Json => true,
                        FieldKind::File => unreachable!(),
                    };
                    if !ok {
                        return Err(Error::Invalid(format!("field \"{}\" must be {:?}{}{}", f.name, f.kind, f.max.map(|m| format!(" (at most {m} bytes)")).unwrap_or_default(), if f.required && matches!(f.kind, FieldKind::Text | FieldKind::Relation) { " and not empty" } else { "" })));
                    }
                }
            }
        }
        Ok(())
    }
}

/// The ordered chunks of a file. Entries are chunk keys (the content hash, domain-separated for encrypted chunks) and plain lengths.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub size: u64,
    pub chunks: Vec<([u8; 32], u32)>,
}

impl Manifest {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(12 + self.chunks.len() * 36);
        b.extend_from_slice(&self.size.to_le_bytes());
        b.extend_from_slice(&(self.chunks.len() as u32).to_le_bytes());
        for (k, l) in &self.chunks {
            b.extend_from_slice(k);
            b.extend_from_slice(&l.to_le_bytes());
        }
        b
    }
    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        if b.len() < 12 {
            return None;
        }
        let size = u64::from_le_bytes(b[0..8].try_into().ok()?);
        let n = u32::from_le_bytes(b[8..12].try_into().ok()?) as usize;
        if b.len() != 12 + n * 36 {
            return None;
        }
        let chunks = (0..n)
            .map(|i| {
                let o = 12 + i * 36;
                (b[o..o + 32].try_into().unwrap_or([0; 32]), u32::from_le_bytes(b[o + 32..o + 36].try_into().unwrap_or([0; 4])))
            })
            .collect();
        Some(Self { size, chunks })
    }
    /// The file's id: a hash over its chunks, so equal content under the same codec is one file.
    pub fn file_id(&self) -> [u8; 32] {
        let mut h = blake3::Hasher::new_derive_key("powerdb file id v1");
        h.update(&self.size.to_le_bytes());
        for (k, l) in &self.chunks {
            h.update(k);
            h.update(&l.to_le_bytes());
        }
        *h.finalize().as_bytes()
    }
    pub fn unique_keys(&self) -> Vec<[u8; 32]> {
        let mut v: Vec<[u8; 32]> = self.chunks.iter().map(|(k, _)| *k).collect();
        v.sort_unstable();
        v.dedup();
        v
    }
}
