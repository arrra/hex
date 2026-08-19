//! `hex-climber-digest` — weekly mechanical digest of the auto-tune loop
//! (hill-climber stage 1, spec Tygya6q1s).
//!
//! Every Monday 06:00 UTC this writes `$HEX_DIR/evolution/digests/YYYY-WXX.md`
//! (ISO week) from PURELY mechanical reads of the loop's own tables — NO LLM
//! calls in v1 (spec exclusion). The digest is the conversion fix the design
//! doc names: a proposer feeding an unread queue improves nothing, so the loop
//! reports itself where Mike will see it.
//!
//! Sections, all mechanical reads:
//!   - **Landed** — `win_log` rows (auto-landed Tier-D parameter changes, with
//!     held-out before/after and revert status).
//!   - **Reverted** — `regret_log` `action='reverted'` rows (a landed change
//!     that regressed on a later fresh snapshot and was rolled back).
//!   - **Trend** — the newest `eval_runs` rows (the recall-eval trend table).
//!   - **Loop health** — newest `consolidation-audit-*.md` age (mirrors the
//!     `consolidation_audit_freshness` doctor check) plus each climber worker's
//!     last telemetry status. A hill climber whose input silently died must be
//!     impossible (SO S6): dead feeds show here, loudly.
//!   - **Needs your call** / **Foundation candidates** — placeholder lanes,
//!     rendered with an explicit "nothing yet" line in v1 (no prose proposer).
//!
//! Failure stance (SO S6):
//!   - `memory.db` ABSENT → SKIP LOUDLY (`climber_digest.skipped` + stderr):
//!     the auto-tune loop has never run on this box, so there is nothing to
//!     digest. We check existence BEFORE `open_db` because `open_db` would
//!     otherwise CREATE an empty db and render a phantom all-empty digest — the
//!     exact silent no-op this spec exists to kill. Foundation code never
//!     assumes instance data exists.
//!   - Any REAL failure (open, migration, read, write) returns `Err` → a
//!     `status=error` telemetry row plus stderr. Loud.

use std::path::Path;

use chrono::Datelike;

use hex::memory::provider::hex_root;
use hex::memory::{db_path, open_db, schema};
use hex::worker::{ctx::Ctx, event::Event, Result, Worker};

/// Cron — 06:00 UTC every Monday (7-field: sec min hour dom mon dow year).
pub const CRON_WEEKLY_MON_0600: &str = "0 0 6 * * MON *";

/// Climber workers whose last telemetry status the Loop-health section reports,
/// keyed by the `source` column of the telemetry store (== the worker name).
const CLIMBER_WORKERS: &[&str] = &["hex-eval-trend", "hex-recall-tune", "hex-climber-digest"];

/// Audit files older than this many days are flagged stale in Loop health —
/// mirrors `consolidation_audit_freshness::STALE_DAYS` so the digest and the
/// doctor never disagree about the same number.
const AUDIT_STALE_DAYS: i64 = 3;

/// How many trend rows (newest first) the Trend section shows.
const TREND_LIMIT: usize = 8;

/// One ledger row (`win_log` or `regret_log`), read for Landed / Reverted.
struct LedgerRow {
    ts: String,
    tuning_score: i64,
    heldout_score: i64,
    reverted: bool,
    params_json: String,
}

impl LedgerRow {
    /// The pre-change held-out score recorded in `params_json` (the baseline the
    /// landed change was measured to beat), or `None` when absent/unparseable.
    /// Best-effort: a malformed payload degrades the line, never the digest.
    fn prev_heldout(&self) -> Option<i64> {
        serde_json::from_str::<serde_json::Value>(&self.params_json)
            .ok()
            .and_then(|v| v.get("prev_heldout_score").and_then(serde_json::Value::as_i64))
    }
}

/// One `eval_runs` row for the Trend section.
struct TrendRow {
    ts: String,
    cases_total: i64,
    facts_hits: i64,
    anywhere_hits: i64,
    regressions: i64,
    baseline_present: bool,
    harness_version: String,
}

/// Last telemetry status for one climber worker (by `source` == worker name).
/// `last_status == None` means the worker has no recorded run yet (a fresh box,
/// or the telemetry store is absent) — rendered as "no telemetry yet", never
/// silently dropped.
struct WorkerStatus {
    worker: String,
    last_ts: Option<String>,
    last_status: Option<String>,
}

