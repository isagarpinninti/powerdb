//! OAuth2 sign-in against a mock identity provider: a real local server that checks the PKCE challenge, the client secret,
//! the redirect URI and the single use of a code, the way Google or GitHub would.

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use http_body_util::BodyExt;
use powerdb::api::oauth::{Kind, OAuthConfig, Provider};
use powerdb::api::{self, AppState, Config};
use powerdb::auth::Role;
use powerdb::server;
use powerdb::util::new_id;
use powerdb::{Durability, Options};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

const PUBLIC: &str = "https://db.test";
const DONE: &str = "https://app.test/done";

// ---- the mock provider ----

#[derive(Clone)]
struct Grant {
    challenge: String,
    redirect_uri: String,
    email: String,
    verified: bool,
    /// the provider refuses to exchange this code
    refuse: bool,
}

#[derive(Default)]
struct Idp {
    grants: Mutex<HashMap<String, Grant>>,
    used: Mutex<HashMap<String, Grant>>,
}

async fn token(
    State(idp): State<Arc<Idp>>,
    Form(f): Form<HashMap<String, String>>,
) -> (StatusCode, Json<Value>) {
    let bad = |why: &str| (StatusCode::BAD_REQUEST, Json(json!({"error": why})));
    let code = f.get("code").cloned().unwrap_or_default();
    let Some(g) = idp.grants.lock().unwrap().remove(&code) else {
        return bad("invalid_grant");
    }; // a code works once
    if g.refuse {
        return bad("access_denied");
    }
    if f.get("grant_type").map(String::as_str) != Some("authorization_code")
        || f.get("client_id").map(String::as_str) != Some("cid")
        || f.get("client_secret").map(String::as_str) != Some("sec")
    {
        return bad("invalid_client");
    }
    if f.get("redirect_uri") != Some(&g.redirect_uri) {
        return bad("redirect_uri_mismatch");
    }
    let verifier = f.get("code_verifier").cloned().unwrap_or_default();
    if URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())) != g.challenge {
        return bad("pkce_failed");
    }
    idp.used.lock().unwrap().insert(code.clone(), g);
    (
        StatusCode::OK,
        Json(json!({"access_token": format!("tok-{code}"), "token_type": "bearer"})),
    )
}

async fn userinfo(State(idp): State<Arc<Idp>>, h: HeaderMap) -> (StatusCode, Json<Value>) {
    let t = h
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer tok-"))
        .unwrap_or("");
    match idp.used.lock().unwrap().get(t) {
        Some(g) => (
            StatusCode::OK,
            Json(json!({"email": g.email, "email_verified": g.verified})),
        ),
        None => (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "bad token"})),
        ),
    }
}

async fn start_idp() -> (String, Arc<Idp>) {
    let idp = Arc::new(Idp::default());
    let app = Router::new()
        .route("/token", post(token))
        .route("/userinfo", get(userinfo))
        .with_state(idp.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (base, idp)
}

// ---- the server under test ----

struct App {
    _dir: std::path::PathBuf,
    router: Router,
    st: AppState,
    idp: Arc<Idp>,
    base: String,
}
impl Drop for App {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self._dir);
    }
}

async fn app(open_registration: bool, allow_admin: bool) -> App {
    let (base, idp) = start_idp().await;
    let provider = |name: &str| Provider {
        kind: Kind::Generic,
        client_id: "cid".into(),
        client_secret: "sec".into(),
        auth_url: format!("{base}/authorize"),
        token_url: format!("{base}/token"),
        userinfo_url: format!("{base}/userinfo"),
        emails_url: None,
        scopes: vec!["openid".into(), "email".into(), name.into()],
    };
    let mut providers = std::collections::BTreeMap::new();
    providers.insert("mock".to_string(), provider("mock"));
    providers.insert("other".to_string(), provider("other"));
    let cfg = Config {
        open_registration,
        oauth: OAuthConfig {
            public_url: PUBLIC.into(),
            redirects: vec![DONE.into(), "https://app.test/other".into()],
            allow_admin,
            providers,
        },
        ..Config::default()
    };
    let dir = std::env::temp_dir().join(format!("powerdb-oauth-{}", new_id()));
    let opts = Options {
        durability: Durability::Never,
        ..Options::default()
    };
    let (st, _) = server::build_state(&dir, opts, cfg).unwrap();
    App {
        router: api::router(st.clone()),
        st,
        idp,
        base,
        _dir: dir,
    }
}

struct Res {
    status: StatusCode,
    location: Option<String>,
    body: Value,
}
async fn get_req(a: &App, uri: &str) -> Res {
    let r = a
        .router
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let (p, body) = r.into_parts();
    let bytes = body.collect().await.unwrap().to_bytes();
    Res {
        status: p.status,
        location: p
            .headers
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .map(String::from),
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    }
}

