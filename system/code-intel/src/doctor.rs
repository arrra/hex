//! `cq doctor` — per-workspace index health (SPEC-A1 §5, S9; plan Task 9).
//!
//! JSON report on stdout (the CLI prints it), human-readable summary lines
//! on stderr (written here). Exit code: 1 when anything is red — any
//! workspace with non-empty `red_reasons`, rust-analyzer missing from PATH,
//! or an empty registry (nothing registered means nothing is healthy; spec
//! S9 "no registry") — else 0.
//!
//! No silent fallbacks (Standing Order S6): an unreadable registry is a hard
//! error; an unreadable workspace db is REPORTED as a red workspace with a
//! "db unreadable: …" reason — the loud path — never skipped.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use rusqlite::OpenFlags;
use serde::Serialize;

use crate::store::Store;
use crate::workspace::{Registry, RegistryEntry};

/// Index age beyond which a workspace goes red (spec S9: 7 days).
const MAX_INDEX_AGE_SECS: u64 = 7 * 24 * 60 * 60;

/// The `cq doctor` stdout JSON.
#[derive(Debug, Serialize)]
pub struct DoctorReport {
    pub workspaces: Vec<WorkspaceHealth>,
    pub rust_analyzer: RustAnalyzerHealth,
}

/// Health of one registered workspace. Index-derived fields are `null` when
/// there is no readable index (the reason is then in `red_reasons`).
#[derive(Debug, Serialize)]
pub struct WorkspaceHealth {
    pub id: String,
    pub root: PathBuf,
    pub index_age_secs: Option<u64>,
    pub indexed_commit: Option<String>,
    /// `git rev-list --count <indexed>..HEAD` in the primary root.
    pub commit_lag: Option<u64>,
    pub last_emit_exit: Option<i64>,
    /// Published generation names, newest first.
    pub generations: Vec<String>,
    pub red_reasons: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct RustAnalyzerHealth {
    pub found: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Run the health check over every registered workspace under `home`.
/// Returns the report plus the exit code (0 green, 1 red). Writes the
/// human-readable summary to stderr.
pub fn run(home: &Path) -> Result<(DoctorReport, i32)> {
    // Unreadable/malformed registry is a hard, loud error (S6).
    let registry = Registry::load(home)?;
    let rust_analyzer = rust_analyzer_health();

    let workspaces: Vec<WorkspaceHealth> = registry
        .entries()
        .iter()
        .map(|entry| workspace_health(home, entry))
        .collect();

    let mut red = workspaces.iter().any(|w| !w.red_reasons.is_empty());
    if workspaces.is_empty() {
        eprintln!("doctor: RED — no workspaces registered (run `cq register <PATH>`)");
        red = true;
    }
    if !rust_analyzer.found {
        eprintln!("doctor: RED — rust-analyzer not found on PATH");
        red = true;
    }
    for w in &workspaces {
        if w.red_reasons.is_empty() {
            eprintln!(
                "doctor: OK  {} {} (age {}s, lag {}, {} generation(s))",
                w.id,
                w.root.display(),
                w.index_age_secs.unwrap_or(0),
                w.commit_lag.unwrap_or(0),
                w.generations.len()
            );
        } else {
            eprintln!(
                "doctor: RED {} {}: {}",
                w.id,
                w.root.display(),
                w.red_reasons.join("; ")
            );
        }
    }

    let exit = i32::from(red);
    Ok((DoctorReport { workspaces, rust_analyzer }, exit))
}

/// Health of one workspace. Never errors out of the whole report: every
/// failure becomes a red reason on this workspace (the loud path).
fn workspace_health(home: &Path, entry: &RegistryEntry) -> WorkspaceHealth {
    let mut health = WorkspaceHealth {
        id: entry.id.clone(),
        root: entry.root.clone(),
        index_age_secs: None,
        indexed_commit: None,
        commit_lag: None,
        last_emit_exit: None,
        generations: Vec::new(),
        red_reasons: Vec::new(),
    };

    let store = Store::new(home, &entry.id);
    match store.generations() {
        Ok(generations) => health.generations = generations,
        Err(e) => health.red_reasons.push(format!("generation listing failed: {e:#}")),
    }

    let generation_dir = match store.current() {
        Ok(Some(_)) => match store.current_dir() {
            Ok(dir) => Some(dir),
            Err(e) => {
                health.red_reasons.push(format!("index unreadable: {e:#}"));
                None
            }
        },
        Ok(None) => {
            health.red_reasons.push("no index: run `cq index`".to_string());
            None
        }
        Err(e) => {
            health.red_reasons.push(format!("CURRENT unreadable: {e:#}"));
            None
        }
    };
    let Some(generation_dir) = generation_dir else {
        return health;
    };

    match read_index_meta(&generation_dir.join("index.sqlite")) {
        Err(e) => health.red_reasons.push(format!("db unreadable: {e:#}")),
        Ok(meta) => {
            health.indexed_commit = Some(meta.commit_sha.clone());
            health.last_emit_exit = Some(meta.emit_exit_code);
            if meta.emit_exit_code != 0 {
                health
                    .red_reasons
                    .push(format!("last emit failed (exit {})", meta.emit_exit_code));
            }

            match index_age_secs(&meta.created_at) {
                Ok(age) => {
                    health.index_age_secs = Some(age);
                    if age > MAX_INDEX_AGE_SECS {
                        health
                            .red_reasons
                            .push(format!("index older than 7 days (age {age}s)"));
                    }
                }
                Err(e) => health.red_reasons.push(format!("index age unknown: {e:#}")),
            }

            match commit_lag(&entry.root, &meta.commit_sha) {
                Ok(lag) => health.commit_lag = Some(lag),
                Err(e) => health.red_reasons.push(format!("commit lag unavailable: {e:#}")),
            }
        }
    }
    health
}

struct IndexMeta {
    commit_sha: String,
    created_at: String,
    emit_exit_code: i64,
}

/// Read the health-relevant `meta` keys from a generation db, read-only.
/// Any failure (unopenable, missing keys, malformed values) is an error the
/// caller reports as a red reason.
fn read_index_meta(db: &Path) -> Result<IndexMeta> {
    let conn = rusqlite::Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("opening {}", db.display()))?;
    let get = |key: &str| -> Result<String> {
        conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
            .with_context(|| format!("meta key {key} missing from {}", db.display()))
    };
    let emit_exit_code = get("emit_exit_code")?;
    Ok(IndexMeta {
        commit_sha: get("commit_sha")?,
        created_at: get("created_at")?,
        emit_exit_code: emit_exit_code
            .parse()
            .with_context(|| format!("meta emit_exit_code {emit_exit_code:?} is not an integer"))?,
    })
}

/// now − `created_at` (RFC3339), floored at 0 on clock skew.
fn index_age_secs(created_at: &str) -> Result<u64> {
    let created = chrono::DateTime::parse_from_rfc3339(created_at)
        .with_context(|| format!("meta created_at {created_at:?} is not RFC3339"))?;
    let age = chrono::Utc::now().signed_duration_since(created).num_seconds();
    Ok(u64::try_from(age).unwrap_or(0))
}

/// `git rev-list --count <indexed>..HEAD` in the primary root.
fn commit_lag(primary_root: &Path, indexed_commit: &str) -> Result<u64> {
    let out = Command::new("git")
        .arg("-C")
        .arg(primary_root)
        .args(["rev-list", "--count", &format!("{indexed_commit}..HEAD")])
        .output()
        .with_context(|| format!("spawning git rev-list in {}", primary_root.display()))?;
    if !out.status.success() {
        anyhow::bail!(
            "git rev-list --count {indexed_commit}..HEAD failed in {}: {}",
            primary_root.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let raw = String::from_utf8_lossy(&out.stdout).trim().to_string();
    raw.parse()
        .with_context(|| format!("git rev-list --count produced non-numeric {raw:?}"))
}

/// `rust-analyzer --version` on the current PATH.
fn rust_analyzer_health() -> RustAnalyzerHealth {
    match Command::new("rust-analyzer").arg("--version").output() {
        Ok(out) if out.status.success() => RustAnalyzerHealth {
            found: true,
            version: Some(String::from_utf8_lossy(&out.stdout).trim().to_string()),
        },
        // Found but broken counts as not healthy; report it as missing with
        // no version rather than pretending it works.
        Ok(_) | Err(_) => RustAnalyzerHealth { found: false, version: None },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_age_parses_rfc3339_and_floors_skew() {
        let past = (chrono::Utc::now() - chrono::Duration::seconds(100)).to_rfc3339();
        let age = index_age_secs(&past).unwrap();
        assert!((100..110).contains(&age), "{age}");
        // Future timestamp (clock skew) floors to 0, never errors.
        let future = (chrono::Utc::now() + chrono::Duration::seconds(100)).to_rfc3339();
        assert_eq!(index_age_secs(&future).unwrap(), 0);
        assert!(index_age_secs("not a date").is_err());
    }

    #[test]
    fn unreadable_db_is_an_error_not_a_skip() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("index.sqlite");
        std::fs::write(&db, b"garbage").unwrap();
        assert!(read_index_meta(&db).is_err());
    }

    #[test]
    fn empty_registry_is_red() {
        let home = tempfile::tempdir().unwrap();
        let (report, exit) = run(home.path()).unwrap();
        assert!(report.workspaces.is_empty());
        assert_eq!(exit, 1, "no workspaces registered must be red (spec S9)");
    }
}
