pub mod api;
pub mod commands;
pub mod config;
pub mod manager;

use commands::AppState;
use manager::{Event, Manager, Phase, Status};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager as _, RunEvent, WindowEvent};
use tauri_plugin_autostart::MacosLauncher;

/// The `powerdb` server that ships inside the app, next to the app's own executable (or POWERDB_BIN while developing).
fn server_binary() -> PathBuf {
    if let Some(p) = std::env::var_os("POWERDB_BIN") {
        return PathBuf::from(p);
    }
    let exe = std::env::current_exe().unwrap_or_default();
    exe.parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join(if cfg!(windows) {
            "powerdb.exe"
        } else {
            "powerdb"
        })
}

/// Appends a line to `frontend.log` in the app's settings folder (kept small), and to stderr.
pub fn ui_log(app: &AppHandle, line: &str) {
    eprintln!("{line}");
    if let Ok(dir) = app.path().app_config_dir() {
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("frontend.log");
        if std::fs::metadata(&path)
            .map(|m| m.len() > 200_000)
            .unwrap_or(false)
        {
            let _ = std::fs::rename(&path, dir.join("frontend.log.1"));
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            use std::io::Write;
            let _ = writeln!(f, "{line}");
        }
    }
}

struct TrayItems {
    status: MenuItem<tauri::Wry>,
    toggle: MenuItem<tauri::Wry>,
    dashboard: MenuItem<tauri::Wry>,
}

fn phase_label(s: &Status) -> &'static str {
    match s.phase {
        Phase::Stopped => "Server stopped",
        Phase::Starting => "Server starting…",
        Phase::Running => "Server running",
        Phase::Stopping => "Server stopping…",
        Phase::Failed => "Server failed",
    }
}

fn update_tray(app: &AppHandle, s: &Status) {
    if let Some(t) = app.try_state::<TrayItems>() {
        let _ = t.status.set_text(phase_label(s));
        let running = matches!(s.phase, Phase::Running | Phase::Starting);
        let _ = t.toggle.set_text(if running {
            "Stop Server"
        } else {
            "Start Server"
        });
        let _ = t
            .toggle
            .set_enabled(!matches!(s.phase, Phase::Starting | Phase::Stopping));
        let _ = t.dashboard.set_enabled(s.phase == Phase::Running);
    }
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main(app)
        }))
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec!["--hidden"]),
        ))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let handle = app.handle().clone();
            let config_dir = app.path().app_config_dir()?;
            let cfg = config::load(&config_dir);
            let emit_handle = handle.clone();
            let manager = Manager::new(move |ev| match ev {
                Event::Status(s) => {
                    update_tray(&emit_handle, &s);
                    let _ = emit_handle.emit("server-status", &s);
                }
                Event::Log(l) => {
                    let _ = emit_handle.emit("server-log", l);
                }
            });
            let state = AppState {
                manager,
                config: Mutex::new(cfg.clone()),
                config_dir,
                binary: server_binary(),
                keys: Default::default(),
            };

            // tray
            let status_item =
                MenuItem::with_id(app, "status", "Server stopped", false, None::<&str>)?;
            let toggle = MenuItem::with_id(app, "toggle", "Start Server", true, None::<&str>)?;
            let dashboard =
                MenuItem::with_id(app, "dashboard", "Open Dashboard", false, None::<&str>)?;
            let manager_item =
                MenuItem::with_id(app, "manager", "Open PowerDB Manager", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit PowerDB", true, None::<&str>)?;
            let menu = Menu::with_items(
                app,
                &[
                    &status_item,
                    &PredefinedMenuItem::separator(app)?,
                    &toggle,
                    &dashboard,
                    &manager_item,
                    &PredefinedMenuItem::separator(app)?,
                    &quit,
                ],
            )?;
            app.manage(TrayItems {
                status: status_item,
                toggle,
                dashboard,
            });
            let mut tray = TrayIconBuilder::with_id("main")
                .menu(&menu)
                .tooltip("PowerDB")
                .show_menu_on_left_click(true);
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.on_menu_event(|app, event| match event.id.as_ref() {
                "manager" => show_main(app),
                "dashboard" => {
                    let app = app.clone();
                    tauri::async_runtime::spawn(async move {
                        let st = app.state::<AppState>();
                        let _ = commands::open_dashboard(app.clone(), st).await;
                    });
                }
                "toggle" => {
                    let app = app.clone();
                    tauri::async_runtime::spawn_blocking(move || {
                        let st = app.state::<AppState>();
                        match st.manager.status().phase {
                            Phase::Running | Phase::Starting => {
                                st.manager.stop(Duration::from_secs(30))
                            }
                            _ => {
                                let _ = st.start();
                            }
                        }
                    });
                }
                "quit" => app.exit(0),
                _ => {}
            })
            .build(app)?;

            app.manage(state);
            if cfg.configured && cfg.auto_start_server {
                let _ = app.state::<AppState>().start();
            }
            let hidden = std::env::args().any(|a| a == "--hidden");
            if !hidden || !cfg.configured {
                show_main(&handle);
            }
            Ok(())
        })
        .on_page_load(|webview, payload| {
            ui_log(
                webview.app_handle(),
                &format!("page {:?}: {}", payload.event(), payload.url()),
            );
        })
        .on_window_event(|window, event| {
            // Closing the Manager window leaves the server running in the tray, as a database server should.
            if let WindowEvent::CloseRequested { api, .. } = event
                && window.label() == "main"
            {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::snapshot,
            commands::setup_new,
            commands::setup_existing,
            commands::start_server,
            commands::stop_server,
            commands::restart_server,
            commands::save_settings,
            commands::logs,
            commands::open_dashboard,
            commands::server_stats,
            commands::list_backups,
            commands::create_backup,
            commands::run_check,
            commands::restore_backup,
            commands::use_database,
            commands::reveal_key,
            commands::frontend_log,
        ])
        .build(tauri::generate_context!())
        .expect("error while building PowerDB");

    app.run(|app, event| match event {
        // Quit stops the server cleanly first, so the data directory is closed properly.
        RunEvent::Exit => {
            if let Some(st) = app.try_state::<AppState>() {
                st.manager.stop(Duration::from_secs(30));
            }
        }
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { .. } => show_main(app),
        _ => {}
    });
}
