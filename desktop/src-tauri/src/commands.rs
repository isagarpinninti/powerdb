//! What the interface can ask the app to do. Everything blocking runs off the interface thread.

use crate::api;
use crate::config::{self, Config};
use crate::manager::{Launch, Manager, Status, quiet};
use serde::Serialize;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Manager as _, State, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_autostart::ManagerExt;

pub struct AppState {
    pub manager: Manager,
    pub config: Mutex<Config>,
    pub config_dir: PathBuf,
    pub binary: PathBuf,
    pub keys: config::KeyCache,
}

impl AppState {
    fn cfg(&self) -> Config {
        self.config
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    fn set_cfg(&self, c: Config) -> Result<(), String> {
        config::save(&self.config_dir, &c)?;
        *self.config.lock().unwrap_or_else(|e| e.into_inner()) = c;
        Ok(())
    }
    fn launch(&self) -> Launch {
        let c = self.cfg();
        Launch {
            binary: self.binary.clone(),
            data: PathBuf::from(c.data_dir),
            port: c.port,
        }
    }
    pub fn start(&self) -> Result<(), String> {
        let c = self.cfg();
        if !c.configured {
            return Err("set up a database first".into());
        }
        self.manager.start(&self.launch())
    }
}

/// Runs the bundled `powerdb` CLI and returns what it printed.
fn cli(state: &AppState, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new(&state.binary);
    cmd.args(args);
    quiet(&mut cmd);
    let out = cmd
        .output()
        .map_err(|e| format!("cannot run {}: {e}", state.binary.display()))?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if out.status.success() {
        Ok(text)
    } else {
        Err(text.trim().to_string())
    }
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| e.to_string())?
}

#[derive(Serialize)]
pub struct Snapshot {
    config: Config,
    status: Status,
    app_version: String,
    server_version: String,
    has_key: bool,
    default_data_dir: String,
    platform: &'static str,
    start_at_login: bool,
}

#[tauri::command]
pub async fn snapshot(app: AppHandle, state: State<'_, AppState>) -> Result<Snapshot, String> {
    let t0 = std::time::Instant::now();
    let c = state.cfg();
    let status = state.manager.status();
    let binary = state.binary.clone();
    let server_version = blocking(move || {
        let mut cmd = Command::new(binary);
        cmd.arg("--version");
        quiet(&mut cmd);
        Ok(cmd
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default())
    })
    .await?;
    crate::ui_log(
        &app,
        &format!("rust snapshot: done after {:?}", t0.elapsed()),
    );
    // only what is already in memory: the credential store is not touched while the interface is being drawn
    let has_key = state
        .keys
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_some();
    Ok(Snapshot {
        has_key,
        config: c,
        status,
        app_version: env!("CARGO_PKG_VERSION").into(),
        server_version,
        default_data_dir: config::default_data_dir().display().to_string(),
        platform: std::env::consts::OS,
        start_at_login: app.autolaunch().is_enabled().unwrap_or(false),
    })
}

fn check_port(port: u16) -> Result<(), String> {
    if port < 1024 {
        return Err("choose a port from 1024 up".into());
    }
    if api::healthy(port) {
        return Err(format!(
            "port {port} is already answering as a server; choose another"
        ));
    }
    std::net::TcpListener::bind(("127.0.0.1", port))
        .map(|_| ())
        .map_err(|e| format!("port {port} is not available: {e}"))
}

/// Creates a new database in an empty folder and returns its administrator key (shown once).
#[tauri::command]
pub async fn setup_new(
    app: AppHandle,
    state: State<'_, AppState>,
    data_dir: String,
    port: u16,
    start_at_login: bool,
) -> Result<String, String> {
    let s = state.inner();
    check_port(port)?;
    let dir = PathBuf::from(&data_dir);
    if dir.exists()
        && dir
            .read_dir()
            .map(|mut d| d.next().is_some())
            .unwrap_or(false)
    {
        return Err(format!(
            "{data_dir} is not empty. Choose a new or empty folder, or use 'Open an existing database'."
        ));
    }
    let (binary, d2) = (s.binary.clone(), data_dir.clone());
    let key = blocking(move || {
        let mut cmd = Command::new(&binary);
        cmd.args(["init", &d2])
            .env_remove("POWERDB_ADMIN_EMAIL")
            .env_remove("POWERDB_ADMIN_PASSWORD");
        quiet(&mut cmd);
        let out = cmd.output().map_err(|e| e.to_string())?;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        if !out.status.success() {
            return Err(text.trim().to_string());
        }
        text.split_whitespace()
            .find(|w| w.starts_with("pdb_live_"))
            .map(str::to_string)
            .ok_or_else(|| "the server did not print an administrator key".to_string())
    })
    .await?;
    config::store_key(&data_dir, &key)?;
    config::remember_key(&s.keys, &data_dir, &key);
    s.set_cfg(Config {
        configured: true,
        data_dir,
        port,
        auto_start_server: true,
    })?;
    let al = app.autolaunch();
    let _ = if start_at_login {
        al.enable()
    } else {
        al.disable()
    };
    Ok(key)
}

