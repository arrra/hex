//! Integration tests for the `cq` binary (plan Task 9, SPEC-A1 §5).
//!
//! Every test spawns the real binary via `env!("CARGO_BIN_EXE_cq")` with
//! `CODEINTEL_HOME` pointed at a hermetic tempdir, and asserts BOTH the
//! stdout/stderr JSON shape AND the exit code from the spec §5 table.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

/// PATH with /opt/homebrew/bin prepended — BOI verify subshells strip PATH;
/// rust-analyzer/git must still resolve (CLAUDE.md verify-gate rules).
fn full_path_env() -> String {
    format!(
        "/opt/homebrew/bin:{}",
        std::env::var("PATH").unwrap_or_default()
    )
}

/// Spawn the real `cq` binary with a hermetic CODEINTEL_HOME.
fn cq(home: &Path, cwd: &Path, args: &[&str]) -> Output {
    cq_with_path(home, cwd, args, &full_path_env())
}

fn cq_with_path(home: &Path, cwd: &Path, args: &[&str], path_env: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cq"))
        .args(args)
        .current_dir(cwd)
        .env("CODEINTEL_HOME", home)
        .env("PATH", path_env)
        .output()
        .unwrap_or_else(|e| panic!("spawning cq {args:?}: {e}"))
}

fn stdout_json(out: &Output) -> serde_json::Value {
    let raw = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(raw.trim()).unwrap_or_else(|e| {
        panic!(
            "stdout is not one JSON object: {e}\nstdout: {raw}\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

fn stderr_json(out: &Output) -> serde_json::Value {
    let raw = String::from_utf8_lossy(&out.stderr);
    // Human summary lines may precede/follow; the structured error is the
    // line that parses as JSON.
    for line in raw.lines() {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) {
            return v;
        }
    }
    panic!("no JSON object on stderr\nstderr: {raw}");
}

fn exit_code(out: &Output) -> i32 {
    out.status.code().expect("cq terminated by signal")
}

fn assert_exit(out: &Output, want: i32) {
    assert_eq!(
        exit_code(out),
        want,
        "exit code\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn run_cmd(cwd: &Path, prog: &str, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&out.stdout).trim().to_string()
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
/// (golden-fixture helper pattern from src/ingest.rs tests).
fn golden_repo() -> TempDir {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/golden-crate");
    let dir = tempfile::tempdir().unwrap();
    copy_dir(&fixture, dir.path());
    run_cmd(dir.path(), "git", &["init", "-q", "-b", "main"]);
    run_cmd(dir.path(), "git", &["config", "user.email", "cq@test"]);
    run_cmd(dir.path(), "git", &["config", "user.name", "cq-test"]);
    run_cmd(dir.path(), "git", &["add", "-A"]);
    run_cmd(dir.path(), "git", &["commit", "-q", "-m", "golden"]);
    dir
}

/// Register + index the golden repo under `home`; returns the workspace id.
fn register_and_index(home: &Path, repo: &Path) -> String {
    let out = cq(home, repo, &["register", repo.to_str().unwrap()]);
    assert_exit(&out, 0);
    let id = stdout_json(&out)["registered"]
        .as_str()
        .expect("register stdout has `registered` id")
        .to_string();
    let out = cq(home, repo, &["index"]);
    assert_exit(&out, 0);
    let report = stdout_json(&out);
    assert_eq!(report["emit_exit_code"], 0, "{report}");
    id
}

/// Path to the published current generation's index.sqlite.
fn current_db(home: &Path, ws_id: &str) -> PathBuf {
    let ws_dir = home.join(ws_id);
    let current = std::fs::read_to_string(ws_dir.join("CURRENT"))
        .expect("CURRENT exists after index")
        .trim()
        .to_string();
    ws_dir.join(current).join("index.sqlite")
}

fn meta_update(db: &Path, key: &str, value: &str) {
    let conn = rusqlite::Connection::open(db).unwrap();
    let n = conn
        .execute(
            "UPDATE meta SET value = ?1 WHERE key = ?2",
            rusqlite::params![value, key],
        )
        .unwrap();
    assert_eq!(n, 1, "meta key {key} not present to update");
}

// ---- spec §5 table, row by row ----

#[test]
fn def_success_path_exit_0() {
    let home = tempfile::tempdir().unwrap();
    let repo = golden_repo();
    let ws_id = register_and_index(home.path(), repo.path());

    // By bare symbol name.
    let out = cq(home.path(), repo.path(), &["def", "double"]);
    assert_exit(&out, 0);
    let env = stdout_json(&out);
    assert_eq!(env["source"], "index", "{env}");
    assert_eq!(env["workspace_id"], ws_id.as_str(), "{env}");
    assert_eq!(env["stale_files"], serde_json::json!([]), "{env}");
    assert!(env["indexed_commit"].as_str().unwrap().len() >= 40, "{env}");
    let r = &env["results"][0];
    assert_eq!(r["path"], "src/ops.rs", "{env}");
    assert_eq!(r["line"], 1, "{env}");
    assert_eq!(r["col"], 8, "{env}");
    assert_eq!(r["role"], "definition", "{env}");
    assert_eq!(r["kind"], "function", "{env}");
    assert_eq!(
        r["snippet"], "pub fn double(x: i32) -> i32 { x * 2 }",
        "{env}"
    );
    // Errors never pollute stdout; envelope never pollutes stderr.
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // By FILE:LINE:COL position (1-based): the `double` call in lib.rs:4:43
    // resolves to the same definition.
    let out = cq(home.path(), repo.path(), &["def", "src/lib.rs:4:43"]);
    assert_exit(&out, 0);
    let env = stdout_json(&out);
    let r = &env["results"][0];
    assert_eq!(
        (r["path"].as_str(), r["line"].as_u64(), r["col"].as_u64()),
        (Some("src/ops.rs"), Some(1), Some(8)),
        "{env}"
    );

    // A name containing colons but no trailing numeric segments stays a
    // symbol-name lookup (the documented heuristic), failing as NOT_FOUND
    // rather than as a mangled path.
    let out = cq(home.path(), repo.path(), &["def", "ops::double:nope"]);
    assert_exit(&out, 5);
}

#[test]
fn other_verbs_success_path_exit_0() {
    let home = tempfile::tempdir().unwrap();
    let repo = golden_repo();
    register_and_index(home.path(), repo.path());

    let out = cq(home.path(), repo.path(), &["refs", "double"]);
    assert_exit(&out, 0);
    let env = stdout_json(&out);
    let roles: Vec<&str> = env["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["role"].as_str().unwrap())
        .collect();
    assert!(roles.contains(&"definition"), "{env}");
    assert!(roles.contains(&"reference"), "{env}");

    let out = cq(home.path(), repo.path(), &["callers", "double"]);
    assert_exit(&out, 0);
    let env = stdout_json(&out);
    let names: Vec<&str> = env["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["display_name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"top_level_fn"), "{env}");
    assert!(names.contains(&"fmt_user"), "{env}");

    let out = cq(home.path(), repo.path(), &["symbols", "src/shapes.rs"]);
    assert_exit(&out, 0);
    let env = stdout_json(&out);
    let names: Vec<&str> = env["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["display_name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"Area"), "{env}");
    assert!(names.contains(&"total_area"), "{env}");

    let out = cq(home.path(), repo.path(), &["search", "gener"]);
    assert_exit(&out, 0);
    let env = stdout_json(&out);
    assert!(
        env["results"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["display_name"] == "generic_max"),
        "{env}"
    );
}

#[test]
fn unregistered_cwd_exit_4() {
    let home = tempfile::tempdir().unwrap();

    // A plain non-git tempdir.
    let plain = tempfile::tempdir().unwrap();
    let out = cq(home.path(), plain.path(), &["def", "double"]);
    assert_exit(&out, 4);
    assert_eq!(stderr_json(&out)["error"]["code"], "UNREGISTERED_WORKSPACE");
    assert!(out.stdout.is_empty(), "no envelope on error");

    // A real git repo that was never registered.
    let repo = golden_repo();
    let out = cq(home.path(), repo.path(), &["refs", "double"]);
    assert_exit(&out, 4);
    let err = stderr_json(&out);
    assert_eq!(err["error"]["code"], "UNREGISTERED_WORKSPACE");
    assert!(
        err["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("cq register"),
        "{err}"
    );
}

#[test]
fn registered_no_index_exit_3() {
    let home = tempfile::tempdir().unwrap();
    let repo = golden_repo();
    let out = cq(
        home.path(),
        repo.path(),
        &["register", repo.path().to_str().unwrap()],
    );
    assert_exit(&out, 0);

    let out = cq(home.path(), repo.path(), &["def", "double"]);
    assert_exit(&out, 3);
    let err = stderr_json(&out);
    assert_eq!(err["error"]["code"], "NO_INDEX");
    assert!(
        err["error"]["hint"].as_str().unwrap().contains("cq index"),
        "{err}"
    );
}

#[test]
fn nonsense_symbol_exit_5() {
    let home = tempfile::tempdir().unwrap();
    let repo = golden_repo();
    register_and_index(home.path(), repo.path());

    let out = cq(home.path(), repo.path(), &["def", "no_such_symbol_xyz"]);
    assert_exit(&out, 5);
    let err = stderr_json(&out);
    assert_eq!(err["error"]["code"], "NOT_FOUND");
    assert!(out.stdout.is_empty(), "never empty-success on a miss");
}

#[test]
fn stale_strict_exit_2() {
    let home = tempfile::tempdir().unwrap();
    let repo = golden_repo();
    register_and_index(home.path(), repo.path());

    // Tracked-file modification after indexing.
    let ops = repo.path().join("src/ops.rs");
    let mut content = std::fs::read_to_string(&ops).unwrap();
    content.push_str("// edited after indexing\n");
    std::fs::write(&ops, content).unwrap();

    // Annotated mode: envelope on stdout, stale file listed, exit 2.
    let out = cq(home.path(), repo.path(), &["refs", "double"]);
    assert_exit(&out, 2);
    let env = stdout_json(&out);
    assert_eq!(
        env["stale_files"],
        serde_json::json!(["src/ops.rs"]),
        "{env}"
    );
    for r in env["results"].as_array().unwrap() {
        if r["path"] == "src/ops.rs" {
            assert!(
                r.get("snippet").is_none(),
                "stale file keeps no snippet: {r}"
            );
        }
    }

    // --strict: refusal, structured error, still exit 2.
    let out = cq(home.path(), repo.path(), &["refs", "double", "--strict"]);
    assert_exit(&out, 2);
    let err = stderr_json(&out);
    assert_eq!(err["error"]["code"], "STALE_RESULTS");
    assert!(out.stdout.is_empty(), "strict refusal emits no envelope");
}

// ---- doctor (spec S9) ----

#[test]
fn doctor_red_when_no_index_and_green_after() {
    let home = tempfile::tempdir().unwrap();
    let repo = golden_repo();

    // No workspaces registered at all → red.
    let out = cq(home.path(), repo.path(), &["doctor"]);
    assert_exit(&out, 1);

    // Registered but never indexed → red with a "no index" reason.
    let out = cq(
        home.path(),
        repo.path(),
        &["register", repo.path().to_str().unwrap()],
    );
    assert_exit(&out, 0);
    let ws_id = stdout_json(&out)["registered"]
        .as_str()
        .unwrap()
        .to_string();

    let out = cq(home.path(), repo.path(), &["doctor"]);
    assert_exit(&out, 1);
    let report = stdout_json(&out);
    let ws = &report["workspaces"][0];
    assert_eq!(ws["id"], ws_id.as_str(), "{report}");
    let reasons = ws["red_reasons"].as_array().unwrap();
    assert!(
        reasons
            .iter()
            .any(|r| r.as_str().unwrap().contains("no index")),
        "{report}"
    );

    // Indexed → green: exit 0, empty red_reasons, full health fields.
    let out = cq(home.path(), repo.path(), &["index"]);
    assert_exit(&out, 0);
    let head = run_cmd(repo.path(), "git", &["rev-parse", "HEAD"]);

    let out = cq(home.path(), repo.path(), &["doctor"]);
    assert_exit(&out, 0);
    let report = stdout_json(&out);
    let ws = &report["workspaces"][0];
    assert_eq!(ws["red_reasons"], serde_json::json!([]), "{report}");
    assert_eq!(ws["indexed_commit"], head.as_str(), "{report}");
    assert_eq!(ws["commit_lag"], 0, "{report}");
    assert_eq!(ws["last_emit_exit"], 0, "{report}");
    assert_eq!(ws["generations"].as_array().unwrap().len(), 1, "{report}");
    assert!(ws["index_age_secs"].as_u64().unwrap() < 3600, "{report}");

    // A new commit after indexing → lag 1, but lag alone is not red.
    run_cmd(
        repo.path(),
        "git",
        &["commit", "-q", "--allow-empty", "-m", "post-index"],
    );
    let out = cq(home.path(), repo.path(), &["doctor"]);
    assert_exit(&out, 0);
    assert_eq!(stdout_json(&out)["workspaces"][0]["commit_lag"], 1);

    // Index older than 7 days (inject via SQL UPDATE, per plan) → red.
    let db = current_db(home.path(), &ws_id);
    let old = (chrono::Utc::now() - chrono::Duration::days(8)).to_rfc3339();
    meta_update(&db, "created_at", &old);
    let out = cq(home.path(), repo.path(), &["doctor"]);
    assert_exit(&out, 1);
    let report = stdout_json(&out);
    let reasons = report["workspaces"][0]["red_reasons"].as_array().unwrap();
    assert!(
        reasons
            .iter()
            .any(|r| r.as_str().unwrap().contains("7 days")),
        "{report}"
    );

    // Last emit failed (injected) → red.
    meta_update(&db, "created_at", &chrono::Utc::now().to_rfc3339());
    meta_update(&db, "emit_exit_code", "7");
    let out = cq(home.path(), repo.path(), &["doctor"]);
    assert_exit(&out, 1);
    let report = stdout_json(&out);
    let ws = &report["workspaces"][0];
    assert_eq!(ws["last_emit_exit"], 7, "{report}");
    let reasons = ws["red_reasons"].as_array().unwrap();
    assert!(
        reasons.iter().any(|r| r.as_str().unwrap().contains("emit")),
        "{report}"
    );

    // Corrupt the db entirely → doctor still answers, workspace red with the
    // loud "db unreadable" reason (never silently skipped).
    std::fs::write(&db, b"not a sqlite database").unwrap();
    let out = cq(home.path(), repo.path(), &["doctor"]);
    assert_exit(&out, 1);
    let report = stdout_json(&out);
    let reasons = report["workspaces"][0]["red_reasons"].as_array().unwrap();
    assert!(
        reasons
            .iter()
            .any(|r| r.as_str().unwrap().contains("db unreadable")),
        "{report}"
    );
}

#[test]
fn doctor_verifies_rust_analyzer_on_path() {
    let home = tempfile::tempdir().unwrap();
    let repo = golden_repo();
    register_and_index(home.path(), repo.path());

    // Full PATH: rust-analyzer found, version reported, green.
    let out = cq(home.path(), repo.path(), &["doctor"]);
    assert_exit(&out, 0);
    let report = stdout_json(&out);
    assert_eq!(report["rust_analyzer"]["found"], true, "{report}");
    assert!(
        report["rust_analyzer"]["version"]
            .as_str()
            .unwrap()
            .contains("rust-analyzer"),
        "{report}"
    );

    // PATH with git but NO rust-analyzer → found=false and doctor goes red.
    let bin = tempfile::tempdir().unwrap();
    let git = run_cmd(repo.path(), "which", &["git"]);
    std::os::unix::fs::symlink(&git, bin.path().join("git")).unwrap();
    let out = cq_with_path(
        home.path(),
        repo.path(),
        &["doctor"],
        bin.path().to_str().unwrap(),
    );
    assert_exit(&out, 1);
    let report = stdout_json(&out);
    assert_eq!(report["rust_analyzer"]["found"], false, "{report}");
    assert!(report["rust_analyzer"]["version"].is_null(), "{report}");
}
