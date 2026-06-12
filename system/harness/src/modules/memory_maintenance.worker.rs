//! `hex-memory-maintenance` — Rust port of
//! `system/iii/workers/memory-maintenance.yaml`.
//!
//! Jobs (the YAML kept index + consolidate_full only; this host is canonical):
//!
//! - id `hex::memory::index`               command `hex memory index`               cron `0 */15 * * * * *`
//! - id `hex::memory::consolidate_quick`   command `hex memory consolidate quick`   cron `0 5,20,35,50 * * * * *`
//! - id `hex::memory::parse_transcripts`   command `hex memory parse-transcripts`   cron `0 */15 * * * * *`
//! - id `hex::memory::consolidate_full`    command `hex memory consolidate full`    cron `0 0 3 * * * *`
//! - id `hex::memory::maintain`            command `hex memory maintain --vacuum --backfill-facts`
//!                                                                                  cron `0 30 4 * * SUN *`
//!
//! The YAML file is intentionally left in place (additive migration — a later
//! spec removes the YAML-host path).

use hex::worker::{ctx::Ctx, event::Event, Result, Worker};

/// Cron expression for `hex::memory::index` — every 15 minutes.
pub const CRON_INDEX: &str = "0 */15 * * * * *";

/// Quick consolidation — offset from :00 so it never collides with the
/// 03:00:00Z full run (2026-06-10: full lock-skipped behind a quick tick
/// that fired the same second).
pub const CRON_CONSOLIDATE_QUICK: &str = "0 5,20,35,50 * * * * *";

/// Cron expression for `hex::memory::parse_transcripts` — every 15 minutes.
pub const CRON_PARSE_TRANSCRIPTS: &str = "0 */15 * * * * *";

/// Cron expression for `hex::memory::consolidate_full` — 03:00 daily.
pub const CRON_CONSOLIDATE_FULL: &str = "0 0 3 * * * *";

/// Weekly self-repair — Sunday 04:30Z (after the 04:00Z backup).
/// (cron 0.15, the parser inside the baked-in iii engine, accepts named
/// day-of-week tokens: "sun"|"sunday" → ordinal 1, case-insensitive.)
pub const CRON_MAINTAIN: &str = "0 30 4 * * SUN *";

/// Argv for the index job — mirrors the YAML `command:` array.
pub const ARGV_INDEX: &[&str] = &["hex", "memory", "index"];

/// Argv for the quick-consolidation job (Layers 1+2, deterministic).
pub const ARGV_CONSOLIDATE_QUICK: &[&str] = &["hex", "memory", "consolidate", "quick"];

/// Argv for the transcript-parse job.
pub const ARGV_PARSE_TRANSCRIPTS: &[&str] = &["hex", "memory", "parse-transcripts"];

/// Argv for the nightly full-consolidation job.
pub const ARGV_CONSOLIDATE_FULL: &[&str] = &["hex", "memory", "consolidate", "full"];

/// Argv for the weekly self-repair job.
pub const ARGV_MAINTAIN: &[&str] = &["hex", "memory", "maintain", "--vacuum", "--backfill-facts"];

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

fn run_maintain(_e: Event, ctx: Ctx) -> Result<()> {
    let argv: Vec<String> = ARGV_MAINTAIN.iter().map(|s| s.to_string()).collect();
    ctx.run(&argv).map(|_| ())
}

/// Build the `hex-memory-maintenance` worker.
pub fn worker() -> Worker {
    Worker::new("hex-memory-maintenance")
        .on_cron(CRON_INDEX, run_index)
        .on_cron(CRON_CONSOLIDATE_QUICK, run_consolidate_quick)
        .on_cron(CRON_PARSE_TRANSCRIPTS, run_parse_transcripts)
        .on_cron(CRON_CONSOLIDATE_FULL, run_consolidate_full)
        .on_cron(CRON_MAINTAIN, run_maintain)
}
