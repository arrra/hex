//! `cq index` orchestration: emit → ingest → atomic publish (SPEC-A1 §7).
//!
//! Pipeline for one indexing run:
//! 1. Resolve the workspace from `dir` — a worktree folds back to the PRIMARY
//!    checkout root, which is the only thing ever indexed.
//! 2. The workspace must be registered (else `UNREGISTERED_WORKSPACE`).
//! 3. Take the exclusive store lock; if another emit is in flight, return
//!    [`IndexOutcome::SkippedInFlight`] — the caller prints
//!    `{"skipped":"emit-in-flight"}` (visible, never silent; spec §7).
//! 4. Run `rust-analyzer scip .` with cwd = primary root, stdout/stderr
//!    captured to files inside the in-flight `<gen>.tmp/` dir. Nonzero exit →
//!    `CqError::EmitFailed` with the stderr tail; the `.tmp` dir is KEPT for
//!    post-mortem and never published.
//! 5. Ingest the SCIP index into `<gen>.tmp/index.sqlite`, populate the
//!    `meta` table, write `manifest.json` (same data, for humans), publish
//!    atomically, prune to 2 generations.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs::File;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use serde::Serialize;

use crate::error::CqError;
use crate::ingest;
use crate::schema;
use crate::store::Store;
use crate::workspace::{Registry, Workspace};

/// How many trailing stderr lines go into `EmitFailed.stderr_tail`.
const STDERR_TAIL_LINES: usize = 20;

/// Outcome of one `cq index` invocation.
#[derive(Debug)]
pub enum IndexOutcome {
    /// Another emit holds the store lock; nothing was done. The caller MUST
    /// surface this (`{"skipped":"emit-in-flight"}`), never swallow it.
    SkippedInFlight,
    /// A new generation was published.
    Completed(IndexReport),
}

/// Success report for one published generation (also the CLI's stdout JSON).
#[derive(Debug, Clone, Serialize)]
pub struct IndexReport {
    pub workspace_id: String,
    pub generation: String,
    pub commit_sha: String,
    pub emitter: String,
    pub emit_exit_code: i32,
    pub emit_duration_secs: f64,
    pub file_count: u64,
    pub symbol_count: u64,
    pub pruned: Vec<String>,
}

/// Run the full index pipeline for the workspace containing `dir`.
/// `codeintel_home` is the store root (`$CODEINTEL_HOME` / `~/.codeintel`).
pub fn run(codeintel_home: &Path, dir: &Path) -> Result<IndexOutcome> {
    run_inner(codeintel_home, dir, None)
}

