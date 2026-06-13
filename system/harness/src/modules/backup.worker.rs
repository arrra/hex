//! `hex-backup` — daily backup of the hex workspace.
//!
//! Migrated as a Rust cron worker per the harness-as-Rust-library spec. There
//! is currently no `system/iii/workers/hex-backup.yaml` checked in — the YAML
//! host never carried hex-backup. We register it here directly with a sensible
//! daily schedule; future tweaks happen in this file, not in YAML.
//!
//! - id `hex::backup::daily` command `hex backup` cron `0 0 4 * * * *` (04:00 daily)

use hex::worker::{ctx::Ctx, event::Event, Result, Worker};

/// Cron expression for the nightly backup job — 04:00 daily.
pub const CRON_DAILY: &str = "0 0 4 * * * *";

/// Argv for the backup job.
pub const ARGV_BACKUP: &[&str] = &["hex", "backup"];

fn run_backup(_e: Event, ctx: Ctx) -> Result<()> {
    let argv: Vec<String> = ARGV_BACKUP.iter().map(|s| s.to_string()).collect();
    ctx.run(&argv).map(|_| ())
}

/// Build the `hex-backup` worker.
pub fn worker() -> Worker {
    Worker::new("hex-backup").on_cron_named("daily", CRON_DAILY, run_backup)
}
