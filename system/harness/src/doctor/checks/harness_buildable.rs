use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};
use std::collections::HashSet;
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
    let metadata_stdout = match run_with_timeout(&mut metadata_cmd, timeout) {
        Ok(o) if !o.status.success() => {
            // F1: an `--offline` failure can mean two very different things —
            // a registry/git dependency the lockfile resolves to is simply
            // not present in the LOCAL CARGO CACHE (nothing to do with git
            // tracking; classify as inconclusive), or a repository input
            // (path dependency, Cargo.lock itself) is genuinely missing from
            // the checkout (a real FAIL). `classify_metadata_failure` matches
            // cargo's own offline-mode diagnostic wording ("...offline mode
            // (--offline)..." or "...but --offline was specified...") rather
            // than a bare `--offline` substring, because cargo's missing-
            // Cargo.lock diagnostic ALSO names the flag in its help text
            // ("...use --offline instead") without that being a cache
            // problem at all.
            return classify_metadata_failure(&String::from_utf8_lossy(&o.stderr));
        }
        Err(e) => {
            return CheckResult::fail(format!("cargo metadata did not complete: {e}"));
        }
        Ok(o) => String::from_utf8_lossy(&o.stdout).into_owned(),
    };

    // Step 3: every `include_str!`/`include_bytes!`/`include!` target the
    // harness references at compile time must actually be present in the
    // worktree (F2/F6: driven by metadata's own local packages and target
    // entry points, not a blind directory guess).
    let local_packages = match parse_local_packages(&metadata_stdout) {
        Ok(p) => p,
        Err(e) => {
            return CheckResult::fail(format!("failed to interpret `cargo metadata` output: {e}"));
        }
    };

    let mut state = ScanState::new(worktree_path.to_path_buf(), timeout);
    for pkg in &local_packages {
        // G1: `cargo metadata` resolves a LOCAL package's root wherever it
        // lives on disk — including an absolute-path dependency that
        // points entirely outside the checkout, which `git worktree add`
        // never sees. Require every local package root to canonicalize
        // inside the checkout root before trusting anything under it,
        // the same containment discipline F4/F10 already applies to leaf
        // include! targets.
        if let Err(reason) = canonicalize_within_checkout(&pkg.root, worktree_path) {
            return CheckResult::fail(format!(
                "local package root {} escapes the checkout — {reason}",
                pkg.root.display()
            ));
        }
        // G1 (review_b, iteration 5): containment alone proves the package
        // root RESOLVES inside the checkout — it does not prove the root
        // itself (as opposed to whatever it points to) is tracked by git.
        // A checkout-filter side effect can fabricate an untracked symlink
        // alias for the package root pointing at a different, genuinely
        // tracked directory, exactly like the same gap already closed for
        // `mod`/include! targets.
        if let Err(reason) = verify_raw_path_tracked_by_git(worktree_path, &pkg.root, timeout) {
            return CheckResult::fail(format!(
                "local package root {} {reason}",
                pkg.root.display()
            ));
        }
        for entry in &pkg.target_entry_points {
            if let Err(reason) = canonicalize_within_checkout(entry, worktree_path) {
                return CheckResult::fail(format!(
                    "target entry point {} escapes the checkout — {reason}",
                    entry.display()
                ));
            }
            // G1 (review_b, iteration 5): same gap as the package root
            // above — a Cargo target entry point (`[lib]`/`[[bin]]` etc.
            // `path = "..."`) was only containment-checked, never proven
            // to be itself a tracked entry at HEAD. The subsequent
            // `scan_file_and_follow` byte-compares whatever the entry
            // point CANONICALIZES to against git — which trivially
            // matches if the entry point is an untracked symlink alias to
            // a different, genuinely tracked file, since that comparison
            // never looks at the literal (unresolved) entry-point path at
            // all.
            if let Err(reason) = verify_raw_path_tracked_by_git(worktree_path, entry, timeout) {
                return CheckResult::fail(format!(
                    "target entry point {} {reason}",
                    entry.display()
                ));
            }
            state.scan_file_and_follow(entry, true);
        }
        // G3: only the metadata-reachable source graph (target entry
        // points, followed via `mod`/`#[path]`/`include!`) counts toward
        // whether the harness actually builds — a `.rs` file under
        // `src`/`tests` that nothing ever `mod`-declares is never compiled
        // by cargo, so a broken include! inside it must never fail this
        // check. `check_dir_readable` still walks these directories, but
        // only to surface genuine I/O errors (F9); it never interprets
        // file content or reports a missing include target.
        for dir_name in ["src", "tests"] {
            let dir = pkg.root.join(dir_name);
            if dir.is_dir() {
                state.check_dir_readable(&dir);
            }
        }
        let build_rs = pkg.root.join("build.rs");
        if build_rs.is_file() {
            state.scan_file_and_follow(&build_rs, true);
        }
    }

    // F9: a directory or file the scan could not read means this check
    // cannot certify anything — surface it loudly (with the path) rather
    // than silently treating an incomplete scan as "nothing missing".
    if !state.errors.is_empty() {
        return CheckResult::fail(format!(
            "{} error(s) while scanning the harness for include targets — the \
             scan could not complete, so this check cannot certify the harness \
             builds from git: {}",
            state.errors.len(),
            state.errors.join("; ")
        ))
        .with_details(state.errors.join("\n"));
    }
    if !state.missing.is_empty() {
        return CheckResult::fail(format!(
            "{} include target(s) referenced by the harness are missing from git — \
             fix: git add it or install it from .hex/.upgrade-cache",
            state.missing.len()
        ))
        .with_details(state.missing.join("\n"));
    }
    if !state.inconclusive.is_empty() {
        return CheckResult::warn(format!(
            "harness builds from git ({} include target(s) present), but {} \
             target(s) could not be conclusively verified — a non-literal \
             include! argument, or a target gated behind an #[cfg(...)] this \
             check cannot evaluate",
            state.checked,
            state.inconclusive.len()
        ))
        .with_details(state.inconclusive.join("\n"));
    }
    CheckResult::pass(format!(
        "harness builds from git ({} include target(s) present)",
        state.checked
    ))
}

