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
    /// Where online backups go (default `backups` inside the data directory). Put it on another disk.
    pub backup_dir: Option<PathBuf>,
    /// Sign in with an identity provider.
    pub oauth: Option<OAuthFile>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OAuthFile {
    /// The address browsers use to reach this server, e.g. "https://db.example.com". The providers send people back to it.
    pub public_url: String,
    /// Where a finished sign-in may send the browser. Exact matches only.
    #[serde(default)]
    pub redirects: Vec<String>,
    /// Let administrator accounts sign in this way too. Off: the provider is not a way into the whole database.
    #[serde(default)]
    pub allow_admin: bool,
    #[serde(default)]
    pub providers: std::collections::BTreeMap<String, ProviderFile>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProviderFile {
    /// "google", "github" or "generic". Default: the provider's own name if it is google or github, else generic.
    pub kind: Option<String>,
    pub client_id: String,
    /// Prefer the environment variable POWERDB_OAUTH_<NAME>_CLIENT_SECRET to keeping the secret in this file.
    pub client_secret: Option<String>,
    pub auth_url: Option<String>,
    pub token_url: Option<String>,
    pub userinfo_url: Option<String>,
    pub emails_url: Option<String>,
    pub scopes: Option<Vec<String>>,
}

fn http_url(s: &str, what: &str) -> Result<()> {
    if (s.starts_with("https://") || s.starts_with("http://"))
        && !s.contains(char::is_whitespace)
        && !s.contains('#')
        && s.len() < 2000
    {
        Ok(())
    } else {
        Err(Error::Invalid(format!(
            "{what} must be an http(s) URL without spaces or a #fragment: {s:?}"
        )))
    }
}

/// Turns the file's `[oauth]` section into what the server uses, and refuses anything half-configured.
pub fn resolve_oauth(f: &OAuthFile) -> Result<crate::api::oauth::OAuthConfig> {
    use crate::api::oauth::{Kind, OAuthConfig, Provider, complete};
    http_url(&f.public_url, "oauth.public_url")?;
    for r in &f.redirects {
        http_url(r, "an oauth.redirects entry")?;
    }
    let mut cfg = OAuthConfig {
        public_url: f.public_url.clone(),
        redirects: f.redirects.clone(),
        allow_admin: f.allow_admin,
        providers: Default::default(),
    };
    for (name, p) in &f.providers {
        if !crate::model::valid_name(name) {
            return Err(Error::Invalid(format!(
                "oauth provider name {name:?} must be lower-case letters, digits and underscores"
            )));
        }
        let kind = match p.kind.as_deref().unwrap_or(name) {
            "google" => Kind::Google,
            "github" => Kind::GitHub,
            "generic" => Kind::Generic,
            other if p.kind.is_none() => {
                let _ = other;
                Kind::Generic
            }
            other => {
                return Err(Error::Invalid(format!(
                    "oauth provider {name}: kind must be google, github or generic, not {other:?}"
                )));
            }
        };
        let secret = std::env::var(format!(
            "POWERDB_OAUTH_{}_CLIENT_SECRET",
            name.to_uppercase()
        ))
        .ok()
        .or_else(|| p.client_secret.clone())
        .unwrap_or_default();
        let provider = Provider {
            kind,
            client_id: p.client_id.clone(),
            client_secret: secret,
            auth_url: p.auth_url.clone().unwrap_or_default(),
            token_url: p.token_url.clone().unwrap_or_default(),
            userinfo_url: p.userinfo_url.clone().unwrap_or_default(),
            emails_url: p.emails_url.clone(),
            scopes: p.scopes.clone().unwrap_or_default(),
        };
        let provider = complete(name, provider).map_err(Error::Invalid)?;
        for (u, what) in [
            (&provider.auth_url, "auth_url"),
            (&provider.token_url, "token_url"),
            (&provider.userinfo_url, "userinfo_url"),
        ] {
            http_url(u, &format!("oauth provider {name}: {what}"))?;
        }
        cfg.providers.insert(name.clone(), provider);
    }
    Ok(cfg)
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
            backup_dir: file.backup_dir.clone(),
            oauth: match &file.oauth {
                Some(o) => resolve_oauth(o)?,
                None => Default::default(),
            },
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

# Where online backups (POST /api/v1/maintenance/backups) are written. Relative: inside this directory. Use another disk.
# backup_dir = "backups"

# Sign in with Google, GitHub or another OAuth2 provider. People are sent back to public_url, then on to one of `redirects`
# (exact matches only) with their session token in the URL fragment. Register {public_url}/api/v1/auth/oauth/<provider>/callback
# with the provider. Keep secrets out of this file: set POWERDB_OAUTH_GOOGLE_CLIENT_SECRET (and so on) instead.
# An account must already exist unless open_registration is on; administrator accounts cannot sign in this way unless allow_admin.
# [oauth]
# public_url = "https://db.example.com"
# redirects = ["https://app.example.com/signed-in"]
# [oauth.providers.google]
# client_id = "...apps.googleusercontent.com"
# [oauth.providers.github]
# client_id = "..."
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

    fn oauth(toml: &str) -> Result<Settings> {
        Settings::resolve(Overrides::default(), Some(FileConfig::parse(toml).unwrap()))
    }

    #[test]
    fn oauth_providers_are_completed_and_validated_when_the_server_starts() {
        let ok = oauth("[oauth]\npublic_url = \"https://db.example.com\"\nredirects = [\"https://app.example.com/done\"]\n[oauth.providers.github]\nclient_id = \"id\"\nclient_secret = \"s\"\n[oauth.providers.google]\nclient_id = \"id2\"\nclient_secret = \"s2\"").unwrap();
        let o = &ok.api.oauth;
        assert_eq!(
            o.providers["github"].auth_url,
            "https://github.com/login/oauth/authorize"
        );
        assert!(
            o.providers["github"].emails_url.is_some()
                && o.providers["google"].scopes == ["openid", "email"]
        );
        // what is missing or wrong is refused now, not at someone's first sign-in
        assert!(oauth("[oauth]\npublic_url = \"https://x.example\"\n[oauth.providers.github]\nclient_id = \"id\"").is_err(), "no client secret");
        assert!(
            oauth("[oauth]\npublic_url = \"ftp://x.example\"").is_err(),
            "public_url must be http(s)"
        );
        assert!(oauth("[oauth]\npublic_url = \"https://x.example\"\nredirects = [\"https://a.example/#frag\"]").is_err(), "a redirect cannot carry a fragment");
        assert!(oauth("[oauth]\npublic_url = \"https://x.example\"\n[oauth.providers.okta]\nclient_id = \"i\"\nclient_secret = \"s\"").is_err(), "a generic provider needs its URLs");
        assert!(oauth("[oauth]\npublic_url = \"https://x.example\"\n[oauth.providers.github]\nkind = \"facebook\"\nclient_id = \"i\"\nclient_secret = \"s\"").is_err(), "unknown kind");
        assert!(
            FileConfig::parse("[oauth]\npublic_url = \"https://x.example\"\nredirect = []")
                .is_err(),
            "a typo is an error"
        );
        let none = Settings::resolve(Overrides::default(), None).unwrap();
        assert!(none.api.oauth.providers.is_empty(), "off unless configured");
    }

    #[test]
    fn the_client_secret_can_come_from_the_environment() {
        // a provider name no other test uses, because the environment is shared by the whole process
        unsafe { std::env::set_var("POWERDB_OAUTH_ENVONLYTEST_CLIENT_SECRET", "from-env") };
        let s = oauth("[oauth]\npublic_url = \"https://x.example\"\n[oauth.providers.envonlytest]\nclient_id = \"i\"\nauth_url = \"https://a.example/auth\"\ntoken_url = \"https://a.example/token\"\nuserinfo_url = \"https://a.example/me\"").unwrap();
        assert_eq!(
            s.api.oauth.providers["envonlytest"].client_secret,
            "from-env"
        );
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
