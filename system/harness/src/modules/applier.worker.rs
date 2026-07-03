//! `hex-applier` — the P2 rule-applier's cron heartbeat.
//!
//! Two crons (same shape as `burn_guard.worker.rs`): land ACCEPT_FLAGGED
//! survivors daily at 05:00 UTC, then run the outcome watchdog daily at
//! 05:10 UTC (staggered 10 minutes after `run` so a rule's very first
//! `land` ledger row exists before `watch` looks for wild evidence on it).
//! No logic lives here — both handlers just shell out to the CLI via
//! `ctx.run`; all applier behavior lives in `hex::applier`.

use hex::worker::{ctx::Ctx, event::Event, Result, Worker};

/// Cron expression — daily at 05:00 UTC.
pub const CRON_RUN: &str = "0 0 5 * * * *";

/// Cron expression — daily at 05:10 UTC.
pub const CRON_WATCH: &str = "0 10 5 * * * *";

/// Argv for landing/escalating ACCEPT_FLAGGED survivors.
pub const ARGV_RUN: &[&str] = &["hex", "apply", "run"];

/// Argv for the outcome watchdog (auto-revert / one-time success scoring).
pub const ARGV_WATCH: &[&str] = &["hex", "apply", "watch"];

fn run_apply_run(_e: Event, ctx: Ctx) -> Result<()> {
    let argv: Vec<String> = ARGV_RUN.iter().map(|s| s.to_string()).collect();
    ctx.run(&argv).map(|_| ())
}

fn run_apply_watch(_e: Event, ctx: Ctx) -> Result<()> {
    let argv: Vec<String> = ARGV_WATCH.iter().map(|s| s.to_string()).collect();
    ctx.run(&argv).map(|_| ())
}

/// Build the `hex-applier` worker.
pub fn worker() -> Worker {
    Worker::new("hex-applier")
        .on_cron_named("daily-run-0500", CRON_RUN, run_apply_run)
        .on_cron_named("daily-watch-0510", CRON_WATCH, run_apply_watch)
}
