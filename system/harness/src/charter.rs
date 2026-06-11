//! `hex charter` — ledger-anchored charter governance (mechanics, not convention).
//!
//! Decision: projects/agent-infra/decisions/charter-mechanics-ledger-anchored-2026-06-11.md
//! (Mike: "too soft and leaves too much up to convention. Let's create mechanics").
//!
//! The hash-chained ledger is the source of truth for charter content. Every
//! sanctioned change is a `charter.governance` action row carrying the name,
//! path, monotonic version, sha256 before/after, who, and why. `verify`
//! recomputes each registered charter's on-disk hash against the latest row:
//! any out-of-band edit — agent, session, or human, regardless of write path —
//! surfaces as DRIFT (nonzero exit; optional permanent ledger ALERT row).
//! History cannot be rewritten without breaking the ledger chain.
//!
//! This is detect-loudly, not prevent (Mike rejected commit-blocking hooks
//! 2026-06-05): a same-user editor can always write the file, but no edit can
//! hide. The amend path also parks files at mode 0444 as friction.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use rusqlite::{Connection, OpenFlags};

use crate::gatekeeper::sha256_hex;
use crate::ledger::{self, Ledger};

/// The action_class shared by every charter governance row.
pub const ACTION_CLASS: &str = "charter.governance";
/// The agent recorded on governance rows (the CLI is the actor; `by` in the
/// payload says who drove it).
pub const GOVERNANCE_AGENT: &str = "charter-keeper";

/// Latest recorded state of one registered charter (folded from the ledger).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharterState {
    pub name: String,
    /// Workspace-relative path as registered.
    pub path: String,
    pub version: u64,
    pub sha256: String,
    pub ts: i64,
}

/// One drifted charter found by [`verify`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drift {
    pub name: String,
    pub path: String,
    pub expected_sha256: String,
    /// `None` = file missing/unreadable (also drift — a vanished charter is
    /// the loudest possible edit).
    pub actual_sha256: Option<String>,
    pub version: u64,
}

fn open_ro(db: &Path) -> Result<Connection> {
    Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| anyhow!("charter: open ledger {}: {e}", db.display()))
}

fn file_sha256(path: &Path) -> Result<String> {
    let body = std::fs::read_to_string(path)
        .map_err(|e| anyhow!("charter: read {}: {e}", path.display()))?;
    Ok(sha256_hex(&body))
}

/// Fold all governance rows into the latest state per charter name.
/// Returns names in BTreeMap order (stable for display).
pub fn latest_states(hex_dir: &Path) -> Result<BTreeMap<String, CharterState>> {
    let db = ledger::default_path(hex_dir);
    if !db.exists() {
        return Ok(BTreeMap::new());
    }
    let conn = open_ro(&db)?;
    let mut stmt = conn.prepare(
        "SELECT ts, payload FROM ledger \
         WHERE action_class = ?1 AND kind = 'action' ORDER BY id ASC",
    )?;
    let rows = stmt.query_map([ACTION_CLASS], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
    })?;
    let mut out: BTreeMap<String, CharterState> = BTreeMap::new();
    for row in rows {
        let (ts, payload_raw) = row?;
        let p: serde_json::Value = match serde_json::from_str(&payload_raw) {
            Ok(v) => v,
            Err(e) => {
                // Malformed governance rows are loud, never silently skipped (S6).
                eprintln!("charter: MALFORMED governance row payload skipped: {e}");
                continue;
            }
        };
        let (Some(name), Some(path), Some(version), Some(sha)) = (
            p.get("name").and_then(|v| v.as_str()),
            p.get("path").and_then(|v| v.as_str()),
            p.get("version").and_then(|v| v.as_u64()),
            p.get("sha256_after").and_then(|v| v.as_str()),
        ) else {
            eprintln!("charter: governance row missing required fields, skipped: {payload_raw}");
            continue;
        };
        out.insert(
            name.to_string(),
            CharterState {
                name: name.to_string(),
                path: path.to_string(),
                version,
                sha256: sha.to_string(),
                ts,
            },
        );
    }
    Ok(out)
}

