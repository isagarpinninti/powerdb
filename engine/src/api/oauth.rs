//! Sign in with an identity provider (Google, GitHub, or any OAuth2 provider you describe): the authorization-code flow
//! with PKCE, run entirely on the server.
//!
//! * `GET /auth/oauth/providers` lists what is configured.
//! * `GET /auth/oauth/{provider}/start?redirect=URL` sends the browser to the provider. `redirect` must be one of the
//!   URLs listed in the settings file, character for character: this endpoint can never be used as an open redirect.
//! * `GET /auth/oauth/{provider}/callback` is where the provider sends the browser back. The `state` is single use, tied
//!   to the provider, and expires after ten minutes; the code is exchanged with the PKCE verifier and the client secret;
//!   the provider must vouch for the email address (an unverified one is refused). The account is the one with that
//!   email. A missing account is created only when open registration is on. Administrator accounts cannot sign in this
//!   way unless `allow_admin` is set, because it would make the provider a way into the whole database.
//! * The browser is sent to the chosen redirect with the session token in the URL *fragment* (`#token=...`), which no
//!   server or proxy ever receives or logs, or with `#error=<code>` when sign-in did not happen.

use super::{ApiError, AppState, Peer, client_ip};
use crate::auth::Role;
use crate::util::random_bytes;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect, Response};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const STATE_TTL: Duration = Duration::from_secs(600);
const MAX_PENDING: usize = 2000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Google,
    GitHub,
    Generic,
}