/// Newest consolidation-audit freshness, read from the `YYYY-MM-DD` filename
/// suffix using `chrono::Local` (matches both `consolidate.rs`'s writer and the
/// doctor check's reader).
enum AuditFreshness {
    /// `evolution/` absent, or present but holding no `consolidation-audit-*.md`.
    Absent,
    /// Newest audit's filename date and its age in days.
    Newest { date: String, age_days: i64 },
}

/// Loop-health inputs: newest consolidation-audit age + each climber worker's
/// last telemetry status. Constructed by the live gather path; the unit test
/// builds one directly so `render_digest` stays pure (no DB, no telemetry, no
/// `HEX_DIR`).
struct LoopHealth {
    audit: AuditFreshness,
    workers: Vec<WorkerStatus>,
}

/// Read ledger rows from `table` matching `filter`, newest first. Both `table`
/// and `filter` are internal string literals (never user input), so the
/// interpolation carries no injection risk.
fn read_ledger(conn: &rusqlite::Connection, table: &str, filter: &str) -> Result<Vec<LedgerRow>> {
    let sql = format!(
        "SELECT ts, tuning_score, heldout_score, reverted, params_json
             FROM {table} WHERE {filter} ORDER BY id DESC"
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| anyhow::anyhow!("climber-digest: prepare {table} read failed: {e}"))?;
    let rows = stmt
        .query_map([], |r| {
            Ok(LedgerRow {
                ts: r.get(0)?,
                tuning_score: r.get(1)?,
                heldout_score: r.get(2)?,
                reverted: r.get::<_, i64>(3)? != 0,
                params_json: r.get(4)?,
            })
        })
        .map_err(|e| anyhow::anyhow!("climber-digest: query {table} failed: {e}"))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| anyhow::anyhow!("climber-digest: read {table} rows failed: {e}"))?;
    Ok(rows)
}

/// Landed changes = every `win_log` row (all are `action='land'`).
fn read_landed(conn: &rusqlite::Connection) -> Result<Vec<LedgerRow>> {
    read_ledger(conn, "win_log", "action = 'land'")
}

/// Reverted changes = `regret_log` rows written by the auto-revert path.
fn read_reverted(conn: &rusqlite::Connection) -> Result<Vec<LedgerRow>> {
    read_ledger(conn, "regret_log", "action = 'reverted'")
}

/// Newest `limit` `eval_runs` rows for the Trend section (newest first).
fn read_trend(conn: &rusqlite::Connection, limit: usize) -> Result<Vec<TrendRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT ts, cases_total, facts_hits, anywhere_hits, regressions, \
                    baseline_present, harness_version
             FROM eval_runs ORDER BY id DESC LIMIT ?1",
        )
        .map_err(|e| anyhow::anyhow!("climber-digest: prepare eval_runs read failed: {e}"))?;
    let rows = stmt
        .query_map(rusqlite::params![limit as i64], |r| {
            Ok(TrendRow {
                ts: r.get(0)?,
                cases_total: r.get(1)?,
                facts_hits: r.get(2)?,
                anywhere_hits: r.get(3)?,
                regressions: r.get(4)?,
                baseline_present: r.get::<_, i64>(5)? != 0,
                harness_version: r.get(6)?,
            })
        })
        .map_err(|e| anyhow::anyhow!("climber-digest: query eval_runs failed: {e}"))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| anyhow::anyhow!("climber-digest: read eval_runs rows failed: {e}"))?;
    Ok(rows)
}