/// F1: classifies an `--offline` `cargo metadata` failure as either
/// "dependency cache unavailable" (WARN — nothing to do with git tracking)
/// or a genuine repository-input FAIL. See the call site for why the
/// substring check is reliable.
fn classify_metadata_failure(stderr: &str) -> CheckResult {
    // Match on cargo's own offline-mode wording rather than a bare
    // "--offline" substring: a missing/out-of-date `Cargo.lock` under
    // `--locked` also *mentions* the flag (cargo's help text suggests
    // "remove the --locked flag and use --offline instead"), which would
    // otherwise misclassify that genuine repository-input FAIL as a
    // cache-unavailable WARN.
    let lower = stderr.to_lowercase();
    if lower.contains("offline mode (--offline)") || lower.contains("--offline was specified") {
        return CheckResult::warn(format!(
            ".hex/harness -> a dependency the lockfile resolves to is not present \
             in the local Cargo registry/git cache (dependency cache unavailable — \
             this is not a missing-from-git problem) — fix: warm the cache with \
             network access before running doctor offline, or run `cargo fetch` \
             once online: {}",
            stderr.trim()
        ));
    }
    CheckResult::fail(format!(
        ".hex/harness -> `cargo metadata` failed in a fresh git checkout \
         (missing/out-of-date Cargo.lock or a missing path dependency) — \
         fix: git add it or install it from .hex/.upgrade-cache: {}",
        stderr.trim()
    ))
}

