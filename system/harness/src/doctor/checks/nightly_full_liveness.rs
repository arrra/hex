//! FAIL when the nightly full consolidation hasn't completed in >26h —
//! catches lock-timeouts, harness-down nights, and kills-in-flight that the
//! per-run telemetry can't see (the run never finished to record anything).
//!
//! Reads `metadata.last_full_consolidated` from `.hex/memory.db` (stamped by
//! `consolidate::run` on a clean FULL completion). Companion to
//! `reflection_liveness.rs`, which reads `last_consolidated` (any mode, 48h);
//! this one is full-mode-only with a 26h threshold (nightly cadence + slack).

use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};
use std::time::Duration;

const THRESHOLD: Duration = Duration::from_secs(26 * 3600);
const KEY: &str = "last_full_consolidated";

pub struct NightlyFullLiveness;

impl DoctorCheck for NightlyFullLiveness {
    fn name(&self) -> &str {
        "nightly-full-liveness"
    }
    fn category(&self) -> Category {
        Category::Health
    }
    fn run(&self, ctx: &Context) -> CheckResult {
        let db_path = ctx.hex_dir.join(".hex/memory.db");
        if !db_path.exists() {
            return CheckResult::fail(
                "memory.db not found — run `hex memory consolidate full` to initialize",
            );
        }
        let conn = match rusqlite::Connection::open(&db_path) {
            Ok(c) => c,
            Err(e) => return CheckResult::fail(format!("cannot open memory.db: {e}")),
        };
        let stamp: Option<String> = conn
            .query_row(
                "SELECT value FROM metadata WHERE key = ?1",
                rusqlite::params![KEY],
                |row| row.get(0),
            )
            .ok();
        let stamp = match stamp {
            Some(s) => s,
            None => {
                return CheckResult::fail(
                    "nightly full consolidation has never completed — run `hex memory consolidate full`",
                );
            }
        };
        let parsed = match chrono::DateTime::parse_from_rfc3339(&stamp) {
            Ok(dt) => dt,
            Err(e) => {
                return CheckResult::fail(format!("metadata.{KEY} unparseable ({stamp:?}): {e}"));
            }
        };
        let now = chrono::Local::now();
        let elapsed = now.signed_duration_since(parsed);
        let hours = elapsed.num_hours().max(0);
        if elapsed.to_std().unwrap_or(Duration::ZERO) > THRESHOLD {
            let msg = format!(
                "last full consolidation {hours}h ago (>26h) — run `hex memory consolidate full`"
            );
            crate::alert::notify_at(
                &ctx.hex_dir,
                "nightly-full-liveness",
                "hex nightly consolidation missed",
                &msg,
            );
            CheckResult::fail(msg)
        } else {
            CheckResult::pass(format!("full consolidation last completed {hours}h ago"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doctor::check::{Context, DoctorCheck, Status};

    fn ctx_with_db(tmp: &std::path::Path, value: Option<&str>) -> Context {
        std::fs::create_dir_all(tmp.join(".hex")).unwrap();
        let db = tmp.join(".hex/memory.db");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )
        .unwrap();
        if let Some(v) = value {
            conn.execute(
                "INSERT OR REPLACE INTO metadata (key, value) VALUES (?1, ?2)",
                rusqlite::params![KEY, v],
            )
            .unwrap();
        }
        // Keep the alert pathway's telemetry write hermetic (telemetry
        // resolves events.db from $HEX_DIR) — same pattern as the
        // telemetry/mod.rs tests.
        std::env::set_var("HEX_DIR", tmp);
        Context::new(tmp.to_path_buf(), false)
    }

    #[test]
    fn fails_when_stamp_missing() {
        // ctx_with_db mutates the process-global HEX_DIR — hold the crate's
        // single env lock for the test's duration (telemetry/mod.rs contract).
        let _g = crate::telemetry::test_support::lock_env();
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx_with_db(tmp.path(), None);
        let res = NightlyFullLiveness.run(&ctx);
        assert_eq!(
            res.status,
            Status::Fail,
            "missing key must FAIL, got {:?}",
            res
        );
    }

    #[test]
    fn fails_when_stamp_stale() {
        let _g = crate::telemetry::test_support::lock_env();
        let tmp = tempfile::tempdir().unwrap();
        // 30h ago — past the 26h threshold.
        let stale = chrono::Local::now() - chrono::Duration::hours(30);
        let ctx = ctx_with_db(tmp.path(), Some(&stale.to_rfc3339()));
        let res = NightlyFullLiveness.run(&ctx);
        assert_eq!(
            res.status,
            Status::Fail,
            "stale stamp must FAIL, got {:?}",
            res
        );
    }

    #[test]
    fn passes_when_stamp_fresh() {
        let _g = crate::telemetry::test_support::lock_env();
        let tmp = tempfile::tempdir().unwrap();
        let fresh = chrono::Local::now() - chrono::Duration::hours(2);
        let ctx = ctx_with_db(tmp.path(), Some(&fresh.to_rfc3339()));
        let res = NightlyFullLiveness.run(&ctx);
        assert_eq!(
            res.status,
            Status::Pass,
            "fresh stamp must PASS, got {:?}",
            res
        );
    }
}
