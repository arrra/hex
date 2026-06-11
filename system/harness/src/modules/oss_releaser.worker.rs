//! `oss-releaser` — GitFlow release worker (oss-releaser spec, scope item 6).
//!
//! Deliberately unprefixed: the product name IS the worker name. Two
//! triggers, one ceremony:
//!
//! - **`release.requested` event** — the manual escape hatch. Spawns the
//!   release ceremony (`hex release cut`) for the repo named in the event.
//! - **Branch watch cron** (every 5 minutes, [`CRON_WATCH`]) — polls origin
//!   `release/*` and `hotfix/*` heads of every watched releases.toml profile
//!   (`watch = true` + `repo_dir`) and spawns the finish ceremony
//!   (`hex release cut --finish <branch>`) for any head it has not seen
//!   before. A brand-new branch counts as new commits.
//!
//! Both paths spawn the ceremony as a DETACHED child of the current
//! executable.
//!
//! ## Why detached — the real drain semantics
//!
//! The worker runtime's graceful drain is bounded at 30 seconds
//! (`DRAIN_TIMEOUT` in `worker/runtime.rs`). A release ceremony takes
//! minutes — run in-process it would die with any harness restart, worst
//! case between push and tag. So the handlers spawn the ceremony as a
//! detached child in its own process group (`CommandExt::process_group(0)`)
//! of the CURRENT executable (`std::env::current_exe`, never a PATH lookup)
//! and return `Ok` immediately: the child survives a harness restart and
//! finishes the release. The handler's telemetry only covers the spawn (the
//! runtime auto-records the handler invocation); the child records its own
//! pipeline events (`release::gate::<name>`, `release::cut`). Concurrent
//! triggers are serialized by the ceremony's exclusive lock
//! (`<git-common-dir>/hex-release.lock`), not by these handlers.
//!
//! ## Event contract
//!
//! - `repo_dir` (required): the repo to release. Missing or nonexistent →
//!   one loud error and `Err` — never a silent no-op (S6).
//! - `level` (optional): patch | minor | major; defaults to patch.
//!
//! ## Branch watch — idempotency design
//!
//! Last-seen origin SHAs persist per (profile, branch) in the
//! `oss_releaser_seen` table of the harness runtime-state db
//! (`module_state::db_path`, `$HEX_DIR/.hex/harness/state.db`), so the watch
//! survives harness restarts. The loop never re-triggers on SHAs produced by
//! an in-flight or completed ceremony it spawned:
//!
//! 1. The observed SHA is persisted BEFORE the ceremony child spawns — one
//!    observed (profile, branch, sha) spawns at most one ceremony, across
//!    ticks and restarts. A failed spawn rolls the row back (loudly) so the
//!    next tick retries; a failed CEREMONY is not auto-retried (its branch
//!    head never moved — the loud `release::child-exit` record plus the
//!    manual event path own that).
//! 2. A ceremony never moves its branch on origin mid-run (it pushes
//!    main/develop/tag at the end and DELETES the branch on success), so no
//!    ceremony-produced SHA ever shows up as a new head. A vanished branch's
//!    row is pruned, so a future same-named branch counts as brand new.
//! 3. While the ceremony lock (`hex::release::lock_file_path`) is held, the
//!    repo's poll is deferred entirely — no triggers, no state writes;
//!    pushes landing mid-ceremony are reconsidered next tick instead of
//!    spawning a child doomed to die on the lock. The lock still serializes
//!    any concurrent cut that slips through the check-then-spawn window.
//! 4. At most one trigger per repo per tick; further new branches are
//!    deferred (state untouched) to later ticks — the lock would reject
//!    them anyway.
//!
//! Failures are loud and per-repo isolated (S6): a repo that fails to poll
//! (ls-remote failure, unreadable seen-state, missing repo_dir, spawn
//! failure) logs to stderr + telemetry (`release::watch`, status error) and
//! never blocks polling the others. Triggers record
//! `release::watch::trigger`.
//!
//! Child stdout+stderr go to a timestamped log file under the repo's git
//! common dir (safe from the clean-tree gate).

use anyhow::Context;
use hex::worker::{ctx::Ctx, event::Event, Result, Worker};
use std::collections::BTreeMap;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Event that triggers a release cut.
pub const EVENT_RELEASE_REQUESTED: &str = "release.requested";

/// Cron for the branch watch — every 5 minutes (7-field iii cron:
/// sec min hour dom mon dow year).
pub const CRON_WATCH: &str = "0 */5 * * * * *";

/// The ceremony trigger seam: `(repo_root, finish_branch)`. Production
/// spawns a detached `hex release cut --finish <branch>`; tests inject a
/// recorder so nothing ever execs the test binary.
type SpawnFn<'a> = &'a dyn Fn(&Path, &str) -> Result<()>;

