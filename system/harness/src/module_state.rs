//! Enable/disable state for harness modules — `hex module enable|disable <name>`.
//!
//! Replaces ad-hoc per-feature gates (the agent-infra ARMED flag file, killed
//! 2026-06-11) with one platform primitive over the worker registry.
//!
//! STORAGE: SQLite at `$HEX_DIR/.hex/harness/state.db`, table
//! `disabled_modules` (row present = disabled). Deliberately NOT a file under
//! `.hex/config/` — Mike 2026-06-11: "Putting this control (or any real-time
//! ish control) into the config dir is confusing. Use a DB instead to hide
//! the impl." Config dir = human-edited declarative config; runtime controls
//! are opaque state reached only through the CLI.
//!
//! The harness runtime consults the store AT FIRE TIME (fresh read per fire),
//! so toggling needs no restart; a disabled module stays registered and
//! scheduled, but its fires log one loud skip line and do nothing.
//!
//! Failure stance (S6, loud + fail-open): an unreadable store must not
//! silently disable the whole harness (self-DoS) NOR silently run modules
//! Mike believes disabled without saying so — so a load error runs everything
//! (fail-open) while shouting on stderr and into telemetry on every fire
//! until fixed.

use rusqlite::Connection;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The harness-owned runtime-state database.
pub fn db_path(hex_dir: &Path) -> PathBuf {
    hex_dir.join(".hex").join("harness").join("state.db")
}

/// Open (creating dir/db/table as needed). Errors are strings so both the
/// CLI (hard-fail) and the runtime (loud fail-open) can phrase them.
fn open(hex_dir: &Path) -> Result<Connection, String> {
    let p = db_path(hex_dir);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let conn =
        Connection::open(&p).map_err(|e| format!("cannot open {}: {e}", p.display()))?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS disabled_modules (
            name       TEXT PRIMARY KEY,
            updated_at INTEGER NOT NULL
        );",
    )
    .map_err(|e| format!("state db schema ({}): {e}", p.display()))?;
    Ok(conn)
}

/// Every currently-disabled module name.
pub fn disabled_set(hex_dir: &Path) -> Result<BTreeSet<String>, String> {
    let conn = open(hex_dir)?;
    let mut stmt = conn
        .prepare("SELECT name FROM disabled_modules")
        .map_err(|e| format!("state db query: {e}"))?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| format!("state db query: {e}"))?;
    let mut out = BTreeSet::new();
    for r in rows {
        out.insert(r.map_err(|e| format!("state db row: {e}"))?);
    }
    Ok(out)
}

/// Disable (`disabled = true`) or enable a module. Returns whether the state
/// actually changed (idempotent calls return Ok(false)).
pub fn set_disabled(hex_dir: &Path, name: &str, disabled: bool) -> Result<bool, String> {
    let conn = open(hex_dir)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let changed = if disabled {
        conn.execute(
            "INSERT OR IGNORE INTO disabled_modules (name, updated_at) VALUES (?1, ?2)",
            rusqlite::params![name, now],
        )
        .map_err(|e| format!("state db write: {e}"))?
    } else {
        conn.execute(
            "DELETE FROM disabled_modules WHERE name = ?1",
            rusqlite::params![name],
        )
        .map_err(|e| format!("state db write: {e}"))?
    };
    Ok(changed > 0)
}

/// Fire-time check used by the harness runtime. Resolves the workspace like
/// the rest of the runtime ($HEX_DIR, else $HOME/hex). On a load error it
/// returns `false` (fail-open) after shouting — see module docs.
pub fn is_disabled(worker_name: &str) -> bool {
    let hex_dir = match std::env::var("HEX_DIR") {
        Ok(d) => PathBuf::from(d),
        Err(_) => match std::env::var("HOME") {
            Ok(h) => PathBuf::from(h).join("hex"),
            Err(_) => return false,
        },
    };
    match disabled_set(&hex_dir) {
        Ok(set) => set.contains(worker_name),
        Err(e) => {
            eprintln!(
                "hex harness serve: MODULE-STATE DB UNREADABLE ({e}) — failing OPEN: all modules run until the store is fixed"
            );
            crate::telemetry::record_loud(&crate::telemetry::TelemetryEvent {
                source: "harness".into(),
                event: "module_state.load".into(),
                status: "error".into(),
                duration_ms: None,
                exit_code: None,
                detail: Some(e),
            });
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_hex_dir() -> (tempfile::TempDir, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().to_path_buf();
        (d, p)
    }

    #[test]
    fn module_state_fresh_db_is_all_enabled() {
        let (_d, hex_dir) = tmp_hex_dir();
        assert!(disabled_set(&hex_dir).unwrap().is_empty());
    }

    #[test]
    fn module_state_disable_enable_roundtrip() {
        let (_d, hex_dir) = tmp_hex_dir();
        assert!(set_disabled(&hex_dir, "agent-infra-proposer", true).unwrap());
        assert!(set_disabled(&hex_dir, "agent-infra-auditor", true).unwrap());
        let set = disabled_set(&hex_dir).unwrap();
        assert!(set.contains("agent-infra-proposer"));
        assert!(set.contains("agent-infra-auditor"));
        // Re-enable one; the other stays.
        assert!(set_disabled(&hex_dir, "agent-infra-auditor", false).unwrap());
        let set = disabled_set(&hex_dir).unwrap();
        assert!(set.contains("agent-infra-proposer"));
        assert!(!set.contains("agent-infra-auditor"));
    }

    #[test]
    fn module_state_idempotent_calls_report_unchanged() {
        let (_d, hex_dir) = tmp_hex_dir();
        assert!(set_disabled(&hex_dir, "x", true).unwrap());
        assert!(!set_disabled(&hex_dir, "x", true).unwrap(), "second disable: no change");
        assert!(set_disabled(&hex_dir, "x", false).unwrap());
        assert!(!set_disabled(&hex_dir, "x", false).unwrap(), "second enable: no change");
    }

    #[test]
    fn module_state_corrupt_db_is_loud_err() {
        let (_d, hex_dir) = tmp_hex_dir();
        let p = db_path(&hex_dir);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "this is not a sqlite database, not even close").unwrap();
        let err = disabled_set(&hex_dir).unwrap_err();
        assert!(!err.is_empty());
    }
}
