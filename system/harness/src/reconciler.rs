//! Reconciler — closes the loop between linter `intent` rows and real
//! per-gate outcomes recorded by BOI workers in `~/.boi/v2/boi.db`.
//!
//! Spec S253fety6, task Tdf23yg2y. Critique-v1 F1 (binding) pins the join:
//!
//! - **Per-gate join on normalized-command content hash.** The reconciler
//!   reads the `verdict` JSON of each `validate` phase_run and pulls
//!   `outcome.evidence.verifications[]` (each `{command, exit_code, name}`),
//!   then matches by `sha256(normalize(command))` against the gate hash the
//!   linter wrote into its intent rows. NEVER on spec-level status — the
//!   teardown bug marks merged-green specs failed.
//! - **First-attempt labeling.** The "footgun signal" is the FIRST failure
//!   per (spec_id, gate-hash). The reconciler records the first attempt's
//!   exit_code as the label; the final-attempt exit_code is recorded
//!   alongside but not used for the corpus.
//! - **Read-only access to boi.db.** Opened with
//!   [`OpenFlags::SQLITE_OPEN_READ_ONLY`]; any attempt to mutate is a bug.
//! - **No quiet failures.** Malformed verdict JSON is COUNTED into the
//!   run-summary `skipped` field and surfaced via the worker's heartbeat;
//!   it never silently continues. Per S6.
//!
//! The worker stub lives in `src/modules/reconciler.worker.rs`; the heavy
//! lifting is here so it is unit-testable.

use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

use crate::ledger::Ledger;

/// Normalize a verify-gate command for stable content-hashing.
///
/// Collapses runs of whitespace to a single space and trims; tolerant to
/// reformatting between dispatch and execute. The hash is over this normalized
/// form so the linter and reconciler hash the SAME thing regardless of how
/// the command was wrapped in the spec.
pub fn normalize_command(cmd: &str) -> String {
    let mut out = String::with_capacity(cmd.len());
    let mut in_space = false;
    for ch in cmd.chars() {
        if ch.is_whitespace() {
            if !in_space {
                out.push(' ');
                in_space = true;
            }
        } else {
            out.push(ch);
            in_space = false;
        }
    }
    out.trim().to_string()
}

/// Stable content hash of a verify-gate command. Linter and reconciler MUST
/// agree on the algorithm — both call this function.
pub fn gate_hash(cmd: &str) -> String {
    let mut h = Sha256::new();
    h.update(normalize_command(cmd).as_bytes());
    format!("{:x}", h.finalize())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconcileRecord {
    pub spec_id: String,
    pub task_id: Option<String>,
    pub gate_hash: String,
    pub command: String,
    pub first_exit_code: i64,
    pub final_exit_code: i64,
    pub attempts: u32,
    pub first_started_at: String,
}

/// Per-run summary surfaced via the reconciler's heartbeat payload.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ReconcileSummary {
    pub records: usize,
    pub skipped_malformed: usize,
    pub spec_runs_scanned: usize,
}

