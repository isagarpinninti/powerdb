use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine;
use hmac::{Hmac, KeyInit, Mac};
use powerdb::auth::{Access, Action, Auth, Principal, Role, SESSION_SECS};
use powerdb::util::new_id;
use powerdb::{Db, Durability, Options};
use serde_json::json;
use sha2::Sha256;

struct Dir(std::path::PathBuf);
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn setup() -> (Dir, Db, Auth) {
    let d = Dir(std::env::temp_dir().join(format!("powerdb-auth-{}", new_id())));
    let db = Db::open(&d.0, Options { durability: Durability::Never, ..Options::default() }).unwrap();
    let auth = Auth::new(&db, [3; 32]).unwrap();
    (d, db, auth)
}

fn forge(key: &[u8], header: &str, claims: serde_json::Value) -> String {
    let input = format!("{}.{}", B64.encode(header), B64.encode(claims.to_string()));
    let mut m = <Hmac<Sha256> as KeyInit>::new_from_slice(key).unwrap();
    m.update(input.as_bytes());
    format!("{input}.{}", B64.encode(m.finalize().into_bytes()))
}

#[test]
fn register_login_and_the_session_it_returns() {
    let (_d, _db, auth) = setup();
    let u = auth.register("  Ada@Example.COM ", "correct horse battery", Role::User).unwrap();
    assert_eq!(u.email, "ada@example.com");
    assert!(u.id.starts_with("usr_"));
    let (token, who) = auth.login("ADA@example.com", "correct horse battery").unwrap();
    assert_eq!(who, u);
    match auth.authenticate(Some(&token)).unwrap() {
        Principal::User(p) => assert_eq!(p.id, u.id),
        other => panic!("{other:?}"),
    }
    assert!(matches!(auth.authenticate(None).unwrap(), Principal::Anonymous));
    assert!(auth.register("ada@example.com", "another password", Role::User).unwrap_err().to_string().contains("already exists"));
}

#[test]
fn bad_input_and_wrong_passwords_are_refused_alike() {
    let (_d, _db, auth) = setup();
    for e in ["", "no-at-sign", "a@b", "a b@c.d", "@c.d", "a@@c.d", "a@c..d", "a@.c.d"] {
        assert!(auth.register(e, "long enough pw", Role::User).is_err(), "{e:?}");
    }
    assert!(auth.register("a@b.co", "short", Role::User).is_err());
    auth.register("a@b.co", "long enough pw", Role::User).unwrap();
    let wrong = auth.login("a@b.co", "wrong password").unwrap_err().to_string();
    let unknown = auth.login("nobody@b.co", "wrong password").unwrap_err().to_string();
    assert_eq!(wrong, unknown, "the message must not reveal whether the account exists");
}

