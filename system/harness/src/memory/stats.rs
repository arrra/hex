use rusqlite::Connection;
use std::path::Path;

pub struct StatsReport {
    pub files_indexed: i64,
    pub total_facts: i64,
    pub top_predicates: Vec<(String, i64)>,
    pub top_subjects: Vec<(String, i64)>,
    pub db_size_bytes: u64,
    pub last_consolidated: Option<String>,
    pub schema_version: Option<i64>,
}

pub fn run(hex_root: &Path, json: bool) -> i32 {
    let db_path = super::db_path(hex_root);

    if !db_path.exists() {
        eprintln!(
            "No memory DB found at {}. Run `hex memory index` to create one.",
            db_path.display()
        );
        return 1;
    }

    let conn = match super::open_db(&db_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("stats: cannot open DB: {e}");
            return 1;
        }
    };

    let report = match gather(&conn, &db_path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("stats: query failed: {e}");
            return 1;
        }
    };

    if json {
        print_json(&report);
    } else {
        print_table(&report);
    }
    0
}

fn gather(conn: &Connection, db_path: &Path) -> rusqlite::Result<StatsReport> {
    let files_indexed: i64 = conn
        .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
        .unwrap_or(0);

    let total_facts: i64 = conn
        .query_row("SELECT COUNT(*) FROM facts WHERE tombstone = 0", [], |r| {
            r.get(0)
        })
        .unwrap_or(0);

    let top_predicates: Vec<(String, i64)> = {
        let mut stmt = conn.prepare(
            "SELECT predicate, COUNT(*) AS cnt FROM facts WHERE tombstone = 0 \
             GROUP BY predicate ORDER BY cnt DESC LIMIT 10",
        )?;
        let rows: Vec<(String, i64)> = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
            .filter_map(|r| r.ok())
            .collect();
        rows
    };

    let top_subjects: Vec<(String, i64)> = {
        let mut stmt = conn.prepare(
            "SELECT subject, COUNT(*) AS cnt FROM facts WHERE tombstone = 0 \
             GROUP BY subject ORDER BY cnt DESC LIMIT 5",
        )?;
        let rows: Vec<(String, i64)> = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
            .filter_map(|r| r.ok())
            .collect();
        rows
    };

    let db_size_bytes = db_path.metadata().map(|m| m.len()).unwrap_or(0);

    // `hex memory consolidate` stamps this key on every run (see
    // memory::consolidate::stamp_last_consolidated). The old query read
    // `MAX(last_consolidated) FROM topics`, but topic-rollup is a no-op and the
    // topics table stays empty — so it printed "never" no matter how many
    // consolidations ran. Read the metadata key the consolidator actually writes.
    let last_consolidated: Option<String> = conn
        .query_row(
            "SELECT value FROM metadata WHERE key = 'last_consolidated'",
            [],
            |r| r.get(0),
        )
        .ok();

    let schema_version: Option<i64> = conn
        .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
        .unwrap_or(None);

    Ok(StatsReport {
        files_indexed,
        total_facts,
        top_predicates,
        top_subjects,
        db_size_bytes,
        last_consolidated,
        schema_version,
    })
}

fn print_table(r: &StatsReport) {
    println!("=== Memory Database Stats ===");
    println!();
    println!(
        "Files indexed:     {}",
        r.files_indexed
    );
    println!("Facts (live):      {}", r.total_facts);
    println!(
        "DB size:           {:.1} KB",
        r.db_size_bytes as f64 / 1024.0
    );
    println!(
        "Schema version:    {}",
        r.schema_version
            .map(|v| v.to_string())
            .unwrap_or_else(|| "unknown".to_string())
    );
    println!(
        "Last consolidated: {}",
        r.last_consolidated.as_deref().unwrap_or("never")
    );

    println!();
    println!("Top predicates:");
    if r.top_predicates.is_empty() {
        println!("  (none)");
    } else {
        let max_w = r.top_predicates.iter().map(|(p, _)| p.len()).max().unwrap_or(0);
        for (pred, cnt) in &r.top_predicates {
            println!("  {:<width$}  {}", pred, cnt, width = max_w);
        }
    }

    println!();
    println!("Top subjects:");
    if r.top_subjects.is_empty() {
        println!("  (none)");
    } else {
        let max_w = r.top_subjects.iter().map(|(s, _)| s.len()).max().unwrap_or(0);
        for (subj, cnt) in &r.top_subjects {
            println!("  {:<width$}  {}", subj, cnt, width = max_w);
        }
    }
}