/// Newest consolidation-audit freshness from `evolution/`. Mirrors the doctor
/// check `newest_audit_date`: same filename pattern, same `%Y-%m-%d` parse, same
/// `chrono::Local` today — so digest and doctor report the same age. Absent dir
/// or no matching files → `Absent` (never an error: an instance can have an
/// `evolution/` dir without ever running a full consolidation).
fn newest_audit_freshness(root: &Path) -> AuditFreshness {
    let evo = root.join("evolution");
    let mut newest: Option<chrono::NaiveDate> = None;
    if let Ok(rd) = std::fs::read_dir(&evo) {
        for entry in rd.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(ds) = name
                .strip_prefix("consolidation-audit-")
                .and_then(|s| s.strip_suffix(".md"))
            {
                if let Ok(d) = chrono::NaiveDate::parse_from_str(ds, "%Y-%m-%d") {
                    if newest.map(|n| d > n).unwrap_or(true) {
                        newest = Some(d);
                    }
                }
            }
        }
    }
    match newest {
        None => AuditFreshness::Absent,
        Some(d) => {
            let today = chrono::Local::now().date_naive();
            AuditFreshness::Newest {
                date: d.to_string(),
                age_days: (today - d).num_days(),
            }
        }
    }
}

/// Last telemetry status for one climber worker, by `source` (== worker name).
///
/// Guards on `telemetry::db_exists()` FIRST: `open_ro` errors on a missing
/// store and must never create it, so a box that has never recorded any event
/// reports "no telemetry yet" rather than failing the whole digest. Reading the
/// digest worker's OWN status yields the PREVIOUS run — the runtime writes this
/// run's telemetry row only after the handler returns.
fn last_status_for_source(worker: &str) -> WorkerStatus {
    if !hex::telemetry::db_exists() {
        return WorkerStatus {
            worker: worker.to_string(),
            last_ts: None,
            last_status: None,
        };
    }
    match query_last_status(worker) {
        Ok(Some((ts, status))) => WorkerStatus {
            worker: worker.to_string(),
            last_ts: Some(ts),
            last_status: Some(status),
        },
        Ok(None) => WorkerStatus {
            worker: worker.to_string(),
            last_ts: None,
            last_status: None,
        },
        // A telemetry read error is loud but must not sink the digest — surface
        // it in-line so a dead read is visible, per S6.
        Err(e) => {
            eprintln!("[climber-digest] telemetry read for '{worker}' failed: {e}");
            WorkerStatus {
                worker: worker.to_string(),
                last_ts: None,
                last_status: Some(format!("read error: {e}")),
            }
        }
    }
}

/// The newest `(ts, status)` for `source`, or `None` when it has no rows.
fn query_last_status(worker: &str) -> rusqlite::Result<Option<(String, String)>> {
    let conn = hex::telemetry::open_ro()?;
    let mut stmt =
        conn.prepare("SELECT ts, status FROM events WHERE source = ?1 ORDER BY id DESC LIMIT 1")?;
    let mut rows = stmt.query(rusqlite::params![worker])?;
    match rows.next()? {
        Some(r) => Ok(Some((r.get(0)?, r.get(1)?))),
        None => Ok(None),
    }
}

/// Gather the live Loop-health inputs (audit age + per-worker telemetry).
fn gather_loop_health(root: &Path) -> LoopHealth {
    LoopHealth {
        audit: newest_audit_freshness(root),
        workers: CLIMBER_WORKERS
            .iter()
            .map(|w| last_status_for_source(w))
            .collect(),
    }
}

