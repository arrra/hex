use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};
use regex::Regex;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Default deadline for every external command this check spawns (F14):
/// `cargo` can wait on a shared cache lock, `rustup` can perform toolchain
/// setup despite `--offline`, and git checkout filters can block — none of
/// that may wedge an ordinary health check. `run_check_with_timeout` is the
/// configurable entry point; `run_check` (the one the doctor registry
/// actually calls) applies this default.
const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(120);

/// B2: the harness must be buildable from a fresh `git worktree` of HEAD —
/// i.e. everything `cargo metadata` and every `include_str!`/`include_bytes!`
/// need must actually be tracked by git, not merely present on disk (gitignored,
/// or hidden by a local `.git/info/exclude`). Never compiles anything; finishes
/// in seconds.
pub struct HarnessBuildableFromGit;

impl DoctorCheck for HarnessBuildableFromGit {
    fn name(&self) -> &str {
        "harness-buildable-from-git"
    }
    fn category(&self) -> Category {
        Category::Health
    }
    fn run(&self, ctx: &Context) -> CheckResult {
        run_check(&ctx.hex_dir)
    }
}

/// Removes the temporary `git worktree` this check creates, no matter how the
/// check function returns (early `return`, panic-unwind, whatever) — B2's
/// RED tests assert the worktree is gone after both a PASS and a FAIL run.
///
/// Holds the `TempDir` itself (rather than just a path built by hand) so the
/// directory name comes from `mkdtemp`-style atomic creation: doctor checks
/// can run concurrently with other doctor checks and with each other's tests,
/// and a path built from `process::id()` + a wall-clock timestamp can collide
/// under parallel `cargo test` — two `git worktree add` calls racing onto the
/// same not-yet-existing path corrupt each other's worktree metadata (seen as
/// spurious "nonexistent object" / "not a git repository" failures).
struct WorktreeGuard {
    repo_dir: PathBuf,
    worktree_path: PathBuf,
    timeout: Duration,
    _tempdir: tempfile::TempDir,
}

impl WorktreeGuard {
    /// Removes exactly this invocation's own worktree registration, verifies
    /// it is actually gone, and reports failure instead of swallowing it.
    ///
    /// F15/F16: earlier code ran a repository-wide `git worktree prune` in
    /// `Drop`, which can delete OTHER eligible worktree registrations too —
    /// e.g. an unlocked worktree on a currently-unavailable mount — and lose
    /// their administrative state. `prune` is gone entirely; cleanup here
    /// only ever names `self.worktree_path`.
    fn cleanup(&self) -> Result<(), String> {
        let mut remove = Command::new("git");
        remove
            .args(["worktree", "remove", "--force"])
            .arg(&self.worktree_path)
            .current_dir(&self.repo_dir);
        match run_with_timeout(&mut remove, self.timeout) {
            Ok(o) if !o.status.success() => {
                return Err(format!(
                    "git worktree remove --force {} failed: {}",
                    self.worktree_path.display(),
                    String::from_utf8_lossy(&o.stderr).trim()
                ));
            }
            Err(e) => return Err(format!("git worktree remove --force did not complete: {e}")),
            Ok(_) => {}
        }
        // Bounded fallback (F14): a plain filesystem removal, never a
        // subprocess that can block on a shared lock, guarantees nothing is
        // left on disk even if the administrative removal above raced with
        // something else.
        let _ = std::fs::remove_dir_all(&self.worktree_path);
        if self.worktree_path.exists() {
            return Err(format!(
                "residual worktree directory after cleanup: {}",
                self.worktree_path.display()
            ));
        }
        Ok(())
    }
}

impl Drop for WorktreeGuard {
    fn drop(&mut self) {
        // Best-effort fallback for early returns/panics that never reach
        // the explicit `cleanup()` call in `run_check_with_timeout` —
        // scoped to exactly this invocation's own worktree, same as
        // `cleanup()`, and never a repo-wide `git worktree prune` (F15/F16).
        let mut remove = Command::new("git");
        remove
            .args(["worktree", "remove", "--force"])
            .arg(&self.worktree_path)
            .current_dir(&self.repo_dir);
        let _ = run_with_timeout(&mut remove, self.timeout);
        let _ = std::fs::remove_dir_all(&self.worktree_path);
    }
}

