//! `hex-burn-guard` — Claude spend guardrail on a 10-minute cadence.
//!
//! Cron stub (same shape as `backup.worker.rs`) wrapping `hex usage burn`:
//! trailing-60m burn rate over ALL Claude transcripts (recursive — subagents
//! included), alert above $100/hr (decision 2026-06-12, threshold Mike's).
//! Alerting is loud and deduped (6h) via the shared alert pathway; the
//! guardrail never throttles anything (S6: no silent caps).

use hex::worker::{ctx::Ctx, event::Event, Result, Worker};

/// Cron expression — every 10 minutes.
pub const CRON_EVERY_10M: &str = "0 */10 * * * * *";

/// Argv for the burn check (defaults: $100/hr threshold, 60m window).
pub const ARGV_BURN_CHECK: &[&str] = &["hex", "usage", "burn"];

fn run_burn_check(_e: Event, ctx: Ctx) -> Result<()> {
    let argv: Vec<String> = ARGV_BURN_CHECK.iter().map(|s| s.to_string()).collect();
    ctx.run(&argv).map(|_| ())
}

/// Build the `hex-burn-guard` worker.
pub fn worker() -> Worker {
    Worker::new("hex-burn-guard").on_cron_named("every-10m", CRON_EVERY_10M, run_burn_check)
}
