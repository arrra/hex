//! Runtime rule registry — landed shadow-linter rules (P2 applier deliverable
//! 1; see `projects/agent-infra/decisions/p2-apply-mechanism-deterministic-applier-2026-06-12.md`).
//!
//! A single JSON file recording every rule the applier has landed into the
//! shadow linter (`lint_gates`). Default location:
//! `$HEX_DIR/projects/agent-infra/gates/landed-rules.json` — but every
//! function here takes an explicit path so tests never touch the real
//! workspace file.
//!
//! Contract (S6 — loud, never silent):
//! - Missing file => empty registry. This is NOT an error: day one, nothing
//!   has landed yet.
//! - Malformed file => hard error. A registry is the applier's source of
//!   truth for "what's live in the linter" — silently treating garbage as
//!   empty would let already-landed rules vanish from enforcement.
//! - Save is atomic: write to a tmp file in the same directory, then rename.
//! - Mutations are append-only or status-flip-only. Entries are NEVER
//!   deleted and history is NEVER rewritten — `revert` only flips `status`
//!   and stamps `reverted_ts`/`revert_reason` on the existing entry.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Lifecycle status of a landed rule entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleStatus {
    Active,
    Reverted,
}

/// One landed rule. Field order matches the spec contract exactly:
/// `{rule_id, pattern, proposal_id, verdict_sha256, landed_ts, status,
/// reverted_ts?, revert_reason?}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleEntry {
    pub rule_id: String,
    pub pattern: String,
    pub proposal_id: String,
    pub verdict_sha256: String,
    pub landed_ts: String,
    pub status: RuleStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reverted_ts: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revert_reason: Option<String>,
}

/// The full registry: an ordered, append-only list of [`RuleEntry`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RuleRegistry {
    #[serde(default)]
    pub entries: Vec<RuleEntry>,
}

impl RuleRegistry {
    /// Active (not reverted) entries, in landed order.
    pub fn active_entries(&self) -> impl Iterator<Item = &RuleEntry> {
        self.entries
            .iter()
            .filter(|e| e.status == RuleStatus::Active)
    }

    /// True if `rule_id` names a currently-active entry.
    pub fn has_active_rule_id(&self, rule_id: &str) -> bool {
        self.active_entries().any(|e| e.rule_id == rule_id)
    }

    /// Append a new entry. Mutation-only-append: never touches existing rows.
    pub fn append(&mut self, entry: RuleEntry) {
        self.entries.push(entry);
    }

    /// Flip the most recent ACTIVE entry for `rule_id` to `reverted`,
    /// stamping `reverted_ts`/`revert_reason`. The entry itself is never
    /// removed — this is a status flip, not a delete. Errs loudly if no
    /// active entry exists for `rule_id`.
    pub fn revert(&mut self, rule_id: &str, reason: &str, ts: &str) -> Result<(), RegistryError> {
        for e in self.entries.iter_mut().rev() {
            if e.rule_id == rule_id && e.status == RuleStatus::Active {
                e.status = RuleStatus::Reverted;
                e.reverted_ts = Some(ts.to_string());
                e.revert_reason = Some(reason.to_string());
                return Ok(());
            }
        }
        Err(RegistryError::Malformed(format!(
            "rule registry: no active entry for rule_id '{rule_id}' — nothing to revert"
        )))
    }
}

