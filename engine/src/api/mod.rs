//! The HTTP API. Every request is authenticated into a `Principal`, then every action on a collection goes through `authorize`, which applies, in order: the API key's
//! scope, administrator bypass, and the collection's rule for that action. A collection with no rule for an action is locked for everyone but administrators.

mod admin;
mod authn;
mod error;
mod files;
pub mod oauth;
mod objects;
mod query;
mod records;
mod ui;
mod upload;

use crate::auth::{Action, Auth, Principal};
use crate::model::{CollectionDef, Record};
use crate::security::expr::{self, Ctx};
use crate::security::signed_url::UrlSigner;
use crate::storage::Db;
use axum::Router;
use axum::extract::{DefaultBodyLimit, FromRequestParts};
use axum::http::{HeaderValue, Method, header, request::Parts};
use axum::routing::{get, post};
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
    /// Where online backups are written; relative paths are relative to the data directory. Default: `<data>/backups`.
    pub backup_dir: Option<std::path::PathBuf>,
    /// A request body that stalls this long between pieces is cut off (slow-body clients cannot hold connections for ever).
    pub body_idle_timeout: std::time::Duration,
    /// How long a JSON API request may take to be answered (uploads and the streaming of downloads are exempt).
    pub request_timeout: std::time::Duration,
    /// Send `Strict-Transport-Security`: set when the server itself terminates TLS.
    pub hsts: bool,
    /// Sign in with Google, GitHub or another OAuth2 provider. Empty: not offered.
    pub oauth: oauth::OAuthConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            open_registration: false,
            trust_proxy: false,
            max_upload_bytes: 5 << 30,
            url_ttl_secs: crate::security::signed_url::DEFAULT_TTL_SECS,
            cors_origins: vec![],
            login_attempts_per_minute: 10,
            backup_dir: None,
            body_idle_timeout: std::time::Duration::from_secs(30),
            request_timeout: std::time::Duration::from_secs(60),
            hsts: false,
            oauth: Default::default(),
        }
    }
}

/// A fixed-window counter per key, for throttling sign-in attempts.
pub struct Limiter {
    map: Mutex<HashMap<String, (Instant, u32)>>,
}

impl Limiter {
    pub fn new() -> Self {
        Self {
            map: Mutex::new(HashMap::new()),
        }
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
    pub oauth: Arc<oauth::OAuthState>,
}

/// The peer address of the connection, when the server was started with connection info (tests that call the router directly have none).
pub struct Peer(pub Option<std::net::SocketAddr>);

impl<S: Send + Sync> FromRequestParts<S> for Peer {
    type Rejection = std::convert::Infallible;
    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(Peer(
            parts
                .extensions
                .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
                .map(|c| c.0),
        ))
    }
}

/// The authenticated caller of a request.
pub struct Caller(pub Principal);

impl FromRequestParts<AppState> for Caller {
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let bearer = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| {
                v.strip_prefix("Bearer ")
                    .or_else(|| v.strip_prefix("bearer "))
            })
            .map(str::trim);
        let auth = state.auth.clone();
        let b = bearer.map(str::to_string);
        let p = tokio::task::spawn_blocking(move || auth.authenticate(b.as_deref()))
            .await
            .map_err(|_| ApiError::internal("authentication task failed"))?;
        match p {
            Ok(p) => Ok(Caller(p)),
            Err(_) => Err(ApiError::unauthorized(
                "the API key or session is invalid or has expired",
            )),
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
    pub fn allows(
        &self,
        who: &Principal,
        a: Action,
        record: Option<&Record>,
        body: Option<&Value>,
    ) -> bool {
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
                e.matches(&Ctx {
                    record,
                    auth: auth.as_ref(),
                    body,
                    method,
                })
            }
        }
    }
}

fn forbidden(who: &Principal, why: String) -> ApiError {
    if matches!(who, Principal::Anonymous) {
        ApiError::unauthorized(&why)
    } else {
        ApiError::forbidden(&why)
    }
}