fn abs_path(hex_dir: &Path, rel: &str) -> PathBuf {
    hex_dir.join(rel)
}

fn set_readonly(path: &Path, readonly: bool) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if readonly { 0o444 } else { 0o644 };
        if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)) {
            eprintln!("charter: chmod {:o} {}: {e}", mode, path.display());
        }
    }
}

fn append_row_kind(hex_dir: &Path, kind: &str, payload: &serde_json::Value) -> Result<()> {
    let db = ledger::default_path(hex_dir);
    let l = Ledger::open(&db).map_err(|e| anyhow!("charter: open ledger for append: {e}"))?;
    l.append(GOVERNANCE_AGENT, ACTION_CLASS, kind, payload)
        .map_err(|e| anyhow!("charter: ledger append: {e}"))?;
    Ok(())
}

fn append_row(hex_dir: &Path, payload: &serde_json::Value) -> Result<()> {
    append_row_kind(hex_dir, "action", payload)
}

/// Register a charter file: the genesis row anchoring its CURRENT content.
/// Refuses if the name is already registered (use `amend`/`rebaseline`).
pub fn register(hex_dir: &Path, name: &str, rel_path: &str, by: &str, why: &str) -> Result<CharterState> {
    if latest_states(hex_dir)?.contains_key(name) {
        return Err(anyhow!(
            "charter: '{name}' is already registered — use amend (or rebaseline after drift)"
        ));
    }
    let path = abs_path(hex_dir, rel_path);
    let sha = file_sha256(&path)?;
    append_row(
        hex_dir,
        &serde_json::json!({
            "op": "register", "name": name, "path": rel_path, "version": 1,
            "sha256_before": null, "sha256_after": sha, "by": by, "why": why,
        }),
    )?;
    set_readonly(&path, true);
    Ok(CharterState { name: name.into(), path: rel_path.into(), version: 1, sha256: sha, ts: 0 })
}

/// Amend a registered charter: the ONLY sanctioned write path. Refuses if the
/// on-disk file has drifted from the last recorded hash — reconcile first
/// (rebaseline) so the trail never silently absorbs an out-of-band edit.
pub fn amend(hex_dir: &Path, name: &str, new_content_file: &Path, by: &str, why: &str) -> Result<CharterState> {
    let states = latest_states(hex_dir)?;
    let cur = states
        .get(name)
        .ok_or_else(|| anyhow!("charter: '{name}' is not registered"))?;
    let path = abs_path(hex_dir, &cur.path);
    let on_disk = file_sha256(&path)?;
    if on_disk != cur.sha256 {
        return Err(anyhow!(
            "charter: '{name}' has DRIFTED on disk (expected {}, found {on_disk}) — \
             refusing to amend over an unrecorded edit. Inspect the diff, then either \
             restore the recorded content or `hex charter rebaseline {name} --why ...` \
             to accept the drift into the trail.",
            cur.sha256
        ));
    }
    let new_body = std::fs::read_to_string(new_content_file)
        .map_err(|e| anyhow!("charter: read new content {}: {e}", new_content_file.display()))?;
    let new_sha = sha256_hex(&new_body);
    if new_sha == cur.sha256 {
        return Err(anyhow!("charter: '{name}' new content is identical to v{} — nothing to amend", cur.version));
    }
    set_readonly(&path, false);
    std::fs::write(&path, &new_body)
        .map_err(|e| anyhow!("charter: write {}: {e}", path.display()))?;
    set_readonly(&path, true);
    let version = cur.version + 1;
    append_row(
        hex_dir,
        &serde_json::json!({
            "op": "amend", "name": name, "path": cur.path, "version": version,
            "sha256_before": cur.sha256, "sha256_after": new_sha, "by": by, "why": why,
        }),
    )?;
    Ok(CharterState { name: name.into(), path: cur.path.clone(), version, sha256: new_sha, ts: 0 })
}