#[derive(Clone, Debug)]
pub struct Provider {
    pub kind: Kind,
    pub client_id: String,
    pub client_secret: String,
    pub auth_url: String,
    pub token_url: String,
    pub userinfo_url: String,
    /// GitHub only: the list of the user's emails, because `/user` may hide the primary one.
    pub emails_url: Option<String>,
    pub scopes: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct OAuthConfig {
    /// The address browsers use to reach this server, for example `https://db.example.com`.
    pub public_url: String,
    /// Where a finished sign-in may send the browser (exact matches only).
    pub redirects: Vec<String>,
    pub allow_admin: bool,
    pub providers: BTreeMap<String, Provider>,
}

/// A sign-in in progress: what the callback needs to finish it.
struct Pending {
    provider: String,
    redirect: String,
    verifier: String,
    expires: Instant,
}

#[derive(Default)]
pub struct OAuthState {
    pending: Mutex<HashMap<String, Pending>>,
}

fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// Everything except RFC 3986's unreserved characters is percent-encoded.
const QUERY: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

fn enc(s: &str) -> String {
    percent_encoding::utf8_percent_encode(s, QUERY).to_string()
}

/// Appends parameters to a URL. The names are fixed ASCII words; only the values need encoding.
fn with_query(base: &str, params: &[(&str, &str)]) -> String {
    let q: Vec<String> = params
        .iter()
        .map(|(k, v)| format!("{k}={}", enc(v)))
        .collect();
    format!(
        "{base}{}{}",
        if base.contains('?') { '&' } else { '?' },
        q.join("&")
    )
}

fn callback_url(cfg: &OAuthConfig, provider: &str) -> String {
    format!(
        "{}/api/v1/auth/oauth/{provider}/callback",
        cfg.public_url.trim_end_matches('/')
    )
}

pub async fn providers(State(st): State<AppState>) -> Json<Value> {
    Json(json!({ "providers": st.cfg.oauth.providers.keys().collect::<Vec<_>>() }))
}

/// One client address may begin or finish only so many sign-ins a minute, so it cannot fill the table of sign-ins in
/// progress (and lock everyone else out) or hammer the provider through this server.
fn throttle(st: &AppState, headers: &HeaderMap, peer: Peer) -> Result<(), ApiError> {
    let ip = client_ip(headers, peer.0, st.cfg.trust_proxy);
    if st.limiter.allow(
        &format!("oauth:{ip}"),
        st.cfg.login_attempts_per_minute * 2,
        Duration::from_secs(60),
    ) {
        Ok(())
    } else {
        Err(ApiError::too_many(
            "too many sign-in attempts; wait a minute and try again",
        ))
    }
}

pub async fn start(
    State(st): State<AppState>,
    headers: HeaderMap,
    peer: Peer,
    Path(name): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, ApiError> {
    throttle(&st, &headers, peer)?;
    let cfg = &st.cfg.oauth;
    let p = cfg
        .providers
        .get(&name)
        .ok_or_else(|| ApiError::not_found("no such sign-in provider"))?;
    let redirect = match q.get("redirect") {
        Some(r) if cfg.redirects.iter().any(|a| a == r) => r.clone(),
        Some(_) => {
            return Err(ApiError::bad_request(
                "that redirect is not allowed; list it under oauth.redirects in the settings file",
            ));
        }
        None => cfg.redirects.first().cloned().ok_or_else(|| {
            ApiError::bad_request(
                "give a redirect, and list the allowed ones under oauth.redirects",
            )
        })?,
    };
    let state = crate::util::hex(&random_bytes::<32>());
    let verifier = URL_SAFE_NO_PAD.encode(random_bytes::<32>());
    {
        let mut m = st.oauth.pending.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        m.retain(|_, v| v.expires > now);
        if m.len() >= MAX_PENDING {
            return Err(ApiError::too_many(
                "too many sign-ins in progress; try again in a minute",
            ));
        }
        m.insert(
            state.clone(),
            Pending {
                provider: name.clone(),
                redirect,
                verifier: verifier.clone(),
                expires: now + STATE_TTL,
            },
        );
    }
    let url = with_query(
        &p.auth_url,
        &[
            ("response_type", "code"),
            ("client_id", &p.client_id),
            ("redirect_uri", &callback_url(cfg, &name)),
            ("scope", &p.scopes.join(" ")),
            ("state", &state),
            ("code_challenge", &pkce_challenge(&verifier)),
            ("code_challenge_method", "S256"),
        ],
    );
    Ok(Redirect::to(&url).into_response())
}

/// What the provider vouches for.
struct Identity {
    email: String,
    verified: bool,
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .http_status_as_error(false)
        .user_agent("PowerDB")
        .build()
        .into()
}

fn exchange(
    p: &Provider,
    redirect_uri: &str,
    code: &str,
    verifier: &str,
) -> Result<String, String> {
    let mut res = agent()
        .post(&p.token_url)
        .header("Accept", "application/json")
        .send_form([
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", &p.client_id),
            ("client_secret", &p.client_secret),
            ("code_verifier", verifier),
        ])
        .map_err(|e| format!("token request failed: {e}"))?;
    if !res.status().is_success() {
        return Err(format!("the provider refused the code ({})", res.status()));
    }
    let v: Value = res
        .body_mut()
        .read_json()
        .map_err(|e| format!("unreadable token answer: {e}"))?;
    v["access_token"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| "the provider sent no access token".to_string())
}

fn get_json(url: &str, token: &str) -> Result<Value, String> {
    let mut res = agent()
        .get(url)
        .header("Authorization", &format!("Bearer {token}"))
        .header("Accept", "application/json")
        .call()
        .map_err(|e| format!("request failed: {e}"))?;
    if !res.status().is_success() {
        return Err(format!("the provider answered {}", res.status()));
    }
    res.body_mut()
        .read_json()
        .map_err(|e| format!("unreadable answer: {e}"))
}

fn identity(p: &Provider, token: &str) -> Result<Identity, String> {
    match p.kind {
        Kind::GitHub => {
            // /user may show no email at all; the emails list says which is primary and verified
            let list = get_json(
                p.emails_url.as_deref().ok_or("GitHub needs emails_url")?,
                token,
            )?;
            let primary = list
                .as_array()
                .and_then(|a| a.iter().find(|e| e["primary"] == true))
                .ok_or("GitHub reported no primary email")?;
            Ok(Identity {
                email: primary["email"].as_str().unwrap_or_default().to_lowercase(),
                verified: primary["verified"] == true,
            })
        }
        Kind::Google | Kind::Generic => {
            let v = get_json(&p.userinfo_url, token)?;
            Ok(Identity {
                email: v["email"].as_str().unwrap_or_default().to_lowercase(),
                verified: v["email_verified"] == true,
            })
        }
    }
}

fn finish(redirect: &str, fragment: String) -> Response {
    Redirect::to(&format!("{redirect}#{fragment}")).into_response()
}

pub async fn callback(
    State(st): State<AppState>,
    headers: HeaderMap,
    peer: Peer,
    Path(name): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, ApiError> {
    throttle(&st, &headers, peer)?;
    // The state decides everything. Without a valid one there is nowhere safe to send the browser, so it is a plain error.
    let pending = {
        let mut m = st.oauth.pending.lock().unwrap_or_else(|e| e.into_inner());
        q.get("state").and_then(|s| m.remove(s))
    };
    let pending = match pending {
        Some(p) if p.provider == name && p.expires > Instant::now() => p,
        _ => {
            return Err(ApiError::bad_request(
                "this sign-in link is invalid or has expired; start again",
            ));
        }
    };
    let fail = |code: &str| Ok(finish(&pending.redirect, format!("error={code}")));
    if q.contains_key("error") {
        return fail("access_denied");
    }
    let Some(code) = q.get("code").cloned() else {
        return fail("no_code");
    };
    let cfg = st.cfg.oauth.clone();
    let Some(p) = cfg.providers.get(&name).cloned() else {
        return fail("unknown_provider");
    };
    let (redirect_uri, verifier) = (callback_url(&cfg, &name), pending.verifier.clone());

    let ident = tokio::task::spawn_blocking(move || {
        let token = exchange(&p, &redirect_uri, &code, &verifier)?;
        identity(&p, &token)
    })
    .await
    .map_err(|_| ApiError::internal("task failed"))?;
    let ident = match ident {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!("oauth sign-in with {name} failed: {e}");
            return fail("exchange_failed");
        }
    };
    if ident.email.is_empty() || !ident.verified {
        return fail("unverified_email");
    }

    let auth = st.auth.clone();
    let (email, create, allow_admin) = (ident.email, st.cfg.open_registration, cfg.allow_admin);
    let signed_in = tokio::task::spawn_blocking(move || {
        if !allow_admin
            && let Some(u) = auth.users()?.into_iter().find(|u| u.email == email)
            && u.role == Role::Admin
        {
            return Err(crate::Error::Denied("admin_not_allowed".into()));
        }
        auth.login_verified(&email, create)
    })
    .await
    .map_err(|_| ApiError::internal("task failed"))?;
    match signed_in {
        Ok((token, _)) => Ok(finish(&pending.redirect, format!("token={token}"))),
        Err(crate::Error::Denied(m)) if m == "admin_not_allowed" => fail("admin_not_allowed"),
        Err(crate::Error::Denied(_)) => fail("no_account"),
        Err(e) => {
            tracing::warn!("oauth sign-in could not complete: {e}");
            fail("failed")
        }
    }
}

/// Fills in what a provider name implies (`google`, `github`) and checks the rest is there.
pub fn complete(name: &str, mut p: Provider) -> Result<Provider, String> {
    match (p.kind, name) {
        (Kind::Google, _) => {
            if p.auth_url.is_empty() {
                p.auth_url = "https://accounts.google.com/o/oauth2/v2/auth".into();
            }
            if p.token_url.is_empty() {
                p.token_url = "https://oauth2.googleapis.com/token".into();
            }
            if p.userinfo_url.is_empty() {
                p.userinfo_url = "https://openidconnect.googleapis.com/v1/userinfo".into();
            }
            if p.scopes.is_empty() {
                p.scopes = vec!["openid".into(), "email".into()];
            }
        }
        (Kind::GitHub, _) => {
            if p.auth_url.is_empty() {
                p.auth_url = "https://github.com/login/oauth/authorize".into();
            }
            if p.token_url.is_empty() {
                p.token_url = "https://github.com/login/oauth/access_token".into();
            }
            if p.userinfo_url.is_empty() {
                p.userinfo_url = "https://api.github.com/user".into();
            }
            if p.emails_url.is_none() {
                p.emails_url = Some("https://api.github.com/user/emails".into());
            }
            if p.scopes.is_empty() {
                p.scopes = vec!["read:user".into(), "user:email".into()];
            }
        }
        (Kind::Generic, _) => {
            if p.auth_url.is_empty() || p.token_url.is_empty() || p.userinfo_url.is_empty() {
                return Err(format!(
                    "provider \"{name}\" needs auth_url, token_url and userinfo_url (or kind = \"google\" / \"github\")"
                ));
            }
            if p.scopes.is_empty() {
                p.scopes = vec!["openid".into(), "email".into()];
            }
        }
    }
    if p.client_id.is_empty() || p.client_secret.is_empty() {
        return Err(format!(
            "provider \"{name}\" needs client_id and client_secret (the secret can come from POWERDB_OAUTH_{}_CLIENT_SECRET)",
            name.to_uppercase()
        ));
    }
    Ok(p)
}
