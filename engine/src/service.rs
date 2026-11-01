//! Running PowerDB as an operating-system service: a Windows Service, a launchd job (macOS) or a systemd unit (Linux).
//!
//! The text of the unit and the job description is produced by plain functions so it can be tested everywhere; the
//! parts that talk to the service manager are per platform, and only the platform they are written for can run them.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command;

#[allow(dead_code)] // the Windows service name; Linux and macOS use their own
pub const NAME: &str = "PowerDB";
pub const LABEL: &str = "com.powerdb.server";

#[derive(Debug, Clone)]
pub struct Spec {
    pub exe: PathBuf,
    pub data: PathBuf,
    pub addr: Option<SocketAddr>,
    /// Account the service runs as (Linux and macOS daemons). None: the platform's own isolation (systemd DynamicUser, LocalService).
    pub run_as: Option<String>,
}

impl Spec {
    fn serve_args(&self) -> Vec<String> {
        let mut a = vec![
            "serve".to_string(),
            "--data".into(),
            self.data.display().to_string(),
        ];
        if let Some(addr) = self.addr {
            a.push("--addr".into());
            a.push(addr.to_string());
        }
        a
    }
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// A systemd unit. Without `run_as` the service uses a transient system user (`DynamicUser`) whose home is `/var/lib/powerdb`;
/// that only works for the default data directory, so a different directory needs an explicit account that owns it.
#[allow(dead_code)] // only installed on Linux, but tested everywhere
pub fn systemd_unit(s: &Spec) -> String {
    let args = s.serve_args().join(" ");
    let account = match &s.run_as {
        Some(u) => format!("User={u}\n"),
        None => "DynamicUser=yes\nStateDirectory=powerdb\n".to_string(),
    };
    let write_path = if s.run_as.is_some() {
        format!("ReadWritePaths={}\n", s.data.display())
    } else {
        String::new()
    };
    format!(
        "[Unit]\nDescription=PowerDB\nAfter=network-online.target\nWants=network-online.target\n\n\
         [Service]\n{account}ExecStart={exe} {args}\nRestart=on-failure\nTimeoutStopSec=30\n\
         NoNewPrivileges=true\nProtectSystem=strict\nProtectHome=true\nPrivateTmp=true\n{write_path}\n\
         [Install]\nWantedBy=multi-user.target\n",
        exe = s.exe.display()
    )
}

pub fn launchd_plist(s: &Spec) -> String {
    let mut args = format!(
        "    <string>{}</string>\n",
        xml(&s.exe.display().to_string())
    );
    for a in s.serve_args() {
        args.push_str(&format!("    <string>{}</string>\n", xml(&a)));
    }
    let user = s
        .run_as
        .as_ref()
        .map(|u| format!("  <key>UserName</key><string>{}</string>\n", xml(u)))
        .unwrap_or_default();
    let log = xml(&s.data.join("powerdb.log").display().to_string());
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\"><dict>\n  <key>Label</key><string>{LABEL}</string>\n  <key>ProgramArguments</key><array>\n{args}  </array>\n{user}\
         \x20 <key>RunAtLoad</key><true/>\n  <key>KeepAlive</key><true/>\n  <key>StandardOutPath</key><string>{log}</string>\n  <key>StandardErrorPath</key><string>{log}</string>\n</dict></plist>\n"
    )
}

/// Where a service keeps its data when `--data` is not given.
pub fn default_data_dir(per_user: bool) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(std::env::var("ProgramData").unwrap_or_else(|_| r"C:\ProgramData".into()))
            .join("PowerDB")
    } else if cfg!(target_os = "macos") {
        if per_user {
            PathBuf::from(std::env::var("HOME").unwrap_or_default())
                .join("Library/Application Support/PowerDB")
        } else {
            PathBuf::from("/usr/local/var/powerdb")
        }
    } else {
        PathBuf::from("/var/lib/powerdb")
    }
}

fn run(cmd: &mut Command) -> Result<String, String> {
    let out = cmd
        .output()
        .map_err(|e| format!("cannot run {:?}: {e}", cmd.get_program()))?;
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

fn need_root(e: std::io::Error, what: &Path) -> String {
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        format!(
            "cannot write {}: run this command as an administrator (sudo)",
            what.display()
        )
    } else {
        format!("{}: {e}", what.display())
    }
}

// ---- Linux (systemd) ----

#[cfg(target_os = "linux")]
const UNIT_PATH: &str = "/etc/systemd/system/powerdb.service";

