//! Claude Code `PreToolUse` hook — enforces Standing Order 7
//! ("all work in worktrees, never the shared checkout").
//!
//! Refuses Write/Edit/MultiEdit/NotebookEdit on a FLAGGED repo when the target
//! lives in the repo's SHARED checkout (the main working tree) rather than a
//! linked worktree. This is the mechanical backstop for the dirty-tree hazard in
//! `evolution/observations.md` OBS-030 (a concurrent agent's reset silently wiped
//! an uncommitted edit twice in one day).
//!
//! Port of the former `system/hooks/scripts/worktree-guard.sh`. Two differences
//! from the bash, both deliberate:
//!   1. Logic lives in the typed, tested harness (no logic in scripts).
//!   2. DENY-ONLY. The bash emitted an explicit `permissionDecision":"allow"` on
//!      every non-blocked edit, which auto-approves the tool and short-circuits
//!      Claude Code's permission prompt. A guard's single responsibility is to
//!      say "no"; granting blanket approval as a side effect is permission-policy
//!      leakage. So on every allow path we ABSTAIN — emit nothing, exit 0 — and
//!      only ever emit deny + exit 2 on a block.
//!
//! Honest framing (mirrors the old script): this is a FOOTGUN-GUARD, not a
//! security boundary. An agent runs as the same user and can bypass it. It makes
//! the right path (worktree) the easy path; it does not make the wrong path
//! impossible.
//!
//! Flagged repos: env `HEX_WORKTREE_GUARD_REPOS` (space/comma list of repo dir
//! basenames) overrides the default. Default: `hex-foundation`. Personal-data
//! workspaces (the HEX_DIR checkout) are intentionally NOT flagged — the hazard
//! is multi-agent CODE repos.
//!
//! Fail-OPEN: any internal error or unparseable input → Abstain. A broken guard
//! must never wedge the session.

use serde_json::Value;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

const DEFAULT_FLAGGED: &str = "hex-foundation";

/// The guard's verdict for a single tool call. `Deny` carries a human-readable,
/// actionable reason; `Abstain` means "no opinion — defer to normal permission
/// flow" and is emitted as nothing-on-stdout + exit 0.
#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    Deny(String),
    Abstain,
}

pub fn run() {
    let mut raw = String::new();
    if std::io::stdin().read_to_string(&mut raw).is_err() {
        std::process::exit(0); // fail-open
    }
    match decide(&raw, &flagged_repos()) {
        Decision::Deny(reason) => {
            // Block via BOTH channels so the guard holds across Claude Code
            // versions: structured JSON on stdout (modern contract) AND exit 2
            // with the reason on stderr (legacy/universal block path). Either
            // alone blocks; together they're robust to contract drift (S6).
            let out = serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "deny",
                    "permissionDecisionReason": reason,
                }
            });
            println!("{out}");
            eprintln!("{reason}");
            std::process::exit(2);
        }
        Decision::Abstain => std::process::exit(0),
    }
}

/// Flagged-repo basenames from env, default `hex-foundation`. Comma OR whitespace
/// separated.
fn flagged_repos() -> Vec<String> {
    let raw = std::env::var("HEX_WORKTREE_GUARD_REPOS").unwrap_or_else(|_| DEFAULT_FLAGGED.into());
    raw.split([',', ' ', '\t', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_owned())
        .collect()
}

/// Pure decision over the raw hook stdin. Factored out of `run` so it is testable
/// without capturing stdout/exit codes. Every error path returns `Abstain`
/// (fail-open).
pub fn decide(raw: &str, flagged: &[String]) -> Decision {
    let input: Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(_) => return Decision::Abstain,
    };

    // Only mutating file tools are in scope.
    let tool = input.get("tool_name").and_then(Value::as_str).unwrap_or("");
    match tool {
        "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => {}
        _ => return Decision::Abstain,
    }

    let file = match input
        .get("tool_input")
        .and_then(|ti| ti.get("file_path"))
        .and_then(Value::as_str)
    {
        Some(f) if !f.is_empty() => f,
        _ => return Decision::Abstain,
    };

    decide_for_path(Path::new(file), flagged)
}