/// Runs `cmd` under a hard deadline using only `std`: spawn, then poll
/// `try_wait` in a loop instead of blocking on `output()`/`wait()` (F14).
/// Killing the child on timeout means a wedged external command — `cargo`
/// stuck on a shared cache lock, `rustup` performing toolchain setup despite
/// `--offline`, a blocking checkout filter — can never hang an ordinary
/// health check. stdout/stderr are drained on background threads so a
/// chatty child can't deadlock on a full pipe buffer while we poll.
pub(crate) fn run_with_timeout(cmd: &mut Command, timeout: Duration) -> Result<Output, String> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn command: {e}"))?;

    let mut stdout_pipe = child.stdout.take().expect("stdout was piped");
    let mut stderr_pipe = child.stderr.take().expect("stderr was piped");
    let (stdout_tx, stdout_rx) = std::sync::mpsc::channel();
    let (stderr_tx, stderr_rx) = std::sync::mpsc::channel();
    // G2: these reader threads are intentionally never joined below. The
    // direct child exiting (observed via `try_wait`) does not mean its
    // stdout/stderr pipes are closed — a descendant it spawned can inherit
    // the fd and keep the write end open indefinitely. An unconditional
    // `.join()` here would then block for as long as that descendant lives,
    // past this function's own `timeout`. Sending the buffer over a channel
    // and bounding the receive below lets us stop waiting on a straggler
    // thread instead of hanging on it.
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout_pipe.read_to_end(&mut buf);
        let _ = stdout_tx.send(buf);
    });
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut buf);
        let _ = stderr_tx.send(buf);
    });

    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err(format!("command timed out after {timeout:?}"));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => break Err(format!("failed to wait on command: {e}")),
        }
    }?;

    // Bound how long we wait for each reader thread to observe EOF, rather
    // than joining unconditionally (G2). A small floor keeps this from
    // collapsing to a zero-length window when the main loop above already
    // consumed the whole `timeout` (e.g. the child itself was killed for
    // running over).
    //
    // G2 (follow-up): a single `drain_budget` value handed unchanged to
    // BOTH `recv_timeout` calls let a pipe that closes quickly "refund"
    // nothing to the other — stdout succeeding after using most of the
    // budget still let stderr wait for a second FULL budget on top, so a
    // staggered stdout/stderr closure could push total drain time to
    // roughly double the intended bound. Use one absolute deadline for the
    // whole drain phase instead, and recompute the remaining time before
    // each call so the two receives share a single window.
    let drain_deadline = Instant::now()
        + timeout
            .saturating_sub(start.elapsed())
            .max(Duration::from_millis(200));
    let stdout_budget = drain_deadline.saturating_duration_since(Instant::now());
    let stdout = stdout_rx.recv_timeout(stdout_budget).map_err(|_| {
        format!(
            "command exited but its stdout was not closed within {stdout_budget:?} \
             (a descendant process may still be holding the pipe open)"
        )
    })?;
    let stderr_budget = drain_deadline.saturating_duration_since(Instant::now());
    let stderr = stderr_rx.recv_timeout(stderr_budget).map_err(|_| {
        format!(
            "command exited but its stderr was not closed within {stderr_budget:?} \
             (a descendant process may still be holding the pipe open)"
        )
    })?;
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

fn run_check(hex_dir: &Path) -> CheckResult {
    run_check_with_timeout(hex_dir, DEFAULT_COMMAND_TIMEOUT)
}