/// Resolve the repo's git common dir (absolute) — where the child's log
/// file lands. Also serves as proof that `repo_dir` is a git repo.
fn git_common_dir(repo: &Path) -> Result<PathBuf> {
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

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// One loud telemetry record from the watch loop.
fn record_watch(event: &str, status: &str, detail: String) {
    hex::telemetry::record_loud(&hex::telemetry::TelemetryEvent {
        source: "oss-releaser".into(),
        event: event.into(),
        status: status.into(),
        duration_ms: None,
        exit_code: None,
        detail: Some(detail),
    });
}

/// Spawn the release ceremony (`hex release cut <args>`) as a detached
/// child working in `repo`, logging to
/// `<git-common-dir>/hex-release-cut-<ts>.log`, and reap it from a
/// background thread with a loud `release::child-exit` telemetry record
/// (`detail_prefix` names the trigger, e.g. `level=patch` or
/// `finish=release/1.2.0`). Shared by the manual event handler and the
/// branch watch.
fn spawn_detached_cut(repo: &Path, args: &[String], detail_prefix: &str) -> Result<()> {
    let git_dir = git_common_dir(repo)?;
    let log_path = git_dir.join(format!("hex-release-cut-{}.log", unix_now()));
    let log = std::fs::File::create(&log_path)
        .with_context(|| format!("oss-releaser: creating log file {}", log_path.display()))?;
    let log_err = log
        .try_clone()
        .context("oss-releaser: cloning log handle for stderr")?;

    // The CURRENT executable — never a PATH lookup (the deployed `hex` on
    // PATH may be older than the harness that took the trigger).
    let exe = std::env::current_exe().context("oss-releaser: resolving current executable")?;

    let mut argv: Vec<String> = vec!["release".to_string(), "cut".to_string()];
    argv.extend(args.iter().cloned());
    let child = Command::new(&exe)
        .args(&argv)
        .current_dir(repo)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .process_group(0) // own process group — survives the harness's SIGTERM
        .spawn()
        .with_context(|| {
            format!(
                "oss-releaser: spawning detached `hex release cut {}` in {}",
                args.join(" "),
                repo.display()
            )
        })?;

    eprintln!(
        "oss-releaser: spawned detached `hex release cut {}` (pid {}) in {}; log: {}",
        args.join(" "),
        child.id(),
        repo.display(),
        log_path.display()
    );

    // Reap the child when it exits. Without this the Child handle drops
    // un-waited and every finished ceremony leaves a ZOMBIE in the
    // long-lived harness until restart (oss-releaser review nonblocker,
    // 2026-06-11). The exit callback doubles as the loud "the ceremony
    // ENDED, and how" record — previously only the child's own log knew.
    let detail = format!(
        "{detail_prefix} repo={} log={}",
        repo.display(),
        log_path.display()
    );
    reap_in_background(child, move |status| {
        hex::telemetry::record_loud(&hex::telemetry::TelemetryEvent {
            source: "oss-releaser".into(),
            event: "release::child-exit".into(),
            status: if status.success() { "ok" } else { "error" }.into(),
            duration_ms: None,
            exit_code: status.code().map(i64::from),
            detail: Some(detail),
        });
    });
    Ok(())
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

    if let Err(err) = spawn_detached_cut(
        &repo,
        &["--level".to_string(), level.clone()],
        &format!("level={level}"),
    ) {
        eprintln!("{err:#}");
        return Err(err);
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

// ---------------------------------------------------------------------------
// Branch watch — seen-state store.
//
// Same doctrine as `module_state`: opaque runtime state lives in the
// harness-owned SQLite db (`module_state::db_path`), never under
// `.hex/config/`; `Result<_, String>` errors so callers phrase the loud
// failure; open creates dir/db/table idempotently.
// ---------------------------------------------------------------------------

fn seen_open(hex_dir: &Path) -> std::result::Result<rusqlite::Connection, String> {
    let p = hex::module_state::db_path(hex_dir);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let conn = rusqlite::Connection::open(&p)
        .map_err(|e| format!("cannot open {}: {e}", p.display()))?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS oss_releaser_seen (
            profile    TEXT NOT NULL,
            branch     TEXT NOT NULL,
            sha        TEXT NOT NULL,
            updated_at INTEGER NOT NULL,
            PRIMARY KEY (profile, branch)
        );",
    )
    .map_err(|e| format!("seen-state schema ({}): {e}", p.display()))?;
    Ok(conn)
}

/// All last-seen `branch → sha` rows for one profile.
fn seen_load(
    hex_dir: &Path,
    profile: &str,
) -> std::result::Result<BTreeMap<String, String>, String> {
    let conn = seen_open(hex_dir)?;
    let mut stmt = conn
        .prepare("SELECT branch, sha FROM oss_releaser_seen WHERE profile = ?1")
        .map_err(|e| format!("seen-state query: {e}"))?;
    let rows = stmt
        .query_map([profile], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(|e| format!("seen-state query: {e}"))?;
    let mut out = BTreeMap::new();
    for row in rows {
        let (branch, sha) = row.map_err(|e| format!("seen-state row: {e}"))?;
        out.insert(branch, sha);
    }
    Ok(out)
}

/// Upsert one last-seen sha.
fn seen_record(
    hex_dir: &Path,
    profile: &str,
    branch: &str,
    sha: &str,
) -> std::result::Result<(), String> {
    let conn = seen_open(hex_dir)?;
    conn.execute(
        "INSERT INTO oss_releaser_seen (profile, branch, sha, updated_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (profile, branch) DO UPDATE SET sha = ?3, updated_at = ?4",
        rusqlite::params![profile, branch, sha, unix_now()],
    )
    .map_err(|e| format!("seen-state write: {e}"))?;
    Ok(())
}

/// Forget a branch (it vanished from origin).
fn seen_forget(hex_dir: &Path, profile: &str, branch: &str) -> std::result::Result<(), String> {
    let conn = seen_open(hex_dir)?;
    conn.execute(
        "DELETE FROM oss_releaser_seen WHERE profile = ?1 AND branch = ?2",
        rusqlite::params![profile, branch],
    )
    .map_err(|e| format!("seen-state delete: {e}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Branch watch — pure planning (unit-tested; no IO).
// ---------------------------------------------------------------------------

/// One tick's decisions for one watched repo, computed purely from the
/// persisted last-seen map and the live origin heads. See the module doc's
/// "Branch watch — idempotency design" for the contract each field serves.
#[derive(Debug, Default, PartialEq, Eq)]
struct WatchPlan {
    /// The single `(branch, sha)` to finish this tick — persisted to the
    /// seen-store BEFORE the ceremony spawns.
    trigger: Option<(String, String)>,
    /// Further new heads this tick — untouched, later ticks pick them up.
    deferred: Vec<String>,
    /// Heads matching the watch pattern whose names are not finishable
    /// (`{release|hotfix}/X.Y.Z`) — warned loudly once per observed sha.
    unfinishable: Vec<(String, String)>,
    /// Seen branches that vanished from origin (ceremony completed or
    /// deleted by hand) — their rows are dropped.
    prune: Vec<String>,
}

/// Diff the persisted last-seen map against the live origin heads.
fn plan_watch(seen: &BTreeMap<String, String>, live: &[(String, String)]) -> WatchPlan {
    let mut plan = WatchPlan::default();
    for (branch, sha) in live {
        if seen.get(branch) == Some(sha) {
            continue; // already handled — this is the idempotency core
        }
        if hex::release::parse_finish_branch(branch).is_err() {
            plan.unfinishable.push((branch.clone(), sha.clone()));
        } else if plan.trigger.is_none() {
            plan.trigger = Some((branch.clone(), sha.clone()));
        } else {
            plan.deferred.push(branch.clone());
        }
    }
    plan.prune = seen
        .keys()
        .filter(|branch| !live.iter().any(|(b, _)| b == *branch))
        .cloned()
        .collect();
    plan
}

// ---------------------------------------------------------------------------
// Branch watch — per-repo tick and the cron handler.
// ---------------------------------------------------------------------------

/// Poll one watched profile and trigger at most one finish ceremony.
/// Returns `Ok(None)` for a quiet tick (every head already seen) or
/// `Ok(Some(summary))` when something happened; every `Err` is one repo's
/// loud failure — the caller isolates it from the other repos.
fn watch_repo(
    hex_dir: &Path,
    profile: &hex::release::ReleaseProfile,
    spawn: SpawnFn,
) -> Result<Option<String>> {
    let name = profile.name.as_str();
    let repo = profile.repo_dir.as_deref().with_context(|| {
        format!(
            "profile `{name}`: watch = true but repo_dir is unset — the loader \
             should have refused this config"
        )
    })?;
    if !repo.is_dir() {
        anyhow::bail!(
            "profile `{name}`: repo_dir {} does not exist or is not a directory",
            repo.display()
        );
    }

    // Idempotency #3: a held ceremony lock defers the WHOLE repo — no
    // triggers, no state writes — until the in-flight cut finishes.
    let lock = hex::release::lock_file_path(repo)
        .with_context(|| format!("profile `{name}`: resolving the ceremony lock path"))?;
    if lock.exists() {
        record_watch(
            "release::watch",
            "skipped",
            format!("profile={name} deferred — ceremony in flight (lock {})", lock.display()),
        );
        return Ok(Some(format!(
            "deferred — ceremony in flight (lock {})",
            lock.display()
        )));
    }

    let heads = hex::release::ls_remote_watch_heads(repo)
        .with_context(|| format!("profile `{name}`: polling origin heads from {}", repo.display()))?;
    let seen = seen_load(hex_dir, name).map_err(|e| {
        anyhow::anyhow!("profile `{name}`: seen-state unreadable — refusing to trigger blind: {e}")
    })?;
    let plan = plan_watch(&seen, &heads);

    // Unfinishable names matching the watch pattern: warn loudly ONCE per
    // observed sha (persist it) instead of every 5 minutes forever.
    for (branch, sha) in &plan.unfinishable {
        eprintln!(
            "oss-releaser watch: profile `{name}`: origin branch `{branch}` matches the \
             watch pattern but is not finishable ({{release|hotfix}}/X.Y.Z) — ignoring \
             until its head moves"
        );
        record_watch(
            "release::watch",
            "skipped",
            format!("profile={name} branch={branch} sha={sha} unfinishable"),
        );
        seen_record(hex_dir, name, branch, sha).map_err(|e| {
            anyhow::anyhow!("profile `{name}`: persisting unfinishable {branch}: {e}")
        })?;
    }

    // Idempotency #2: forget vanished branches so a future same-named
    // branch counts as brand new.
    for branch in &plan.prune {
        seen_forget(hex_dir, name, branch).map_err(|e| {
            anyhow::anyhow!("profile `{name}`: pruning vanished {branch}: {e}")
        })?;
    }

    if let Some((branch, sha)) = &plan.trigger {
        // Idempotency #1: persist FIRST — this sha must never double-fire,
        // even if the harness dies between this write and the spawn.
        seen_record(hex_dir, name, branch, sha).map_err(|e| {
            anyhow::anyhow!(
                "profile `{name}`: cannot persist seen sha for {branch} — NOT spawning \
                 (a spawn without the record could double-fire): {e}"
            )
        })?;
        if let Err(err) = spawn(repo, branch) {
            // Roll the row back so the next tick retries; if even that
            // fails, this sha is lost to the watch — say so, loudly.
            let rollback = match seen_forget(hex_dir, name, branch) {
                Ok(()) => "state rolled back — next tick retries".to_string(),
                Err(forget) => format!(
                    "state rollback ALSO failed ({forget}) — {branch}@{sha} will not \
                     re-trigger; use the release.requested event or run \
                     `hex release cut --finish {branch}` by hand"
                ),
            };
            return Err(err).with_context(|| {
                format!("profile `{name}`: spawning finish ceremony for {branch} ({rollback})")
            });
        }
        record_watch(
            "release::watch::trigger",
            "ok",
            format!(
                "profile={name} branch={branch} sha={sha} repo={}",
                repo.display()
            ),
        );
        return Ok(Some(format!(
            "triggered finish ceremony for {branch}@{} ({} deferred, {} pruned)",
            &sha[..sha.len().min(12)],
            plan.deferred.len(),
            plan.prune.len()
        )));
    }

    if plan.deferred.is_empty() && plan.unfinishable.is_empty() && plan.prune.is_empty() {
        Ok(None) // every head already seen — quiet tick
    } else {
        Ok(Some(format!(
            "no trigger ({} deferred, {} unfinishable, {} pruned)",
            plan.deferred.len(),
            plan.unfinishable.len(),
            plan.prune.len()
        )))
    }
}

/// One watch tick over every watched profile. Per-repo fault isolation: a
/// failing repo is reported loudly (stderr + telemetry) and the loop moves
/// on; the tick itself errors at the end if anything failed, so the
/// runtime's auto-telemetry shows the failure too.
fn watch_all(
    hex_dir: &Path,
    profiles: &[hex::release::ReleaseProfile],
    spawn: SpawnFn,
) -> Result<()> {
    let watched: Vec<_> = profiles.iter().filter(|p| p.watch).collect();
    if watched.is_empty() {
        return Ok(());
    }
    let mut failures: Vec<String> = Vec::new();
    for p in &watched {
        match watch_repo(hex_dir, p, spawn) {
            Ok(None) => {}
            Ok(Some(summary)) => eprintln!("oss-releaser watch: {}: {summary}", p.name),
            Err(err) => {
                eprintln!("oss-releaser watch: profile `{}` FAILED: {err:#}", p.name);
                record_watch("release::watch", "error", format!("profile={} {err:#}", p.name));
                failures.push(p.name.clone());
            }
        }
    }
    if !failures.is_empty() {
        anyhow::bail!(
            "branch watch failed for {}/{} watched repos: {}",
            failures.len(),
            watched.len(),
            failures.join(", ")
        );
    }
    Ok(())
}

/// `$HEX_DIR`, else `$HOME/hex` — the same resolution `module_state` uses.
fn resolve_hex_dir() -> Result<PathBuf> {
    if let Ok(d) = std::env::var("HEX_DIR") {
        return Ok(PathBuf::from(d));
    }
    let home = std::env::var("HOME").context(
        "oss-releaser watch: neither HEX_DIR nor HOME set — cannot locate the \
         runtime-state db",
    )?;
    Ok(PathBuf::from(home).join("hex"))
}

/// Production ceremony trigger: detached `hex release cut --finish <branch>`.
fn spawn_finish(repo: &Path, branch: &str) -> Result<()> {
    spawn_detached_cut(
        repo,
        &["--finish".to_string(), branch.to_string()],
        &format!("finish={branch}"),
    )
}

fn run_watch(_e: Event, _ctx: Ctx) -> Result<()> {
    let hex_dir = match resolve_hex_dir() {
        Ok(d) => d,
        Err(err) => {
            eprintln!("{err:#}");
            return Err(err);
        }
    };
    let profiles = match hex::release::known_profiles() {
        Ok(p) => p,
        Err(err) => {
            eprintln!("oss-releaser watch: cannot load release profiles: {err:#}");
            record_watch("release::watch", "error", format!("loading profiles: {err:#}"));
            return Err(err);
        }
    };
    watch_all(&hex_dir, &profiles, &spawn_finish)
}

/// Build the `oss-releaser` worker.
pub fn worker() -> Worker {
    Worker::new("oss-releaser")
        .on_event(EVENT_RELEASE_REQUESTED, run_release)
        .on_cron(CRON_WATCH, run_watch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

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

    #[test]
    fn worker_registers_event_and_watch_cron() {
        let w = worker();
        assert_eq!(w.name, "oss-releaser");
        let specs: Vec<_> = w.handlers.iter().map(|(s, _)| s.clone()).collect();
        assert!(specs.contains(&hex::worker::TriggerSpec::State {
            scope: "events".to_string(),
            key: EVENT_RELEASE_REQUESTED.to_string(),
        }));
        assert!(specs.contains(&hex::worker::TriggerSpec::Cron {
            expression: CRON_WATCH.to_string(),
        }));
    }

    // -- pure planning ------------------------------------------------------

    fn live(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(b, s)| (b.to_string(), s.to_string())).collect()
    }

    fn seen(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(b, s)| (b.to_string(), s.to_string())).collect()
    }

    #[test]
    fn plan_watch_new_branch_triggers() {
        let plan = plan_watch(&seen(&[]), &live(&[("release/0.2.0", "aaa")]));
        assert_eq!(plan.trigger, Some(("release/0.2.0".to_string(), "aaa".to_string())));
        assert!(plan.deferred.is_empty());
        assert!(plan.unfinishable.is_empty());
        assert!(plan.prune.is_empty());
    }

    #[test]
    fn plan_watch_seen_sha_does_not_retrigger() {
        // The persisted-at-spawn sha (idempotency #1): an in-flight or
        // completed-but-unpruned ceremony's branch never fires twice.
        let plan = plan_watch(
            &seen(&[("release/0.2.0", "aaa")]),
            &live(&[("release/0.2.0", "aaa")]),
        );
        assert_eq!(plan, WatchPlan::default());
    }

    #[test]
    fn plan_watch_new_commit_on_known_branch_triggers() {
        let plan = plan_watch(
            &seen(&[("release/0.2.0", "aaa")]),
            &live(&[("release/0.2.0", "bbb")]),
        );
        assert_eq!(plan.trigger, Some(("release/0.2.0".to_string(), "bbb".to_string())));
    }

    #[test]
    fn plan_watch_one_trigger_per_repo_per_tick_rest_deferred() {
        let plan = plan_watch(
            &seen(&[]),
            &live(&[("hotfix/0.1.1", "aaa"), ("release/0.2.0", "bbb")]),
        );
        // Live heads arrive sorted (ls_remote_watch_heads sorts); the first
        // new finishable head wins, the rest wait for later ticks.
        assert_eq!(plan.trigger, Some(("hotfix/0.1.1".to_string(), "aaa".to_string())));
        assert_eq!(plan.deferred, vec!["release/0.2.0".to_string()]);
    }

    #[test]
    fn plan_watch_unfinishable_branch_is_flagged_never_triggered() {
        let plan = plan_watch(&seen(&[]), &live(&[("release/not-semver", "aaa")]));
        assert_eq!(plan.trigger, None);
        assert_eq!(
            plan.unfinishable,
            vec![("release/not-semver".to_string(), "aaa".to_string())]
        );
        // Once persisted (the caller records the sha), it goes quiet.
        let plan = plan_watch(
            &seen(&[("release/not-semver", "aaa")]),
            &live(&[("release/not-semver", "aaa")]),
        );
        assert_eq!(plan, WatchPlan::default());
    }

    #[test]
    fn plan_watch_vanished_branch_is_pruned_and_recreation_counts_as_new() {
        // Ceremony completed: the branch is gone from origin → prune.
        let plan = plan_watch(&seen(&[("release/0.2.0", "aaa")]), &live(&[]));
        assert_eq!(plan.prune, vec!["release/0.2.0".to_string()]);
        assert_eq!(plan.trigger, None);
        // After the prune, a same-named branch at a new sha is brand new.
        let plan = plan_watch(&seen(&[]), &live(&[("release/0.2.0", "ccc")]));
        assert_eq!(plan.trigger, Some(("release/0.2.0".to_string(), "ccc".to_string())));
    }

    #[test]
    fn plan_watch_full_cycle_is_idempotent() {
        // tick 1: new branch → trigger; caller persists the sha.
        let l = live(&[("release/0.3.0", "aaa")]);
        let mut s = seen(&[]);
        let plan = plan_watch(&s, &l);
        let (branch, sha) = plan.trigger.expect("tick 1 triggers");
        s.insert(branch, sha);
        // tick 2 (ceremony in flight, head unmoved): nothing fires.
        assert_eq!(plan_watch(&s, &l), WatchPlan::default());
        // tick 3 (ceremony completed, branch deleted on origin): prune only.
        let plan = plan_watch(&s, &live(&[]));
        assert_eq!(plan.trigger, None);
        assert_eq!(plan.prune, vec!["release/0.3.0".to_string()]);
    }

    // -- seen-state store ---------------------------------------------------

    #[test]
    fn seen_store_roundtrip_scoped_by_profile() {
        let td = tempfile::tempdir().unwrap();
        let hex_dir = td.path();
        seen_record(hex_dir, "p1", "release/0.2.0", "aaa").unwrap();
        seen_record(hex_dir, "p2", "release/0.2.0", "bbb").unwrap();
        assert_eq!(
            seen_load(hex_dir, "p1").unwrap(),
            seen(&[("release/0.2.0", "aaa")])
        );
        // Upsert replaces.
        seen_record(hex_dir, "p1", "release/0.2.0", "ccc").unwrap();
        assert_eq!(
            seen_load(hex_dir, "p1").unwrap(),
            seen(&[("release/0.2.0", "ccc")])
        );
        // Forget removes only the named row in the named profile.
        seen_forget(hex_dir, "p1", "release/0.2.0").unwrap();
        assert!(seen_load(hex_dir, "p1").unwrap().is_empty());
        assert_eq!(
            seen_load(hex_dir, "p2").unwrap(),
            seen(&[("release/0.2.0", "bbb")])
        );
    }

    #[test]
    fn seen_store_corrupt_db_is_loud_err() {
        let td = tempfile::tempdir().unwrap();
        let p = hex::module_state::db_path(td.path());
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "this is not a sqlite database, not even close").unwrap();
        assert!(seen_load(td.path(), "p").is_err());
        assert!(seen_record(td.path(), "p", "release/0.2.0", "aaa").is_err());
    }

    // -- integration: local fixture repos (no network, no real spawning) ----

    fn git(repo: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?} in {} failed: {}{}",
            repo.display(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn git_out(repo: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("git runs");
        assert!(out.status.success(), "git {args:?} failed");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn commit_file(repo: &Path, name: &str, msg: &str) {
        std::fs::write(repo.join(name), format!("{msg}\n")).unwrap();
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-q", "-m", msg]);
    }

    /// A local bare origin plus a clone with one commit on `main`, pushed.
    fn watch_fixture(td: &Path) -> PathBuf {
        let origin = td.join("origin.git");
        std::fs::create_dir_all(&origin).unwrap();
        git(&origin, &["init", "-q", "--bare"]);
        let clone = td.join("clone");
        std::fs::create_dir_all(&clone).unwrap();
        git(&clone, &["init", "-q"]);
        git(&clone, &["config", "user.name", "watch-test"]);
        git(&clone, &["config", "user.email", "watch-test@example.invalid"]);
        git(&clone, &["checkout", "-q", "-b", "main"]);
        commit_file(&clone, "seed.txt", "chore: seed");
        git(&clone, &["remote", "add", "origin", origin.to_str().unwrap()]);
        git(&clone, &["push", "-q", "origin", "main"]);
        clone
    }

    /// Cut `branch` from main with one commit and push it; returns its sha.
    fn push_branch(clone: &Path, branch: &str, file: &str) -> String {
        git(clone, &["checkout", "-q", "-B", branch, "main"]);
        commit_file(clone, file, &format!("feat: {branch}"));
        git(clone, &["push", "-q", "origin", branch]);
        let sha = git_out(clone, &["rev-parse", "HEAD"]);
        git(clone, &["checkout", "-q", "main"]);
        sha
    }

    fn watch_profile(name: &str, repo_dir: &Path) -> hex::release::ReleaseProfile {
        hex::release::ReleaseProfile {
            name: name.to_string(),
            match_remote: None,
            match_dir: Some(name.to_string()),
            gates: vec![],
            version_files: vec![],
            build_command: None,
            tag_prefix: "v".to_string(),
            gh_release: false,
            main_branch: "main".to_string(),
            develop_branch: "develop".to_string(),
            repo_dir: Some(repo_dir.to_path_buf()),
            watch: true,
        }
    }

    /// A spawner that records `(branch)` instead of spawning anything.
    fn recorder() -> (Arc<Mutex<Vec<String>>>, impl Fn(&Path, &str) -> Result<()>) {
        let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let log2 = Arc::clone(&log);
        (log, move |_repo: &Path, branch: &str| {
            log2.lock().unwrap().push(branch.to_string());
            Ok(())
        })
    }

    #[test]
    fn watch_repo_triggers_finish_once_per_observed_sha() {
        let (tmp, _guard) = hex::telemetry::test_support::isolate();
        let hex_dir = tmp.path().to_path_buf();
        let td = tempfile::tempdir().unwrap();
        let clone = watch_fixture(td.path());
        let sha = push_branch(&clone, "release/0.1.0", "feature.txt");
        let profile = watch_profile("fix", &clone);
        let (spawned, spawn) = recorder();

        // tick 1: brand-new branch → exactly one trigger, sha persisted.
        watch_repo(&hex_dir, &profile, &spawn).unwrap();
        assert_eq!(*spawned.lock().unwrap(), vec!["release/0.1.0".to_string()]);
        assert_eq!(seen_load(&hex_dir, "fix").unwrap(), seen(&[("release/0.1.0", sha.as_str())]));

        // tick 2: same head → no re-trigger (idempotency across ticks).
        assert_eq!(watch_repo(&hex_dir, &profile, &spawn).unwrap(), None);
        assert_eq!(spawned.lock().unwrap().len(), 1);

        // New commit pushed to the branch → that IS a new request.
        git(&clone, &["checkout", "-q", "release/0.1.0"]);
        commit_file(&clone, "more.txt", "feat: more");
        git(&clone, &["push", "-q", "origin", "release/0.1.0"]);
        git(&clone, &["checkout", "-q", "main"]);
        watch_repo(&hex_dir, &profile, &spawn).unwrap();
        assert_eq!(spawned.lock().unwrap().len(), 2);
    }

    #[test]
    fn watch_repo_prunes_vanished_branch_after_ceremony_completion() {
        let (tmp, _guard) = hex::telemetry::test_support::isolate();
        let hex_dir = tmp.path().to_path_buf();
        let td = tempfile::tempdir().unwrap();
        let clone = watch_fixture(td.path());
        push_branch(&clone, "release/0.1.0", "feature.txt");
        let profile = watch_profile("fix", &clone);
        let (spawned, spawn) = recorder();

        watch_repo(&hex_dir, &profile, &spawn).unwrap();
        assert_eq!(spawned.lock().unwrap().len(), 1);

        // Ceremony completion deletes the branch on origin (cleanup step).
        git(&clone, &["push", "-q", "origin", ":refs/heads/release/0.1.0"]);
        watch_repo(&hex_dir, &profile, &spawn).unwrap();
        assert!(seen_load(&hex_dir, "fix").unwrap().is_empty(), "row pruned");
        assert_eq!(spawned.lock().unwrap().len(), 1, "prune never spawns");

        // A future same-named branch (new work) counts as brand new.
        push_branch(&clone, "release/0.1.0", "again.txt");
        watch_repo(&hex_dir, &profile, &spawn).unwrap();
        assert_eq!(spawned.lock().unwrap().len(), 2);
    }

    #[test]
    fn watch_repo_defers_everything_while_ceremony_lock_held() {
        let (tmp, _guard) = hex::telemetry::test_support::isolate();
        let hex_dir = tmp.path().to_path_buf();
        let td = tempfile::tempdir().unwrap();
        let clone = watch_fixture(td.path());
        push_branch(&clone, "release/0.1.0", "feature.txt");
        let profile = watch_profile("fix", &clone);
        let (spawned, spawn) = recorder();

        let lock = hex::release::lock_file_path(&clone).unwrap();
        std::fs::write(&lock, "pid=watch-test").unwrap();
        let summary = watch_repo(&hex_dir, &profile, &spawn).unwrap();
        assert!(summary.unwrap().contains("deferred"), "lock defers the tick");
        assert!(spawned.lock().unwrap().is_empty(), "no spawn under lock");
        assert!(seen_load(&hex_dir, "fix").unwrap().is_empty(), "no state writes under lock");

        // Lock released → the branch is picked up.
        std::fs::remove_file(&lock).unwrap();
        watch_repo(&hex_dir, &profile, &spawn).unwrap();
        assert_eq!(*spawned.lock().unwrap(), vec!["release/0.1.0".to_string()]);
    }

    #[test]
    fn watch_repo_spawn_failure_rolls_back_state_for_retry() {
        let (tmp, _guard) = hex::telemetry::test_support::isolate();
        let hex_dir = tmp.path().to_path_buf();
        let td = tempfile::tempdir().unwrap();
        let clone = watch_fixture(td.path());
        push_branch(&clone, "release/0.1.0", "feature.txt");
        let profile = watch_profile("fix", &clone);

        let failing = |_repo: &Path, _branch: &str| -> Result<()> {
            anyhow::bail!("spawn exploded")
        };
        let err = format!("{:#}", watch_repo(&hex_dir, &profile, &failing).unwrap_err());
        assert!(err.contains("spawn exploded"), "got: {err}");
        assert!(err.contains("rolled back"), "got: {err}");
        assert!(seen_load(&hex_dir, "fix").unwrap().is_empty(), "rollback clears the row");

        // Next tick retries with a working spawner.
        let (spawned, spawn) = recorder();
        watch_repo(&hex_dir, &profile, &spawn).unwrap();
        assert_eq!(*spawned.lock().unwrap(), vec!["release/0.1.0".to_string()]);
    }

    #[test]
    fn watch_repo_missing_repo_dir_is_loud_err() {
        let (tmp, _guard) = hex::telemetry::test_support::isolate();
        let hex_dir = tmp.path().to_path_buf();
        let td = tempfile::tempdir().unwrap();
        let profile = watch_profile("gone", &td.path().join("does-not-exist"));
        let (spawned, spawn) = recorder();
        let err = format!("{:#}", watch_repo(&hex_dir, &profile, &spawn).unwrap_err());
        assert!(err.contains("does not exist"), "got: {err}");
        assert!(spawned.lock().unwrap().is_empty());
    }

    #[test]
    fn watch_repo_ls_remote_failure_is_loud_err() {
        let (tmp, _guard) = hex::telemetry::test_support::isolate();
        let hex_dir = tmp.path().to_path_buf();
        let td = tempfile::tempdir().unwrap();
        let clone = watch_fixture(td.path());
        // Point origin at a nonexistent path — ls-remote must hard-fail,
        // never read as "no branches".
        git(&clone, &["remote", "set-url", "origin", td.path().join("nope.git").to_str().unwrap()]);
        let profile = watch_profile("fix", &clone);
        let (spawned, spawn) = recorder();
        let err = format!("{:#}", watch_repo(&hex_dir, &profile, &spawn).unwrap_err());
        assert!(err.contains("polling origin heads"), "got: {err}");
        assert!(spawned.lock().unwrap().is_empty());
    }

    #[test]
    fn watch_repo_unreadable_state_refuses_to_trigger() {
        let (tmp, _guard) = hex::telemetry::test_support::isolate();
        let hex_dir = tmp.path().to_path_buf();
        let td = tempfile::tempdir().unwrap();
        let clone = watch_fixture(td.path());
        push_branch(&clone, "release/0.1.0", "feature.txt");
        let profile = watch_profile("fix", &clone);
        let p = hex::module_state::db_path(&hex_dir);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "definitely not sqlite").unwrap();
        let (spawned, spawn) = recorder();
        let err = format!("{:#}", watch_repo(&hex_dir, &profile, &spawn).unwrap_err());
        assert!(err.contains("refusing to trigger blind"), "got: {err}");
        assert!(spawned.lock().unwrap().is_empty(), "never trigger blind");
    }

    #[test]
    fn watch_all_isolates_per_repo_failures() {
        let (tmp, _guard) = hex::telemetry::test_support::isolate();
        let hex_dir = tmp.path().to_path_buf();
        let td = tempfile::tempdir().unwrap();
        let clone = watch_fixture(td.path());
        push_branch(&clone, "release/0.1.0", "feature.txt");

        let bad = watch_profile("bad", &td.path().join("does-not-exist"));
        let good = watch_profile("good", &clone);
        let mut unwatched = watch_profile("unwatched", &td.path().join("also-missing"));
        unwatched.watch = false; // opted out — must be skipped entirely

        let (spawned, spawn) = recorder();
        let err = format!(
            "{:#}",
            watch_all(&hex_dir, &[bad, good, unwatched], &spawn).unwrap_err()
        );
        // The bad repo failed loudly…
        assert!(err.contains("1/2"), "got: {err}");
        assert!(err.contains("bad"), "got: {err}");
        // …and the good repo still got its trigger (fault isolation).
        assert_eq!(*spawned.lock().unwrap(), vec!["release/0.1.0".to_string()]);
    }
}
