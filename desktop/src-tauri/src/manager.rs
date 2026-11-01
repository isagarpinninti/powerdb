//! Starts, watches and stops the `powerdb` server process. It knows nothing about windows or menus: it reports through a
//! callback, so the same code is driven by the app and by the tests (which run the real server binary).

use crate::api;
use serde::Serialize;
use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const LOG_LINES: usize = 3000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Stopped,
    Starting,
    Running,
    Stopping,
    Failed,
}

#[derive(Clone, Debug, Serialize)]
pub struct Status {
    pub phase: Phase,
    pub pid: Option<u32>,
    /// Unix milliseconds when the server became ready.
    pub since: Option<u64>,
    /// Why it failed, when it did.
    pub message: Option<String>,
}

#[derive(Clone)]
pub enum Event {
    Status(Status),
    Log(String),
}

#[derive(Clone, Debug)]
pub struct Launch {
    pub binary: PathBuf,
    pub data: PathBuf,
    pub port: u16,
}

struct Inner {
    status: Status,
    child: Option<Child>,
    logs: VecDeque<String>,
    /// Bumped on every start, so a watcher from an earlier run never reports on a later one.
    generation: u64,
}

#[derive(Clone)]
pub struct Manager {
    inner: Arc<Mutex<Inner>>,
    emit: Arc<dyn Fn(Event) + Send + Sync>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Runs a helper process without flashing a console window on Windows.
pub fn quiet(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd
}

impl Manager {
    pub fn new(emit: impl Fn(Event) + Send + Sync + 'static) -> Self {
        Manager {
            inner: Arc::new(Mutex::new(Inner {
                status: Status {
                    phase: Phase::Stopped,
                    pid: None,
                    since: None,
                    message: None,
                },
                child: None,
                logs: VecDeque::new(),
                generation: 0,
            })),
            emit: Arc::new(emit),
        }
    }

    pub fn status(&self) -> Status {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .status
            .clone()
    }

    pub fn logs(&self) -> Vec<String> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .logs
            .iter()
            .cloned()
            .collect()
    }

    fn set(&self, generation: u64, f: impl FnOnce(&mut Status)) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if g.generation != generation {
            return;
        }
        f(&mut g.status);
        let s = g.status.clone();
        drop(g);
        (self.emit)(Event::Status(s));
    }

    fn log(&self, line: String) {
        {
            let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if g.logs.len() == LOG_LINES {
                g.logs.pop_front();
            }
            g.logs.push_back(line.clone());
        }
        (self.emit)(Event::Log(line));
    }

    /// Starts the server. Returns once the process is spawned; the status turns `running` when it answers.
    pub fn start(&self, l: &Launch) -> Result<(), String> {
        let generation = {
            let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if matches!(
                g.status.phase,
                Phase::Starting | Phase::Running | Phase::Stopping
            ) {
                return Err("the server is already running".into());
            }
            g.generation += 1;
            g.generation
        };
        if api::healthy(l.port) {
            return Err(format!(
                "something is already answering on port {} (another PowerDB, or another program); choose a different port",
                l.port
            ));
        }
        let mut cmd = Command::new(&l.binary);
        cmd.arg("serve")
            .arg("--data")
            .arg(&l.data)
            .arg("--addr")
            .arg(format!("127.0.0.1:{}", l.port))
            .arg("--shutdown-on-stdin-close")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        quiet(&mut cmd);
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("cannot start {}: {e}", l.binary.display()))?;
        let pid = child.id();
        for stream in [
            child
                .stdout
                .take()
                .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
            child
                .stderr
                .take()
                .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
        ]
        .into_iter()
        .flatten()
        {
            let m = self.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(stream).lines().map_while(Result::ok) {
                    m.log(strip_ansi(&line));
                }
            });
        }
        {
            let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            g.child = Some(child);
            g.status = Status {
                phase: Phase::Starting,
                pid: Some(pid),
                since: None,
                message: None,
            };
        }
        (self.emit)(Event::Status(self.status()));
        let m = self.clone();
        let port = l.port;
        std::thread::spawn(move || m.watch(generation, port));
        Ok(())
    }

    fn watch(&self, generation: u64, port: u16) {
        let began = Instant::now();
        loop {
            std::thread::sleep(Duration::from_millis(250));
            let exited = {
                let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                if g.generation != generation {
                    return;
                }
                match g.child.as_mut().map(|c| c.try_wait()) {
                    Some(Ok(Some(status))) => {
                        g.child = None;
                        Some(status)
                    }
                    _ => None,
                }
            };
            if let Some(code) = exited {
                let stopping = self.status().phase == Phase::Stopping;
                let tail: Vec<String> = self
                    .logs()
                    .into_iter()
                    .rev()
                    .take(6)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                self.set(generation, |s| {
                    s.pid = None;
                    s.since = None;
                    if stopping || code.success() {
                        s.phase = Phase::Stopped;
                        s.message = None;
                    } else {
                        s.phase = Phase::Failed;
                        s.message = Some(format!(
                            "the server exited unexpectedly ({code}). {}",
                            tail.join(" | ")
                        ));
                    }
                });
                return;
            }
            let phase = self.status().phase;
            if phase == Phase::Starting {
                if api::healthy(port) {
                    self.set(generation, |s| {
                        s.phase = Phase::Running;
                        s.since = Some(now_ms());
                    });
                } else if began.elapsed() > Duration::from_secs(45) {
                    self.set(generation, |s| {
                        s.phase = Phase::Failed;
                        s.message =
                            Some("the server did not become ready within 45 seconds".into());
                    });
                    self.kill();
                    return;
                }
            }
        }
    }

    fn kill(&self) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(c) = g.child.as_mut() {
            let _ = c.kill();
        }
    }

    /// Asks the server to finish what it is doing and exit (by closing its standard input), and waits for it; after
    /// `grace` it is killed. Safe to call when nothing is running.
    pub fn stop(&self, grace: Duration) {
        let generation = {
            let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if g.child.is_none() {
                return;
            }
            let generation = g.generation;
            if let Some(c) = g.child.as_mut() {
                drop(c.stdin.take());
            }
            generation
        };
        self.set(generation, |s| s.phase = Phase::Stopping);
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            if self
                .inner
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .child
                .is_none()
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        self.log("the server did not stop in time and was ended".into());
        self.kill();
        let until = Instant::now() + Duration::from_secs(5);
        while Instant::now() < until
            && self
                .inner
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .child
                .is_some()
        {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\u{1b}' && it.peek() == Some(&'[') {
            for n in it.by_ref() {
                if n.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_colour_codes_are_removed_from_log_lines() {
        assert_eq!(
            strip_ansi("\u{1b}[2m2026\u{1b}[0m \u{1b}[32m INFO\u{1b}[0m hello"),
            "2026  INFO hello"
        );
    }
}