/// Same check as `run_check`, but every external command it spawns is bound
/// to `timeout` instead of the default (F14 — "configurable via the check's
/// params"; `run_check` is the thin wrapper the registry actually calls,
/// applying `DEFAULT_COMMAND_TIMEOUT`).
pub(crate) fn run_check_with_timeout(hex_dir: &Path, timeout: Duration) -> CheckResult {
    // `tempfile::tempdir()` uses `mkdtemp`-equivalent atomic creation, so the
    // path is guaranteed unique even when many doctor-check invocations (or
    // their tests) race concurrently — see the comment on `WorktreeGuard`.
    let tempdir = match tempfile::Builder::new()
        .prefix("hex-doctor-harness-buildable-")
        .tempdir()
    {
        Ok(t) => t,
        Err(e) => return CheckResult::fail(format!("failed to create temp dir: {e}")),
    };
    let worktree_path = tempdir.path().to_path_buf();

    let worktree_guard = WorktreeGuard {
        repo_dir: hex_dir.to_path_buf(),
        worktree_path: worktree_path.clone(),
        timeout,
        _tempdir: tempdir,
    };

    // F13: `git worktree add` invokes `post-checkout` and honors the
    // repository's `core.hooksPath` by default — registering this check
    // would otherwise make an ordinary doctor run execute arbitrary
    // checkout automation (state mutation, network access). Override
    // `core.hooksPath` to an empty directory for this command only; the
    // repository's own configuration is never touched.
    let empty_hooks_dir = match tempfile::Builder::new()
        .prefix("hex-doctor-harness-buildable-empty-hooks-")
        .tempdir()
    {
        Ok(t) => t,
        Err(e) => return CheckResult::fail(format!("failed to create empty hooks dir: {e}")),
    };
    let mut add_cmd = Command::new("git");
    add_cmd
        .arg("-c")
        .arg(format!(
            "core.hooksPath={}",
            empty_hooks_dir.path().display()
        ))
        .args(["worktree", "add", "--detach"])
        .arg(&worktree_path)
        .arg("HEAD")
        .current_dir(hex_dir);
    let add_output = match run_with_timeout(&mut add_cmd, timeout) {
        Ok(o) => o,
        Err(e) => return CheckResult::fail(format!("git worktree add did not complete: {e}")),
    };
    if !add_output.status.success() {
        return CheckResult::fail(format!(
            "git worktree add failed — cannot verify the harness builds from git: {}",
            String::from_utf8_lossy(&add_output.stderr).trim()
        ));
    }

    // G1: from here on a worktree registration actually exists on disk, so
    // every return path below — including the ones inside `diagnose` —
    // MUST run through `finalize_with_cleanup` so `cleanup()` always runs
    // and its failure is never silently swallowed. Earlier code called
    // `cleanup()` only from the single final branch at the bottom of this
    // function; every earlier `return` relied on `Drop`, which discards
    // cleanup errors and, on the success path, merely appended a "(cleanup
    // warning: ...)" suffix while still reporting `Status::Pass` — a doctor
    // check reporting PASS after leaking a worktree it could not remove is
    // exactly the "quiet failure" SO S6 forbids.
    let result = diagnose(&worktree_path, empty_hooks_dir.path(), timeout);
    finalize_with_cleanup(result, &worktree_guard)
}

