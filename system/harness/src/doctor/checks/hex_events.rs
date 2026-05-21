use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};
use std::time::{Duration, SystemTime};

/// check_16: hex-events daemon is reachable.
///
/// Validates the Rust-native event engine setup at `~/.hex-events/`:
///   - policies directory present and non-empty
///   - events.db exists
///   - recent heartbeat (< 5 min old) indicates the daemon is running
pub struct HexEventsReachable;

impl DoctorCheck for HexEventsReachable {
    fn name(&self) -> &str { "hex-events-reachable" }
    fn category(&self) -> Category { Category::Health }
    fn run(&self, ctx: &Context) -> CheckResult {
        let events_dir = ctx.home.join(".hex-events");
        if !events_dir.is_dir() {
            return CheckResult::warn("~/.hex-events/ directory not found");
        }

        let policies_dir = events_dir.join("policies");
        if !policies_dir.is_dir() {
            return CheckResult::warn("~/.hex-events/policies/ not found");
        }

        let policy_count = match std::fs::read_dir(&policies_dir) {
            Ok(rd) => rd
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.path()
                        .extension()
                        .and_then(|s| s.to_str())
                        .map(|s| s == "yaml" || s == "yml")
                        .unwrap_or(false)
                })
                .count(),
            Err(_) => 0,
        };
        if policy_count == 0 {
            return CheckResult::warn("~/.hex-events/policies/ has no .yaml files");
        }

        let db_path = events_dir.join("events.db");
        if !db_path.is_file() {
            return CheckResult::warn("~/.hex-events/events.db not found");
        }

        // Heartbeat liveness: the `hex events daemon` loop writes
        // ~/.hex-events/last-heartbeat.json atomically on each 60s heartbeat
        // tick (and once at startup); we treat a <5min-old mtime as healthy.
        let heartbeat_path = events_dir.join("last-heartbeat.json");
        let heartbeat_age = match heartbeat_path.metadata().and_then(|m| m.modified()) {
            Ok(mtime) => SystemTime::now().duration_since(mtime).ok(),
            Err(_) => None,
        };

        match heartbeat_age {
            Some(age) if age < Duration::from_secs(300) => CheckResult::pass(format!(
                "hex events daemon healthy ({} policies, heartbeat {}s old)",
                policy_count,
                age.as_secs()
            )),
            Some(age) => CheckResult::warn(format!(
                "hex events daemon stale: last heartbeat {}s old (run: hex events daemon)",
                age.as_secs()
            )),
            None => CheckResult::warn(
                "hex events daemon never started (run: hex events daemon)",
            ),
        }
    }
}
