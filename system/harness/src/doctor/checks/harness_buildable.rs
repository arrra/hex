use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};
use regex::Regex;
use std::path::{Path, PathBuf};
use std::process::Command;

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
    _tempdir: tempfile::TempDir,
}

impl Drop for WorktreeGuard {
    fn drop(&mut self) {
        let _ = Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(&self.worktree_path)
            .current_dir(&self.repo_dir)
            .output();
        // Belt-and-braces: guarantee nothing is left on disk even if
        // `git worktree add` itself never succeeded (so `remove` above
        // has nothing registered to act on).
        let _ = std::fs::remove_dir_all(&self.worktree_path);
        let _ = Command::new("git")
            .args(["worktree", "prune"])
            .current_dir(&self.repo_dir)
            .output();
    }
}

fn run_check(hex_dir: &Path) -> CheckResult {
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

    let _worktree_guard = WorktreeGuard {
        repo_dir: hex_dir.to_path_buf(),
        worktree_path: worktree_path.clone(),
        _tempdir: tempdir,
    };

    let add_output = match Command::new("git")
        .args(["worktree", "add", "--detach"])
        .arg(&worktree_path)
        .arg("HEAD")
        .current_dir(hex_dir)
        .output()
    {
        Ok(o) => o,
        Err(e) => return CheckResult::fail(format!("failed to spawn `git worktree add`: {e}")),
    };
    if !add_output.status.success() {
        return CheckResult::fail(format!(
            "git worktree add failed — cannot verify the harness builds from git: {}",
            String::from_utf8_lossy(&add_output.stderr).trim()
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
    // than being silently skipped. (Review finding G1: skipping this step
    // when Cargo.lock is absent let a genuinely unbuildable checkout falsely
    // PASS.) This never generates a lockfile and never compiles anything.
    //
    // `--filter-platform <host>` is required: without it, `cargo metadata
    // --offline` resolves ALL platforms in the lockfile, including
    // cfg-gated deps (e.g. `android_system_properties`) that are never in
    // the local registry cache on this host — that fails --offline even
    // when everything this host actually needs is present, producing a
    // permanent false FAIL. The host triple comes from `rustc -vV`'s
    // `host:` line; if `rustc` itself fails, fall back to no filter rather
    // than skip the check.
    let mut metadata_args = vec!["metadata", "--locked", "--offline", "--format-version", "1"];
    let host_triple = host_triple();
    if let Some(host) = host_triple.as_deref() {
        metadata_args.push("--filter-platform");
        metadata_args.push(host);
    }
    match Command::new("cargo")
        .args(&metadata_args)
        .current_dir(&harness_dir)
        .output()
    {
        Ok(o) if !o.status.success() => {
            return CheckResult::fail(format!(
                ".hex/harness -> `cargo metadata` failed in a fresh git checkout \
                 (missing/out-of-date Cargo.lock or a missing path dependency) — \
                 fix: git add it or install it from .hex/.upgrade-cache: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            ));
        }
        Err(e) => {
            return CheckResult::fail(format!("failed to spawn `cargo metadata`: {e}"));
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
            scan_dir(&dir, &worktree_path, &mut checked, &mut missing);
        }
    }
    let build_rs = harness_dir.join("build.rs");
    if build_rs.is_file() {
        scan_file(&build_rs, &worktree_path, &mut checked, &mut missing);
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

/// Parses the `host: <triple>` line out of `rustc -vV` so `cargo metadata`
/// can be scoped with `--filter-platform` to this machine's actual target.
/// Returns `None` (rather than panicking) if `rustc` cannot be spawned or
/// the expected line is absent — callers fall back to an unfiltered query.
fn host_triple() -> Option<String> {
    let output = Command::new("rustc").arg("-vV").output().ok()?;
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
