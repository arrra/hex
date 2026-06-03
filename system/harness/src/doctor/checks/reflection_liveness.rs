//! Folded in from the former `hex memory check-reflection-liveness` subcommand.
//! Warns if evolution/reflection-log.md hasn't been updated within 48h — a session
//! that ran recently should have produced a reflection entry. Staleness is a WARN,
//! not a hard FAIL (it indicates the reflection pipeline may be stalled, not broken).

use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};
use std::time::Duration;

const THRESHOLD: Duration = Duration::from_secs(48 * 3600);

pub struct ReflectionLogFresh;

impl DoctorCheck for ReflectionLogFresh {
    fn name(&self) -> &str { "reflection-liveness" }
    fn category(&self) -> Category { Category::Health }
    fn run(&self, ctx: &Context) -> CheckResult {
        let log_path = ctx.hex_dir.join("evolution/reflection-log.md");
        if !log_path.exists() {
            return CheckResult::warn(
                "evolution/reflection-log.md not found — run `hex session reflect` after a session",
            );
        }
        match std::fs::metadata(&log_path).and_then(|m| m.modified()) {
            Ok(modified) => {
                let elapsed = modified.elapsed().unwrap_or(Duration::MAX);
                let hours = elapsed.as_secs() / 3600;
                if elapsed > THRESHOLD {
                    CheckResult::warn(format!(
                        "reflection-log.md last updated {hours}h ago (threshold: 48h) — run `hex session reflect`"
                    ))
                } else {
                    CheckResult::pass(format!("reflection-log.md updated {hours}h ago"))
                }
            }
            Err(e) => CheckResult::warn(format!("cannot read mtime of reflection-log.md: {e}")),
        }
    }
}
