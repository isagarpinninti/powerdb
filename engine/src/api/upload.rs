//! Reading a record's incoming data: either a JSON body, or `multipart/form-data` whose first part is `data` (the fields as JSON) followed by one part per file,
//! named after its file field. The `data` part comes first so the request can be authorized before any file is accepted.
//!
//! Files are streamed: each part goes through a small channel into a blocking thread that chunks, hashes and writes it. The first 512 bytes are checked against the
//! declared type before anything reaches the disk, and the size limit is enforced as bytes arrive, not after.

use super::{ApiError, AppState};
use crate::model::{CollectionDef, FieldDef, FieldKind};
use crate::security::magic;
use crate::storage::PendingFile;
use axum::extract::multipart::{Field, Multipart};
use axum::http::StatusCode;
use bytes::Bytes;
use serde_json::{Map, Value};
use std::io::Read;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

pub struct Upload {
    pub fields: Map<String, Value>,
    pub files: Vec<(String, PendingFile)>,
    pub remove: Vec<String>,
}

/// Separates the reserved `_remove_files` list (names of file fields to detach on update) from the record's own fields.
pub fn split_body(
    mut obj: Map<String, Value>,
) -> Result<(Map<String, Value>, Vec<String>), ApiError> {
    let remove = match obj.remove("_remove_files") {
        None => vec![],
        Some(Value::Array(a)) => a
            .into_iter()
            .map(|v| {
                v.as_str().map(str::to_string).ok_or_else(|| {
                    ApiError::bad_request("_remove_files must be a list of field names")
                })
            })
            .collect::<Result<_, _>>()?,
        Some(_) => {
            return Err(ApiError::bad_request(
                "_remove_files must be a list of field names",
            ));
        }
    };
    Ok((obj, remove))
}

pub fn parse_json_object(bytes: &[u8]) -> Result<Map<String, Value>, ApiError> {
    match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Object(o)) => Ok(o),
        Ok(_) => Err(ApiError::bad_request("the body must be a JSON object")),
        Err(e) => Err(ApiError::bad_request(&format!(
            "the body is not valid JSON: {e}"
        ))),
    }
}

fn mp_err(e: axum::extract::multipart::MultipartError) -> ApiError {
    ApiError::new(
        e.status(),
        "invalid",
        format!("bad multipart request: {}", e.body_text()),
    )
}

fn clean_name(n: &str) -> Option<String> {
    let base = n.rsplit(['/', '\\']).next().unwrap_or("");
    let s: String = base.chars().filter(|c| !c.is_control()).take(200).collect();
    if s.trim().is_empty() { None } else { Some(s) }
}

#[derive(Default)]
struct Shared {
    failure: Mutex<Option<(StatusCode, String)>>,
    mime: Mutex<Option<String>>,
}

impl Shared {
    fn fail(&self, status: StatusCode, msg: String) -> std::io::Error {
        *self.failure.lock().unwrap_or_else(|e| e.into_inner()) = Some((status, msg));
        std::io::Error::other("upload rejected")
    }
}

/// Presents the chunks arriving from the network as a blocking `Read`, applying the content check and the size limit on the way.
struct UploadReader {
    rx: mpsc::Receiver<Bytes>,
    declared: String,
    max: u64,
    total: u64,
    buf: Bytes,
    pos: usize,
    primed: bool,
    shared: Arc<Shared>,
}

impl UploadReader {
    fn count(&mut self, n: usize) -> std::io::Result<()> {
        self.total += n as u64;
        if self.total > self.max {
            return Err(self.shared.fail(
                StatusCode::PAYLOAD_TOO_LARGE,
                format!("the file is larger than the {} byte limit", self.max),
            ));
        }
        Ok(())
    }

    /// Collects the first bytes, checks them against the declared type, and only then lets anything through to be written.
    fn prime(&mut self) -> std::io::Result<()> {
        let mut head: Vec<u8> = Vec::new();
        while head.len() < magic::HEAD_BYTES {
            match self.rx.blocking_recv() {
                Some(b) => {
                    self.count(b.len())?;
                    head.extend_from_slice(&b);
                }
                None => break,
            }
        }
        match magic::check(&self.declared, &head[..head.len().min(magic::HEAD_BYTES)]) {
            Ok(m) => *self.shared.mime.lock().unwrap_or_else(|e| e.into_inner()) = Some(m),
            Err(e) => {
                return Err(self.shared.fail(
                    StatusCode::BAD_REQUEST,
                    e.to_string().trim_start_matches("invalid: ").to_string(),
                ));
            }
        }
        self.buf = Bytes::from(head);
        self.pos = 0;
        self.primed = true;
        Ok(())
    }
}

impl Read for UploadReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if !self.primed {
            self.prime()?;
        }
        while self.pos >= self.buf.len() {
            match self.rx.blocking_recv() {
                Some(b) => {
                    self.count(b.len())?;
                    self.buf = b;
                    self.pos = 0;
                }
                None => return Ok(0),
            }
        }
        let n = out.len().min(self.buf.len() - self.pos);
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

