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
    run_check_impl(hex_dir, timeout, &[])
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
    run_check_impl(hex_dir, timeout, &[("PATH", path_override)])
}

#[cfg(test)]
pub(crate) fn run_check_with_timeout_and_env_override(
    hex_dir: &Path,
    timeout: Duration,
    env_overrides: &[(&str, &str)],
) -> CheckResult {
    run_check_impl(hex_dir, timeout, env_overrides)
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

fn run_check_impl(
    hex_dir: &Path,
    timeout: Duration,
    env_overrides: &[(&str, &str)],
) -> CheckResult {
    let start = Instant::now();
    // A-R-F14-1: the export below (ls-tree + cat-file, see
    // `export_committed_head`) and the `cargo check` further down share
    // this SAME deadline — `cargo check` gets whatever the export left
    // over, never a second fresh `timeout`-sized window on top of it.
    let deadline = start + timeout;

    // Export cleanup (success, failure, or panic-unwind) is handled by
    // `tempfile::TempDir`'s own `Drop` — `export` is the guard, and every
    // return path below simply lets it go out of scope.
    let export = match export_committed_head(
        hex_dir,
        deadline.saturating_duration_since(Instant::now()),
        &[],
    ) {
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
    for (key, value) in env_overrides {
        cmd.env(key, value);
    }

    // A-R-F14-1: whatever remains of the SAME deadline the export above
    // drew from — not a fresh `timeout`-sized window, which is exactly
    // the gap that let a slow-but-successful export plus a
    // slow-but-successful `cargo check` sum past the advertised cap.
    let output =
        match run_with_timeout(&mut cmd, deadline.saturating_duration_since(Instant::now())) {
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
            "cargo could not get a working rustc toolchain (unreachable, or \
             present but failing cargo's own version probe — this is not a \
             build failure) — fix: install or repair the rust toolchain: {}",
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
    lower.contains("error[")
        || stderr.contains("-->")
        || lower.contains("could not compile `")
        // A failing `build.rs` is just as much a real build failure as a
        // `rustc` diagnostic, but cargo reports it with its OWN top-level
        // marker instead — never `error[...]`/`-->` — so a build script
        // whose own panic/stderr text happens to *quote* the offline or
        // toolchain wording (this is genuinely the crate's source, already
        // fed to `rustc`/`cargo` and run) must still gate the classifiers
        // below rather than fall through to a WARN.
        || lower.contains("failed to run custom build command")
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
    if lower_stderr.contains("could not execute process `rustc")
        || lower_stderr.contains("could not execute process 'rustc")
    {
        return true;
    }
    // An explicit toolchain override (a `RUSTC` env var, or a broken rustup
    // shim) names its OWN path instead of the bare `rustc` — e.g. `` could
    // not execute process `/no/such/path -vV` (never executed) `` — so the
    // bare-name match above misses it. cargo always probes the toolchain
    // with the same `-vV` flag before it runs anything else, which
    // uniquely identifies this as a toolchain-unreachable failure (never a
    // real compile error — no source has been fed to `rustc` yet).
    if lower_stderr.contains("could not execute process `")
        && lower_stderr.contains("-vv`")
        && lower_stderr.contains("(never executed)")
    {
        return true;
    }
    // A toolchain binary that EXISTS and RUNS, but exits non-zero on that
    // same `-vV` probe (a broken rustup proxy naming a toolchain that
    // isn't installed, or any other broken `RUSTC` override pointing at a
    // real-but-wrong executable) never reaches the ENOENT wrapper above —
    // cargo instead reports its own "process didn't exit successfully"
    // wrapper around the probe command. Still gated on the `-vv\`` marker
    // so a real compile failure that happens to mention "-vV" elsewhere in
    // its own diagnostic text still falls through to FAIL.
    lower_stderr.contains("process didn't exit successfully:") && lower_stderr.contains("-vv`")
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
    run_with_timeout_and_stdin(cmd, timeout, None)
}

/// Same as `run_with_timeout`, but optionally feeds `stdin_data` to the
/// child on a background writer thread before draining stdout/stderr —
/// lets `git cat-file --batch` (which reads a stream of object ids from
/// stdin) share the exact same wall-clock cap as every other external
/// command this check runs (F14) instead of blocking unboundedly on
/// `Child::wait`/`read_to_end` the way a plain `.output()` call would.
pub(crate) fn run_with_timeout_and_stdin(
    cmd: &mut Command,
    timeout: Duration,
    stdin_data: Option<Vec<u8>>,
) -> Result<Output, String> {
    let mut child = cmd
        .stdin(if stdin_data.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn command: {e}"))?;

    if let Some(data) = stdin_data {
        let mut stdin_pipe = child.stdin.take().expect("stdin was piped");
        std::thread::spawn(move || {
            use std::io::Write;
            let _ = stdin_pipe.write_all(&data);
            // Dropping `stdin_pipe` here (end of closure) closes the pipe,
            // which is what tells a command like `git cat-file --batch`
            // there is no more input to expect.
        });
    }

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

/// A committed path or symlink target, preserving git's raw bytes exactly.
/// `git` paths and symlink-target blobs are arbitrary byte strings, not
/// necessarily valid UTF-8 — `git ls-tree` returns a tree object's
/// filenames verbatim regardless of what platform is reading them, so a
/// tree that originated on a filesystem allowing arbitrary bytes can still
/// be exported by a build running on a platform that cannot represent
/// those bytes in a real filename. This must never be a `String` produced
/// by a lossy conversion (A-R-22, round-2/round-3 review, F22):
/// `String::from_utf8_lossy` replaces an invalid byte with U+FFFD's own
/// (valid) 3-byte encoding, which can rename an exported path to something
/// a source file's `include_str!`/`mod` reference happens to match even
/// though the commit never actually contained anything there — a false
/// PASS. `OsString` has no such lossy step on unix, so it preserves every
/// byte exactly there; on a platform whose filesystem cannot represent an
/// arbitrary byte sequence (`CommittedPath = String`), silently
/// lossy-renaming would reproduce the exact same false-PASS mechanism one
/// level up — so `raw_path_from_bytes` there REFUSES instead (returns
/// `Err`), and the whole export becomes inconclusive rather than silently
/// wrong.
#[cfg(unix)]
pub(crate) type CommittedPath = std::ffi::OsString;
#[cfg(not(unix))]
pub(crate) type CommittedPath = String;

/// Converts one git object's raw path or symlink-target bytes into a
/// `CommittedPath`, preserving every byte exactly on unix, or refusing
/// (`Err`) on a platform that cannot represent them at all (see
/// `CommittedPath`'s docs for why silently lossy-renaming there would be
/// exactly as wrong as never preserving the bytes in the first place).
#[cfg(unix)]
pub(crate) fn raw_path_from_bytes(bytes: &[u8]) -> Result<CommittedPath, String> {
    use std::os::unix::ffi::OsStrExt;
    Ok(std::ffi::OsStr::from_bytes(bytes).to_os_string())
}
#[cfg(not(unix))]
pub(crate) fn raw_path_from_bytes(bytes: &[u8]) -> Result<CommittedPath, String> {
    std::str::from_utf8(bytes).map(str::to_string).map_err(|_| {
        format!(
            "a committed path or symlink target contains bytes that are \
             not valid UTF-8 ({bytes:?}) — this platform's filesystem \
             cannot represent the committed name exactly, and silently \
             substituting a lossy rendering would let the export diverge \
             from what the commit actually recorded (A-R-22); refusing \
             the export as inconclusive rather than falsely certifying a \
             renamed path as the committed one"
        )
    })
}

/// One entry from `git ls-tree -r -z HEAD`: a file mode, its blob sha, and
/// its repo-relative path.
pub(crate) struct TreeEntry {
    pub(crate) mode: String,
    pub(crate) sha: String,
    pub(crate) path: CommittedPath,
}

/// Parses one `\0`-terminated `git ls-tree -r -z --full-tree HEAD` entry
/// (`<mode> <type> <sha>\t<path>`) into a `TreeEntry`. Splits on the raw
/// TAB byte and decodes ONLY the ASCII metadata (mode/type/sha) as UTF-8 —
/// the path itself goes through `raw_path_from_bytes`, never a lossy
/// decode of the whole line (A-R-22; see `CommittedPath`'s docs). Pure and
/// filesystem-independent, so it is unit-testable without needing a
/// filesystem that actually accepts an invalid-UTF-8 byte in a real
/// filename (macOS APFS, for one, refuses to create such a file at all —
/// this is the only way to regression-test the byte-preservation
/// contract on every platform this suite runs on).
pub(crate) fn parse_ls_tree_entry(raw: &[u8]) -> Result<TreeEntry, String> {
    let tab_pos = raw.iter().position(|&b| b == b'\t').ok_or_else(|| {
        format!(
            "unexpected `git ls-tree` line: {}",
            String::from_utf8_lossy(raw)
        )
    })?;
    let (meta_bytes, rest) = raw.split_at(tab_pos);
    let path_bytes = &rest[1..];
    let meta = std::str::from_utf8(meta_bytes).map_err(|_| {
        format!(
            "malformed `git ls-tree` metadata (non-UTF-8 mode/type/sha): {}",
            String::from_utf8_lossy(meta_bytes)
        )
    })?;
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
    Ok(TreeEntry {
        mode,
        sha,
        path: raw_path_from_bytes(path_bytes)?,
    })
}

/// Resolves a symlink chain the way the real filesystem does: `stack` is
/// the export-relative directory the walk currently sits in (as path
/// segments, so popping past empty means climbing above the export root),
/// and `remaining` is the path components still to walk. Any `Normal`
/// component that names another COMMITTED symlink (looked up in
/// `symlinks`, repo-relative path -> its raw target) is substituted with
/// that target and the walk continues from the symlink's OWN parent
/// directory — never treated as an ordinary directory push — because a
/// purely lexical push-then-pop assumes every intermediate component
/// contributes exactly one real directory level, which is false the moment
/// that component is itself a symlink to `.` (or anywhere else): the real
/// filesystem resolves it FIRST, and a `..` right after can walk out from
/// wherever that resolves to rather than cancelling the push. Returns
/// `None` if the walk climbs above the export root, hits an absolute
/// target anywhere in the chain, or the chain is implausibly deep (cycle
/// guard).
fn resolve_realpath_within_export(
    mut stack: Vec<std::ffi::OsString>,
    remaining: Vec<std::path::Component>,
    symlinks: &std::collections::HashMap<std::path::PathBuf, CommittedPath>,
    depth: usize,
) -> Option<Vec<std::ffi::OsString>> {
    if depth > 40 {
        return None;
    }
    let mut queue: std::collections::VecDeque<_> = remaining.into();
    while let Some(component) = queue.pop_front() {
        match component {
            std::path::Component::ParentDir => {
                stack.pop()?;
            }
            std::path::Component::CurDir => {}
            std::path::Component::Normal(segment) => {
                let candidate: std::path::PathBuf =
                    stack.iter().collect::<std::path::PathBuf>().join(segment);
                if let Some(target) = symlinks.get(&candidate) {
                    let target_path = Path::new(target);
                    if target_path.is_absolute() {
                        return None;
                    }
                    let mut new_remaining: Vec<_> = target_path.components().collect();
                    new_remaining.extend(queue);
                    return resolve_realpath_within_export(
                        stack,
                        new_remaining,
                        symlinks,
                        depth + 1,
                    );
                }
                stack.push(segment.to_os_string());
            }
            std::path::Component::RootDir | std::path::Component::Prefix(_) => return None,
        }
    }
    Some(stack)
}

/// `true` iff a symlink recorded at `dest_rel` (export-relative path of the
/// symlink itself) with the given raw `target` (the exact bytes git stored
/// for a mode `120000` blob), resolved through the REAL filesystem
/// semantics of any other committed symlink it chains through (`symlinks`),
/// stays inside the export root. The target is never required to exist —
/// that is exactly the attack this guards against: a target naming a real
/// but UNCOMMITTED file elsewhere on this machine (e.g. back in the actual
/// checkout this export was made from). An absolute target, or a relative
/// one whose real (chain-resolved) location walks back out of the export
/// root, must never be materialized: `cargo check` would then silently
/// read content from outside the git-committed export and this check would
/// falsely certify a broken commit as buildable.
#[cfg(unix)]
fn symlink_target_stays_within_export(
    dest_rel: &Path,
    target: &std::ffi::OsStr,
    symlinks: &std::collections::HashMap<std::path::PathBuf, CommittedPath>,
) -> bool {
    let target_path = Path::new(target);
    if target_path.is_absolute() {
        return false;
    }
    let Some(start_dir) = dest_rel.parent() else {
        return false;
    };
    let stack: Vec<std::ffi::OsString> = start_dir
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(segment) => Some(segment.to_os_string()),
            _ => None,
        })
        .collect();
    resolve_realpath_within_export(stack, target_path.components().collect(), symlinks, 0).is_some()
}

/// `true` iff a path reference named by something committed at `base_dir`
/// (a manifest's own directory, or a source file's own directory) resolves
/// OUTSIDE the export root — either directly (an absolute target, or a
/// `..` climbing above every `Normal` component of `base_dir`'s own
/// ancestry) or by chaining through a COMMITTED SYMLINK along the way
/// (round-4 review: a committed `link -> .` at the export root, referenced
/// as e.g. `../../../link/../outside.txt`, resolves `link` on the REAL
/// filesystem BEFORE applying the `..` that follows it — a purely lexical
/// walk of `..`/`.` components alone, with no symlink awareness, misses
/// this entirely). Delegates to `resolve_realpath_within_export`, the
/// SAME symlink-chain-aware walker the materialized-symlink guards above
/// use — a Cargo path dependency or an `include!`-family literal is just
/// another path reference resolved against a directory already inside the
/// export, with exactly the same escape hazard (A-R-4 / F4).
fn target_escapes_export(
    base_dir: &Path,
    target: &str,
    symlinks: &std::collections::HashMap<std::path::PathBuf, CommittedPath>,
) -> bool {
    let target_path = Path::new(target);
    if target_path.is_absolute() {
        return true;
    }
    let stack: Vec<std::ffi::OsString> = base_dir
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(segment) => Some(segment.to_os_string()),
            _ => None,
        })
        .collect();
    resolve_realpath_within_export(stack, target_path.components().collect(), symlinks, 0).is_none()
}

/// A-R-4 (round-2/round-4 review, F4): a committed `Cargo.toml`'s `path`
/// dependency — in `[dependencies]`/`[dev-dependencies]`/
/// `[build-dependencies]`, the equivalent `[workspace.dependencies]`
/// table, OR (round-4 review) a per-target
/// `[target.'cfg(...)'.dependencies]` table (and its dev-/build-
/// equivalents) — is resolved by `cargo` directly against the manifest's
/// OWN directory on the real filesystem, never through this export. A path
/// that resolves outside the export root (directly, or via a committed
/// symlink hop — see `target_escapes_export`) lets `cargo check` build
/// against a package this commit never actually contains, falsely
/// certifying a broken commit as buildable the moment a package happens to
/// exist at that path on THIS machine. Malformed TOML, or a manifest this
/// walk doesn't recognize the shape of, is left alone here — `cargo`
/// itself will fail or succeed on it exactly as it always would; this is
/// an ADDITIONAL provenance guard on top of compilation, never a
/// replacement for it.
fn validate_manifest_path_dependencies_stay_within_export(
    entries: &[TreeEntry],
    contents: &[Vec<u8>],
    symlinks: &std::collections::HashMap<std::path::PathBuf, CommittedPath>,
) -> Result<(), String> {
    for (entry, content) in entries.iter().zip(contents.iter()) {
        let path = Path::new(&entry.path);
        if path.file_name().and_then(|f| f.to_str()) != Some("Cargo.toml") {
            continue;
        }
        let Ok(text) = std::str::from_utf8(content) else {
            continue;
        };
        let Ok(value) = text.parse::<toml::Value>() else {
            continue;
        };
        let manifest_dir = path.parent().unwrap_or_else(|| Path::new(""));

        let mut dependency_tables: Vec<&toml::Value> =
            ["dependencies", "dev-dependencies", "build-dependencies"]
                .into_iter()
                .filter_map(|name| value.get(name))
                .collect();
        if let Some(ws_deps) = value.get("workspace").and_then(|w| w.get("dependencies")) {
            dependency_tables.push(ws_deps);
        }
        if let Some(target_table) = value.get("target").and_then(|t| t.as_table()) {
            for target_value in target_table.values() {
                for name in ["dependencies", "dev-dependencies", "build-dependencies"] {
                    if let Some(t) = target_value.get(name) {
                        dependency_tables.push(t);
                    }
                }
            }
        }

        for table in dependency_tables {
            let Some(table) = table.as_table() else {
                continue;
            };
            for (dep_name, dep_value) in table {
                let Some(dep_path) = dep_value.get("path").and_then(|p| p.as_str()) else {
                    continue;
                };
                if target_escapes_export(manifest_dir, dep_path, symlinks) {
                    return Err(format!(
                        "committed manifest {} declares dependency `{dep_name}` with \
                         path `{dep_path}`, which resolves outside this export's root \
                         — `cargo check` would then build a package from wherever that \
                         path resolves on THIS machine, not from anything the commit \
                         actually contains, falsely certifying a broken commit as \
                         buildable; refusing to export it",
                        path.display()
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Recognizes any Rust string-literal-LIKE token beginning at `source[i]`:
/// an ordinary `"..."`, a byte string `b"..."`, a C string `c"..."`
/// (Rust 2021+), a raw string `r"..."`/`r#"..."#`/…, a raw byte string
/// `br"..."`/`br#"..."#`/…, or a raw C string `cr"..."`/`cr#"..."#`/… —
/// round-5/round-6 review, F3: the first masking draft recognized only a
/// bare `r`-prefixed raw string (missing `br…`); the next recognized `b`
/// and `r` prefixes but not `c` at all, so a `c"..."` literal's opening
/// `"` (preceded by the alphanumeric `c`) was rejected by the
/// identifier-boundary check below and left as ordinary code — the
/// literal's CLOSING `"` (preceded by a non-identifier byte) was then
/// mistaken for the OPENING of a brand-new string, and the search for
/// ITS closing `"` swallowed everything up to and including whatever
/// real macro call followed, hiding it from the search entirely. Returns
/// `(is_raw, quote_pos, hash_count)`: `quote_pos` is the index of the
/// literal's OPENING `"`, and for a raw form, `hash_count` is how many
/// `#`s its closing delimiter must match (always 0 for a non-raw form).
/// `None` if nothing at `i` matches any of these six forms, or the byte
/// immediately before `i` is itself an identifier character (so this
/// can't be misread as the tail of some longer identifier).
fn string_literal_start(source: &[u8], i: usize) -> Option<(bool, usize, usize)> {
    let n = source.len();
    if i >= n {
        return None;
    }
    if i > 0 {
        let prev = source[i - 1];
        if prev.is_ascii_alphanumeric() || prev == b'_' {
            return None;
        }
    }
    if source[i] == b'"' {
        return Some((false, i, 0));
    }
    // One-byte prefix, ordinary (non-raw) form: `b"..."` (byte string) or
    // `c"..."` (C string, Rust 2021+).
    if (source[i] == b'b' || source[i] == b'c') && i + 1 < n && source[i + 1] == b'"' {
        return Some((false, i + 1, 0));
    }
    // Raw forms: `r"..."`/`r#"..."#`/… ; `br"..."`/`br#"..."#`/… (raw byte
    // string); `cr"..."`/`cr#"..."#`/… (raw C string, Rust 2021+).
    let r_at = if source[i] == b'r' {
        i
    } else if (source[i] == b'b' || source[i] == b'c') && i + 1 < n && source[i + 1] == b'r' {
        i + 1
    } else {
        return None;
    };
    let mut j = r_at + 1;
    let mut hashes = 0usize;
    while j < n && source[j] == b'#' {
        hashes += 1;
        j += 1;
    }
    if j < n && source[j] == b'"' {
        Some((true, j, hashes))
    } else {
        None
    }
}

/// Advances past an ordinary (non-raw) string literal whose OPENING `"`
/// sits at `quote_pos`, returning the index one past its closing `"`
/// (honoring `\"`/`\\` escapes) — or `None` if unterminated (the file
/// itself will fail to compile for the same reason).
fn skip_ordinary_string_from_quote(source: &[u8], quote_pos: usize) -> Option<usize> {
    let n = source.len();
    let mut i = quote_pos + 1;
    while i < n {
        if source[i] == b'\\' && i + 1 < n {
            i += 2;
            continue;
        }
        if source[i] == b'"' {
            return Some(i + 1);
        }
        i += 1;
    }
    None
}

/// Advances past a raw (or raw byte) string literal whose OPENING `"` sits
/// at `quote_pos`, closed by `"` followed by exactly `hashes` `#`s —
/// returning the index one past that closing delimiter, or `None` if
/// unterminated. Returning `Option` rather than a `source.len()` sentinel
/// keeps "unterminated" unambiguous even when a well-formed literal
/// legitimately ends at end-of-file (no trailing newline) — a raw
/// string's closing run of `#`s could otherwise be mistaken for the
/// sentinel by any caller trying to infer termination from the returned
/// offset alone.
fn skip_raw_string_from_quote(source: &[u8], quote_pos: usize, hashes: usize) -> Option<usize> {
    let mut i = quote_pos + 1;
    loop {
        let rel = source[i..].iter().position(|&b| b == b'"')?;
        let at = i + rel;
        let closes = source
            .get(at + 1..at + 1 + hashes)
            .map(|s| s.iter().all(|&b| b == b'#'))
            .unwrap_or(false);
        if closes {
            return Some(at + 1 + hashes);
        }
        i = at + 1;
    }
}

/// Produces a byte-for-byte-same-length "masked" copy of `source` with
/// every LINE COMMENT, BLOCK COMMENT (correctly nested), and the BODY of
/// every ordinary/byte/raw/raw-byte STRING LITERAL replaced with ASCII
/// spaces. Used ONLY to decide WHERE a genuine `include!`-family call
/// sits — an argument's actual content is always extracted from the
/// UNMASKED original afterward (`extract_string_literal_argument`).
/// Closes false-positive routes a naive byte search over raw source has
/// (round-4/round-5 review, F3, reintroduced by this check's own first
/// draft, then again by its first masking fix): (a) a comment merely
/// MENTIONING `include_str!(...)` as documentation must not be treated as
/// a real call; (b) the same mechanism applies to an unrelated string
/// literal (ordinary OR byte OR raw OR raw-byte) that happens to contain
/// that text. This is a pure LEXICAL pass — comment/string BOUNDARY
/// recognition, not a parser and not a semantic model of what the code
/// does; it never evaluates `concat!`/`env!`/any other non-literal path
/// construction (see `validate_source_include_targets_stay_within_export`'s
/// doc comment for the still-accepted, deliberate gaps).
fn mask_comments_and_strings_for_search(source: &[u8]) -> Vec<u8> {
    let mut out = source.to_vec();
    let n = source.len();
    let mut i = 0usize;
    while i < n {
        if source[i] == b'/' && i + 1 < n && source[i + 1] == b'/' {
            let start = i;
            while i < n && source[i] != b'\n' {
                i += 1;
            }
            out[start..i].fill(b' ');
        } else if source[i] == b'/' && i + 1 < n && source[i + 1] == b'*' {
            let start = i;
            i += 2;
            let mut depth = 1usize;
            while i < n && depth > 0 {
                if i + 1 < n && source[i] == b'/' && source[i + 1] == b'*' {
                    depth += 1;
                    i += 2;
                } else if i + 1 < n && source[i] == b'*' && source[i + 1] == b'/' {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            out[start..i].fill(b' ');
        } else if let Some((is_raw, quote_pos, hashes)) = string_literal_start(source, i) {
            let start = i;
            i = if is_raw {
                skip_raw_string_from_quote(source, quote_pos, hashes)
            } else {
                skip_ordinary_string_from_quote(source, quote_pos)
            }
            .unwrap_or(n); // unterminated: mask to EOF; the file will fail to compile for the same reason
            out[start..i].fill(b' ');
        } else {
            i += 1;
        }
    }
    out
}

/// Parses a string-literal-or-raw-string-literal argument beginning at
/// `start` in `content` (the UNMASKED original — see
/// `mask_comments_and_strings_for_search`'s docs for why extraction always
/// reads from there, never from the masked copy) and returns its content
/// bytes — verbatim for a raw string; for an ordinary string, `\"`→`"` and
/// `\\`→`\` are undone and every OTHER escape sequence is left as literal
/// text (enough to correctly see a path's `.`/`..`/`/` structure without a
/// full escape-sequence implementature) — together with the offset one
/// past the literal's closing delimiter. Deliberately does NOT accept a
/// byte-string prefix (`b"..."`/`br"..."`): none of `include_str!`/
/// `include_bytes!`/`include!` accept one as their path argument — only
/// `string_literal_start`'s BYTE-STRING recognition matters here, and only
/// for correctly skipping one elsewhere in the file during masking.
/// `None` if `start` begins neither literal form, or it is unterminated.
fn extract_string_literal_argument(content: &[u8], start: usize) -> Option<(Vec<u8>, usize)> {
    // Reject a byte-string (`b"..."`/`br"..."`) or C-string
    // (`c"..."`/`cr"..."`) prefix by its FIRST byte, not by comparing
    // `quote_pos` to `start`: `string_literal_start` also advances
    // `quote_pos` past `start` for a bare `r"..."` raw string (no prefix
    // byte involved at all), so that comparison alone can't tell the two
    // apart — an earlier draft of this check used it and rejected every
    // legitimate raw string as a result. Neither a `&[u8]` nor a
    // `&core::ffi::CStr` is valid syntax for `include_str!`'s/
    // `include_bytes!`'s/`include!`'s path argument (a plain `&str`
    // shape only) — only `string_literal_start`'s recognition of these
    // prefix forms matters here, and only for correctly skipping one
    // elsewhere in the file during masking.
    if matches!(content.get(start), Some(&b'b') | Some(&b'c')) {
        return None;
    }
    let (is_raw, quote_pos, hashes) = string_literal_start(content, start)?;
    if is_raw {
        let end = skip_raw_string_from_quote(content, quote_pos, hashes)?;
        return Some((content[quote_pos + 1..end - 1 - hashes].to_vec(), end));
    }
    let end = skip_ordinary_string_from_quote(content, quote_pos)?;
    let mut out = Vec::new();
    let mut i = quote_pos + 1;
    while i < end - 1 {
        if content[i] == b'\\' && i + 1 < end - 1 {
            match content[i + 1] {
                b'"' => out.push(b'"'),
                b'\\' => out.push(b'\\'),
                other => {
                    out.push(b'\\');
                    out.push(other);
                }
            }
            i += 2;
        } else {
            out.push(content[i]);
            i += 1;
        }
    }
    Some((out, end))
}

/// A-R-4 (round-6 review, F4): resolves `concat!(<lit>, <lit>, …)` to the
/// concatenation of its arguments' bytes, IF every argument is itself a
/// plain literal `extract_string_literal_argument` already recognizes,
/// separated by commas (an optional trailing comma before `)` is
/// accepted, same as the outer `include!`-family call). Returns the
/// concatenated bytes and the offset one past `concat!`'s closing `)`.
///
/// `None` the moment anything inside the parens ISN'T a literal this
/// module already knows how to read — an identifier, a number, `env!`,
/// a NESTED `concat!`, any other expression. That is a deliberate
/// stopping point, not an oversight: an `env!`-derived argument reads
/// the environment, not git, so no lexical check could EVER validate it
/// against export containment regardless of effort — repository content
/// is simply not where its value comes from. Extending this to a nested
/// `concat!` or an arbitrary constant expression would mean evaluating a
/// general Rust constant expression, which is the exact unbounded
/// modeling problem this whole redesign exists to avoid (see the
/// module-level doc comment on `HarnessBuildableFromGit`). This closes
/// the common, BOUNDED case named by the review — a `concat!` of nothing
/// but plain literals — while leaving genuinely dynamic construction out
/// of scope by construction, not merely by choice.
fn try_resolve_concat_literal(content: &[u8], start: usize) -> Option<(Vec<u8>, usize)> {
    const CONCAT: &[u8] = b"concat!";
    if start > 0 {
        let prev = content[start - 1];
        if prev.is_ascii_alphanumeric() || prev == b'_' {
            return None;
        }
    }
    if !content.get(start..)?.starts_with(CONCAT) {
        return None;
    }
    let mut i = start + CONCAT.len();
    while i < content.len() && content[i].is_ascii_whitespace() {
        i += 1;
    }
    if content.get(i) != Some(&b'(') {
        return None;
    }
    i += 1;
    let mut out = Vec::new();
    loop {
        while i < content.len() && content[i].is_ascii_whitespace() {
            i += 1;
        }
        if content.get(i) == Some(&b')') {
            return Some((out, i + 1));
        }
        let (piece, after) = extract_string_literal_argument(content, i)?;
        out.extend_from_slice(&piece);
        i = after;
        while i < content.len() && content[i].is_ascii_whitespace() {
            i += 1;
        }
        match content.get(i) {
            Some(&b',') => i += 1,
            Some(&b')') => return Some((out, i + 1)),
            _ => return None, // not an all-literal concat! call — out of scope
        }
    }
}

/// A-R-4 (round-7 review, F4): recognizes
/// `concat!(env!("OUT_DIR"), <literal>, <literal>, …)` or
/// `concat!(env!("CARGO_MANIFEST_DIR"), <literal>, …)` — the idiomatic
/// Rust pattern for including a build-script-generated file, and the
/// EXACT shape all three genuine computed includes in this very
/// workspace use (`main.rs`, `workers/mod.rs`, `integration.rs`).
///
/// Cargo GUARANTEES both of these two specific variables' values (never
/// any other env var): `CARGO_MANIFEST_DIR` is always the crate's own
/// manifest directory — already inside this export by construction,
/// since `cargo check` runs against the EXPORTED harness directory (see
/// `run_check_impl`). `OUT_DIR` is always THIS crate's own build-script
/// output directory for the CURRENT build, populated by running the
/// crate's own (also-committed) `build.rs` against this same export —
/// never a pre-existing external file coincidentally present on the
/// machine, which is the entire hazard F4 guards against. A path built
/// from either variable is therefore verified-safe BY CONSTRUCTION, not
/// merely unverifiable: this is a bounded, named exception to F4's
/// unresolved-argument refusal in
/// `validate_source_include_targets_stay_within_export`, not a general
/// `env!()` allowance — any OTHER environment variable's value genuinely
/// IS arbitrary external state and is NOT recognized here (round-6
/// review already rejected general `env!()` support for exactly that
/// reason).
///
/// Returns the offset one past the call's closing `)` on a match. Every
/// literal SUFFIX argument is validated by `suffix_has_no_parent_dir_segment`
/// (round-8 review: an EARLIER version of this function left the suffix
/// wholly unchecked, on the reasoning that reproducing Cargo's exact
/// `OUT_DIR`/manifest-dir layout here is out of scope — true, but
/// irrelevant: a suffix containing NO `..` segment can only ever land
/// INSIDE whatever directory it is appended to, regardless of where that
/// directory actually is or how it's laid out, so this needs no
/// knowledge of Cargo's layout at all to be a real guarantee). A suffix
/// containing a `..` segment — the review's own adversarial example,
/// `concat!(env!("CARGO_MANIFEST_DIR"), "/../../../outside.txt")` — is
/// refused (this function returns `None`, and the caller's "unresolved"
/// refusal fires) rather than accepted unchecked.
fn try_resolve_cargo_safe_concat(content: &[u8], start: usize) -> Option<usize> {
    const CONCAT: &[u8] = b"concat!";
    const ENV: &[u8] = b"env!";
    if start > 0 {
        let prev = content[start - 1];
        if prev.is_ascii_alphanumeric() || prev == b'_' {
            return None;
        }
    }
    if !content.get(start..)?.starts_with(CONCAT) {
        return None;
    }
    let mut i = start + CONCAT.len();
    while i < content.len() && content[i].is_ascii_whitespace() {
        i += 1;
    }
    if content.get(i) != Some(&b'(') {
        return None;
    }
    i += 1;
    while i < content.len() && content[i].is_ascii_whitespace() {
        i += 1;
    }
    if !content.get(i..)?.starts_with(ENV) {
        return None;
    }
    i += ENV.len();
    while i < content.len() && content[i].is_ascii_whitespace() {
        i += 1;
    }
    if content.get(i) != Some(&b'(') {
        return None;
    }
    i += 1;
    while i < content.len() && content[i].is_ascii_whitespace() {
        i += 1;
    }
    let (var_name, after_var) = extract_string_literal_argument(content, i)?;
    if !matches!(var_name.as_slice(), b"OUT_DIR" | b"CARGO_MANIFEST_DIR") {
        return None;
    }
    i = after_var;
    while i < content.len() && content[i].is_ascii_whitespace() {
        i += 1;
    }
    if content.get(i) != Some(&b')') {
        return None; // env!(...) itself malformed or has a second argument
    }
    i += 1; // past env!(...)'s own closing paren
    loop {
        while i < content.len() && content[i].is_ascii_whitespace() {
            i += 1;
        }
        match content.get(i) {
            Some(&b')') => return Some(i + 1),
            Some(&b',') => {
                i += 1;
                while i < content.len() && content[i].is_ascii_whitespace() {
                    i += 1;
                }
                if content.get(i) == Some(&b')') {
                    return Some(i + 1); // trailing comma before concat!'s own close
                }
                let (suffix, after) = extract_string_literal_argument(content, i)?;
                if !suffix_has_no_parent_dir_segment(&suffix) {
                    return None;
                }
                i = after;
            }
            _ => return None,
        }
    }
}

/// `true` iff `suffix`, split on `/`, contains no `..` segment — i.e. it
/// can only ever be appended to a directory, never climb above it,
/// regardless of what that directory's actual location is (round-8
/// review; see `try_resolve_cargo_safe_concat`'s doc comment). Splits the
/// raw bytes directly rather than going through `std::path::Path`: a
/// leading `/` in a suffix like `"/personal_mods.rs"` is the ordinary,
/// idiomatic separator between `env!("OUT_DIR")` and the filename once
/// concatenated — NOT an attempt to jump to the filesystem root — but
/// `Path::new` would parse that same string IN ISOLATION as beginning
/// with `Component::RootDir`, a false positive `Path`-based component
/// analysis would need extra handling to avoid; a raw `/`-split sidesteps
/// the ambiguity entirely by only ever caring about literal `..`
/// segments, never about a leading separator. Non-UTF-8 bytes are
/// rejected outright (`false`) — the same posture as everywhere else in
/// this module's literal handling: an ambiguous byte sequence is treated
/// as unresolved, not as safe.
fn suffix_has_no_parent_dir_segment(suffix: &[u8]) -> bool {
    let Ok(s) = std::str::from_utf8(suffix) else {
        return false;
    };
    s.split('/').all(|segment| segment != "..")
}

/// A-R-4 (round-2/round-4/round-6 review, F4): an `include_str!`/
/// `include_bytes!`/`include!` literal in a committed `.rs` file is
/// resolved by `rustc` directly against the SOURCE file's own directory
/// on the real filesystem — never through this export. Same hazard and
/// same remedy as the manifest path-dependency guard above.
///
/// Searches a MASKED copy of the source (`mask_comments_and_strings_for_search`)
/// for the macro name, so a mention inside a comment or an unrelated
/// string literal is never mistaken for a real call (F3, reintroduced by
/// this check's own first draft and fixed here). Once a genuine call site
/// is found — the macro name, then only whitespace, then `(`, then only
/// whitespace — the argument is resolved from the UNMASKED original as
/// one of: an ordinary/byte-string-excluded/C-string-excluded/raw literal
/// (`extract_string_literal_argument`); an all-literal `concat!(...)`
/// call (`try_resolve_concat_literal`, round-6 review); or the one named,
/// bounded exception to "all-literal" —
/// `concat!(env!("OUT_DIR"|"CARGO_MANIFEST_DIR"), <literal>, …)`
/// (`try_resolve_cargo_safe_concat`, round-7 review) — and the call must
/// close with only whitespace, an optional trailing comma, then `)`.
///
/// ROUND-7 REVIEW CHANGED THE FAILURE MODE: an argument that resolves to
/// NONE of the above is no longer silently skipped. Compilation
/// succeeding on such a call would not establish that its actual input
/// came from this export — an `env!()`-derived (any OTHER variable), a
/// `concat!` mixing in a non-literal, an identifier, or any other
/// non-literal expression could all name a pre-existing, uncommitted,
/// merely-coincidental file on this one machine, exactly the false-PASS
/// hazard F4 names. This guard now refuses the export outright (the
/// caller's existing "could not export" WARN path) rather than silently
/// certifying unverifiable input as buildable — per the review's own
/// stated remedy: "an inconclusive result when provenance cannot be
/// established."
///
/// This remains a BOUNDED check, not an attempt to re-build the unbounded
/// mod/include-graph model this whole redesign replaced: it recognizes
/// exactly the shapes named above and, crucially, does NOT attempt to
/// evaluate an arbitrary Rust constant expression (that IS the same
/// unbounded modeling problem one level down). `env!("OUT_DIR")` and
/// `env!("CARGO_MANIFEST_DIR")` are the only two environment variables
/// recognized, and only because Cargo itself guarantees both are tied
/// to THIS build/THIS crate's own directory, never arbitrary external
/// state (see `try_resolve_cargo_safe_concat`'s doc comment) — this is
/// exactly the pattern all three genuine computed includes in this
/// workspace already use (`main.rs`, `workers/mod.rs`,
/// `integration.rs`); the earlier, unconditional "any unresolved
/// argument is fine" posture would have made THIS check permanently
/// unable to certify PASS on the very repository it protects, the same
/// A-R-1 anti-pattern this whole check has already learned to avoid
/// once.
fn validate_source_include_targets_stay_within_export(
    entries: &[TreeEntry],
    contents: &[Vec<u8>],
    symlinks: &std::collections::HashMap<std::path::PathBuf, CommittedPath>,
) -> Result<(), String> {
    const INCLUDE_MACROS: [&[u8]; 3] = [b"include_str!", b"include_bytes!", b"include!"];

    // Byte-slice search, never `&str` indexing: `clippy::string_slice` flags
    // any `&str` byte-range slice as a potential UTF-8-boundary panic (even
    // though every offset here comes from a `find` on the SAME string and
    // is therefore always on a boundary). Operating on `&[u8]` throughout
    // sidesteps that entirely — no boundary to violate.
    fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        if needle.is_empty() || haystack.len() < needle.len() {
            return None;
        }
        haystack.windows(needle.len()).position(|w| w == needle)
    }

    for (entry, content) in entries.iter().zip(contents.iter()) {
        let path = Path::new(&entry.path);
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let dir = path.parent().unwrap_or_else(|| Path::new(""));
        let masked = mask_comments_and_strings_for_search(content);

        for macro_name in INCLUDE_MACROS {
            let mut search_from = 0usize;
            while let Some(rel) = find_bytes(&masked[search_from..], macro_name) {
                let call_start = search_from + rel + macro_name.len();
                // Advance past this occurrence regardless of what follows,
                // so a call that doesn't match the recognized shape can't
                // loop forever re-finding the same macro name.
                search_from = call_start;

                // From here on, parse against the UNMASKED `content`, not
                // `masked` — `masked` blanks out an ENTIRE string literal
                // INCLUDING its delimiting quotes (needed so a `//`/`/*`
                // inside a string body doesn't get mistaken for a comment
                // start during masking itself), so continuing to walk
                // `masked` past `(` would skip straight through the very
                // literal this call is looking for and never see it.
                // `masked` has already done its ONE job — confirming the
                // macro NAME occurrence above is real code, not text
                // inside a comment or an unrelated string — by the time
                // control reaches here.
                let mut i = call_start;
                while i < content.len() && content[i].is_ascii_whitespace() {
                    i += 1;
                }
                if i >= content.len() || content[i] != b'(' {
                    continue;
                }
                i += 1;
                while i < content.len() && content[i].is_ascii_whitespace() {
                    i += 1;
                }
                enum ResolvedArgument {
                    Literal(Vec<u8>),
                    CargoBuildTree,
                }

                let resolved = extract_string_literal_argument(content, i)
                    .map(|(bytes, after)| (ResolvedArgument::Literal(bytes), after))
                    .or_else(|| {
                        try_resolve_concat_literal(content, i)
                            .map(|(bytes, after)| (ResolvedArgument::Literal(bytes), after))
                    })
                    .or_else(|| {
                        try_resolve_cargo_safe_concat(content, i)
                            .map(|after| (ResolvedArgument::CargoBuildTree, after))
                    });

                // Only a resolution followed by (optional trailing comma,
                // then) the call's OWN closing `)` counts — anything else
                // trailing (a second real argument, unexpected tokens)
                // means this isn't the single-bare-argument shape this
                // guard understands either, and falls into the same
                // "unresolved" refusal below.
                let resolved = resolved.and_then(|(outcome, after)| {
                    let mut j = after;
                    while j < content.len() && content[j].is_ascii_whitespace() {
                        j += 1;
                    }
                    if j < content.len() && content[j] == b',' {
                        // A trailing comma after the sole argument is
                        // valid Rust for a macro call (round-5 review,
                        // F4: `include_str!("...",)`).
                        j += 1;
                        while j < content.len() && content[j].is_ascii_whitespace() {
                            j += 1;
                        }
                    }
                    (j < content.len() && content[j] == b')').then_some(outcome)
                });

                let Some(outcome) = resolved else {
                    let macro_name = String::from_utf8_lossy(macro_name);
                    return Err(format!(
                        "committed source {} calls {macro_name}(...) with an \
                         argument this guard cannot resolve to a literal path, \
                         an all-literal concat!(...), or a recognized cargo \
                         build-tree reference (env!(\"OUT_DIR\")/ \
                         env!(\"CARGO_MANIFEST_DIR\")) — its actual input \
                         cannot be verified to come from this export, so \
                         `cargo check` succeeding on it would not establish \
                         that the commit actually contains what it reads; \
                         refusing to export it rather than falsely certifying \
                         unverified input as buildable",
                        path.display()
                    ));
                };

                let literal_bytes = match outcome {
                    ResolvedArgument::Literal(bytes) => bytes,
                    // Verified safe by construction — Cargo's own build
                    // tree (OUT_DIR) or manifest directory, never external
                    // machine state; see `try_resolve_cargo_safe_concat`.
                    ResolvedArgument::CargoBuildTree => continue,
                };
                // A non-UTF-8 literal can't name a valid Rust path string in
                // the first place; compilation itself will reject the
                // source, so there's nothing for this guard to add here.
                let Ok(literal) = std::str::from_utf8(&literal_bytes) else {
                    continue;
                };
                if target_escapes_export(dir, literal, symlinks) {
                    let macro_name = String::from_utf8_lossy(macro_name);
                    return Err(format!(
                        "committed source {} calls {macro_name}(\"{literal}\"), which \
                         resolves outside this export's root — `cargo check` would then \
                         read a file from wherever that path resolves on THIS machine, \
                         not from anything the commit actually contains, falsely \
                         certifying a broken commit as buildable; refusing to export it",
                        path.display()
                    ));
                }
            }
        }
    }
    Ok(())
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
fn export_committed_head(
    repo_root: &Path,
    timeout: Duration,
    env_overrides: &[(&str, &str)],
) -> Result<tempfile::TempDir, String> {
    // A-R-F14-1: ls-tree, cat-file --batch below share ONE wall-clock
    // budget instead of each independently re-arming a fresh copy of
    // `timeout` — three commands that are each individually fast enough
    // to finish under the cap could otherwise sum to multiples of the
    // advertised cap before anything is ever killed. Every call below
    // uses whatever this deadline has left, never `timeout` itself again.
    let deadline = Instant::now() + timeout;

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
    for (key, value) in env_overrides {
        ls_tree.env(key, value);
    }
    // F14 / A-R-F14-1: bound by whatever remains of the shared deadline
    // above, not a fresh `timeout`-sized window — a wedged `git` (lock
    // contention, a hung globally configured credential helper) must
    // never hang this health check any more than a wedged `cargo` can,
    // and a merely-slow-but-successful `git` must not be able to eat into
    // `cat-file --batch`'s (and, via `run_check_impl`, `cargo check`'s)
    // own share of the SAME advertised cap.
    let ls_output = run_with_timeout(
        &mut ls_tree,
        deadline.saturating_duration_since(Instant::now()),
    )
    .map_err(|e| format!("git ls-tree -r HEAD failed to run: {e}"))?;
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
        let entry = parse_ls_tree_entry(raw)?;
        // Gitlinks (submodule references, mode 160000) name a commit in
        // another repository, not a blob this export can materialize —
        // `git cat-file --batch` against THIS repo's own object database
        // can never read it, submodule "initialized" or not.
        //
        // A-R-1 (arrra/hex PR #7, workflow wf_c16ed20d-1bb final round):
        // F11's original fix aborted the WHOLE export the moment ANY
        // gitlink appeared ANYWHERE in the committed tree — not just under
        // this crate's own dependency closure. On the real repository this
        // check exists to protect (`~/hex`), an accidentally nested repo
        // wholly unrelated to `.hex/harness` (`.hex/.upgrade-cache`, no
        // `.gitmodules`, confirmed live) made the check permanently WARN,
        // never able to certify PASS or FAIL at all — worse in practice
        // than the pre-redesign scanner, which walked only source files a
        // crate actually references. Skip materializing a gitlink's path
        // instead: nothing this export could put there would be real
        // content anyway, so this is no different from any other file the
        // build never reads. An unreferenced gitlink then PASSes (correct:
        // the crate builds without it); a gitlink the build DOES read
        // surfaces as a normal FAIL by compilation, naming the missing
        // path by construction (rustc's own "file not found" / "unresolved
        // module" error) — the exact mechanism this whole redesign already
        // relies on for every other missing-from-git case. Note the
        // skipped path so an operator reading stderr sees why, without
        // gating PASS/FAIL/WARN on it.
        if entry.mode == "160000" {
            eprintln!(
                "[doctor] harness-buildable: skipping uninitialized gitlink at \
                 {} (a submodule reference, not content this export can \
                 materialize) — the build only fails on this if it actually \
                 reads that path",
                Path::new(&entry.path).display()
            );
            continue;
        }
        entries.push(entry);
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
        .env("GIT_NO_REPLACE_OBJECTS", "1");
    for (key, value) in env_overrides {
        batch_cmd.env(key, value);
    }
    let shas: String = entries.iter().map(|e| format!("{}\n", e.sha)).collect();
    // F14 / A-R-F14-1: whatever remains of the SAME shared deadline
    // `ls-tree` above drew from — never a fresh `timeout`-sized window —
    // via the stdin-capable variant (`run_with_timeout` alone can't feed
    // this command the object-id stream it reads from stdin).
    let batch_output = run_with_timeout_and_stdin(
        &mut batch_cmd,
        deadline.saturating_duration_since(Instant::now()),
        Some(shas.into_bytes()),
    )
    .map_err(|e| format!("git cat-file --batch failed to run: {e}"))?;
    if !batch_output.status.success() {
        return Err(format!(
            "git cat-file --batch exited with failure: {}",
            String::from_utf8_lossy(&batch_output.stderr).trim()
        ));
    }
    let buf = batch_output.stdout;

    // Parsed as a full pass BEFORE any file is written — a symlink's target
    // may name a path that appears LATER in `entries` (git ls-tree order is
    // lexical, not dependency order), so the escape check below needs every
    // OTHER committed symlink's target known up front rather than only the
    // ones already materialized.
    let mut contents: Vec<Vec<u8>> = Vec::with_capacity(entries.len());
    let mut cursor = 0usize;
    for _entry in &entries {
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
        contents.push(content.to_vec());
    }

    // Built BEFORE the F4 provenance guards below (moved up from its
    // original position just after symlink materialization) so those
    // guards can resolve a manifest path-dependency or an `include!`
    // target through a committed symlink hop the same symlink-aware way
    // `resolve_realpath_within_export` already does for a MATERIALIZED
    // symlink's own target (round-4 review). No longer `#[cfg(unix)]`:
    // this is pure path arithmetic over `CommittedPath`/`OsString`/
    // `Component`, none of it unix-specific — only actually CREATING a
    // real filesystem symlink (pass 2, below) is platform-gated.
    let symlinks: std::collections::HashMap<std::path::PathBuf, CommittedPath> = entries
        .iter()
        .zip(contents.iter())
        .filter(|(entry, _)| entry.mode == "120000")
        .map(|(entry, content)| {
            Ok((
                std::path::PathBuf::from(&entry.path),
                // A-R-22 (F22): the symlink's TARGET is committed blob
                // content, exactly as arbitrary-byte as a path — never
                // lossy-decode it either, for the same reason.
                raw_path_from_bytes(content)?,
            ))
        })
        .collect::<Result<std::collections::HashMap<_, _>, String>>()?;

    // A-R-4 (round-2/round-4 review, F4): compilation alone establishes
    // that `rustc`/`cargo` COULD read every input they touched — not that
    // every input came from this export (i.e. from something the commit
    // actually contains). An absolute `include_str!`/`include_bytes!`/
    // `include!` literal, or a Cargo path dependency that resolves outside
    // the export root (directly, or by chaining through a committed
    // symlink), is read straight off the REAL filesystem by
    // `rustc`/`cargo`, bypassing this export entirely — if a file happens
    // to exist at that path on THIS machine (even coincidentally), the
    // check certifies PASS for content the commit never actually
    // contains. Refuse before ever materializing or compiling anything,
    // the same way an escaping committed symlink is refused below.
    validate_manifest_path_dependencies_stay_within_export(&entries, &contents, &symlinks)?;
    validate_source_include_targets_stay_within_export(&entries, &contents, &symlinks)?;

    // `git ls-tree` guarantees every entry's path is a distinct byte
    // string, but on a case- or Unicode-normalization-insensitive
    // filesystem (macOS APFS by default) two distinct committed paths can
    // fold onto the SAME directory entry. If an earlier entry already
    // materialized something at that real location (most dangerously: a
    // symlink), writing or linking "through" it here would silently mix
    // one committed path's bytes into another's — `std::fs::write` follows
    // an existing symlink (O_TRUNC) rather than replacing it.
    // `symlink_metadata` never follows the final component, so it reports
    // the collision itself rather than whatever a followed symlink points
    // at.
    // Creates every directory a committed entry's path needs, one
    // component at a time, checking the REAL on-disk entry at each level
    // with `symlink_metadata` (never `metadata`, which follows the final
    // component, and never `create_dir_all`, which silently traverses
    // whatever already sits at an intermediate component) before creating
    // anything past it. This is what stops a directory needed by one
    // committed path from ever being created THROUGH a symlink already
    // materialized for a DIFFERENT committed path: on a case- or
    // Unicode-normalization-insensitive filesystem (macOS APFS by
    // default), a later entry's parent directory can fold onto an
    // earlier entry's symlink name even though the two differ in
    // spelling — `create_dir_all` would follow that symlink to wherever
    // ITS target resolves and create the remaining components there,
    // landing a real directory (and, for a symlink entry, the symlink
    // itself) OUTSIDE the export root before any batched check ever ran
    // (workflow ledger, final round: pass 2's `create_dir_all` did
    // exactly this). Walking one level at a time and refusing the moment
    // a non-directory sits where a directory is needed closes that off:
    // nothing below can ever hand `create_dir`/`symlink` a path whose
    // ancestors are anything other than directories already verified, by
    // this same walk, to be real and inside the export root.
    let ensure_dir_within_export = |rel_dir: &Path| -> Result<(), String> {
        let mut current = tempdir.path().to_path_buf();
        for component in rel_dir.components() {
            let std::path::Component::Normal(segment) = component else {
                continue;
            };
            current.push(segment);
            match std::fs::symlink_metadata(&current) {
                Ok(meta) if meta.is_dir() => {}
                Ok(_) => {
                    return Err(format!(
                        "a directory needed by a committed path collides, \
                         on this filesystem, with a different committed \
                         path's entry already materialized at {} (a case- \
                         or Unicode-normalization-insensitive filesystem \
                         folding two distinct committed names onto one \
                         directory entry) — creating anything through it \
                         could land real files or symlinks outside the \
                         export root; refusing to export it",
                        current.display()
                    ));
                }
                Err(_) => {
                    std::fs::create_dir(&current)
                        .map_err(|e| format!("failed to create {}: {e}", current.display()))?;
                }
            }
        }
        Ok(())
    };

    let refuse_if_dest_collides = |dest: &Path, entry_path: &Path| -> Result<(), String> {
        if std::fs::symlink_metadata(dest).is_ok() {
            return Err(format!(
                "committed path {} collides, on this filesystem, \
                 with a different committed path already materialized at \
                 the same location (a case- or Unicode-normalization- \
                 insensitive filesystem folding two distinct committed \
                 names onto one directory entry) — writing or linking \
                 through it would silently mix one committed path's bytes \
                 into another's, which could falsely certify a broken \
                 commit as buildable; refusing to export it",
                entry_path.display()
            ));
        }
        Ok(())
    };

    // PASS 1: materialize every regular (non-symlink) entry FIRST, in
    // `git ls-tree` order — before ANY symlink is created. `fs::write`
    // itself still follows an existing symlink at the LEAF (O_TRUNC
    // through it) — that is what `refuse_if_dest_collides` below guards
    // against — but `ensure_dir_within_export` above rules out the same
    // hazard for every INTERMEDIATE path component, on both the real and
    // (round-4 review, A-R1/B-R1) the case-/Unicode-normalization-folded
    // filesystem alike: it refuses the moment a non-directory sits where
    // a directory is needed, rather than traversing through it the way
    // `create_dir_all` would. With no symlink materialized yet in pass 1,
    // this can only ever find real directories or nothing at all.
    for (entry, content) in entries.iter().zip(contents.iter()) {
        if entry.mode == "120000" {
            continue;
        }
        let dest = tempdir.path().join(&entry.path);
        if let Some(parent) = Path::new(&entry.path).parent() {
            ensure_dir_within_export(parent)?;
        }
        refuse_if_dest_collides(&dest, Path::new(&entry.path))?;
        std::fs::write(&dest, content.as_slice())
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

    // PASS 2: materialize every committed symlink. `ensure_dir_within_export`
    // rules out the mkdir-follows-an-earlier-symlink hazard the same way
    // it does in pass 1 — an entry whose parent directory case-folds onto
    // a symlink pass 2 already created this same pass (workflow ledger,
    // final round: this is exactly how `create_dir_all` used to land a
    // real directory and a real symlink OUTSIDE the export root, before
    // `verify_symlinks_resolve_within_export` below ever ran) now refuses
    // at that intermediate component instead of traversing through it.
    // `refuse_if_dest_collides` still guards the LEAF the same way it does
    // for a regular file. `symlink()` itself only ever creates a link
    // entry — it never copies bytes through wherever the target resolves
    // — so with both the directory chain and the leaf guarded, nothing
    // below this point can place any filesystem entry outside the export
    // root; a symlink whose TARGET (not its own destination) resolves
    // outside the root can still be materialized here (its chain may
    // depend on another committed symlink this loop has not reached yet —
    // `git ls-tree` order is lexical, not dependency order), which is
    // exactly why `symlink_target_stays_within_export` below still gates
    // the `symlink()` call itself, and `verify_symlinks_resolve_within_export`
    // still runs once more after this whole pass as a second, real-
    // filesystem-canonicalizing check.
    for (entry, content) in entries.iter().zip(contents.iter()) {
        if entry.mode != "120000" {
            continue;
        }
        let dest = tempdir.path().join(&entry.path);
        if let Some(parent) = Path::new(&entry.path).parent() {
            ensure_dir_within_export(parent)?;
        }
        refuse_if_dest_collides(&dest, Path::new(&entry.path))?;
        #[cfg(unix)]
        {
            // A-R-22 (F22): the target is committed blob content, exactly
            // as arbitrary-byte as a path — preserve it raw, never a lossy
            // UTF-8 decode (see `CommittedPath`'s docs).
            let target = raw_path_from_bytes(content)?;
            if !symlink_target_stays_within_export(Path::new(&entry.path), &target, &symlinks) {
                return Err(format!(
                    "committed symlink {} -> {} escapes the export \
                     root — creating it would let `cargo check` read \
                     whatever file (committed or not) happens to sit at \
                     that path elsewhere on this machine, falsely \
                     certifying a broken commit as buildable; refusing to \
                     export it",
                    Path::new(&entry.path).display(),
                    Path::new(&target).display()
                ));
            }
            std::os::unix::fs::symlink(&target, &dest)
                .map_err(|e| format!("failed to create symlink {}: {e}", dest.display()))?;
        }
        #[cfg(not(unix))]
        {
            // No portable symlink primitive on this platform; write the
            // target's raw (never smudged/dereferenced) content instead of
            // failing the whole export outright.
            std::fs::write(&dest, content.as_slice())
                .map_err(|e| format!("failed to write {}: {e}", dest.display()))?;
        }
    }

    #[cfg(unix)]
    verify_symlinks_resolve_within_export(tempdir.path(), &entries)?;

    Ok(tempdir)
}

/// Second, independent guard on top of `symlink_target_stays_within_export`'s
/// lexical walk: asks the REAL filesystem to resolve every committed
/// symlink (`std::fs::canonicalize` follows the full chain exactly the way
/// `cargo`/`rustc` will) and refuses the export if any of them lands
/// outside the export root. The lexical walk substitutes a chained hop only
/// when a path component matches a committed symlink's path by EXACT byte
/// equality, which a case- or Unicode-normalization-insensitive filesystem
/// (macOS APFS by default) can bypass: a reference that differs only by
/// case or normalization from a committed symlink's name still resolves,
/// on the real filesystem, to that same symlink, even though the lexical
/// walk never substitutes it and so never flags the escape. Runs after
/// every symlink in pass 2 has been created — never after pass 1's
/// regular-file writes too, which is what let a round-4-review finding
/// (B-R1) observe a real file's bytes already outside the export root by
/// the time this ran; with symlinks materialized last and no further
/// `fs::write` of real content after this point, that ordering hazard is
/// gone regardless of exactly where within pass 2 this check sits. A
/// symlink that is simply dangling (its target committed nowhere) fails
/// to canonicalize with `NotFound` — that is not an escape, just a broken
/// link `cargo check` will itself fail to read, so it is left alone here.
#[cfg(unix)]
fn verify_symlinks_resolve_within_export(
    export_root: &Path,
    entries: &[TreeEntry],
) -> Result<(), String> {
    let canonical_root = std::fs::canonicalize(export_root).map_err(|e| {
        format!(
            "failed to canonicalize export root {}: {e}",
            export_root.display()
        )
    })?;
    for entry in entries {
        if entry.mode != "120000" {
            continue;
        }
        let dest = export_root.join(&entry.path);
        let resolved = match std::fs::canonicalize(&dest) {
            Ok(r) => r,
            Err(_) => continue,
        };
        if !resolved.starts_with(&canonical_root) {
            return Err(format!(
                "committed symlink {} resolves, on this filesystem, to {} \
                 — outside the export root — even though a purely lexical \
                 check did not catch it (a case- or Unicode-normalization- \
                 insensitive filesystem folding a reference onto a \
                 differently-spelled committed symlink); refusing to \
                 continue the export",
                Path::new(&entry.path).display(),
                resolved.display()
            ));
        }
    }
    Ok(())
}

/// Test-only entry point into `export_committed_head` — lets
/// `doctor::runner`'s test module exercise the export mechanism directly
/// (adversarial smudge filters, symlinks, fabrication traps) without going
/// through the full `cargo check` pipeline.
#[cfg(test)]
pub(crate) fn export_committed_head_for_tests(
    repo_root: &Path,
) -> Result<tempfile::TempDir, String> {
    // A generous, fixed timeout — every OTHER export test in this module
    // exercises correctness, not the wall-clock cap itself, so a tiny one
    // would just be one more way for those tests to flake on a slow CI
    // runner. `export_committed_head_for_tests_with_timeout_and_env_override`
    // below is the dedicated entry point for the F14/A-R-F14-1 cap
    // contracts.
    export_committed_head(repo_root, Duration::from_secs(120), &[])
}

/// Test-only entry point for the F14 / A-R-F14-1 wall-clock-cap contracts:
/// same as `export_committed_head_for_tests`, but also lets a test
/// override the environment (e.g. `PATH`, to point `git` at a wrapper
/// script) for the `git ls-tree`/`git cat-file --batch` child processes
/// this export runs, as a per-`Command` `cmd.env()` call — never the
/// process-wide `std::env::set_var`, which would race every other test in
/// this module that spawns `git` with no override of its own.
#[cfg(test)]
pub(crate) fn export_committed_head_for_tests_with_timeout_and_env_override(
    repo_root: &Path,
    timeout: Duration,
    env_overrides: &[(&str, &str)],
) -> Result<tempfile::TempDir, String> {
    export_committed_head(repo_root, timeout, env_overrides)
}
