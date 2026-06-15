//! `hex-codeintel-indexer` — nightly reindex of every registered code-intel
//! workspace (migrated from the hand-rolled
//! `system/templates/launchd/com.hex.codeintel-indexer.plist` LaunchAgents,
//! one-per-workspace, to a single harness cron worker).
//!
//! - id `hex::codeintel::index_all`  cron `0 30 2 * * * *` (02:30 daily)
//!
//! Behavior contract:
//! - Loads the codeintel `Registry` from `$CODEINTEL_HOME` (default
//!   `~/.codeintel`) — same resolution as the `cq` CLI.
//! - Indexes workspaces **SEQUENTIALLY**. `rust-analyzer scip` emits are
//!   ~3GB transient RSS each (docs/code-intel.md); running them concurrently
//!   is forbidden by design.
//! - `SkippedInFlight` is a visible log line, not an error (spec §7 — never
//!   silent, never fatal).
//! - A per-workspace failure is logged loudly (telemetry captures handler
//!   stderr) but the loop CONTINUES to the remaining workspaces; afterwards
//!   the handler returns an `Err` summarizing every failure, so a partial
//!   success is never reported as a clean run (Standing Order S6).

use std::path::Path;

use hex::worker::{ctx::Ctx, event::Event, Result, Worker};
use scipd_core::indexer::{self, IndexOutcome};
use scipd_core::workspace::{codeintel_home, Registry};

/// Cron expression for the nightly reindex — 02:30 daily (the same schedule
/// the retired launchd plist used; 7-field: sec min hour dom mon dow year).
pub const CRON_NIGHTLY: &str = "0 30 2 * * * *";

fn run_nightly(_e: Event, _ctx: Ctx) -> Result<()> {
    let home = codeintel_home().map_err(|e| anyhow::anyhow!("{e}"))?;
    index_all(&home)
}

/// Index every workspace registered under `home`, sequentially.
///
/// Separated from the cron stub so tests can point `home` at a tempdir
/// without mutating process-global env (not thread-safe under `cargo test`).
fn index_all(home: &Path) -> Result<()> {
    let registry = Registry::load(home).map_err(|e| anyhow::anyhow!("{e}"))?;
    let entries = registry.entries();
    if entries.is_empty() {
        // No-op is loud, mirroring hex-reconciler's absent-source convention:
        // log + Ok, never a silent empty run.
        eprintln!(
            "codeintel-indexer: no workspaces registered in {} — nothing to index",
            home.display()
        );
        return Ok(());
    }

    let mut failures: Vec<String> = Vec::new();
    // SEQUENTIAL by design — each emit is ~3GB transient RSS; never overlap.
    for entry in entries {
        match indexer::run(home, &entry.root) {
            Ok(IndexOutcome::Completed(report)) => {
                eprintln!(
                    "codeintel-indexer: {} ({}) published gen {} — {} files, {} symbols",
                    entry.id,
                    entry.root.display(),
                    report.generation,
                    report.file_count,
                    report.symbol_count
                );
            }
            Ok(IndexOutcome::SkippedInFlight) => {
                // Visible, not an error: another emit holds the store lock.
                eprintln!(
                    "codeintel-indexer: {} ({}) skipped — emit in flight",
                    entry.id,
                    entry.root.display()
                );
            }
            Err(e) => {
                // Loud per-workspace failure, but CONTINUE: one broken
                // workspace must not starve the rest of their nightly index.
                eprintln!(
                    "codeintel-indexer: {} ({}) FAILED: {e:#}",
                    entry.id,
                    entry.root.display()
                );
                failures.push(format!("{} ({}): {e:#}", entry.id, entry.root.display()));
            }
        }
    }

    if !failures.is_empty() {
        anyhow::bail!(
            "codeintel-indexer: {}/{} workspace(s) failed:\n{}",
            failures.len(),
            entries.len(),
            failures.join("\n")
        );
    }
    Ok(())
}

/// Build the `hex-codeintel-indexer` worker.
pub fn worker() -> Worker {
    Worker::new("hex-codeintel-indexer").on_cron_named("nightly", CRON_NIGHTLY, run_nightly)
}

#[cfg(test)]
mod tests {
    use super::*;
    use scipd_core::store::Store;
    use scipd_core::workspace::{register_workspace, Workspace};
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn run_cmd(cwd: &Path, prog: &str, args: &[&str]) {
        let out = std::process::Command::new(prog)
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap_or_else(|e| panic!("spawning {prog}: {e}"));
        assert!(
            out.status.success(),
            "{prog} {args:?} failed\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn copy_dir(src: &Path, dst: &Path) {
        for entry in std::fs::read_dir(src).unwrap() {
            let entry = entry.unwrap();
            let to = dst.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                std::fs::create_dir_all(&to).unwrap();
                copy_dir(&entry.path(), &to);
            } else {
                std::fs::copy(entry.path(), &to).unwrap();
            }
        }
    }

