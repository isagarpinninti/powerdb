//! The app's own settings, and the administrator key. Settings are a small JSON file; the key lives in the operating
//! system's credential store (macOS Keychain, Windows Credential Manager), never in a file.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const DEFAULT_PORT: u16 = 8090;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    /// False until the setup wizard has created or chosen a database.
    pub configured: bool,
    pub data_dir: String,
    pub port: u16,
    /// Start the server whenever the app starts.
    pub auto_start_server: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            configured: false,
            data_dir: String::new(),
            port: DEFAULT_PORT,
            auto_start_server: true,
        }
    }
}

pub fn load(dir: &Path) -> Config {
    std::fs::read(dir.join("config.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save(dir: &Path, c: &Config) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let tmp = dir.join("config.json.tmp");
    std::fs::write(
        &tmp,
        serde_json::to_vec_pretty(c).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, dir.join("config.json")).map_err(|e| e.to_string())
}

/// Where a new database goes by default: inside the user's own application data, so no administrator rights are needed.
pub fn default_data_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    let base =
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"));
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    #[cfg(not(any(target_os = "macos", windows)))]
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")));
    base.unwrap_or_else(|| PathBuf::from("."))
        .join("PowerDB")
        .join("data")
}

fn entry(data_dir: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new("PowerDB", &format!("admin-key:{data_dir}")).map_err(|e| e.to_string())
}

pub fn store_key(data_dir: &str, key: &str) -> Result<(), String> {
    entry(data_dir)?
        .set_password(key)
        .map_err(|e| format!("cannot save the key in the system credential store: {e}"))
}

pub fn read_key(data_dir: &str) -> Option<String> {
    entry(data_dir).ok()?.get_password().ok()
}

/// The administrator key, kept in memory after the first successful read.
///
/// The system credential store can be slow, and on macOS it may wait for the person to answer a Keychain dialog, so it is
/// never read on the path that draws the interface, never read twice, and never waited on for ever.
pub type KeyCache = Arc<Mutex<Option<(String, String)>>>;

pub fn remember_key(cache: &KeyCache, data_dir: &str, key: &str) {
    *cache.lock().unwrap_or_else(|e| e.into_inner()) =
        Some((data_dir.to_string(), key.to_string()));
}

/// The key for this database, from memory or else the credential store, waiting at most `wait` for it.
pub fn key_for(cache: &KeyCache, data_dir: &str, wait: Duration) -> Result<String, String> {
    if let Some((d, k)) = cache.lock().unwrap_or_else(|e| e.into_inner()).as_ref()
        && d == data_dir
    {
        return Ok(k.clone());
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let (cache2, dir) = (cache.clone(), data_dir.to_string());
    std::thread::spawn(move || {
        let found = read_key(&dir);
        if let Some(k) = &found {
            remember_key(&cache2, &dir, k); // even if the caller gave up waiting, the next call finds it
        }
        let _ = tx.send(found);
    });
    match rx.recv_timeout(wait) {
        Ok(Some(k)) => Ok(k),
        Ok(None) => Err("the administrator key is not in this computer's credential store. Enter it again under Settings.".into()),
        Err(_) => Err("the system is still waiting for you to allow PowerDB to use its saved administrator key. Look for a Keychain dialog (it may be behind other windows), choose Allow, then try again.".into()),
    }
}

pub fn forget_key(data_dir: &str) {
    if let Ok(e) = entry(data_dir) {
        let _ = e.delete_credential();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_a_damaged_file_falls_back_to_defaults() {
        let d = std::env::temp_dir().join(format!("pdb-desktop-cfg-{}", std::process::id()));
        let c = Config {
            configured: true,
            data_dir: "/x".into(),
            port: 9000,
            auto_start_server: false,
        };
        save(&d, &c).unwrap();
        assert_eq!(load(&d), c);
        std::fs::write(d.join("config.json"), b"{not json").unwrap();
        assert_eq!(load(&d), Config::default());
        std::fs::write(d.join("config.json"), br#"{"port": 9100}"#).unwrap(); // a partial file keeps the other defaults
        assert_eq!(load(&d).port, 9100);
        let _ = std::fs::remove_dir_all(&d);
    }
}
