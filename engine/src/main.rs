mod bench;
mod service;

use clap::{Parser, Subcommand};
use powerdb::config::{self, FileConfig, Overrides, Settings};
use powerdb::security::crypto::ChaChaCodec;
use powerdb::security::secrets::Secrets;
use powerdb::{Db, Options, api, auth::Access, ops, server};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

/// Standard error for the log, which never fails. When the process that started the server goes away (the desktop app
/// crashing, a terminal closing), the pipe behind stderr breaks; a failed write must not take the server down with it,
/// least of all in the middle of shutting down and flushing the database.
struct QuietStderr;

impl std::io::Write for QuietStderr {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let _ = std::io::stderr().write(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        let _ = std::io::stderr().flush();
        Ok(())
    }
}

#[derive(Parser)]
#[command(
    name = "powerdb",
    version,
    about = "One database for records and files. Run it locally, self-host it, back it up.",
    propagate_version = true
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a new data directory with a settings file, a server secret and a first administrator.
    Init {
        #[arg(default_value = "./powerdb-data")]
        dir: PathBuf,
    },
    /// Run the server (API and dashboard on one port). A new directory is initialized first.
    Serve {
        #[arg(long, env = "POWERDB_DATA", default_value = "./powerdb-data")]
        data: PathBuf,
        /// Address to listen on [default 127.0.0.1:8090, or the settings file's addr].
        #[arg(long, env = "POWERDB_ADDR")]
        addr: Option<SocketAddr>,
        /// Let anyone create an account (administrators create users otherwise).
        #[arg(long)]
        open_registration: bool,
        /// Allow browsers on this origin to call the API (repeatable).
        #[arg(long = "cors-origin")]
        cors_origin: Vec<String>,
        /// Trust X-Forwarded-For for rate limiting. Only behind a proxy you control.
        #[arg(long)]
        trust_proxy: bool,
        /// commit (every write fsynced, safest), interval (background fsync) or never.
        #[arg(long, env = "POWERDB_DURABILITY")]
        durability: Option<String>,
        /// With durability interval: milliseconds between fsyncs.
        #[arg(long)]
        interval_ms: Option<u64>,
        /// Largest single file accepted, in GiB.
        #[arg(long)]
        max_upload_gib: Option<u64>,
        /// PEM certificate chain: serve HTTPS directly. Needs --tls-key.
        #[arg(long, env = "POWERDB_TLS_CERT")]
        tls_cert: Option<PathBuf>,
        /// PEM private key for --tls-cert.
        #[arg(long, env = "POWERDB_TLS_KEY")]
        tls_key: Option<PathBuf>,
        /// Shut down cleanly when standard input closes (or a line `shutdown` arrives). For a parent process, such as the
        /// desktop app, that has no portable way to send a termination signal.
        #[arg(long)]
        shutdown_on_stdin_close: bool,
        /// Use a temporary data directory that is deleted when the server stops. For tests and trying things out.
        #[arg(long)]
        ephemeral: bool,
    },
    /// Ask a running server whether it is healthy. Exit status 0 when it is.
    Status {
        #[arg(long, default_value = "127.0.0.1:8090", env = "POWERDB_ADDR")]
        addr: SocketAddr,
    },
    /// Check a stopped database's integrity: every stored chunk, every reference, every counter.
    Check {
        #[arg(long, env = "POWERDB_DATA", default_value = "./powerdb-data")]
        data: PathBuf,
        /// Skip re-reading file contents; check structure and counters only.
        #[arg(long)]
        quick: bool,
    },
    /// Copy a stopped database into a new directory, with checksums. The server secret is left out unless asked.
    Backup {
        #[arg(long, env = "POWERDB_DATA", default_value = "./powerdb-data")]
        data: PathBuf,
        #[arg(long)]
        to: PathBuf,
        /// Also copy secret.key. Convenient, but then the backup holds the key to its own encrypted files.
        #[arg(long)]
        include_secret: bool,
    },
    /// Restore a backup into a new, empty directory, verifying it before and after.
    Restore {
        #[arg(long)]
        from: PathBuf,
        #[arg(long, env = "POWERDB_DATA")]
        data: PathBuf,
    },
    /// Create an API key without starting the server.
    CreateKey {
        #[arg(long, env = "POWERDB_DATA", default_value = "./powerdb-data")]
        data: PathBuf,
        #[arg(long)]
        name: String,
        /// read, write or read-write. Default: read, or read-write with --admin.
        #[arg(long, value_enum)]
        access: Option<AccessArg>,
        /// Limit the key to this collection (repeatable). Default: all of them.
        #[arg(long = "collection")]
        collections: Vec<String>,
        /// A key that bypasses collection rules, like an administrator account.
        #[arg(long)]
        admin: bool,
    },
    /// Install and control PowerDB as an operating-system service (Windows Service, launchd, systemd).
    Service {
        #[command(subcommand)]
        action: ServiceCmd,
    },
    /// Buckets on a running server: create, list, delete.
    Bucket {
        #[command(subcommand)]
        action: BucketCmd,
    },
    /// Files in buckets on a running server: put, get, list, delete, versions.
    Object {
        #[command(subcommand)]
        action: ObjectCmd,
    },
    /// Measure the storage engine on this machine.
    Bench {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

/// Where the server is and who is asking: for the commands that talk to a running server.
#[derive(clap::Args)]
struct Remote {
    /// The server's address.
    #[arg(long, env = "POWERDB_URL", default_value = "http://127.0.0.1:8090")]
    url: String,
    /// An administrator API key (`powerdb init` printed one).
    #[arg(long, env = "POWERDB_KEY", hide_env_values = true)]
    key: Option<String>,
}

#[derive(Subcommand)]
enum BucketCmd {
    /// Create a bucket.
    Create {
        name: String,
        /// Keep every version of every object.
        #[arg(long)]
        versioning: bool,
        /// Encrypt the bucket's file data at rest.
        #[arg(long)]
        encrypt: bool,
        #[command(flatten)]
        remote: Remote,
    },
    /// List the buckets.
    Ls {
        #[command(flatten)]
        remote: Remote,
    },
    /// Delete an empty bucket.
    Rm {
        name: String,
        #[command(flatten)]
        remote: Remote,
    },
    /// Turn versioning on for a bucket (it cannot be turned off).
    Versioning {
        name: String,
        #[command(flatten)]
        remote: Remote,
    },
}

#[derive(Subcommand)]
enum ObjectCmd {
    /// Upload a file as BUCKET/KEY (`-` reads standard input).
    Put {
        /// BUCKET/KEY, for example documents/contracts/acme.pdf
        path: String,
        file: PathBuf,
        /// The content type. Default: guessed from the file name.
        #[arg(long = "type")]
        mime: Option<String>,
        /// Custom metadata, NAME=VALUE (repeatable).
        #[arg(long = "meta")]
        meta: Vec<String>,
        #[command(flatten)]
        remote: Remote,
    },
    /// Download BUCKET/KEY to a file (default: its last name in the current folder; `-` writes to standard output).
    Get {
        path: String,
        to: Option<PathBuf>,
        /// A specific version instead of the newest.
        #[arg(long)]
        version: Option<String>,
        #[command(flatten)]
        remote: Remote,
    },
    /// List the objects in a bucket, optionally only those whose key starts with PREFIX.
    Ls {
        bucket: String,
        prefix: Option<String>,
        #[command(flatten)]
        remote: Remote,
    },
    /// Delete BUCKET/KEY. In a bucket with versioning this hides it; add --version to remove one version for good.
    Rm {
        path: String,
        #[arg(long)]
        version: Option<String>,
        #[command(flatten)]
        remote: Remote,
    },
    /// List every version of BUCKET/KEY, newest first.
    Versions {
        path: String,
        #[command(flatten)]
        remote: Remote,
    },
    /// Make an old version of BUCKET/KEY the newest again.
    Restore {
        path: String,
        #[arg(long)]
        version: String,
        #[command(flatten)]
        remote: Remote,
    },
}

#[derive(Subcommand)]
enum ServiceCmd {
    /// Register the service and start it. Needs administrator rights (sudo) unless --user on macOS.
    Install {
        /// Where the service keeps its data. Default: the platform's usual place.
        #[arg(long)]
        data: Option<PathBuf>,
        #[arg(long)]
        addr: Option<SocketAddr>,
        /// Linux and macOS daemons: the account to run as (it must own the data directory).
        #[arg(long)]
        run_as: Option<String>,
        /// macOS: install for the signed-in user (a launch agent) instead of the whole machine.
        #[arg(long)]
        user: bool,
        /// Only print the unit or job description, change nothing.
        #[arg(long)]
        print: bool,
    },
    /// Stop and remove the service. Data is left alone.
    Uninstall {
        #[arg(long)]
        user: bool,
    },
    Start {
        #[arg(long)]
        user: bool,
    },
    Stop {
        #[arg(long)]
        user: bool,
    },
    Status {
        #[arg(long)]
        user: bool,
    },
    /// Entry point for the Windows Service Control Manager. Not for people.
    #[command(hide = true)]
    Run {
        #[arg(long)]
        data: PathBuf,
        #[arg(long)]
        addr: Option<SocketAddr>,
    },
}

#[derive(Clone, clap::ValueEnum)]
enum AccessArg {
    Read,
    Write,
    ReadWrite,
}

fn fail(e: impl std::fmt::Display) -> i32 {
    eprintln!("powerdb: {e}");
    1
}

fn client(r: Remote) -> powerdb::Result<powerdb::client::Client> {
    powerdb::client::Client::new(&r.url, r.key)
}

/// Epoch milliseconds as `YYYY-MM-DD HH:MM` (UTC), without pulling in a date library.
fn fmt_time(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // civil-from-days (H. Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        rem / 3600,
        rem % 3600 / 60
    )
}