async fn stage_part(
    st: &AppState,
    def: &CollectionDef,
    fd: &FieldDef,
    mime: String,
    filename: Option<String>,
    part: &mut Field<'_>,
) -> Result<PendingFile, ApiError> {
    let (tx, rx) = mpsc::channel::<Bytes>(8);
    let shared = Arc::new(Shared::default());
    let max = fd.max.unwrap_or(u64::MAX).min(st.cfg.max_upload_bytes);
    let reader = UploadReader {
        rx,
        declared: mime.clone(),
        max,
        total: 0,
        buf: Bytes::new(),
        pos: 0,
        primed: false,
        shared: shared.clone(),
    };
    let db = st.db.clone();
    let collection = def.name.clone();
    let name = filename.clone();
    let declared = mime.clone();
    let job = tokio::task::spawn_blocking(move || {
        db.stage_file(&collection, &declared, name.as_deref(), reader)
    });
    let mut transport_error = None;
    loop {
        match part.chunk().await {
            Ok(Some(c)) => {
                if tx.send(c).await.is_err() {
                    break; // the writer gave up (rejected content, too large, or an error): stop reading
                }
            }
            Ok(None) => break,
            Err(e) => {
                transport_error = Some(mp_err(e));
                break;
            }
        }
    }
    drop(tx);
    let result = job
        .await
        .map_err(|_| ApiError::internal("upload task failed"))?;
    if let Some((status, msg)) = shared
        .failure
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
    {
        return Err(ApiError::new(
            status,
            if status == StatusCode::PAYLOAD_TOO_LARGE {
                "too_large"
            } else {
                "invalid"
            },
            format!("\"{}\": {msg}", fd.name),
        ));
    }
    if let Some(e) = transport_error {
        return Err(e);
    }
    let mut p = result.map_err(ApiError::from)?;
    if let Some(m) = shared.mime.lock().unwrap_or_else(|e| e.into_inner()).take() {
        p.mime = m;
    }
    Ok(p)
}

/// Reads a multipart request. `authorize` runs on the `data` part before any file is read.
pub async fn read_multipart(
    st: &AppState,
    def: &CollectionDef,
    mut mp: Multipart,
    authorize: impl Fn(&Map<String, Value>) -> Result<(), ApiError>,
) -> Result<Upload, ApiError> {
    let mut up = Upload {
        fields: Map::new(),
        files: Vec::new(),
        remove: Vec::new(),
    };
    let mut got_data = false;
    while let Some(mut field) = mp.next_field().await.map_err(mp_err)? {
        let name = field
            .name()
            .ok_or_else(|| ApiError::bad_request("a part has no name"))?
            .to_string();
        if name == "data" {
            if got_data {
                return Err(ApiError::bad_request("the \"data\" part appears twice"));
            }
            let mut body = Vec::new();
            while let Some(c) = field.chunk().await.map_err(mp_err)? {
                body.extend_from_slice(&c);
                if body.len() > 1 << 20 {
                    return Err(ApiError::too_large(
                        "the \"data\" part is larger than 1 MiB",
                    ));
                }
            }
            let (fields, remove) = split_body(parse_json_object(&body)?)?;
            authorize(&fields)?;
            up.fields = fields;
            up.remove = remove;
            got_data = true;
            continue;
        }
        if !got_data {
            return Err(ApiError::bad_request(
                "send the \"data\" part (the record's fields as JSON, {} if none) before any file",
            ));
        }
        let fd = def
            .fields
            .iter()
            .find(|f| f.name == name && f.kind == FieldKind::File)
            .ok_or_else(|| {
                ApiError::bad_request(&format!(
                    "collection \"{}\" has no file field \"{name}\"",
                    def.name
                ))
            })?;
        if up.files.iter().any(|(n, _)| n == &name) {
            return Err(ApiError::bad_request(&format!(
                "file field \"{name}\" appears twice"
            )));
        }
        let mime = field
            .content_type()
            .unwrap_or("application/octet-stream")
            .to_string();
        let filename = field.file_name().and_then(clean_name);
        let pending = stage_part(st, def, fd, mime, filename, &mut field).await?;
        up.files.push((name, pending));
    }
    if !got_data {
        return Err(ApiError::bad_request("the request has no \"data\" part"));
    }
    Ok(up)
}

/// Stages a raw request body as an object's bytes, through the same streaming path as a multipart part: the first bytes are checked against the declared
/// type before anything is written, and the size limit is enforced as bytes arrive.
pub async fn stage_stream(
    st: &AppState,
    bucket: &str,
    mime: String,
    filename: Option<String>,
    body: axum::body::Body,
) -> Result<PendingFile, ApiError> {
    use futures_util::StreamExt;
    let (tx, rx) = mpsc::channel::<Bytes>(8);
    let shared = Arc::new(Shared::default());
    let reader = UploadReader {
        rx,
        declared: mime.clone(),
        max: st.cfg.max_upload_bytes,
        total: 0,
        buf: Bytes::new(),
        pos: 0,
        primed: false,
        shared: shared.clone(),
    };
    let db = st.db.clone();
    let b = bucket.to_string();
    let declared = mime.clone();
    let job = tokio::task::spawn_blocking(move || {
        db.stage_object(&b, &declared, filename.as_deref(), reader)
    });
    let mut stream = body.into_data_stream();
    let mut transport_error = None;
    while let Some(next) = stream.next().await {
        match next {
            Ok(c) => {
                if tx.send(c).await.is_err() {
                    break; // the writer gave up (rejected content, too large, or an error): stop reading
                }
            }
            Err(e) => {
                transport_error = Some(ApiError::bad_request(&format!(
                    "the upload was interrupted: {e}"
                )));
                break;
            }
        }
    }
    drop(tx);
    let result = job
        .await
        .map_err(|_| ApiError::internal("upload task failed"))?;
    if let Some((status, msg)) = shared
        .failure
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
    {
        return Err(ApiError::new(
            status,
            if status == StatusCode::PAYLOAD_TOO_LARGE {
                "too_large"
            } else {
                "invalid"
            },
            msg,
        ));
    }
    if let Some(e) = transport_error {
        return Err(e); // a body cut short must not become a (shorter) object
    }
    let mut p = result.map_err(ApiError::from)?;
    if let Some(m) = shared.mime.lock().unwrap_or_else(|e| e.into_inner()).take() {
        p.mime = m;
    }
    Ok(p)
}
