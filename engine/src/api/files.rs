//! File download. A link is either a signed URL (`?token=`, valid for one file of one record for a minute) or a request with credentials that pass the
//! collection's `view` rule. Either way the record must still exist, so deleting a record ends access to its files at once. Ranges are supported so video and large
//! downloads can seek and resume.

use super::{authorize, AppState, ApiError, Caller};
use crate::auth::Action;
use crate::security::magic;
use crate::util::{from_hex_32, now_ms};
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use bytes::Bytes;
use std::collections::HashMap;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

/// `bytes=a-b`, `bytes=a-`, `bytes=-n`. None for anything else (a header we ignore is answered with the whole file, as the standard allows); Some(Err) for a
/// range that cannot be satisfied.
fn parse_range(h: &str, size: u64) -> Option<Result<(u64, u64), ()>> {
    let spec = h.strip_prefix("bytes=")?;
    if spec.contains(',') {
        return None; // multiple ranges: serve everything
    }
    let (a, b) = spec.split_once('-')?;
    let (start, end) = match (a.trim(), b.trim()) {
        ("", n) => {
            let n: u64 = n.parse().ok()?;
            if n == 0 {
                return Some(Err(()));
            }
            (size.saturating_sub(n), size)
        }
        (a, "") => (a.parse().ok()?, size),
        (a, b) => (a.parse().ok()?, b.parse::<u64>().ok()?.checked_add(1)?),
    };
    if start >= size || start >= end {
        return Some(Err(()));
    }
    Some(Ok((start, end.min(size))))
}

fn disposition(mime: &str, name: Option<&str>) -> String {
    let kind = if magic::must_download(mime) { "attachment" } else { "inline" };
    match name {
        Some(n) => {
            let enc: String = n.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect();
            format!("{kind}; filename*=UTF-8''{enc}")
        }
        None => kind.to_string(),
    }
}

pub async fn download(State(st): State<AppState>, Caller(who): Caller, Path((collection, id, field)): Path<(String, String, String)>, Query(q): Query<HashMap<String, String>>, headers: HeaderMap) -> Result<Response, ApiError> {
    let db = st.db.clone();
    let (c, i) = (collection.clone(), id.clone());
    let (def, rec) = tokio::task::spawn_blocking(move || -> Result<_, crate::Error> { Ok((db.collection(&c)?, db.get(&c, &i)?)) }).await.map_err(|_| ApiError::internal("task failed"))??;
    let rec = rec.ok_or_else(|| ApiError::not_found("file"))?;
    let f = rec.files.get(&field).ok_or_else(|| ApiError::not_found("file"))?.clone();
    match q.get("token") {
        Some(t) => st.signer.verify(&format!("{collection}/{id}/{field}/{}", f.id), t, now_ms() / 1000).map_err(ApiError::from)?,
        None => authorize(&who, &def, Action::View, Some(&rec), None)?,
    }
    let fid = from_hex_32(&f.id).ok_or_else(|| ApiError::internal("bad file id"))?;
    let db = st.db.clone();
    let manifest = tokio::task::spawn_blocking(move || db.manifest(&fid)).await.map_err(|_| ApiError::internal("task failed"))??;
    let size = manifest.size;
    let etag = format!("\"{}\"", f.id);
    let mut resp = Response::builder()
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::ETAG, &etag)
        .header(header::CACHE_CONTROL, "private, max-age=0, must-revalidate")
        .header(header::CONTENT_TYPE, HeaderValue::from_str(&f.mime).unwrap_or(HeaderValue::from_static("application/octet-stream")))
        .header(header::CONTENT_DISPOSITION, HeaderValue::from_str(&disposition(&f.mime, f.name.as_deref())).unwrap_or(HeaderValue::from_static("attachment")))
        .header(header::CONTENT_SECURITY_POLICY, "default-src 'none'; sandbox");
    if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(etag.as_str()) {
        return resp.status(StatusCode::NOT_MODIFIED).body(Body::empty()).map_err(|_| ApiError::internal("response"));
    }
    let (start, end, status) = match headers.get(header::RANGE).and_then(|v| v.to_str().ok()).and_then(|r| parse_range(r, size)) {
        Some(Ok((s, e))) => {
            resp = resp.header(header::CONTENT_RANGE, format!("bytes {s}-{}/{size}", e - 1));
            (s, e, StatusCode::PARTIAL_CONTENT)
        }
        Some(Err(())) => return resp.status(StatusCode::RANGE_NOT_SATISFIABLE).header(header::CONTENT_RANGE, format!("bytes */{size}")).body(Body::empty()).map_err(|_| ApiError::internal("response")),
        None => (0, size, StatusCode::OK),
    };
    let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(4);
    let db = st.db.clone();
    tokio::task::spawn_blocking(move || {
        let r = db.read_file_range(&manifest, start, end, &mut |b| tx.blocking_send(Ok(Bytes::copy_from_slice(b))).map_err(|_| crate::Error::Io(std::io::Error::other("client went away"))));
        if let Err(e) = r {
            let _ = tx.blocking_send(Err(std::io::Error::other(e.to_string())));
        }
    });
    resp.status(status).header(header::CONTENT_LENGTH, end - start).body(Body::from_stream(ReceiverStream::new(rx))).map_err(|_| ApiError::internal("response"))
}