/// Runs every check step that requires the diagnostic worktree to already
/// exist: materializing a full checkout (F20), verifying `.hex/harness` is
/// present, running `cargo metadata`, and scanning for missing
/// `include_str!`/`include_bytes!` targets. Deliberately does not touch
/// `WorktreeGuard` — the caller always runs cleanup afterward via
/// `finalize_with_cleanup`, regardless of which branch here returns (G1).
fn diagnose(worktree_path: &Path, empty_hooks_dir: &Path, timeout: Duration) -> CheckResult {
    // F20: `git worktree add` copies the CALLER's sparse-checkout patterns
    // and `core.sparseCheckout` setting into the new worktree — a tracked
    // file excluded only by the caller's own sparse patterns would then
    // read as "missing from git" below. Materialize a full checkout scoped
    // to this worktree's own (worktree-private) index, without touching the
    // caller's config or sparse-checkout patterns at all.
    if let Err(e) = materialize_full_checkout(worktree_path, empty_hooks_dir, timeout) {
        return CheckResult::fail(format!(
            "failed to materialize a full checkout in the diagnostic worktree: {e}"
        ));
    }

    let harness_dir = worktree_path.join(".hex/harness");
    if !harness_dir.is_dir() {
        return CheckResult::fail(format!(
            ".hex/harness -> missing from a fresh git checkout (not tracked, or gitignored) — \
             fix: git add it or install it from .hex/.upgrade-cache (checked {})",
            harness_dir.display()
        ));
    }

    // Step 2: `cargo metadata --locked` catches both a missing path
    // dependency (e.g. the `.hex/code-intel` path dep hidden by a local
    // `.git/info/exclude`) and a Cargo.lock that isn't tracked at all —
    // `--locked` refuses to generate/update one, so a fresh checkout with no
    // lockfile fails immediately (no network needed to detect that), rather
    // than being silently skipped. This never generates a lockfile and
    // never compiles anything.
    //
    // `--filter-platform <host>` is required: without it, `cargo metadata
    // --offline` resolves ALL platforms in the lockfile, including
    // cfg-gated deps (e.g. `android_system_properties`) that are never in
    // the local registry cache on this host — that fails --offline even
    // when everything this host actually needs is present, producing a
    // permanent false FAIL. The host triple comes from `rustc -vV`'s
    // `host:` line; if `rustc` itself fails, fall back to no filter rather
    // than skip the check.
    //
    // F12: `host_triple_impl` runs with `current_dir` set to `harness_dir`
    // — the same directory context `cargo metadata` below uses — rather
    // than doctor's own ambient cwd, so a rustup directory override or
    // `rust-toolchain.toml` can't select a different toolchain than the one
    // Cargo actually resolves.
    let mut metadata_args = vec!["metadata", "--locked", "--offline", "--format-version", "1"];
    let host_triple = host_triple_impl(&harness_dir, timeout);
    if let Some(host) = host_triple.as_deref() {
        metadata_args.push("--filter-platform");
        metadata_args.push(host);
    }
    let mut metadata_cmd = Command::new("cargo");
    metadata_cmd.args(&metadata_args).current_dir(&harness_dir);
    match run_with_timeout(&mut metadata_cmd, timeout) {
        Ok(o) if !o.status.success() => {
            return CheckResult::fail(format!(
                ".hex/harness -> `cargo metadata` failed in a fresh git checkout \
                 (missing/out-of-date Cargo.lock or a missing path dependency) — \
                 fix: git add it or install it from .hex/.upgrade-cache: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            ));
        }
        Err(e) => {
            return CheckResult::fail(format!("cargo metadata did not complete: {e}"));
        }
        Ok(_) => {}
    }

    // Step 3: every `include_str!`/`include_bytes!` target the harness
    // references at compile time must actually be present in the worktree.
    let mut checked = 0usize;
    let mut missing: Vec<(String, String)> = Vec::new();
    for dir_name in ["src", "tests"] {
        let dir = harness_dir.join(dir_name);
        if dir.is_dir() {
            scan_dir(&dir, worktree_path, &mut checked, &mut missing);
        }
    }
    let build_rs = harness_dir.join("build.rs");
    if build_rs.is_file() {
        scan_file(&build_rs, worktree_path, &mut checked, &mut missing);
    }

    if missing.is_empty() {
        CheckResult::pass(format!(
            "harness builds from git ({checked} include target(s) present)"
        ))
    } else {
        let details = missing
            .iter()
            .map(|(referencing, target)| format!("{referencing} -> {target}"))
            .collect::<Vec<_>>()
            .join("\n");
        CheckResult::fail(format!(
            "{} include target(s) referenced by the harness are missing from git — \
             fix: git add it or install it from .hex/.upgrade-cache",
            missing.len()
        ))
        .with_details(details)
    }
}

/// Always runs `guard.cleanup()` regardless of `result`'s status (G1), and
/// never lets a cleanup failure surface as `Status::Pass` — a doctor check
/// reporting PASS after leaking a worktree it could not remove is exactly
/// the "quiet failure" SO S6 forbids. A successful cleanup passes `result`
/// through unchanged; F15/F16 already scope `cleanup()` to exactly this
/// invocation's own worktree (never a repo-wide `git worktree prune`) and
/// verify it is actually gone.
fn finalize_with_cleanup(result: CheckResult, guard: &WorktreeGuard) -> CheckResult {
    match guard.cleanup() {
        Ok(()) => result,
        Err(cleanup_err) => CheckResult::fail(format!(
            "{} (cleanup failed: {cleanup_err})",
            result.message
        )),
    }
}

/// Clears any skip-worktree bits inherited into this worktree's own
/// (worktree-private) index and checks the corresponding paths out, so a
/// tracked file excluded only by the CALLER's sparse-checkout patterns is
/// still present here (F20).
///
/// Deliberately does not use `git sparse-checkout disable` (which persists
/// `extensions.worktreeConfig = true` into the shared repository config as
/// a side effect) or a bare `-c core.sparseCheckout=false` override (which
/// does not clear skip-worktree bits already set on index entries) — this
/// touches neither the caller's config nor its sparse-checkout patterns,
/// only this worktree's own private index and working tree.
///
/// The `checkout` step can itself trigger repository checkout automation
/// (the same F13 concern as the initial `worktree add`), so it carries the
/// same empty `core.hooksPath` override.
fn materialize_full_checkout(
    worktree_path: &Path,
    empty_hooks_dir: &Path,
    timeout: Duration,
) -> Result<(), String> {
    let mut ls_files = Command::new("git");
    ls_files.args(["ls-files", "-v"]).current_dir(worktree_path);
    let output = run_with_timeout(&mut ls_files, timeout)?;
    if !output.status.success() {
        return Err(format!(
            "git ls-files -v failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let skipped: Vec<&str> = stdout
        .lines()
        .filter_map(|line| line.strip_prefix("S "))
        .collect();
    if skipped.is_empty() {
        return Ok(());
    }

    let mut update_index = Command::new("git");
    update_index
        .args(["update-index", "--no-skip-worktree"])
        .args(&skipped)
        .current_dir(worktree_path);
    let output = run_with_timeout(&mut update_index, timeout)?;
    if !output.status.success() {
        return Err(format!(
            "git update-index --no-skip-worktree failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    let mut checkout = Command::new("git");
    checkout
        .arg("-c")
        .arg(format!("core.hooksPath={}", empty_hooks_dir.display()))
        .args(["checkout", "HEAD", "--"])
        .args(&skipped)
        .current_dir(worktree_path);
    let output = run_with_timeout(&mut checkout, timeout)?;
    if !output.status.success() {
        return Err(format!(
            "git checkout HEAD -- <previously sparse-excluded paths> failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

/// Parses the `host: <triple>` line out of `rustc -vV` so `cargo metadata`
/// can be scoped with `--filter-platform` to this machine's actual target.
/// Returns `None` (rather than panicking) if `rustc` cannot be spawned, it
/// times out, or the expected line is absent — callers fall back to an
/// unfiltered query.
///
/// F12: runs with `current_dir` set to `dir`, matching how `run_check_with_timeout`
/// calls it (the harness dir inside the diagnostic worktree — the same
/// directory context `cargo metadata` uses), rather than doctor's own
/// ambient cwd.
///
/// Test-only entry point: production code calls `host_triple_impl` directly
/// (threading through the check's configured timeout); this single-arg
/// wrapper exists so the F12 regression can call it without reaching into
/// `host_triple_impl`'s timeout parameter.
#[cfg(test)]
pub(crate) fn host_triple(dir: &Path) -> Option<String> {
    host_triple_impl(dir, DEFAULT_COMMAND_TIMEOUT)
}

fn host_triple_impl(dir: &Path, timeout: Duration) -> Option<String> {
    let mut cmd = Command::new("rustc");
    cmd.arg("-vV").current_dir(dir);
    let output = run_with_timeout(&mut cmd, timeout).ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(|s| s.trim().to_string())
}

fn include_regex() -> Regex {
    // Only matches a plain string-literal argument, so `concat!(env!(...), "...")`
    // forms (which start with `concat!`, not `"`) are skipped automatically.
    Regex::new(r#"include_(?:str|bytes)!\s*\(\s*"([^"]*)"\s*\)"#).expect("static regex is valid")
}

fn scan_dir(
    dir: &Path,
    repo_root: &Path,
    checked: &mut usize,
    missing: &mut Vec<(String, String)>,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan_dir(&path, repo_root, checked, missing);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            scan_file(&path, repo_root, checked, missing);
        }
    }
}

fn scan_file(
    path: &Path,
    repo_root: &Path,
    checked: &mut usize,
    missing: &mut Vec<(String, String)>,
) {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return,
    };
    let re = include_regex();
    for cap in re.captures_iter(&content) {
        let literal = &cap[1];
        *checked += 1;
        let resolved = match path.parent() {
            Some(p) => p.join(literal),
            None => PathBuf::from(literal),
        };
        if !resolved.exists() {
            missing.push((
                display_rel(path, repo_root),
                display_rel(&resolved, repo_root),
            ));
        }
    }
}

fn display_rel(path: &Path, repo_root: &Path) -> String {
    match path.strip_prefix(repo_root) {
        Ok(rel) => rel.display().to_string(),
        Err(_) => path.display().to_string(),
    }
}