/// Accept an out-of-band edit into the trail, explicitly and loudly. The row
/// is marked `op=rebaseline` + `drift_accepted=true` so the audit trail shows
/// exactly where governance was bypassed and then reconciled.
pub fn rebaseline(hex_dir: &Path, name: &str, by: &str, why: &str) -> Result<CharterState> {
    let states = latest_states(hex_dir)?;
    let cur = states
        .get(name)
        .ok_or_else(|| anyhow!("charter: '{name}' is not registered"))?;
    let path = abs_path(hex_dir, &cur.path);
    let on_disk = file_sha256(&path)?;
    if on_disk == cur.sha256 {
        return Err(anyhow!("charter: '{name}' has not drifted — nothing to rebaseline"));
    }
    let version = cur.version + 1;
    append_row(
        hex_dir,
        &serde_json::json!({
            "op": "rebaseline", "name": name, "path": cur.path, "version": version,
            "sha256_before": cur.sha256, "sha256_after": on_disk, "by": by, "why": why,
            "drift_accepted": true,
        }),
    )?;
    set_readonly(&path, true);
    Ok(CharterState { name: name.into(), path: cur.path.clone(), version, sha256: on_disk, ts: 0 })
}

/// Verify every registered charter against the ledger. Returns the drifted
/// ones (empty = clean). With `alert`, each drift also appends a permanent
/// ledger ALERT row — wired into the nightly vehicle and `hex doctor`.
pub fn verify(hex_dir: &Path, alert: bool) -> Result<Vec<Drift>> {
    let mut drifts = Vec::new();
    for (name, st) in latest_states(hex_dir)? {
        let path = abs_path(hex_dir, &st.path);
        let actual = file_sha256(&path).ok();
        if actual.as_deref() != Some(st.sha256.as_str()) {
            let d = Drift {
                name: name.clone(),
                path: st.path.clone(),
                expected_sha256: st.sha256.clone(),
                actual_sha256: actual,
                version: st.version,
            };
            eprintln!(
                "charter: DRIFT on '{}' (v{}): expected {}, found {} — out-of-band edit \
                 (restore the recorded content or `hex charter rebaseline`)",
                d.name,
                d.version,
                d.expected_sha256,
                d.actual_sha256.as_deref().unwrap_or("<file missing/unreadable>"),
            );
            if alert {
                append_row_kind(
                    hex_dir,
                    "alert",
                    &serde_json::json!({
                        "op": "drift-alert", "name": d.name, "path": d.path,
                        "version": d.version, "expected": d.expected_sha256,
                        "found": d.actual_sha256,
                    }),
                )
                .unwrap_or_else(|e| eprintln!("charter: drift ALERT append failed: {e}"));
            }
            drifts.push(d);
        }
    }
    Ok(drifts)
}

