//! The version of the on-disk layout, and the upgrades between versions.
//!
//! A data directory records the layout that wrote it in `FORMAT`. Opening a directory written by a newer layout is
//! refused untouched; one written by an older layout is upgraded in place, one step at a time:
//!
//! * before the first step the metadata store (`meta/`) is copied to `pre-migration-v<N>/`, so a bad step can be undone
//!   by hand. The blob log is append-only and a step must not rewrite it in place, so it is not copied;
//! * `FORMAT` is rewritten atomically after each step, so an interrupted upgrade resumes from the last completed step;
//! * a failure stops at that step and says where the safety copy is. The directory stays at the last good version.
//!
//! Layout 1 is 0.1 to 0.3.1. Layout 2 (buckets and objects) is the first real step; see `v1_to_v2`. A change that makes old data unreadable
//! adds an entry here and bumps `FORMAT_VERSION`; the machinery below is exercised by tests with injected steps and by `tools/upgrade-test.py`.

use crate::error::{Error, Result};
use std::path::Path;

/// One upgrade: brings a directory at format `from` to `from + 1`. It runs with the directory locked and not yet open.
pub struct Migration {
    pub from: u32,
    pub describe: &'static str,
    pub run: fn(&Path) -> Result<()>,
}

/// Layout 2 adds the `buckets` and `objects` keyspaces. They are created empty when the database opens, so there is nothing to rewrite: the step
/// exists so that a layout-1 directory is copied aside and then marked layout 2, and so an older binary, which would not count objects as references
/// to files, refuses the directory instead of mis-reading it.
fn v1_to_v2(_path: &Path) -> Result<()> {
    Ok(())
}

pub const MIGRATIONS: &[Migration] = &[Migration {
    from: 1,
    describe: "adds buckets and objects (new, empty keyspaces)",
    run: v1_to_v2,
}];

fn write_version(path: &Path, v: u32) -> Result<()> {
    let tmp = path.join("FORMAT.tmp");
    std::fs::write(&tmp, format!("{v}\n"))?;
    std::fs::rename(&tmp, path.join("FORMAT"))?;
    Ok(())
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let target = to.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir(&e.path(), &target)?;
        } else {
            std::fs::copy(e.path(), target)?;
        }
    }
    Ok(())
}

