pub mod cap;
pub mod dedup;
pub mod extract;
pub mod judge;
pub mod watermark;

use crate::memory::predicates;
use crate::memory::provider::ProviderError;
use extract::Candidate;
use rusqlite::Connection;
use ulid::Ulid;

#[derive(Default, Debug)]
pub struct DistillReport {
    pub adds: u32,
    pub updates: u32,
    pub noops: u32,
    pub flags: u32,
}

/// Hard floor for the budget bisection. Cause-agnostic — handles output
/// truncation and content-filter rejections by shrinking the input slice.
const BUDGET_FLOOR_TOKENS: u32 = 2_000;
/// Default input cap when no llm_config override is present.
const DEFAULT_INPUT_BUDGET_TOKENS: u32 = 48_000;
/// Consecutive failures at which a slice is loudly skipped (poison-slice
/// escape hatch). The cron is the loop — the next tick takes the next slice.
const STRIKE_LIMIT: u32 = 3;

/// Emit a `distill::slice` telemetry event. Observational; never fails the
/// caller (see `telemetry::record_loud`).
fn telemetry_slice(
    path: &str,
    start_offset: i64,
    bytes: i64,
    est_tokens: u32,
    outcome: &str,
    strikes: u32,
) {
    crate::telemetry::record_loud(&crate::telemetry::TelemetryEvent {
        source: "memory::distill".into(),
        event: "distill::slice".into(),
        status: outcome.into(),
        duration_ms: None,
        exit_code: None,
        detail: Some(format!(
            "path={} offset={} bytes={} est_tokens={} strikes={}",
            path, start_offset, bytes, est_tokens, strikes
        )),
    });
}

/// Run extract on the supplied span. The `HEX_DISTILL_FORCE_EXTRACT_FAIL`
/// env-var is a test seam: when set, returns a deterministic `Upstream`
/// failure without any network call. This is how the strike-escalation tests
/// run without an API key.
fn extract_or_forced_fail(span: &str) -> Result<Vec<Candidate>, ProviderError> {
    if std::env::var("HEX_DISTILL_FORCE_EXTRACT_FAIL")
        .ok()
        .filter(|v| !v.is_empty())
        .is_some()
    {
        return Err(ProviderError::Upstream(
            "forced extract failure (HEX_DISTILL_FORCE_EXTRACT_FAIL test seam)".to_string(),
        ));
    }
    extract::extract_from_span(span)
}

