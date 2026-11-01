//! Accounts, sessions and API keys.
//!
//! * Passwords are hashed with Argon2id (memory-hard, per-password random salt). A login for an address that has no account runs the same work against a dummy
//!   hash, so response time does not reveal which addresses are registered.
//! * Sessions are HS256 JWTs. Verification accepts exactly the one header we issue (no `alg` negotiation, so `alg: none` and algorithm-confusion tokens are
//!   rejected), checks the signature in constant time, then the expiry, then looks the user up again: a deleted user or a changed password (which bumps the user's
//!   token version) invalidates every earlier token at once.
//! * API keys (`pdb_live_...`) are random 256-bit secrets shown once; only their BLAKE3 hash is stored. A key carries a scope: read, write or both, optionally limited
//!   to named collections, optionally administrative.

use crate::error::{Error, Result};
use crate::storage::Db;
use crate::util::{hex, new_id, now_ms, random_bytes};
use argon2::Argon2;
use argon2::password_hash::{PasswordHasher, PasswordVerifier, phc::PasswordHash};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use fjall::Keyspace;
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Sha256;
use std::sync::{Arc, Mutex};

type HmacSha256 = Hmac<Sha256>;
pub const SESSION_SECS: i64 = 3600;
const JWT_HEADER: &str = r#"{"alg":"HS256","typ":"JWT"}"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Admin,
    User,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct UserRow {
    id: String,
    email: String,
    password_hash: String,
    role: Role,
    token_version: u32,
    created_at: i64,
}

/// What the outside world may know about a user.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct UserInfo {
    pub id: String,
    pub email: String,
    pub role: Role,
    pub created_at: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    Read,
    Write,
    ReadWrite,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct KeyMeta {
    pub id: String,
    pub name: String,
    pub access: Access,
    /// Empty means every collection.
    #[serde(default)]
    pub collections: Vec<String>,
    /// An administrative key may manage collections, users and keys, and bypasses access rules.
    #[serde(default)]
    pub admin: bool,
    pub created_at: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    List,
    View,
    Create,
    Update,
    Delete,
}

#[derive(Clone, Debug)]
pub enum Principal {
    Anonymous,
    User(UserInfo),
    Key(KeyMeta),
}

impl Principal {
    /// The value rules see as `@request.auth`.
    pub fn auth_value(&self) -> Option<Value> {
        match self {
            Principal::Anonymous => None,
            Principal::User(u) => {
                Some(json!({"id": u.id, "email": u.email, "role": u.role, "kind": "user"}))
            }
            Principal::Key(k) => Some(json!({"id": k.id, "name": k.name, "kind": "key"})),
        }
    }
    pub fn is_admin(&self) -> bool {
        match self {
            Principal::User(u) => u.role == Role::Admin,
            Principal::Key(k) => k.admin,
            Principal::Anonymous => false,
        }
    }
    /// Whether an API key's scope allows this action on this collection. Users and anonymous callers are not limited by scope (rules decide).
    pub fn scope_allows(&self, collection: &str, a: Action) -> bool {
        match self {
            Principal::Key(k) => {
                if !k.collections.is_empty() && !k.collections.iter().any(|c| c == collection) {
                    return false;
                }
                matches!(
                    (k.access, a),
                    (Access::ReadWrite, _)
                        | (Access::Read, Action::List | Action::View)
                        | (
                            Access::Write,
                            Action::Create | Action::Update | Action::Delete
                        )
                )
            }
            _ => true,
        }
    }
}

#[derive(Clone)]
pub struct Auth {
    inner: Arc<AuthInner>,
}

struct AuthInner {
    users: Keyspace,
    emails: Keyspace,
    keys: Keyspace,
    jwt_key: [u8; 32],
    lock: Mutex<()>,
    dummy_hash: String,
}

fn valid_email(e: &str) -> bool {
    let Some((local, domain)) = e.split_once('@') else {
        return false;
    };
    e.len() <= 254
        && !local.is_empty()
        && local.len() <= 64
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !domain.contains("..")
        && !e.chars().any(|c| c.is_whitespace() || c.is_control())
        && e.matches('@').count() == 1
}

fn hash_password(pw: &str) -> Result<String> {
    Argon2::default()
        .hash_password(pw.as_bytes())
        .map(|h| h.to_string())
        .map_err(|e| Error::Store(format!("password hashing failed: {e}")))
}

impl Auth {
    pub fn new(db: &Db, jwt_key: [u8; 32]) -> Result<Self> {
        Ok(Self {
            inner: Arc::new(AuthInner {
                users: db.keyspace("auth_users")?,
                emails: db.keyspace("auth_emails")?,
                keys: db.keyspace("auth_keys")?,
                jwt_key,
                lock: Mutex::new(()),
                dummy_hash: hash_password(
                    "not a real password, only here to make unknown-account logins cost the same",
                )?,
            }),
        })
    }

    fn row(&self, id: &str) -> Result<Option<UserRow>> {
        match self.inner.users.get(id.as_bytes())? {
            Some(v) => Ok(Some(serde_json::from_slice(&v)?)),
            None => Ok(None),
        }
    }

