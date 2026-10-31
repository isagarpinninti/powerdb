//! Short-lived signed file URLs. A token is  base64url( expiry (8 bytes, big endian) | HMAC-SHA256(key, scope | expiry) )  where the scope names exactly one
//! file of one record (`collection/record/field/file`). It cannot be moved to another file, extended, or reused after the record is deleted or its file replaced
//! (the API also requires the record to exist, so deleting a record revokes its URLs at once).

use crate::error::{Error, Result};
use crate::security::secrets::Secrets;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;
pub const DEFAULT_TTL_SECS: i64 = 60;

#[derive(Clone)]
pub struct UrlSigner {
    key: [u8; 32],
}

impl UrlSigner {
    pub fn new(secrets: &Secrets) -> Self {
        Self { key: secrets.derive("signed urls") }
    }

    fn mac(&self, scope: &str, exp: i64) -> HmacSha256 {
        let mut m = <HmacSha256 as KeyInit>::new_from_slice(&self.key).expect("HMAC accepts any key length");
        m.update(b"powerdb-url-v1\0");
        m.update(scope.as_bytes());
        m.update(&[0]);
        m.update(&exp.to_be_bytes());
        m
    }

    pub fn sign(&self, scope: &str, expires_at_unix: i64) -> String {
        let tag = self.mac(scope, expires_at_unix).finalize().into_bytes();
        let mut raw = Vec::with_capacity(8 + 32);
        raw.extend_from_slice(&expires_at_unix.to_be_bytes());
        raw.extend_from_slice(&tag);
        URL_SAFE_NO_PAD.encode(raw)
    }

    pub fn verify(&self, scope: &str, token: &str, now_unix: i64) -> Result<()> {
        let denied = || Error::Denied("this file link is invalid or has expired".into());
        let raw = URL_SAFE_NO_PAD.decode(token).map_err(|_| denied())?;
        if raw.len() != 40 {
            return Err(denied());
        }
        let exp = i64::from_be_bytes(raw[0..8].try_into().map_err(|_| denied())?);
        // Check the MAC first and in constant time, then the clock: an attacker learns nothing about either from timing.
        self.mac(scope, exp).verify_slice(&raw[8..]).map_err(|_| denied())?;
        if now_unix > exp {
            return Err(denied());
        }
        Ok(())
    }
}
