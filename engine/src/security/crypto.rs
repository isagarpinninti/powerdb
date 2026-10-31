//! Authenticated encryption of file chunks at rest: ChaCha20-Poly1305 with a fresh random 96-bit nonce per chunk. The chunk's index key is bound in as
//! associated data, so a ciphertext copied to another chunk's slot fails to authenticate. The stored form is  nonce (12) | ciphertext | tag (16).

use super::secrets::Secrets;
use crate::error::{Error, Result};
use crate::storage::ChunkCodec;
use crate::util::random_bytes;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};

pub const ENCRYPTED_FLAG: u8 = 1;

pub struct ChaChaCodec {
    cipher: ChaCha20Poly1305,
}

impl ChaChaCodec {
    pub fn new(secrets: &Secrets) -> Self {
        let key = secrets.derive("chunk encryption");
        Self { cipher: ChaCha20Poly1305::new(&Key::from(key)) }
    }
}

impl ChunkCodec for ChaChaCodec {
    fn flags(&self) -> u8 {
        ENCRYPTED_FLAG
    }
    fn encode(&self, key: &[u8; 32], raw: &[u8]) -> Result<Vec<u8>> {
        let nonce: [u8; 12] = random_bytes();
        let ct = self.cipher.encrypt(&Nonce::from(nonce), Payload { msg: raw, aad: key }).map_err(|_| Error::Corrupt("encryption failed".into()))?;
        let mut out = Vec::with_capacity(12 + ct.len());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        Ok(out)
    }
    fn decode(&self, key: &[u8; 32], stored: &[u8]) -> Result<Vec<u8>> {
        if stored.len() < 12 + 16 {
            return Err(Error::Corrupt("an encrypted chunk is shorter than its nonce and tag".into()));
        }
        let (nonce, ct) = stored.split_at(12);
        let nonce: [u8; 12] = nonce.try_into().map_err(|_| Error::Corrupt("nonce".into()))?;
        self.cipher.decrypt(&Nonce::from(nonce), Payload { msg: ct, aad: key }).map_err(|_| Error::Corrupt("an encrypted chunk failed authentication: wrong key, or the data was altered".into()))
    }
}
