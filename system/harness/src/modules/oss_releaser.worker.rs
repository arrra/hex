//! `oss-releaser` — GitFlow release worker (oss-releaser spec, scope item 6).
//!
//! Deliberately unprefixed: the product name IS the worker name. Listens for
//! `release.requested` events and spawns the release ceremony (`hex release
//! cut`) as a DETACHED child of the current executable.
//!
//! ## Why detached — the real drain semantics
//!
//! The worker runtime's graceful drain is bounded at 30 seconds
//! (`DRAIN_TIMEOUT` in `worker/runtime.rs`). A release ceremony takes
//! minutes — run in-process it would die with any harness restart, worst
//! case between push and tag. So the handler spawns the ceremony as a
//! detached child in its own process group (`CommandExt::process_group(0)`)
//! of the CURRENT executable (`std::env::current_exe`, never a PATH lookup)
//! and returns `Ok` immediately: the child survives a harness restart and
//! finishes the release. The handler's telemetry only covers the spawn (the
//! runtime auto-records the handler invocation); the child records its own
//! pipeline events (`release::gate::<name>`, `release::cut`). Concurrent
//! triggers are serialized by the ceremony's exclusive lock
//! (`<git-common-dir>/hex-release.lock`), not by this handler.
//!
//! ## Event contract
//!
//! - `repo_dir` (required): the repo to release. Missing or nonexistent →
//!   one loud error and `Err` — never a silent no-op (S6).
//! - `level` (optional): patch | minor | major; defaults to patch.
//!
//! Child stdout+stderr go to a timestamped log file under the repo's git
//! common dir (safe from the clean-tree gate).

use anyhow::Context;
use hex::worker::{ctx::Ctx, event::Event, Result, Worker};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Event that triggers a release cut.
pub const EVENT_RELEASE_REQUESTED: &str = "release.requested";

/// Resolve the repo's git common dir (absolute) — where the child's log
/// file lands. Also serves as proof that `repo_dir` is a git repo.
fn git_common_dir(repo: &PathBuf) -> Result<PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(repo)
        .output()
        .context("oss-releaser: running git rev-parse --git-common-dir")?;
    if !out.status.success() {
        anyhow::bail!(
            "oss-releaser: {} is not a git repository: {}",
            repo.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let raw = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let p = PathBuf::from(&raw);
    Ok(if p.is_absolute() { p } else { repo.join(p) })
}

fn run_release(e: Event, _ctx: Ctx) -> Result<()> {
    let data = e.data();

    // repo_dir is required — no fallback guessing (S6: loud, never silent).
    let repo_dir = match data.str("repo_dir") {
        Ok(d) => d.to_string(),
        Err(err) => {
            eprintln!("oss-releaser: release.requested rejected — {err}");
            return Err(err);
        }
    };
    let repo = PathBuf::from(&repo_dir);
    if !repo.is_dir() {
        eprintln!("oss-releaser: release.requested rejected — repo_dir '{repo_dir}' does not exist or is not a directory");
        anyhow::bail!("oss-releaser: repo_dir '{repo_dir}' does not exist or is not a directory");
    }

    // level is optional, default patch; validate eagerly so a typo fails
    // loudly at the trigger instead of inside a detached child's log.
    let level = match data.raw().get("level") {
        None => "patch".to_string(),
        Some(v) => match v.as_str() {
            Some(s) => s.to_string(),
            None => {
                eprintln!("oss-releaser: release.requested rejected — `level` is not a string");
                anyhow::bail!("oss-releaser: `level` in event data is not a string");
            }
        },
    };
    if let Err(err) = level.parse::<hex::release::BumpLevel>() {
        eprintln!("oss-releaser: release.requested rejected — {err}");
        return Err(err);
    }

    let git_dir = match git_common_dir(&repo) {
        Ok(d) => d,
        Err(err) => {
            eprintln!("{err:#}");
            return Err(err);
        }
    };
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let log_path = git_dir.join(format!("hex-release-cut-{ts}.log"));
    let log = std::fs::File::create(&log_path)
        .with_context(|| format!("oss-releaser: creating log file {}", log_path.display()))?;
    let log_err = log
        .try_clone()
        .context("oss-releaser: cloning log handle for stderr")?;

    // The CURRENT executable — never a PATH lookup (the deployed `hex` on
    // PATH may be older than the harness that received the event).
    let exe = std::env::current_exe().context("oss-releaser: resolving current executable")?;

    let child = Command::new(&exe)
        .args(["release", "cut", "--level", &level])
        .current_dir(&repo)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .process_group(0) // own process group — survives the harness's SIGTERM
        .spawn()
        .with_context(|| {
            format!("oss-releaser: spawning detached `hex release cut` in {repo_dir}")
        })?;

    eprintln!(
        "oss-releaser: spawned detached `hex release cut --level {level}` (pid {}) in {repo_dir}; log: {}",
        child.id(),
        log_path.display()
    );

    // Reap the child when it exits. Without this the Child handle drops
    // un-waited and every finished ceremony leaves a ZOMBIE in the
    // long-lived harness until restart (oss-releaser review nonblocker,
    // 2026-06-11). The exit callback doubles as the loud "the ceremony
    // ENDED, and how" record — previously only the child's own log knew.
    {
        let level = level.clone();
        let repo_dir = repo_dir.to_string();
        let log_display = log_path.display().to_string();
        reap_in_background(child, move |status| {
            hex::telemetry::record_loud(&hex::telemetry::TelemetryEvent {
                source: "oss-releaser".into(),
                event: "release::child-exit".into(),
                status: if status.success() { "ok" } else { "error" }.into(),
                duration_ms: None,
                exit_code: status.code().map(i64::from),
                detail: Some(format!(
                    "level={level} repo={repo_dir} log={log_display}"
                )),
            });
        });
    }
    Ok(())
}

/// Wait on the detached ceremony child from a background thread so it never
/// zombifies, invoking `on_exit` with its exit status. The thread dies with
/// the harness (the child survives detached, re-parented to init, which
/// reaps it — the zombie risk exists only while the harness outlives the
/// child without waiting).
fn reap_in_background(
    mut child: std::process::Child,
    on_exit: impl FnOnce(std::process::ExitStatus) + Send + 'static,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let pid = child.id();
        match child.wait() {
            Ok(status) => on_exit(status),
            Err(e) => eprintln!("oss-releaser: wait on ceremony child pid={pid} failed: {e}"),
        }
    })
}

/// Build the `oss-releaser` worker.
pub fn worker() -> Worker {
    Worker::new("oss-releaser").on_event(EVENT_RELEASE_REQUESTED, run_release)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reaper_waits_child_and_reports_exit_status() {
        let child = Command::new("/bin/sh")
            .args(["-c", "exit 7"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn test child");
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = reap_in_background(child, move |status| {
            tx.send(status.code()).expect("report exit");
        });
        let code = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("reaper must report within 10s");
        assert_eq!(code, Some(7));
        handle.join().expect("reaper thread exits cleanly");
    }
}
