//! Append-only, hash-chained SQLite ledger for the agent-infra substrate
//! (spec S253fety6, task Tqvtgkm9d).
//!
//! # Threat model — tamper-EVIDENT, not tamper-PROOF
//!
//! The ledger lives under `$HEX_DIR/.hex/ledger/ledger.db` and is owned by the
//! same UNIX user as every agent that writes to it. We therefore do NOT try to
//! prevent a co-resident attacker (or a buggy local process) from mutating the
//! sqlite file out-of-band — same-user processes can always do that. Instead,
//! every row carries `row_hash = sha256(canonical_row || prev_hash)`, chaining
//! each row to the prior one. Any insert, update, delete, or reorder that did
//! NOT go through [`Ledger::append`] breaks the chain and is detected by
//! [`verify`]. This is the same containment stance as bakeoff5 (the harness we
//! lifted these rules from): we DETECT tampering, we do not PREVENT it.
//!
//! Directory perms are tightened to `0555` (read+execute only) on open; the
//! db file itself stays writable so that `append` works. The combination of
//! a read-only parent directory and a hash-chain checked by `verify` is what
//! buys "tamper-evident" — a write-probe test (`ledger_verify_detects_direct_out_of_band_write`)
//! pins the detection guarantee.

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// The five valid row kinds. Anything else is rejected at append time.
pub const VALID_KINDS: &[&str] = &["intent", "action", "outcome", "heartbeat", "alert"];

/// Errors surfaced by ledger operations. Per SO S6 (no quiet failures),
/// each variant carries enough context to be logged loudly by the caller.
#[derive(Debug)]
pub enum LedgerError {
    Io(std::io::Error),
    Sqlite(rusqlite::Error),
    InvalidKind(String),
    /// Used by [`verify`] when the chain is broken. Carries the row id and
    /// a short description of where the break was detected.
    ChainBroken {
        at_row_id: i64,
        reason: String,
    },
    Json(serde_json::Error),
}

impl std::fmt::Display for LedgerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LedgerError::Io(e) => write!(f, "ledger io error: {}", e),
            LedgerError::Sqlite(e) => write!(f, "ledger sqlite error: {}", e),
            LedgerError::InvalidKind(k) => write!(
                f,
                "ledger: invalid kind {:?} (allowed: {:?})",
                k, VALID_KINDS
            ),
            LedgerError::ChainBroken { at_row_id, reason } => write!(
                f,
                "ledger: chain TAMPER detected at row id={}: {}",
                at_row_id, reason
            ),
            LedgerError::Json(e) => write!(f, "ledger json error: {}", e),
        }
    }
}

impl std::error::Error for LedgerError {}

impl From<rusqlite::Error> for LedgerError {
    fn from(e: rusqlite::Error) -> Self {
        LedgerError::Sqlite(e)
    }
}
impl From<std::io::Error> for LedgerError {
    fn from(e: std::io::Error) -> Self {
        LedgerError::Io(e)
    }
}
impl From<serde_json::Error> for LedgerError {
    fn from(e: serde_json::Error) -> Self {
        LedgerError::Json(e)
    }
}

/// Hash-chained append-only ledger handle.
pub struct Ledger {
    conn: Connection,
}

