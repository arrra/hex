use std::fs;
use std::io::Write;
use std::path::PathBuf;

/// Port of .hex/scripts/session-reflect.sh
/// Post-session reflection orchestrator: appends a timestamped entry to
/// evolution/reflection-log.md, then optionally runs session-delta.py.
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

    let delta_script = hex_dir.join("evolution/eval/session-delta.py");
    let memory_db = hex_dir.join(".hex/memory.db");
    if delta_script.exists() && memory_db.exists() {
        let sid = session_id.unwrap_or("");
        let status = std::process::Command::new("python3")
            .arg(&delta_script)
            .arg("--session-id")
            .arg(sid)
            .env("HEX_DIR", hex_dir)
            .status();
        // Mirror shell `|| true`: ignore failure
        if let Err(e) = status {
            eprintln!("session-reflect: session-delta.py failed to launch: {e}");
        }
    }

    if !quiet {
        println!("session-reflect: done");
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
        // No delta script, no memory.db — should complete without error
        run_in(&hex_dir, Some("abc"), true);
    }
}
