use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Default wall-clock cap for the `cargo check` this check runs: a wedged
/// `cargo` (shared cache lock, a hung build script, a dependency resolution
/// that blocks despite `--offline`) must never wedge an ordinary health
/// check. Env-overridable via `HEX_DOCTOR_HARNESS_BUILDABLE_TIMEOUT_SECS`
/// for a slower machine or a deliberately slow build.
const DEFAULT_CARGO_CHECK_TIMEOUT_SECS: u64 = 600;
const CARGO_CHECK_TIMEOUT_ENV: &str = "HEX_DOCTOR_HARNESS_BUILDABLE_TIMEOUT_SECS";

fn cargo_check_timeout() -> Duration {
    std::env::var(CARGO_CHECK_TIMEOUT_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(DEFAULT_CARGO_CHECK_TIMEOUT_SECS))
}

/// The harness must actually BUILD from exactly what git has committed at
/// `HEAD` — not from whatever a locally configured smudge filter, checkout
/// hook, or working-tree state happens to produce on this one machine.
///
/// A hand-written source scanner previously approximated this (mod/include
/// graph walk vs. committed blobs) and absorbed 8+ review rounds, each
/// finding a new way real source could diverge from what the scanner
/// modeled (cfg-gated modules, inline module bodies, comments in
/// lookahead, sparse-checkout restoration, committed symlinks, a
/// smudge-filtered `Cargo.toml`). That class of gap is unbounded because
/// the scanner approximates `cargo`+`rustc`. This check now runs the real
/// thing instead: export the committed tree straight from git's object
/// database (never `git archive`, `git checkout`, or `git worktree add` —
/// all three run the same smudge/clean filter pipeline an ordinary
/// checkout does, which is exactly what a machine-local filter driver can
/// use to defeat a check based on any of them) and run `cargo check`
/// against that export, offline.
///
/// See `~/hex/me/decisions/hex-doctor-harness-buildable-build-not-scan-2026-09-11.md`
/// for the design decision this replaces.
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

fn run_check(hex_dir: &Path) -> CheckResult {
    run_check_with_timeout(hex_dir, cargo_check_timeout())
}

/// Same check as `run_check`, but the `cargo check` this runs is bound to
/// `timeout` instead of the env-configurable default — lets tests inject a
/// tiny cap (the wall-clock-cap contract test) or a generous one, without
/// depending on process-wide environment mutation.
pub(crate) fn run_check_with_timeout(hex_dir: &Path, timeout: Duration) -> CheckResult {
    run_check_impl(hex_dir, timeout, None)
}

/// Test-only entry point: same as `run_check_with_timeout`, but overrides
/// `PATH` for the `cargo check` child process only (never the whole test
/// process's environment, which would race every other concurrently
/// running test) — lets the "no toolchain on PATH" contract test simulate
/// a machine with no `cargo`/`rustc` reachable at all.
#[cfg(test)]
pub(crate) fn run_check_with_timeout_and_path_override(
    hex_dir: &Path,
    timeout: Duration,
    path_override: &str,
) -> CheckResult {
    run_check_impl(hex_dir, timeout, Some(path_override))
}