/// Test-only entry point into `classify_metadata_failure`, mirroring the
/// `host_triple`/`host_triple_impl` split above — lets `doctor::runner`'s
/// test module feed exact cargo diagnostic strings straight to the
/// classifier without spinning up a real `cargo metadata --offline` run.
#[cfg(test)]
pub(crate) fn classify_metadata_failure_for_tests(stderr: &str) -> CheckResult {
    classify_metadata_failure(stderr)
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

fn display_rel(path: &Path, repo_root: &Path) -> String {
    match path.strip_prefix(repo_root) {
        Ok(rel) => rel.display().to_string(),
        Err(_) => path.display().to_string(),
    }
}

// ---------------------------------------------------------------------
// F2/F6: metadata-driven local-package/target discovery
// ---------------------------------------------------------------------

/// A local (non-registry, non-git) package `cargo metadata` resolved —
/// either the harness itself or a local path dependency (e.g. the
/// `.hex/code-intel` -> `scipd` path dep) — with every target entry point
/// (`lib`/`bin`/`test`/`custom-build`, including custom `path = "..."`
/// targets) it declares.
struct LocalPackage {
    root: PathBuf,
    target_entry_points: Vec<PathBuf>,
}

/// Parses `cargo metadata --format-version 1` JSON and returns every LOCAL
/// package (a package whose `"source"` field is `null` — i.e. resolved from
/// the filesystem, not a registry or git checkout) with its target entry
/// points. Uses `serde_json::Value` rather than typed structs so unrelated
/// metadata fields (there are many, and they vary by cargo version) never
/// need to round-trip through this check.
fn parse_local_packages(metadata_json: &str) -> Result<Vec<LocalPackage>, String> {
    let doc: serde_json::Value = serde_json::from_str(metadata_json)
        .map_err(|e| format!("failed to parse `cargo metadata` JSON output: {e}"))?;
    let packages = doc
        .get("packages")
        .and_then(|p| p.as_array())
        .ok_or_else(|| "cargo metadata output has no `packages` array".to_string())?;
    let mut out = Vec::new();
    for pkg in packages {
        // Local (path/workspace) packages always have a null "source";
        // registry and git dependencies carry a non-null source string.
        if pkg.get("source").map(|s| !s.is_null()).unwrap_or(false) {
            continue;
        }
        let manifest_path = match pkg.get("manifest_path").and_then(|m| m.as_str()) {
            Some(m) => PathBuf::from(m),
            None => continue,
        };
        let root = manifest_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| manifest_path.clone());
        let mut target_entry_points = Vec::new();
        if let Some(targets) = pkg.get("targets").and_then(|t| t.as_array()) {
            for target in targets {
                if let Some(src) = target.get("src_path").and_then(|s| s.as_str()) {
                    target_entry_points.push(PathBuf::from(src));
                }
            }
        }
        out.push(LocalPackage {
            root,
            target_entry_points,
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------
// F3/F7/F21: hand-written Rust-aware include/mod scanner
// ---------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum IncludeKind {
    Str,
    Bytes,
    /// `include!` — inlines another Rust source file, so a resolved target
    /// is itself recursively scanned for further `mod`/include references.
    Include,
}

impl IncludeKind {
    fn macro_name(self) -> &'static str {
        match self {
            IncludeKind::Str => "include_str",
            IncludeKind::Bytes => "include_bytes",
            IncludeKind::Include => "include",
        }
    }
}

struct ParsedModDecl {
    name: String,
    path_override: Option<String>,
    cfg_gated: bool,
}

struct ParsedMacroCall {
    kind: IncludeKind,
    /// `Some(decoded)` when the argument was a single string literal
    /// (plain or raw, with escapes decoded); `None` for any other
    /// expression (const, `concat!`, ...) — F3 requires these be reported
    /// as inconclusive, never silently treated as present.
    literal: Option<String>,
    cfg_gated: bool,
}

struct ParsedSource {
    mods: Vec<ParsedModDecl>,
    macros: Vec<ParsedMacroCall>,
}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}
fn is_ident_continue(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Small hand-written scanner (no parser dependency, per design guardrails):
/// walks the source once, skipping `//`/`/* */` comments and string/char
/// literals, and records exactly two things: `mod x;` declarations (with
/// any immediately-preceding `#[path = "..."]` override and whether an
/// immediately-preceding `#[cfg(...)]` this scanner can't evaluate gates
/// the item) and `include_str!`/`include_bytes!`/`include!` invocations
/// (matched on the exact macro identifier — never `optional_include_str!`
/// or similar suffix matches — with the same cfg-gating).
///
/// cfg-gating heuristic (F8): any `#[cfg(...)]` attribute is treated as
/// unevaluated (this scanner does not implement cfg evaluation), and gates
/// every `mod`/include item up to the end of that item — the next `;` or
/// matching `}` at the same brace depth the attribute was seen at.
fn scan_source(src: &str) -> ParsedSource {
    let chars: Vec<char> = src.chars().collect();
    let n = chars.len();
    let mut i = 0usize;
    let mut depth: i32 = 0;
    let mut pending_cfg: Option<i32> = None;
    let mut pending_path: Option<(i32, String)> = None;
    let mut mods = Vec::new();
    let mut macros = Vec::new();

    while i < n {
        let c = chars[i];

        if c == '/' && i + 1 < n && chars[i + 1] == '/' {
            i += 2;
            while i < n && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && i + 1 < n && chars[i + 1] == '*' {
            i += 2;
            let mut bd = 1;
            while i < n && bd > 0 {
                if chars[i] == '/' && i + 1 < n && chars[i + 1] == '*' {
                    bd += 1;
                    i += 2;
                } else if chars[i] == '*' && i + 1 < n && chars[i + 1] == '/' {
                    bd -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            continue;
        }
        if let Some((_, end)) = try_parse_string_literal(&chars, i) {
            i = end;
            continue;
        }
        if c == '\'' {
            i = skip_char_literal_or_lifetime(&chars, i);
            continue;
        }
        if c == '#' && i + 1 < n && chars[i + 1] == '[' {
            let (inner, end) = scan_balanced_generic(&chars, i + 1, '[', ']');
            let trimmed = inner.trim();
            let is_cfg_attr = trimmed
                .strip_prefix("cfg")
                .map(|rest| rest.trim_start().starts_with('('))
                .unwrap_or(false);
            if is_cfg_attr {
                pending_cfg = Some(depth);
            } else if let Some(rest) = trimmed.strip_prefix("path") {
                if let Some((_, rhs)) = rest.split_once('=') {
                    let rhs = rhs.trim();
                    let rhs_chars: Vec<char> = rhs.chars().collect();
                    if let Some((val, _)) = try_parse_string_literal(&rhs_chars, 0) {
                        pending_path = Some((depth, val));
                    }
                }
            }
            i = end;
            continue;
        }
        if c == '{' {
            depth += 1;
            i += 1;
            continue;
        }
        if c == '}' {
            depth -= 1;
            i += 1;
            if pending_cfg == Some(depth) {
                pending_cfg = None;
            }
            if pending_path.as_ref().map(|(d, _)| *d) == Some(depth) {
                pending_path = None;
            }
            continue;
        }
        if c == ';' {
            if pending_cfg == Some(depth) {
                pending_cfg = None;
            }
            if pending_path.as_ref().map(|(d, _)| *d) == Some(depth) {
                pending_path = None;
            }
            i += 1;
            continue;
        }
        if is_ident_start(c) {
            let start = i;
            i += 1;
            while i < n && is_ident_continue(chars[i]) {
                i += 1;
            }
            let ident: String = chars[start..i].iter().collect();
            match ident.as_str() {
                "mod" => {
                    let mut j = i;
                    while j < n && chars[j].is_whitespace() {
                        j += 1;
                    }
                    if j < n && is_ident_start(chars[j]) {
                        let name_start = j;
                        j += 1;
                        while j < n && is_ident_continue(chars[j]) {
                            j += 1;
                        }
                        let name: String = chars[name_start..j].iter().collect();
                        let mut k = j;
                        while k < n && chars[k].is_whitespace() {
                            k += 1;
                        }
                        if k < n && chars[k] == ';' {
                            mods.push(ParsedModDecl {
                                name,
                                path_override: pending_path.take().map(|(_, p)| p),
                                cfg_gated: pending_cfg.is_some(),
                            });
                            if pending_cfg == Some(depth) {
                                pending_cfg = None;
                            }
                            i = k + 1;
                            continue;
                        }
                        // `mod name { ... }` (inline body) or malformed —
                        // not a file reference; leave pending_cfg/path
                        // untouched, they clear at the real item boundary.
                    }
                    continue;
                }
                "include_str" | "include_bytes" | "include" => {
                    let kind = match ident.as_str() {
                        "include_str" => IncludeKind::Str,
                        "include_bytes" => IncludeKind::Bytes,
                        _ => IncludeKind::Include,
                    };
                    let mut j = i;
                    while j < n && chars[j].is_whitespace() {
                        j += 1;
                    }
                    if j < n && chars[j] == '!' {
                        j += 1;
                        while j < n && chars[j].is_whitespace() {
                            j += 1;
                        }
                        if j < n && (chars[j] == '(' || chars[j] == '[' || chars[j] == '{') {
                            let open = chars[j];
                            let close = match open {
                                '(' => ')',
                                '[' => ']',
                                _ => '}',
                            };
                            let (arg_text, end) = scan_balanced_generic(&chars, j, open, close);
                            let trimmed = arg_text.trim();
                            let arg_chars: Vec<char> = trimmed.chars().collect();
                            let literal = try_parse_string_literal(&arg_chars, 0)
                                .filter(|(_, end)| *end == arg_chars.len())
                                .map(|(val, _)| val);
                            macros.push(ParsedMacroCall {
                                kind,
                                literal,
                                cfg_gated: pending_cfg.is_some(),
                            });
                            i = end;
                            continue;
                        }
                    }
                    continue;
                }
                _ => {}
            }
            continue;
        }
        i += 1;
    }

    ParsedSource { mods, macros }
}

/// Attempts to parse a Rust string literal (plain `"..."` or raw
/// `r"..."`/`r#"..."#`/...) starting exactly at `pos`. Returns the decoded
/// value and the index just past the closing delimiter, or `None` if `pos`
/// is not the start of a string literal.
fn try_parse_string_literal(chars: &[char], pos: usize) -> Option<(String, usize)> {
    if pos >= chars.len() {
        return None;
    }
    if chars[pos] == '"' {
        return parse_plain_string(chars, pos);
    }
    if chars[pos] == 'r' {
        let mut p = pos + 1;
        let mut hashes = 0usize;
        while p < chars.len() && chars[p] == '#' {
            hashes += 1;
            p += 1;
        }
        if p < chars.len() && chars[p] == '"' {
            return parse_raw_string(chars, hashes, p);
        }
    }
    None
}

fn parse_plain_string(chars: &[char], pos: usize) -> Option<(String, usize)> {
    let n = chars.len();
    let mut i = pos + 1;
    let mut out = String::new();
    while i < n {
        let c = chars[i];
        if c == '"' {
            return Some((out, i + 1));
        }
        if c == '\\' && i + 1 < n {
            let esc = chars[i + 1];
            match esc {
                'n' => {
                    out.push('\n');
                    i += 2;
                }
                't' => {
                    out.push('\t');
                    i += 2;
                }
                'r' => {
                    out.push('\r');
                    i += 2;
                }
                '\\' => {
                    out.push('\\');
                    i += 2;
                }
                '"' => {
                    out.push('"');
                    i += 2;
                }
                '\'' => {
                    out.push('\'');
                    i += 2;
                }
                '0' => {
                    out.push('\0');
                    i += 2;
                }
                'x' if i + 3 < n => {
                    let hex: String = chars[i + 2..i + 4].iter().collect();
                    match u8::from_str_radix(&hex, 16) {
                        Ok(v) => {
                            out.push(v as char);
                            i += 4;
                        }
                        Err(_) => i += 2,
                    }
                }
                'u' if i + 2 < n && chars[i + 2] == '{' => {
                    let mut j = i + 3;
                    let mut hex = String::new();
                    while j < n && chars[j] != '}' {
                        hex.push(chars[j]);
                        j += 1;
                    }
                    if j < n {
                        if let Ok(cp) = u32::from_str_radix(&hex, 16) {
                            if let Some(ch) = char::from_u32(cp) {
                                out.push(ch);
                            }
                        }
                        i = j + 1;
                    } else {
                        i += 2;
                    }
                }
                '\n' => {
                    // String continuation: the backslash-newline and any
                    // leading whitespace on the next line are elided.
                    i += 2;
                    while i < n && chars[i].is_whitespace() {
                        i += 1;
                    }
                }
                other => {
                    out.push(other);
                    i += 2;
                }
            }
            continue;
        }
        out.push(c);
        i += 1;
    }
    None
}

fn parse_raw_string(chars: &[char], hashes: usize, quote_pos: usize) -> Option<(String, usize)> {
    let n = chars.len();
    let mut i = quote_pos + 1;
    let content_start = i;
    while i < n {
        if chars[i] == '"' {
            let mut ok = true;
            for h in 0..hashes {
                if i + 1 + h >= n || chars[i + 1 + h] != '#' {
                    ok = false;
                    break;
                }
            }
            if ok {
                let content: String = chars[content_start..i].iter().collect();
                return Some((content, i + 1 + hashes));
            }
        }
        i += 1;
    }
    None
}

/// Skips a char literal (`'a'`, `'\n'`, `'\x41'`, `'\u{2764}'`) or, when the
/// `'` is actually the start of a lifetime (`'a`, no closing quote), just
/// the tick itself — either way, never mistakes an apostrophe for the start
/// of a string.
fn skip_char_literal_or_lifetime(chars: &[char], pos: usize) -> usize {
    let n = chars.len();
    if pos + 1 >= n {
        return pos + 1;
    }
    if chars[pos + 1] == '\\' {
        let mut i = pos + 2;
        if i < n && chars[i] == 'x' && i + 2 < n {
            i += 3;
        } else if i < n && chars[i] == 'u' && i + 1 < n && chars[i + 1] == '{' {
            i += 2;
            while i < n && chars[i] != '}' {
                i += 1;
            }
            if i < n {
                i += 1;
            }
        } else if i < n {
            i += 1;
        }
        if i < n && chars[i] == '\'' {
            return i + 1;
        }
        return i;
    }
    if pos + 2 < n && chars[pos + 2] == '\'' {
        return pos + 3;
    }
    pos + 1
}

/// Scans forward from `open_pos` (which must point at `open`) to the
/// matching `close`, skipping nested strings/comments and matching nested
/// occurrences of `open`/`close` themselves, and returns the text strictly
/// between the outer pair plus the index just past `close`. Falls back to
/// "rest of input" if unterminated (never panics/loops on malformed input).
fn scan_balanced_generic(
    chars: &[char],
    open_pos: usize,
    open: char,
    close: char,
) -> (String, usize) {
    let n = chars.len();
    let mut i = open_pos + 1;
    let content_start = i;
    let mut depth = 1i32;
    while i < n {
        let c = chars[i];
        if let Some((_, end)) = try_parse_string_literal(chars, i) {
            i = end;
            continue;
        }
        if c == '/' && i + 1 < n && chars[i + 1] == '/' {
            while i < n && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && i + 1 < n && chars[i + 1] == '*' {
            i += 2;
            let mut bd = 1;
            while i < n && bd > 0 {
                if chars[i] == '/' && i + 1 < n && chars[i + 1] == '*' {
                    bd += 1;
                    i += 2;
                } else if chars[i] == '*' && i + 1 < n && chars[i + 1] == '/' {
                    bd -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            continue;
        }
        if c == open {
            depth += 1;
            i += 1;
            continue;
        }
        if c == close {
            depth -= 1;
            if depth == 0 {
                let content: String = chars[content_start..i].iter().collect();
                return (content, i + 1);
            }
            i += 1;
            continue;
        }
        i += 1;
    }
    let content: String = chars[content_start..n].iter().collect();
    (content, n)
}

// ---------------------------------------------------------------------
// F4/F10, G1: containment + readability validation
// ---------------------------------------------------------------------

/// Reads the raw git blob at `rel_git` — an already-verified, `/`-joined
/// path relative to `checkout_root`, returned by
/// `verify_raw_path_tracked_by_git` — via `git show HEAD:<path>` (G1,
/// review_b iteration 1): a `.gitattributes`-configured smudge filter runs
/// during `git worktree add` just like it would during any other checkout
/// of this repository, and the checked-out worktree shares the SAME
/// `.git/config` (and therefore the same filter drivers) as the caller —
/// so a filter can freely substitute on-disk bytes for content that is NOT
/// what git actually committed. That substitution only reproduces on a
/// machine that happens to have the identical local filter driver
/// configured, which is exactly the kind of machine-local dependency this
/// check exists to catch (the same class of problem F1 already treats a
/// warm Cargo cache as). `git show` reads straight from the object
/// database, bypassing the working-tree filter pipeline entirely, so its
/// output is the one thing actually guaranteed reproducible from
/// `git worktree add` on any machine.
///
/// Takes the LITERAL git-relative path, never a canonicalized
/// (symlink-followed) one (G1, review_b iteration 6): comparing disk
/// content against the blob at wherever a path's symlinks happen to
/// RESOLVE to — rather than the blob at the path's own name — lets a
/// checkout filter replace a tracked regular file with a symlink alias to
/// a DIFFERENT, also-tracked file; the disk read and the git-blob lookup
/// would then both transparently follow the same alias and trivially
/// agree, hiding whatever the aliased file's own git blob actually
/// contains (e.g. a `mod`/`include!` reference the real committed content
/// is missing). `verify_raw_path_tracked_by_git` is the only thing that
/// gets to decide what path this reads.
fn read_git_tracked_bytes(
    checkout_root: &Path,
    rel_git: &str,
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    let mut cmd = Command::new("git");
    cmd.args(["show", &format!("HEAD:{rel_git}")])
        .current_dir(checkout_root);
    let output = run_with_timeout(&mut cmd, timeout)
        .map_err(|e| format!("could not read git-tracked content of {rel_git}: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "not tracked by git at HEAD ({}): {}",
            rel_git,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output.stdout)
}

/// Verifies that `raw_path` — the exact path this check is about to treat
/// as present, with EVERY component (not just the final one) still
/// unresolved on disk (no symlink anywhere in it followed) — is itself
/// reachable through git-tracked entries at HEAD (G1, review_b iterations
/// 3-4).
///
/// `read_git_tracked_bytes` / `canonicalize_within_checkout` only prove
/// that whatever `raw_path` ultimately *resolves to* (after following
/// every symlink) is tracked and matches git's blob. That is not the same
/// claim: a checkout-filter side effect can fabricate an untracked
/// symlink ALIAS anywhere along `raw_path` — a leaf file (iteration 3:
/// `mod missing_dep;` resolving through a filter-created
/// `src/missing_dep.rs -> real.rs` symlink) or a whole DIRECTORY
/// (iteration 4: `mod missing_dir;` resolving through a filter-created
/// `src/missing_dir -> real_dir` symlink to `src/missing_dir/mod.rs`) —
/// pointing at different, genuinely tracked content. Canonicalizing any
/// ancestor before asking git would silently follow that alias and ask
/// about the wrong (but tracked) path, passing even though the literal
/// path `raw_path` names is not itself committed, so a machine without
/// that exact filter driver gets nothing there at all.
///
/// Walks `raw_path` component by component relative to `checkout_root`
/// WITHOUT canonicalizing anything. Any ancestor prefix that is a symlink
/// on disk — plus the final component, always — must itself be a
/// git-tracked entry at HEAD (whether a tracked symlink object or,
/// combined with the caller's separate containment/content checks, a
/// tracked regular file/directory) for this to succeed.
///
/// `..`/`.` components (a `#[path]`/`mod` join can introduce them) are
/// resolved as the walk proceeds, component by component, WITHOUT ever
/// canonicalizing (that would defeat the whole point of the walk). A `..`
/// that cancels an ORDINARY (non-symlink) directory is safe to fold away —
/// that is exactly how the filesystem would resolve it too. But a `..`
/// that cancels a component this walk already found to be a symlink is
/// NOT safe to fold away lexically (review_b, iteration 5): the OS
/// resolves `..` after a symlink relative to wherever the symlink's
/// target actually is, which can be anywhere — not "back to where the
/// symlink's name lexically sat". Silently popping it here would let a
/// checkout-filter-fabricated untracked symlink component vanish from the
/// walk entirely (never asked about) as long as whatever remains after
/// the fold happens to name a tracked path, which is precisely how a
/// prior version of this walk (that normalized `.`/`..` in a first pass,
/// separate from the symlink check in a second pass) missed it. Refuse to
/// guess in that case: fail rather than silently resolve past an
/// unverified symlink.
fn verify_raw_path_tracked_by_git(
    checkout_root: &Path,
    raw_path: &Path,
    timeout: Duration,
) -> Result<String, String> {
    let canonical_root = std::fs::canonicalize(checkout_root)
        .map_err(|e| format!("failed to canonicalize checkout root: {e}"))?;
    let rel = raw_path
        .strip_prefix(checkout_root)
        .or_else(|_| raw_path.strip_prefix(&canonical_root))
        .map_err(|_| "resolves outside the checkout root (escaping path or symlink)".to_string())?;

    struct Segment {
        name: std::ffi::OsString,
        is_symlink: bool,
    }
    let mut stack: Vec<Segment> = Vec::new();
    let mut on_disk = checkout_root.to_path_buf();
    for component in rel.components() {
        match component {
            std::path::Component::ParentDir => match stack.pop() {
                None => {
                    return Err(
                        "resolves outside the checkout root (escaping path or symlink)".to_string(),
                    );
                }
                Some(popped) => {
                    on_disk.pop();
                    if popped.is_symlink {
                        return Err(format!(
                            "path navigates `..` through symlink component `{}` — \
                             refusing to resolve past it without following it, which \
                             this check will not do (a checkout filter could fabricate \
                             the alias)",
                            popped.name.to_string_lossy(),
                        ));
                    }
                }
            },
            std::path::Component::CurDir => {}
            other => {
                on_disk.push(other.as_os_str());
                let is_symlink = std::fs::symlink_metadata(&on_disk)
                    .map(|m| m.file_type().is_symlink())
                    .unwrap_or(false);
                stack.push(Segment {
                    name: other.as_os_str().to_owned(),
                    is_symlink,
                });
            }
        }
    }
    if stack.is_empty() {
        return Err("resolves to the checkout root itself, not a file".to_string());
    }

    let mut accumulated = PathBuf::new();
    let last_index = stack.len() - 1;
    for (index, segment) in stack.iter().enumerate() {
        accumulated.push(&segment.name);
        if index != last_index && !segment.is_symlink {
            // An ordinary (non-symlink) intermediate directory is exactly
            // what `git worktree add` would have produced for any tracked
            // path through it — no alias risk, no need to ask git about
            // every ancestor.
            continue;
        }
        let rel_git = accumulated
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        let mut cmd = Command::new("git");
        cmd.args(["cat-file", "-e", &format!("HEAD:{rel_git}")])
            .current_dir(checkout_root);
        let output = run_with_timeout(&mut cmd, timeout)
            .map_err(|e| format!("could not verify git tracking of {rel_git}: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "{rel_git} is not itself a tracked entry at HEAD (only \
                 something it points to, e.g. via a symlink alias, is)"
            ));
        }
    }
    // Every checked segment above passed, so `accumulated` now holds the
    // complete literal path — build the same `/`-joined form once more and
    // hand it back so callers can read the git blob at this EXACT path
    // (never a canonicalized one) via `read_git_tracked_bytes` (G1,
    // review_b iteration 6).
    let final_rel_git = accumulated
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/");
    Ok(final_rel_git)
}

/// Canonicalizes `path` and confirms it resolves inside `checkout_root`
/// (G1): a local package root, target entry point, or `mod` resolution
/// reached only through an escaping absolute path or symlink must never
/// be treated as present just because something exists there on this
/// host — only content this check can prove came from the git checkout
/// (i.e. lives inside the fresh `git worktree`) counts.
fn canonicalize_within_checkout(path: &Path, checkout_root: &Path) -> Result<PathBuf, String> {
    let canonical_root = std::fs::canonicalize(checkout_root)
        .map_err(|e| format!("failed to canonicalize checkout root: {e}"))?;
    let canonical = std::fs::canonicalize(path)
        .map_err(|_| "not tracked by git (missing from a fresh checkout)".to_string())?;
    if !canonical.starts_with(&canonical_root) {
        return Err("resolves outside the checkout root (escaping path or symlink)".to_string());
    }
    Ok(canonical)
}

/// Validates an include target for repository-containment (F4/F10):
/// rejects absolute-path arguments outright, canonicalizes against the
/// checkout root and rejects anything that resolves outside it (an
/// escaping relative path OR a symlink whose target escapes), requires a
/// readable regular file, and — for `include_str!` specifically — requires
/// valid UTF-8. Returns the canonicalized target on success (used by
/// `include!` to recurse into it as further Rust source).
fn validate_include_target(
    referencing_file: &Path,
    literal: &str,
    checkout_root: &Path,
    kind: IncludeKind,
    timeout: Duration,
) -> Result<PathBuf, String> {
    if Path::new(literal).is_absolute() {
        return Err("absolute path; not something git tracks".to_string());
    }
    let candidate = referencing_file
        .parent()
        .unwrap_or(referencing_file)
        .join(literal);
    let canonical_target = canonicalize_within_checkout(&candidate, checkout_root)?;
    // G1 (review_b iteration 3): confirm the LITERAL path this macro names
    // is itself tracked by git, not just whatever it resolves to after
    // following symlinks — see `verify_raw_path_tracked_by_git`. Keep the
    // exact git-relative path it returns; the byte comparison below must
    // read THAT blob, not whatever `canonical_target` happens to resolve
    // to (review_b iteration 6).
    let rel_git = verify_raw_path_tracked_by_git(checkout_root, &candidate, timeout)?;
    let meta = std::fs::metadata(&canonical_target)
        .map_err(|e| format!("could not stat resolved target: {e}"))?;
    if !meta.is_file() {
        return Err("resolves to a directory, not a readable regular file".to_string());
    }
    // G2: a regular file that exists but cannot actually be READ
    // (permission denied) must never be counted as "present" — `stat`
    // alone cannot see that. Read the full bytes for every kind (not just
    // `Str`), so a permission error surfaces uniformly and the G1 check
    // below has disk bytes to compare against git's own copy.
    let disk_bytes = std::fs::read(&canonical_target)
        .map_err(|e| format!("could not read resolved target: {e}"))?;
    if kind == IncludeKind::Str && std::str::from_utf8(&disk_bytes).is_err() {
        return Err("is not valid UTF-8, required by include_str!".to_string());
    }
    // G1 (review_b, iteration 1): `disk_bytes` came off disk AFTER the
    // diagnostic checkout ran — a configured smudge filter could have
    // substituted them for content that is not what git actually
    // committed. Compare against the raw git blob at the LITERAL path
    // (review_b iteration 6) — using `canonical_target` here would follow
    // any symlink `candidate` itself turned out to be and silently compare
    // against a different (but also tracked) file's blob instead. A
    // mismatch means this check cannot certify the bytes cargo would embed
    // as something a fresh checkout on ANY machine (with or without that
    // filter driver configured) is guaranteed to reproduce.
    let git_bytes = read_git_tracked_bytes(checkout_root, &rel_git, timeout)?;
    if git_bytes != disk_bytes {
        return Err(
            "checked-out content differs from the committed git blob — a \
             checkout filter (see .gitattributes) substituted content \
             this check cannot verify came from git"
                .to_string(),
        );
    }
    // Return the RAW (unresolved) candidate, not `canonical_target`: a
    // caller that recurses into this target (an `include!`) must keep
    // treating it as an unverified literal path so its own top-level
    // git-content check (see `scan_file_and_follow`) asks git about the
    // exact same name, not whatever it resolves to on disk.
    Ok(candidate)
}

/// The module-resolution base directory `mod x;` (without `#[path]`)
/// resolves against: the same directory for a crate root (a target entry
/// point) or an explicit `mod.rs`, otherwise a subdirectory named after the
/// current file's own stem — the standard (non-`#[path]`) Rust 2018+ rule.
fn module_dir_for(path: &Path, is_root: bool) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let is_mod_rs = path.file_name().and_then(|n| n.to_str()) == Some("mod.rs");
    if is_root || is_mod_rs {
        parent.to_path_buf()
    } else {
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        parent.join(stem)
    }
}

// ---------------------------------------------------------------------
// F5/F9: symlink-safe, error-propagating traversal
// ---------------------------------------------------------------------

/// Accumulates scan results across every local package. `errors` (F9) are
/// checked first by the caller and always force a non-PASS result; `missing`
/// (F2/F4/F5-adjacent) are real FAILs; `inconclusive` (F3/F8) downgrade an
/// otherwise-clean scan to WARN instead of a false PASS.
struct ScanState {
    worktree_path: PathBuf,
    checked: usize,
    missing: Vec<String>,
    inconclusive: Vec<String>,
    errors: Vec<String>,
    visited: HashSet<PathBuf>,
    /// Git-verified literal paths (the `rel_git` a file's own content was
    /// already checked against) whose `mod`/`include!` children have
    /// already been followed. Keyed on the LITERAL tracked path, never on
    /// `visited`'s canonicalized one (G1, review_b iteration 7): a checkout
    /// filter can clobber one tracked file's checked-out bytes into a
    /// symlink alias pointing at a DIFFERENT, already-scanned tracked
    /// file, so `canonical` for the clobbered file collides with a path
    /// already in `visited` even though the clobbered file's own git blob
    /// was never compared against its (now substituted) disk content. This
    /// set is only consulted AFTER that per-file content comparison
    /// succeeds, so it dedupes recursion (cycle protection) without ever
    /// skipping the check that catches the substitution.
    verified: HashSet<String>,
    /// Deadline for every `git show` this scan issues to verify content
    /// against the raw git blob (G1, review_b iteration 1) — same F14
    /// discipline as every other external command this check spawns.
    timeout: Duration,
}

impl ScanState {
    fn new(worktree_path: PathBuf, timeout: Duration) -> Self {
        Self {
            worktree_path,
            checked: 0,
            missing: Vec::new(),
            inconclusive: Vec::new(),
            errors: Vec::new(),
            visited: HashSet::new(),
            verified: HashSet::new(),
            timeout,
        }
    }

    fn rel(&self, path: &Path) -> String {
        display_rel(path, &self.worktree_path)
    }

    /// Defensive readability probe (F9), independent of the
    /// metadata-reachable module graph (G3): confirms every `.rs` file
    /// under `src`/`tests` can actually be read, WITHOUT interpreting its
    /// content — a `mod`/`include!` reference inside a file no target
    /// entry point's module graph ever reaches is never compiled by
    /// cargo, so a missing target there must never fail this check
    /// (that was G3's bug: the directory walk used to call
    /// `scan_file_and_follow` on every `.rs` file here, turning dead code
    /// into a false FAIL). An outright I/O error reading the checkout
    /// itself — this check's own diagnostic worktree materialization went
    /// wrong somehow — is a different, always-relevant class of problem
    /// and is still surfaced here, with the same symlink-skip and
    /// error-propagation discipline the reachable-graph walk uses
    /// (F5/F9).
    fn check_dir_readable(&mut self, dir: &Path) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) => {
                self.errors.push(format!("{}: {e}", self.rel(dir)));
                return;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    self.errors.push(format!("{}: {e}", self.rel(dir)));
                    continue;
                }
            };
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(ft) => ft,
                Err(e) => {
                    self.errors.push(format!("{}: {e}", self.rel(&path)));
                    continue;
                }
            };
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                self.check_dir_readable(&path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                if let Ok(canonical) = std::fs::canonicalize(&path) {
                    if self.visited.contains(&canonical) {
                        // Already read via the metadata-reachable graph
                        // walk — no need to probe it again.
                        continue;
                    }
                }
                if let Err(e) = std::fs::read(&path) {
                    self.errors.push(format!("{}: {e}", self.rel(&path)));
                }
            }
        }
    }

    /// Scans one Rust source file for `mod`/include references and follows
    /// them (F2/F6). `is_root` marks a metadata target entry point (or
    /// `build.rs`) — the module-resolution basis for any bare `mod x;` it
    /// declares. Every call verifies `path`'s own content against its own
    /// git blob before consulting any dedup set (G1, review_b iteration 7)
    /// — cycle protection (`self.verified`, keyed on the literal git path)
    /// is applied only AFTER that verification succeeds, so it can never
    /// skip the check that catches a clobbered file impersonating an
    /// already-scanned one.
    fn scan_file_and_follow(&mut self, path: &Path, is_root: bool) {
        let canonical = match std::fs::canonicalize(path) {
            Ok(p) => p,
            Err(e) => {
                self.errors.push(format!("{}: {e}", self.rel(path)));
                return;
            }
        };
        // Perf-only bookkeeping for `check_dir_readable`'s "already read via
        // the graph walk" skip — never used to gate the content check below.
        self.visited.insert(canonical);
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                self.errors.push(format!("{}: {e}", self.rel(path)));
                return;
            }
        };
        // G1 (review_b, iteration 1): `content` came off disk AFTER the
        // diagnostic checkout ran, so a configured smudge filter could
        // have silently rewritten it — hiding a real `mod`/`include!`
        // reference (or fabricating one) that only the raw git blob
        // actually contains. Parsing filtered content would examine
        // source no fresh checkout on another machine is guaranteed to
        // reproduce; treat any mismatch (or an unreadable/non-UTF-8 git
        // blob) as a scan failure rather than trusting disk content.
        //
        // Ask git about `path` itself — never `canonical` (review_b
        // iteration 6): a checkout filter can clobber a tracked regular
        // file's checked-out bytes with a symlink alias to a DIFFERENT,
        // also-tracked file. `canonical` follows that alias, so both the
        // disk read above and a git-blob lookup keyed on `canonical` would
        // transparently agree on the alias TARGET's content — hiding
        // whatever `path`'s own git blob (the file this scan actually
        // thinks it is reading) really contains.
        let rel_git = match verify_raw_path_tracked_by_git(&self.worktree_path, path, self.timeout)
        {
            Ok(s) => s,
            Err(e) => {
                self.errors.push(format!("{}: {e}", self.rel(path)));
                return;
            }
        };
        let git_bytes = match read_git_tracked_bytes(&self.worktree_path, &rel_git, self.timeout) {
            Ok(b) => b,
            Err(e) => {
                self.errors.push(format!("{}: {e}", self.rel(path)));
                return;
            }
        };
        match String::from_utf8(git_bytes) {
            Ok(git_content) if git_content == content => {}
            Ok(_) => {
                self.errors.push(format!(
                    "{}: checked-out content differs from the committed git \
                     blob — a checkout filter (see .gitattributes) \
                     substituted content this check cannot verify came \
                     from git",
                    self.rel(path)
                ));
                return;
            }
            Err(e) => {
                self.errors.push(format!(
                    "{}: committed git blob is not valid UTF-8: {e}",
                    self.rel(path)
                ));
                return;
            }
        }
        // Cycle protection only now, after `path`'s own git blob has been
        // proven to match what's on disk — dedupes on the literal tracked
        // path (finite: it names one git blob), never on `canonical`, which
        // a clobbering symlink could make collide with an unrelated,
        // already-verified file.
        if !self.verified.insert(rel_git.clone()) {
            return;
        }
        let parsed = scan_source(&content);
        let dir_for_submodules = module_dir_for(path, is_root);

        for m in parsed.mods {
            // F8: a `mod` this scanner cannot evaluate the cfg-gate for is
            // skipped entirely rather than validated — Rust never expands
            // an inactive module, so an absent target there must never FAIL.
            if m.cfg_gated {
                continue;
            }
            let resolved = match &m.path_override {
                Some(p) => path.parent().unwrap_or(path).join(p),
                None => {
                    let same_name = dir_for_submodules.join(format!("{}.rs", m.name));
                    if same_name.is_file() {
                        same_name
                    } else {
                        dir_for_submodules.join(&m.name).join("mod.rs")
                    }
                }
            };
            if !resolved.is_file() {
                self.missing.push(format!(
                    "{} -> {} (mod `{}` not tracked by git)",
                    self.rel(path),
                    self.rel(&resolved),
                    m.name,
                ));
                continue;
            }
            // G1: `resolved.is_file()` follows symlinks — a `mod`
            // resolved only through a symlink escaping the checkout root
            // must be rejected the same way an escaping include! target
            // already is (F4/F10), not silently followed and scanned as
            // if it were checked-out content.
            let containment = canonicalize_within_checkout(&resolved, &self.worktree_path);
            // G1 (review_b iteration 3): containment alone proves
            // `resolved` lives inside the checkout — it does not prove
            // `resolved` itself (as opposed to whatever it points to) is
            // tracked by git. A checkout-filter side effect can fabricate
            // an untracked symlink alias at exactly this path pointing at
            // a DIFFERENT, genuinely tracked file, which containment and
            // the eventual byte comparison both miss.
            let tracked = containment.as_ref().ok().and_then(|_| {
                verify_raw_path_tracked_by_git(&self.worktree_path, &resolved, self.timeout).err()
            });
            match (containment, tracked) {
                (Ok(_), None) => self.scan_file_and_follow(&resolved, false),
                (Ok(_), Some(reason)) => {
                    self.missing.push(format!(
                        "{} -> {} (mod `{}` {reason})",
                        self.rel(path),
                        self.rel(&resolved),
                        m.name,
                    ));
                }
                (Err(reason), _) => {
                    self.missing.push(format!(
                        "{} -> {} (mod `{}` {reason})",
                        self.rel(path),
                        self.rel(&resolved),
                        m.name,
                    ));
                }
            }
        }

        for mc in parsed.macros {
            if mc.cfg_gated {
                self.inconclusive.push(format!(
                    "{} -> {}!(...) is gated by an #[cfg(...)] this check cannot \
                     evaluate — treated as inconclusive",
                    self.rel(path),
                    mc.kind.macro_name(),
                ));
                continue;
            }
            let literal = match &mc.literal {
                Some(l) => l,
                None => {
                    self.inconclusive.push(format!(
                        "{} -> {}!(...) argument is not a string literal — \
                         treated as inconclusive",
                        self.rel(path),
                        mc.kind.macro_name(),
                    ));
                    continue;
                }
            };
            match validate_include_target(path, literal, &self.worktree_path, mc.kind, self.timeout)
            {
                Ok(resolved) => {
                    self.checked += 1;
                    if mc.kind == IncludeKind::Include {
                        self.scan_file_and_follow(&resolved, false);
                    }
                }
                Err(reason) => {
                    self.missing
                        .push(format!("{} -> {literal} ({reason})", self.rel(path),));
                }
            }
        }
    }
}
