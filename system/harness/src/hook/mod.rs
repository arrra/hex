pub mod capture;
pub mod lint_predispatch;
pub mod title_nudge;
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
    /// Claude Code PreToolUse hook — block edits to a flagged repo's shared
    /// checkout (Standing Order 7: all work in worktrees). Deny-only; abstains
    /// otherwise.
    #[command(name = "worktree-guard")]
    WorktreeGuard,
    /// Claude Code PreToolUse hook — shadow-lint `boi dispatch <spec>` Bash
    /// calls in-process (one intent ledger row per gate); blocks only specs
    /// BOI itself would reject (unreadable / parse error).
    #[command(name = "lint-predispatch")]
    LintPredispatch,
}

pub fn run(command: HookCommands) {
    match command {
        HookCommands::Capture => capture::run(),
        HookCommands::UserPromptSubmit => user_prompt_submit::run(),
        HookCommands::WorktreeGuard => worktree_guard::run(),
        HookCommands::LintPredispatch => lint_predispatch::run(),
    }
}