/// Implementation with an optional PATH override for the spawned
/// `rust-analyzer` (tests inject a shim dir ahead in PATH this way without
/// mutating process-global env, which is not thread-safe under `cargo test`).
fn run_inner(codeintel_home: &Path, dir: &Path, path_env: Option<&OsStr>) -> Result<IndexOutcome> {
    let ws = Workspace::resolve(dir)?;
    let registry = Registry::load(codeintel_home)?;
    if !registry.contains(&ws.id) {
        return Err(CqError::UnregisteredWorkspace {
            cwd: dir.display().to_string(),
        }
        .into());
    }

    let store = Store::new(codeintel_home, &ws.id);
    let Some(_guard) = store.try_lock()? else {
        return Ok(IndexOutcome::SkippedInFlight);
    };

    let generation = store.begin_generation()?;
    let scip_path = generation.dir().join("index.scip");
    let stdout_log = generation.dir().join("emit.stdout.log");
    let stderr_log = generation.dir().join("emit.stderr.log");

    // Emit: `rust-analyzer scip .` in the PRIMARY checkout (never a worktree),
    // stdout/stderr captured to files in the tmp generation dir.
    let started = Instant::now();
    let mut cmd = Command::new("rust-analyzer");
    cmd.args(["scip", "."])
        .arg("--output")
        .arg(&scip_path)
        .current_dir(&ws.primary_root)
        .stdout(create_log(&stdout_log)?)
        .stderr(create_log(&stderr_log)?);
    if let Some(path) = path_env {
        cmd.env("PATH", path);
    }
    let status = match cmd.status() {
        Ok(status) => status,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Missing emitter binary: loud, hinted (`cq doctor` owns the full
            // PATH check, but this failure must never be cryptic).
            return Err(CqError::EmitFailed {
                stderr_tail: "rust-analyzer not found on PATH — install it \
                              (e.g. `brew install rust-analyzer` or `rustup component add \
                              rust-analyzer`) and verify with `cq doctor`"
                    .into(),
            }
            .into());
        }
        Err(e) => {
            return Err(e).with_context(|| {
                format!("spawning rust-analyzer scip in {}", ws.primary_root.display())
            })
        }
    };
    let emit_duration_secs = started.elapsed().as_secs_f64();

    if !status.success() {
        // KEEP the .tmp generation dir (emit logs inside) for post-mortem;
        // never publish a failed emit.
        return Err(CqError::EmitFailed {
            stderr_tail: stderr_tail(&stderr_log, status),
        }
        .into());
    }
    if !scip_path.is_file() {
        return Err(CqError::EmitFailed {
            stderr_tail: format!(
                "rust-analyzer scip exited 0 but produced no {}",
                scip_path.display()
            ),
        }
        .into());
    }

    let emitter = analyzer_version(path_env)?;
    let commit_sha = git_head_sha(&ws.primary_root)?;

    // Ingest into the generation database.
    let db_path = generation.dir().join("index.sqlite");
    let conn = rusqlite::Connection::open(&db_path)
        .with_context(|| format!("opening generation db {}", db_path.display()))?;
    schema::create(&conn).context("creating generation schema")?;
    let stats = ingest::ingest(&scip_path, &conn, &ws.primary_root)?;

    // meta table + manifest.json carry the same map (spec §4 keys).
    let mut meta = BTreeMap::new();
    meta.insert("schema_version".to_string(), "1".to_string());
    meta.insert(
        "workspace_root".to_string(),
        ws.primary_root.display().to_string(),
    );
    meta.insert("commit_sha".to_string(), commit_sha.clone());
    meta.insert("emitter".to_string(), emitter.clone());
    meta.insert("created_at".to_string(), chrono::Utc::now().to_rfc3339());
    meta.insert("emit_exit_code".to_string(), "0".to_string());
    meta.insert(
        "emit_duration_secs".to_string(),
        format!("{emit_duration_secs:.3}"),
    );
    meta.insert("file_count".to_string(), stats.files.to_string());
    meta.insert("symbol_count".to_string(), stats.symbols.to_string());
    {
        let mut insert = conn.prepare("INSERT INTO meta (key, value) VALUES (?1, ?2)")?;
        for (key, value) in &meta {
            insert.execute(rusqlite::params![key, value])?;
        }
    }
    drop(conn);

    let manifest = serde_json::to_string_pretty(&meta).context("serializing manifest.json")?;
    std::fs::write(generation.dir().join("manifest.json"), manifest)
        .context("writing manifest.json")?;

    let gen_name = store.publish(generation)?;
    let pruned = store.prune()?;

    Ok(IndexOutcome::Completed(IndexReport {
        workspace_id: ws.id,
        generation: gen_name,
        commit_sha,
        emitter,
        emit_exit_code: 0,
        emit_duration_secs,
        file_count: stats.files,
        symbol_count: stats.symbols,
        pruned,
    }))
}

/// Open a log file for child stdout/stderr capture.
fn create_log(path: &Path) -> Result<File> {
    File::create(path).with_context(|| format!("creating emit log {}", path.display()))
}

/// Last [`STDERR_TAIL_LINES`] of the captured emit stderr, with exit status.
fn stderr_tail(stderr_log: &Path, status: std::process::ExitStatus) -> String {
    let raw = std::fs::read_to_string(stderr_log).unwrap_or_else(|e| {
        // The tail is best-effort context inside an already-failing path; an
        // unreadable log is reported, not swallowed.
        format!("<emit stderr log unreadable: {e}>")
    });
    let lines: Vec<&str> = raw.lines().collect();
    let tail_start = lines.len().saturating_sub(STDERR_TAIL_LINES);
    format!("exit {status}: {}", lines[tail_start..].join("\n"))
}