    /// The shared golden fixture crate (`system/code-intel/tests/fixtures/
    /// golden-crate`) copied to a tempdir, git-initialized + committed — the
    /// same helper pattern the scipd indexer tests use.
    fn golden_repo() -> TempDir {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../code-intel/tests/fixtures/golden-crate");
        let dir = tempfile::tempdir().unwrap();
        copy_dir(&fixture, dir.path());
        run_cmd(dir.path(), "git", &["init", "-q", "-b", "main"]);
        run_cmd(dir.path(), "git", &["add", "-A"]);
        run_cmd(
            dir.path(),
            "git",
            &[
                "-c",
                "user.email=ci@test",
                "-c",
                "user.name=ci-test",
                "commit",
                "-q",
                "-m",
                "golden",
            ],
        );
        dir
    }

    /// Git repo with a Cargo.toml (registrable) that we can break by deleting.
    fn throwaway_repo() -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        run_cmd(dir.path(), "git", &["init", "-q", "-b", "main"]);
        run_cmd(dir.path(), "git", &["add", "-A"]);
        run_cmd(
            dir.path(),
            "git",
            &[
                "-c",
                "user.email=ci@test",
                "-c",
                "user.name=ci-test",
                "commit",
                "-q",
                "-m",
                "init",
            ],
        );
        dir
    }

    #[test]
    fn empty_registry_is_loud_noop_ok() {
        // CODEINTEL_HOME-equivalent tempdir with no registry.toml: log + Ok,
        // matching the hex-reconciler absent-source convention.
        let home = tempfile::tempdir().unwrap();
        index_all(home.path()).expect("empty registry must be Ok, not Err");
    }

    #[test]
    fn worker_registers_nightly_cron() {
        let w = worker();
        assert_eq!(w.name, "hex-codeintel-indexer");
        assert_eq!(w.handlers.len(), 1, "exactly one trigger");
        assert_eq!(
            w.handlers[0].1,
            hex::worker::TriggerSpec::Cron {
                expression: CRON_NIGHTLY.to_string()
            }
        );
    }

    /// End-to-end on the golden fixture, plus the failure-continuation and
    /// skip-in-flight contracts (one test — the real `rust-analyzer scip`
    /// emit takes seconds, so we exercise all three claims on one run).
    #[test]
    fn golden_fixture_indexes_continues_past_failures_and_skips_in_flight() {
        let home = tempfile::tempdir().unwrap();

        // Workspace 1: registered, then deleted → its index run must FAIL.
        let broken = throwaway_repo();
        register_workspace(home.path(), broken.path()).unwrap();
        let broken_path = broken.path().to_path_buf();
        drop(broken); // TempDir drop removes the directory

        // Workspace 2: the golden fixture → must index despite ws1 failing.
        let golden = golden_repo();
        register_workspace(home.path(), golden.path()).unwrap();
        let golden_id = Workspace::resolve(golden.path()).unwrap().id;

        // Broken workspace is FIRST in the registry: the loop must continue
        // past its failure and still index golden, then summarize the failure.
        let err = index_all(home.path()).expect_err("partial failure must be Err");
        let msg = format!("{err:#}");
        assert!(msg.contains("1/2 workspace(s) failed"), "got: {msg}");
        assert!(
            msg.contains(&broken_path.display().to_string()),
            "failure summary should name the broken root, got: {msg}"
        );

        // Golden workspace got a published generation despite the failure.
        let store = Store::new(home.path(), &golden_id);
        let published = store
            .current()
            .unwrap()
            .expect("golden workspace must be indexed despite earlier failure");
        assert!(!published.is_empty());

        // In-flight lock on golden: visible skip, NOT an error (the broken
        // workspace still fails, so index_all stays Err — but the golden
        // outcome is SkippedInFlight, proven by the generation not advancing).
        let _held = store.try_lock().unwrap().expect("lock free after run");
        let err2 = index_all(home.path()).expect_err("broken ws still fails");
        let msg2 = format!("{err2:#}");
        assert!(
            msg2.contains("1/2 workspace(s) failed"),
            "skip must not count as a failure, got: {msg2}"
        );
        assert_eq!(
            store.current().unwrap().unwrap(),
            published,
            "skipped-in-flight run must not publish a new generation"
        );
    }
}
