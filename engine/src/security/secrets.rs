//! One server secret, from which every key the server needs is derived with a distinct label, so the encryption key, the URL-signing key and the token-signing
//! key never coincide. The secret comes from `POWERDB_SECRET` (64 hex characters) or from `<data>/secret.key`, created with mode 0600 on first run.
//!
//! Keeping the secret in the same directory as the data protects against a copied database file or a stolen backup of `meta/` and `blobs/`, not against someone who
//! can read the whole data directory: for that, set `POWERDB_SECRET` from your secret store and keep the file out of the directory.

use crate::error::{Error, Result};
use crate::util::{from_hex_32, hex, random_bytes};
use std::path::Path;

#[derive(Clone)]
pub struct Secrets {
    master: [u8; 32],
}

impl Secrets {
    pub fn from_bytes(master: [u8; 32]) -> Self {
        Self { master }
    }

    /// Like `load_or_create` but never writes anything: None when the environment and the data directory hold no secret.
    pub fn load_existing(dir: &Path) -> Result<Option<Self>> {
        if std::env::var("POWERDB_SECRET").is_ok() {
            return Self::load_or_create(dir).map(Some);
        }
        if dir.join("secret.key").is_file() {
            return Self::load_or_create(dir).map(Some);
        }
        Ok(None)
    }

    /// Reads the secret from the environment, else from `<dir>/secret.key`, else creates that file.
    pub fn load_or_create(dir: &Path) -> Result<Self> {
        if let Ok(v) = std::env::var("POWERDB_SECRET") {
            let k = from_hex_32(v.trim()).ok_or_else(|| Error::Invalid("POWERDB_SECRET must be exactly 64 hex characters".into()))?;
            return Ok(Self { master: k });
        }
        std::fs::create_dir_all(dir)?;
        let path = dir.join("secret.key");
        match std::fs::read_to_string(&path) {
            Ok(s) => Ok(Self { master: from_hex_32(s.trim()).ok_or_else(|| Error::Corrupt(format!("{} is not 64 hex characters", path.display())))? }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let k: [u8; 32] = random_bytes();
                use std::io::Write;
                let mut f = crate::fileio::create_private(&path)?;
                f.write_all(hex(&k).as_bytes())?;
                f.sync_all()?;
                Ok(Self { master: k })
            }
            Err(e) => Err(e.into()),
        }
    }

    /// A 32-byte key for one purpose. Different labels give unrelated keys.
    pub fn derive(&self, label: &str) -> [u8; 32] {
        let mut h = blake3::Hasher::new_derive_key(&format!("powerdb {label} v1"));
        h.update(&self.master);
        *h.finalize().as_bytes()
    }
}
