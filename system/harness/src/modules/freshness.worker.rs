//! `hex-freshness` — daily freshness alerting over the agent-infra ledger.
//!
//! Closes PLAN.md E0 step 4 ("freshness daily cron"): `hex ledger freshness`
//! prints last-seen-at per agent, appends an `alert` ledger row plus a macOS
//! notification for any agent whose age exceeds its charter window
//! (`hex::ledger::default_freshness_window_secs`), and exits non-zero when
//! anything is stale — loud per SO S6. This file is only the cron stub the
//! worker registry picks up (same shape as `backup.worker.rs`).
//!
//! 09:00 daily, so stale-agent alerts land while Mike is awake.

use hex::worker::{ctx::Ctx, event::Event, Result, Worker};

/// Cron expression — 09:00 daily.
pub const CRON_DAILY_0900: &str = "0 0 9 * * * *";

/// Argv for the freshness check.
pub const ARGV_FRESHNESS: &[&str] = &["hex", "ledger", "freshness"];

fn run_freshness(_e: Event, ctx: Ctx) -> Result<()> {
    let argv: Vec<String> = ARGV_FRESHNESS.iter().map(|s| s.to_string()).collect();
    ctx.run(&argv).map(|_| ())
}

/// Build the `hex-freshness` worker.
pub fn worker() -> Worker {
    Worker::new("hex-freshness").on_cron_named("daily", CRON_DAILY_0900, run_freshness)
}
