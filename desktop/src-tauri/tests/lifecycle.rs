//! The server manager against the real `powerdb` binary (POWERDB_BIN, or ../../target/release/powerdb).
//! These are the behaviours the app depends on: it starts and stops the server cleanly, it notices when the server dies,
//! it refuses a busy port or a data folder already in use, and a manager that disappears takes the server with it.

use powerdb_desktop_lib::manager::{Event, Launch, Manager, Phase};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn binary() -> PathBuf {
    if let Some(p) = std::env::var_os("POWERDB_BIN") {
        return PathBuf::from(p);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/release")
        .join(if cfg!(windows) {
            "powerdb.exe"
        } else {
            "powerdb"
        })
}

fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

struct Data(PathBuf);
impl Data {
    fn new() -> Data {
        let d = std::env::temp_dir().join(format!(
            "pdb-desktop-test-{}-{}",
            std::process::id(),
            free_port()
        ));
        let out = Command::new(binary())
            .arg("init")
            .arg(&d)
            .output()
            .expect("powerdb must be built first: cargo build --release");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        Data(d)
    }
    fn launch(&self, port: u16) -> Launch {
        Launch {
            binary: binary(),
            data: self.0.clone(),
            port,
        }
    }
}
impl Drop for Data {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn manager() -> (Manager, Arc<Mutex<Vec<Phase>>>) {
    let phases = Arc::new(Mutex::new(Vec::new()));
    let p = phases.clone();
    let m = Manager::new(move |e| {
        if let Event::Status(s) = e {
            let mut v = p.lock().unwrap();
            if v.last() != Some(&s.phase) {
                v.push(s.phase);
            }
        }
    });
    (m, phases)
}

fn wait_for(m: &Manager, want: Phase, secs: u64) {
    let until = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < until {
        if m.status().phase == want {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!(
        "timed out waiting for {want:?}; status {:?}; log tail {:?}",
        m.status(),
        m.logs().iter().rev().take(5).collect::<Vec<_>>()
    );
}

fn port_open(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_millis(300))
        .is_ok()
}

#[test]
fn start_run_stop_reports_every_step_and_frees_the_port() {
    let (data, port) = (Data::new(), free_port());
    let (m, phases) = manager();
    m.start(&data.launch(port)).unwrap();
    wait_for(&m, Phase::Running, 30);
    assert!(powerdb_desktop_lib::api::healthy(port));
    assert!(m.status().pid.is_some() && m.status().since.is_some());
    assert!(
        m.logs().iter().any(|l| l.contains("listening")),
        "the server's log is captured: {:?}",
        m.logs()
    );
    assert!(
        m.start(&data.launch(port)).is_err(),
        "starting twice is refused"
    );
    m.stop(Duration::from_secs(30));
    wait_for(&m, Phase::Stopped, 5);
    assert!(!port_open(port), "the port is released");
    m.stop(Duration::from_secs(1)); // stopping a stopped server is harmless
    let seen = phases.lock().unwrap().clone();
    assert_eq!(
        seen,
        vec![
            Phase::Starting,
            Phase::Running,
            Phase::Stopping,
            Phase::Stopped
        ],
        "{seen:?}"
    );
}

#[test]
fn a_clean_stop_lets_the_server_finish_and_release_the_data_folder() {
    let (data, port) = (Data::new(), free_port());
    let (m, _) = manager();
    m.start(&data.launch(port)).unwrap();
    wait_for(&m, Phase::Running, 30);
    m.stop(Duration::from_secs(30));
    assert!(
        m.logs().iter().any(|l| l.contains("shutting down")),
        "it shut down by itself, not by being killed: {:?}",
        m.logs()
    );
    // the folder's lock is free: an integrity check can open it
    let out = Command::new(binary())
        .args(["check", "--quick", "--data"])
        .arg(&data.0)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn it_restarts_repeatedly() {
    let (data, port) = (Data::new(), free_port());
    let (m, _) = manager();
    for _ in 0..3 {
        m.start(&data.launch(port)).unwrap();
        wait_for(&m, Phase::Running, 30);
        m.stop(Duration::from_secs(30));
        wait_for(&m, Phase::Stopped, 5);
    }
}

#[test]
fn a_server_that_dies_is_reported_as_failed_with_its_last_words() {
    let (data, port) = (Data::new(), free_port());
    let (m, _) = manager();
    // take the port with something that is not a PowerDB: the server cannot bind and exits
    let _squatter = TcpListener::bind(("127.0.0.1", port)).unwrap();
    m.start(&data.launch(port)).unwrap();
    wait_for(&m, Phase::Failed, 30);
    let msg = m.status().message.unwrap();
    assert!(msg.contains("exited unexpectedly"), "{msg}");
}

#[test]
fn a_port_already_answering_as_powerdb_is_refused_up_front() {
    let (a, b, port) = (Data::new(), Data::new(), free_port());
    let (m1, _) = manager();
    let (m2, _) = manager();
    m1.start(&a.launch(port)).unwrap();
    wait_for(&m1, Phase::Running, 30);
    let err = m2.start(&b.launch(port)).unwrap_err();
    assert!(err.contains("already answering"), "{err}");
    m1.stop(Duration::from_secs(30));
}

#[test]
fn the_same_data_folder_cannot_be_served_twice() {
    let (data, p1, p2) = (Data::new(), free_port(), free_port());
    let (m1, _) = manager();
    let (m2, _) = manager();
    m1.start(&data.launch(p1)).unwrap();
    wait_for(&m1, Phase::Running, 30);
    m2.start(&data.launch(p2)).unwrap();
    wait_for(&m2, Phase::Failed, 30);
    assert!(
        m2.status().message.unwrap().contains("in use"),
        "the lock explains itself"
    );
    assert_eq!(
        m1.status().phase,
        Phase::Running,
        "the first server is unaffected"
    );
    m1.stop(Duration::from_secs(30));
}

#[test]
fn a_server_ended_from_outside_is_noticed() {
    let (data, port) = (Data::new(), free_port());
    let (m, _) = manager();
    m.start(&data.launch(port)).unwrap();
    wait_for(&m, Phase::Running, 30);
    let pid = m.status().pid.unwrap();
    #[cfg(unix)]
    let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
    #[cfg(windows)]
    let _ = Command::new("taskkill")
        .args(["/F", "/PID", &pid.to_string()])
        .output();
    wait_for(&m, Phase::Failed, 10);
}
