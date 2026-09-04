//! `hex-e2e` — test-only worker exercised by `tests/harness-e2e/`.
//!
//! NOT part of the production registry. `workers::registry()` appends it ONLY
//! when `HEX_HARNESS_E2E=1`, so it never registers in a real deployment. Its
//! handlers drop marker files under `$HEX_DIR/.hex/harness/markers/` that the
//! lifecycle script asserts on, proving the at-most-once serve loop end to end:
//!
//! - `harness.e2e.ping`   → touch `ping.ran`            (basic emit→handler)
//! - `harness.e2e.slow`   → sleep `sleep_ms`, then
//!   `ctx.emit("harness.e2e.drained")` (diverts to the
//!   outbox if mid-drain), then touch `slow.completed`
//!   (graceful-drain proof)
//! - `harness.e2e.drained`→ append one line to `drained.delivered`
//!   (exactly-once replay proof — line count == deliveries)

use crate::worker::{ctx::Ctx, event::Event, Result, Worker};
use std::io::Write;
use std::time::Duration;

/// Directory the marker files live in: `$HEX_DIR/.hex/harness/markers/`.
fn markers_dir() -> std::path::PathBuf {
    let hex_dir = std::env::var("HEX_DIR").unwrap_or_else(|_| ".".to_string());
    std::path::PathBuf::from(hex_dir)
        .join(".hex")
        .join("harness")
        .join("markers")
}

/// Touch (truncate-create) a marker file, or append a line to it.
fn write_marker(name: &str, append_line: bool) -> Result<()> {
    let dir = markers_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(name);
    if append_line {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        writeln!(f, "delivered")?;
    } else {
        std::fs::write(&path, b"1\n")?;
    }
    Ok(())
}

fn on_ping(_e: Event, _ctx: Ctx) -> Result<()> {
    write_marker("ping.ran", false)
}

fn on_slow(e: Event, ctx: Ctx) -> Result<()> {
    let sleep_ms = e
        .data()
        .raw()
        .get("sleep_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    // Block the (blocking) handler thread for the requested duration so a
    // SIGTERM arriving now finds this handler in-flight — the drain must wait
    // for it to finish (graceful drain), not abandon it.
    std::thread::sleep(Duration::from_millis(sleep_ms));
    // Emitted while (likely) draining → Ctx diverts this to the durable outbox
    // instead of the engine, to be replayed exactly once on the next start.
    ctx.emit("harness.e2e.drained", serde_json::json!({ "from": "slow" }))?;
    write_marker("slow.completed", false)
}

fn on_drained(_e: Event, _ctx: Ctx) -> Result<()> {
    // Append-mode: each delivery adds one line, so the script's line count is
    // the exact delivery count (at-most-once → exactly 1 after a clean replay).
    write_marker("drained.delivered", true)
}

/// Build the `hex-e2e` test worker (registered only under HEX_HARNESS_E2E=1).
pub fn worker() -> Worker {
    Worker::new("hex-e2e")
        .on_event("harness.e2e.ping", on_ping)
        .on_event("harness.e2e.slow", on_slow)
        .on_event("harness.e2e.drained", on_drained)
}
