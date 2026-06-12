//! Real-binary tests for cq's A2 surface (plan Task 6): routing
//! (auto-escalation, warming, daemon-down, `--live`, `--no-live`), rename
//! (plan + apply + compile check), and `cq check` (0/1/8).
//!
//! Every test spawns the real `cq` (and where needed the real `scipd`)
//! via `env!("CARGO_BIN_EXE_*")` with `CODEINTEL_HOME` pointed at a
//! hermetic tempdir. Live tests pay one real rust-analyzer prime on the
//! golden fixture (seconds).

use std::process::Command;
use std::time::{Duration, Instant};

mod common;
use common::{
    append_brand_new_caller, assert_exit, cq, cq_with_path, find_pos, full_path_env, golden_repo,
    register, register_and_index, run_cmd, spawn_scipd, stderr_json, stdout_json,
    wait_daemon_ready, READY_BUDGET,
};

// ---------------------------------------------------------------------------
// Routing (SPEC-A2 §5)
// ---------------------------------------------------------------------------

/// One daemon, one prime: the warming envelope arrives first (bounded),
/// then the live answer with the brand-new call site the index cannot see.
#[test]
fn stale_query_escalates_to_live_after_warming() {
    let home = tempfile::tempdir().unwrap();
    let repo = golden_repo();
    register_and_index(home.path(), repo.path());
    let new_line = append_brand_new_caller(repo.path());

    let _daemon = spawn_scipd(home.path());
    wait_daemon_ready(home.path());

    // First query triggers the spawn: index answer + escalated.warming,
    // returned fast — never queued behind the prime (SPEC-A2 §6 S5).
    let t0 = Instant::now();
    let out = cq(home.path(), repo.path(), &["refs", "double"]);
    let elapsed = t0.elapsed();
    assert!(
        elapsed < Duration::from_secs(5),
        "query during prime took {elapsed:?} — queued behind the prime?"
    );
    assert_exit(&out, 2);
    let env = stdout_json(&out);
    assert_eq!(env["source"], "index", "{env}");
    assert_eq!(env["stale_files"], serde_json::json!(["src/ops.rs"]), "{env}");
    assert_eq!(env["escalated"]["reason"], "warming", "{env}");
    assert!(env["escalated"]["elapsed_secs"].is_u64(), "{env}");

    // Poll until the instance is ready: the SAME command now answers live.
    let deadline = Instant::now() + READY_BUDGET;
    let env = loop {
        let out = cq(home.path(), repo.path(), &["refs", "double"]);
        let env = stdout_json(&out);
        if env["source"] == "live" {
            assert_exit(&out, 0);
            break env;
        }
        assert_eq!(env["escalated"]["reason"], "warming", "{env}");
        assert!(
            Instant::now() < deadline,
            "instance never became ready within {READY_BUDGET:?}; last: {env}"
        );
        std::thread::sleep(Duration::from_millis(500));
    };
    assert_eq!(env["stale_files"], serde_json::json!([]), "{env}");
    assert!(env.get("escalated").is_none(), "{env}");
    let results = env["results"].as_array().unwrap();
    let new_site = results.iter().find(|r| r["line"] == new_line);
    assert!(
        new_site.is_some(),
        "live refs must include the brand-new call site at ops.rs:{new_line}: {env}"
    );
    assert_eq!(new_site.unwrap()["path"], "src/ops.rs");

    // The same query with --no-live stays pure index: misses the new site,
    // flags the stale file, no escalated section (it never touched the
    // socket — the daemon is up and would have answered).
    let out = cq(home.path(), repo.path(), &["refs", "double", "--no-live"]);
    assert_exit(&out, 2);
    let env = stdout_json(&out);
    assert_eq!(env["source"], "index", "{env}");
    assert!(env.get("escalated").is_none(), "{env}");
    assert!(
        !env["results"].as_array().unwrap().iter().any(|r| r["line"] == new_line),
        "--no-live must miss the brand-new call site: {env}"
    );
}