    fn info(r: &UserRow) -> UserInfo {
        UserInfo {
            id: r.id.clone(),
            email: r.email.clone(),
            role: r.role,
            created_at: r.created_at,
        }
    }

    pub fn register(&self, email: &str, password: &str, role: Role) -> Result<UserInfo> {
        let email = email.trim().to_lowercase();
        if !valid_email(&email) {
            return Err(Error::Invalid(
                "that does not look like an email address".into(),
            ));
        }
        if password.chars().count() < 8 || password.len() > 1024 {
            return Err(Error::Invalid(
                "a password needs at least 8 characters (and at most 1024 bytes)".into(),
            ));
        }
        let password_hash = hash_password(password)?; // the slow part, outside the lock
        let _g = self.inner.lock.lock().unwrap_or_else(|e| e.into_inner());
        if self.inner.emails.contains_key(email.as_bytes())? {
            return Err(Error::Conflict(
                "an account with this email already exists".into(),
            ));
        }
        let row = UserRow {
            id: format!("usr_{}", new_id()),
            email: email.clone(),
            password_hash,
            role,
            token_version: 0,
            created_at: now_ms(),
        };
        self.inner
            .users
            .insert(row.id.as_bytes().to_vec(), serde_json::to_vec(&row)?)?;
        self.inner
            .emails
            .insert(email.into_bytes(), row.id.as_bytes().to_vec())?;
        Ok(Self::info(&row))
    }

    pub fn user_count(&self) -> Result<usize> {
        let mut n = 0;
        for g in self.inner.users.iter() {
            g.key()?;
            n += 1;
        }
        Ok(n)
    }

    pub fn login(&self, email: &str, password: &str) -> Result<(String, UserInfo)> {
        let email = email.trim().to_lowercase();
        let denied = || Error::Denied("wrong email or password".into());
        let row = match self.inner.emails.get(email.as_bytes())? {
            Some(id) => self.row(&String::from_utf8_lossy(&id))?,
            None => None,
        };
        let hash = row
            .as_ref()
            .map(|r| r.password_hash.as_str())
            .unwrap_or(&self.inner.dummy_hash);
        let parsed =
            PasswordHash::new(hash).map_err(|_| Error::Corrupt("stored password hash".into()))?;
        let ok = Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok();
        match row {
            Some(r) if ok => Ok((self.issue(&r), Self::info(&r))),
            _ => Err(denied()),
        }
    }

    /// Signs in someone whose email address an identity provider has vouched for. The account is the one with that email;
    /// when there is none it is created if `create` allows, with a random password nobody knows, so only the provider can
    /// sign it in (the person can still set a password later by an administrator resetting it).
    pub fn login_verified(&self, email: &str, create: bool) -> Result<(String, UserInfo)> {
        let email = email.trim().to_lowercase();
        let existing = match self.inner.emails.get(email.as_bytes())? {
            Some(id) => self.row(&String::from_utf8_lossy(&id))?,
            None => None,
        };
        let row = match existing {
            Some(r) => r,
            None if create => {
                let info = self.register(
                    &email,
                    &crate::util::hex(&crate::util::random_bytes::<32>()),
                    Role::User,
                )?;
                self.row(&info.id)?
                    .ok_or_else(|| Error::Corrupt("new account".into()))?
            }
            None => return Err(Error::Denied("no account for this email".into())),
        };
        Ok((self.issue(&row), Self::info(&row)))
    }

    pub fn change_password(&self, user_id: &str, new_password: &str) -> Result<()> {
        if new_password.chars().count() < 8 || new_password.len() > 1024 {
            return Err(Error::Invalid(
                "a password needs at least 8 characters (and at most 1024 bytes)".into(),
            ));
        }
        let h = hash_password(new_password)?;
        let _g = self.inner.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut r = self
            .row(user_id)?
            .ok_or_else(|| Error::NotFound("user".into()))?;
        r.password_hash = h;
        r.token_version += 1; // every session issued before this moment stops working
        self.inner
            .users
            .insert(r.id.as_bytes().to_vec(), serde_json::to_vec(&r)?)?;
        Ok(())
    }

    pub fn delete_user(&self, user_id: &str) -> Result<bool> {
        let _g = self.inner.lock.lock().unwrap_or_else(|e| e.into_inner());
        let Some(r) = self.row(user_id)? else {
            return Ok(false);
        };
        self.inner.users.remove(r.id.as_bytes().to_vec())?;
        self.inner.emails.remove(r.email.into_bytes())?;
        Ok(true)
    }

    pub fn users(&self) -> Result<Vec<UserInfo>> {
        let mut out = Vec::new();
        for g in self.inner.users.iter() {
            let (_, v) = g.into_inner()?;
            out.push(Self::info(&serde_json::from_slice(&v)?));
        }
        Ok(out)
    }

    // ---- sessions ----

    fn mac(&self, signing_input: &str) -> HmacSha256 {
        let mut m = <HmacSha256 as KeyInit>::new_from_slice(&self.inner.jwt_key)
            .expect("HMAC accepts any key length");
        m.update(signing_input.as_bytes());
        m
    }