/// Errors surfaced by registry load/save. Per SO S6 every variant carries
/// enough context to log loudly.
#[derive(Debug)]
pub enum RegistryError {
    Io(std::io::Error),
    Malformed(String),
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegistryError::Io(e) => write!(f, "rule registry io error: {e}"),
            RegistryError::Malformed(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for RegistryError {}

impl From<std::io::Error> for RegistryError {
    fn from(e: std::io::Error) -> Self {
        RegistryError::Io(e)
    }
}

/// Default on-disk location of the rule registry, under `$HEX_DIR`. Mirrors
/// `ledger::default_path` — the crate's established hex-dir-relative-path
/// pattern; callers resolve `hex_dir` via the CLI's existing hex-dir helper
/// (`get_hex_dir()` in main.rs / `hex::env::detect_hex_dir`).
pub fn default_path(hex_dir: &Path) -> PathBuf {
    hex_dir.join("projects/agent-infra/gates/landed-rules.json")
}

/// Load the registry from `path`. Missing file => empty registry (NOT an
/// error). Malformed file => hard error (S6 — never silently treated as
/// empty; that would drop already-landed rules from enforcement).
pub fn load(path: &Path) -> Result<RuleRegistry, RegistryError> {
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(RuleRegistry::default()),
        Err(e) => return Err(RegistryError::Io(e)),
    };
    serde_json::from_str(&src).map_err(|e| {
        RegistryError::Malformed(format!(
            "rule registry {}: malformed JSON: {e}",
            path.display()
        ))
    })
}

/// Save the registry to `path` atomically: write a tmp file in the same
/// directory, then rename over the target. Creates the parent directory if
/// missing.
pub fn save(path: &Path, reg: &RuleRegistry) -> Result<(), RegistryError> {
    let json = serde_json::to_string_pretty(reg)
        .map_err(|e| RegistryError::Malformed(format!("rule registry: serialize failed: {e}")))?;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let tmp_name = format!(
        ".{}.tmp.{}",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("landed-rules.json"),
        std::process::id()
    );
    let tmp_path = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(&tmp_name),
        _ => PathBuf::from(&tmp_name),
    };
    std::fs::write(&tmp_path, json)?;
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(rule_id: &str, status: RuleStatus) -> RuleEntry {
        RuleEntry {
            rule_id: rule_id.to_string(),
            pattern: "foo.*bar".to_string(),
            proposal_id: "p-test".to_string(),
            verdict_sha256: "a".repeat(64),
            landed_ts: "2026-06-12T00:00:00Z".to_string(),
            status,
            reverted_ts: None,
            revert_reason: None,
        }
    }

    // -- load: missing / malformed -------------------------------------------

