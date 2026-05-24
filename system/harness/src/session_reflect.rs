use std::fs;
use std::io::Write;
use std::path::PathBuf;

/// Post-session reflection — Rust port of the former session-reflect script.
/// Post-session reflection orchestrator: appends a timestamped entry to
/// evolution/reflection-log.md, then calls session_delta directly in Rust.
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

    let reflection_log = hex_dir.join("evolution/reflection-log.md");
    if let Some(parent) = reflection_log.parent() {
        fs::create_dir_all(parent).unwrap_or_else(|e| {
            eprintln!("session-reflect: cannot create log dir: {e}");
            std::process::exit(1);
        });
    }

    let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();

    let mut entry = format!("\n## {} — session reflection\n", timestamp);
    if let Some(id) = session_id {
        if !id.is_empty() {
            entry.push_str(&format!("Session: {}\n", id));
        }
    }
    entry.push_str("(reflection placeholder — see observations.md)\n");

    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&reflection_log)
        .unwrap_or_else(|e| {
            eprintln!("session-reflect: cannot open reflection log: {e}");
            std::process::exit(1);
        });
    file.write_all(entry.as_bytes()).unwrap_or_else(|e| {
        eprintln!("session-reflect: cannot write reflection log: {e}");
        std::process::exit(1);
    });

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
    fn appends_timestamped_entry_to_log() {
        let dir = tempfile::tempdir().expect("tempdir");
        let hex_dir = dir.path().to_path_buf();
        fs::create_dir_all(hex_dir.join("evolution")).unwrap();

        run_in(&hex_dir, Some("test-session-123"), true);

        let log_path = hex_dir.join("evolution/reflection-log.md");
        assert!(log_path.exists(), "reflection-log.md must be created");

        let mut contents = String::new();
        fs::File::open(&log_path)
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();

        assert!(contents.contains("session reflection"), "log must contain 'session reflection'");
        assert!(contents.contains("test-session-123"), "log must contain session id");
        assert!(contents.contains("reflection placeholder"), "log must contain placeholder");
    }

    #[test]
    fn no_session_id_omits_session_line() {
        let dir = tempfile::tempdir().expect("tempdir");
        let hex_dir = dir.path().to_path_buf();
        fs::create_dir_all(hex_dir.join("evolution")).unwrap();

        run_in(&hex_dir, None, true);

        let log_path = hex_dir.join("evolution/reflection-log.md");
        let mut contents = String::new();
        fs::File::open(&log_path)
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();

        assert!(!contents.contains("Session:"), "no session id should produce no 'Session:' line");
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
