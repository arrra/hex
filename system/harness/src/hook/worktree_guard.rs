//! Claude Code `PreToolUse` hook — enforces Standing Order 7
//! ("all work in worktrees, never the shared checkout").
//!
//! Refuses Write/Edit/MultiEdit/NotebookEdit on **any git repo** when the target
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
//! Scope (Mike, 2026-06-11: "this worktree practice is for any git repo" — the
//! per-repo allowlist `HEX_WORKTREE_GUARD_REPOS` is GONE): every git repo is
//! guarded. The single exemption is the hex workspace itself — the repo whose
//! toplevel is `$HEX_DIR` — because persist-immediately writes to workspace
//! state (todo.md, me/decisions, evolution/) ARE the hex operating model, not
//! multi-agent code work; its concurrent-write hazard is handled by S3 locks.
//!
//! Fail-OPEN: any internal error or unparseable input → Abstain. A broken guard
//! must never wedge the session.

use serde_json::Value;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

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
    match decide(&raw, hex_dir().as_deref()) {
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

/// The hex workspace root from `$HEX_DIR`, canonicalized. `None` when unset —
/// the guard then has no exemption and every repo is guarded.
fn hex_dir() -> Option<PathBuf> {
    let raw = std::env::var("HEX_DIR").ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let p = PathBuf::from(trimmed);
    Some(p.canonicalize().unwrap_or(p))
}

/// Pure decision over the raw hook stdin. Factored out of `run` so it is testable
/// without capturing stdout/exit codes. Every error path returns `Abstain`
/// (fail-open). `exempt_root` is the canonicalized workspace root (`$HEX_DIR`)
/// whose own repo is not guarded.
pub fn decide(raw: &str, exempt_root: Option<&Path>) -> Decision {
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

    decide_for_path(Path::new(file), exempt_root)
}

/// Decision for a concrete target path. Shells out to `git` (the established
/// convention in this crate — see `upgrade.rs`); no git2/gix dependency.
fn decide_for_path(file: &Path, exempt_root: Option<&Path>) -> Decision {
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

    // The hex workspace itself is the one exempt repo (see module docs).
    if let (Some(root), Some(exempt)) = (common_abs.parent(), exempt_root) {
        let root_canon = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        if root_canon == exempt {
            return Decision::Abstain;
        }
    }

    // Ignored runtime files are sweepable state, not work worth protecting —
    // reuse git's own ignore logic.
    if git_check_ignore(&dir, file) {
        return Decision::Abstain;
    }

    let repo_name = common_abs
        .parent()
        .and_then(Path::file_name)
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let repo_root = common_abs.parent().unwrap_or(&common_abs).display();
    Decision::Deny(format!(
        "S7 violation: editing '{}' in the SHARED checkout of git repo '{repo_name}'. \
         All work — any worker, any git repo — happens in a dedicated worktree (concurrent \
         agents in one working tree silently clobber each other's uncommitted edits — see \
         OBS-030/OBS-033). \
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

    #[test]
    fn non_mutating_tool_abstains() {
        let raw = r#"{"tool_name":"Bash","tool_input":{"command":"ls"}}"#;
        assert_eq!(decide(raw, None), Decision::Abstain);
    }

    #[test]
    fn garbage_stdin_abstains() {
        assert_eq!(decide("not json", None), Decision::Abstain);
        assert_eq!(decide("", None), Decision::Abstain);
    }

    #[test]
    fn missing_file_path_abstains() {
        let raw = r#"{"tool_name":"Write","tool_input":{}}"#;
        assert_eq!(decide(raw, None), Decision::Abstain);
    }

    /// Build a real shared checkout (any name — there is no allowlist) and
    /// assert a Write into it is denied; then add a linked worktree and assert
    /// an edit there abstains.
    #[test]
    fn shared_checkout_denied_worktree_abstains() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("any-old-repo");
        fs::create_dir(&repo).unwrap();
        run_git(&repo, &["init", "-q"]);
        run_git(&repo, &["config", "user.email", "t@t"]);
        run_git(&repo, &["config", "user.name", "t"]);
        fs::write(repo.join("seed.txt"), "seed").unwrap();
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-qm", "seed"]);

        // Edit in the SHARED checkout → Deny.
        let target = repo.join("src.rs");
        match decide_for_path(&target, None) {
            Decision::Deny(r) => assert!(r.contains("S7 violation"), "reason: {r}"),
            d => panic!("expected Deny, got {d:?}"),
        }

        // Add a linked worktree and edit there → Abstain.
        let wt = tmp.path().join("any-old-repo-task");
        run_git(
            &repo,
            &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "feat"],
        );
        let wt_target = wt.join("src.rs");
        assert_eq!(decide_for_path(&wt_target, None), Decision::Abstain);
    }

    /// The hex workspace repo (toplevel == exempt_root) is the one exemption —
    /// and the same repo WITHOUT the exemption is denied, proving the exemption
    /// is what does the work.
    #[test]
    fn hex_workspace_repo_exempt() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("hex");
        fs::create_dir(&repo).unwrap();
        run_git(&repo, &["init", "-q"]);
        run_git(&repo, &["config", "user.email", "t@t"]);
        run_git(&repo, &["config", "user.name", "t"]);
        fs::write(repo.join("todo.md"), "now").unwrap();
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-qm", "seed"]);

        let exempt = repo.canonicalize().unwrap();
        let target = repo.join("todo.md");
        assert_eq!(
            decide_for_path(&target, Some(&exempt)),
            Decision::Abstain,
            "the $HEX_DIR workspace repo must not be guarded"
        );

        match decide_for_path(&target, None) {
            Decision::Deny(_) => {}
            d => panic!("expected Deny without exemption, got {d:?}"),
        }
    }

    #[test]
    fn ignored_file_in_shared_checkout_abstains() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("some-repo");
        fs::create_dir(&repo).unwrap();
        run_git(&repo, &["init", "-q"]);
        run_git(&repo, &["config", "user.email", "t@t"]);
        run_git(&repo, &["config", "user.name", "t"]);
        fs::write(repo.join(".gitignore"), "target/\n").unwrap();
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-qm", "seed"]);
        fs::create_dir(repo.join("target")).unwrap();
        let ignored = repo.join("target").join("out.bin");
        assert_eq!(decide_for_path(&ignored, None), Decision::Abstain);
    }

    #[test]
    fn not_a_git_repo_abstains() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("loose.txt");
        assert_eq!(decide_for_path(&target, None), Decision::Abstain);
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
