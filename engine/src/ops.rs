//! Operating a database directory: create one, check it, back it up, restore it, ask a running server how it is.
//!
//! Backup and restore are offline on purpose. The directory's lock is held for the whole copy, so no server can be writing while files are read, and the copy is
//! a consistent state. (An online backup would need the storage engine to expose a snapshot of its files; it does not yet.)

use crate::config::{FILE_NAME, TEMPLATE};
use crate::error::{Error, Result};
use crate::server;
use crate::storage::{Db, Options};
use crate::util::{hex, now_ms};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

fn is_empty_dir(p: &Path) -> Result<bool> {
    match std::fs::read_dir(p) {
        Ok(mut d) => Ok(d.next().is_none()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(e) => Err(e.into()),
    }
}

#[derive(Debug)]
pub struct Initialized {
    pub admin_key: Option<String>,
    pub admin_user: Option<String>,
    pub config_path: PathBuf,
}

/// Creates a new data directory: the settings file, the server secret, and a first administrator (from POWERDB_ADMIN_EMAIL / POWERDB_ADMIN_PASSWORD, else a key).
pub fn init(dir: &Path) -> Result<Initialized> {
    if !is_empty_dir(dir)? {
        return Err(Error::Conflict(format!(
            "{} is not empty; choose a new directory (init never touches an existing database)",
            dir.display()
        )));
    }
    std::fs::create_dir_all(dir)?;
    let config_path = dir.join(FILE_NAME);
    std::fs::write(&config_path, TEMPLATE)?;
    let (state, boot) =
        server::build_state(dir, Options::default(), crate::api::Config::default())?;
    state.db.flush()?;
    Ok(Initialized {
        admin_key: boot.admin_key,
        admin_user: boot.admin_user,
        config_path,
    })
}

#[derive(Serialize, Deserialize)]
struct BackupFile {
    path: String,
    size: u64,
    blake3: String,
}

#[derive(Serialize, Deserialize)]
struct BackupManifest {
    powerdb_version: String,
    created_at: i64,
    includes_secret: bool,
    files: Vec<BackupFile>,
}

#[derive(Debug)]
pub struct BackupReport {
    pub files: usize,
    pub bytes: u64,
    pub includes_secret: bool,
}

fn walk(root: &Path, rel: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for e in std::fs::read_dir(root.join(rel))? {
        let e = e?;
        let r = rel.join(e.file_name());
        if e.file_type()?.is_dir() {
            walk(root, &r, out)?;
        } else if e.file_type()?.is_file() {
            out.push(r);
        }
    }
    Ok(())
}

fn hash_file(p: &Path) -> Result<(u64, String)> {
    let mut f = std::fs::File::open(p)?;
    let mut h = blake3::Hasher::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut n = 0u64;
    loop {
        let k = f.read(&mut buf)?;
        if k == 0 {
            break;
        }
        h.update(&buf[..k]);
        n += k as u64;
    }
    Ok((n, hex(h.finalize().as_bytes())))
}

fn copy_file(from: &Path, to: &Path) -> Result<()> {
    if let Some(p) = to.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::copy(from, to)?;
    Ok(())
}

/// Copies a database to `dest` (which must be empty or absent) with a manifest of checksums. The server secret is left out unless `include_secret`: with it, a stolen
/// backup also holds the key to its own encrypted files; without it, restoring needs POWERDB_SECRET or the original `secret.key` kept somewhere else.
pub fn backup(data: &Path, dest: &Path, include_secret: bool) -> Result<BackupReport> {
    if !is_empty_dir(dest)? {
        return Err(Error::Conflict(format!("{} is not empty", dest.display())));
    }
    // Holding the database open holds its lock: a running server makes this fail, and no one can start one until the copy is done.
    let db = Db::open(data, Options::default())?;
    db.flush()?;
    let mut rel = Vec::new();
    for dir in ["meta", "blobs"] {
        if data.join(dir).is_dir() {
            walk(data, Path::new(dir), &mut rel)?;
        }
    }
    let secret_present = data.join("secret.key").is_file();
    if include_secret && secret_present {
        rel.push(PathBuf::from("secret.key"));
    }
    if data.join(FILE_NAME).is_file() {
        rel.push(PathBuf::from(FILE_NAME));
    }
    rel.push(PathBuf::from("FORMAT"));
    rel.sort();
    std::fs::create_dir_all(dest)?;
    let mut files = Vec::new();
    let mut bytes = 0;
    for r in &rel {
        copy_file(&data.join(r), &dest.join(r))?;
        let (size, blake3) = hash_file(&dest.join(r))?;
        bytes += size;
        files.push(BackupFile {
            path: r.to_string_lossy().replace('\\', "/"),
            size,
            blake3,
        });
    }
    let manifest = BackupManifest {
        powerdb_version: env!("CARGO_PKG_VERSION").into(),
        created_at: now_ms(),
        includes_secret: include_secret && secret_present,
        files,
    };
    let mut f = std::fs::File::create(dest.join("BACKUP.json"))?;
    f.write_all(serde_json::to_string_pretty(&manifest)?.as_bytes())?;
    f.sync_all()?;
    Ok(BackupReport {
        files: manifest.files.len(),
        bytes,
        includes_secret: manifest.includes_secret,
    })
}

/// Restores a backup into an empty directory, checking every file against the manifest before and the database's own structure after.
pub fn restore(from: &Path, data: &Path) -> Result<BackupReport> {
    if !is_empty_dir(data)? {
        return Err(Error::Conflict(format!(
            "{} is not empty; restore only into a new directory",
            data.display()
        )));
    }
    let manifest: BackupManifest =
        serde_json::from_slice(&std::fs::read(from.join("BACKUP.json")).map_err(|e| {
            Error::Invalid(format!(
                "{} is not a PowerDB backup (no BACKUP.json): {e}",
                from.display()
            ))
        })?)?;
    let mut bytes = 0;
    for f in &manifest.files {
        if f.path.split('/').any(|c| c == ".." || c.is_empty()) || Path::new(&f.path).is_absolute()
        {
            return Err(Error::Corrupt(format!(
                "the backup lists an unsafe path: {}",
                f.path
            )));
        }
        let p = from.join(&f.path);
        let (size, h) = hash_file(&p).map_err(|e| Error::Corrupt(format!("{}: {e}", f.path)))?;
        if size != f.size || h != f.blake3 {
            return Err(Error::Corrupt(format!(
                "{} does not match the backup's checksum; the backup is damaged",
                f.path
            )));
        }
    }
    std::fs::create_dir_all(data)?;
    for f in &manifest.files {
        copy_file(&from.join(&f.path), &data.join(&f.path))?;
        bytes += f.size;
    }
    // open it the way a server would, and check the structure
    let db = Db::open(data, Options::default())?;
    let report = db.verify(false)?;
    if !report.problems.is_empty() {
        return Err(Error::Corrupt(format!(
            "the restored database fails its integrity check: {}",
            report.problems.join("; ")
        )));
    }
    Ok(BackupReport {
        files: manifest.files.len(),
        bytes,
        includes_secret: manifest.includes_secret,
    })
}

/// Accepts any certificate. Only for `status`, which sends no credentials and learns nothing but "is it up";
/// it must never be used for a connection that carries data.
#[derive(Debug)]
struct LivenessOnly(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for LivenessOnly {
    fn verify_server_cert(
        &self,
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &[rustls::pki_types::CertificateDer<'_>],
        _: &rustls::pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        m: &[u8],
        c: &rustls::pki_types::CertificateDer<'_>,
        d: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(m, c, d, &self.0.signature_verification_algorithms)
    }
    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &rustls::pki_types::CertificateDer<'_>,
        d: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(m, c, d, &self.0.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

fn probe(addr: SocketAddr, tls: bool) -> Result<String> {
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(3)).map_err(|e| {
        Error::Io(std::io::Error::new(
            e.kind(),
            format!("cannot reach {addr}: {e}"),
        ))
    })?;
    s.set_read_timeout(Some(Duration::from_secs(3)))?;
    let request =
        format!("GET /api/v1/healthz HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    let raw = if tls {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let cfg = rustls::ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .map_err(|e| Error::Invalid(e.to_string()))?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(LivenessOnly(provider)))
            .with_no_client_auth();
        let name = rustls::pki_types::ServerName::IpAddress(addr.ip().into());
        let mut conn = rustls::ClientConnection::new(Arc::new(cfg), name)
            .map_err(|e| Error::Invalid(e.to_string()))?;
        let mut stream = rustls::Stream::new(&mut conn, &mut s);
        stream.write_all(request.as_bytes())?;
        // a server that closes without a TLS close_notify ends the read with an error after the body; the body is what matters
        let mut raw = Vec::new();
        let _ = stream.read_to_end(&mut raw);
        raw
    } else {
        s.write_all(request.as_bytes())?;
        let mut raw = Vec::new();
        let _ = s.read_to_end(&mut raw);
        raw
    };
    Ok(String::from_utf8_lossy(&raw).into_owned())
}

/// Asks a running server whether it is up. Plain HTTP first; if nothing HTTP comes back, HTTPS (a server with TLS configured).
pub fn status(addr: SocketAddr) -> Result<String> {
    let mut scheme = "http";
    let mut resp = probe(addr, false)?;
    if !resp.starts_with("HTTP/") {
        resp = probe(addr, true).unwrap_or_default();
        scheme = "https";
    }
    let line = resp.lines().next().unwrap_or("");
    if line.contains(" 200 ") && resp.trim_end().ends_with("ok") {
        Ok(format!("PowerDB is running at {scheme}://{addr}"))
    } else {
        Err(Error::Invalid(format!(
            "something answered at {addr}, but it is not a healthy PowerDB: {line}"
        )))
    }
}