/// Render the digest markdown. PURE — takes only owned/borrowed data, touches
/// no DB, telemetry, filesystem, or clock. The unit test drives this directly
/// with fixture rows so the whole rendering contract is testable offline.
fn render_digest(
    week: &str,
    landed: &[LedgerRow],
    reverted: &[LedgerRow],
    trend: &[TrendRow],
    health: &LoopHealth,
) -> String {
    let mut o = String::new();
    o.push_str(&format!("# Hill-climber digest {week}\n\n"));
    o.push_str(
        "_Mechanical read of the auto-tune loop's own tables. No LLM, no judgment applied._\n\n",
    );

    // ── Landed ────────────────────────────────────────────────────────────
    o.push_str("## Landed\n\n");
    if landed.is_empty() {
        o.push_str("_nothing yet_\n\n");
    } else {
        for r in landed {
            let before = r
                .prev_heldout()
                .map(|p| p.to_string())
                .unwrap_or_else(|| "?".to_string());
            let flag = if r.reverted { " *(reverted)*" } else { "" };
            o.push_str(&format!(
                "- {} — held-out {before}→{}, tuning {}{flag}\n",
                r.ts, r.heldout_score, r.tuning_score
            ));
        }
        o.push('\n');
    }

    // ── Reverted ──────────────────────────────────────────────────────────
    o.push_str("## Reverted\n\n");
    if reverted.is_empty() {
        o.push_str("_nothing yet_\n\n");
    } else {
        for r in reverted {
            let baseline = r
                .prev_heldout()
                .map(|p| p.to_string())
                .unwrap_or_else(|| "?".to_string());
            o.push_str(&format!(
                "- {} — rolled back: live held-out {} < pre-change {baseline}\n",
                r.ts, r.heldout_score
            ));
        }
        o.push('\n');
    }

    // ── Trend ─────────────────────────────────────────────────────────────
    o.push_str("## Trend\n\n");
    if trend.is_empty() {
        o.push_str("_nothing yet_\n\n");
    } else {
        for r in trend {
            let baseline = if r.baseline_present {
                "baseline present"
            } else {
                "baseline absent"
            };
            o.push_str(&format!(
                "- {} — {}/{} facts, {} anywhere, {} regressions, {baseline} ({})\n",
                r.ts, r.facts_hits, r.cases_total, r.anywhere_hits, r.regressions, r.harness_version
            ));
        }
        o.push('\n');
    }

    // ── Loop health ───────────────────────────────────────────────────────
    o.push_str("## Loop health\n\n");
    match &health.audit {
        AuditFreshness::Absent => {
            o.push_str("- Newest consolidation audit: none found (evolution/ absent or no audits yet)\n");
        }
        AuditFreshness::Newest { date, age_days } => {
            let stale = if *age_days > AUDIT_STALE_DAYS {
                format!(" — STALE (>{AUDIT_STALE_DAYS}d)")
            } else {
                String::new()
            };
            o.push_str(&format!(
                "- Newest consolidation audit: {date} ({age_days}d old){stale}\n"
            ));
        }
    }
    for w in &health.workers {
        match (&w.last_status, &w.last_ts) {
            (Some(status), Some(ts)) => {
                o.push_str(&format!("- {}: {status} @ {ts}\n", w.worker));
            }
            (Some(status), None) => {
                o.push_str(&format!("- {}: {status}\n", w.worker));
            }
            _ => {
                o.push_str(&format!("- {}: no telemetry yet\n", w.worker));
            }
        }
    }
    o.push('\n');

    // ── Needs your call (placeholder lane, v1) ────────────────────────────
    o.push_str("## Needs your call\n\n");
    o.push_str("_nothing yet_ — the drafted-diff approval lane is empty in v1 (no prose proposer at this stage).\n\n");

    // ── Foundation candidates (placeholder lane, v1) ──────────────────────
    o.push_str("## Foundation candidates\n\n");
    o.push_str("_nothing yet_ — the foundation-promotion lane is empty in v1.\n");

    o
}

/// The current ISO week label `YYYY-WXX`. Uses `%G`-style ISO year via
/// `iso_week().year()` (NOT the calendar year), so the label is correct across
/// year boundaries (e.g. 2026-12-28 is `2026-W53`).
fn iso_week_label(now: chrono::DateTime<chrono::Utc>) -> String {
    let w = now.iso_week();
    format!("{}-W{:02}", w.year(), w.week())
}

