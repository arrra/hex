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

use crate::daemon::socket_path;
use crate::live::client::LiveClient;
use crate::proto::PoolStatus;
use crate::store::Store;
use crate::workspace::{Registry, RegistryEntry};

/// Index age beyond which a workspace goes red (spec S9: 7 days).
const MAX_INDEX_AGE_SECS: u64 = 7 * 24 * 60 * 60;

/// The `cq doctor` stdout JSON.
#[derive(Debug, Serialize)]
pub struct DoctorReport {
    pub workspaces: Vec<WorkspaceHealth>,
    pub rust_analyzer: RustAnalyzerHealth,
    /// Live daemon health (SPEC-A2 §5).
    pub scipd: ScipdHealth,
}

/// `scipd` daemon health (SPEC-A2 §5): socket ping + pool status
/// passthrough. An unreachable daemon is a WARNING — A1 still works — and
/// only goes red when launchd claims the agent is loaded yet the socket is
/// dead ("scipd loaded but socket dead").
#[derive(Debug, Serialize)]
pub struct ScipdHealth {
    pub socket: PathBuf,
    pub reachable: bool,
    /// Pool occupancy passthrough when reachable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<PoolStatus>,
    /// `launchctl print` verdict for com.hex.scipd; `null` when launchctl
    /// is unavailable (e.g. tests, non-macOS).
    pub launchd_loaded: Option<bool>,
    /// `"ok"` | `"warning"` | `"red"`.
    pub level: String,
    pub detail: String,
}

/// The launchd agent label scipd is deployed under (SPEC-A2 §2).
pub const SCIPD_LAUNCHD_LABEL: &str = "com.hex.scipd";

/// Pure classification of the scipd verdict (unit-tested per plan Task 6):
/// reachable → ok; unreachable + launchd-loaded → red (supposed-running
/// daemon is dead); unreachable otherwise → warning (A1 still works).
pub fn classify_scipd(reachable: bool, launchd_loaded: Option<bool>) -> (&'static str, String) {
    match (reachable, launchd_loaded) {
        (true, _) => ("ok", "scipd answering on the socket".into()),
        (false, Some(true)) => (
            "red",
            "scipd loaded but socket dead (launchd claims the agent is running)".into(),
        ),
        (false, _) => (
            "warning",
            "scipd unreachable — index queries (A1) still work; live escalation, \
             rename and check-routing are unavailable"
                .into(),
        ),
    }
}

/// Probe the daemon: connect + ping + status within the client's 500ms
/// connect bound. Best-effort launchd check via `launchctl print`.
fn scipd_health(home: &Path) -> ScipdHealth {
    let socket = socket_path(home);
    let (reachable, status) = match LiveClient::connect(home) {
        Ok(mut client) => match client.ping().and_then(|()| client.status()) {
            Ok(status) => (true, Some(status)),
            Err(e) => {
                eprintln!("doctor: scipd socket connected but ping/status failed: {e}");
                (false, None)
            }
        },
        Err(_) => (false, None),
    };
    // The launchd agent supervises the DEFAULT home's (~/.codeintel) daemon
    // only; for any other home (hermetic tests, $CODEINTEL_HOME overrides)
    // launchd state says nothing about this home's socket — report unknown.
    let is_default_home = std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join(".codeintel") == home)
        .unwrap_or(false);
    let launchd_loaded = if is_default_home {
        launchd_agent_loaded(SCIPD_LAUNCHD_LABEL)
    } else {
        None
    };
    let (level, detail) = classify_scipd(reachable, launchd_loaded);
    ScipdHealth {
        socket,
        reachable,
        status,
        launchd_loaded,
        level: level.into(),
        detail,
    }
}

/// `launchctl print gui/$UID/<label>` — `Some(true)` when the agent is
/// loaded, `Some(false)` when launchctl answers "not loaded", `None` when
/// launchctl itself is unavailable (best-effort by design).
fn launchd_agent_loaded(label: &str) -> Option<bool> {
    let uid = run_id_u()?;
    let out = Command::new("launchctl")
        .args(["print", &format!("gui/{uid}/{label}")])
        .output()
        .ok()?;
    Some(out.status.success())
}

/// Current uid via `id -u` (no libc dependency for one syscall).
fn run_id_u() -> Option<String> {
    let out = Command::new("id").arg("-u").output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
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
    let scipd = scipd_health(home);

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
    match scipd.level.as_str() {
        "ok" => {
            let instances = scipd
                .status
                .as_ref()
                .map_or(0, |s| s.instances.len());
            eprintln!("doctor: OK  scipd answering ({instances} live instance(s))");
        }
        "warning" => eprintln!("doctor: WARN scipd — {}", scipd.detail),
        _ => {
            eprintln!("doctor: RED scipd — {}", scipd.detail);
            red = true;
        }
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
    Ok((DoctorReport { workspaces, rust_analyzer, scipd }, exit))
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

    // ---- scipd classification (SPEC-A2 §5, plan Task 6) ----

    #[test]
    fn scipd_reachable_is_ok_regardless_of_launchd() {
        for loaded in [None, Some(true), Some(false)] {
            let (level, _) = classify_scipd(true, loaded);
            assert_eq!(level, "ok", "loaded={loaded:?}");
        }
    }

    #[test]
    fn scipd_unreachable_is_warning_not_red() {
        for loaded in [None, Some(false)] {
            let (level, detail) = classify_scipd(false, loaded);
            assert_eq!(level, "warning", "loaded={loaded:?}");
            assert!(detail.contains("A1"), "{detail}");
        }
    }

    #[test]
    fn scipd_loaded_but_socket_dead_is_red() {
        let (level, detail) = classify_scipd(false, Some(true));
        assert_eq!(level, "red");
        assert!(detail.contains("loaded but socket dead"), "{detail}");
    }

    #[test]
    fn scipd_section_present_in_report_and_unreachable_daemon_not_red() {
        // Hermetic home: no daemon. The report must carry the section and
        // the missing daemon alone must NOT add a red reason (the exit-1
        // here comes from the empty registry).
        let home = tempfile::tempdir().unwrap();
        let (report, _) = run(home.path()).unwrap();
        assert!(!report.scipd.reachable);
        assert!(report.scipd.status.is_none());
        assert_eq!(report.scipd.socket, home.path().join("scipd.sock"));
        assert_ne!(report.scipd.level, "red", "{:?}", report.scipd);
    }
}
