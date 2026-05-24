//! `hex memory eval` — nightly verification (spec §12). Two checks:
//!  1. Smoke-eval: each query in `.hex/memory/eval-queries.json` must surface
//!     at least one chunk whose source path contains `expect_path_contains`.
//!  2. Consumption rate: over the last 7 days of `recall-log.jsonl`, the
//!     fraction of non-gated recalls that injected must clear a floor.
//! Exits non-zero and emits `memory.eval.regression` on any failure — this is
//! what catches V1's pathology (nothing injected, nobody noticed).

use serde_json::{json, Value};
use std::path::Path;

const CONSUMPTION_FLOOR: f64 = 0.30; // ≥30% of real recalls should inject

#[derive(serde::Deserialize)]
struct EvalQuery {
    query: String,
    expect_path_contains: String,
}

pub fn run(hex_root: &Path) -> i32 {
    let mut failures: Vec<String> = Vec::new();

    // ── Check 1: smoke-eval ────────────────────────────────────────────────
    let qpath = hex_root.join(".hex/memory/eval-queries.json");
    match std::fs::read_to_string(&qpath) {
        Ok(raw) => match serde_json::from_str::<Vec<EvalQuery>>(&raw) {
            Ok(queries) => match super::open_db(&super::db_path(hex_root)) {
                Ok(conn) => {
                    for q in &queries {
                        let results = super::search::search_fts_public(&conn, &q.query, 10, None)
                            .unwrap_or_default();
                        // expect_path_contains == "__NEGATIVE__" marks an absent-topic
                        // query — it passes when nothing matches (spec §12).
                        let is_negative = q.expect_path_contains == "__NEGATIVE__";
                        let matched = results
                            .iter()
                            .any(|r| r.source_path.contains(&q.expect_path_contains));
                        let pass = if is_negative { results.is_empty() } else { matched };
                        if !pass {
                            failures.push(if is_negative {
                                format!("smoke-eval: negative query \"{}\" unexpectedly matched", q.query)
                            } else {
                                format!(
                                    "smoke-eval miss: \"{}\" expected a path containing \"{}\"",
                                    q.query, q.expect_path_contains
                                )
                            });
                        }
                    }
                    println!(
                        "smoke-eval: {} queries, {} smoke-eval failure(s)",
                        queries.len(),
                        failures.len()
                    );
                }
                // A DB that won't open invalidates every smoke-eval query — report
                // that one root cause loudly, not N misleading per-query misses.
                Err(e) => failures.push(format!(
                    "cannot open memory DB at {}: {e}",
                    super::db_path(hex_root).display()
                )),
            },
            Err(e) => failures.push(format!("eval-queries.json is malformed: {e}")),
        },
        Err(e) => failures.push(format!("eval-queries.json unreadable at {}: {e}", qpath.display())),
    }

    // ── Check 2: consumption rate ──────────────────────────────────────────
    match consumption_rate(hex_root, 7) {
        Some((rate, total)) => {
            println!("consumption rate (7d): {:.0}% over {} real recalls", rate * 100.0, total);
            if total >= 10 && rate < CONSUMPTION_FLOOR {
                failures.push(format!(
                    "consumption rate {:.0}% is below the {:.0}% floor — memory is barely \
                     being injected",
                    rate * 100.0, CONSUMPTION_FLOOR * 100.0
                ));
            }
        }
        None => println!("consumption rate: no recall-log data yet (skipped)"),
    }

    emit_result(hex_root, &failures);
    if failures.is_empty() {
        println!("hex memory eval: OK");
        0
    } else {
        for f in &failures {
            eprintln!("REGRESSION: {f}");
        }
        1
    }
}

/// Fraction of non-gated recalls in the last `days` that injected.
/// Returns (rate, non_gated_total), or None if there is no data.
fn consumption_rate(hex_root: &Path, days: i64) -> Option<(f64, usize)> {
    let log = hex_root.join(".hex/memory/recall-log.jsonl");
    let raw = std::fs::read_to_string(&log).ok()?;
    let cutoff = chrono::Utc::now() - chrono::Duration::days(days);
    let (mut injected, mut total) = (0usize, 0usize);
    for line in raw.lines() {
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let in_window = v.get("ts").and_then(|t| t.as_str())
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .is_some_and(|t| t.with_timezone(&chrono::Utc) >= cutoff);
        if !in_window || v.get("gated").and_then(|g| g.as_bool()).unwrap_or(false) {
            continue;
        }
        total += 1;
        if v.get("injected").and_then(|i| i.as_bool()).unwrap_or(false) {
            injected += 1;
        }
    }
    if total == 0 { None } else { Some((injected as f64 / total as f64, total)) }
}

fn emit_result(hex_root: &Path, failures: &[String]) {
    let bus = crate::sse::SseBus::new();
    let telemetry = std::sync::Arc::new(crate::telemetry::Telemetry::new(hex_root));
    match crate::events::EventEngine::new(hex_root, telemetry, bus) {
        Ok(engine) => {
            let (event, payload) = if failures.is_empty() {
                ("memory.eval.ok", json!({}))
            } else {
                ("memory.eval.regression", json!({ "failures": failures }))
            };
            engine.ingest(event, &payload, "hex:memory");
        }
        // S6: never swallow silently. The eval still exits non-zero and the
        // nightly policy re-emits memory.eval.regression, but a dropped result
        // event is a real fault — say so.
        Err(e) => eprintln!("[memory eval] could not emit result event: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consumption_rate_none_when_no_log() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(consumption_rate(tmp.path(), 7).is_none());
    }

    #[test]
    fn consumption_rate_ignores_gated() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join(".hex/memory");
        std::fs::create_dir_all(&dir).unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        let lines = format!(
            "{}\n{}\n{}\n",
            json!({"ts": now, "gated": true,  "injected": false}),
            json!({"ts": now, "gated": false, "injected": true}),
            json!({"ts": now, "gated": false, "injected": false}),
        );
        std::fs::write(dir.join("recall-log.jsonl"), lines).unwrap();
        let (rate, total) = consumption_rate(tmp.path(), 7).unwrap();
        assert_eq!(total, 2);          // the gated row is excluded
        assert!((rate - 0.5).abs() < 1e-9);
    }

    #[test]
    fn consumption_rate_excludes_entries_outside_the_window() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join(".hex/memory");
        std::fs::create_dir_all(&dir).unwrap();
        let old = (chrono::Utc::now() - chrono::Duration::days(10)).to_rfc3339();
        let fresh = chrono::Utc::now().to_rfc3339();
        let lines = format!(
            "{}\n{}\n",
            json!({"ts": old,   "gated": false, "injected": true}),
            json!({"ts": fresh, "gated": false, "injected": false}),
        );
        std::fs::write(dir.join("recall-log.jsonl"), lines).unwrap();
        // Only the fresh row falls inside the 7-day window; a sign-flipped
        // cutoff would wrongly count the 10-day-old (injected) row too.
        let (rate, total) = consumption_rate(tmp.path(), 7).unwrap();
        assert_eq!(total, 1);
        assert!(rate.abs() < 1e-9);
    }
}