fn human(n: u64) -> String {
    const U: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

fn bucket_command(action: BucketCmd) -> i32 {
    let run = || -> powerdb::Result<()> {
        match action {
            BucketCmd::Create {
                name,
                versioning,
                encrypt,
                remote,
            } => {
                client(remote)?.bucket_create(&name, versioning, encrypt)?;
                println!(
                    "Created bucket {name}{}{}",
                    if versioning { " (versioned)" } else { "" },
                    if encrypt { " (encrypted)" } else { "" }
                );
            }
            BucketCmd::Ls { remote } => {
                let v = client(remote)?.buckets()?;
                let items = v["items"].as_array().cloned().unwrap_or_default();
                if items.is_empty() {
                    println!("No buckets.");
                }
                for b in items {
                    let flags: Vec<&str> = [("versioned", "versioning"), ("encrypted", "encrypt")]
                        .iter()
                        .filter(|(_, k)| b[*k].as_bool() == Some(true))
                        .map(|(l, _)| *l)
                        .collect();
                    println!(
                        "{:<30} {}",
                        b["name"].as_str().unwrap_or(""),
                        flags.join(", ")
                    );
                }
            }
            BucketCmd::Rm { name, remote } => {
                client(remote)?.bucket_delete(&name)?;
                println!("Deleted bucket {name}");
            }
            BucketCmd::Versioning { name, remote } => {
                client(remote)?.bucket_versioning(&name)?;
                println!("Versioning is on for {name}");
            }
        }
        Ok(())
    };
    match run() {
        Ok(()) => 0,
        Err(e) => fail(e),
    }
}

fn object_command(action: ObjectCmd) -> i32 {
    use powerdb::client::split_path;
    let run = || -> powerdb::Result<()> {
        match action {
            ObjectCmd::Put {
                path,
                file,
                mime,
                meta,
                remote,
            } => {
                let (b, k) = split_path(&path)?;
                let meta: Vec<(String, String)> = meta
                    .iter()
                    .map(|m| {
                        m.split_once('=')
                            .map(|(a, b)| (a.to_string(), b.to_string()))
                            .ok_or_else(|| {
                                powerdb::Error::Invalid(format!(
                                    "--meta wants NAME=VALUE, got \"{m}\""
                                ))
                            })
                    })
                    .collect::<powerdb::Result<_>>()?;
                let v = client(remote)?.object_put(b, k, &file, mime.as_deref(), &meta)?;
                let o = &v["object"];
                println!(
                    "Stored {b}/{k}  {}  version {}  checksum (BLAKE3) {}",
                    human(o["size"].as_u64().unwrap_or(0)),
                    o["version_id"].as_str().unwrap_or(""),
                    o["checksum"].as_str().unwrap_or("")
                );
            }
            ObjectCmd::Get {
                path,
                to,
                version,
                remote,
            } => {
                let (b, k) = split_path(&path)?;
                let to = to.unwrap_or_else(|| PathBuf::from(k.rsplit('/').next().unwrap_or(k)));
                let n = client(remote)?.object_get(b, k, version.as_deref(), &to)?;
                if to.as_os_str() != "-" {
                    println!("Wrote {} to {}", human(n), to.display());
                }
            }
            ObjectCmd::Ls {
                bucket,
                prefix,
                remote,
            } => {
                let c = client(remote)?;
                let prefix = prefix.unwrap_or_default();
                let mut after: Option<String> = None;
                let mut total = 0usize;
                loop {
                    let v = c.objects(&bucket, &prefix, after.as_deref())?;
                    for o in v["items"].as_array().cloned().unwrap_or_default() {
                        println!(
                            "{:>10}  {}  {}",
                            human(o["size"].as_u64().unwrap_or(0)),
                            fmt_time(o["created_at"].as_i64().unwrap_or(0)),
                            o["key"].as_str().unwrap_or("")
                        );
                        total += 1;
                    }
                    match v["next"].as_str() {
                        Some(n) => after = Some(n.to_string()),
                        None => break,
                    }
                }
                if total == 0 {
                    println!("No objects.");
                }
            }
            ObjectCmd::Rm {
                path,
                version,
                remote,
            } => {
                let (b, k) = split_path(&path)?;
                client(remote)?.object_delete(b, k, version.as_deref())?;
                println!(
                    "Deleted {b}/{k}{}",
                    version
                        .map(|v| format!(" (version {v})"))
                        .unwrap_or_default()
                );
            }
            ObjectCmd::Versions { path, remote } => {
                let (b, k) = split_path(&path)?;
                let v = client(remote)?.versions(b, k)?;
                for o in v["items"].as_array().cloned().unwrap_or_default() {
                    let what = if o["delete_marker"].as_bool() == Some(true) {
                        "(deleted)".to_string()
                    } else {
                        human(o["size"].as_u64().unwrap_or(0))
                    };
                    println!(
                        "{}  {}  {}",
                        o["version_id"].as_str().unwrap_or(""),
                        fmt_time(o["created_at"].as_i64().unwrap_or(0)),
                        what
                    );
                }
            }
            ObjectCmd::Restore {
                path,
                version,
                remote,
            } => {
                let (b, k) = split_path(&path)?;
                let v = client(remote)?.restore(b, k, &version)?;
                println!(
                    "Restored {b}/{k} as version {}",
                    v["object"]["version_id"].as_str().unwrap_or("")
                );
            }
        }
        Ok(())
    };
    match run() {
        Ok(()) => 0,
        Err(e) => fail(e),
    }
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "powerdb=info,warn".into());
    // A service has no console, so its log goes to a file beside the data.
    let service_log = match &cli.cmd {
        Cmd::Service {
            action: ServiceCmd::Run { data, .. },
        } => {
            let _ = std::fs::create_dir_all(data);
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(data.join("powerdb.log"))
                .ok()
        }
        _ => None,
    };
    match service_log {
        Some(f) => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(false)
            .with_writer(std::sync::Mutex::new(f))
            .init(),
        None => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(|| QuietStderr)
            .init(),
    }
    let code = match cli.cmd {
        Cmd::Service { action } => service_command(action).await,
        Cmd::Bucket { action } => tokio::task::spawn_blocking(move || bucket_command(action))
            .await
            .unwrap_or(1),
        Cmd::Object { action } => tokio::task::spawn_blocking(move || object_command(action))
            .await
            .unwrap_or(1),
        Cmd::Bench { args } => tokio::task::spawn_blocking(move || bench::run(&args))
            .await
            .unwrap_or(1),
        Cmd::Init { dir } => match ops::init(&dir) {
            Ok(i) => {
                println!("Created {}", dir.display());
                println!("  settings: {}", i.config_path.display());
                if let Some(k) = i.admin_key {
                    println!("\nAdministrator API key (shown once; save it):\n\n    {k}\n");
                }
                if let Some(e) = i.admin_user {
                    println!("\nAdministrator account: {e}\n");
                }
                println!(
                    "Start it:   powerdb serve --data {}\nThen open:  http://127.0.0.1:8090",
                    dir.display()
                );
                0
            }
            Err(e) => fail(e),
        },
        Cmd::Status { addr } => match ops::status(addr) {
            Ok(m) => {
                println!("{m}");
                0
            }
            Err(e) => fail(e),
        },
        Cmd::Check { data, quick } => check(data, quick),
        Cmd::Backup {
            data,
            to,
            include_secret,
        } => match ops::backup(&data, &to, include_secret) {
            Ok(r) => {
                println!(
                    "Backed up {} files ({:.1} MiB) to {}",
                    r.files,
                    r.bytes as f64 / 1048576.0,
                    to.display()
                );
                if !r.includes_secret {
                    println!(
                        "The server secret was NOT copied. To restore encrypted files or keep sessions and file links valid, keep secret.key (or POWERDB_SECRET) somewhere safe and separate."
                    );
                }
                0
            }
            Err(e) => fail(e),
        },
        Cmd::Restore { from, data } => match ops::restore(&from, &data) {
            Ok(r) => {
                println!(
                    "Restored {} files ({:.1} MiB) into {} and the integrity check passed.",
                    r.files,
                    r.bytes as f64 / 1048576.0,
                    data.display()
                );
                if !r.includes_secret && !data.join("secret.key").is_file() {
                    println!(
                        "This backup has no secret.key: put the original there, or set POWERDB_SECRET, before starting the server (a new secret cannot read encrypted files)."
                    );
                }
                0
            }
            Err(e) => fail(e),
        },
        Cmd::CreateKey {
            data,
            name,
            access,
            collections,
            admin,
        } => {
            let r = (|| -> powerdb::Result<String> {
                let (st, _) =
                    server::build_state(&data, Options::default(), api::Config::default())?;
                // an administrative key always has full access, so asking for --admin alone means exactly that
                let a = match access.unwrap_or(if admin {
                    AccessArg::ReadWrite
                } else {
                    AccessArg::Read
                }) {
                    AccessArg::Read => Access::Read,
                    AccessArg::Write => Access::Write,
                    AccessArg::ReadWrite => Access::ReadWrite,
                };
                Ok(st.auth.create_key(&name, a, collections, admin)?.0)
            })();
            match r {
                Ok(k) => {
                    println!("{k}");
                    eprintln!("This is the only time the key is shown.");
                    0
                }
                Err(e) => fail(e),
            }
        }
        Cmd::Serve {
            data,
            addr,
            open_registration,
            cors_origin,
            trust_proxy,
            durability,
            interval_ms,
            max_upload_gib,
            tls_cert,
            tls_key,
            shutdown_on_stdin_close,
            ephemeral,
        } => {
            let cli = Overrides {
                addr,
                open_registration,
                trust_proxy,
                cors_origins: cors_origin,
                max_upload_gib,
                durability,
                interval_ms,
                tls_cert,
                tls_key,
            };
            match serve(
                data,
                cli,
                ephemeral,
                shutdown_signal(shutdown_on_stdin_close),
            )
            .await
            {
                Ok(()) => 0,
                Err(e) => fail(e),
            }
        }
    };
    std::process::exit(code);
}