fn run_climber_digest(_e: Event, ctx: Ctx) -> Result<()> {
    let root = hex_root();

    // Foundation never assumes instance data exists: no memory.db → the loop has
    // never run here. SKIP LOUDLY. Check BEFORE open_db, which would create the
    // file and render a phantom empty digest.
    let dbp = db_path(&root);
    if !dbp.exists() {
        eprintln!(
            "[climber-digest] SKIP: memory.db absent at {} — auto-tune loop has not run",
            dbp.display()
        );
        ctx.emit(
            "climber_digest.skipped",
            serde_json::json!({
                "reason": "memory_db_absent",
                "db_path": dbp.display().to_string(),
            }),
        )?;
        return Ok(());
    }

    let conn = open_db(&dbp)
        .map_err(|e| anyhow::anyhow!("climber-digest: open memory.db {} failed: {e}", dbp.display()))?;
    // Idempotent migrations so a box where only SOME climber workers have run
    // reads an empty table (not "no such table") for the ones that have not.
    schema::apply_tune_log_schema(&conn)
        .map_err(|e| anyhow::anyhow!("climber-digest: win_log/regret_log migration failed: {e}"))?;
    schema::apply_eval_runs_schema(&conn)
        .map_err(|e| anyhow::anyhow!("climber-digest: eval_runs migration failed: {e}"))?;

    let landed = read_landed(&conn)?;
    let reverted = read_reverted(&conn)?;
    let trend = read_trend(&conn, TREND_LIMIT)?;
    let health = gather_loop_health(&root);

    let week = iso_week_label(chrono::Utc::now());
    let body = render_digest(&week, &landed, &reverted, &trend, &health);

    // Write evolution/digests/YYYY-WXX.md atomically (tmp then rename).
    let dir = root.join("evolution/digests");
    std::fs::create_dir_all(&dir)
        .map_err(|e| anyhow::anyhow!("climber-digest: mkdir {} failed: {e}", dir.display()))?;
    let out = dir.join(format!("{week}.md"));
    let tmp = out.with_extension("md.tmp");
    std::fs::write(&tmp, &body)
        .map_err(|e| anyhow::anyhow!("climber-digest: write tmp {} failed: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &out)
        .map_err(|e| anyhow::anyhow!("climber-digest: rename {} failed: {e}", out.display()))?;

    eprintln!(
        "[climber-digest] wrote {} (landed={}, reverted={}, trend={})",
        out.display(),
        landed.len(),
        reverted.len(),
        trend.len()
    );
    ctx.emit(
        "climber_digest.written",
        serde_json::json!({
            "path": out.display().to_string(),
            "week": week,
            "landed": landed.len(),
            "reverted": reverted.len(),
            "trend_rows": trend.len(),
        }),
    )?;
    Ok(())
}

/// Build the `hex-climber-digest` worker.
pub fn worker() -> Worker {
    Worker::new("hex-climber-digest").on_cron_named(
        "weekly",
        CRON_WEEKLY_MON_0600,
        run_climber_digest,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Seed one `win_log` land row, one `regret_log` reverted row, and two
    /// `eval_runs` trend rows into an in-memory DB, read them back through the
    /// real read functions, and render. Asserts every required section header
    /// is present AND that fixture values reach the output — the digest is
    /// genuinely rendered FROM fixture win_log/regret_log/eval_runs rows (spec
    /// "digest-rendered").
    #[test]
    fn digest_renders_all_sections_from_fixture_rows() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        schema::apply_tune_log_schema(&conn).unwrap();
        schema::apply_eval_runs_schema(&conn).unwrap();

        // win_log: a landed change, pre-change held-out 7 → landed 9, tuning 11.
        conn.execute(
            "INSERT INTO win_log (ts, params_json, tuning_score, heldout_score, action, reverted)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                "2026-08-17T05:00:00Z",
                r#"{"prev_heldout_score":7}"#,
                11,
                9,
                "land",
                0
            ],
        )
        .unwrap();

        // regret_log: a later auto-revert (live held-out 6 < pre-change 7).
        conn.execute(
            "INSERT INTO regret_log (ts, params_json, tuning_score, heldout_score, action, reverted)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                "2026-08-18T05:00:00Z",
                r#"{"prev_heldout_score":7,"reverted_win_id":1}"#,
                0,
                6,
                "reverted",
                1
            ],
        )
        .unwrap();

        // eval_runs: two nights of trend.
        for (ts, ct, fh, ah, reg, bp, hv) in [
            ("2026-08-16T04:30:00Z", 60, 45, 52, 1, 1, "abc1234"),
            ("2026-08-17T04:30:00Z", 60, 48, 55, 0, 1, "def5678"),
        ] {
            conn.execute(
                "INSERT INTO eval_runs
                     (ts, cases_total, facts_hits, anywhere_hits, regressions, baseline_present, harness_version)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![ts, ct, fh, ah, reg, bp, hv],
            )
            .unwrap();
        }

        let landed = read_landed(&conn).unwrap();
        let reverted = read_reverted(&conn).unwrap();
        let trend = read_trend(&conn, TREND_LIMIT).unwrap();
        assert_eq!(landed.len(), 1, "one landed row");
        assert_eq!(reverted.len(), 1, "one reverted row");
        assert_eq!(trend.len(), 2, "two trend rows");

        // Loop health built directly — render stays free of telemetry/HEX_DIR.
        let health = LoopHealth {
            audit: AuditFreshness::Newest {
                date: "2026-08-17".to_string(),
                age_days: 2,
            },
            workers: vec![
                WorkerStatus {
                    worker: "hex-eval-trend".to_string(),
                    last_ts: Some("2026-08-17T04:30:00Z".to_string()),
                    last_status: Some("ok".to_string()),
                },
                WorkerStatus {
                    worker: "hex-recall-tune".to_string(),
                    last_ts: None,
                    last_status: None,
                },
            ],
        };

        let md = render_digest("2026-W34", &landed, &reverted, &trend, &health);

        // Every required section header — exact strings the verification greps.
        for header in [
            "## Landed",
            "## Reverted",
            "## Trend",
            "## Loop health",
            "## Needs your call",
            "## Foundation candidates",
        ] {
            assert!(md.contains(header), "digest must contain header `{header}`\n{md}");
        }

        // Placeholder lanes carry the explicit "nothing yet" line.
        assert!(
            md.matches("nothing yet").count() >= 2,
            "both placeholder lanes must say 'nothing yet'\n{md}"
        );

        // Fixture values reached the output — proof it rendered FROM the rows.
        assert!(md.contains("held-out 7→9"), "landed before/after\n{md}");
        assert!(md.contains("live held-out 6 < pre-change 7"), "reverted line\n{md}");
        assert!(md.contains("48/60 facts"), "trend row\n{md}");
        assert!(md.contains("def5678"), "trend harness version\n{md}");
        assert!(md.contains("2026-08-17 (2d old)"), "audit age\n{md}");
        assert!(md.contains("hex-eval-trend: ok @"), "worker status\n{md}");
        assert!(md.contains("hex-recall-tune: no telemetry yet"), "empty worker status\n{md}");
        assert!(md.starts_with("# Hill-climber digest 2026-W34"), "titled by ISO week\n{md}");
    }

    /// Empty tables render the placeholder ("nothing yet") in EVERY list
    /// section — a fresh box's digest is legible, not blank.
    #[test]
    fn empty_tables_render_nothing_yet_everywhere() {
        let health = LoopHealth {
            audit: AuditFreshness::Absent,
            workers: vec![WorkerStatus {
                worker: "hex-recall-tune".to_string(),
                last_ts: None,
                last_status: None,
            }],
        };
        let md = render_digest("2026-W01", &[], &[], &[], &health);
        for header in [
            "## Landed",
            "## Reverted",
            "## Trend",
            "## Loop health",
            "## Needs your call",
            "## Foundation candidates",
        ] {
            assert!(md.contains(header), "missing `{header}`\n{md}");
        }
        // Landed, Reverted, Trend, Needs your call, Foundation candidates → 5.
        assert!(
            md.matches("nothing yet").count() >= 5,
            "all list sections must say 'nothing yet' when empty\n{md}"
        );
        assert!(md.contains("none found"), "absent audit reported\n{md}");
    }

    /// A stale audit (older than the threshold) is flagged STALE in Loop health.
    #[test]
    fn stale_audit_is_flagged() {
        let health = LoopHealth {
            audit: AuditFreshness::Newest {
                date: "2026-08-01".to_string(),
                age_days: 18,
            },
            workers: vec![],
        };
        let md = render_digest("2026-W34", &[], &[], &[], &health);
        assert!(
            md.contains("2026-08-01 (18d old) — STALE"),
            "an audit older than {AUDIT_STALE_DAYS}d must be flagged STALE\n{md}"
        );
    }

    /// ISO week label uses the ISO year, so it is correct across the calendar
    /// year boundary (2026-12-28 falls in ISO week 2026-W53).
    #[test]
    fn iso_week_label_uses_iso_year() {
        use chrono::TimeZone;
        let dt = chrono::Utc.with_ymd_and_hms(2026, 12, 28, 6, 0, 0).unwrap();
        assert_eq!(iso_week_label(dt), "2026-W53");
        // Two-digit zero padding early in the year.
        let dt2 = chrono::Utc.with_ymd_and_hms(2026, 2, 2, 6, 0, 0).unwrap();
        assert_eq!(iso_week_label(dt2), "2026-W06");
    }
}