/// Makes the directory's layout `current`: records it for a new directory, refuses a newer one, upgrades an older one.
pub fn ensure(path: &Path, current: u32, migrations: &[Migration]) -> Result<()> {
    let file = path.join("FORMAT");
    let found =
        match std::fs::read_to_string(&file) {
            Ok(text) => Some(text.trim().parse::<u32>().map_err(|_| {
                Error::Corrupt(format!("{} is not a version number", file.display()))
            })?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
    let from = match found {
        Some(v) => v,
        // No marker: a new directory, or one written before the marker existed (that was format 1).
        None if path.join("meta").exists() => {
            write_version(path, 1)?;
            1
        }
        None => {
            write_version(path, current)?;
            return Ok(());
        }
    };
    if from == current {
        return Ok(());
    }
    if from > current {
        return Err(Error::Invalid(format!(
            "{} uses data format {from}, but this PowerDB understands up to {current}; run a newer PowerDB",
            path.display()
        )));
    }
    // refuse before changing anything if the chain has a gap
    for v in from..current {
        if !migrations.iter().any(|m| m.from == v) {
            return Err(Error::Invalid(format!(
                "{} is at data format {from} and this PowerDB ({current}) has no upgrade for format {v}; use the release that matches the data",
                path.display()
            )));
        }
    }
    let safety = path.join(format!("pre-migration-v{from}"));
    if !safety.exists() {
        // build it aside and move it into place, so a crash cannot leave a half-copied "safety copy" that looks complete
        let building = path.join(format!("pre-migration-v{from}.partial"));
        let _ = std::fs::remove_dir_all(&building);
        copy_dir(&path.join("meta"), &building.join("meta"))?;
        std::fs::copy(&file, building.join("FORMAT"))?;
        std::fs::rename(&building, &safety)?;
    }
    for v in from..current {
        let step = migrations
            .iter()
            .find(|m| m.from == v)
            .expect("checked above");
        tracing::info!("upgrading data format {v} -> {}: {}", v + 1, step.describe);
        (step.run)(path).map_err(|e| {
            Error::Invalid(format!(
                "the upgrade from data format {v} failed ({e}). The data is still at format {v}; a copy of the metadata from before the upgrade is in {}. Fix the cause and open it again to resume",
                safety.display()
            ))
        })?;
        write_version(path, v + 1)?;
    }
    tracing::info!(
        "data format is now {current}; the pre-upgrade metadata copy is in {}",
        safety.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "pdb-format-{tag}-{}-{}",
            std::process::id(),
            crate::util::new_id()
        ));
        std::fs::create_dir_all(d.join("meta")).unwrap();
        std::fs::write(d.join("meta/data.bin"), b"original").unwrap();
        d
    }
    fn version(d: &Path) -> u32 {
        std::fs::read_to_string(d.join("FORMAT"))
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }
    fn step_a(d: &Path) -> Result<()> {
        std::fs::write(d.join("a"), b"1")?;
        Ok(())
    }
    fn step_b(d: &Path) -> Result<()> {
        assert!(d.join("a").exists(), "steps run in order");
        std::fs::write(d.join("b"), b"1")?;
        Ok(())
    }
    static BROKEN: AtomicU32 = AtomicU32::new(1);
    fn step_b_flaky(d: &Path) -> Result<()> {
        if BROKEN.load(Ordering::SeqCst) == 1 {
            return Err(Error::Invalid("disk full".into()));
        }
        step_b(d)
    }
    const CHAIN: &[Migration] = &[
        Migration {
            from: 1,
            describe: "a",
            run: step_a,
        },
        Migration {
            from: 2,
            describe: "b",
            run: step_b,
        },
    ];

    #[test]
    fn an_older_directory_is_upgraded_in_order_with_a_safety_copy() {
        let d = dir("ok");
        write_version(&d, 1).unwrap();
        ensure(&d, 3, CHAIN).unwrap();
        assert_eq!(version(&d), 3);
        assert!(d.join("a").exists() && d.join("b").exists());
        assert_eq!(
            std::fs::read(d.join("pre-migration-v1/meta/data.bin")).unwrap(),
            b"original"
        );
        assert_eq!(
            std::fs::read_to_string(d.join("pre-migration-v1/FORMAT"))
                .unwrap()
                .trim(),
            "1"
        );
        ensure(&d, 3, CHAIN).unwrap(); // already current: nothing runs again
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_failed_step_stops_there_says_where_the_copy_is_and_resumes() {
        let d = dir("resume");
        write_version(&d, 1).unwrap();
        BROKEN.store(1, Ordering::SeqCst);
        let chain = [
            Migration {
                from: 1,
                describe: "a",
                run: step_a,
            },
            Migration {
                from: 2,
                describe: "b",
                run: step_b_flaky,
            },
        ];
        let err = ensure(&d, 3, &chain).unwrap_err().to_string();
        assert!(
            err.contains("format 2")
                && err.contains("pre-migration-v1")
                && err.contains("disk full"),
            "{err}"
        );
        assert_eq!(version(&d), 2, "the first step stays done");
        BROKEN.store(0, Ordering::SeqCst);
        ensure(&d, 3, &chain).unwrap();
        assert_eq!(version(&d), 3);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_gap_in_the_chain_changes_nothing() {
        let d = dir("gap");
        write_version(&d, 1).unwrap();
        let err = ensure(&d, 3, &CHAIN[1..]).unwrap_err().to_string();
        assert!(err.contains("no upgrade for format 1"), "{err}");
        assert_eq!(version(&d), 1);
        assert!(!d.join("pre-migration-v1").exists() && !d.join("a").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_newer_directory_is_refused_and_an_unmarked_one_is_adopted() {
        let d = dir("newer");
        write_version(&d, 9).unwrap();
        assert!(
            ensure(&d, 3, CHAIN)
                .unwrap_err()
                .to_string()
                .contains("understands up to 3")
        );
        assert_eq!(version(&d), 9);
        // written before the marker existed (it has metadata): that was format 1, and gets upgraded
        std::fs::remove_file(d.join("FORMAT")).unwrap();
        ensure(&d, 3, CHAIN).unwrap();
        assert_eq!(version(&d), 3);
        // a new, empty directory is simply stamped
        let fresh =
            std::env::temp_dir().join(format!("pdb-format-fresh-{}", crate::util::new_id()));
        std::fs::create_dir_all(&fresh).unwrap();
        ensure(&fresh, 3, CHAIN).unwrap();
        assert_eq!(version(&fresh), 3);
        assert!(!fresh.join("a").exists());
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&fresh);
    }
}
