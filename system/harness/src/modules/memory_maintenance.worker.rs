//! `hex-memory-maintenance` — Rust port of
//! `system/iii/workers/memory-maintenance.yaml`.
//!
//! Preserves the YAML's job ids, commands, and cron expressions exactly:
//!
//! - id `hex::memory::index`               command `hex memory index`               cron `0 */15 * * * * *`
//! - id `hex::memory::consolidate_quick`   command `hex memory consolidate quick`   cron `0 */15 * * * * *`
//! - id `hex::memory::parse_transcripts`   command `hex memory parse-transcripts`   cron `0 */15 * * * * *`
//! - id `hex::memory::consolidate_full`    command `hex memory consolidate full`    cron `0 0 3 * * * *`
//!
//! The YAML file is intentionally left in place (additive migration — a later
//! spec removes the YAML-host path).

use hex::worker::{ctx::Ctx, event::Event, Result, Worker};

/// Cron expression for `hex::memory::index` — every 15 minutes.
pub const CRON_INDEX: &str = "0 */15 * * * * *";

/// Cron expression for `hex::memory::consolidate_quick` — every 15 minutes.
pub const CRON_CONSOLIDATE_QUICK: &str = "0 */15 * * * * *";

/// Cron expression for `hex::memory::parse_transcripts` — every 15 minutes.
pub const CRON_PARSE_TRANSCRIPTS: &str = "0 */15 * * * * *";

/// Cron expression for `hex::memory::consolidate_full` — 03:00 daily.
pub const CRON_CONSOLIDATE_FULL: &str = "0 0 3 * * * *";

/// Argv for the index job — mirrors the YAML `command:` array.
pub const ARGV_INDEX: &[&str] = &["hex", "memory", "index"];

/// Argv for the quick-consolidation job (Layers 1+2, deterministic).
pub const ARGV_CONSOLIDATE_QUICK: &[&str] = &["hex", "memory", "consolidate", "quick"];

/// Argv for the transcript-parse job.
pub const ARGV_PARSE_TRANSCRIPTS: &[&str] = &["hex", "memory", "parse-transcripts"];

/// Argv for the nightly full-consolidation job.
pub const ARGV_CONSOLIDATE_FULL: &[&str] = &["hex", "memory", "consolidate", "full"];

fn run_index(_e: Event, ctx: Ctx) -> Result<()> {
    let argv: Vec<String> = ARGV_INDEX.iter().map(|s| s.to_string()).collect();
    ctx.run(&argv).map(|_| ())
}

fn run_consolidate_quick(_e: Event, ctx: Ctx) -> Result<()> {
    let argv: Vec<String> = ARGV_CONSOLIDATE_QUICK
        .iter()
        .map(|s| s.to_string())
        .collect();
    ctx.run(&argv).map(|_| ())
}

fn run_parse_transcripts(_e: Event, ctx: Ctx) -> Result<()> {
    let argv: Vec<String> = ARGV_PARSE_TRANSCRIPTS
        .iter()
        .map(|s| s.to_string())
        .collect();
    ctx.run(&argv).map(|_| ())
}

fn run_consolidate_full(_e: Event, ctx: Ctx) -> Result<()> {
    let argv: Vec<String> = ARGV_CONSOLIDATE_FULL
        .iter()
        .map(|s| s.to_string())
        .collect();
    ctx.run(&argv).map(|_| ())
}

/// Build the `hex-memory-maintenance` worker.
pub fn worker() -> Worker {
    Worker::new("hex-memory-maintenance")
        .on_cron(CRON_INDEX, run_index)
        .on_cron(CRON_CONSOLIDATE_QUICK, run_consolidate_quick)
        .on_cron(CRON_PARSE_TRANSCRIPTS, run_parse_transcripts)
        .on_cron(CRON_CONSOLIDATE_FULL, run_consolidate_full)
}