fn query_of(url: &str) -> HashMap<String, String> {
    let q = url.split_once('?').map(|x| x.1).unwrap_or("");
    q.split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| {
            (
                k.to_string(),
                percent_encoding::percent_decode_str(v)
                    .decode_utf8_lossy()
                    .into_owned(),
            )
        })
        .collect()
}

/// Starts a sign-in as the browser would, lets "the user approve" at the provider, and returns where the callback sends the browser.
async fn sign_in(
    a: &App,
    provider: &str,
    email: &str,
    verified: bool,
    refuse: bool,
) -> (String, Res) {
    let start = get_req(
        a,
        &format!(
            "/api/v1/auth/oauth/{provider}/start?redirect={}",
            percent_encoding::utf8_percent_encode(DONE, percent_encoding::NON_ALPHANUMERIC)
        ),
    )
    .await;
    assert_eq!(start.status, StatusCode::SEE_OTHER, "{:?}", start.body);
    let to = start.location.unwrap();
    assert!(to.starts_with(&format!("{}/authorize?", a.base)), "{to}");
    let q = query_of(&to);
    assert_eq!(q["response_type"], "code");
    assert_eq!(q["client_id"], "cid");
    assert_eq!(q["code_challenge_method"], "S256");
    assert_eq!(
        q["redirect_uri"],
        format!("{PUBLIC}/api/v1/auth/oauth/{provider}/callback")
    );
    assert!(!q["state"].is_empty() && q["state"].len() >= 32);
    let code = format!("code-{}", new_id());
    a.idp.grants.lock().unwrap().insert(
        code.clone(),
        Grant {
            challenge: q["code_challenge"].clone(),
            redirect_uri: q["redirect_uri"].clone(),
            email: email.into(),
            verified,
            refuse,
        },
    );
    let back = get_req(
        a,
        &format!(
            "/api/v1/auth/oauth/{provider}/callback?code={code}&state={}",
            q["state"]
        ),
    )
    .await;
    (q["state"].clone(), back)
}

fn fragment(r: &Res) -> HashMap<String, String> {
    let loc = r.location.as_deref().expect("a redirect");
    assert!(
        loc.starts_with(&format!("{DONE}#")),
        "the browser goes only to a configured redirect, with the result in the fragment: {loc}"
    );
    loc.split_once('#')
        .unwrap()
        .1
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.into(), v.into()))
        .collect()
}

fn user(a: &App, email: &str, role: Role) {
    a.st.auth
        .register(email, "correct horse battery", role)
        .unwrap();
}

// ---- tests ----

