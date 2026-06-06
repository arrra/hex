pub mod capture;
pub mod title_nudge;
pub mod user_prompt_submit;

use clap::Subcommand;

#[derive(Subcommand)]
pub enum HookCommands {
    /// Claude Code Stop hook — copy live .jsonl to raw/transcripts/
    #[command(name = "capture")]
    Capture,
    /// Claude Code UserPromptSubmit hook — inject relevant workspace memory
    #[command(name = "user-prompt-submit")]
    UserPromptSubmit,
}

pub fn run(command: HookCommands) {
    match command {
        HookCommands::Capture => capture::run(),
        HookCommands::UserPromptSubmit => user_prompt_submit::run(),
    }
}