    fn issue(&self, r: &UserRow) -> String {
        self.issue_at(r, now_ms() / 1000)
    }

    fn issue_at(&self, r: &UserRow, now: i64) -> String {
        let claims =
            json!({"sub": r.id, "tv": r.token_version, "iat": now, "exp": now + SESSION_SECS});
        let input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(JWT_HEADER),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        let sig = self.mac(&input).finalize().into_bytes();
        format!("{input}.{}", URL_SAFE_NO_PAD.encode(sig))
    }

    /// Issues a new token for a still-valid one.
    pub fn refresh(&self, token: &str) -> Result<String> {
        let p = self.verify_token(token)?;
        let Principal::User(u) = p else {
            return Err(Error::Denied("only a user session can be refreshed".into()));
        };
        let row = self
            .row(&u.id)?
            .ok_or_else(|| Error::Denied("this account no longer exists".into()))?;
        Ok(self.issue(&row))
    }

    pub fn verify_token(&self, token: &str) -> Result<Principal> {
        self.verify_token_at(token, now_ms() / 1000)
    }

    pub fn verify_token_at(&self, token: &str, now: i64) -> Result<Principal> {
        let denied =
            || Error::Denied("the session is invalid or has expired; sign in again".into());
        let mut parts = token.split('.');
        let (Some(h), Some(p), Some(s), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(denied());
        };
        if token.len() > 2048 || h != URL_SAFE_NO_PAD.encode(JWT_HEADER) {
            return Err(denied()); // exactly our header: no other algorithm is ever honoured
        }
        let sig = URL_SAFE_NO_PAD.decode(s).map_err(|_| denied())?;
        self.mac(&format!("{h}.{p}"))
            .verify_slice(&sig)
            .map_err(|_| denied())?;
        let claims: Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(p).map_err(|_| denied())?)
                .map_err(|_| denied())?;
        let exp = claims["exp"].as_i64().ok_or_else(denied)?;
        if now >= exp {
            return Err(denied());
        }
        let sub = claims["sub"].as_str().ok_or_else(denied)?;
        let row = self.row(sub)?.ok_or_else(denied)?;
        if claims["tv"].as_u64() != Some(row.token_version as u64) {
            return Err(denied());
        }
        Ok(Principal::User(Self::info(&row)))
    }

    // ---- API keys ----

    fn key_hash(key: &str) -> Vec<u8> {
        blake3::hash(key.as_bytes()).as_bytes().to_vec()
    }

    /// Creates a key and returns its secret, which is not stored and cannot be shown again.
    pub fn create_key(
        &self,
        name: &str,
        access: Access,
        collections: Vec<String>,
        admin: bool,
    ) -> Result<(String, KeyMeta)> {
        if name.trim().is_empty() || name.len() > 100 {
            return Err(Error::Invalid(
                "a key needs a name of 1 to 100 characters".into(),
            ));
        }
        if admin && (access != Access::ReadWrite || !collections.is_empty()) {
            return Err(Error::Invalid(
                "an administrative key has full access; it cannot be limited".into(),
            ));
        }
        let secret = format!("pdb_live_{}", URL_SAFE_NO_PAD.encode(random_bytes::<32>()));
        let meta = KeyMeta {
            id: format!("key_{}", hex(&random_bytes::<8>())),
            name: name.trim().to_string(),
            access,
            collections,
            admin,
            created_at: now_ms(),
        };
        self.inner
            .keys
            .insert(Self::key_hash(&secret), serde_json::to_vec(&meta)?)?;
        Ok((secret, meta))
    }

    pub fn verify_key(&self, secret: &str) -> Result<Principal> {
        if !secret.starts_with("pdb_live_") || secret.len() > 128 {
            return Err(Error::Denied("unknown API key".into()));
        }
        match self.inner.keys.get(Self::key_hash(secret))? {
            Some(v) => Ok(Principal::Key(serde_json::from_slice(&v)?)),
            None => Err(Error::Denied("unknown API key".into())),
        }
    }

    pub fn keys(&self) -> Result<Vec<KeyMeta>> {
        let mut out = Vec::new();
        for g in self.inner.keys.iter() {
            let (_, v) = g.into_inner()?;
            out.push(serde_json::from_slice(&v)?);
        }
        Ok(out)
    }

    pub fn revoke_key(&self, key_id: &str) -> Result<bool> {
        for g in self.inner.keys.iter() {
            let (k, v) = g.into_inner()?;
            let m: KeyMeta = serde_json::from_slice(&v)?;
            if m.id == key_id {
                self.inner.keys.remove(k.to_vec())?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Identifies the caller from an `Authorization: Bearer ...` value: an API key, a session token, or nothing (anonymous).
    pub fn authenticate(&self, bearer: Option<&str>) -> Result<Principal> {
        match bearer {
            None | Some("") => Ok(Principal::Anonymous),
            Some(t) if t.starts_with("pdb_live_") => self.verify_key(t),
            Some(t) => self.verify_token(t),
        }
    }
}
