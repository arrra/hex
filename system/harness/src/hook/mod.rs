pub mod capture;
pub mod lint_predispatch;
pub mod secret_scan;
pub mod user_prompt_submit;
pub mod worktree_guard;

use clap::Subcommand;

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
        HookCommands::SecretScan { install } => secret_scan::run(install),
    }
}