/// Read `boi.db` READ-ONLY and reconcile per-gate first-attempt outcomes.
///
/// Returns the records the caller is expected to write to the ledger as
/// `outcome` rows, plus a summary safe to emit into a heartbeat payload.
pub fn reconcile_from_boi_db<P: AsRef<Path>>(
    boi_db: P,
) -> Result<(Vec<ReconcileRecord>, ReconcileSummary), String> {
    let conn = Connection::open_with_flags(
        boi_db.as_ref(),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("reconciler: open boi.db read-only failed: {e}"))?;

    let mut stmt = conn
        .prepare(
            "SELECT spec_id, task_id, verdict, started_at \
             FROM phase_runs \
             WHERE phase = 'validate' AND verdict IS NOT NULL \
             ORDER BY spec_id, task_id, started_at ASC",
        )
        .map_err(|e| format!("reconciler: prepare select failed: {e}"))?;

    // (spec_id, gate_hash) -> ReconcileRecord (first-attempt label).
    let mut by_key: std::collections::BTreeMap<(String, String), ReconcileRecord> =
        std::collections::BTreeMap::new();

    let mut summary = ReconcileSummary::default();

    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(|e| format!("reconciler: query failed: {e}"))?;

    for r in rows {
        let (spec_id, task_id, verdict_json, started_at) =
            r.map_err(|e| format!("reconciler: row read failed: {e}"))?;
        summary.spec_runs_scanned += 1;

        let v: serde_json::Value = match serde_json::from_str(&verdict_json) {
            Ok(v) => v,
            Err(_) => {
                summary.skipped_malformed += 1;
                continue;
            }
        };

        let verifications = v
            .get("outcome")
            .and_then(|o| o.get("evidence"))
            .and_then(|e| e.get("verifications"))
            .and_then(|x| x.as_array());

        let verifications = match verifications {
            Some(a) => a,
            None => {
                // No verifications array — not a per-gate verdict shape we
                // can use. Count and move on (S6: loud — surfaced via summary).
                summary.skipped_malformed += 1;
                continue;
            }
        };

        for vv in verifications {
            let command = match vv.get("command").and_then(|x| x.as_str()) {
                Some(c) => c.to_string(),
                None => {
                    summary.skipped_malformed += 1;
                    continue;
                }
            };
            let exit_code = match vv.get("exit_code").and_then(|x| x.as_i64()) {
                Some(e) => e,
                None => {
                    summary.skipped_malformed += 1;
                    continue;
                }
            };
            let hash = gate_hash(&command);
            let key = (spec_id.clone(), hash.clone());
            by_key
                .entry(key)
                .and_modify(|rec| {
                    rec.final_exit_code = exit_code;
                    rec.attempts += 1;
                })
                .or_insert(ReconcileRecord {
                    spec_id: spec_id.clone(),
                    task_id: task_id.clone(),
                    gate_hash: hash,
                    command,
                    first_exit_code: exit_code,
                    final_exit_code: exit_code,
                    attempts: 1,
                    first_started_at: started_at.clone(),
                });
        }
    }

    let records: Vec<ReconcileRecord> = by_key.into_values().collect();
    summary.records = records.len();
    Ok((records, summary))
}

