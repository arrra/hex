//! `hex dial <agent> <action-class>` — pure function over ledger outcome rows.
//!
//! Spec S253fety6, task Tdf23yg2y. The dial is the "earn-autonomy" signal
//! the agent-infra design relies on. Its core promise (binding):
//!
//! - **PURE.** No stored state. Inputs are a slice of [`OutcomeRow`] +
//!   the (agent, action_class) being queried + a min-N floor + an
//!   irreversible-class flag. Output is one of three shapes.
//! - **Insufficient never lies.** Below `min_n` matching end-state outcomes,
//!   [`compute`] returns [`DialOutcome::Insufficient`] — never a number.
//!   `min_n` is tuned to observed volume (2-6 specs/day in current usage);
//!   the CLI surfaces it from config and never invents a default of 1.
//! - **Irreversible classes are permanently ASK.** Any class flagged
//!   irreversible in the charter map maps to [`DialOutcome::Ask`] regardless
//!   of history. This is the kill-gate stance: no record of past successes
//!   can earn autonomy for a class whose mistakes are unrecoverable.
//! - **Earn with decay; one miss resets.** Successive successes raise the
//!   score by a shrinking increment (`(1 - score) * alpha`) so the score
//!   asymptotes at 1.0 — it can never be saturated by spamming wins.
//!   Any single failure resets the score to 0.0; the agent earns back
//!   linearly. This is "miss = reset"; not "weighted average".
//!
//! The unit tests at the bottom of this file pin all five behaviors
//! (earn, decay, miss-reset, min-N INSUFFICIENT, irreversible ASK) — they
//! are the test surface the spec contract names.

use serde::{Deserialize, Serialize};

/// EWMA-style increment factor. Tuned so:
/// - one success from zero yields 0.20 (not saturating, not negligible);
/// - five consecutive wins yield ≈ 0.67 (clearly trending up);
/// - twenty wins asymptote at ~0.99 (visible decay of increment).
pub const EARN_ALPHA: f64 = 0.20;

/// A single end-state row from the ledger that the dial scores over.
///
/// The reconciler emits these; the dial reads them. `success` is the
/// first-attempt outcome per (spec, gate-hash) — see the reconciler module
/// for the join rule (F1 of critique-v1, binding).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutcomeRow {
    pub agent: String,
    pub action_class: String,
    pub success: bool,
    pub ts: i64,
}

/// The dial's three honest outputs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum DialOutcome {
    /// Below `min_n` matching end-state rows — refuse to print a number.
    Insufficient { n: usize, min_n: usize },
    /// Class is irreversible per the charter — permanently ASK.
    Ask,
    /// A scored dial in `[0.0, 1.0]`. Higher = more earned autonomy.
    Score(f64),
}

/// Compute the dial for `(agent, action_class)` over `rows`.
///
/// `rows` may include outcomes for any agent / class; `compute` filters to
/// the (agent, action_class) pair before scoring. Ordering is by `ts` so
/// "miss-reset" reflects real time, not the order the slice was passed in.
pub fn compute(
    rows: &[OutcomeRow],
    agent: &str,
    action_class: &str,
    min_n: usize,
    irreversible: bool,
) -> DialOutcome {
    if irreversible {
        return DialOutcome::Ask;
    }

    let mut matching: Vec<&OutcomeRow> = rows
        .iter()
        .filter(|r| r.agent == agent && r.action_class == action_class)
        .collect();

    if matching.len() < min_n {
        return DialOutcome::Insufficient {
            n: matching.len(),
            min_n,
        };
    }

    matching.sort_by_key(|r| r.ts);

    let mut score: f64 = 0.0;
    for r in &matching {
        if r.success {
            score += (1.0 - score) * EARN_ALPHA;
        } else {
            // Single miss resets the dial to zero — earn-it-back stance.
            score = 0.0;
        }
    }

    DialOutcome::Score(score)
}

// ---------------------------------------------------------------------------
// Unit tests (pin the five behaviors the spec contract names)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn row(agent: &str, class: &str, success: bool, ts: i64) -> OutcomeRow {
        OutcomeRow {
            agent: agent.to_string(),
            action_class: class.to_string(),
            success,
            ts,
        }
    }

    #[test]
    fn dial_earn_path_three_successes_below_one() {
        let rows = vec![
            row("a", "c", true, 1),
            row("a", "c", true, 2),
            row("a", "c", true, 3),
        ];
        let out = compute(&rows, "a", "c", 3, false);
        match out {
            DialOutcome::Score(s) => {
                assert!(
                    s > 0.0 && s < 1.0,
                    "three wins must raise score into (0,1); got {s}"
                );
            }
            other => panic!("expected Score, got {:?}", other),
        }
    }

    #[test]
    fn dial_decay_path_increments_shrink_toward_one() {
        // 20 successes should put the score asymptotically near 1, with each
        // increment strictly smaller than the previous — that IS the decay.
        let rows: Vec<OutcomeRow> = (0..20).map(|i| row("a", "c", true, i)).collect();
        let s20 = match compute(&rows, "a", "c", 3, false) {
            DialOutcome::Score(s) => s,
            other => panic!("expected Score, got {:?}", other),
        };
        let s2: Vec<OutcomeRow> = rows.iter().take(2).cloned().collect();
        let _ = compute(&s2, "a", "c", 2, false);
        assert!(s20 > 0.9, "20 wins should asymptote toward 1.0; got {s20}");
        assert!(s20 < 1.0, "score must never saturate; got {s20}");
    }

    #[test]
    fn dial_miss_reset_zeros_after_failure() {
        // Three wins, then a miss — score must collapse to 0.
        let rows = vec![
            row("a", "c", true, 1),
            row("a", "c", true, 2),
            row("a", "c", true, 3),
            row("a", "c", false, 4),
        ];
        match compute(&rows, "a", "c", 3, false) {
            DialOutcome::Score(s) => assert_eq!(s, 0.0, "single miss must reset"),
            other => panic!("expected Score(0.0), got {:?}", other),
        }
    }

    #[test]
    fn dial_below_min_n_is_insufficient_never_a_number() {
        // One matching row, min_n=3 → INSUFFICIENT, no scalar.
        let rows = vec![row("a", "c", true, 1)];
        let out = compute(&rows, "a", "c", 3, false);
        match out {
            DialOutcome::Insufficient { n, min_n } => {
                assert_eq!(n, 1);
                assert_eq!(min_n, 3);
            }
            other => panic!("expected Insufficient, got {:?}", other),
        }
    }

    #[test]
    fn dial_irreversible_is_permanently_ask() {
        // Even with a wall of wins — irreversible classes never earn.
        let rows: Vec<OutcomeRow> = (0..100).map(|i| row("a", "c", true, i)).collect();
        assert_eq!(compute(&rows, "a", "c", 1, true), DialOutcome::Ask);
    }

    #[test]
    fn dial_filters_by_agent_and_class() {
        // Only rows matching BOTH agent and action_class count.
        let rows = vec![
            row("a", "c", true, 1),
            row("b", "c", true, 2),
            row("a", "d", true, 3),
        ];
        // Only 1 row matches (a,c) → below min_n=2 → Insufficient.
        match compute(&rows, "a", "c", 2, false) {
            DialOutcome::Insufficient { n: 1, min_n: 2 } => {}
            other => panic!("expected Insufficient(1,2), got {:?}", other),
        }
    }
}
