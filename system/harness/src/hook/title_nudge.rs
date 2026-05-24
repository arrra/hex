//! Emits a Happy session-title nudge when stamp file is missing or stale.
//! Returns None when no nudge is needed.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

const NUDGE_INTERVAL: Duration = Duration::from_secs(30 * 60);

pub fn maybe_nudge(prompt: &str) -> Option<String> {
    let session_id = std::env::var("CLAUDE_SESSION_ID").ok();
    let state_dir = dirs::home_dir().map(|h| h.join(".happy/state"));
    maybe_nudge_impl(session_id, prompt, state_dir)
}

fn maybe_nudge_impl(session_id: Option<String>, prompt: &str, state_dir: Option<PathBuf>) -> Option<String> {
    let session_id = session_id?;
    // Skip startup-flavored prompts so we don't title "hex startup".
    let trimmed = prompt.trim_start();
    if trimmed.starts_with("/hex-startup") || trimmed == "Invoke the hex-startup skill." {
        return None;
    }
    let state_dir = state_dir?;
    if std::fs::create_dir_all(&state_dir).is_err() {
        return None;
    }
    let stamp: PathBuf = state_dir.join(format!("{session_id}.last-title-at"));
    let needs_nudge = match std::fs::metadata(&stamp).and_then(|m| m.modified()) {
        Ok(mtime) => SystemTime::now()
            .duration_since(mtime)
            .map(|d| d >= NUDGE_INTERVAL)
            .unwrap_or(true),
        Err(_) => true, // missing or unreadable → first nudge
    };
    if !needs_nudge {
        return None;
    }
    Some(format!(
        "[system-reminder] Re-title this Happy session via the `mcp__happy__change_title` MCP tool. \
        Pick a 4-6 word focus descriptor based on what we're actually working on (NOT \"hex startup\" \
        or other startup boilerplate). Then touch the stamp file so this nudge doesn't fire again \
        for 30 minutes:\n  touch ~/.happy/state/{session_id}.last-title-at"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn make_state_dir(tmp: &TempDir) -> PathBuf {
        let dir = tmp.path().join("state");
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn stamp_path(state_dir: &PathBuf, session_id: &str) -> PathBuf {
        state_dir.join(format!("{session_id}.last-title-at"))
    }

    #[test]
    fn nudge_when_stamp_missing() {
        let tmp = TempDir::new().unwrap();
        let state_dir = make_state_dir(&tmp);
        let result = maybe_nudge_impl(
            Some("test-session-missing".into()),
            "real prompt",
            Some(state_dir),
        );
        assert!(result.is_some(), "expected nudge when stamp is absent");
    }

    #[test]
    fn no_nudge_when_stamp_fresh() {
        let tmp = TempDir::new().unwrap();
        let state_dir = make_state_dir(&tmp);
        let session_id = "test-session-fresh";
        fs::write(stamp_path(&state_dir, session_id), "").unwrap();
        let result = maybe_nudge_impl(
            Some(session_id.into()),
            "real prompt",
            Some(state_dir),
        );
        assert!(result.is_none(), "expected no nudge when stamp is fresh");
    }

    #[test]
    fn nudge_when_stamp_old() {
        let tmp = TempDir::new().unwrap();
        let state_dir = make_state_dir(&tmp);
        let session_id = "test-session-old";
        let stamp = stamp_path(&state_dir, session_id);
        fs::write(&stamp, "").unwrap();
        // Set mtime to 31 minutes ago.
        let old_time = SystemTime::now() - Duration::from_secs(31 * 60);
        filetime::set_file_mtime(&stamp, filetime::FileTime::from_system_time(old_time)).unwrap();
        let result = maybe_nudge_impl(
            Some(session_id.into()),
            "real prompt",
            Some(state_dir),
        );
        assert!(result.is_some(), "expected nudge when stamp is 31 min old");
    }

    #[test]
    fn no_nudge_for_hex_startup_prompt() {
        let tmp = TempDir::new().unwrap();
        let state_dir = make_state_dir(&tmp);
        let session_id = "test-session-startup";
        let r1 = maybe_nudge_impl(
            Some(session_id.into()),
            "/hex-startup",
            Some(state_dir.clone()),
        );
        assert!(r1.is_none(), "expected no nudge for /hex-startup");
        let r2 = maybe_nudge_impl(
            Some(session_id.into()),
            "Invoke the hex-startup skill.",
            Some(state_dir),
        );
        assert!(r2.is_none(), "expected no nudge for hex-startup skill invocation");
    }

    #[test]
    fn no_nudge_without_session_id() {
        let tmp = TempDir::new().unwrap();
        let state_dir = make_state_dir(&tmp);
        let result = maybe_nudge_impl(None, "real prompt", Some(state_dir));
        assert!(result.is_none(), "expected no nudge without session id");
    }
}