/// Applies, in order, the API key's scope, the administrator bypass, and the collection's rule for the action. An action with no rule is locked for everyone but administrators;
/// an empty rule is the explicit way to make it public. Fails fast when no record or body could possibly satisfy it.
pub fn gate(who: &Principal, def: &CollectionDef, a: Action) -> Result<Gate, ApiError> {
    if !who.scope_allows(&def.name, a) {
        return Err(ApiError::forbidden(
            "this API key is not allowed to do that on this collection",
        ));
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
pub fn authorize(
    who: &Principal,
    def: &CollectionDef,
    a: Action,
    record: Option<&Record>,
    body: Option<&Value>,
) -> Result<(), ApiError> {
    if gate(who, def, a)?.allows(who, a, record, body) {
        Ok(())
    } else {
        Err(forbidden(
            who,
            format!(
                "the rule for {} on \"{}\" does not allow this",
                format!("{a:?}").to_lowercase(),
                def.name
            ),
        ))
    }
}

/// A handler that panics fails that one request, with a 500 that says nothing about why; the server and every other
/// connection carry on. (The panic itself is logged with its message.)
pub fn panic_response(err: Box<dyn std::any::Any + Send + 'static>) -> axum::response::Response {
    let what = err
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| err.downcast_ref::<&str>().copied())
        .unwrap_or("(no message)");
    tracing::error!("a request handler panicked: {what}");
    axum::response::Response::builder()
        .status(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
        .header(header::CONTENT_TYPE, "application/json")
        .body(axum::body::Body::from(
            r#"{"error":{"code":"internal","message":"internal error"}}"#,
        ))
        .unwrap_or_default()
}

/// Nothing this server sends may be framed by another site, except the dashboard page for the desktop app. The dashboard
/// is the one response that carries its own Content-Security-Policy (with a `frame-ancestors` list), so every other
/// response gets `X-Frame-Options: DENY`.
async fn frame_policy(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let mut res = next.run(req).await;
    if !res.headers().contains_key(header::CONTENT_SECURITY_POLICY) {
        res.headers_mut().insert(
            header::HeaderName::from_static("x-frame-options"),
            HeaderValue::from_static("DENY"),
        );
    }
    res
}

pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/auth/register", post(authn::register))
        .route("/auth/login", post(authn::login))
        .route("/auth/refresh", post(authn::refresh))
        .route("/auth/me", get(authn::me))
        .route("/auth/password", post(authn::password))
        .route("/auth/oauth/providers", get(oauth::providers))
        .route("/auth/oauth/{provider}/start", get(oauth::start))
        .route("/auth/oauth/{provider}/callback", get(oauth::callback))
        .route(
            "/collections",
            get(admin::list_collections).post(admin::create_collection),
        )
        .route(
            "/collections/{name}",
            get(admin::get_collection)
                .patch(admin::update_collection)
                .delete(admin::drop_collection),
        )
        .route("/collections/{name}/records", get(records::list))
        .route(
            "/collections/{name}/records/{id}",
            get(records::get).delete(records::delete),
        )
        .route("/files/{collection}/{id}/{field}", get(files::download))
        .route(
            "/buckets",
            get(objects::list_buckets).post(objects::create_bucket),
        )
        .route(
            "/buckets/{name}",
            get(objects::get_bucket)
                .patch(objects::update_bucket)
                .delete(objects::delete_bucket),
        )
        .route("/buckets/{name}/objects", get(objects::list_objects))
        .route("/buckets/{name}/versions", get(objects::list_versions))
        .route("/buckets/{name}/restore", post(objects::restore_version))
        .route(
            "/buckets/{name}/objects/{*key}",
            get(objects::get_object).delete(objects::delete_object),
        )
        .route("/keys", get(admin::list_keys).post(admin::create_key))
        .route("/keys/{id}", axum::routing::delete(admin::revoke_key))
        .route("/users", get(admin::list_users).post(admin::create_user))
        .route("/users/{id}", axum::routing::delete(admin::delete_user))
        .route("/stats", get(admin::stats))
        .route("/metrics", get(admin::metrics))
        .route("/maintenance/gc", post(admin::gc))
        .route(
            "/maintenance/backups",
            get(admin::list_backups).post(admin::backup),
        )
        // JSON bodies are small; uploads stream and enforce their own limits per field
        .layer(DefaultBodyLimit::max(1 << 20))
        // an answer must start within `request_timeout` (this does not limit how long a download streams)
        .layer(tower_http::timeout::TimeoutLayer::with_status_code(
            axum::http::StatusCode::REQUEST_TIMEOUT,
            state.cfg.request_timeout,
        ))
        // API answers hold accounts, tokens and records: nothing may keep a copy (downloads set their own caching)
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        ));
    let upload_routes = Router::new()
        .route("/collections/{name}/records", post(records::create))
        .route(
            "/collections/{name}/records/{id}",
            axum::routing::patch(records::update),
        )
        // an object is the raw request body, streamed: no size cap here, the upload limit is enforced as bytes arrive
        .route(
            "/buckets/{name}/objects/{*key}",
            axum::routing::put(objects::put_object),
        )
        .layer(DefaultBodyLimit::disable());

    let mut app = Router::new()
        .nest("/api/v1", api.merge(upload_routes))
        .fallback(ui::serve)
        .with_state(state.clone());
    app = app
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        ))
        .layer(axum::middleware::from_fn(frame_policy))
        .layer(tower_http::timeout::RequestBodyTimeoutLayer::new(
            state.cfg.body_idle_timeout,
        ))
        .layer(tower_http::catch_panic::CatchPanicLayer::custom(
            panic_response as fn(_) -> _,
        ));
    if state.cfg.hsts {
        app = app.layer(SetResponseHeaderLayer::if_not_present(
            header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static("max-age=31536000"),
        ));
    }
    if !state.cfg.cors_origins.is_empty() {
        let origins: Vec<HeaderValue> = state
            .cfg
            .cors_origins
            .iter()
            .filter_map(|o| HeaderValue::from_str(o).ok())
            .collect();
        app = app.layer(
            CorsLayer::new()
                .allow_origin(origins)
                .allow_methods([Method::GET, Method::POST, Method::PATCH, Method::DELETE])
                .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
                .max_age(Duration::from_secs(600)),
        );
    }
    app
}

pub fn client_ip(
    headers: &axum::http::HeaderMap,
    peer: Option<std::net::SocketAddr>,
    trust_proxy: bool,
) -> String {
    if trust_proxy
        && let Some(v) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok())
        && let Some(first) = v.split(',').next()
    {
        return first.trim().to_string();
    }
    peer.map(|p| p.ip().to_string())
        .unwrap_or_else(|| "unknown".into())
}
