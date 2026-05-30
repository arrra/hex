use crate::types::{ClaudeOutput, Cost};
use chrono::Utc;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

pub fn record_invocation(cost: &mut Cost, output: &ClaudeOutput) {
    let usd = output.total_cost_usd;
    cost.last_wake_usd += usd;
    cost.current_period.spent_usd += usd;
    cost.lifetime_usd += usd;
}

pub fn append_ledger(ledger_dir: &Path, agent_id: &str, output: &ClaudeOutput) {
    let path = ledger_dir.join("ledger.jsonl");
    if let Err(e) = fs::create_dir_all(ledger_dir) {
        eprintln!(
            "COST LEDGER FAILED: cannot create {}: {e}",
            ledger_dir.display()
        );
        return;
    }
    let entry = serde_json::json!({
        "ts": Utc::now().to_rfc3339(),
        "agent": agent_id,
        "cost_usd": output.total_cost_usd,
        "input_tokens": output.usage.input_tokens,
        "output_tokens": output.usage.output_tokens,
        "cache_read_tokens": output.usage.cache_read_input_tokens,
        "cache_creation_tokens": output.usage.cache_creation_input_tokens,
        "duration_ms": output.duration_ms,
    });
    let line = match serde_json::to_string(&entry) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("COST SERIALIZE FAILED: {e}");
            return;
        }
    };
    match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(mut file) => {
            if let Err(e) = writeln!(file, "{}", line) {
                eprintln!(
                    "COST LEDGER FAILED: cannot write to {}: {e}",
                    path.display()
                );
            }
        }
        Err(e) => {
            eprintln!("COST LEDGER FAILED: cannot open {}: {e}", path.display());
        }
    }
}

pub fn shift_budget_remaining(cost: &Cost, per_shift: f64) -> f64 {
    per_shift - cost.last_wake_usd
}

/// Returns `true` when the agent's current-period spend has met or exceeded
/// its period cap, so the next `claude::invoke` in the wake loop must be
/// skipped. A `budget_usd` of `0.0` means "unlimited" — convention shared
/// with `shift_budget_remaining` — and never returns `true`.
///
/// The wake loop checks this AFTER the shift gate so per-wake (shift) and
/// per-period both backstop overspend. Without this, a single wake could
/// burn 100% of the period budget before any update landed (releaser
/// budget-LARP: 2026-05-24 single wake = $24.14 vs $10 cap).
pub fn period_budget_exhausted(cost: &Cost) -> bool {
    cost.current_period.budget_usd > 0.0
        && cost.current_period.spent_usd >= cost.current_period.budget_usd
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::CostPeriod;
    use chrono::Utc;

    /// Build a `Cost` with the supplied period budget + spend. Other fields
    /// don't influence the helper — they're filler.
    fn cost_with_period(budget_usd: f64, spent_usd: f64) -> Cost {
        Cost {
            lifetime_usd: spent_usd,
            current_period: CostPeriod {
                start: Utc::now(),
                spent_usd,
                budget_usd,
            },
            last_wake_usd: 0.0,
        }
    }

    #[test]
    fn period_budget_exhausted_returns_false_when_budget_is_zero() {
        // `budget_usd == 0.0` is the "unlimited" sentinel — must never
        // exhaust, even with non-zero spend.
        let cost = cost_with_period(0.0, 100.0);
        assert!(
            !period_budget_exhausted(&cost),
            "budget_usd=0 means unlimited; gate must never fire"
        );
    }

    #[test]
    fn period_budget_exhausted_returns_false_when_under_cap() {
        let cost = cost_with_period(10.0, 4.99);
        assert!(
            !period_budget_exhausted(&cost),
            "under-cap spend must not exhaust"
        );
    }

    #[test]
    fn period_budget_exhausted_returns_true_at_cap() {
        // Exact equality counts as exhausted — the next invocation would
        // push over.
        let cost = cost_with_period(10.0, 10.0);
        assert!(
            period_budget_exhausted(&cost),
            "spent == budget must be treated as exhausted"
        );
    }

    #[test]
    fn period_budget_exhausted_returns_true_over_cap() {
        // The releaser case — a single wake already burned past the cap.
        let cost = cost_with_period(10.0, 24.14);
        assert!(
            period_budget_exhausted(&cost),
            "over-cap spend must exhaust (releaser 2026-05-24: $24.14 vs $10)"
        );
    }
}
