//! `hex-failures` — daily unexpected-failure digest over the telemetry store.
//! Detection only; alerts via hex::alert::notify (deduped per condition key).
//! Runs INSIDE the harness — its own absence is covered by the out-of-process
//! probe (`hex failures probe`, launchd: com.hex.failures-probe).
//!
//! `hex failures` exits 1 when anything is bad and ctx.run treats non-zero as
//! Err — so a bad digest records status=error for this fire with the digest
//! tail in detail. Correct and intentional: the digest IS the failure surface.

use hex::worker::{ctx::Ctx, event::Event, Result, Worker};

/// 13:30 UTC daily ≈ 06:30 PT — digest lands at the start of Mike's day.
pub const CRON_DAILY_1330_UTC: &str = "0 30 13 * * * *";

fn run_failures(_e: Event, ctx: Ctx) -> Result<()> {
    ctx.run(&["hex".to_string(), "failures".to_string(), "--alert".to_string()])
        .map(|_| ())
}

pub fn worker() -> Worker {
    Worker::new("hex-failures").on_cron_named("daily", CRON_DAILY_1330_UTC, run_failures)
}
