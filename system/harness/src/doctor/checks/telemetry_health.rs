//! telemetry-health: is the native telemetry store healthy?
//!
//! Checks the SQLite events store at `$HEX_DIR/.hex/telemetry/events.db`:
//! - DB file missing                    -> Skip ("no telemetry store yet")
//! - Any non-`ok` event in last 24h     -> Warn (LOUD + actionable; Standing Order S6)
//! - All events ok (or none) in last 24h -> Pass
//!
//! The crate::telemetry module owns the schema and queries. This check just
//! aggregates the last-24h slice via `telemetry::failures` and `telemetry::recent`.

use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};
use crate::telemetry;
use chrono::{Duration, Utc};

pub struct TelemetryHealth;

impl DoctorCheck for TelemetryHealth {
    fn name(&self) -> &str { "telemetry-health" }
    fn category(&self) -> Category { Category::Health }
    fn run(&self, ctx: &Context) -> CheckResult {
        let db = ctx.hex_dir.join(".hex/telemetry/events.db");
        if !db.is_file() {
            return CheckResult::skip("no telemetry store yet");
        }

        let since = Utc::now() - Duration::hours(24);
        let failures = match telemetry::failures(since) {
            Ok(rows) => rows,
            Err(e) => {
                return CheckResult::warn(&format!(
                    "telemetry store unreadable: {e} — inspect .hex/telemetry/events.db"
                ));
            }
        };

        if !failures.is_empty() {
            let last = &failures[0];
            return CheckResult::warn(&format!(
                "{} telemetry failure(s) in last 24h (most recent: {}). \
                 Run `hex telemetry failures` to inspect",
                failures.len(),
                last.event
            ));
        }

        // Count total events in last 24h for the pass message.
        let total = telemetry::recent(usize::MAX)
            .map(|rows| rows.into_iter().filter(|r| r.ts.as_str() >= since.to_rfc3339().as_str()).count())
            .unwrap_or(0);
        CheckResult::pass(&format!(
            "telemetry store healthy ({total} events, 0 failures/24h)"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn ctx() -> Context {
        Context { hex_dir: PathBuf::from("/tmp/fake-hex-telemetry-nope"), home: PathBuf::from("/tmp"), fix: false }
    }

    #[test]
    fn name_and_category() {
        assert_eq!(TelemetryHealth.name(), "telemetry-health");
        assert_eq!(TelemetryHealth.category(), Category::Health);
    }

    #[test]
    fn skips_when_db_missing() {
        let r = TelemetryHealth.run(&ctx());
        assert!(!r.message.is_empty());
    }
}
