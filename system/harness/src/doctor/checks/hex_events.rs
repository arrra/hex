use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};

/// check_16: hex-events daemon (hex_eventd.py) is reachable.
pub struct HexEventsReachable;

impl DoctorCheck for HexEventsReachable {
    fn name(&self) -> &str { "hex-events-reachable" }
    fn category(&self) -> Category { Category::Health }
    fn run(&self, ctx: &Context) -> CheckResult {
        let home = &ctx.home;
        let events_dir = home.join(".hex-events");

        if !events_dir.is_dir() {
            return CheckResult::warn("~/.hex-events/ directory not found");
        }

        // Check for the daemon script
        let daemon = events_dir.join("hex_eventd.py");
        if !daemon.is_file() {
            return CheckResult::warn("~/.hex-events/hex_eventd.py not found");
        }

        // Check venv
        let venv = events_dir.join("venv");
        if !venv.is_dir() {
            return CheckResult::warn("~/.hex-events/venv not found — run hex-events setup");
        }

        CheckResult::pass("hex-events reachable (daemon + venv present)")
    }
}