/// `rust-analyzer --version` (the `emitter` meta value).
fn analyzer_version(path_env: Option<&OsStr>) -> Result<String> {
    let mut cmd = Command::new("rust-analyzer");
    cmd.arg("--version");
    if let Some(path) = path_env {
        cmd.env("PATH", path);
    }
    let out = cmd.output().context("spawning rust-analyzer --version")?;
    if !out.status.success() {
        bail!(
            "rust-analyzer --version failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `git rev-parse HEAD` of the primary checkout (the `commit_sha` meta value).
fn git_head_sha(primary_root: &Path) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(primary_root)
        .args(["rev-parse", "HEAD"])
        .output()
        .with_context(|| format!("spawning git rev-parse in {}", primary_root.display()))?;
    if !out.status.success() {
        bail!(
            "git rev-parse HEAD failed in {}: {}",
            primary_root.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::register_workspace;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// PATH with /opt/homebrew/bin prepended — BOI verify subshells strip
    /// PATH; rust-analyzer/git must still resolve (CLAUDE.md verify-gate rules).
    fn full_path_env() -> String {
        format!(
            "/opt/homebrew/bin:{}",
            std::env::var("PATH").unwrap_or_default()
        )
    }

    fn run_cmd(cwd: &Path, prog: &str, args: &[&str]) {
        let out = Command::new(prog)
            .args(args)
            .current_dir(cwd)
            .env("PATH", full_path_env())
            .output()
            .unwrap_or_else(|e| panic!("spawning {prog}: {e}"));
        assert!(
            out.status.success(),
            "{prog} {args:?} failed\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn copy_dir(src: &Path, dst: &Path) {
        for entry in std::fs::read_dir(src).unwrap() {
            let entry = entry.unwrap();
            let to = dst.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                std::fs::create_dir_all(&to).unwrap();
                copy_dir(&entry.path(), &to);
            } else {
                std::fs::copy(entry.path(), &to).unwrap();
            }
        }
    }

    /// Golden fixture crate copied to a tempdir, git-initialized + committed
    /// (T5 helper pattern; the indexer itself runs the emit).
    fn golden_repo() -> TempDir {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/golden-crate");
        let dir = tempfile::tempdir().unwrap();
        copy_dir(&fixture, dir.path());
        run_cmd(dir.path(), "git", &["init", "-q", "-b", "main"]);
        run_cmd(dir.path(), "git", &["add", "-A"]);
        run_cmd(
            dir.path(),
            "git",
            &[
                "-c",
                "user.email=cq@test",
                "-c",
                "user.name=cq-test",
                "commit",
                "-q",
                "-m",
                "golden",
            ],
        );
        dir
    }

    fn completed(outcome: IndexOutcome) -> IndexReport {
        match outcome {
            IndexOutcome::Completed(report) => report,
            IndexOutcome::SkippedInFlight => panic!("expected Completed, got SkippedInFlight"),
        }
    }

    #[test]
    fn index_end_to_end_on_golden_crate() {
        let home = tempfile::tempdir().unwrap(); // CODEINTEL_HOME
        let repo = golden_repo();
        register_workspace(home.path(), repo.path()).unwrap();

        let path_env = full_path_env();
        let report = completed(
            run_inner(home.path(), repo.path(), Some(path_env.as_ref())).unwrap(),
        );
        assert_eq!(report.emit_exit_code, 0);
        assert!(report.emit_duration_secs > 0.0);
        assert!(report.file_count >= 3, "got {}", report.file_count);
        assert!(report.symbol_count >= 10, "got {}", report.symbol_count);
        assert!(report.emitter.contains("rust-analyzer"));

        let store = Store::new(home.path(), &report.workspace_id);
        let current = store.current_dir().unwrap();
        assert_eq!(store.current().unwrap().unwrap(), report.generation);
        assert!(current.join("index.sqlite").exists());
        assert!(current.join("manifest.json").exists());

        // meta table populated, commit_sha matches the repo HEAD.
        let head = git_head_sha(&repo.path().canonicalize().unwrap()).unwrap();
        assert_eq!(report.commit_sha, head);
        let conn = rusqlite::Connection::open(current.join("index.sqlite")).unwrap();
        let meta_sha: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'commit_sha'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(meta_sha, head);
        for key in [
            "schema_version",
            "workspace_root",
            "emitter",
            "created_at",
            "emit_exit_code",
            "emit_duration_secs",
            "file_count",
            "symbol_count",
        ] {
            let v: String = conn
                .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
                .unwrap_or_else(|e| panic!("meta key {key} missing: {e}"));
            assert!(!v.is_empty(), "meta key {key} empty");
        }

        // manifest.json mirrors the meta map.
        let manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(current.join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["commit_sha"], head);
        assert_eq!(manifest["schema_version"], "1");

        // Second run while the lock is held: visible Skipped, never silent.
        let _held = store.try_lock().unwrap().expect("lock free after run");
        let second = run_inner(home.path(), repo.path(), Some(path_env.as_ref())).unwrap();
        assert!(
            matches!(second, IndexOutcome::SkippedInFlight),
            "expected SkippedInFlight, got {second:?}"
        );
    }

    #[test]
    fn index_from_worktree_indexes_primary_root() {
        let home = tempfile::tempdir().unwrap();
        let repo = golden_repo();
        register_workspace(home.path(), repo.path()).unwrap();

        let wt_parent = tempfile::tempdir().unwrap();
        let wt = wt_parent.path().join("wt-t8");
        run_cmd(
            repo.path(),
            "git",
            &["worktree", "add", "-q", wt.to_str().unwrap()],
        );

        let path_env = full_path_env();
        let report =
            completed(run_inner(home.path(), &wt, Some(path_env.as_ref())).unwrap());
        // Identity and indexed root are the PRIMARY checkout's, not the worktree's.
        let ws = Workspace::resolve(repo.path()).unwrap();
        assert_eq!(report.workspace_id, ws.id);
        let conn = rusqlite::Connection::open(
            Store::new(home.path(), &ws.id)
                .current_dir()
                .unwrap()
                .join("index.sqlite"),
        )
        .unwrap();
        let root: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'workspace_root'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(PathBuf::from(root), ws.primary_root);
    }

    #[test]
    fn unregistered_workspace_is_refused() {
        let home = tempfile::tempdir().unwrap();
        let repo = golden_repo(); // valid repo, but never registered
        let err = run(home.path(), repo.path()).unwrap_err();
        let cq = err.downcast_ref::<CqError>().expect("CqError");
        assert!(matches!(cq, CqError::UnregisteredWorkspace { .. }), "{cq}");
    }

    #[test]
    fn emit_failure_is_emit_failed_with_stderr_tail() {
        let home = tempfile::tempdir().unwrap();
        let repo = golden_repo();
        register_workspace(home.path(), repo.path()).unwrap();

        // Fake rust-analyzer shim ahead in PATH: prints "boom" to stderr, exits 7.
        let shim_dir = tempfile::tempdir().unwrap();
        let shim = shim_dir.path().join("rust-analyzer");
        std::fs::write(&shim, "#!/bin/sh\necho boom >&2\nexit 7\n").unwrap();
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path_env = format!("{}:{}", shim_dir.path().display(), full_path_env());

        let err = run_inner(home.path(), repo.path(), Some(path_env.as_ref())).unwrap_err();
        let cq = err.downcast_ref::<CqError>().expect("CqError, got: {err}");
        match cq {
            CqError::EmitFailed { stderr_tail } => {
                assert!(stderr_tail.contains("boom"), "tail: {stderr_tail}");
            }
            other => panic!("expected EmitFailed, got {other}"),
        }

        // Never published; the .tmp dir is KEPT for post-mortem with the logs.
        let ws = Workspace::resolve(repo.path()).unwrap();
        let store = Store::new(home.path(), &ws.id);
        assert!(store.current().unwrap().is_none());
        let tmp_dirs: Vec<_> = std::fs::read_dir(store.workspace_dir())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert_eq!(tmp_dirs.len(), 1, "post-mortem .tmp dir must be kept");
        assert!(tmp_dirs[0].path().join("emit.stderr.log").exists());
    }

    #[test]
    fn missing_analyzer_binary_is_loud_hinted_emit_failure() {
        let home = tempfile::tempdir().unwrap();
        let repo = golden_repo();
        register_workspace(home.path(), repo.path()).unwrap();

        // PATH with no rust-analyzer at all (git still needed before the
        // emit? no — resolve uses absolute env PATH of THIS process; only the
        // child gets the stripped PATH).
        let empty = tempfile::tempdir().unwrap();
        let path_env = empty.path().as_os_str().to_os_string();
        let err = run_inner(home.path(), repo.path(), Some(&path_env)).unwrap_err();
        let cq = err.downcast_ref::<CqError>().expect("CqError");
        match cq {
            CqError::EmitFailed { stderr_tail } => {
                assert!(
                    stderr_tail.contains("not found on PATH"),
                    "tail: {stderr_tail}"
                );
            }
            other => panic!("expected EmitFailed, got {other}"),
        }
    }
}
