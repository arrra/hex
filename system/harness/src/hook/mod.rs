pub mod backup_session;
pub mod post_tool_use;
pub mod session_start;
pub mod title_nudge;
pub mod user_prompt_submit;

use clap::Subcommand;

#[derive(Subcommand)]
pub enum HookCommands {
    /// Claude Code SessionStart hook — blocker primitive + channel checkpoint resume
    #[command(name = "session-start")]
    SessionStart,
    /// Claude Code UserPromptSubmit/Stop hook — copy live .jsonl to raw/transcripts/
    #[command(name = "backup-session")]
    BackupSession,
    /// Claude Code PostToolUse hook — emit tool.post_use events (filtered)
    #[command(name = "post-tool-use")]
    PostToolUse,
    /// Claude Code UserPromptSubmit hook — inject relevant workspace memory
    #[command(name = "user-prompt-submit")]
    UserPromptSubmit,
}

pub fn run(command: HookCommands) {
    match command {
        HookCommands::SessionStart => session_start::run(),
        HookCommands::BackupSession => backup_session::run(),
        HookCommands::PostToolUse => post_tool_use::run(),
        HookCommands::UserPromptSubmit => user_prompt_submit::run(),
    }
}
