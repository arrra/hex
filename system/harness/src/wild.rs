//! `hex ledger wild` — the S1-wild join: linter intent rows × reconciler
//! outcome rows, keyed by `gate_hash` (sha256 of the normalized gate command).
//!
//! This is the proposer's nightly feed AND the linter's charter metric in one
//! deterministic read surface. It exists so that no agent prompt ever carries
//! SQL or join logic (me/decisions/avoid-scripts-logic-in-typed-harness):
//! the ledger's only read paths are typed hex subcommands.
//!
//! # Semantics
//!
//! - **DISTINCT by gate_hash.** The reconciler currently re-appends its full
//!   join hourly (PLAN.md P1 audit debt). Dedup keeps, per gate, the outcome
//!   with the LATEST wild event time.
//! - **Event time, not row time.** `--since` filters on the outcome payload's
//!   `first_started_at` (when the gate actually ran in the wild). Row append
//!   time is useless under re-appends — every gate would look new every hour.
//!   Rows missing/unparseable `first_started_at` fall back to row `ts` with a
//!   loud stderr warning (S6).
//! - **Confusion matrix over JOINED gates only.** A gate with no intent row
//!   (pre-hook era, or dispatched outside Claude Code) appears in `gates` with
//!   `predicted: null` and is excluded from tp/fp/fn/tn.
//! - **miss** = linter `predicted == "pass"` and the gate failed in the wild
//!   (`success == false`). `summary.misses == summary.fn`.
//!
//! Output is replay-deterministic for a fixed db state: gates sort by
//! gate_hash (BTreeMap), no clock reads.

use rusqlite::{Connection, OpenFlags};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, serde::Serialize)]
pub struct WildGate {
    pub gate_hash: String,
    pub command: String,
    /// "pass" | "fail" from the linter intent row; None = never linted.
    pub predicted: Option<String>,
    pub rules_fired: Vec<String>,
    pub success: bool,
    pub final_exit_code: i64,
    pub spec_id: String,
    /// Wild event time (epoch secs) of the kept outcome for this gate.
    pub last_outcome_ts: i64,
    /// predicted == "pass" && !success — the linter's false negatives,
    /// the proposer's raw material.
    pub miss: bool,
}

#[derive(Debug, serde::Serialize)]
pub struct WildSummary {
    pub distinct_gates: usize,
    pub joined: usize,
    pub tp: i64,
    pub fp: i64,
    #[serde(rename = "fn")]
    pub fn_: i64,
    pub tn: i64,
    /// tp / (tp + fp); null when the linter has flagged nothing yet.
    pub precision: Option<f64>,
    pub misses: i64,
    /// Outcome rows skipped for unparseable payloads — surfaced, not hidden.
    pub malformed_skipped: usize,
}

#[derive(Debug, serde::Serialize)]
pub struct WildReport {
    /// The --since argument echoed verbatim (already validated by the CLI).
    pub since: Option<String>,
    pub summary: WildSummary,
    pub gates: Vec<WildGate>,
}

struct OutcomeRec {
    command: String,
    success: bool,
    final_exit_code: i64,
    spec_id: String,
    event_ts: i64,
}

