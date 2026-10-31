//! Administrator endpoints: collections (schema and rules), users, API keys, statistics, maintenance. Anonymous callers get 401, everyone else who is not an administrator 403.

use super::{AppState, ApiError, Caller};
use crate::auth::{Access, Principal, Role};
use crate::model::CollectionDef;
use crate::security::expr;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

fn admin(who: &Principal) -> Result<(), ApiError> {
    match who {
        p if p.is_admin() => Ok(()),
        Principal::Anonymous => Err(ApiError::unauthorized("sign in as an administrator")),
        _ => Err(ApiError::forbidden("only an administrator may do that")),
    }
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, crate::Error> + Send + 'static) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(f).await.map_err(|_| ApiError::internal("task failed"))?.map_err(ApiError::from)
}

fn check_rules(def: &CollectionDef) -> Result<(), ApiError> {
    for (name, r) in [("list", &def.rules.list), ("view", &def.rules.view), ("create", &def.rules.create), ("update", &def.rules.update), ("delete", &def.rules.delete)] {
        if let Some(r) = r {
            if !r.trim().is_empty() {
                expr::parse(r).map_err(|e| ApiError::bad_request(&format!("the {name} rule does not parse: {e}")))?;
            }
        }
    }
    Ok(())
}

pub async fn list_collections(State(st): State<AppState>, Caller(who): Caller) -> Result<Json<Value>, ApiError> {
    admin(&who)?;
    let db = st.db.clone();
    let cols = blocking(move || db.collections()).await?;
    Ok(Json(json!({"items": cols})))
}

pub async fn get_collection(State(st): State<AppState>, Caller(who): Caller, Path(name): Path<String>) -> Result<Json<Value>, ApiError> {
    admin(&who)?;
    let db = st.db.clone();
    let (def, count) = blocking(move || Ok((db.collection(&name)?, db.count(&name)?))).await?;
    Ok(Json(json!({"collection": def, "records": count})))
}

pub async fn create_collection(State(st): State<AppState>, Caller(who): Caller, Json(def): Json<CollectionDef>) -> Result<(StatusCode, Json<Value>), ApiError> {
    admin(&who)?;
    check_rules(&def)?;
    let db = st.db.clone();
    let def = blocking(move || db.create_collection(def)).await?;
    Ok((StatusCode::CREATED, Json(json!({"collection": def}))))
}

pub async fn update_collection(State(st): State<AppState>, Caller(who): Caller, Path(name): Path<String>, Json(mut def): Json<CollectionDef>) -> Result<Json<Value>, ApiError> {
    admin(&who)?;
    def.name = name;
    check_rules(&def)?;
    let db = st.db.clone();
    let def = blocking(move || db.update_collection(def)).await?;
    Ok(Json(json!({"collection": def})))
}

pub async fn drop_collection(State(st): State<AppState>, Caller(who): Caller, Path(name): Path<String>) -> Result<Json<Value>, ApiError> {
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

pub async fn create_key(State(st): State<AppState>, Caller(who): Caller, Json(k): Json<NewKey>) -> Result<(StatusCode, Json<Value>), ApiError> {
    admin(&who)?;
    let auth = st.auth.clone();
    let (secret, meta) = blocking(move || auth.create_key(&k.name, if k.admin { Access::ReadWrite } else { k.access }, k.collections, k.admin)).await?;
    Ok((StatusCode::CREATED, Json(json!({"key": secret, "meta": meta, "note": "this is the only time the key is shown"}))))
}

pub async fn list_keys(State(st): State<AppState>, Caller(who): Caller) -> Result<Json<Value>, ApiError> {
    admin(&who)?;
    let auth = st.auth.clone();
    Ok(Json(json!({"items": blocking(move || auth.keys()).await?})))
}

pub async fn revoke_key(State(st): State<AppState>, Caller(who): Caller, Path(id): Path<String>) -> Result<StatusCode, ApiError> {
    admin(&who)?;
    let auth = st.auth.clone();
    if blocking(move || auth.revoke_key(&id)).await? { Ok(StatusCode::NO_CONTENT) } else { Err(ApiError::not_found("key")) }
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

pub async fn create_user(State(st): State<AppState>, Caller(who): Caller, Json(u): Json<NewUser>) -> Result<(StatusCode, Json<Value>), ApiError> {
    admin(&who)?;
    let auth = st.auth.clone();
    let u = blocking(move || auth.register(&u.email, &u.password, u.role)).await?;
    Ok((StatusCode::CREATED, Json(json!({"user": u}))))
}

pub async fn list_users(State(st): State<AppState>, Caller(who): Caller) -> Result<Json<Value>, ApiError> {
    admin(&who)?;
    let auth = st.auth.clone();
    Ok(Json(json!({"items": blocking(move || auth.users()).await?})))
}

pub async fn delete_user(State(st): State<AppState>, Caller(who): Caller, Path(id): Path<String>) -> Result<StatusCode, ApiError> {
    admin(&who)?;
    if let Principal::User(u) = &who {
        if u.id == id {
            return Err(ApiError::bad_request("you cannot delete the account you are signed in with"));
        }
    }
    let auth = st.auth.clone();
    if blocking(move || auth.delete_user(&id)).await? { Ok(StatusCode::NO_CONTENT) } else { Err(ApiError::not_found("user")) }
}

pub async fn stats(State(st): State<AppState>, Caller(who): Caller) -> Result<Json<Value>, ApiError> {
    admin(&who)?;
    let db = st.db.clone();
    let s = blocking(move || db.stats()).await?;
    Ok(Json(json!({"segments": s.segments, "log_bytes": s.log_bytes, "dead_bytes": s.dead_bytes, "gc_pending": s.gc_pending})))
}

pub async fn gc(State(st): State<AppState>, Caller(who): Caller) -> Result<Json<Value>, ApiError> {
    admin(&who)?;
    let db = st.db.clone();
    let (g, c) = blocking(move || db.collect(0.5)).await?;
    Ok(Json(json!({"files_released": g.files_released, "bytes_reclaimed": c.bytes_reclaimed, "segments_compacted": c.segments_compacted})))
}
