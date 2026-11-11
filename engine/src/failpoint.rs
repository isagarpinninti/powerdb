//! Failure injection for tests: a named point in the commit, collection and compaction paths where a test can stop the process.
//!
//! Compiled only with the `failpoints` feature, so a normal build contains none of it. With the feature on, the environment variable
//! `POWERDB_FAILPOINT=name:N` aborts the process (as `SIGABRT` would: no destructors, no flushing) the N-th time the point `name` is reached.
//! The tests in `engine/tests/failpoints.rs` run a workload in a child process, let it die at each point in turn, then reopen the data
//! directory and check that nothing acknowledged was lost and nothing was left inconsistent.

#[cfg(feature = "failpoints")]
pub fn hit(name: &str) {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static TARGET: OnceLock<Option<(String, u64)>> = OnceLock::new();
    static COUNTS: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();
    let target = TARGET.get_or_init(|| {
        let v = std::env::var("POWERDB_FAILPOINT").ok()?;
        let (n, c) = v.split_once(':').unwrap_or((v.as_str(), "1"));
        Some((n.to_string(), c.parse().unwrap_or(1)))
    });
    let Some((want, nth)) = target else { return };
    if want != name {
        return;
    }
    let mut counts = COUNTS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let c = counts.entry(name.to_string()).or_default();
    *c += 1;
    if *c == *nth {
        eprintln!("failpoint {name} hit {c}: aborting");
        std::process::abort();
    }
}

/// `failpoint!("name")`: does nothing unless the `failpoints` feature is on and the environment selects this point.
#[macro_export]
macro_rules! failpoint {
    ($name:expr) => {
        #[cfg(feature = "failpoints")]
        {
            $crate::failpoint::hit($name);
        }
    };
}