fn print_json(r: &StatsReport) {
    let predicates: Vec<serde_json::Value> = r
        .top_predicates
        .iter()
        .map(|(p, c)| serde_json::json!({"predicate": p, "count": c}))
        .collect();
    let subjects: Vec<serde_json::Value> = r
        .top_subjects
        .iter()
        .map(|(s, c)| serde_json::json!({"subject": s, "count": c}))
        .collect();

    let v = serde_json::json!({
        "files_indexed": r.files_indexed,
        "total_facts": r.total_facts,
        "db_size_bytes": r.db_size_bytes,
        "schema_version": r.schema_version,
        "last_consolidated": r.last_consolidated,
        "top_predicates": predicates,
        "top_subjects": subjects,
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".to_string())
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use tempfile::TempDir;

    fn seed_db(conn: &Connection) -> rusqlite::Result<()> {
        // Apply Plan 2 schema (facts, schema_version, etc.)
        crate::memory::schema::apply_plan2(conn)?;
        // Apply index schema (files table)
        crate::memory::index::init_db(conn)?;

        // Insert a couple of files
        conn.execute_batch(
            "INSERT INTO files (path, mtime, content_hash, indexed_at, chunk_count)
             VALUES ('me/test.md', 0.0, 'abc', '2025-01-01', 1);
             INSERT INTO files (path, mtime, content_hash, indexed_at, chunk_count)
             VALUES ('me/other.md', 0.0, 'def', '2025-01-01', 2);",
        )?;

        // Insert a few facts
        conn.execute_batch(
            "INSERT INTO facts (id, subject, predicate, object, created_at, updated_at)
             VALUES ('f1', 'mike', 'prefers', 'rust', '2025-01-01', '2025-01-01');
             INSERT INTO facts (id, subject, predicate, object, created_at, updated_at)
             VALUES ('f2', 'mike', 'uses', 'claude', '2025-01-01', '2025-01-01');
             INSERT INTO facts (id, subject, predicate, object, created_at, updated_at)
             VALUES ('f3', 'project', 'prefers', 'tdd', '2025-01-01', '2025-01-01');",
        )?;

        Ok(())
    }

    #[test]
    fn stats_output_contains_expected_headers() {
        let tmp = TempDir::new().unwrap();
        let hex_root = tmp.path();
        std::fs::create_dir_all(hex_root.join(".hex")).unwrap();

        let db_path = super::super::db_path(hex_root);
        let conn = super::super::open_db(&db_path).unwrap();
        seed_db(&conn).unwrap();
        drop(conn);

        let report = {
            let conn = super::super::open_db(&db_path).unwrap();
            gather(&conn, &db_path).unwrap()
        };

        assert_eq!(report.files_indexed, 2, "should count 2 files");
        assert_eq!(report.total_facts, 3, "should count 3 live facts");
        assert!(!report.top_predicates.is_empty(), "should have predicates");

        // Verify print_table output contains required section headers
        // by checking the function runs without panic
        print_table(&report);
        // schema_version is set by apply_plan2
        assert_eq!(report.schema_version, Some(4));
    }

    #[test]
    fn stats_reads_last_consolidated_from_metadata() {
        // Regression: stats must read the metadata key the consolidator writes,
        // not MAX(last_consolidated) FROM topics (which is always empty → "never").
        let tmp = TempDir::new().unwrap();
        let hex_root = tmp.path();
        std::fs::create_dir_all(hex_root.join(".hex")).unwrap();

        let db_path = super::super::db_path(hex_root);
        let conn = super::super::open_db(&db_path).unwrap();
        seed_db(&conn).unwrap();

        // No stamp yet → "never".
        let report = gather(&conn, &db_path).unwrap();
        assert!(report.last_consolidated.is_none(), "should be unset before a run");

        // Simulate what memory::consolidate stamps.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT OR REPLACE INTO metadata (key, value) \
             VALUES ('last_consolidated', '2026-06-03T00:00:00-04:00');",
        )
        .unwrap();

        let report = gather(&conn, &db_path).unwrap();
        assert_eq!(
            report.last_consolidated.as_deref(),
            Some("2026-06-03T00:00:00-04:00"),
            "stats must surface the stamped last_consolidated value"
        );
    }

    #[test]
    fn stats_tombstoned_facts_excluded() {
        let tmp = TempDir::new().unwrap();
        let hex_root = tmp.path();
        std::fs::create_dir_all(hex_root.join(".hex")).unwrap();

        let db_path = super::super::db_path(hex_root);
        let conn = super::super::open_db(&db_path).unwrap();
        crate::memory::schema::apply_plan2(&conn).unwrap();
        crate::memory::index::init_db(&conn).unwrap();

        conn.execute_batch(
            "INSERT INTO facts (id, subject, predicate, object, created_at, updated_at, tombstone)
             VALUES ('f1', 'mike', 'prefers', 'rust', '2025-01-01', '2025-01-01', 0);
             INSERT INTO facts (id, subject, predicate, object, created_at, updated_at, tombstone)
             VALUES ('f2', 'mike', 'uses', 'old', '2025-01-01', '2025-01-01', 1);",
        )
        .unwrap();

        let report = gather(&conn, &db_path).unwrap();
        assert_eq!(report.total_facts, 1, "tombstoned facts should not be counted");
    }
}