/// Decision for a concrete target path. Shells out to `git` (the established
/// convention in this crate — see `upgrade.rs`); no git2/gix dependency.
fn decide_for_path(file: &Path, flagged: &[String]) -> Decision {
    // The file may not exist yet (a new Write): resolve the deepest EXISTING
    // ancestor directory and run git there.
    let dir = match deepest_existing_dir(file) {
        Some(d) => d,
        None => return Decision::Abstain,
    };

    // Is this a git repo at all?
    if git(&dir, &["rev-parse", "--git-dir"]).is_none() {
        return Decision::Abstain;
    }

    // Submodule guard: a submodule also has git-dir != git-common-dir, but it is
    // NOT the multi-agent-worktree hazard. Treat submodules as Abstain.
    if git(&dir, &["rev-parse", "--show-superproject-working-tree"])
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
    {
        return Decision::Abstain;
    }

    let git_dir = match git(&dir, &["rev-parse", "--absolute-git-dir"]) {
        Some(s) => PathBuf::from(s.trim()),
        None => return Decision::Abstain,
    };
    // `--git-common-dir` may be relative to cwd; resolve it against `dir`.
    let common_rel = match git(&dir, &["rev-parse", "--git-common-dir"]) {
        Some(s) => s.trim().to_owned(),
        None => return Decision::Abstain,
    };
    let common_abs = {
        let p = PathBuf::from(&common_rel);
        let joined = if p.is_absolute() { p } else { dir.join(p) };
        joined.canonicalize().unwrap_or(joined)
    };
    let git_dir_abs = git_dir.canonicalize().unwrap_or(git_dir);

    // A linked worktree has git-dir != git-common-dir → the SAFE, isolated case.
    if git_dir_abs != common_abs {
        return Decision::Abstain;
    }
    // From here: git-dir == git-common-dir → the SHARED checkout (main worktree).

    // Is this repo flagged? Canonical name = basename of the dir containing the
    // common .git.
    let repo_name = common_abs
        .parent()
        .and_then(Path::file_name)
        .and_then(|s| s.to_str())
        .unwrap_or("");
    if !flagged.iter().any(|r| r == repo_name) {
        return Decision::Abstain;
    }

    // Ignored runtime files are sweepable state, not work worth protecting —
    // reuse git's own ignore logic.
    if git_check_ignore(&dir, file) {
        return Decision::Abstain;
    }

    let repo_root = common_abs.parent().unwrap_or(&common_abs).display();
    Decision::Deny(format!(
        "S7 violation: editing '{}' in the SHARED checkout of flagged repo '{repo_name}'. \
         All work must happen in a git worktree (concurrent agents in one working tree silently \
         clobber each other's uncommitted edits — see OBS-030). \
         Fix: git -C {repo_root} worktree add ../{repo_name}-<task> -b feature/<name>  \
         (or use the using-git-worktrees skill), then edit there.",
        file.display(),
    ))
}

/// Deepest existing directory at or above `file`.
fn deepest_existing_dir(file: &Path) -> Option<PathBuf> {
    let mut dir: &Path = if file.is_dir() { file } else { file.parent()? };
    loop {
        if dir.is_dir() {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
}

/// Run a git command in `dir`, returning trimmed stdout on success (exit 0),
/// else None. Never panics.
fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        None
    }
}

/// True iff `file` is git-ignored (in `dir`'s repo). `check-ignore -q` exits 0
/// when the path IS ignored.
fn git_check_ignore(dir: &Path, file: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["check-ignore", "-q"])
        .arg(file)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn flagged() -> Vec<String> {
        vec!["hex-foundation".to_owned()]
    }

    #[test]
    fn non_mutating_tool_abstains() {
        let raw = r#"{"tool_name":"Bash","tool_input":{"command":"ls"}}"#;
        assert_eq!(decide(raw, &flagged()), Decision::Abstain);
    }

    #[test]
    fn garbage_stdin_abstains() {
        assert_eq!(decide("not json", &flagged()), Decision::Abstain);
        assert_eq!(decide("", &flagged()), Decision::Abstain);
    }

    #[test]
    fn missing_file_path_abstains() {
        let raw = r#"{"tool_name":"Write","tool_input":{}}"#;
        assert_eq!(decide(raw, &flagged()), Decision::Abstain);
    }

    /// Build a real shared checkout named like a flagged repo and assert a Write
    /// into it is denied; then add a linked worktree and assert an edit there
    /// abstains.
    #[test]
    fn shared_checkout_denied_worktree_abstains() {
        let tmp = tempfile::tempdir().unwrap();
        // Repo dir basename must match the flagged name.
        let repo = tmp.path().join("hex-foundation");
        fs::create_dir(&repo).unwrap();
        run_git(&repo, &["init", "-q"]);
        run_git(&repo, &["config", "user.email", "t@t"]);
        run_git(&repo, &["config", "user.name", "t"]);
        fs::write(repo.join("seed.txt"), "seed").unwrap();
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-qm", "seed"]);

        // Edit in the SHARED checkout → Deny.
        let target = repo.join("src.rs");
        match decide_for_path(&target, &flagged()) {
            Decision::Deny(r) => assert!(r.contains("S7 violation"), "reason: {r}"),
            d => panic!("expected Deny, got {d:?}"),
        }

        // Add a linked worktree and edit there → Abstain.
        let wt = tmp.path().join("hex-foundation-task");
        run_git(
            &repo,
            &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "feat"],
        );
        let wt_target = wt.join("src.rs");
        assert_eq!(decide_for_path(&wt_target, &flagged()), Decision::Abstain);
    }

    #[test]
    fn unflagged_repo_abstains() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("some-other-repo");
        fs::create_dir(&repo).unwrap();
        run_git(&repo, &["init", "-q"]);
        let target = repo.join("src.rs");
        assert_eq!(decide_for_path(&target, &flagged()), Decision::Abstain);
    }

    #[test]
    fn ignored_file_in_flagged_checkout_abstains() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("hex-foundation");
        fs::create_dir(&repo).unwrap();
        run_git(&repo, &["init", "-q"]);
        run_git(&repo, &["config", "user.email", "t@t"]);
        run_git(&repo, &["config", "user.name", "t"]);
        fs::write(repo.join(".gitignore"), "target/\n").unwrap();
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-qm", "seed"]);
        fs::create_dir(repo.join("target")).unwrap();
        let ignored = repo.join("target").join("out.bin");
        assert_eq!(decide_for_path(&ignored, &flagged()), Decision::Abstain);
    }

    #[test]
    fn not_a_git_repo_abstains() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("loose.txt");
        assert_eq!(decide_for_path(&target, &flagged()), Decision::Abstain);
    }

    fn run_git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }
}
