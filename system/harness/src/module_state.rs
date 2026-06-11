//! Enable/disable state for harness modules — `hex module enable|disable <name>`.
//!
//! Replaces ad-hoc per-feature gates (the agent-infra ARMED flag file, killed
//! 2026-06-11: "I don't like that. Just remove this feature flag altogether.
//! If anything, we should be able to enable and disable hex modules via the
//! cli") with one platform primitive over the worker registry.
//!
//! State: `$HEX_DIR/.hex/config/modules-disabled.json` →
//! `{ "disabled": ["worker-name", ...] }`. Only the CLI writes it. The harness
//! runtime consults it AT FIRE TIME (fresh read per fire), so toggling needs
//! no restart; a disabled module stays registered and scheduled, but its fires
//! log one loud skip line and do nothing.
//!
//! Failure stance (S6, loud + fail-open): a malformed store must not silently
//! disable the whole harness (self-DoS) NOR silently run modules Mike believes
//! disabled without saying so — so a load error runs everything (fail-open)
//! while shouting on stderr and into telemetry on every fire until fixed.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// On-disk shape. Kept trivially small and typed.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct DisabledStore {
    #[serde(default)]
    pub disabled: BTreeSet<String>,
}

/// Store location under a hex workspace.
pub fn store_path(hex_dir: &Path) -> PathBuf {
    hex_dir
        .join(".hex")
        .join("config")
        .join("modules-disabled.json")
}

/// Load the disabled set. Absent file = empty set (everything enabled).
/// Malformed file = Err — callers decide loud-fail (CLI) vs loud-fail-open
/// (runtime).
pub fn load(hex_dir: &Path) -> Result<BTreeSet<String>, String> {
    let p = store_path(hex_dir);
    let src = match std::fs::read_to_string(&p) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(e) => return Err(format!("cannot read {}: {e}", p.display())),
    };
    let store: DisabledStore = serde_json::from_str(&src)
        .map_err(|e| format!("malformed {}: {e}", p.display()))?;
    Ok(store.disabled)
}

/// Persist the disabled set (pretty JSON, parent dir created).
pub fn save(hex_dir: &Path, disabled: &BTreeSet<String>) -> Result<(), String> {
    let p = store_path(hex_dir);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let store = DisabledStore {
        disabled: disabled.clone(),
    };
    let json = serde_json::to_string_pretty(&store)
        .map_err(|e| format!("serialize disabled store: {e}"))?;
    std::fs::write(&p, json).map_err(|e| format!("cannot write {}: {e}", p.display()))
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
    match load(&hex_dir) {
        Ok(set) => set.contains(worker_name),
        Err(e) => {
            eprintln!(
                "hex harness serve: DISABLED-STORE UNREADABLE ({e}) — failing OPEN: all modules run until the store is fixed"
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
    fn module_state_absent_file_is_all_enabled() {
        let (_d, hex_dir) = tmp_hex_dir();
        assert!(load(&hex_dir).unwrap().is_empty());
    }

    #[test]
    fn module_state_roundtrip_persists() {
        let (_d, hex_dir) = tmp_hex_dir();
        let mut set = BTreeSet::new();
        set.insert("agent-infra-proposer".to_string());
        set.insert("agent-infra-auditor".to_string());
        save(&hex_dir, &set).unwrap();
        let back = load(&hex_dir).unwrap();
        assert_eq!(back, set);
        // Removing one and re-saving sticks.
        let mut set2 = back;
        set2.remove("agent-infra-auditor");
        save(&hex_dir, &set2).unwrap();
        let back2 = load(&hex_dir).unwrap();
        assert!(back2.contains("agent-infra-proposer"));
        assert!(!back2.contains("agent-infra-auditor"));
    }

    #[test]
    fn module_state_malformed_store_is_loud_err() {
        let (_d, hex_dir) = tmp_hex_dir();
        let p = store_path(&hex_dir);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "{ not json").unwrap();
        let err = load(&hex_dir).unwrap_err();
        assert!(err.contains("malformed"), "got: {err}");
    }

    #[test]
    fn module_state_empty_object_is_valid_and_empty() {
        let (_d, hex_dir) = tmp_hex_dir();
        let p = store_path(&hex_dir);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "{}").unwrap();
        assert!(load(&hex_dir).unwrap().is_empty());
    }
}
