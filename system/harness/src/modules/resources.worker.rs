//! `hex-resources` — hourly disk/resource sampler (tier 0+1).
//! Detection + emission only; never cleans anything. Subscribers to
//! resource.pressure are staged until the first real pressure event
//! (proposal: telemetry-consumption-layer v2, C2 tier-2).
//!
//! Worker name `hex-resources` matches the telemetry `source` used by the
//! CLI's sample rows on purpose: the runtime's auto-trace rows for this
//! worker land as event `hex-resources::0` which does not collide with
//! `sample::df`/`sample::du` (different event names, same source).

use hex::worker::{ctx::Ctx, event::Event, Result, Worker};

/// Hourly, on the half-hour offset to avoid colliding with the
/// hex-reconciler's on-the-hour cron.
pub const CRON_HOURLY: &str = "0 30 * * * * *";

fn run_sample(_e: Event, ctx: Ctx) -> Result<()> {
    ctx.run(&["hex".to_string(), "resources".to_string(), "sample".to_string()])
        .map(|_| ())
}

pub fn worker() -> Worker {
    Worker::new("hex-resources").on_cron(CRON_HOURLY, run_sample)
}