#[tauri::command]
pub async fn setup_existing(
    app: AppHandle,
    state: State<'_, AppState>,
    data_dir: String,
    port: u16,
    key: String,
    start_at_login: bool,
) -> Result<(), String> {
    let s = state.inner();
    check_port(port)?;
    if !PathBuf::from(&data_dir).join("FORMAT").is_file() {
        return Err(format!(
            "{data_dir} does not look like a PowerDB database (no FORMAT file)"
        ));
    }
    if !key.trim().starts_with("pdb_live_") {
        return Err("an administrator key starts with pdb_live_".into());
    }
    config::store_key(&data_dir, key.trim())?;
    config::remember_key(&s.keys, &data_dir, key.trim());
    s.set_cfg(Config {
        configured: true,
        data_dir,
        port,
        auto_start_server: true,
    })?;
    let al = app.autolaunch();
    let _ = if start_at_login {
        al.enable()
    } else {
        al.disable()
    };
    Ok(())
}

#[tauri::command]
pub async fn start_server(state: State<'_, AppState>) -> Result<(), String> {
    let s = state.inner();
    s.start()
}

#[tauri::command]
pub async fn stop_server(state: State<'_, AppState>) -> Result<(), String> {
    let m = state.manager.clone();
    blocking(move || {
        m.stop(Duration::from_secs(30));
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn restart_server(state: State<'_, AppState>) -> Result<(), String> {
    let (m, launch) = (state.manager.clone(), state.launch());
    blocking(move || {
        m.stop(Duration::from_secs(30));
        m.start(&launch)
    })
    .await
}

#[tauri::command]
pub async fn save_settings(
    app: AppHandle,
    state: State<'_, AppState>,
    port: u16,
    auto_start_server: bool,
    start_at_login: bool,
) -> Result<(), String> {
    let s = state.inner();
    let old = s.cfg();
    if port != old.port {
        let running = !matches!(
            s.manager.status().phase,
            crate::manager::Phase::Stopped | crate::manager::Phase::Failed
        );
        if !running {
            check_port(port)?;
        }
    }
    s.set_cfg(Config {
        port,
        auto_start_server,
        ..old.clone()
    })?;
    let al = app.autolaunch();
    let _ = if start_at_login {
        al.enable()
    } else {
        al.disable()
    };
    if port != old.port
        && !matches!(
            s.manager.status().phase,
            crate::manager::Phase::Stopped | crate::manager::Phase::Failed
        )
    {
        let (m, launch) = (s.manager.clone(), s.launch());
        blocking(move || {
            m.stop(Duration::from_secs(30));
            m.start(&launch)
        })
        .await?;
    }
    Ok(())
}

#[tauri::command]
pub fn logs(state: State<'_, AppState>) -> Vec<String> {
    state.manager.logs()
}

/// Opens the dashboard in its own window, already signed in (the key travels in the URL fragment, which no server ever receives).
#[tauri::command]
pub async fn open_dashboard(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let c = state.cfg();
    let (keys, dir) = (state.keys.clone(), c.data_dir.clone());
    let key = blocking(move || config::key_for(&keys, &dir, Duration::from_secs(25))).await?;
    let url = format!("http://127.0.0.1:{}/#key={key}", c.port)
        .parse()
        .map_err(|e| format!("{e}"))?;
    if let Some(w) = app.get_webview_window("dashboard") {
        let _ = w.close();
    }
    WebviewWindowBuilder::new(&app, "dashboard", WebviewUrl::External(url))
        .title("PowerDB Dashboard")
        .inner_size(1280.0, 820.0)
        .min_inner_size(900.0, 600.0)
        .build()
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn api_call(
    state: &AppState,
    method: &str,
    path: &str,
    body: Option<&str>,
    timeout: Duration,
) -> Result<String, String> {
    let c = state.cfg();
    let key = config::key_for(&state.keys, &c.data_dir, Duration::from_secs(25))?;
    let r = api::request(c.port, method, path, Some(&key), body, timeout)?;
    if (200..300).contains(&r.status) {
        Ok(r.body)
    } else {
        let msg = serde_json::from_str::<serde_json::Value>(&r.body)
            .ok()
            .and_then(|v| v["error"]["message"].as_str().map(str::to_string));
        Err(msg.unwrap_or_else(|| format!("the server answered {}", r.status)))
    }
}

#[tauri::command]
pub async fn server_stats(state: State<'_, AppState>) -> Result<String, String> {
    let (c, binary, manager, config_dir, keys) = (
        state
            .config
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone(),
        state.binary.clone(),
        state.manager.clone(),
        state.config_dir.clone(),
        state.keys.clone(),
    );
    blocking(move || {
        let st = AppState {
            manager,
            config: Mutex::new(c),
            config_dir,
            binary,
            keys,
        };
        api_call(&st, "GET", "/api/v1/stats", None, Duration::from_secs(5))
    })
    .await
}

fn with_state<T: Send + 'static>(
    state: &AppState,
    f: impl FnOnce(&AppState) -> Result<T, String> + Send + 'static,
) -> impl std::future::Future<Output = Result<T, String>> {
    let st = AppState {
        manager: state.manager.clone(),
        config: Mutex::new(state.cfg()),
        config_dir: state.config_dir.clone(),
        binary: state.binary.clone(),
        keys: state.keys.clone(),
    };
    blocking(move || f(&st))
}

#[tauri::command]
pub async fn list_backups(state: State<'_, AppState>) -> Result<String, String> {
    with_state(state.inner(), |s| {
        api_call(
            s,
            "GET",
            "/api/v1/maintenance/backups",
            None,
            Duration::from_secs(10),
        )
    })
    .await
}

#[tauri::command]
pub async fn create_backup(state: State<'_, AppState>, deep: bool) -> Result<String, String> {
    let body = format!("{{\"deep\":{deep}}}");
    with_state(state.inner(), move |s| {
        api_call(
            s,
            "POST",
            "/api/v1/maintenance/backups",
            Some(&body),
            Duration::from_secs(3600),
        )
    })
    .await
}

/// The integrity check needs the database to itself, so it stops the server for its duration and starts it again.
#[tauri::command]
pub async fn run_check(state: State<'_, AppState>, quick: bool) -> Result<String, String> {
    let (m, launch) = (state.manager.clone(), state.launch());
    let was_running = !matches!(
        m.status().phase,
        crate::manager::Phase::Stopped | crate::manager::Phase::Failed
    );
    let data = launch.data.display().to_string();
    let st = AppState {
        manager: m.clone(),
        config: Mutex::new(state.cfg()),
        config_dir: state.config_dir.clone(),
        binary: state.binary.clone(),
        keys: state.keys.clone(),
    };
    blocking(move || {
        m.stop(Duration::from_secs(30));
        let mut args = vec!["check", "--data", &data];
        if quick {
            args.push("--quick");
        }
        let result = cli(&st, &args);
        if was_running {
            let _ = m.start(&launch);
        }
        result
    })
    .await
}

/// Restores one of the server's own backups into a new folder, keeping this database's secret so encrypted collections stay readable.
#[tauri::command]
pub async fn restore_backup(
    state: State<'_, AppState>,
    name: String,
    target: String,
) -> Result<String, String> {
    if name.is_empty() || name.contains(['/', '\\']) || name.starts_with('.') {
        return Err("invalid backup name".into());
    }
    let cfg = state.cfg();
    let from = PathBuf::from(&cfg.data_dir).join("backups").join(&name);
    with_state(state.inner(), move |s| {
        let out = cli(
            s,
            &[
                "restore",
                "--from",
                &from.display().to_string(),
                "--data",
                &target,
            ],
        )?;
        let secret = PathBuf::from(&cfg.data_dir).join("secret.key");
        if secret.is_file() && !PathBuf::from(&target).join("secret.key").exists() {
            std::fs::copy(&secret, PathBuf::from(&target).join("secret.key"))
                .map_err(|e| e.to_string())?;
        }
        Ok(out)
    })
    .await
}

/// Switches the app to a different database folder (for example one just restored) and keeps using the same administrator key.
#[tauri::command]
pub async fn use_database(state: State<'_, AppState>, data_dir: String) -> Result<(), String> {
    let s = state.inner();
    let old = s.cfg();
    let (keys, dir) = (s.keys.clone(), old.data_dir.clone());
    let key = blocking(move || config::key_for(&keys, &dir, Duration::from_secs(25))).await?;
    let m = s.manager.clone();
    blocking(move || {
        m.stop(Duration::from_secs(30));
        Ok(())
    })
    .await?;
    config::store_key(&data_dir, &key)?;
    config::remember_key(&s.keys, &data_dir, &key);
    s.set_cfg(Config { data_dir, ..old })?;
    s.start()
}

#[tauri::command]
pub async fn reveal_key(state: State<'_, AppState>) -> Result<String, String> {
    let (keys, dir) = (state.keys.clone(), state.cfg().data_dir);
    blocking(move || config::key_for(&keys, &dir, Duration::from_secs(25))).await
}

/// The interface reports what it is doing, and any error, so a blank window can be diagnosed: a line in `frontend.log`.
#[tauri::command]
pub fn frontend_log(app: AppHandle, level: String, msg: String) {
    crate::ui_log(&app, &format!("ui {level}: {msg}"));
}
