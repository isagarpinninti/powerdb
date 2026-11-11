//! Buckets and objects over HTTP. In this release they are for administrators (and administrator API keys): collections have rules, and
//! buckets get their own permissions later; until then nothing here is reachable by an ordinary user or a scoped key.
//!
//! `PUT /buckets/{bucket}/objects/{key}` takes the file as the raw request body (`Content-Type` is the declared type, `?filename=` an optional
//! name, `X-PowerDB-Meta-<name>: value` headers become metadata) and streams it. `GET`/`HEAD` on the same address return it, with ranges.

use super::admin::{admin, blocking};
use super::files::serve_file;
use super::upload::stage_stream;
use super::{ApiError, AppState, Caller};
use crate::storage::objects::{BucketDef, ObjectMeta, valid_key};
use crate::util::from_hex_32;
use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::Response;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};

const META_PREFIX: &str = "x-powerdb-meta-";

/// Parses a small JSON body *after* the caller has been authorized, so a malformed body never changes the answer for someone who may not use the route
/// (an extractor would reject it first, with a 422 where a 401 or 403 belongs).
fn parse<T: serde::de::DeserializeOwned>(body: &Bytes) -> Result<T, ApiError> {
    serde_json::from_slice(body)
        .map_err(|e| ApiError::bad_request(&format!("the body is not valid: {e}")))
}

pub async fn list_buckets(
    State(st): State<AppState>,
    Caller(who): Caller,
) -> Result<Json<Value>, ApiError> {
    admin(&who)?;
    let db = st.db.clone();
    let items = blocking(move || db.buckets()).await?;
    Ok(Json(json!({ "items": items })))
}

#[derive(Deserialize)]
pub struct NewBucket {
    name: String,
    #[serde(default)]
    versioning: bool,
    #[serde(default)]
    encrypt: bool,
}

pub async fn create_bucket(
    State(st): State<AppState>,
    Caller(who): Caller,
    body: Bytes,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    admin(&who)?;
    let b: NewBucket = parse(&body)?;
    let db = st.db.clone();
    let def = blocking(move || {
        db.create_bucket(BucketDef {
            name: b.name,
            versioning: b.versioning,
            encrypt: b.encrypt,
            created_at: 0,
        })
    })
    .await?;
    Ok((StatusCode::CREATED, Json(json!({ "bucket": def }))))
}

pub async fn get_bucket(
    State(st): State<AppState>,
    Caller(who): Caller,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    admin(&who)?;
    let db = st.db.clone();
    let def = blocking(move || db.bucket(&name)).await?;
    Ok(Json(json!({ "bucket": def })))
}

#[derive(Deserialize)]
pub struct BucketPatch {
    versioning: bool,
}

pub async fn update_bucket(
    State(st): State<AppState>,
    Caller(who): Caller,
    Path(name): Path<String>,
    body: Bytes,
) -> Result<Json<Value>, ApiError> {
    admin(&who)?;
    let p: BucketPatch = parse(&body)?;
    let db = st.db.clone();
    let def = blocking(move || db.set_bucket_versioning(&name, p.versioning)).await?;
    Ok(Json(json!({ "bucket": def })))
}

pub async fn delete_bucket(
    State(st): State<AppState>,
    Caller(who): Caller,
    Path(name): Path<String>,
) -> Result<StatusCode, ApiError> {
    admin(&who)?;
    let db = st.db.clone();
    blocking(move || db.delete_bucket(&name)).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn list_objects(
    State(st): State<AppState>,
    Caller(who): Caller,
    Path(bucket): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    admin(&who)?;
    let limit = match q.get("limit") {
        None => 100,
        Some(l) => l
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=1000).contains(n))
            .ok_or_else(|| ApiError::bad_request("limit must be a number from 1 to 1000"))?,
    };
    let (prefix, after) = (
        q.get("prefix").cloned().unwrap_or_default(),
        q.get("after").cloned(),
    );
    let db = st.db.clone();
    let page = blocking(move || db.list_objects(&bucket, &prefix, after.as_deref(), limit)).await?;
    Ok(Json(
        json!({ "items": page.items, "next": page.next, "limit": limit }),
    ))
}

/// The declared type without parameters (`text/plain; charset=utf-8` -> `text/plain`), or `application/octet-stream`.
fn declared_type(h: &HeaderMap) -> String {
    h.get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .map(|v| v.trim().to_ascii_lowercase())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "application/octet-stream".into())
}

