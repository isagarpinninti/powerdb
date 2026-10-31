//! The HTTP API. Every request is authenticated into a `Principal`, then every action on a collection goes through `authorize`, which applies, in order: the API key's
//! scope, administrator bypass, and the collection's rule for that action. A collection with no rule for an action is locked for everyone but administrators.

mod admin;
mod authn;
mod error;
mod files;
mod query;
mod records;
mod ui;
mod upload;

use crate::auth::{Action, Auth, Principal};
use crate::model::{CollectionDef, Record};
use crate::security::expr::{self, Ctx};
use crate::security::signed_url::UrlSigner;
use crate::storage::Db;
use axum::extract::{DefaultBodyLimit, FromRequestParts};
use axum::http::{header, request::Parts, HeaderValue, Method};
use axum::routing::{get, post};
use axum::Router;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tower_http::cors::CorsLayer;
use tower_http::set_header::SetResponseHeaderLayer;

pub use error::ApiError;

#[derive(Clone, Debug)]
pub struct Config {
    /// Let anyone create an account. Off by default: administrators create users.
    pub open_registration: bool,
    /// Read the client address from `X-Forwarded-For` (only behind a proxy you control) for rate limiting.
    pub trust_proxy: bool,
    pub max_upload_bytes: u64,
    pub url_ttl_secs: i64,
    pub cors_origins: Vec<String>,
    pub login_attempts_per_minute: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self { open_registration: false, trust_proxy: false, max_upload_bytes: 5 << 30, url_ttl_secs: crate::security::signed_url::DEFAULT_TTL_SECS, cors_origins: vec![], login_attempts_per_minute: 10 }
    }
}

/// A fixed-window counter per key, for throttling sign-in attempts.
pub struct Limiter {
    map: Mutex<HashMap<String, (Instant, u32)>>,
}

impl Limiter {
    pub fn new() -> Self {
        Self { map: Mutex::new(HashMap::new()) }
    }
    /// Returns false once `key` has been hit `max` times within `window`.
    pub fn allow(&self, key: &str, max: u32, window: Duration) -> bool {
        let mut m = self.map.lock().unwrap_or_else(|e| e.into_inner());
        if m.len() > 50_000 {
            m.retain(|_, (t, _)| t.elapsed() < window);
        }
        let e = m.entry(key.to_string()).or_insert((Instant::now(), 0));
        if e.0.elapsed() >= window {
            *e = (Instant::now(), 0);
        }
        e.1 += 1;
        e.1 <= max
    }
}

impl Default for Limiter {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub auth: Auth,
    pub signer: UrlSigner,
    pub cfg: Arc<Config>,
    pub limiter: Arc<Limiter>,
}

/// The peer address of the connection, when the server was started with connection info (tests that call the router directly have none).
pub struct Peer(pub Option<std::net::SocketAddr>);

impl<S: Send + Sync> FromRequestParts<S> for Peer {
    type Rejection = std::convert::Infallible;
    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(Peer(parts.extensions.get::<axum::extract::ConnectInfo<std::net::SocketAddr>>().map(|c| c.0)))
    }
}

/// The authenticated caller of a request.
pub struct Caller(pub Principal);

impl FromRequestParts<AppState> for Caller {
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let bearer = parts.headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer "))).map(str::trim);
        let auth = state.auth.clone();
        let b = bearer.map(str::to_string);
        let p = tokio::task::spawn_blocking(move || auth.authenticate(b.as_deref())).await.map_err(|_| ApiError::internal("authentication task failed"))?;
        match p {
            Ok(p) => Ok(Caller(p)),
            Err(_) => Err(ApiError::unauthorized("the API key or session is invalid or has expired")),
        }
    }
}

fn rule_of(def: &CollectionDef, a: Action) -> &Option<String> {
    match a {
        Action::List => &def.rules.list,
        Action::View => &def.rules.view,
        Action::Create => &def.rules.create,
        Action::Update => &def.rules.update,
        Action::Delete => &def.rules.delete,
    }
}

/// The outcome of checking scope, administrator status and rule presence for one action. `Rule` still has to be evaluated against the record or request body.
pub enum Gate {
    Open,
    Rule(expr::Expr),
}

impl Gate {
    pub fn allows(&self, who: &Principal, a: Action, record: Option<&Record>, body: Option<&Value>) -> bool {
        match self {
            Gate::Open => true,
            Gate::Rule(e) => {
                let auth = who.auth_value();
                let method = match a {
                    Action::List | Action::View => "GET",
                    Action::Create => "POST",
                    Action::Update => "PATCH",
                    Action::Delete => "DELETE",
                };
                e.matches(&Ctx { record, auth: auth.as_ref(), body, method })
            }
        }
    }
}

