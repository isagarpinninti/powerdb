//! Administrator endpoints: collections (schema and rules), users, API keys, statistics, maintenance. Anonymous callers get 401, everyone else who is not an administrator 403.

use super::{ApiError, AppState, Caller};
use crate::auth::{Access, Principal, Role};
use crate::model::CollectionDef;
use crate::security::expr;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

fn admin(who: &Principal) -> Result<(), ApiError> {
    match who {
        p if p.is_admin() => Ok(()),
        Principal::Anonymous => Err(ApiError::unauthorized("sign in as an administrator")),
        _ => Err(ApiError::forbidden("only an administrator may do that")),
    }
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, crate::Error> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| ApiError::internal("task failed"))?
        .map_err(ApiError::from)
}

fn check_rules(def: &CollectionDef) -> Result<(), ApiError> {
    for (name, r) in [
        ("list", &def.rules.list),
        ("view", &def.rules.view),
        ("create", &def.rules.create),
        ("update", &def.rules.update),
        ("delete", &def.rules.delete),
    ] {
        if let Some(r) = r
            && !r.trim().is_empty()
        {
            expr::parse(r).map_err(|e| {
                ApiError::bad_request(&format!("the {name} rule does not parse: {e}"))
            })?;
        }
    }
    Ok(())
}

pub async fn list_collections(
    State(st): State<AppState>,
    Caller(who): Caller,
) -> Result<Json<Value>, ApiError> {
    admin(&who)?;
    let db = st.db.clone();
    let cols = blocking(move || db.collections()).await?;
    Ok(Json(json!({"items": cols})))
}

pub async fn get_collection(
    State(st): State<AppState>,
    Caller(who): Caller,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    admin(&who)?;
    let db = st.db.clone();
    let (def, count) = blocking(move || Ok((db.collection(&name)?, db.count(&name)?))).await?;
    Ok(Json(json!({"collection": def, "records": count})))
}

pub async fn create_collection(
    State(st): State<AppState>,
    Caller(who): Caller,
    Json(def): Json<CollectionDef>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    admin(&who)?;
    check_rules(&def)?;
    let db = st.db.clone();
    let def = blocking(move || db.create_collection(def)).await?;
    Ok((StatusCode::CREATED, Json(json!({"collection": def}))))
}

pub async fn update_collection(
    State(st): State<AppState>,
    Caller(who): Caller,
    Path(name): Path<String>,
    Json(mut def): Json<CollectionDef>,
) -> Result<Json<Value>, ApiError> {
    admin(&who)?;
    def.name = name;
    check_rules(&def)?;
    let db = st.db.clone();
    let def = blocking(move || db.update_collection(def)).await?;
    Ok(Json(json!({"collection": def})))
}

pub async fn drop_collection(
    State(st): State<AppState>,
    Caller(who): Caller,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    admin(&who)?;
    let db = st.db.clone();
    let n = blocking(move || db.drop_collection(&name)).await?;
    Ok(Json(json!({"deleted_records": n})))
}

#[derive(Deserialize)]
pub struct NewKey {
    name: String,
    #[serde(default = "default_access")]
    access: Access,
    #[serde(default)]
    collections: Vec<String>,
    #[serde(default)]
    admin: bool,
}
fn default_access() -> Access {
    Access::Read
}

pub async fn create_key(
    State(st): State<AppState>,
    Caller(who): Caller,
    Json(k): Json<NewKey>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    admin(&who)?;
    let auth = st.auth.clone();
    let (secret, meta) = blocking(move || {
        auth.create_key(
            &k.name,
            if k.admin { Access::ReadWrite } else { k.access },
            k.collections,
            k.admin,
        )
    })
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(
            json!({"key": secret, "meta": meta, "note": "this is the only time the key is shown"}),
        ),
    ))
}

pub async fn list_keys(
    State(st): State<AppState>,
    Caller(who): Caller,
) -> Result<Json<Value>, ApiError> {
    admin(&who)?;
    let auth = st.auth.clone();
    Ok(Json(json!({"items": blocking(move || auth.keys()).await?})))
}

pub async fn revoke_key(
    State(st): State<AppState>,
    Caller(who): Caller,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    admin(&who)?;
    let auth = st.auth.clone();
    if blocking(move || auth.revoke_key(&id)).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("key"))
    }
}

#[derive(Deserialize)]
pub struct NewUser {
    email: String,
    password: String,
    #[serde(default = "default_role")]
    role: Role,
}
fn default_role() -> Role {
    Role::User
}

pub async fn create_user(
    State(st): State<AppState>,
    Caller(who): Caller,
    Json(u): Json<NewUser>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    admin(&who)?;
    let auth = st.auth.clone();
    let u = blocking(move || auth.register(&u.email, &u.password, u.role)).await?;
    Ok((StatusCode::CREATED, Json(json!({"user": u}))))
}

pub async fn list_users(
    State(st): State<AppState>,
    Caller(who): Caller,
) -> Result<Json<Value>, ApiError> {
    admin(&who)?;
    let auth = st.auth.clone();
    Ok(Json(
        json!({"items": blocking(move || auth.users()).await?}),
    ))
}

