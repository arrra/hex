//! Consolidate-freshness check (Phase A of the session-less hex redesign).
//!
//! Re-keyed 2026-06-05: instead of inspecting an evolution-log mtime, this check
//! reads `metadata.last_consolidated` from `.hex/memory.db` (stamped by
//! `memory::consolidate::stamp_last_consolidated`). FAIL when the stamp is missing
//! or older than 48h — the consolidate pipeline is the new heartbeat.

use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};
use std::time::Duration;

const THRESHOLD: Duration = Duration::from_secs(48 * 3600);

pub struct ReflectionLogFresh;

impl DoctorCheck for ReflectionLogFresh {
    fn name(&self) -> &str { "consolidate-liveness" }
    fn category(&self) -> Category { Category::Health }
    fn run(&self, ctx: &Context) -> CheckResult {
        let db_path = ctx.hex_dir.join(".hex/memory.db");
        if !db_path.exists() {
            return CheckResult::fail(
                "memory.db not found — run `hex memory consolidate quick` to initialize",
            );
        }
        let conn = match rusqlite::Connection::open(&db_path) {
            Ok(c) => c,
            Err(e) => return CheckResult::fail(format!("cannot open memory.db: {e}")),
        };
        let stamp: Option<String> = conn
            .query_row(
                "SELECT value FROM metadata WHERE key = 'last_consolidated'",
                [],
                |row| row.get(0),
            )
            .ok();
        let stamp = match stamp {
            Some(s) => s,
            None => {
                return CheckResult::fail(
                    "metadata.last_consolidated missing — run `hex memory consolidate quick`",
                );
            }
        };
        let parsed = match chrono::DateTime::parse_from_rfc3339(&stamp) {
            Ok(dt) => dt,
            Err(e) => {
                return CheckResult::fail(format!(
                    "metadata.last_consolidated unparseable ({stamp:?}): {e}"
                ));
            }
        };
        let now = chrono::Local::now();
        let elapsed = now.signed_duration_since(parsed);
        let hours = elapsed.num_hours().max(0);
        if elapsed.to_std().unwrap_or(Duration::ZERO) > THRESHOLD {
            CheckResult::fail(format!(
                "consolidate last ran {hours}h ago (threshold: 48h) — run `hex memory consolidate quick`"
            ))
        } else {
            CheckResult::pass(format!("consolidate last ran {hours}h ago"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doctor::check::{Status, Context, DoctorCheck};

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
                "INSERT OR REPLACE INTO metadata (key, value) VALUES ('last_consolidated', ?)",
                rusqlite::params![v],
            )
            .unwrap();
        }
        Context::new(tmp.to_path_buf(), false)
    }

    #[test]
    fn fails_when_last_consolidated_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx_with_db(tmp.path(), None);
        let res = ReflectionLogFresh.run(&ctx);
        assert_eq!(res.status, Status::Fail, "missing key must FAIL, got {:?}", res);
    }

    #[test]
    fn fails_when_last_consolidated_stale() {
        let tmp = tempfile::tempdir().unwrap();
        // 72h ago — past the 48h threshold.
        let stale = chrono::Local::now() - chrono::Duration::hours(72);
        let ctx = ctx_with_db(tmp.path(), Some(&stale.to_rfc3339()));
        let res = ReflectionLogFresh.run(&ctx);
        assert_eq!(res.status, Status::Fail, "stale stamp must FAIL, got {:?}", res);
    }

    #[test]
    fn passes_when_last_consolidated_fresh() {
        let tmp = tempfile::tempdir().unwrap();
        let fresh = chrono::Local::now() - chrono::Duration::hours(2);
        let ctx = ctx_with_db(tmp.path(), Some(&fresh.to_rfc3339()));
        let res = ReflectionLogFresh.run(&ctx);
        assert_eq!(res.status, Status::Pass, "fresh stamp must PASS, got {:?}", res);
    }
}