#[test]
fn forged_and_stale_tokens_are_rejected() {
    let (_d, _db, auth) = setup();
    let u = auth.register("u@x.io", "password one", Role::User).unwrap();
    let (token, _) = auth.login("u@x.io", "password one").unwrap();
    let now = powerdb::util::now_ms() / 1000;

    assert!(auth.verify_token_at(&token, now).is_ok());
    assert!(auth.verify_token_at(&token, now + SESSION_SECS + 1).is_err(), "expired");

    // a token signed with a different key
    let other = forge(&[9; 32], r#"{"alg":"HS256","typ":"JWT"}"#, json!({"sub": u.id, "tv": 0, "exp": now + 1000}));
    assert!(auth.verify_token(&other).is_err());
    // alg none, with and without a signature
    let none = format!("{}.{}.", B64.encode(r#"{"alg":"none","typ":"JWT"}"#), B64.encode(json!({"sub": u.id, "tv": 0, "exp": now + 1000}).to_string()));
    assert!(auth.verify_token(&none).is_err());
    // a different (even if valid-looking) header is never honoured
    let hs512 = forge(&[3; 32], r#"{"alg":"HS512","typ":"JWT"}"#, json!({"sub": u.id, "tv": 0, "exp": now + 1000}));
    assert!(auth.verify_token(&hs512).is_err());
    // editing the claims of a real token breaks the signature
    let parts: Vec<&str> = token.split('.').collect();
    let promoted = format!("{}.{}.{}", parts[0], B64.encode(json!({"sub": u.id, "tv": 0, "exp": now + 99999, "role": "admin"}).to_string()), parts[2]);
    assert!(auth.verify_token(&promoted).is_err());
    for junk in ["", "a.b", "a.b.c.d", "....", &"x".repeat(5000)] {
        assert!(auth.verify_token(junk).is_err());
    }
}

#[test]
fn changing_a_password_or_deleting_a_user_ends_every_earlier_session() {
    let (_d, _db, auth) = setup();
    let u = auth.register("u@x.io", "password one", Role::User).unwrap();
    let (t1, _) = auth.login("u@x.io", "password one").unwrap();
    assert!(auth.verify_token(&t1).is_ok());
    auth.change_password(&u.id, "password two").unwrap();
    assert!(auth.verify_token(&t1).is_err(), "the old session died with the old password");
    assert!(auth.login("u@x.io", "password one").is_err());
    let (t2, _) = auth.login("u@x.io", "password two").unwrap();
    assert!(auth.verify_token(&t2).is_ok());
    let t3 = auth.refresh(&t2).unwrap();
    assert!(auth.verify_token(&t3).is_ok());
    assert!(auth.delete_user(&u.id).unwrap());
    assert!(auth.verify_token(&t2).is_err() && auth.verify_token(&t3).is_err(), "a deleted user's tokens stop working");
    assert!(auth.login("u@x.io", "password two").is_err());
    assert!(auth.register("u@x.io", "password three", Role::User).is_ok(), "the address is free again");
}

#[test]
fn api_keys_are_secret_scoped_and_revocable() {
    let (_d, _db, auth) = setup();
    let (secret, meta) = auth.create_key("reporting", Access::Read, vec!["invoices".into()], false).unwrap();
    assert!(secret.starts_with("pdb_live_") && secret.len() > 40);
    let p = auth.authenticate(Some(&secret)).unwrap();
    assert!(!p.is_admin());
    assert!(p.scope_allows("invoices", Action::List) && p.scope_allows("invoices", Action::View));
    assert!(!p.scope_allows("invoices", Action::Create) && !p.scope_allows("invoices", Action::Delete), "read-only");
    assert!(!p.scope_allows("users", Action::List), "other collections are out of scope");
    let (wsecret, _) = auth.create_key("ingest", Access::Write, vec![], false).unwrap();
    let w = auth.authenticate(Some(&wsecret)).unwrap();
    assert!(w.scope_allows("anything", Action::Create) && !w.scope_allows("anything", Action::View));
    let (asecret, _) = auth.create_key("root", Access::ReadWrite, vec![], true).unwrap();
    assert!(auth.authenticate(Some(&asecret)).unwrap().is_admin());
    assert!(auth.create_key("bad", Access::Read, vec![], true).is_err(), "an admin key cannot be limited");
    // a wrong or altered secret is refused; the secret is not stored in the clear
    assert!(auth.authenticate(Some(&format!("{secret}x"))).is_err());
    assert!(auth.authenticate(Some("pdb_live_nope")).is_err());
    assert_eq!(auth.keys().unwrap().len(), 3);
    assert!(auth.revoke_key(&meta.id).unwrap());
    assert!(auth.authenticate(Some(&secret)).is_err(), "revoked at once");
    assert!(!auth.revoke_key(&meta.id).unwrap());
}

#[test]
fn rules_see_who_is_calling() {
    let (_d, _db, auth) = setup();
    let u = auth.register("u@x.io", "password one", Role::Admin).unwrap();
    let (t, _) = auth.login("u@x.io", "password one").unwrap();
    let p = auth.authenticate(Some(&t)).unwrap();
    assert!(p.is_admin());
    assert_eq!(p.auth_value().unwrap()["id"], u.id.as_str());
    assert!(Principal::Anonymous.auth_value().is_none());
    assert!(!Principal::Anonymous.is_admin());
}
