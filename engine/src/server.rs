//! Wiring: open a data directory and build everything the HTTP layer needs.

use crate::api::{AppState, Config, Limiter};
use crate::auth::{Access, Auth, Role};
use crate::error::Result;
use crate::security::crypto::ChaChaCodec;
use crate::security::secrets::Secrets;
use crate::security::signed_url::UrlSigner;
use crate::storage::{Db, Options};
use std::path::Path;
use std::sync::Arc;

pub struct Bootstrap {
    /// The secret of a freshly created administrator key. Shown once; the server keeps only its hash.
    pub admin_key: Option<String>,
    pub admin_user: Option<String>,
}

pub fn build_state(dir: &Path, opts: Options, cfg: Config) -> Result<(AppState, Bootstrap)> {
    let secrets = Secrets::load_or_create(dir)?;
    let db = Db::open(dir, opts)?;
    db.register_codec(Arc::new(ChaChaCodec::new(&secrets)));
    let auth = Auth::new(&db, secrets.derive("sessions"))?;
    let signer = UrlSigner::new(&secrets);
    let mut boot = Bootstrap { admin_key: None, admin_user: None };
    // First run: nothing exists, so nobody could ever sign in. Create an administrator from the environment if given, else mint one admin API key.
    if auth.user_count()? == 0 && auth.keys()?.is_empty() {
        match (std::env::var("POWERDB_ADMIN_EMAIL"), std::env::var("POWERDB_ADMIN_PASSWORD")) {
            (Ok(e), Ok(p)) => {
                auth.register(&e, &p, Role::Admin)?;
                boot.admin_user = Some(e);
            }
            _ => boot.admin_key = Some(auth.create_key("bootstrap admin", Access::ReadWrite, vec![], true)?.0),
        }
    }
    Ok((AppState { db, auth, signer, cfg: Arc::new(cfg), limiter: Arc::new(Limiter::new()) }, boot))
}

/// Space reclamation while the server runs: release deleted files often, compact mostly-dead segments now and then.
pub fn spawn_maintenance(db: Db) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(10));
        let mut n = 0u64;
        loop {
            tick.tick().await;
            n += 1;
            let db = db.clone();
            let compact = n.is_multiple_of(30);
            let r = tokio::task::spawn_blocking(move || -> Result<()> {
                db.gc_step(128)?;
                if compact {
                    db.compact(0.5)?;
                }
                Ok(())
            })
            .await;
            match r {
                Ok(Err(e)) => tracing::error!("maintenance: {e}"),
                Err(e) => tracing::error!("maintenance task: {e}"),
                _ => {}
            }
        }
    });
}
