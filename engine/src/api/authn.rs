use super::{ApiError, AppState, Caller, Peer, client_ip};
use crate::auth::{Principal, Role, SESSION_SECS};
use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;

#[derive(Deserialize)]
pub struct Credentials {
    email: String,
    password: String,
}

fn throttle(st: &AppState, headers: &HeaderMap, peer: Peer, email: &str) -> Result<(), ApiError> {
    let ip = client_ip(headers, peer.0, st.cfg.trust_proxy);
    let window = Duration::from_secs(60);
    let n = st.cfg.login_attempts_per_minute;
    if !st.limiter.allow(&format!("ip:{ip}"), n * 6, window)
        || !st.limiter.allow(
            &format!("acct:{ip}:{}", email.trim().to_lowercase()),
            n,
            window,
        )
    {
        return Err(ApiError::too_many(
            "too many sign-in attempts; wait a minute and try again",
        ));
    }
    Ok(())
}

pub async fn register(
    State(st): State<AppState>,
    headers: HeaderMap,
    peer: Peer,
    Json(c): Json<Credentials>,
) -> Result<(axum::http::StatusCode, Json<Value>), ApiError> {
    if !st.cfg.open_registration {
        return Err(ApiError::forbidden(
            "registration is closed; ask an administrator to create your account",
        ));
    }
    throttle(&st, &headers, peer, &c.email)?;
    let auth = st.auth.clone();
    let u = tokio::task::spawn_blocking(move || auth.register(&c.email, &c.password, Role::User))
        .await
        .map_err(|_| ApiError::internal("task failed"))??;
    Ok((axum::http::StatusCode::CREATED, Json(json!({"user": u}))))
}

pub async fn login(
    State(st): State<AppState>,
    headers: HeaderMap,
    peer: Peer,
    Json(c): Json<Credentials>,
) -> Result<Json<Value>, ApiError> {
    throttle(&st, &headers, peer, &c.email)?;
    let auth = st.auth.clone();
    let (token, user) = tokio::task::spawn_blocking(move || auth.login(&c.email, &c.password))
        .await
        .map_err(|_| ApiError::internal("task failed"))?
        .map_err(|_| ApiError::unauthorized("wrong email or password"))?;
    Ok(Json(
        json!({"token": token, "user": user, "expires_in": SESSION_SECS}),
    ))
}

pub async fn refresh(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or_else(|| ApiError::unauthorized("send the session token as a Bearer token"))?;
    let t = st.auth.refresh(token).map_err(|_| {
        ApiError::unauthorized("the session is invalid or has expired; sign in again")
    })?;
    Ok(Json(json!({"token": t, "expires_in": SESSION_SECS})))
}

pub async fn me(Caller(who): Caller) -> Result<Json<Value>, ApiError> {
    match who {
        Principal::Anonymous => Err(ApiError::unauthorized("not signed in")),
        Principal::User(u) => Ok(Json(json!({"kind": "user", "user": u}))),
        Principal::Key(k) => Ok(Json(json!({"kind": "key", "key": k}))),
    }
}

#[derive(Deserialize)]
pub struct PasswordChange {
    current_password: String,
    new_password: String,
}

pub async fn password(
    State(st): State<AppState>,
    Caller(who): Caller,
    Json(p): Json<PasswordChange>,
) -> Result<Json<Value>, ApiError> {
    let Principal::User(u) = who else {
        return Err(ApiError::unauthorized(
            "sign in with an account to change its password",
        ));
    };
    let auth = st.auth.clone();
    let id = u.id.clone();
    tokio::task::spawn_blocking(move || -> Result<(), ApiError> {
        auth.login(&u.email, &p.current_password)
            .map_err(|_| ApiError::forbidden("the current password is wrong"))?;
        auth.change_password(&id, &p.new_password)?;
        Ok(())
    })
    .await
    .map_err(|_| ApiError::internal("task failed"))??;
    Ok(Json(
        json!({"ok": true, "note": "every earlier session has been signed out"}),
    ))
}