    #[test]
    fn rule_registry_missing_file_is_empty_not_error() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("nonexistent").join("landed-rules.json");
        let reg = load(&path).expect("missing file must not error");
        assert!(reg.entries.is_empty());
    }

    #[test]
    fn rule_registry_malformed_file_is_hard_error() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("landed-rules.json");
        std::fs::write(&path, "not json at all {{{").unwrap();
        let err = load(&path).unwrap_err();
        assert!(matches!(err, RegistryError::Malformed(_)));
        assert!(err.to_string().contains("malformed JSON"));
    }

    #[test]
    fn rule_registry_empty_object_without_entries_key_loads_empty() {
        // #[serde(default)] on `entries` means `{}` is a valid empty registry.
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("landed-rules.json");
        std::fs::write(&path, "{}").unwrap();
        let reg = load(&path).unwrap();
        assert!(reg.entries.is_empty());
    }

    // -- save / load roundtrip + atomicity -----------------------------------

    #[test]
    fn rule_registry_save_load_roundtrip() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("landed-rules.json");
        let mut reg = RuleRegistry::default();
        reg.append(entry("footgun-x", RuleStatus::Active));
        save(&path, &reg).unwrap();

        let loaded = load(&path).unwrap();
        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(loaded.entries[0].rule_id, "footgun-x");
        assert_eq!(loaded.entries[0].status, RuleStatus::Active);
    }

    #[test]
    fn rule_registry_save_creates_parent_dir() {
        let d = tempfile::tempdir().unwrap();
        let path = d
            .path()
            .join("nested")
            .join("dir")
            .join("landed-rules.json");
        let reg = RuleRegistry::default();
        save(&path, &reg).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn rule_registry_save_leaves_no_tmp_file_behind() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("landed-rules.json");
        let mut reg = RuleRegistry::default();
        reg.append(entry("footgun-x", RuleStatus::Active));
        save(&path, &reg).unwrap();

        let leftovers: Vec<_> = std::fs::read_dir(d.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp."))
            .collect();
        assert!(leftovers.is_empty(), "tmp files left behind: {leftovers:?}");
    }

    #[test]
    fn rule_registry_save_is_atomic_target_never_partially_written() {
        // Save twice in a row; the target must always be fully valid JSON,
        // never a half-written intermediate state (rename is atomic on the
        // same filesystem).
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("landed-rules.json");
        let mut reg = RuleRegistry::default();
        reg.append(entry("a", RuleStatus::Active));
        save(&path, &reg).unwrap();
        reg.append(entry("b", RuleStatus::Active));
        save(&path, &reg).unwrap();

        let loaded = load(&path).unwrap();
        assert_eq!(loaded.entries.len(), 2);
    }

    // -- append / revert: mutation-only-append, never delete -----------------

    #[test]
    fn rule_registry_append_preserves_existing_entries() {
        let mut reg = RuleRegistry::default();
        reg.append(entry("a", RuleStatus::Active));
        reg.append(entry("b", RuleStatus::Active));
        assert_eq!(reg.entries.len(), 2);
        assert_eq!(reg.entries[0].rule_id, "a");
        assert_eq!(reg.entries[1].rule_id, "b");
    }

    #[test]
    fn rule_registry_revert_flips_status_and_preserves_entry() {
        let mut reg = RuleRegistry::default();
        reg.append(entry("a", RuleStatus::Active));
        reg.revert("a", "false positive spree", "2026-06-13T00:00:00Z")
            .unwrap();

        assert_eq!(reg.entries.len(), 1, "entry must never be deleted");
        assert_eq!(reg.entries[0].status, RuleStatus::Reverted);
        assert_eq!(
            reg.entries[0].revert_reason.as_deref(),
            Some("false positive spree")
        );
        assert_eq!(
            reg.entries[0].reverted_ts.as_deref(),
            Some("2026-06-13T00:00:00Z")
        );
        // Original landed fields untouched — history not rewritten.
        assert_eq!(reg.entries[0].rule_id, "a");
        assert_eq!(reg.entries[0].landed_ts, "2026-06-12T00:00:00Z");
    }

    #[test]
    fn rule_registry_revert_unknown_rule_id_is_loud_error() {
        let mut reg = RuleRegistry::default();
        reg.append(entry("a", RuleStatus::Active));
        let err = reg.revert("does-not-exist", "why", "ts").unwrap_err();
        assert!(err.to_string().contains("no active entry"));
    }

    #[test]
    fn rule_registry_revert_already_reverted_rule_is_loud_error() {
        let mut reg = RuleRegistry::default();
        reg.append(entry("a", RuleStatus::Active));
        reg.revert("a", "first revert", "ts1").unwrap();
        let err = reg.revert("a", "second revert", "ts2").unwrap_err();
        assert!(err.to_string().contains("no active entry"));
    }

    // -- active_entries / has_active_rule_id ---------------------------------

    #[test]
    fn rule_registry_active_entries_excludes_reverted() {
        let mut reg = RuleRegistry::default();
        reg.append(entry("a", RuleStatus::Active));
        reg.append(entry("b", RuleStatus::Reverted));
        let active: Vec<_> = reg.active_entries().map(|e| e.rule_id.as_str()).collect();
        assert_eq!(active, vec!["a"]);
    }

    #[test]
    fn rule_registry_has_active_rule_id() {
        let mut reg = RuleRegistry::default();
        reg.append(entry("a", RuleStatus::Active));
        assert!(reg.has_active_rule_id("a"));
        assert!(!reg.has_active_rule_id("b"));
        reg.revert("a", "why", "ts").unwrap();
        assert!(
            !reg.has_active_rule_id("a"),
            "reverted rule is no longer active"
        );
    }

    // -- JSON shape matches the spec contract --------------------------------

    #[test]
    fn rule_registry_json_field_names_match_spec_contract() {
        let mut reg = RuleRegistry::default();
        reg.append(entry("a", RuleStatus::Active));
        let json = serde_json::to_value(&reg).unwrap();
        let e = &json["entries"][0];
        assert_eq!(e["status"], "active");
        assert!(e.get("rule_id").is_some());
        assert!(e.get("pattern").is_some());
        assert!(e.get("proposal_id").is_some());
        assert!(e.get("verdict_sha256").is_some());
        assert!(e.get("landed_ts").is_some());
        // Optional fields omitted when None (never emitted as null noise).
        assert!(e.get("reverted_ts").is_none());
        assert!(e.get("revert_reason").is_none());
    }

    #[test]
    fn rule_registry_default_path_is_hex_dir_relative() {
        let hex_dir = Path::new("/tmp/some-hex-dir");
        let p = default_path(hex_dir);
        assert_eq!(
            p,
            Path::new("/tmp/some-hex-dir/projects/agent-infra/gates/landed-rules.json")
        );
    }
}
