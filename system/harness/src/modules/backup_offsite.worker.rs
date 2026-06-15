//! `hex-backup-offsite` — nightly off-site encrypted backup of the operating
//! layer via restic → mounted Google Drive.
//!
//! Companion to `hex-backup` (modules/backup.worker.rs): that one takes the
//! local consistent sqlite snapshot at 04:00; this one ships the whole
//! operating layer off-machine at 04:30 (encrypted, deduplicated, retained,
//! integrity-checked). Logic lives in typed Rust (`hex::backup::run_offsite`),
//! invoked via `hex backup offsite` — no shell script (the missing
//! `backup-to-gdrive.sh` is exactly what silently killed the old gdrive worker).
//!
//! - id `hex::backup::offsite` command `hex backup offsite` cron `0 30 4 * * * *` (04:30 daily)
//!
//! No-op until `RESTIC_REPOSITORY` (+ a Keychain password) is set in the
//! harness env, so it never false-alarms before the repo is initialized.

use hex::worker::{ctx::Ctx, event::Event, Result, Worker};

/// Cron expression for the nightly off-site backup — 04:30 daily, after the
/// 04:00 local snapshot.
pub const CRON_OFFSITE: &str = "0 30 4 * * * *";

/// Argv for the off-site backup job.
pub const ARGV_OFFSITE: &[&str] = &["hex", "backup", "offsite"];

fn run_offsite(_e: Event, ctx: Ctx) -> Result<()> {
    let argv: Vec<String> = ARGV_OFFSITE.iter().map(|s| s.to_string()).collect();
    ctx.run(&argv).map(|_| ())
}

/// Build the `hex-backup-offsite` worker.
pub fn worker() -> Worker {
    Worker::new("hex-backup-offsite").on_cron(CRON_OFFSITE, run_offsite)
}