#[cfg(target_os = "linux")]
pub fn install(s: &Spec, print: bool) -> Result<String, String> {
    let unit = systemd_unit(s);
    if print {
        return Ok(unit);
    }
    std::fs::write(UNIT_PATH, unit).map_err(|e| need_root(e, Path::new(UNIT_PATH)))?;
    run(Command::new("systemctl").arg("daemon-reload"))?;
    run(Command::new("systemctl").args(["enable", "--now", "powerdb"]))?;
    Ok(format!(
        "Installed and started the powerdb service ({UNIT_PATH})."
    ))
}
#[cfg(target_os = "linux")]
pub fn uninstall() -> Result<String, String> {
    let _ = run(Command::new("systemctl").args(["disable", "--now", "powerdb"]));
    match std::fs::remove_file(UNIT_PATH) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(need_root(e, Path::new(UNIT_PATH))),
    }
    run(Command::new("systemctl").arg("daemon-reload"))?;
    Ok("Removed the powerdb service. Its data was not touched.".into())
}
#[cfg(target_os = "linux")]
pub fn control(action: &str) -> Result<String, String> {
    match action {
        "status" => run(Command::new("systemctl").args(["is-active", "powerdb"]))
            .map(|s| s.trim().to_string())
            .or_else(|e| Ok(e)),
        a => run(Command::new("systemctl").args([a, "powerdb"])).map(|_| format!("{a} requested")),
    }
}

// ---- macOS (launchd) ----

#[cfg(target_os = "macos")]
fn plist_path(per_user: bool) -> PathBuf {
    if per_user {
        PathBuf::from(std::env::var("HOME").unwrap_or_default())
            .join(format!("Library/LaunchAgents/{LABEL}.plist"))
    } else {
        PathBuf::from(format!("/Library/LaunchDaemons/{LABEL}.plist"))
    }
}
#[cfg(target_os = "macos")]
fn domain(per_user: bool) -> Result<String, String> {
    if per_user {
        let uid = run(Command::new("id").arg("-u"))?;
        Ok(format!("gui/{}", uid.trim()))
    } else {
        Ok("system".into())
    }
}
#[cfg(target_os = "macos")]
pub fn install(s: &Spec, print: bool, per_user: bool) -> Result<String, String> {
    let plist = launchd_plist(s);
    if print {
        return Ok(plist);
    }
    let path = plist_path(per_user);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::create_dir_all(&s.data);
    std::fs::write(&path, plist).map_err(|e| need_root(e, &path))?;
    let dom = domain(per_user)?;
    let _ = run(Command::new("launchctl").args(["bootout", &format!("{dom}/{LABEL}")]));
    run(Command::new("launchctl").args(["bootstrap", &dom, &path.display().to_string()]))?;
    Ok(format!(
        "Installed and started {LABEL} ({}).",
        path.display()
    ))
}
#[cfg(target_os = "macos")]
pub fn uninstall(per_user: bool) -> Result<String, String> {
    let dom = domain(per_user)?;
    let _ = run(Command::new("launchctl").args(["bootout", &format!("{dom}/{LABEL}")]));
    let path = plist_path(per_user);
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(need_root(e, &path)),
    }
    Ok("Removed the service. Its data was not touched.".into())
}
#[cfg(target_os = "macos")]
pub fn control(action: &str, per_user: bool) -> Result<String, String> {
    let dom = domain(per_user)?;
    let target = format!("{dom}/{LABEL}");
    match action {
        "start" => run(Command::new("launchctl").args(["kickstart", "-k", &target]))
            .or_else(|_| {
                run(Command::new("launchctl").args([
                    "bootstrap",
                    &dom,
                    &plist_path(per_user).display().to_string(),
                ]))
            })
            .map(|_| "start requested".into()),
        "stop" => run(Command::new("launchctl").args(["bootout", &target]))
            .map(|_| "stopped (it starts again at the next login or boot)".into()),
        _ => Ok(
            match run(Command::new("launchctl").args(["print", &target])) {
                Ok(t) if t.contains("state = running") => "running".into(),
                Ok(_) => "loaded, not running".into(),
                Err(_) => "not loaded".into(),
            },
        ),
    }
}

// ---- Windows (Service Control Manager) ----

#[cfg(windows)]
pub fn install(s: &Spec, print: bool) -> Result<String, String> {
    use std::os::windows::process::CommandExt;
    let mut args = format!(
        "\"{}\" service run --data \"{}\"",
        s.exe.display(),
        s.data.display()
    );
    if let Some(a) = s.addr {
        args.push_str(&format!(" --addr {a}"));
    }
    if print {
        return Ok(format!("sc create {NAME} binPath= {args}"));
    }
    std::fs::create_dir_all(&s.data).map_err(|e| format!("{}: {e}", s.data.display()))?;
    // LocalService is a low-privilege account; it needs to own the data directory (S-1-5-19)
    run(Command::new("icacls")
        .arg(&s.data)
        .args(["/grant", "*S-1-5-19:(OI)(CI)F"]))?;
    run(Command::new("sc.exe")
        .raw_arg(format!("create {NAME} binPath= \"{}\" start= auto obj= \"NT AUTHORITY\\LocalService\" DisplayName= \"PowerDB Server\"", args.replace('"', "\\\""))))?;
    let _ = run(Command::new("sc.exe").args([
        "description",
        NAME,
        "PowerDB: one database for records and files",
    ]));
    let _ = run(Command::new("sc.exe").args([
        "failure",
        NAME,
        "reset=",
        "86400",
        "actions=",
        "restart/5000/restart/5000/restart/30000",
    ]));
    run(Command::new("sc.exe").args(["start", NAME]))?;
    Ok(format!("Installed and started the {NAME} service."))
}
#[cfg(windows)]
pub fn uninstall() -> Result<String, String> {
    let _ = run(Command::new("sc.exe").args(["stop", NAME]));
    run(Command::new("sc.exe").args(["delete", NAME]))?;
    Ok("Removed the PowerDB service. Its data was not touched.".into())
}
#[cfg(windows)]
pub fn control(action: &str) -> Result<String, String> {
    match action {
        "status" => run(Command::new("sc.exe").args(["query", NAME]))
            .map(|t| {
                if t.contains("RUNNING") {
                    "running".into()
                } else if t.contains("STOPPED") {
                    "stopped".into()
                } else {
                    t.trim().to_string()
                }
            })
            .or_else(|_| Ok("not installed".into())),
        a => run(Command::new("sc.exe").args([a, NAME])).map(|_| format!("{a} requested")),
    }
}