/// Daemon down: full A1 behavior + escalated.daemon-unavailable, bounded
/// latency (socket failure adds ≤500ms; bound generously at 2s).
#[test]
fn daemon_down_degrades_loudly_and_fast() {
    let home = tempfile::tempdir().unwrap();
    let repo = golden_repo();
    register_and_index(home.path(), repo.path());

    // Fresh query, daemon down: byte-identical A1 surface — source index,
    // no escalated (the socket is never consulted when nothing is stale).
    let out = cq(home.path(), repo.path(), &["def", "double"]);
    assert_exit(&out, 0);
    let env = stdout_json(&out);
    assert_eq!(env["source"], "index");
    assert!(env.get("escalated").is_none(), "{env}");

    // Stale query, daemon down: A1 answer + escalated, fast.
    append_brand_new_caller(repo.path());
    let t0 = Instant::now();
    let out = cq(home.path(), repo.path(), &["refs", "double"]);
    let elapsed = t0.elapsed();
    assert!(
        elapsed < Duration::from_secs(2),
        "daemon-down escalation took {elapsed:?} (> 2s)"
    );
    assert_exit(&out, 2);
    let env = stdout_json(&out);
    assert_eq!(env["source"], "index", "{env}");
    assert_eq!(env["stale_files"], serde_json::json!(["src/ops.rs"]), "{env}");
    assert_eq!(env["escalated"]["reason"], "daemon-unavailable", "{env}");
    assert!(
        env["escalated"]["detail"].as_str().unwrap().contains("scipd"),
        "{env}"
    );
}

/// `--live` with the daemon down: LIVE_UNAVAILABLE, exit 7, no envelope.
#[test]
fn forced_live_with_daemon_down_exits_7() {
    let home = tempfile::tempdir().unwrap();
    let repo = golden_repo();
    register_and_index(home.path(), repo.path());

    let out = cq(home.path(), repo.path(), &["refs", "double", "--live"]);
    assert_exit(&out, 7);
    assert!(out.stdout.is_empty(), "no envelope on a forced-live failure");
    let err = stderr_json(&out);
    assert_eq!(err["error"]["code"], "LIVE_UNAVAILABLE", "{err}");
    assert!(!err["error"]["hint"].as_str().unwrap().is_empty(), "{err}");
}

// ---------------------------------------------------------------------------
// Rename (SPEC-A2 §5/§6 S4, on the macro-free `generic_max`)
// ---------------------------------------------------------------------------

/// Rename with the daemon down is exit 7 — rename is live-only.
#[test]
fn rename_with_daemon_down_exits_7() {
    let home = tempfile::tempdir().unwrap();
    let repo = golden_repo();
    register(home.path(), repo.path());

    let (line, col) = find_pos(&repo.path().join("src/ops.rs"), "generic_max");
    let target = format!("src/ops.rs:{line}:{col}");
    let out = cq(home.path(), repo.path(), &["rename", &target, "generic_maximum"]);
    assert_exit(&out, 7);
    assert_eq!(stderr_json(&out)["error"]["code"], "LIVE_UNAVAILABLE");
}