#[cfg(test)]
thread_local! {
    static LAST_EXPORT_PATH: std::cell::RefCell<Option<std::path::PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Test-only: the export directory path this thread's most recent
/// `run_check_with_timeout`/`run_check_with_timeout_and_path_override` call
/// used, so a test can assert it was removed after the check returned.
#[cfg(test)]
pub(crate) fn last_export_path_for_tests() -> Option<std::path::PathBuf> {
    LAST_EXPORT_PATH.with(|p| p.borrow().clone())
}

fn run_check_impl(hex_dir: &Path, timeout: Duration, path_override: Option<&str>) -> CheckResult {
    let start = Instant::now();

    // Export cleanup (success, failure, or panic-unwind) is handled by
    // `tempfile::TempDir`'s own `Drop` — `export` is the guard, and every
    // return path below simply lets it go out of scope.
    let export = match export_committed_head(hex_dir) {
        Ok(e) => e,
        Err(e) => {
            return CheckResult::warn(format!(
                "could not export the committed HEAD tree to verify the harness \
                 builds from git (archive failure) — this check cannot certify \
                 anything: {e}"
            ));
        }
    };
    // Test-only: record this invocation's export path in a THREAD-LOCAL
    // (never a process-global) so the "tempdir removed after every outcome"
    // contract test can assert on this exact path after the check returns
    // and the `TempDir` guard has dropped — safe under `cargo test`'s
    // parallel execution because each test runs on its own thread, so a
    // concurrently running test's export can never clobber this one's
    // recorded path.
    #[cfg(test)]
    LAST_EXPORT_PATH.with(|p| *p.borrow_mut() = Some(export.path().to_path_buf()));

    let harness_dir = export.path().join(".hex/harness");
    if !harness_dir.join("Cargo.toml").is_file() {
        return CheckResult::fail(
            ".hex/harness/Cargo.toml -> missing from a fresh export of git's \
             committed HEAD (not tracked, or gitignored) — fix: git add it or \
             install it from .hex/.upgrade-cache"
                .to_string(),
        );
    }

    let target_dir = hex_dir.join(".hex/cache/doctor-buildable-target");
    if let Err(e) = ensure_target_cache_dir(&target_dir) {
        return CheckResult::warn(format!(
            "could not prepare the cargo target cache at {} — this check \
             cannot run cargo at all: {e}",
            target_dir.display()
        ));
    }

    let mut cmd = Command::new("cargo");
    cmd.args(["check", "-p", "hex-harness", "--offline", "--locked"])
        .current_dir(&harness_dir)
        .env("CARGO_TARGET_DIR", &target_dir)
        .env("CARGO_NET_OFFLINE", "true");
    if let Some(path) = path_override {
        cmd.env("PATH", path);
    }

    let output = match run_with_timeout(&mut cmd, timeout) {
        Ok(o) => o,
        Err(e) => {
            return CheckResult::warn(format!(
                "could not run `cargo check -p hex-harness --offline --locked` \
                 to verify the harness builds from git — this is inconclusive, \
                 not a build failure: {e}"
            ));
        }
    };

    let elapsed = start.elapsed();
    if output.status.success() {
        return CheckResult::pass(format!(
            "harness builds from git (cargo check -p hex-harness --offline \
             --locked passed in {:.1}s)",
            elapsed.as_secs_f64()
        ));
    }
    classify_check_failure(&output, elapsed)
}

/// Creates `dir` (and its ancestors) if needed and restricts it to owner
/// access only (`0700`) — a persistent, shared cache directory under
/// `$HEX_DIR/.hex/cache`, not a one-shot temp dir, so it is worth locking
/// down the same way any other `.hex/cache` artifact is.
fn ensure_target_cache_dir(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("failed to create {}: {e}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("failed to set permissions on {}: {e}", dir.display()))?;
    }
    Ok(())
}

/// Distinguishes a genuine build failure (FAIL — the harness does not
/// compile from what git has committed) from a `cargo check` that ran but
/// could not certify anything either way (WARN): an offline dependency it
/// could not resolve, or a toolchain it could not even invoke (`rustc`
/// unreachable). Both checks are gated on `looks_like_a_real_compiler_diagnostic`
/// first — a genuine compile error (missing module/include, broken
/// manifest target) always carries its own `error[E....]:`/`-->`/`could not
/// compile` markers, and a fixture crafted so its OWN failing source text
/// happens to *contain* the offline or toolchain wording (e.g. a
/// `compile_error!` message quoting it) must still FAIL — a substring match
/// against the whole stderr blob, with no such gate, would misclassify that
/// real failure as an inconclusive WARN.
fn classify_check_failure(output: &Output, elapsed: Duration) -> CheckResult {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let lower = stderr.to_lowercase();
    let real_diagnostic = looks_like_a_real_compiler_diagnostic(&stderr, &lower);

    if !real_diagnostic && is_offline_dependency_failure(&lower) {
        return CheckResult::warn(format!(
            "a dependency the lockfile resolves to is not present in the \
             local Cargo registry/git cache (dependency cache unavailable — \
             this is not a build failure) — fix: warm the cache with network \
             access, or run `cargo fetch` once online: {}",
            stderr.trim()
        ));
    }
    if !real_diagnostic && is_toolchain_unavailable_failure(&lower) {
        return CheckResult::warn(format!(
            "cargo could not execute `rustc` at all (toolchain unreachable \
             on this machine — this is not a build failure) — fix: install \
             or repair the rust toolchain: {}",
            stderr.trim()
        ));
    }
    let first_error = first_cargo_error_line(&stderr).unwrap_or_else(|| stderr.trim());
    CheckResult::fail(format!(
        "harness does not build from the committed git tree (cargo check -p \
         hex-harness --offline --locked failed in {:.1}s) — {}",
        elapsed.as_secs_f64(),
        first_error
    ))
    .with_details(stderr.into_owned())
}

/// `true` iff `stderr` carries the unmistakable shape of an actual `rustc`
/// diagnostic (an error CODE, a `-->` source-location arrow, or cargo's own
/// "could not compile" summary line) rather than a pre-compilation cargo
/// failure (offline dependency resolution, an unreachable toolchain). Real
/// compilation never gets far enough to emit any of these three markers
/// until at least one translation unit has actually been fed to `rustc`.
fn looks_like_a_real_compiler_diagnostic(stderr: &str, lower: &str) -> bool {
    lower.contains("error[") || stderr.contains("-->") || lower.contains("could not compile `")
}

/// `cargo check --offline` reports an unresolvable dependency with this
/// reminder wording regardless of subcommand — never itself the FIRST
/// `error:` line (that names the missing package instead), which is why
/// this checks the whole stderr blob, gated by `looks_like_a_real_compiler_diagnostic`
/// above rather than trusted on its own.
fn is_offline_dependency_failure(lower_stderr: &str) -> bool {
    lower_stderr.contains("offline mode (--offline)")
        || lower_stderr.contains("--offline was specified")
}

/// `cargo check` reports an unreachable `rustc` (missing from `PATH`, no
/// working toolchain) as `error: could not execute process `rustc -vV`
/// (never executed)` — a cargo-level failure, not a compile error, so it
/// must WARN rather than FAIL just like the offline case above.
fn is_toolchain_unavailable_failure(lower_stderr: &str) -> bool {
    lower_stderr.contains("could not execute process `rustc")
        || lower_stderr.contains("could not execute process 'rustc")
}

/// The first line of cargo's own diagnostic output that names a compile or
/// manifest error — by construction, the line that names a missing
/// file/module/manifest target. cargo's own error lines always start with
/// `error` (colorization is off because stdout/stderr are piped, not a
/// tty).
fn first_cargo_error_line(stderr: &str) -> Option<&str> {
    stderr.lines().find(|l| l.trim_start().starts_with("error"))
}

/// Runs `cmd` under a hard deadline using only `std`: spawn, then poll
/// `try_wait` in a loop instead of blocking on `output()`/`wait()`.
/// Killing the child on timeout means a wedged external command — `cargo`
/// stuck on a shared cache lock, a hung build script, a blocking checkout
/// filter — can never hang an ordinary health check. stdout/stderr are
/// drained on background threads so a chatty child can't deadlock on a
/// full pipe buffer while we poll.
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
    // These reader threads are intentionally never joined below. The
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
    // than joining unconditionally. A small floor keeps this from
    // collapsing to a zero-length window when the main loop above already
    // consumed the whole `timeout` (e.g. the child itself was killed for
    // running over).
    //
    // A single `drain_budget` value handed unchanged to BOTH
    // `recv_timeout` calls would let a pipe that closes quickly "refund"
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

/// One entry from `git ls-tree -r -z HEAD`: a file mode, its blob sha, and
/// its repo-relative path.
struct TreeEntry {
    mode: String,
    sha: String,
    path: String,
}

/// `true` iff a symlink recorded at `dest` (inside `export_root`) with the
/// given raw `target` (the exact bytes git stored for a mode `120000`
/// blob) resolves, purely LEXICALLY, to a path still inside `export_root`.
/// The target is never required to exist — that is exactly the attack this
/// guards against: a target naming a real but UNCOMMITTED file elsewhere on
/// this machine (e.g. back in the actual checkout this export was made
/// from). An absolute target, or a relative one whose `..` segments walk
/// back out of `export_root`, must never be materialized: `cargo check`
/// would then silently read content from outside the git-committed export
/// and this check would falsely certify a broken commit as buildable.
#[cfg(unix)]
fn symlink_target_stays_within_export(export_root: &Path, dest: &Path, target: &str) -> bool {
    let target_path = Path::new(target);
    if target_path.is_absolute() {
        return false;
    }
    let Some(start_dir) = dest.parent() else {
        return false;
    };
    let mut resolved = start_dir.to_path_buf();
    for component in target_path.components() {
        match component {
            std::path::Component::ParentDir => {
                resolved.pop();
            }
            std::path::Component::CurDir => {}
            std::path::Component::Normal(segment) => resolved.push(segment),
            std::path::Component::RootDir | std::path::Component::Prefix(_) => return false,
        }
    }
    resolved.starts_with(export_root)
}

/// Materializes the git-committed tree at `HEAD` of the repository rooted
/// at `repo_root` into a fresh, unique temp directory, using ONLY raw git
/// objects (`git ls-tree` + `git cat-file --batch`) — never `git archive`
/// (which runs the exact same smudge-filter pipeline as `git checkout`; a
/// locally configured smudge filter defeats it exactly like it defeats a
/// worktree-based scanner), `git checkout`, or `git worktree add` (which
/// also run that pipeline and can trigger checkout/index hooks). No filter
/// driver, hook, or credential helper is ever invoked; the caller's
/// working tree, index, and `.git` state are never touched. Returns the
/// `TempDir` itself — its `Drop` removes the export unconditionally
/// (success, failure, or panic-unwind), so it doubles as the cleanup guard.
fn export_committed_head(repo_root: &Path) -> Result<tempfile::TempDir, String> {
    let tempdir = tempfile::Builder::new()
        .prefix("hex-doctor-harness-buildable-export-")
        .tempdir()
        .map_err(|e| format!("failed to create export temp dir: {e}"))?;

    let mut ls_tree = Command::new("git");
    ls_tree
        .args(["ls-tree", "-r", "-z", "--full-tree", "HEAD"])
        .current_dir(repo_root)
        // A locally configured `git replace` ref transparently substitutes
        // a different object for the one a sha names, at any layer (the
        // `HEAD` commit, a tree, or a blob) — silently walking the
        // SUBSTITUTED tree here would export whatever the replacement
        // graph says instead of what git actually has committed, letting a
        // machine-local replacement defeat this check exactly like a
        // smudge filter would. `GIT_NO_REPLACE_OBJECTS` disables that
        // substitution for this process only.
        .env("GIT_NO_REPLACE_OBJECTS", "1");
    let ls_output = ls_tree
        .output()
        .map_err(|e| format!("git ls-tree failed to spawn: {e}"))?;
    if !ls_output.status.success() {
        return Err(format!(
            "git ls-tree -r HEAD failed: {}",
            String::from_utf8_lossy(&ls_output.stderr).trim()
        ));
    }

    let mut entries = Vec::new();
    for raw in ls_output.stdout.split(|&b| b == 0) {
        if raw.is_empty() {
            continue;
        }
        let line = String::from_utf8_lossy(raw);
        let (meta, path) = line
            .split_once('\t')
            .ok_or_else(|| format!("unexpected `git ls-tree` line: {line}"))?;
        let mut parts = meta.split(' ');
        let mode = parts
            .next()
            .ok_or("malformed `git ls-tree` entry (no mode)")?
            .to_string();
        let _kind = parts.next();
        let sha = parts
            .next()
            .ok_or("malformed `git ls-tree` entry (no sha)")?
            .to_string();
        // Gitlinks (submodule references, mode 160000) name a commit in
        // another repository, not a blob this export can materialize.
        if mode == "160000" {
            continue;
        }
        entries.push(TreeEntry {
            mode,
            sha,
            path: path.to_string(),
        });
    }

    if entries.is_empty() {
        return Ok(tempdir);
    }

    // One `git cat-file --batch` process reads every blob's content —
    // avoids spawning one `git cat-file -p <sha>` process per tracked file.
    let mut batch_cmd = Command::new("git");
    batch_cmd
        .args(["cat-file", "--batch"])
        .current_dir(repo_root)
        // Same replacement-ref hazard as `ls-tree` above — a blob sha
        // resolved through a local replace ref would hand this export
        // substituted content instead of what `HEAD`'s tree actually
        // records.
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = batch_cmd
        .spawn()
        .map_err(|e| format!("git cat-file --batch failed to spawn: {e}"))?;
    let mut stdin = child.stdin.take().expect("stdin was piped");
    let mut stdout = child.stdout.take().expect("stdout was piped");
    let mut stderr = child.stderr.take().expect("stderr was piped");
    let shas: String = entries.iter().map(|e| format!("{}\n", e.sha)).collect();
    let writer = std::thread::spawn(move || {
        use std::io::Write;
        let _ = stdin.write_all(shas.as_bytes());
        // Dropping `stdin` here (end of closure) closes the pipe, which is
        // what tells `git cat-file --batch` there are no more objects to
        // look up.
    });
    let mut buf = Vec::new();
    stdout
        .read_to_end(&mut buf)
        .map_err(|e| format!("failed reading `git cat-file --batch` output: {e}"))?;
    let mut err_buf = Vec::new();
    let _ = stderr.read_to_end(&mut err_buf);
    writer
        .join()
        .map_err(|_| "git cat-file --batch stdin writer thread panicked".to_string())?;
    let status = child
        .wait()
        .map_err(|e| format!("git cat-file --batch did not exit: {e}"))?;
    if !status.success() {
        return Err(format!(
            "git cat-file --batch exited with failure: {}",
            String::from_utf8_lossy(&err_buf).trim()
        ));
    }

    let mut cursor = 0usize;
    for entry in &entries {
        let header_end = buf[cursor..]
            .iter()
            .position(|&b| b == b'\n')
            .ok_or("truncated `git cat-file --batch` output (missing header)")?;
        let header = String::from_utf8_lossy(&buf[cursor..cursor + header_end]).into_owned();
        cursor += header_end + 1;
        let mut header_parts = header.split(' ');
        let _sha = header_parts.next();
        let _kind = header_parts.next();
        let size: usize = header_parts
            .next()
            .ok_or("malformed `git cat-file --batch` header (no size)")?
            .parse()
            .map_err(|_| "malformed `git cat-file --batch` size field".to_string())?;
        if cursor + size > buf.len() {
            return Err("truncated `git cat-file --batch` output (missing content)".to_string());
        }
        let content = &buf[cursor..cursor + size];
        cursor += size;
        if buf.get(cursor) == Some(&b'\n') {
            cursor += 1;
        }

        let dest = tempdir.path().join(&entry.path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create {}: {e}", parent.display()))?;
        }
        if entry.mode == "120000" {
            let target = String::from_utf8_lossy(content).into_owned();
            #[cfg(unix)]
            {
                if !symlink_target_stays_within_export(tempdir.path(), &dest, &target) {
                    return Err(format!(
                        "committed symlink {} -> {target} escapes the \
                         export root — creating it would let `cargo check` \
                         read whatever file (committed or not) happens to \
                         sit at that path elsewhere on this machine, \
                         falsely certifying a broken commit as buildable; \
                         refusing to export it",
                        entry.path
                    ));
                }
                std::os::unix::fs::symlink(&target, &dest)
                    .map_err(|e| format!("failed to create symlink {}: {e}", dest.display()))?;
            }
            #[cfg(not(unix))]
            {
                // No portable symlink primitive on this platform; write the
                // target's raw (never smudged/dereferenced) content instead
                // of failing the whole export outright.
                std::fs::write(&dest, content)
                    .map_err(|e| format!("failed to write {}: {e}", dest.display()))?;
            }
        } else {
            std::fs::write(&dest, content)
                .map_err(|e| format!("failed to write {}: {e}", dest.display()))?;
            #[cfg(unix)]
            if entry.mode == "100755" {
                use std::os::unix::fs::PermissionsExt;
                let mut perm = std::fs::metadata(&dest)
                    .map_err(|e| format!("failed to stat {}: {e}", dest.display()))?
                    .permissions();
                perm.set_mode(0o755);
                std::fs::set_permissions(&dest, perm)
                    .map_err(|e| format!("failed to chmod {}: {e}", dest.display()))?;
            }
        }
    }

    Ok(tempdir)
}

/// Test-only entry point into `export_committed_head` — lets
/// `doctor::runner`'s test module exercise the export mechanism directly
/// (adversarial smudge filters, symlinks, fabrication traps) without going
/// through the full `cargo check` pipeline.
#[cfg(test)]
pub(crate) fn export_committed_head_for_tests(
    repo_root: &Path,
) -> Result<tempfile::TempDir, String> {
    export_committed_head(repo_root)
}