/// The governance trail for one charter (or all, when `name` is None),
/// oldest first: (ts, payload) pairs straight from the ledger.
pub fn log(hex_dir: &Path, name: Option<&str>) -> Result<Vec<(i64, serde_json::Value)>> {
    let db = ledger::default_path(hex_dir);
    if !db.exists() {
        return Ok(Vec::new());
    }
    let conn = open_ro(&db)?;
    let mut stmt = conn.prepare(
        "SELECT ts, payload FROM ledger WHERE action_class = ?1 ORDER BY id ASC",
    )?;
    let rows = stmt.query_map([ACTION_CLASS], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (ts, raw) = row?;
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap_or(serde_json::json!({"malformed": raw}));
        if let Some(n) = name {
            if v.get("name").and_then(|x| x.as_str()) != Some(n) {
                continue;
            }
        }
        out.push((ts, v));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let hex_dir = dir.path().to_path_buf();
        std::fs::create_dir_all(hex_dir.join("charters")).unwrap();
        std::fs::write(hex_dir.join("charters/proposer.md"), "# Proposer v1\n").unwrap();
        (dir, hex_dir)
    }

    #[test]
    fn register_amend_verify_log_roundtrip() {
        let (_g, hex_dir) = setup();
        let st = register(&hex_dir, "proposer", "charters/proposer.md", "test", "genesis").unwrap();
        assert_eq!(st.version, 1);
        // Clean right after register.
        assert!(verify(&hex_dir, false).unwrap().is_empty());
        // Double-register refused.
        assert!(register(&hex_dir, "proposer", "charters/proposer.md", "test", "again").is_err());

        // Sanctioned amend: new content via the CLI path only.
        let new = hex_dir.join("new.md");
        std::fs::write(&new, "# Proposer v2\n").unwrap();
        let st2 = amend(&hex_dir, "proposer", &new, "test", "update").unwrap();
        assert_eq!(st2.version, 2);
        assert_eq!(
            std::fs::read_to_string(hex_dir.join("charters/proposer.md")).unwrap(),
            "# Proposer v2\n"
        );
        assert!(verify(&hex_dir, false).unwrap().is_empty());

        // The trail shows both ops, oldest first.
        let trail = log(&hex_dir, Some("proposer")).unwrap();
        assert_eq!(trail.len(), 2);
        assert_eq!(trail[0].1["op"], "register");
        assert_eq!(trail[1].1["op"], "amend");
        assert_eq!(trail[1].1["version"], 2);

        // Identical re-amend refused.
        assert!(amend(&hex_dir, "proposer", &new, "test", "noop").is_err());
    }

    #[test]
    fn out_of_band_edit_is_drift_and_blocks_amend() {
        let (_g, hex_dir) = setup();
        register(&hex_dir, "proposer", "charters/proposer.md", "test", "genesis").unwrap();
        // Tamper out-of-band (flip writable first, as any editor would).
        let p = hex_dir.join("charters/proposer.md");
        set_readonly(&p, false);
        std::fs::write(&p, "# Proposer TAMPERED\n").unwrap();

        let drifts = verify(&hex_dir, false).unwrap();
        assert_eq!(drifts.len(), 1);
        assert_eq!(drifts[0].name, "proposer");
        assert!(drifts[0].actual_sha256.is_some());

        // Amend over drift refused — the trail never silently absorbs it.
        let new = hex_dir.join("new.md");
        std::fs::write(&new, "# Proposer v2\n").unwrap();
        let err = amend(&hex_dir, "proposer", &new, "test", "update").unwrap_err();
        assert!(err.to_string().contains("DRIFTED"), "{err}");

        // Rebaseline accepts the drift explicitly and loudly.
        let st = rebaseline(&hex_dir, "proposer", "test", "accepting reviewed drift").unwrap();
        assert_eq!(st.version, 2);
        assert!(verify(&hex_dir, false).unwrap().is_empty());
        let trail = log(&hex_dir, Some("proposer")).unwrap();
        assert_eq!(trail[1].1["op"], "rebaseline");
        assert_eq!(trail[1].1["drift_accepted"], true);
        // Rebaseline without drift refused.
        assert!(rebaseline(&hex_dir, "proposer", "test", "noop").is_err());
    }

    #[test]
    fn missing_file_is_drift_and_alert_rows_land() {
        let (_g, hex_dir) = setup();
        register(&hex_dir, "proposer", "charters/proposer.md", "test", "genesis").unwrap();
        let p = hex_dir.join("charters/proposer.md");
        set_readonly(&p, false);
        std::fs::remove_file(&p).unwrap();
        let drifts = verify(&hex_dir, true).unwrap();
        assert_eq!(drifts.len(), 1);
        assert!(drifts[0].actual_sha256.is_none());
        // The drift ALERT row landed in the governance trail.
        let trail = log(&hex_dir, Some("proposer")).unwrap();
        assert!(trail.iter().any(|(_, v)| v["op"] == "drift-alert"));
        // Ledger chain still verifies (alert rows are chained like any other).
        assert!(crate::ledger::verify(crate::ledger::default_path(&hex_dir)).is_ok());
    }

    #[test]
    fn amend_unregistered_refused_and_empty_ledger_is_clean() {
        let (_g, hex_dir) = setup();
        assert!(verify(&hex_dir, false).unwrap().is_empty());
        let new = hex_dir.join("new.md");
        std::fs::write(&new, "x").unwrap();
        assert!(amend(&hex_dir, "ghost", &new, "test", "no").is_err());
    }
}