fn metadata_of(h: &HeaderMap) -> Result<BTreeMap<String, String>, ApiError> {
    let mut m = BTreeMap::new();
    for (k, v) in h {
        if let Some(name) = k.as_str().strip_prefix(META_PREFIX) {
            let v = v
                .to_str()
                .map_err(|_| ApiError::bad_request("a metadata header is not plain text"))?;
            m.insert(name.to_string(), v.to_string());
        }
    }
    Ok(m)
}

fn clean_filename(n: &str) -> Option<String> {
    let s: String = n.chars().filter(|c| !c.is_control()).take(200).collect();
    if s.trim().is_empty() { None } else { Some(s) }
}

pub async fn put_object(
    State(st): State<AppState>,
    Caller(who): Caller,
    Path((bucket, key)): Path<(String, String)>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Body,
) -> Result<Json<Value>, ApiError> {
    admin(&who)?; // before a byte of the body is read
    if !valid_key(&key) {
        return Err(ApiError::bad_request(
            "the object key must be 1 to 1024 bytes, without control characters, not starting with `/`, and with no `.` or `..` segment",
        ));
    }
    let metadata = metadata_of(&headers)?;
    let db = st.db.clone();
    let b = bucket.clone();
    blocking(move || db.bucket(&b).map(|_| ())).await?; // a missing bucket is answered before the upload is read
    let filename = q.get("filename").and_then(|n| clean_filename(n));
    let pending = stage_stream(&st, &bucket, declared_type(&headers), filename, body).await?;
    let db = st.db.clone();
    let meta = blocking(move || db.put_object(&bucket, &key, pending, metadata)).await?;
    Ok(Json(json!({ "object": meta })))
}

fn object_headers(m: &ObjectMeta) -> Vec<(HeaderName, HeaderValue)> {
    let mut v = Vec::new();
    let mut push = |k: &str, val: &str| {
        if let (Ok(k), Ok(val)) = (
            HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_str(val),
        ) {
            v.push((k, val));
        }
    };
    push("x-powerdb-version-id", &m.version_id);
    push("x-powerdb-object-id", &m.id);
    for (k, val) in &m.metadata {
        push(&format!("{META_PREFIX}{k}"), val);
    }
    v
}

pub async fn get_object(
    State(st): State<AppState>,
    Caller(who): Caller,
    Path((bucket, key)): Path<(String, String)>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    admin(&who)?;
    let version = q.get("version").cloned();
    let db = st.db.clone();
    let m = blocking(move || db.get_object(&bucket, &key, version.as_deref())).await?;
    let fid = from_hex_32(&m.file_id).ok_or_else(|| ApiError::internal("bad file id"))?;
    serve_file(
        &st,
        fid,
        &m.mime,
        m.filename.as_deref(),
        format!("\"{}\"", m.checksum),
        &headers,
        object_headers(&m),
    )
    .await
}

pub async fn delete_object(
    State(st): State<AppState>,
    Caller(who): Caller,
    Path((bucket, key)): Path<(String, String)>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<StatusCode, ApiError> {
    admin(&who)?;
    let version = q.get("version").cloned();
    let db = st.db.clone();
    let changed = blocking(move || db.delete_object(&bucket, &key, version.as_deref())).await?;
    if changed {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("the object"))
    }
}

pub async fn list_versions(
    State(st): State<AppState>,
    Caller(who): Caller,
    Path(bucket): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    admin(&who)?;
    let key = q
        .get("key")
        .cloned()
        .ok_or_else(|| ApiError::bad_request("give the object's key as ?key="))?;
    let db = st.db.clone();
    let items = blocking(move || db.object_versions(&bucket, &key)).await?;
    Ok(Json(json!({ "items": items })))
}

#[derive(Deserialize)]
pub struct Restore {
    key: String,
    version: String,
}

pub async fn restore_version(
    State(st): State<AppState>,
    Caller(who): Caller,
    Path(bucket): Path<String>,
    body: Bytes,
) -> Result<Json<Value>, ApiError> {
    admin(&who)?;
    let r: Restore = parse(&body)?;
    let db = st.db.clone();
    let m = blocking(move || db.restore_object_version(&bucket, &r.key, &r.version)).await?;
    Ok(Json(json!({ "object": m })))
}