fn forbidden(who: &Principal, why: String) -> ApiError {
    if matches!(who, Principal::Anonymous) { ApiError::unauthorized(&why) } else { ApiError::forbidden(&why) }
}

/// Applies, in order, the API key's scope, the administrator bypass, and the collection's rule for the action. An action with no rule is locked for everyone but administrators;
/// an empty rule is the explicit way to make it public. Fails fast when no record or body could possibly satisfy it.
pub fn gate(who: &Principal, def: &CollectionDef, a: Action) -> Result<Gate, ApiError> {
    if !who.scope_allows(&def.name, a) {
        return Err(ApiError::forbidden("this API key is not allowed to do that on this collection"));
    }
    if who.is_admin() {
        return Ok(Gate::Open);
    }
    let verb = format!("{a:?}").to_lowercase();
    match rule_of(def, a) {
        None => Err(forbidden(who, format!("collection \"{}\" is locked for {verb}: only an administrator may, until a rule is set", def.name))),
        Some(r) if r.trim().is_empty() => Ok(Gate::Open),
        Some(r) => expr::parse(r).map(Gate::Rule).map_err(|_| ApiError::internal("this collection has an access rule that cannot be parsed; an administrator must fix it")),
    }
}

/// `gate` plus the rule evaluated against `record` and `body`.
pub fn authorize(who: &Principal, def: &CollectionDef, a: Action, record: Option<&Record>, body: Option<&Value>) -> Result<(), ApiError> {
    if gate(who, def, a)?.allows(who, a, record, body) {
        Ok(())
    } else {
        Err(forbidden(who, format!("the rule for {} on \"{}\" does not allow this", format!("{a:?}").to_lowercase(), def.name)))
    }
}

pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/auth/register", post(authn::register))
        .route("/auth/login", post(authn::login))
        .route("/auth/refresh", post(authn::refresh))
        .route("/auth/me", get(authn::me))
        .route("/auth/password", post(authn::password))
        .route("/collections", get(admin::list_collections).post(admin::create_collection))
        .route("/collections/{name}", get(admin::get_collection).patch(admin::update_collection).delete(admin::drop_collection))
        .route("/collections/{name}/records", get(records::list))
        .route("/collections/{name}/records/{id}", get(records::get).delete(records::delete))
        .route("/files/{collection}/{id}/{field}", get(files::download))
        .route("/keys", get(admin::list_keys).post(admin::create_key))
        .route("/keys/{id}", axum::routing::delete(admin::revoke_key))
        .route("/users", get(admin::list_users).post(admin::create_user))
        .route("/users/{id}", axum::routing::delete(admin::delete_user))
        .route("/stats", get(admin::stats))
        .route("/maintenance/gc", post(admin::gc))
        // JSON bodies are small; uploads stream and enforce their own limits per field
        .layer(DefaultBodyLimit::max(1 << 20));
    let upload_routes = Router::new().route("/collections/{name}/records", post(records::create)).route("/collections/{name}/records/{id}", axum::routing::patch(records::update)).layer(DefaultBodyLimit::disable());

    let mut app = Router::new().nest("/api/v1", api.merge(upload_routes)).fallback(ui::serve).with_state(state.clone());
    app = app
        .layer(SetResponseHeaderLayer::if_not_present(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff")))
        .layer(SetResponseHeaderLayer::if_not_present(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer")))
        .layer(SetResponseHeaderLayer::if_not_present(header::HeaderName::from_static("x-frame-options"), HeaderValue::from_static("DENY")));
    if !state.cfg.cors_origins.is_empty() {
        let origins: Vec<HeaderValue> = state.cfg.cors_origins.iter().filter_map(|o| HeaderValue::from_str(o).ok()).collect();
        app = app.layer(CorsLayer::new().allow_origin(origins).allow_methods([Method::GET, Method::POST, Method::PATCH, Method::DELETE]).allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE]).max_age(Duration::from_secs(600)));
    }
    app
}

pub fn client_ip(headers: &axum::http::HeaderMap, peer: Option<std::net::SocketAddr>, trust_proxy: bool) -> String {
    if trust_proxy
        && let Some(v) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok())
            && let Some(first) = v.split(',').next() {
                return first.trim().to_string();
            }
    peer.map(|p| p.ip().to_string()).unwrap_or_else(|| "unknown".into())
}
