//! Folded in from the former `hex memory check-vector-search` subcommand.
//! Opens memory.db (which registers sqlite-vec) and counts `vec_chunks` rows.
//! Vector search is an OPTIONAL upgrade over FTS5-only (README), so a missing
//! extension or empty index is a WARN, not a hard FAIL.

use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};
use crate::memory;

pub struct VectorSearchHealthy;

impl DoctorCheck for VectorSearchHealthy {
    fn name(&self) -> &str { "vector-search" }
    fn category(&self) -> Category { Category::Health }
    fn run(&self, ctx: &Context) -> CheckResult {
        let db_path = memory::db_path(&ctx.hex_dir);
        if !db_path.exists() {
            return CheckResult::skip("memory.db not present — vector search check skipped");
        }
        let conn = match memory::open_db(&db_path) {
            Ok(c) => c,
            Err(e) => return CheckResult::warn(format!("cannot open memory.db: {e}")),
        };
        // open_db already called register_sqlite_vec(); if vec0 isn't available the
        // COUNT query fails with "no such module: vec0" or "no such table".
        match conn.query_row("SELECT COUNT(*) FROM vec_chunks", [], |r| r.get::<_, i64>(0)) {
            Ok(0) => CheckResult::warn("vec_chunks empty — no vectors indexed (run `hex memory index --full`)"),
            Ok(n) => CheckResult::pass(format!("sqlite-vec loadable, {n} vectors in vec_chunks")),
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("no such module") {
                    CheckResult::warn("sqlite-vec extension not loadable — FTS5-only mode (optional upgrade unavailable)")
                } else if msg.contains("no such table") {
                    CheckResult::warn("vec_chunks table missing — run `hex memory index --full`")
                } else {
                    CheckResult::warn(format!("sqlite error querying vec_chunks: {msg}"))
                }
            }
        }
    }
}
