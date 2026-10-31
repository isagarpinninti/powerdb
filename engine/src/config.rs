//! Settings, resolved in a fixed order, highest first: command-line flags, environment variables, `powerdb.toml` in the data directory, built-in defaults.
//! The file is strict: an unknown key is an error, so a typo cannot silently leave a setting at its default.

use crate::api::Config as ApiConfig;
use crate::error::{Error, Result};
use crate::storage::{Durability, Options};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

pub const FILE_NAME: &str = "powerdb.toml";

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FileConfig {
    pub addr: Option<SocketAddr>,
    pub open_registration: Option<bool>,
    pub trust_proxy: Option<bool>,
    pub cors_origins: Option<Vec<String>>,
    pub max_upload_gib: Option<u64>,
    /// "commit", "interval" or "never"
    pub durability: Option<String>,
    pub interval_ms: Option<u64>,
    /// PEM certificate chain and private key; both or neither. Relative paths are relative to the data directory.
    pub tls_cert: Option<PathBuf>,
    pub tls_key: Option<PathBuf>,
}

/// What the command line (and environment) said. `None` or `false` means "not given", so the file may decide.
#[derive(Debug, Clone, Default)]
pub struct Overrides {
    pub addr: Option<SocketAddr>,
    pub open_registration: bool,
    pub trust_proxy: bool,
    pub cors_origins: Vec<String>,
    pub max_upload_gib: Option<u64>,
    pub durability: Option<String>,
    pub interval_ms: Option<u64>,
    pub tls_cert: Option<PathBuf>,
    pub tls_key: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct Settings {
    /// Certificate and key, when the server terminates TLS itself.
    pub tls: Option<(PathBuf, PathBuf)>,
    pub addr: SocketAddr,
    pub api: ApiConfig,
    pub options: Options,
}

impl FileConfig {
    pub fn load(data_dir: &Path) -> Result<Option<FileConfig>> {
        let path = data_dir.join(FILE_NAME);
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text)
                .map(Some)
                .map_err(|e| Error::Invalid(format!("{}: {e}", path.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
    pub fn parse(text: &str) -> std::result::Result<FileConfig, String> {
        toml::from_str(text).map_err(|e| e.to_string())
    }
}

pub fn parse_durability(name: &str, interval_ms: u64) -> Result<Durability> {
    match name.trim().to_ascii_lowercase().as_str() {
        "commit" => Ok(Durability::Commit),
        "interval" => Ok(Durability::Interval(interval_ms.max(1))),
        "never" => Ok(Durability::Never),
        other => Err(Error::Invalid(format!(
            "durability must be commit, interval or never, not \"{other}\""
        ))),
    }
}

impl Settings {
    pub fn resolve(cli: Overrides, file: Option<FileConfig>) -> Result<Settings> {
        let file = file.unwrap_or_default();
        let interval = cli.interval_ms.or(file.interval_ms).unwrap_or(100);
        let durability = parse_durability(
            &cli.durability
                .or(file.durability)
                .unwrap_or_else(|| "commit".into()),
            interval,
        )?;
        let mut cors = file.cors_origins.unwrap_or_default();
        if !cli.cors_origins.is_empty() {
            cors = cli.cors_origins; // given on the command line: replaces the file's list
        }
        let api = ApiConfig {
            open_registration: cli.open_registration || file.open_registration.unwrap_or(false),
            trust_proxy: cli.trust_proxy || file.trust_proxy.unwrap_or(false),
            cors_origins: cors,
            max_upload_bytes: cli.max_upload_gib.or(file.max_upload_gib).unwrap_or(5) << 30,
            ..ApiConfig::default()
        };
        let tls = match (cli.tls_cert.or(file.tls_cert), cli.tls_key.or(file.tls_key)) {
            (Some(c), Some(k)) => Some((c, k)),
            (None, None) => None,
            _ => {
                return Err(Error::Invalid(
                    "tls_cert and tls_key must be given together".into(),
                ));
            }
        };
        Ok(Settings {
            tls,
            addr: cli
                .addr
                .or(file.addr)
                .unwrap_or_else(|| "127.0.0.1:8090".parse().expect("a valid default address")),
            api,
            options: Options {
                durability,
                ..Options::default()
            },
        })
    }
}

/// The file `powerdb init` writes: every setting, commented, at its default.
pub const TEMPLATE: &str = r#"# PowerDB settings. Command-line flags and environment variables (POWERDB_ADDR, POWERDB_DURABILITY, ...) override this file.

# Where to listen. 127.0.0.1 accepts connections from this machine only. To serve other machines use 0.0.0.0:8090 and serve HTTPS (tls_cert, tls_key below) or put a TLS proxy in front (see docs/OPERATIONS.md).
addr = "127.0.0.1:8090"

# When a write must be on disk before it is acknowledged:
#   "commit"   every write is fsynced first: safe against power loss, about 330 writes per second on an SSD
#   "interval" fsync in the background every interval_ms: tens of thousands per second, a power cut can lose the last interval
#   "never"    no fsync until shutdown: for throwaway data
durability = "commit"
interval_ms = 100

# Let anyone create an account. Off: administrators create users.
open_registration = false

# Trust the X-Forwarded-For header for rate limiting. Only when a proxy you control sits in front.
trust_proxy = false

# Browser origins allowed to call the API from another site, e.g. ["https://app.example.com"]. Empty: same-origin only.
cors_origins = []

# Largest single file accepted, in GiB.
max_upload_gib = 5

# Serve HTTPS directly instead of using a proxy: PEM files (relative paths are relative to this directory).
# The files are re-read every hour, so a renewed certificate takes effect without a restart.
# tls_cert = "cert.pem"
# tls_key = "key.pem"
"#;

pub fn data_dir_default() -> PathBuf {
    PathBuf::from("./powerdb-data")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_the_safe_ones() {
        let s = Settings::resolve(Overrides::default(), None).unwrap();
        assert_eq!(
            s.addr.to_string(),
            "127.0.0.1:8090",
            "listens on this machine only"
        );
        assert_eq!(s.options.durability, Durability::Commit);
        assert!(!s.api.open_registration && !s.api.trust_proxy && s.api.cors_origins.is_empty());
    }

    #[test]
    fn the_template_parses_and_means_the_defaults() {
        let f = FileConfig::parse(TEMPLATE).unwrap();
        assert_eq!(
            Settings::resolve(Overrides::default(), Some(f))
                .unwrap()
                .options
                .durability,
            Durability::Commit
        );
    }

    #[test]
    fn flags_beat_the_file_and_the_file_beats_defaults() {
        let file = FileConfig::parse("addr = \"0.0.0.0:9000\"\ndurability = \"interval\"\ninterval_ms = 250\nmax_upload_gib = 2\ncors_origins = [\"https://a.example\"]").unwrap();
        let s = Settings::resolve(Overrides::default(), Some(file.clone())).unwrap();
        assert_eq!(s.addr.to_string(), "0.0.0.0:9000");
        assert_eq!(s.options.durability, Durability::Interval(250));
        assert_eq!(s.api.max_upload_bytes, 2 << 30);
        assert_eq!(s.api.cors_origins, vec!["https://a.example"]);
        let cli = Overrides {
            addr: Some("127.0.0.1:1234".parse().unwrap()),
            durability: Some("never".into()),
            cors_origins: vec!["https://b.example".into()],
            ..Default::default()
        };
        let s = Settings::resolve(cli, Some(file)).unwrap();
        assert_eq!(s.addr.port(), 1234);
        assert_eq!(s.options.durability, Durability::Never);
        assert_eq!(s.api.cors_origins, vec!["https://b.example"]);
    }

    #[test]
    fn tls_needs_both_files_or_neither() {
        let one = |c: Option<&str>, k: Option<&str>| {
            Settings::resolve(
                Overrides {
                    tls_cert: c.map(Into::into),
                    tls_key: k.map(Into::into),
                    ..Default::default()
                },
                None,
            )
        };
        assert!(one(Some("c"), None).is_err() && one(None, Some("k")).is_err());
        assert!(one(Some("c"), Some("k")).unwrap().tls.is_some());
        assert!(one(None, None).unwrap().tls.is_none());
    }

    #[test]
    fn a_typo_in_the_file_is_an_error_not_a_silent_default() {
        assert!(FileConfig::parse("adr = \"127.0.0.1:1\"").is_err());
        assert!(FileConfig::parse("durability = 5").is_err());
        assert!(FileConfig::parse("addr = \"not an address\"").is_err());
        assert!(
            Settings::resolve(
                Overrides {
                    durability: Some("sometimes".into()),
                    ..Default::default()
                },
                None
            )
            .is_err()
        );
    }
}