/// Build the wild report. `since_epoch` (already parsed by the CLI) filters
/// on outcome EVENT time; intents are never filtered — an old prediction
/// still pairs with a newer outcome of the same gate.
pub fn wild_report(
    db: &Path,
    since_epoch: Option<i64>,
    since_echo: Option<String>,
) -> Result<WildReport, String> {
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("open {} read-only: {e}", db.display()))?;

    let mut malformed = 0usize;

    // --- outcomes: per gate_hash keep the latest wild event -----------------
    let mut outcomes: BTreeMap<String, OutcomeRec> = BTreeMap::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT id, ts, payload FROM ledger \
                 WHERE agent='reconciler' AND kind='outcome' ORDER BY id ASC",
            )
            .map_err(|e| format!("prepare outcomes: {e}"))?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?))
            })
            .map_err(|e| format!("query outcomes: {e}"))?;
        for row in rows {
            let (id, row_ts, payload) = row.map_err(|e| format!("outcome row: {e}"))?;
            let v: serde_json::Value = match serde_json::from_str(&payload) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("hex ledger wild: outcome row id={id} unparseable payload ({e}) — skipped");
                    malformed += 1;
                    continue;
                }
            };
            let (Some(gate_hash), Some(success)) =
                (v["gate_hash"].as_str(), v["success"].as_bool())
            else {
                eprintln!(
                    "hex ledger wild: outcome row id={id} missing gate_hash/success — skipped"
                );
                malformed += 1;
                continue;
            };
            let event_ts = match v["first_started_at"]
                .as_str()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            {
                Some(dt) => dt.timestamp(),
                None => {
                    eprintln!(
                        "hex ledger wild: outcome row id={id} has no parseable first_started_at — using row ts"
                    );
                    row_ts
                }
            };
            let rec = OutcomeRec {
                command: v["command"].as_str().unwrap_or("").to_string(),
                success,
                final_exit_code: v["final_exit_code"].as_i64().unwrap_or(-1),
                spec_id: v["spec_id"].as_str().unwrap_or("").to_string(),
                event_ts,
            };
            match outcomes.get(gate_hash) {
                Some(prev) if prev.event_ts >= rec.event_ts => {}
                _ => {
                    outcomes.insert(gate_hash.to_string(), rec);
                }
            }
        }
    }

    // --- since filter on the kept (latest) event per gate -------------------
    if let Some(since) = since_epoch {
        outcomes.retain(|_, rec| rec.event_ts >= since);
    }

    // --- intents: per gate_hash keep the latest prediction ------------------
    let mut intents: BTreeMap<String, (String, Vec<String>)> = BTreeMap::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT id, payload FROM ledger \
                 WHERE agent='lint-gates' AND kind='intent' ORDER BY id ASC",
            )
            .map_err(|e| format!("prepare intents: {e}"))?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| format!("query intents: {e}"))?;
        for row in rows {
            let (id, payload) = row.map_err(|e| format!("intent row: {e}"))?;
            let v: serde_json::Value = match serde_json::from_str(&payload) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("hex ledger wild: intent row id={id} unparseable payload ({e}) — skipped");
                    malformed += 1;
                    continue;
                }
            };
            let (Some(gate_hash), Some(predicted)) =
                (v["gate_hash"].as_str(), v["predicted"].as_str())
            else {
                eprintln!(
                    "hex ledger wild: intent row id={id} missing gate_hash/predicted — skipped"
                );
                malformed += 1;
                continue;
            };
            let rules = v["rules_fired"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            // ORDER BY id ASC + plain insert ⇒ the latest row wins.
            intents.insert(gate_hash.to_string(), (predicted.to_string(), rules));
        }
    }

    // --- join + classify -----------------------------------------------------
    let (mut tp, mut fp, mut fn_, mut tn) = (0i64, 0i64, 0i64, 0i64);
    let mut joined = 0usize;
    let mut gates = Vec::with_capacity(outcomes.len());
    for (gate_hash, rec) in &outcomes {
        let intent = intents.get(gate_hash);
        let (predicted, rules_fired) = match intent {
            Some((p, r)) => (Some(p.clone()), r.clone()),
            None => (None, Vec::new()),
        };
        let mut miss = false;
        if let Some(p) = predicted.as_deref() {
            joined += 1;
            match (p, rec.success) {
                ("fail", false) => tp += 1,
                ("fail", true) => fp += 1,
                ("pass", false) => {
                    fn_ += 1;
                    miss = true;
                }
                ("pass", true) => tn += 1,
                (other, _) => {
                    // Unknown vocabulary is a contract break — loud, and the
                    // gate stays out of the matrix rather than miscounted.
                    eprintln!(
                        "hex ledger wild: gate {gate_hash} has unknown predicted value '{other}' — excluded from matrix"
                    );
                    joined -= 1;
                }
            }
        }
        gates.push(WildGate {
            gate_hash: gate_hash.clone(),
            command: rec.command.clone(),
            predicted,
            rules_fired,
            success: rec.success,
            final_exit_code: rec.final_exit_code,
            spec_id: rec.spec_id.clone(),
            last_outcome_ts: rec.event_ts,
            miss,
        });
    }

    let precision = if tp + fp > 0 {
        Some(tp as f64 / (tp + fp) as f64)
    } else {
        None
    };

    Ok(WildReport {
        since: since_echo,
        summary: WildSummary {
            distinct_gates: gates.len(),
            joined,
            tp,
            fp,
            fn_,
            tn,
            precision,
            misses: fn_,
            malformed_skipped: malformed,
        },
        gates,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::Ledger;
    use serde_json::json;

    fn seed(
        l: &Ledger,
        agent: &str,
        kind: &str,
        payload: serde_json::Value,
    ) {
        l.append(agent, "verify-gate", kind, &payload).unwrap();
    }

    fn outcome(hash: &str, success: bool, started: &str) -> serde_json::Value {
        json!({
            "gate_hash": hash, "command": format!("cmd-{hash}"), "success": success,
            "final_exit_code": if success {0} else {1}, "spec_id": "S00000000",
            "task_id": "T0", "attempts": 1, "first_started_at": started,
        })
    }

    fn intent(hash: &str, predicted: &str) -> serde_json::Value {
        json!({
            "gate_hash": hash, "predicted": predicted, "rules_fired": ["r1"],
            "shadow": true, "command": format!("cmd-{hash}"),
        })
    }

    fn tmpdb() -> (tempfile::TempDir, std::path::PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("ledger.db");
        (d, p)
    }

    #[test]
    fn wild_joins_and_classifies_all_four_cells() {
        let (_d, p) = tmpdb();
        let l = Ledger::open(&p).unwrap();
        seed(&l, "lint-gates", "intent", intent("g-tp", "fail"));
        seed(&l, "lint-gates", "intent", intent("g-fp", "fail"));
        seed(&l, "lint-gates", "intent", intent("g-fn", "pass"));
        seed(&l, "lint-gates", "intent", intent("g-tn", "pass"));
        seed(&l, "reconciler", "outcome", outcome("g-tp", false, "2026-06-10T01:00:00+00:00"));
        seed(&l, "reconciler", "outcome", outcome("g-fp", true, "2026-06-10T01:00:00+00:00"));
        seed(&l, "reconciler", "outcome", outcome("g-fn", false, "2026-06-10T01:00:00+00:00"));
        seed(&l, "reconciler", "outcome", outcome("g-tn", true, "2026-06-10T01:00:00+00:00"));
        let r = wild_report(&p, None, None).unwrap();
        assert_eq!(r.summary.distinct_gates, 4);
        assert_eq!(r.summary.joined, 4);
        assert_eq!((r.summary.tp, r.summary.fp, r.summary.fn_, r.summary.tn), (1, 1, 1, 1));
        assert_eq!(r.summary.precision, Some(0.5));
        assert_eq!(r.summary.misses, 1);
        let miss_gate = r.gates.iter().find(|g| g.gate_hash == "g-fn").unwrap();
        assert!(miss_gate.miss);
    }

    #[test]
    fn wild_distinct_under_reconciler_reappends() {
        let (_d, p) = tmpdb();
        let l = Ledger::open(&p).unwrap();
        // The idempotency debt: same outcome re-appended 3 times.
        for _ in 0..3 {
            seed(&l, "reconciler", "outcome", outcome("g-dup", false, "2026-06-10T01:00:00+00:00"));
        }
        let r = wild_report(&p, None, None).unwrap();
        assert_eq!(r.summary.distinct_gates, 1);
    }

    #[test]
    fn wild_latest_event_wins_per_gate() {
        let (_d, p) = tmpdb();
        let l = Ledger::open(&p).unwrap();
        seed(&l, "reconciler", "outcome", outcome("g-re", false, "2026-06-10T01:00:00+00:00"));
        seed(&l, "reconciler", "outcome", outcome("g-re", true, "2026-06-10T05:00:00+00:00"));
        // An older re-append AFTER the newer event must not regress it.
        seed(&l, "reconciler", "outcome", outcome("g-re", false, "2026-06-10T01:00:00+00:00"));
        let r = wild_report(&p, None, None).unwrap();
        assert_eq!(r.summary.distinct_gates, 1);
        assert!(r.gates[0].success, "latest wild event (success=true) must win");
    }

    #[test]
    fn wild_since_filters_on_event_time_not_row_time() {
        let (_d, p) = tmpdb();
        let l = Ledger::open(&p).unwrap();
        // Both rows APPENDED now, but their wild events are 1h apart.
        seed(&l, "reconciler", "outcome", outcome("g-old", false, "2026-06-10T01:00:00+00:00"));
        seed(&l, "reconciler", "outcome", outcome("g-new", false, "2026-06-10T03:00:00+00:00"));
        let since = chrono::DateTime::parse_from_rfc3339("2026-06-10T02:00:00+00:00")
            .unwrap()
            .timestamp();
        let r = wild_report(&p, Some(since), Some("2026-06-10T02:00:00Z".into())).unwrap();
        assert_eq!(r.summary.distinct_gates, 1);
        assert_eq!(r.gates[0].gate_hash, "g-new");
        assert_eq!(r.since.as_deref(), Some("2026-06-10T02:00:00Z"));
    }

    #[test]
    fn wild_unjoined_gate_excluded_from_matrix() {
        let (_d, p) = tmpdb();
        let l = Ledger::open(&p).unwrap();
        seed(&l, "reconciler", "outcome", outcome("g-unseen", false, "2026-06-10T01:00:00+00:00"));
        let r = wild_report(&p, None, None).unwrap();
        assert_eq!(r.summary.distinct_gates, 1);
        assert_eq!(r.summary.joined, 0);
        assert_eq!(r.summary.fn_, 0, "unjoined failure is NOT a linter miss");
        assert!(r.gates[0].predicted.is_none());
        assert!(!r.gates[0].miss);
    }

    #[test]
    fn wild_precision_null_when_nothing_flagged() {
        let (_d, p) = tmpdb();
        let l = Ledger::open(&p).unwrap();
        seed(&l, "lint-gates", "intent", intent("g-a", "pass"));
        seed(&l, "reconciler", "outcome", outcome("g-a", true, "2026-06-10T01:00:00+00:00"));
        let r = wild_report(&p, None, None).unwrap();
        assert_eq!(r.summary.precision, None);
    }

    #[test]
    fn wild_malformed_payload_counted_not_hidden() {
        let (_d, p) = tmpdb();
        let l = Ledger::open(&p).unwrap();
        seed(&l, "reconciler", "outcome", json!({"not_a_gate": true}));
        seed(&l, "reconciler", "outcome", outcome("g-ok", true, "2026-06-10T01:00:00+00:00"));
        let r = wild_report(&p, None, None).unwrap();
        assert_eq!(r.summary.distinct_gates, 1);
        assert_eq!(r.summary.malformed_skipped, 1);
    }
}
