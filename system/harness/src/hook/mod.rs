pub mod capture;
pub mod lint_predispatch;
pub mod router;
pub mod secret_scan;
pub mod user_prompt_submit;
pub mod worktree_guard;

use clap::Subcommand;
use std::path::PathBuf;

/// Resolve the workspace root a hook should read/write against.
///
/// `HEX_DIR` set → that path, unconditionally (behaviour is unchanged for
/// every real hex-workspace invocation). Otherwise fall back to
/// `CLAUDE_PROJECT_DIR`, but ONLY when it actually looks like a hex workspace
/// (carries the `.hex/version.txt` marker). Without that guard, a hook
/// launched with `HEX_DIR` unset and `CLAUDE_PROJECT_DIR` pointing at some
/// other git checkout (e.g. a BOI worker's worktree) silently writes hex
/// state files into a foreign repo — BUG A, commit a823bee. On the no-op
/// path this prints exactly one stderr line naming the dir and reason, and
/// the caller must write nothing and exit 0.
pub fn resolve_hex_dir(hook_name: &str) -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("HEX_DIR") {
        return Some(PathBuf::from(dir));
    }
    match std::env::var("CLAUDE_PROJECT_DIR") {
        Ok(dir) => {
            let path = PathBuf::from(&dir);
            if path.join(".hex/version.txt").is_file() {
                Some(path)
            } else {
                eprintln!(
                    "[hook/{hook_name}] not a hex workspace ({dir}) and HEX_DIR unset — no-op"
                );
                None
            }
        }
        Err(_) => {
            eprintln!("[hook/{hook_name}] HEX_DIR/CLAUDE_PROJECT_DIR both unset — no-op");
            None
        }
    }
}

#[derive(Subcommand)]
pub enum HookCommands {
    /// Claude Code Stop hook — copy live .jsonl to raw/transcripts/
    // `backup-session` is a hidden deprecated alias kept so sessions that cached
    // the old Stop-hook wiring keep working until they reattach. Remove later.
    #[command(name = "capture", alias = "backup-session")]
    Capture,
    /// Claude Code UserPromptSubmit hook — inject relevant workspace memory
    #[command(name = "user-prompt-submit")]
    UserPromptSubmit,
    /// Claude Code PreToolUse hook — block edits to any git repo's shared
    /// checkout; only the $HEX_DIR workspace repo is exempt (Standing Order 7:
    /// all work in worktrees). Deny-only; abstains otherwise.
    #[command(name = "worktree-guard")]
    WorktreeGuard,
    /// Claude Code PreToolUse hook — shadow-lint `boi dispatch <spec>` Bash
    /// calls in-process (one intent ledger row per gate); blocks only specs
    /// BOI itself would reject (unreadable / parse error).
    #[command(name = "lint-predispatch")]
    LintPredispatch,
    /// Claude Code PreToolUse hook — drop-in Rust port of the reference
    /// `system/hooks/scripts/pretooluse-router.py`. Same rules file, same
    /// decisions, same ledger; see `router.rs` for the pinned contract.
    #[command(name = "router")]
    Router,
    /// Scan the staged diff (added lines) of the repo in the current directory
    /// for secret patterns; on a hit print file/line/pattern with the value
    /// REDACTED and exit 1. `--install` writes a pre-commit shim into the repo's
    /// hooks dir (refuses to overwrite a differing existing hook).
    #[command(name = "secret-scan")]
    SecretScan {
        /// Install a pre-commit shim invoking `hex hook secret-scan`.
        #[arg(long)]
        install: bool,
    },
}

pub fn run(command: HookCommands) {
    match command {
        HookCommands::Capture => capture::run(),
        HookCommands::UserPromptSubmit => user_prompt_submit::run(),
        HookCommands::WorktreeGuard => worktree_guard::run(),
        HookCommands::LintPredispatch => lint_predispatch::run(),
        HookCommands::Router => router::run(),
        HookCommands::SecretScan { install } => secret_scan::run(install),
    }
}