#[tokio::test(flavor = "multi_thread")]
async fn a_verified_email_signs_in_to_its_account_and_the_token_works() {
    let a = app(false, false).await;
    user(&a, "alice@example.com", Role::User);
    let (_, back) = sign_in(&a, "mock", "Alice@Example.com", true, false).await;
    let f = fragment(&back);
    let principal =
        a.st.auth
            .verify_token(&f["token"])
            .expect("a real session token");
    assert_eq!(
        principal.auth_value().unwrap()["id"],
        a.st.auth
            .users()
            .unwrap()
            .iter()
            .find(|u| u.email == "alice@example.com")
            .unwrap()
            .id
    );
    assert!(
        !back.location.as_deref().unwrap().contains('?'),
        "no token in the query string, where it would be logged"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn only_configured_redirects_are_followed_so_it_is_never_an_open_redirect() {
    let a = app(false, false).await;
    for evil in [
        "https://evil.test/",
        "https://app.test/done.evil.test",
        "https://app.test/done/",
        "http://app.test/done",
        "//evil.test",
        "javascript:alert(1)",
        "",
    ] {
        let r = get_req(
            &a,
            &format!(
                "/api/v1/auth/oauth/mock/start?redirect={}",
                percent_encoding::utf8_percent_encode(evil, percent_encoding::NON_ALPHANUMERIC)
            ),
        )
        .await;
        assert_eq!(
            r.status,
            StatusCode::BAD_REQUEST,
            "{evil:?} must be refused"
        );
        assert!(r.location.is_none());
    }
    // with none given, the first configured one is used
    let r = get_req(&a, "/api/v1/auth/oauth/mock/start").await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(
        get_req(&a, "/api/v1/auth/oauth/nope/start").await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_state_works_once_only_and_only_for_its_own_provider() {
    let a = app(false, false).await;
    user(&a, "alice@example.com", Role::User);
    let (state, first) = sign_in(&a, "mock", "alice@example.com", true, false).await;
    assert!(fragment(&first).contains_key("token"));
    // replaying the same callback (a leaked or logged URL) does nothing
    let replay = get_req(
        &a,
        &format!("/api/v1/auth/oauth/mock/callback?code=whatever&state={state}"),
    )
    .await;
    assert_eq!(replay.status, StatusCode::BAD_REQUEST);
    assert!(replay.location.is_none());
    // a made-up state
    assert_eq!(
        get_req(&a, "/api/v1/auth/oauth/mock/callback?code=x&state=nonsense")
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        get_req(&a, "/api/v1/auth/oauth/mock/callback?code=x")
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
    // a state begun with one provider cannot be finished through another
    let start = get_req(&a, "/api/v1/auth/oauth/mock/start").await;
    let state = query_of(&start.location.unwrap())["state"].clone();
    assert_eq!(
        get_req(
            &a,
            &format!("/api/v1/auth/oauth/other/callback?code=x&state={state}")
        )
        .await
        .status,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_email_the_provider_does_not_vouch_for_never_signs_anyone_in() {
    let a = app(true, false).await;
    user(&a, "alice@example.com", Role::User);
    let (_, back) = sign_in(&a, "mock", "alice@example.com", false, false).await;
    let f = fragment(&back);
    assert_eq!(f.get("error").map(String::as_str), Some("unverified_email"));
    assert!(!f.contains_key("token"));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_email_gets_an_account_only_when_registration_is_open() {
    let closed = app(false, false).await;
    let (_, back) = sign_in(&closed, "mock", "new@example.com", true, false).await;
    assert_eq!(
        fragment(&back).get("error").map(String::as_str),
        Some("no_account")
    );
    assert!(
        closed
            .st
            .auth
            .users()
            .unwrap()
            .iter()
            .all(|u| u.email != "new@example.com")
    );

    let open = app(true, false).await;
    let (_, back) = sign_in(&open, "mock", "new@example.com", true, false).await;
    let f = fragment(&back);
    open.st.auth.verify_token(&f["token"]).unwrap();
    let made = open
        .st
        .auth
        .users()
        .unwrap()
        .into_iter()
        .find(|u| u.email == "new@example.com")
        .unwrap();
    assert_eq!(
        made.role,
        Role::User,
        "a provider never creates an administrator"
    );
    assert!(
        open.st.auth.login("new@example.com", "").is_err()
            && open.st.auth.login("new@example.com", "password").is_err(),
        "the account has no password anyone knows"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn administrators_cannot_sign_in_through_a_provider_unless_allowed() {
    let denied = app(false, false).await;
    user(&denied, "root@example.com", Role::Admin);
    let (_, back) = sign_in(&denied, "mock", "root@example.com", true, false).await;
    assert_eq!(
        fragment(&back).get("error").map(String::as_str),
        Some("admin_not_allowed")
    );

    let allowed = app(false, true).await;
    user(&allowed, "root@example.com", Role::Admin);
    let (_, back) = sign_in(&allowed, "mock", "root@example.com", true, false).await;
    assert!(fragment(&back).contains_key("token"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_provider_error_or_a_refused_code_ends_cleanly_without_a_session() {
    let a = app(false, false).await;
    user(&a, "alice@example.com", Role::User);
    // the user clicks "cancel" at the provider
    let start = get_req(&a, "/api/v1/auth/oauth/mock/start").await;
    let state = query_of(&start.location.unwrap())["state"].clone();
    let cancelled = get_req(
        &a,
        &format!("/api/v1/auth/oauth/mock/callback?error=access_denied&state={state}"),
    )
    .await;
    assert_eq!(
        fragment(&cancelled).get("error").map(String::as_str),
        Some("access_denied")
    );
    // the provider refuses to exchange the code
    let (_, back) = sign_in(&a, "mock", "alice@example.com", true, true).await;
    let f = fragment(&back);
    assert_eq!(f.get("error").map(String::as_str), Some("exchange_failed"));
    assert!(!f.contains_key("token"));
    // a code the provider never issued
    let start = get_req(&a, "/api/v1/auth/oauth/mock/start").await;
    let state = query_of(&start.location.unwrap())["state"].clone();
    let forged = get_req(
        &a,
        &format!("/api/v1/auth/oauth/mock/callback?code=forged&state={state}"),
    )
    .await;
    assert_eq!(
        fragment(&forged).get("error").map(String::as_str),
        Some("exchange_failed")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn providers_are_listed_for_a_sign_in_page_without_exposing_secrets() {
    let a = app(false, false).await;
    let r = get_req(&a, "/api/v1/auth/oauth/providers").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        r.body,
        json!({"providers": ["mock", "other"]}),
        "names only: no ids, secrets or URLs"
    );
}