/// Write reconciler outputs into the ledger: one `outcome` row per record
/// + one `heartbeat` row with the run summary. If `records` is empty AND
/// `summary.skipped_malformed > 0`, also write an `alert` row LOUDLY (S6)
/// — silence is a bug when the source has data we couldn't parse.
pub fn write_reconcile_to_ledger(
    ledger: &Ledger,
    records: &[ReconcileRecord],
    summary: &ReconcileSummary,
) -> Result<(), String> {
    for r in records {
        let payload = serde_json::json!({
            "spec_id": r.spec_id,
            "task_id": r.task_id,
            "gate_hash": r.gate_hash,
            "command": r.command,
            "first_exit_code": r.first_exit_code,
            "final_exit_code": r.final_exit_code,
            "attempts": r.attempts,
            "first_started_at": r.first_started_at,
            "success": r.first_exit_code == 0,
        });
        ledger
            .append("reconciler", "verify-gate.outcome", "outcome", &payload)
            .map_err(|e| format!("reconciler: ledger append outcome failed: {e}"))?;
    }

    let summary_payload = serde_json::json!({
        "records": summary.records,
        "skipped_malformed": summary.skipped_malformed,
        "spec_runs_scanned": summary.spec_runs_scanned,
    });
    ledger
        .append("reconciler", "reconcile.run", "heartbeat", &summary_payload)
        .map_err(|e| format!("reconciler: ledger append heartbeat failed: {e}"))?;

    if records.is_empty() && summary.skipped_malformed > 0 {
        // LOUD alert (S6): we scanned phase_runs with verdicts but matched
        // ZERO gate outcomes — usually means a schema drift or sabotage,
        // never silence.
        let alert = serde_json::json!({
            "reason": "reconcile_no_match_with_malformed_inputs",
            "skipped_malformed": summary.skipped_malformed,
            "spec_runs_scanned": summary.spec_runs_scanned,
        });
        ledger
            .append("reconciler", "reconcile.run", "alert", &alert)
            .map_err(|e| format!("reconciler: ledger append alert failed: {e}"))?;
        eprintln!(
            "reconciler: ALERT — scanned {} phase_runs, matched 0 gates, {} malformed",
            summary.spec_runs_scanned, summary.skipped_malformed
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Unit tests — fixture pair (clean + sabotage) per critique-v1 F3.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::path::PathBuf;

    fn make_boi_fixture(path: &PathBuf, verdict_json: &str) {
        let conn = Connection::open(path).expect("open boi fixture");
        conn.execute_batch(
            "CREATE TABLE phase_runs (
                id TEXT PRIMARY KEY,
                spec_id TEXT NOT NULL,
                task_id TEXT NULL,
                phase TEXT NOT NULL,
                phase_iteration INTEGER NOT NULL,
                spec_version INTEGER NOT NULL,
                provider TEXT NOT NULL,
                worker_id TEXT NULL,
                files_touched TEXT NOT NULL DEFAULT '[]',
                synopsis TEXT NOT NULL DEFAULT '',
                verdict TEXT NULL,
                last_heartbeat_at TEXT NULL,
                started_at TEXT NOT NULL,
                completed_at TEXT NULL,
                tokens_in INTEGER,
                tokens_out INTEGER
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO phase_runs (id, spec_id, task_id, phase, phase_iteration, spec_version, \
              provider, verdict, started_at) \
             VALUES ('Pxxxxxxx', 'S123', 'T001', 'validate', 0, 1, 'claude_code', ?1, '2026-06-10T10:00:00Z');",
            [verdict_json],
        )
        .unwrap();
    }

    #[test]
    fn reconciler_normalize_command_collapses_whitespace() {
        assert_eq!(
            normalize_command("  echo   hello\tworld\n "),
            "echo hello world"
        );
    }

    #[test]
    fn reconciler_gate_hash_is_stable_across_whitespace() {
        assert_eq!(gate_hash("echo hello"), gate_hash("  echo   hello "));
        assert_ne!(gate_hash("echo hello"), gate_hash("echo goodbye"));
    }

    #[test]
    fn reconciler_clean_fixture_reconciles_to_expected_rows() {
        // Clean: known verdict JSON with two verifications (one pass, one fail).
        let dir = tempfile::tempdir().unwrap();
        let boi_path = dir.path().join("boi.db");
        let verdict = serde_json::json!({
            "outcome": {
                "type": "passing",
                "evidence": {
                    "files_touched": [],
                    "summary": "ok",
                    "verifications": [
                        {"command": "grep -q foo README.md", "exit_code": 0, "level": "l2", "name": null},
                        {"command": "test -f does/not/exist", "exit_code": 1, "level": "l2", "name": null}
                    ]
                }
            },
            "synopsis": "validate"
        })
        .to_string();
        make_boi_fixture(&boi_path, &verdict);

        let (records, summary) = reconcile_from_boi_db(&boi_path).expect("reconcile");
        assert_eq!(records.len(), 2, "two gates → two records");
        assert_eq!(summary.records, 2);
        assert_eq!(summary.skipped_malformed, 0);

        let h_ok = gate_hash("grep -q foo README.md");
        let h_fail = gate_hash("test -f does/not/exist");
        let by_hash: std::collections::HashMap<_, _> = records
            .iter()
            .map(|r| (r.gate_hash.clone(), r))
            .collect();
        assert_eq!(by_hash.get(&h_ok).unwrap().first_exit_code, 0);
        assert_eq!(by_hash.get(&h_fail).unwrap().first_exit_code, 1);
    }

    #[test]
    fn reconciler_sabotage_fixture_produces_alert_not_silence() {
        // Sabotage: verdict is well-formed JSON but has the WRONG SHAPE —
        // no outcome.evidence.verifications array. The reconciler must
        // count this as malformed and (when no records were matched) write
        // a LOUD alert row, not silently continue.
        let dir = tempfile::tempdir().unwrap();
        let boi_path = dir.path().join("boi.db");
        // Wrong shape — `verifications` is at the top, not nested correctly.
        let verdict = serde_json::json!({
            "verifications": [{"command": "x", "exit_code": 0}]
        })
        .to_string();
        make_boi_fixture(&boi_path, &verdict);

        let (records, summary) = reconcile_from_boi_db(&boi_path).expect("reconcile");
        assert!(records.is_empty(), "wrong-shape verdict yields no records");
        assert!(
            summary.skipped_malformed >= 1,
            "wrong-shape verdict must count as skipped_malformed, got {:?}",
            summary
        );

        // Now drive the write path against an empty ledger: an alert row
        // MUST land. The clean test (above) writes outcomes; this one writes
        // ONLY a heartbeat + an alert.
        let ledger_path = dir.path().join("ledger.db");
        let ledger = Ledger::open(&ledger_path).expect("open ledger");
        write_reconcile_to_ledger(&ledger, &records, &summary).expect("write");

        // Count alert rows in the ledger.
        let conn = Connection::open(&ledger_path).unwrap();
        let alert_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM ledger WHERE kind = 'alert'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            alert_count, 1,
            "sabotage fixture must produce a loud alert row, not silence"
        );
    }

    #[test]
    fn reconciler_first_attempt_label_wins_over_final() {
        // Same command, two phase_runs — first FAIL, second PASS.
        // First-attempt label MUST be 1 (the footgun signal); final_exit_code
        // tracks the eventual outcome but is not the label.
        let dir = tempfile::tempdir().unwrap();
        let boi_path = dir.path().join("boi.db");
        let conn = Connection::open(&boi_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE phase_runs (
                id TEXT PRIMARY KEY, spec_id TEXT NOT NULL, task_id TEXT NULL,
                phase TEXT NOT NULL, phase_iteration INTEGER NOT NULL,
                spec_version INTEGER NOT NULL, provider TEXT NOT NULL,
                worker_id TEXT NULL, files_touched TEXT NOT NULL DEFAULT '[]',
                synopsis TEXT NOT NULL DEFAULT '', verdict TEXT NULL,
                last_heartbeat_at TEXT NULL, started_at TEXT NOT NULL,
                completed_at TEXT NULL, tokens_in INTEGER, tokens_out INTEGER
            );",
        )
        .unwrap();
        let mk = |exit: i64| {
            serde_json::json!({
                "outcome": {"type":"passing","evidence":{"files_touched":[],"summary":"",
                    "verifications":[{"command":"echo same-gate","exit_code":exit,"level":"l2","name":null}]}},
                "synopsis":""
            })
            .to_string()
        };
        conn.execute(
            "INSERT INTO phase_runs (id, spec_id, task_id, phase, phase_iteration, spec_version, \
              provider, verdict, started_at) VALUES \
              ('P00000001','S1','T1','validate',0,1,'claude_code',?1,'2026-06-10T10:00:00Z'),\
              ('P00000002','S1','T1','validate',1,1,'claude_code',?2,'2026-06-10T10:05:00Z');",
            rusqlite::params![mk(1), mk(0)],
        )
        .unwrap();

        let (records, _summary) = reconcile_from_boi_db(&boi_path).expect("reconcile");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].first_exit_code, 1, "first attempt FAIL is the label");
        assert_eq!(records[0].final_exit_code, 0, "final attempt PASS is tracked alongside");
        assert_eq!(records[0].attempts, 2);
    }
}