pub fn run_on_file(
    conn: &mut Connection,
    path: &str,
    min_tokens: usize,
) -> anyhow::Result<DistillReport> {
    let mut report = DistillReport::default();
    let full = std::fs::read_to_string(path)?;
    let len_i64 = full.len() as i64;

    // --- Resume hardening ---
    let mut offset = watermark::last_offset(conn, path)?;
    if offset > len_i64 {
        eprintln!(
            "[distill] resume reset: stored offset {} > file length {} for {} — restarting at 0",
            offset, len_i64, path
        );
        offset = 0;
    }
    if offset > 0 && !full.is_char_boundary(offset as usize) {
        let mut o = offset as usize;
        while o > 0 && !full.is_char_boundary(o) {
            o -= 1;
        }
        eprintln!(
            "[distill] resume reset: stored offset {} not a UTF-8 boundary for {} — rounding down to {}",
            offset, path, o
        );
        offset = o as i64;
    }
    if offset >= len_i64 {
        return Ok(report);
    }
    // SAFETY(string_slice): `offset` was rounded down to a char boundary above
    // (the is_char_boundary loop) and `offset >= len_i64` returned early, so it
    // is a valid char boundary within `full`.
    #[allow(clippy::string_slice)]
    let span = &full[offset as usize..];

    // Small-delta short-circuit (semantics unchanged: a span under min_tokens
    // produces exactly zero calls). Whole-span tokens check.
    if span.split_whitespace().count() < min_tokens {
        return Ok(report);
    }

    // --- Budget resolution (strike-aware bisection) ---
    let base_budget = crate::llm_config::resolve("memory_extract")
        .ok()
        .and_then(|c| c.max_input_tokens)
        .unwrap_or(DEFAULT_INPUT_BUDGET_TOKENS);
    let strikes = watermark::strikes(conn, path)?;
    let shift = strikes.min(20);
    let budget = (base_budget >> shift).max(BUDGET_FLOOR_TOKENS);

    // --- Cap the slice ---
    let cap_len = cap::cap_span(span, budget);
    let slice_end_offset = offset + cap_len as i64;
    let est_tokens = ((cap_len as f64) / 3.5).ceil() as u32;
    if cap_len < span.len() {
        eprintln!(
            "[distill] partial slice: file={} slice_bytes={} est_tokens={} bytes_remaining={} budget_tokens={} strikes={}",
            path,
            cap_len,
            est_tokens,
            span.len() - cap_len,
            budget,
            strikes
        );
    }
    // SAFETY(string_slice): `cap::cap_span` returns a char-boundary byte length
    // (either a boundary from char_indices, an index right after an ASCII '\n',
    // `span.len()`, or 0) — never a mid-char offset.
    #[allow(clippy::string_slice)]
    let slice = &span[..cap_len];

    // --- Extract (with deterministic test seam) ---
    let candidates = match extract_or_forced_fail(slice) {
        Ok(c) => c,
        Err(e) => {
            let new_strikes = strikes + 1;
            if new_strikes >= STRIKE_LIMIT {
                // Poison-slice escape hatch: advance past the slice LOUDLY so
                // the cron is not trapped retrying forever.
                eprintln!(
                    "[distill] POISON SLICE SKIP: file={} bytes={}..{} strikes={} budget_tokens={} reason={}",
                    path, offset, slice_end_offset, new_strikes, budget, e
                );
                telemetry_slice(
                    path,
                    offset,
                    cap_len as i64,
                    est_tokens,
                    "skipped",
                    new_strikes,
                );
                let tx = conn.transaction()?;
                watermark::advance_offset(&tx, path, slice_end_offset)?;
                watermark::set_strikes(&tx, path, 0)?;
                tx.commit()?;
            } else {
                eprintln!(
                    "[distill] extract failed (strike {} of {}): file={} budget_tokens={} err={}",
                    new_strikes, STRIKE_LIMIT, path, budget, e
                );
                telemetry_slice(
                    path,
                    offset,
                    cap_len as i64,
                    est_tokens,
                    "failed",
                    new_strikes,
                );
                watermark::set_strikes(conn, path, new_strikes)?;
            }
            return Ok(report);
        }
    };

    let new_offset = slice_end_offset;
    let tx = conn.transaction()?;
    for c in candidates {
        let (pred, obj) = if predicates::validate(&c.predicate).is_ok() {
            (c.predicate.clone(), c.object.clone())
        } else {
            (
                "_unmapped".to_string(),
                format!("{}: {}", c.predicate, c.object),
            )
        };

        let effective = Candidate {
            predicate: pred.clone(),
            object: obj.clone(),
            ..c.clone()
        };
        let outcome = dedup::classify(&tx, &effective, None)?;
        match outcome {
            dedup::DedupOutcome::Noop { existing_id } => {
                report.noops += 1;
                tx.execute(
                    "UPDATE facts SET access_count=access_count+1, last_accessed=datetime('now') WHERE id=?1",
                    [existing_id],
                )?;
            }
            dedup::DedupOutcome::CleanAdd => {
                let id = Ulid::new().to_string();
                tx.execute(
                    "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at,source_ref)
                     VALUES (?1,?2,?3,?4,?5,datetime('now'),datetime('now'),?6)",
                    rusqlite::params![id, c.subject, pred, obj, c.importance, path],
                )?;
                tx.execute(
                    "INSERT INTO fact_history (fact_id,op,new_value,ts) VALUES (?1,'ADD',?2,datetime('now'))",
                    rusqlite::params![id, obj],
                )?;
                report.adds += 1;
            }
            dedup::DedupOutcome::Ambiguous { nearest_ids } => {
                let existing: Vec<(String, String, String, String)> = nearest_ids
                    .iter()
                    .filter_map(|nid| {
                        tx.query_row(
                            "SELECT id,subject,predicate,object FROM facts WHERE id=?1",
                            [nid],
                            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                        )
                        .ok()
                    })
                    .collect();
                let decision = match judge::judge(&c.subject, &pred, &obj, "", &existing) {
                    Ok(d) => d,
                    Err(e) => {
                        // Judge ProviderError MUST NOT discard the slice's
                        // extract work. Record a FLAG row, log loudly, and
                        // continue processing remaining candidates.
                        eprintln!(
                            "[distill] judge failed for ({},{},{}) in {}: {} — recording FLAG and continuing",
                            c.subject, pred, obj, path, e
                        );
                        tx.execute(
                            "INSERT INTO fact_history (fact_id,op,new_value,ts) VALUES (?1,'FLAG',?2,datetime('now'))",
                            rusqlite::params![
                                "",
                                format!(
                                    "judge-error on ({},{},{}): {}",
                                    c.subject, pred, obj, e
                                )
                            ],
                        )?;
                        report.flags += 1;
                        continue;
                    }
                };
                match decision.action {
                    judge::Action::Add => {
                        let id = Ulid::new().to_string();
                        tx.execute(
                            "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at,source_ref)
                             VALUES (?1,?2,?3,?4,?5,datetime('now'),datetime('now'),?6)",
                            rusqlite::params![id, c.subject, pred, obj, c.importance, path],
                        )?;
                        tx.execute(
                            "INSERT INTO fact_history (fact_id,op,new_value,ts) VALUES (?1,'ADD',?2,datetime('now'))",
                            rusqlite::params![id, obj],
                        )?;
                        report.adds += 1;
                    }
                    judge::Action::Update => {
                        if let Some(tid) = decision.target_id {
                            let prev: String = tx.query_row(
                                "SELECT object FROM facts WHERE id=?1",
                                [&tid],
                                |r| r.get(0),
                            )?;
                            tx.execute(
                                "UPDATE facts SET object=?1, updated_at=datetime('now') WHERE id=?2",
                                rusqlite::params![obj, tid],
                            )?;
                            tx.execute(
                                "INSERT INTO fact_history (fact_id,op,prev_value,new_value,ts) VALUES (?1,'UPDATE',?2,?3,datetime('now'))",
                                rusqlite::params![tid, prev, obj],
                            )?;
                            report.updates += 1;
                        }
                    }
                    judge::Action::Noop => {
                        report.noops += 1;
                    }
                    judge::Action::Flag => {
                        let tid = decision.target_id.clone().unwrap_or_default();
                        tx.execute(
                            "INSERT INTO fact_history (fact_id,op,new_value,ts) VALUES (?1,'FLAG',?2,datetime('now'))",
                            rusqlite::params![
                                tid,
                                format!(
                                    "contested by: ({},{},{}) — {}",
                                    c.subject, pred, obj, decision.reason
                                )
                            ],
                        )?;
                        report.flags += 1;
                    }
                }
            }
        }
    }
    watermark::advance_offset(&tx, path, new_offset)?;
    watermark::set_strikes(&tx, path, 0)?;
    tx.commit()?;
    telemetry_slice(path, offset, cap_len as i64, est_tokens, "ok", 0);
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn fixture_conn() -> Connection {
        crate::memory::vector::register_sqlite_vec();
        let conn = Connection::open_in_memory().unwrap();
        crate::memory::schema::apply_plan1_baseline_for_test(&conn).unwrap();
        crate::memory::schema::apply_plan2(&conn).unwrap();
        conn
    }

    /// Red test for Te4qev9ed: strike escalation + 3-strike loud skip.
    ///
    /// When the extract step deterministically fails for a slice, run_on_file
    /// must:
    ///   1. Not advance the watermark on the first two failures (strikes 1, 2)
    ///      — leave room for the next cron tick to retry with halved budget.
    ///   2. After the third consecutive failure (at the budget floor), LOUDLY
    ///      skip the slice by advancing the watermark past it, so a poisoned
    ///      slice cannot wedge the file forever.
    ///
    /// The test uses a process-local env-var seam (`HEX_DISTILL_FORCE_EXTRACT_FAIL`)
    /// that the implementation must honor to make extract deterministically
    /// fail without any network. Cap.rs already exists; the test does NOT
    /// require a live LLM.
    #[test]
    fn deterministically_failing_slice_skipped_after_three_strikes() {
        let _guard = crate::telemetry::test_support::lock_env();

        // Isolated HEX_DIR with a stub extract prompt so extract gets past the
        // template read. No OPENROUTER_API_KEY set → live provider would
        // return Deferred, but the impl-under-test must instead consult the
        // forced-fail seam below.
        let td = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(td.path().join(".hex/memory/prompts")).unwrap();
        std::fs::write(
            td.path().join(".hex/memory/prompts/extract.txt"),
            "extract predicates: {{PREDICATES}}\n",
        )
        .unwrap();
        std::env::set_var("HEX_DIR", td.path());
        std::env::remove_var("OPENROUTER_API_KEY");
        std::env::set_var("HEX_DISTILL_FORCE_EXTRACT_FAIL", "1");

        // A transcript file with enough content to exceed min_tokens and to be
        // sliced. Content is intentionally non-empty so the cap+slice path is
        // exercised (rather than the small-delta short-circuit).
        let mut transcript = String::new();
        for _ in 0..200 {
            transcript
                .push_str("This is a line of transcript content with several tokens in it.\n");
        }
        let file = td.path().join("transcript.md");
        std::fs::write(&file, &transcript).unwrap();
        let path = file.to_str().unwrap();

        let mut conn = fixture_conn();
        let min_tokens = 5usize;

        // Strikes 1 & 2: watermark must remain at 0 (retry next tick).
        let _ = run_on_file(&mut conn, path, min_tokens);
        let offset_after_1 = watermark::last_offset(&conn, path).unwrap();
        assert_eq!(
            offset_after_1, 0,
            "strike 1: watermark must not advance on extract failure"
        );

        let _ = run_on_file(&mut conn, path, min_tokens);
        let offset_after_2 = watermark::last_offset(&conn, path).unwrap();
        assert_eq!(
            offset_after_2, 0,
            "strike 2: watermark must not advance on extract failure"
        );

        // Strike 3: at floor → must LOUDLY SKIP the slice by advancing the
        // watermark past it (poison-slice escape hatch).
        let _ = run_on_file(&mut conn, path, min_tokens);
        let offset_after_3 = watermark::last_offset(&conn, path).unwrap();
        assert!(
            offset_after_3 > 0,
            "strike 3: watermark MUST advance past the poisoned slice \
             (got {}) — otherwise the cron retries forever",
            offset_after_3
        );

        std::env::remove_var("HEX_DISTILL_FORCE_EXTRACT_FAIL");
        std::env::remove_var("HEX_DIR");
    }
}
