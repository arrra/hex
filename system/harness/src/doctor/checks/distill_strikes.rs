//! Surface transcript files whose distill `consecutive_failures` strike
//! counter has gone nonzero. A nonzero strike means the next cron tick will
//! retry the slice with a halved budget; at the floor it loudly skips. We
//! want operators to see WHICH file is struggling, not just the telemetry
//! event after the fact.

use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};

pub struct DistillStrikes;

impl DoctorCheck for DistillStrikes {
    fn name(&self) -> &str {
        "distill-strikes"
    }
    fn category(&self) -> Category {
        Category::Health
    }
    fn run(&self, ctx: &Context) -> CheckResult {
        let db = ctx.hex_dir.join(".hex/memory.db");
        if !db.is_file() {
            return CheckResult::skip("memory.db missing — strike check skipped");
        }
        let conn = match crate::memory::open_db(&db) {
            Ok(c) => c,
            Err(e) => return CheckResult::warn(format!("cannot open memory.db: {e}")),
        };

        // Tolerate older DBs that pre-date the consecutive_failures column.
        let mut stmt = match conn.prepare(
            "SELECT path, consecutive_failures FROM transcript_files \
             WHERE consecutive_failures > 0 ORDER BY consecutive_failures DESC LIMIT 20",
        ) {
            Ok(s) => s,
            Err(_) => return CheckResult::skip("transcript_files.consecutive_failures absent"),
        };
        let rows: Vec<(String, i64)> = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .map(|it| it.filter_map(|r| r.ok()).collect())
            .unwrap_or_default();

        if rows.is_empty() {
            return CheckResult::pass("no transcript files have nonzero strike counters");
        }

        let mut detail = String::new();
        for (path, n) in &rows {
            detail.push_str(&format!("  {n} strike(s): {path}\n"));
        }
        CheckResult::warn(format!(
            "{} transcript file(s) have nonzero distill strike counters",
            rows.len()
        ))
        .with_details(detail)
    }
}