/// Entry point the Service Control Manager runs. Blocks until the service stops.
#[cfg(windows)]
pub fn run_as_service(
    runtime: tokio::runtime::Handle,
    data: PathBuf,
    addr: Option<SocketAddr>,
) -> Result<(), String> {
    use std::ffi::OsString;
    use std::sync::{Mutex, OnceLock};
    use std::time::Duration;
    use windows_service::service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType,
    };
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
    use windows_service::{define_windows_service, service_dispatcher};

    static CTX: OnceLock<(tokio::runtime::Handle, PathBuf, Option<SocketAddr>)> = OnceLock::new();
    let _ = CTX.set((runtime, data, addr));

    define_windows_service!(ffi_service_main, service_main);
    fn service_main(_args: Vec<OsString>) {
        let (handle, data, addr) = CTX
            .get()
            .cloned()
            .expect("context set before the dispatcher starts");
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let tx = Mutex::new(Some(tx));
        let status = service_control_handler::register(NAME, move |control| match control {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                if let Some(t) = tx.lock().ok().and_then(|mut g| g.take()) {
                    let _ = t.send(());
                }
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        });
        let Ok(status) = status else { return };
        let report = |state, accepted, code: u32, hint| {
            let _ = status.set_service_status(ServiceStatus {
                service_type: ServiceType::OWN_PROCESS,
                current_state: state,
                controls_accepted: accepted,
                exit_code: ServiceExitCode::Win32(code),
                checkpoint: 0,
                wait_hint: hint,
                process_id: None,
            });
        };
        report(
            ServiceState::StartPending,
            ServiceControlAccept::empty(),
            0,
            Duration::from_secs(10),
        );
        report(
            ServiceState::Running,
            ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
            0,
            Duration::default(),
        );
        let result = handle.block_on(crate::serve(
            data,
            powerdb::config::Overrides {
                addr,
                ..Default::default()
            },
            false,
            async move {
                let _ = rx.await;
            },
        ));
        let code = if let Err(e) = &result {
            tracing::error!("service stopped with an error: {e}");
            1
        } else {
            0
        };
        report(
            ServiceState::Stopped,
            ServiceControlAccept::empty(),
            code,
            Duration::default(),
        );
    }
    service_dispatcher::start(NAME, ffi_service_main).map_err(|e| e.to_string())
}
#[cfg(test)]
mod tests {
    use super::*;

    fn spec(run_as: Option<&str>) -> Spec {
        Spec {
            exe: "/usr/local/bin/powerdb".into(),
            data: "/var/lib/powerdb".into(),
            addr: Some("0.0.0.0:8443".parse().unwrap()),
            run_as: run_as.map(String::from),
        }
    }

    #[test]
    fn the_systemd_unit_runs_the_server_and_isolates_it() {
        let u = systemd_unit(&spec(None));
        assert!(u.contains(
            "ExecStart=/usr/local/bin/powerdb serve --data /var/lib/powerdb --addr 0.0.0.0:8443"
        ));
        assert!(
            u.contains("DynamicUser=yes")
                && u.contains("StateDirectory=powerdb")
                && u.contains("NoNewPrivileges=true")
        );
        let u = systemd_unit(&spec(Some("db")));
        assert!(
            u.contains("User=db")
                && u.contains("ReadWritePaths=/var/lib/powerdb")
                && !u.contains("DynamicUser")
        );
    }

    #[test]
    fn the_launchd_plist_is_well_formed_and_escapes_paths() {
        let mut s = spec(Some("_powerdb"));
        s.data = "/Users/a&b/<data>".into();
        let p = launchd_plist(&s);
        assert!(
            p.contains("<string>serve</string>")
                && p.contains("<key>UserName</key><string>_powerdb</string>")
        );
        assert!(p.contains("/Users/a&amp;b/&lt;data&gt;") && !p.contains("a&b"));
        assert!(p.contains("<key>KeepAlive</key><true/>") && p.contains(LABEL));
    }
}