fn report(r: Result<String, String>) -> i32 {
    match r {
        Ok(m) => {
            println!("{m}");
            0
        }
        Err(e) => fail(e),
    }
}

async fn service_command(action: ServiceCmd) -> i32 {
    use service::Spec;
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = action;
        fail("services are supported on Linux (systemd), macOS (launchd) and Windows")
    }
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    match action {
        ServiceCmd::Install {
            data,
            addr,
            run_as,
            user,
            print,
        } => {
            let exe = match std::env::current_exe().and_then(|p| p.canonicalize()) {
                Ok(e) => e,
                Err(e) => return fail(e),
            };
            let spec = Spec {
                exe,
                data: data.unwrap_or_else(|| service::default_data_dir(user)),
                addr,
                run_as,
            };
            if let Err(e) = spec.validate() {
                return fail(e);
            }
            #[cfg(target_os = "macos")]
            return report(service::install(&spec, print, user));
            #[cfg(not(target_os = "macos"))]
            {
                let _ = user;
                report(service::install(&spec, print))
            }
        }
        ServiceCmd::Uninstall { user } => {
            #[cfg(target_os = "macos")]
            return report(service::uninstall(user));
            #[cfg(not(target_os = "macos"))]
            {
                let _ = user;
                report(service::uninstall())
            }
        }
        ServiceCmd::Start { user } => service_control("start", user),
        ServiceCmd::Stop { user } => service_control("stop", user),
        ServiceCmd::Status { user } => service_control("status", user),
        ServiceCmd::Run { data, addr } => {
            #[cfg(windows)]
            {
                let handle = tokio::runtime::Handle::current();
                match tokio::task::spawn_blocking(move || {
                    service::run_as_service(handle, data, addr)
                })
                .await
                {
                    Ok(r) => report(r.map(|_| String::new())),
                    Err(e) => fail(e),
                }
            }
            #[cfg(not(windows))]
            {
                let _ = (data, addr);
                fail(
                    "`service run` is only for the Windows Service Control Manager; use `powerdb serve`",
                )
            }
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
fn service_control(action: &str, user: bool) -> i32 {
    #[cfg(target_os = "macos")]
    return report(service::control(action, user));
    #[cfg(not(target_os = "macos"))]
    {
        let _ = user;
        report(service::control(action))
    }
}

fn check(data: PathBuf, quick: bool) -> i32 {
    let run = || -> powerdb::Result<powerdb::storage::verify::VerifyReport> {
        let db = Db::open(&data, Options::default())?;
        if let Some(s) = Secrets::load_existing(&data)? {
            db.register_codec(Arc::new(ChaChaCodec::new(&s))); // so encrypted chunks can be authenticated too
        }
        db.verify(!quick)
    };
    match run() {
        Ok(r) => {
            let how = if quick {
                " (structure and counters only)".to_string()
            } else {
                format!(
                    ", {:.1} MiB of chunk data re-read and verified",
                    r.chunk_bytes_checked as f64 / 1048576.0
                )
            };
            println!(
                "{} records, {} files, {} chunks{how}",
                r.records, r.files, r.chunks
            );
            if r.problems.is_empty() {
                println!("No problems found.");
                0
            } else {
                println!("{} problem(s):", r.problems.len());
                for p in &r.problems {
                    println!("  - {p}");
                }
                1
            }
        }
        Err(e) => fail(e),
    }
}

pub(crate) async fn serve(
    data: PathBuf,
    cli: Overrides,
    ephemeral: bool,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<(), Box<dyn std::error::Error>> {
    let (data, _guard) = if ephemeral {
        let d = std::env::temp_dir().join(format!("powerdb-ephemeral-{}", powerdb::util::new_id()));
        (d.clone(), Some(TempDir(d)))
    } else {
        (data, None)
    };
    // a new directory gets the settings template, like `init`, so the file is there to edit
    if std::fs::read_dir(&data)
        .map(|mut d| d.next().is_none())
        .unwrap_or(true)
    {
        std::fs::create_dir_all(&data)?;
        std::fs::write(data.join(config::FILE_NAME), config::TEMPLATE)?;
    }
    let settings = Settings::resolve(cli, FileConfig::load(&data)?)?;
    let (state, boot) = server::build_state(&data, settings.options.clone(), settings.api.clone())?;
    if let Some(k) = &boot.admin_key {
        eprintln!(
            "\nFirst run: no accounts exist, so an administrator API key was created.\n\n    {k}\n\nSave it now; it is shown only once. Paste it into the dashboard to sign in.\n(Or set POWERDB_ADMIN_EMAIL and POWERDB_ADMIN_PASSWORD before the first start to create an account instead.)\n"
        );
    }
    if let Some(e) = &boot.admin_user {
        eprintln!(
            "First run: administrator account {e} created from POWERDB_ADMIN_EMAIL / POWERDB_ADMIN_PASSWORD."
        );
    }
    if ephemeral {
        eprintln!(
            "Ephemeral mode: data lives in {} and is deleted when the server stops.",
            data.display()
        );
    }
    if !settings.addr.ip().is_loopback() && settings.tls.is_none() {
        eprintln!(
            "Listening on {}: this is reachable from other machines. Serve HTTPS (tls_cert/tls_key) or put a TLS proxy in front of it (docs/OPERATIONS.md); the API sends bearer tokens.",
            settings.addr
        );
    }
    server::spawn_maintenance(state.db.clone());
    let db = state.db.clone();
    let app = api::router(state);
    let svc = app.into_make_service_with_connect_info::<SocketAddr>();
    if let Some((cert, key)) = &settings.tls {
        let rel = |p: &PathBuf| {
            if p.is_relative() {
                data.join(p)
            } else {
                p.clone()
            }
        };
        let (cert, key) = (rel(cert), rel(key));
        let _ = rustls::crypto::ring::default_provider().install_default();
        let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(&cert, &key)
            .await
            .map_err(|e| {
                format!(
                    "cannot load the TLS certificate {} / key {}: {e}",
                    cert.display(),
                    key.display()
                )
            })?;
        let reload = tls.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                match reload.reload_from_pem_file(&cert, &key).await {
                    Ok(()) => tracing::info!("TLS certificate reloaded"),
                    Err(e) => {
                        tracing::warn!("TLS certificate not reloaded, keeping the old one: {e}")
                    }
                }
            }
        });
        let handle = axum_server::Handle::new();
        let h = handle.clone();
        tokio::spawn(async move {
            shutdown.await;
            tracing::info!("shutting down");
            h.graceful_shutdown(Some(std::time::Duration::from_secs(30)));
        });
        tracing::info!(
            "PowerDB {} listening on https://{} (data: {})",
            env!("CARGO_PKG_VERSION"),
            settings.addr,
            data.display()
        );
        axum_server::bind_rustls(settings.addr, tls)
            .handle(handle)
            .serve(svc)
            .await?;
    } else {
        let listener = tokio::net::TcpListener::bind(settings.addr).await?;
        tracing::info!(
            "PowerDB {} listening on http://{} (data: {})",
            env!("CARGO_PKG_VERSION"),
            settings.addr,
            data.display()
        );
        axum::serve(listener, svc)
            .with_graceful_shutdown(async move {
                shutdown.await;
                tracing::info!("shutting down");
            })
            .await?;
    }
    db.flush()?;
    Ok(())
}

/// Resolves when the process is asked to stop: Ctrl-C everywhere, and SIGTERM on Unix, which is what systemd, Docker and most supervisors send.
async fn shutdown_signal(stdin_shutdown: bool) {
    if stdin_shutdown {
        tokio::select! {
            _ = os_signal() => {}
            _ = stdin_closed() => {}
        }
    } else {
        os_signal().await;
    }
}

/// Resolves when standard input reaches end of file, or a line says `shutdown`.
async fn stdin_closed() {
    let _ = tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        let stdin = std::io::stdin();
        loop {
            line.clear();
            match stdin.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) if line.trim() == "shutdown" => break,
                Ok(_) => {}
            }
        }
    })
    .await;
}

async fn os_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await; // Windows: Ctrl-C and Ctrl-Break (the console's close signals) arrive here
    }
}

/// Deletes a directory when dropped (the data of an ephemeral run).
struct TempDir(PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
