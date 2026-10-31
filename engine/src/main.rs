mod bench;

use clap::{Parser, Subcommand};
use powerdb::api::{self, Config};
use powerdb::auth::Access;
use powerdb::server;
use powerdb::Options;
use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "powerdb", version, about = "One database for records and files.")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the server (API and dashboard on one port).
    Serve {
        #[arg(long, default_value = "./powerdb-data", env = "POWERDB_DATA")]
        data: PathBuf,
        #[arg(long, default_value = "127.0.0.1:8090", env = "POWERDB_ADDR")]
        addr: SocketAddr,
        /// Let anyone create an account (off by default: administrators create users).
        #[arg(long)]
        open_registration: bool,
        /// Allow browsers on this origin to call the API (repeatable). Off by default: the dashboard is same-origin.
        #[arg(long = "cors-origin")]
        cors_origin: Vec<String>,
        /// Trust X-Forwarded-For for rate limiting. Only behind a proxy you control.
        #[arg(long)]
        trust_proxy: bool,
        /// Do not fsync on every commit. Faster; the last commits can be lost on power failure.
        #[arg(long)]
        no_sync: bool,
        /// Largest single file accepted, in GiB.
        #[arg(long, default_value_t = 5)]
        max_upload_gib: u64,
    },
    /// Create an API key without starting the server.
    CreateKey {
        #[arg(long, default_value = "./powerdb-data", env = "POWERDB_DATA")]
        data: PathBuf,
        #[arg(long)]
        name: String,
        #[arg(long, value_enum, default_value = "read")]
        access: AccessArg,
        /// Limit the key to these collections (repeatable).
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

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "powerdb=info,warn".into())).init();
    let cli = Cli::parse();
    let code = match cli.cmd {
        Cmd::Bench { args } => tokio::task::spawn_blocking(move || bench::run(&args)).await.unwrap_or(1),
        Cmd::CreateKey { data, name, access, collections, admin } => {
            let r = (|| -> powerdb::Result<String> {
                let (st, _) = server::build_state(&data, Options::default(), Config::default())?;
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
                Err(e) => {
                    eprintln!("powerdb: {e}");
                    1
                }
            }
        }
        Cmd::Serve { data, addr, open_registration, cors_origin, trust_proxy, no_sync, max_upload_gib } => match serve(data, addr, open_registration, cors_origin, trust_proxy, no_sync, max_upload_gib).await {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("powerdb: {e}");
                1
            }
        },
    };
    std::process::exit(code);
}

async fn serve(data: PathBuf, addr: SocketAddr, open_registration: bool, cors_origins: Vec<String>, trust_proxy: bool, no_sync: bool, max_upload_gib: u64) -> Result<(), Box<dyn std::error::Error>> {
    let cfg = Config { open_registration, trust_proxy, cors_origins, max_upload_bytes: max_upload_gib << 30, ..Config::default() };
    let (state, boot) = server::build_state(&data, Options { sync: !no_sync, ..Options::default() }, cfg)?;
    if let Some(k) = &boot.admin_key {
        eprintln!("\nFirst run: no accounts exist, so an administrator API key was created.\n\n    {k}\n\nSave it now; it is shown only once. Paste it into the dashboard to sign in.\n(Or set POWERDB_ADMIN_EMAIL and POWERDB_ADMIN_PASSWORD before the first start to create an account instead.)\n");
    }
    if let Some(e) = &boot.admin_user {
        eprintln!("First run: administrator account {e} created from POWERDB_ADMIN_EMAIL / POWERDB_ADMIN_PASSWORD.");
    }
    server::spawn_maintenance(state.db.clone());
    let db = state.db.clone();
    let app = api::router(state);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("PowerDB listening on http://{addr} (data: {})", data.display());
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("shutting down");
        })
        .await?;
    db.flush()?;
    Ok(())
}