pub async fn delete_user(
    State(st): State<AppState>,
    Caller(who): Caller,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    admin(&who)?;
    if let Principal::User(u) = &who
        && u.id == id
    {
        return Err(ApiError::bad_request(
            "you cannot delete the account you are signed in with",
        ));
    }
    let auth = st.auth.clone();
    if blocking(move || auth.delete_user(&id)).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("user"))
    }
}

pub async fn stats(
    State(st): State<AppState>,
    Caller(who): Caller,
) -> Result<Json<Value>, ApiError> {
    admin(&who)?;
    let db = st.db.clone();
    let s = blocking(move || db.stats()).await?;
    Ok(Json(
        json!({"segments": s.segments, "log_bytes": s.log_bytes, "dead_bytes": s.dead_bytes, "gc_pending": s.gc_pending}),
    ))
}

pub async fn gc(State(st): State<AppState>, Caller(who): Caller) -> Result<Json<Value>, ApiError> {
    admin(&who)?;
    let db = st.db.clone();
    let (g, c) = blocking(move || db.collect(0.5)).await?;
    Ok(Json(
        json!({"files_released": g.files_released, "bytes_reclaimed": c.bytes_reclaimed, "segments_compacted": c.segments_compacted}),
    ))
}

/// Prometheus text exposition of the same numbers as `/stats`, for monitoring. Administrators only: it describes the
/// server's storage, and a scraper is given an API key with admin access.
pub async fn metrics(
    State(st): State<AppState>,
    Caller(who): Caller,
) -> Result<impl axum::response::IntoResponse, ApiError> {
    admin(&who)?;
    let db = st.db.clone();
    let s = blocking(move || db.stats()).await?;
    let body = format!(
        "# HELP powerdb_info Build information.\n# TYPE powerdb_info gauge\npowerdb_info{{version=\"{}\"}} 1\n\
         # HELP powerdb_blob_segments Segment files in the blob log.\n# TYPE powerdb_blob_segments gauge\npowerdb_blob_segments {}\n\
         # HELP powerdb_blob_log_bytes Bytes in the blob log.\n# TYPE powerdb_blob_log_bytes gauge\npowerdb_blob_log_bytes {}\n\
         # HELP powerdb_blob_dead_bytes Bytes in the blob log no record refers to; compaction reclaims them.\n# TYPE powerdb_blob_dead_bytes gauge\npowerdb_blob_dead_bytes {}\n\
         # HELP powerdb_gc_pending Chunks queued for garbage collection.\n# TYPE powerdb_gc_pending gauge\npowerdb_gc_pending {}\n",
        env!("CARGO_PKG_VERSION"),
        s.segments,
        s.log_bytes,
        s.dead_bytes,
        s.gc_pending
    );
    Ok((
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4",
        )],
        body,
    ))
}

#[derive(Deserialize, Default)]
pub struct BackupReq {
    /// Letters, digits, `.`, `_` and `-` (not first): a directory name inside the backup directory. Default: a UTC timestamp.
    name: Option<String>,
    /// Put `secret.key` in the backup. Without it the backup cannot decrypt encrypted collections unless the secret is kept elsewhere.
    #[serde(default)]
    include_secret: bool,
    /// Also re-read and verify every stored chunk before declaring the backup good (slower).
    #[serde(default)]
    deep: bool,
}

fn backup_root(st: &AppState) -> std::path::PathBuf {
    match &st.cfg.backup_dir {
        Some(d) if d.is_absolute() => d.clone(),
        Some(d) => st.db.data_dir().join(d),
        None => st.db.data_dir().join("backups"),
    }
}

fn valid_backup_name(n: &str) -> bool {
    (1..=64).contains(&n.len())
        && !n.starts_with('.')
        && n.bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

/// `backup-YYYYMMDD-HHMMSS` in UTC, without a date library (days since 1970 to a civil date).
fn default_backup_name() -> String {
    let secs = crate::util::now_ms() / 1000;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "backup-{year:04}{month:02}{day:02}-{:02}{:02}{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Takes a consistent backup of the running database without blocking reads or writes.
pub async fn backup(
    State(st): State<AppState>,
    Caller(who): Caller,
    req: Option<Json<BackupReq>>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    admin(&who)?;
    let req = req.map(|Json(r)| r).unwrap_or_default();
    let name = req.name.unwrap_or_else(default_backup_name);
    if !valid_backup_name(&name) {
        return Err(ApiError::bad_request(
            "name must be 1-64 letters, digits, '.', '_' or '-', and not start with '.'",
        ));
    }
    let root = backup_root(&st);
    let dest = root.join(&name);
    let db = st.db.clone();
    let (include_secret, deep) = (req.include_secret, req.deep);
    let report = blocking(move || {
        std::fs::create_dir_all(&root)?;
        crate::ops::backup_online(&db, &dest, include_secret, deep)
    })
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(
            json!({"name": name, "files": report.files, "bytes": report.bytes, "includes_secret": report.includes_secret}),
        ),
    ))
}

pub async fn list_backups(
    State(st): State<AppState>,
    Caller(who): Caller,
) -> Result<Json<Value>, ApiError> {
    admin(&who)?;
    let root = backup_root(&st);
    let items = blocking(move || crate::ops::list_backups(&root)).await?;
    Ok(Json(json!({ "backups": items })))
}
