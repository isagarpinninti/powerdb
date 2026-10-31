mod bench;

use clap::{Parser, Subcommand};
use powerdb::config::{self, FileConfig, Overrides, Settings};
use powerdb::security::crypto::ChaChaCodec;
use powerdb::security::secrets::Secrets;
use powerdb::{Db, Options, api, auth::Access, ops, server};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

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
        #[arg(long, value_enum, default_value = "read")]
        access: AccessArg,
        #[arg(long = "collection")]
        collections: Vec<String>,
        #[arg(long)]
        admin: bool,
    },
    /// Measure the storage engine on this machine.
    Bench {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
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

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "powerdb=info,warn".into()),
        )
        .init();
    let code = match Cli::parse().cmd {
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
                let a = match access {
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
            };
            match serve(data, cli, ephemeral).await {
                Ok(()) => 0,
                Err(e) => fail(e),
            }
        }
    };
    std::process::exit(code);
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

async fn serve(
    data: PathBuf,
    cli: Overrides,
    ephemeral: bool,
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
    if !settings.addr.ip().is_loopback() {
        eprintln!(
            "Listening on {}: this is reachable from other machines. Put a TLS proxy in front of it (docs/OPERATIONS.md); the API sends bearer tokens.",
            settings.addr
        );
    }
    server::spawn_maintenance(state.db.clone());
    let db = state.db.clone();
    let app = api::router(state);
    let listener = tokio::net::TcpListener::bind(settings.addr).await?;
    tracing::info!(
        "PowerDB {} listening on http://{} (data: {})",
        env!("CARGO_PKG_VERSION"),
        settings.addr,
        data.display()
    );
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        shutdown_signal().await;
        tracing::info!("shutting down");
    })
    .await?;
    db.flush()?;
    Ok(())
}

/// Resolves when the process is asked to stop: Ctrl-C everywhere, and SIGTERM on Unix, which is what systemd, Docker and most supervisors send.
async fn shutdown_signal() {
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
