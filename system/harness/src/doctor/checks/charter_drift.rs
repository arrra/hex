use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};

/// Ledger-anchored charter governance: every registered charter's on-disk
/// sha256 must match its latest `charter.governance` row. Drift = an
/// out-of-band edit (decision: charter-mechanics-ledger-anchored-2026-06-11).
/// No registered charters = pass (nothing governed yet, nothing to verify).
pub struct CharterDrift;

impl DoctorCheck for CharterDrift {
    fn name(&self) -> &str { "charter-drift" }
    fn category(&self) -> Category { Category::Config }
    fn run(&self, ctx: &Context) -> CheckResult {
        match crate::charter::verify(&ctx.hex_dir, false) {
            Ok(drifts) if drifts.is_empty() => {
                let n = crate::charter::latest_states(&ctx.hex_dir)
                    .map(|m| m.len())
                    .unwrap_or(0);
                CheckResult::pass(format!("{n} registered charter(s), no drift"))
            }
            Ok(drifts) => CheckResult::fail(format!(
                "{} charter(s) DRIFTED (out-of-band edit): {} — restore recorded \
                 content or `hex charter rebaseline`",
                drifts.len(),
                drifts
                    .iter()
                    .map(|d| d.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            Err(e) => CheckResult::fail(format!("charter verify errored: {e}")),
        }
    }
}