/// Plan (no --apply) prints edits and writes nothing; --apply rewrites the
/// worktree and the fixture still compiles (A2-S4, macro-free symbol).
#[test]
fn rename_plan_then_apply_then_compiles() {
    let home = tempfile::tempdir().unwrap();
    let repo = golden_repo();
    register(home.path(), repo.path());

    let _daemon = spawn_scipd(home.path());
    wait_daemon_ready(home.path());

    let ops = repo.path().join("src/ops.rs");
    let before = std::fs::read_to_string(&ops).unwrap();
    let (line, col) = find_pos(&ops, "generic_max");
    let target = format!("src/ops.rs:{line}:{col}");

    // Poll through warming (exit 7 with a "warming" message) to the plan.
    let deadline = Instant::now() + READY_BUDGET;
    let plan = loop {
        let out = cq(home.path(), repo.path(), &["rename", &target, "generic_maximum"]);
        match out.status.code() {
            Some(0) => break stdout_json(&out),
            Some(7) => {
                let err = stderr_json(&out);
                assert_eq!(err["error"]["code"], "LIVE_UNAVAILABLE", "{err}");
                assert!(
                    err["error"]["message"].as_str().unwrap().contains("warming"),
                    "daemon is up; only warming may defer the plan: {err}"
                );
            }
            other => panic!(
                "unexpected rename exit {other:?}\nstderr: {}",
                String::from_utf8_lossy(&out.stderr)
            ),
        }
        assert!(Instant::now() < deadline, "rename never got past warming");
        std::thread::sleep(Duration::from_millis(500));
    };

    // The plan: one edit (generic_max has no call sites in the fixture),
    // content-asserted via old_text, and NOTHING written yet.
    assert_eq!(plan["applied"], false, "{plan}");
    let edits = plan["edits"].as_array().unwrap();
    assert_eq!(edits.len(), 1, "{plan}");
    assert_eq!(edits[0]["path"], "src/ops.rs", "{plan}");
    assert_eq!(edits[0]["old_text"], "generic_max", "{plan}");
    assert_eq!(edits[0]["new_text"], "generic_maximum", "{plan}");
    assert_eq!(edits[0]["line"], line as u64, "{plan}");
    assert_eq!(
        std::fs::read_to_string(&ops).unwrap(),
        before,
        "plan-only rename must write nothing"
    );

    // Apply (instance is warm now): all-or-nothing application.
    let out = cq(
        home.path(),
        repo.path(),
        &["rename", &target, "generic_maximum", "--apply"],
    );
    assert_exit(&out, 0);
    let applied = stdout_json(&out);
    assert_eq!(applied["applied"], true, "{applied}");
    assert_eq!(
        applied["files_modified"],
        serde_json::json!(["src/ops.rs"]),
        "{applied}"
    );
    let after = std::fs::read_to_string(&ops).unwrap();
    assert!(after.contains("pub fn generic_maximum<T"), "{after}");
    assert!(!after.contains("generic_max<T"), "{after}");

    // The fixture still compiles (per-worktree target dir, like cq check).
    let out = Command::new("cargo")
        .args(["check", "--quiet"])
        .current_dir(repo.path())
        .env("CARGO_TARGET_DIR", repo.path().join("target-cq"))
        .env("PATH", full_path_env())
        .output()
        .expect("spawning cargo check");
    assert!(
        out.status.success(),
        "fixture must compile after the rename\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// ---------------------------------------------------------------------------
// cq check (SPEC-A2 §5: exits 0/1/8)
// ---------------------------------------------------------------------------

#[test]
fn check_clean_then_diagnostic_then_check_failed() {
    let home = tempfile::tempdir().unwrap();
    let repo = golden_repo();
    register(home.path(), repo.path());

    // Clean fixture → exit 0, empty diagnostics, per-worktree target dir.
    let out = cq(home.path(), repo.path(), &["check"]);
    assert_exit(&out, 0);
    let report = stdout_json(&out);
    assert_eq!(report["diagnostics"], serde_json::json!([]), "{report}");
    assert!(report["checked_in_ms"].is_u64(), "{report}");
    assert!(
        repo.path().join("target-cq").is_dir(),
        "check must use <worktree>/target-cq"
    );

    // Injected type error → exit 1 with a structured diagnostic at the
    // correct path:line.
    let ops = repo.path().join("src/ops.rs");
    let clean = std::fs::read_to_string(&ops).unwrap();
    let bad_line = clean.lines().count() as u64 + 1;
    std::fs::write(
        &ops,
        format!("{clean}pub fn broken() -> i32 {{ let x: i32 = \"s\"; x }}\n"),
    )
    .unwrap();
    let out = cq(home.path(), repo.path(), &["check"]);
    assert_exit(&out, 1);
    let report = stdout_json(&out);
    let diags = report["diagnostics"].as_array().unwrap();
    let error = diags
        .iter()
        .find(|d| d["level"] == "error")
        .unwrap_or_else(|| panic!("no error diagnostic: {report}"));
    assert_eq!(error["path"], "src/ops.rs", "{report}");
    assert_eq!(error["line"], bad_line, "{report}");
    assert_eq!(error["code"], "E0308", "{report}");
    assert!(
        error["message"].as_str().unwrap().contains("mismatched types"),
        "{report}"
    );

    // FILE filter: the broken file reports it; another file's report is
    // filtered empty but the exit still says the worktree is not clean.
    let out = cq(home.path(), repo.path(), &["check", "src/ops.rs"]);
    assert_exit(&out, 1);
    assert!(
        !stdout_json(&out)["diagnostics"].as_array().unwrap().is_empty()
    );
    let out = cq(home.path(), repo.path(), &["check", "src/shapes.rs"]);
    assert_exit(&out, 1);
    assert_eq!(stdout_json(&out)["diagnostics"], serde_json::json!([]));

    // cargo absent from PATH → CHECK_FAILED, exit 8, loud.
    std::fs::write(&ops, clean).unwrap();
    let bin = tempfile::tempdir().unwrap();
    let git = run_cmd(repo.path(), "which", &["git"]);
    let git = String::from_utf8_lossy(&git.stdout).trim().to_string();
    std::os::unix::fs::symlink(&git, bin.path().join("git")).unwrap();
    let out = cq_with_path(
        home.path(),
        repo.path(),
        &["check"],
        bin.path().to_str().unwrap(),
    );
    assert_exit(&out, 8);
    let err = stderr_json(&out);
    assert_eq!(err["error"]["code"], "CHECK_FAILED", "{err}");
    assert!(out.stdout.is_empty(), "no report on CHECK_FAILED");
}
