use std::path::PathBuf;

/// Post-session reflection — Rust port of the former session-reflect script.
/// Phase A: no longer writes to evolution/reflection-log.md. The command is
/// kept registered (and exits 0) so the existing Stop hook keeps succeeding
/// until Phase B removes both together. Only the harmless eval_records insert
/// remains.
pub fn run(session_id: Option<&str>, quiet: bool) {
    run_in(&resolve_hex_dir(), session_id, quiet);
}

fn resolve_hex_dir() -> PathBuf {
    if let Ok(v) = std::env::var("HEX_DIR") {
        return PathBuf::from(v);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| {
        eprintln!("session-reflect: neither HEX_DIR nor HOME is set");
        std::process::exit(1);
    });
    PathBuf::from(home).join("hex")
}

fn run_in(hex_dir: &PathBuf, session_id: Option<&str>, quiet: bool) {
    if !quiet {
        println!("session-reflect: starting post-session reflection");
    }

    // Phase A: do NOT write to evolution/reflection-log.md. The placeholder
    // line was noisy and added no signal; consolidate is the source of truth.

    let sid = session_id.unwrap_or("");
    run_session_delta(hex_dir, sid);

    if !quiet {
        println!("session-reflect: done");
    }
}

/// Ensures `eval_records` table exists in memory.db and inserts one row for the session.
/// Non-fatal: emits a warning and returns on any error.
fn run_session_delta(hex_dir: &PathBuf, session_id: &str) {
    let memory_db = hex_dir.join(".hex/memory.db");
    if !memory_db.exists() {
        eprintln!(
            "session-delta: memory.db not found at {}, skipping",
            memory_db.display()
        );
        return;
    }

    let payload = serde_json::json!({
        "session_id": session_id,
        "source": "session-delta"
    })
    .to_string();

    let recorded_at = chrono::Utc::now().to_rfc3339();

    let conn = match rusqlite::Connection::open(&memory_db) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("session-delta: warning — could not open db: {e}");
            return;
        }
    };

    if let Err(e) = conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS eval_records (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id  TEXT,
            recorded_at TEXT NOT NULL,
            payload     TEXT
        )",
    ) {
        eprintln!("session-delta: warning — could not create table: {e}");
        return;
    }

    match conn.execute(
        "INSERT INTO eval_records (session_id, recorded_at, payload) VALUES (?1, ?2, ?3)",
        rusqlite::params![session_id, recorded_at, payload],
    ) {
        Ok(_) => println!(
            "session-delta: eval_record persisted for session {:?}",
            session_id
        ),
        Err(e) => eprintln!("session-delta: warning — could not write to db: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Read;

    #[test]
    fn does_not_create_reflection_log_when_absent() {
        // Phase A: session-reflect must not create or write to reflection-log.md.
        let dir = tempfile::tempdir().expect("tempdir");
        let hex_dir = dir.path().to_path_buf();
        fs::create_dir_all(hex_dir.join("evolution")).unwrap();

        run_in(&hex_dir, Some("test-session-123"), true);

        let log_path = hex_dir.join("evolution/reflection-log.md");
        assert!(
            !log_path.exists(),
            "reflection-log.md must NOT be created by session-reflect"
        );
    }

    #[test]
    fn does_not_append_to_pre_existing_reflection_log() {
        // If reflection-log.md already exists, session-reflect must not append to it.
        let dir = tempfile::tempdir().expect("tempdir");
        let hex_dir = dir.path().to_path_buf();
        fs::create_dir_all(hex_dir.join("evolution")).unwrap();

        let log_path = hex_dir.join("evolution/reflection-log.md");
        std::fs::write(&log_path, "pre-existing\n").unwrap();
        let before = std::fs::metadata(&log_path).unwrap().len();

        run_in(&hex_dir, None, true);

        let after = std::fs::metadata(&log_path).unwrap().len();
        assert_eq!(before, after, "reflection-log.md size must not change");

        let mut contents = String::new();
        fs::File::open(&log_path)
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        assert!(!contents.contains("session reflection"));
        assert!(!contents.contains("placeholder"));
    }

    #[test]
    fn delta_not_called_when_script_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let hex_dir = dir.path().to_path_buf();
        fs::create_dir_all(hex_dir.join("evolution")).unwrap();
        // No memory.db — should complete without error
        run_in(&hex_dir, Some("abc"), true);
    }

    #[test]
    fn does_not_write_any_entry_to_reflection_log() {
        // Phase A de-risk: session-reflect must STOP writing any line to
        // evolution/reflection-log.md. The file should either not be created,
        // or contain no entry written by this command.
        let dir = tempfile::tempdir().expect("tempdir");
        let hex_dir = dir.path().to_path_buf();
        fs::create_dir_all(hex_dir.join("evolution")).unwrap();

        run_in(&hex_dir, Some("phase-a-test-001"), true);

        let log_path = hex_dir.join("evolution/reflection-log.md");
        if log_path.exists() {
            let mut contents = String::new();
            fs::File::open(&log_path)
                .unwrap()
                .read_to_string(&mut contents)
                .unwrap();
            assert!(
                !contents.contains("placeholder"),
                "reflection-log.md must NOT contain placeholder text, got: {contents}"
            );
            assert!(
                !contents.contains("session reflection"),
                "reflection-log.md must NOT contain a session-reflect-written entry, got: {contents}"
            );
        }
    }

    #[test]
    fn session_reflect_delta_inserts_eval_record() {
        let dir = tempfile::tempdir().expect("tempdir");
        let hex_dir = dir.path().to_path_buf();
        fs::create_dir_all(hex_dir.join("evolution")).unwrap();
        fs::create_dir_all(hex_dir.join(".hex")).unwrap();

        let db_path = hex_dir.join(".hex/memory.db");
        // Create empty DB so session-delta proceeds
        fs::File::create(&db_path).unwrap();

        run_in(&hex_dir, Some("session-reflect-test-001"), true);

        let conn = rusqlite::Connection::open(&db_path).expect("open db");
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM eval_records WHERE session_id = 'session-reflect-test-001'",
                [],
                |row| row.get(0),
            )
            .expect("query eval_records");
        assert_eq!(count, 1, "one eval_record must be inserted");
    }
}