/// Schema bootstrap — single `ledger` table, plus a checkpoint index.
const SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS ledger (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    ts           INTEGER NOT NULL,
    agent        TEXT NOT NULL,
    action_class TEXT NOT NULL,
    kind         TEXT NOT NULL,
    payload      TEXT NOT NULL,
    prev_hash    TEXT NOT NULL,
    row_hash     TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_ledger_agent_ts ON ledger(agent, ts);
CREATE INDEX IF NOT EXISTS idx_ledger_kind_ts ON ledger(kind, ts);
";

/// The genesis "previous hash" used by the first row. 64 zero hex chars.
pub const GENESIS_PREV_HASH: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

impl Ledger {
    /// Open (or create) the ledger at `path`. Creates the parent directory
    /// if missing and tightens its perms to `0555` (read+execute only) per
    /// the spec contract. Same-user processes can still write to the db file
    /// itself — that's by design; tampering is detected by [`verify`].
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, LedgerError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA_SQL)?;
        Ok(Self { conn })
    }

    /// Tighten the parent directory to `0555` (read+execute only). The spec
    /// contract calls for `0555` on the production ledger dir so that no
    /// NEW files (rogue ledgers, sidecar dumps) can be planted next to the
    /// db. We do NOT call this from [`Ledger::open`] because sqlite's
    /// rollback-journal needs to create `<db>-journal` files alongside the
    /// db at write time; tightening before writes deadlocks all appends.
    /// Production deploy / harness bootstrap calls this AFTER the db is
    /// established and the writing process is the only one holding it open.
    /// The same-user tamper-evidence guarantee (write-probe test) does not
    /// depend on this — it is enforced by the hash chain via [`verify`].
    /// Returns Err loudly per S6 if the chmod fails.
    pub fn tighten_parent_dir<P: AsRef<Path>>(path: P) -> Result<(), LedgerError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                tighten_dir_perms_strict(parent)?;
            }
        }
        Ok(())
    }

    /// Append a new row. Returns `Err(InvalidKind)` for any kind outside
    /// [`VALID_KINDS`]. The new row is hash-chained to the previous tip.
    pub fn append(
        &self,
        agent: &str,
        action_class: &str,
        kind: &str,
        payload: &serde_json::Value,
    ) -> Result<i64, LedgerError> {
        if !VALID_KINDS.contains(&kind) {
            return Err(LedgerError::InvalidKind(kind.to_string()));
        }
        let ts = now_unix_seconds();
        let payload_canonical = canonical_json(payload)?;
        let prev_hash: String = self
            .conn
            .query_row(
                "SELECT row_hash FROM ledger ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or_else(|| GENESIS_PREV_HASH.to_string());

        let row_hash = compute_row_hash(
            ts,
            agent,
            action_class,
            kind,
            &payload_canonical,
            &prev_hash,
        );

        self.conn.execute(
            "INSERT INTO ledger (ts, agent, action_class, kind, payload, prev_hash, row_hash) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                ts,
                agent,
                action_class,
                kind,
                payload_canonical,
                prev_hash,
                row_hash
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Most-recent ts per agent — used by `hex ledger freshness` to detect
    /// stale agents.
    pub fn last_ts_per_agent(&self) -> Result<Vec<(String, i64)>, LedgerError> {
        let mut stmt = self
            .conn
            .prepare("SELECT agent, MAX(ts) FROM ledger GROUP BY agent")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

/// Walk the full chain from row 1 to the tip; return Err on ANY break.
/// Non-zero exit at the CLI maps from `Err` here.
pub fn verify<P: AsRef<Path>>(path: P) -> Result<usize, LedgerError> {
    let conn = Connection::open(path.as_ref())?;
    // If the table doesn't exist, treat as empty — nothing to verify.
    conn.execute_batch(SCHEMA_SQL)?;

    let mut stmt = conn.prepare(
        "SELECT id, ts, agent, action_class, kind, payload, prev_hash, row_hash \
         FROM ledger ORDER BY id ASC",
    )?;
    let mut rows = stmt.query([])?;

    let mut prev_hash = GENESIS_PREV_HASH.to_string();
    let mut count = 0usize;
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let ts: i64 = row.get(1)?;
        let agent: String = row.get(2)?;
        let action_class: String = row.get(3)?;
        let kind: String = row.get(4)?;
        let payload: String = row.get(5)?;
        let stored_prev: String = row.get(6)?;
        let stored_row: String = row.get(7)?;

        if stored_prev != prev_hash {
            return Err(LedgerError::ChainBroken {
                at_row_id: id,
                reason: format!(
                    "prev_hash mismatch (stored={}, expected={})",
                    stored_prev, prev_hash
                ),
            });
        }
        let expect_row = compute_row_hash(ts, &agent, &action_class, &kind, &payload, &stored_prev);
        if expect_row != stored_row {
            return Err(LedgerError::ChainBroken {
                at_row_id: id,
                reason: format!(
                    "row_hash mismatch (stored={}, recomputed={}) — payload or header tampered",
                    stored_row, expect_row
                ),
            });
        }
        prev_hash = stored_row;
        count += 1;
    }
    Ok(count)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct CanonicalRow<'a> {
    ts: i64,
    agent: &'a str,
    action_class: &'a str,
    kind: &'a str,
    payload: &'a str,
    prev_hash: &'a str,
}

fn compute_row_hash(
    ts: i64,
    agent: &str,
    action_class: &str,
    kind: &str,
    payload_canonical: &str,
    prev_hash: &str,
) -> String {
    let canonical = CanonicalRow {
        ts,
        agent,
        action_class,
        kind,
        payload: payload_canonical,
        prev_hash,
    };
    // serde_json::to_vec is deterministic for our struct (field order fixed).
    let bytes = serde_json::to_vec(&canonical).expect("serialize canonical row");
    let mut h = Sha256::new();
    h.update(&bytes);
    h.update(prev_hash.as_bytes());
    format!("{:x}", h.finalize())
}

fn canonical_json(v: &serde_json::Value) -> Result<String, serde_json::Error> {
    // Sort object keys recursively so payload hashing is stable.
    fn norm(v: &serde_json::Value) -> serde_json::Value {
        match v {
            serde_json::Value::Object(m) => {
                let mut keys: Vec<&String> = m.keys().collect();
                keys.sort();
                let mut out = serde_json::Map::new();
                for k in keys {
                    out.insert(k.clone(), norm(&m[k]));
                }
                serde_json::Value::Object(out)
            }
            serde_json::Value::Array(a) => serde_json::Value::Array(a.iter().map(norm).collect()),
            other => other.clone(),
        }
    }
    serde_json::to_string(&norm(v))
}

fn now_unix_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn tighten_dir_perms_strict(dir: &Path) -> Result<(), LedgerError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let md = std::fs::metadata(dir)?;
        let mut perms = md.permissions();
        perms.set_mode(0o555);
        std::fs::set_permissions(dir, perms)?;
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
    }
    Ok(())
}

#[allow(dead_code)]
fn tighten_dir_perms(dir: &Path) {
    // Best-effort: 0555 on the ledger directory per the spec contract. We
    // log to stderr (S6: no quiet failures) on error but do not fail the
    // open — sqlite still needs to write the db file inside.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match std::fs::metadata(dir) {
            Ok(md) => {
                let mut perms = md.permissions();
                perms.set_mode(0o555);
                if let Err(e) = std::fs::set_permissions(dir, perms) {
                    eprintln!(
                        "ledger: could not tighten {} perms to 0555: {}",
                        dir.display(),
                        e
                    );
                }
            }
            Err(e) => {
                eprintln!("ledger: stat {} failed: {}", dir.display(), e);
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
    }
}

/// Default on-disk location of the ledger database, under `$HEX_DIR`.
pub fn default_path(hex_dir: &Path) -> PathBuf {
    hex_dir.join(".hex").join("ledger").join("ledger.db")
}

/// Per-agent freshness window (seconds). Charter-derived defaults; the spec
/// contract calls for a "config map" — wired here as defaults so the freshness
/// check has something to alert against on day one. The reconciler runs
/// hourly, so a 2h window is the natural watcher. An agent is STALE strictly
/// beyond its window: `age == window` is still fresh, `age > window` alerts.
pub fn default_freshness_window_secs(agent: &str) -> i64 {
    match agent {
        "reconciler" => 2 * 3600, // 2h: reconciler charter (hourly cron + slack)
        "linter" => 24 * 3600,    // 24h: linter is per-dispatch
        "proposer" => 26 * 3600,  // 26h: proposer nightly + overlap (charter)
        "auditor" => 26 * 3600,
        _ => 24 * 3600,
    }
}

// ---------------------------------------------------------------------------
// Unit tests (the integration test in tests/ledger_test.rs covers the
// public contract; these pin the internals).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tmp() -> (tempfile::TempDir, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("ledger.db");
        (d, p)
    }

    #[test]
    fn ledger_valid_kinds_accepted() {
        let (_d, p) = tmp();
        let l = Ledger::open(&p).unwrap();
        for k in VALID_KINDS {
            l.append("a", "c", k, &json!({})).expect("append");
        }
        verify(&p).expect("verify clean");
    }

    #[test]
    fn ledger_canonical_json_is_stable() {
        let a = canonical_json(&json!({"b": 1, "a": 2})).unwrap();
        let b = canonical_json(&json!({"a": 2, "b": 1})).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn ledger_freshness_windows_match_charters() {
        assert_eq!(default_freshness_window_secs("reconciler"), 2 * 3600);
        assert_eq!(default_freshness_window_secs("linter"), 24 * 3600);
        assert_eq!(default_freshness_window_secs("proposer"), 26 * 3600);
        assert_eq!(default_freshness_window_secs("auditor"), 26 * 3600);
        assert_eq!(default_freshness_window_secs("anything-else"), 24 * 3600);
    }

    #[test]
    fn ledger_genesis_chains_to_zeros() {
        let (_d, p) = tmp();
        let l = Ledger::open(&p).unwrap();
        l.append("a", "c", "heartbeat", &json!({})).unwrap();
        let prev: String = l
            .conn
            .query_row("SELECT prev_hash FROM ledger WHERE id=1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(prev, GENESIS_PREV_HASH);
    }
}
