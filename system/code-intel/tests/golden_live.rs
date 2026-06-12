//! Golden LIVE acceptance suite (plan Task 7, SPEC-A2 §6 S2/S3/S4).
//!
//! Real `cq` + real `scipd` (one rust-analyzer prime per test) against the
//! committed golden fixture, systematically over the recorded expectations:
//!
//! - **A2-S3 cross-check:** on FRESH files, for EVERY golden symbol, the
//!   forced-live `def`/`refs` answers equal the plain index answers
//!   (positions + sets; only the envelope `source` differs). One semantic
//!   difference is pinned exactly: LSP references on a trait method also
//!   carry the recorded impl sites. This is the rust-analyzer-upgrade
//!   canary.
//! - **A2-S2 live-on-stale:** in a linked git worktree of the fixture, a
//!   brand-new call site is found by auto-escalation (`source:"live"`)
//!   while `--no-live` misses it AND flags the file stale — both sides
//!   asserted, pinning the value of escalation.
//! - **A2-S4 (pinned limitation):** the live rename plan for `double` →
//!   `twice` is EXACTLY 3 edits and never touches the `macro_rules!`-body
//!   token. The macro-free `generic_max`→`generic_maximum` `--apply` +
//!   compile half of S4 is covered by
//!   `tests/cli_live.rs::rename_plan_then_apply_then_compiles`.
//!
//! Harness helpers shared with tests/cli_live.rs via tests/common/.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Instant;

mod common;
use common::{
    append_brand_new_caller, assert_exit, cq, def_selector, expectations, golden_repo, register,
    register_and_index, run_cmd, spawn_scipd, stderr_json, stdout_json, wait_daemon_ready,
    READY_BUDGET,
};

/// (path, line, col, role) tuple set from envelope results.
fn result_sites(env: &serde_json::Value) -> BTreeSet<(String, u64, u64, String)> {
    env["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["path"].as_str().unwrap().to_string(),
                r["line"].as_u64().unwrap(),
                r["col"].as_u64().unwrap(),
                r["role"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// Poll a forced-live query through instance warming until it answers.
/// Warming surfaces as `LIVE_UNAVAILABLE` exit 7 (the daemon is up, so
/// nothing else may defer the answer); anything else is a hard failure.
fn warm_instance(home: &Path, repo: &Path, selector: &str) {
    let deadline = Instant::now() + READY_BUDGET;
    loop {
        let out = cq(home, repo, &["def", selector, "--live"]);
        match out.status.code() {
            Some(0) => return,
            Some(7) => {
                let err = stderr_json(&out);
                assert_eq!(err["error"]["code"], "LIVE_UNAVAILABLE", "{err}");
            }
            other => panic!(
                "unexpected forced-live exit {other:?} while warming\nstderr: {}",
                String::from_utf8_lossy(&out.stderr)
            ),
        }
        assert!(
            Instant::now() < deadline,
            "live instance never became ready within {READY_BUDGET:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

// ---------------------------------------------------------------------------
// A2-S3: live == index on fresh files, for every golden symbol
// ---------------------------------------------------------------------------

/// On fresh files (real scipd, instance warmed) the forced-live `def` and
/// `refs` answers must equal the plain index answers for EVERY symbol in
/// tests/fixtures/golden-expectations.json — positions and sets identical,
/// envelope `source` the only difference. A divergence here is the
/// rust-analyzer-upgrade canary firing: the live server now answers
/// differently from the SCIP-indexed ground truth.
#[test]
fn live_answers_match_index_for_every_golden_symbol() {
    let home = tempfile::tempdir().unwrap();
    let repo = golden_repo();
    register_and_index(home.path(), repo.path());

    let _daemon = spawn_scipd(home.path());
    wait_daemon_ready(home.path());

    let exp = expectations();
    let symbols = exp["symbols"].as_array().unwrap();
    assert!(symbols.len() >= 10, "golden fixture shrank: {}", symbols.len());
    warm_instance(home.path(), repo.path(), &def_selector(&symbols[0]));

    for sym in symbols {
        let name = sym["name"].as_str().unwrap();
        let selector = def_selector(sym);

        // -- def: position must agree --
        let out = cq(home.path(), repo.path(), &["def", &selector]);
        assert_exit(&out, 0);
        let index_env = stdout_json(&out);
        assert_eq!(index_env["source"], "index", "{name}: {index_env}");

        let out = cq(home.path(), repo.path(), &["def", &selector, "--live"]);
        assert_exit(&out, 0);
        let live_env = stdout_json(&out);
        assert_eq!(live_env["source"], "live", "{name}: {live_env}");
        assert_eq!(
            live_env["stale_files"],
            serde_json::json!([]),
            "{name}: nothing is stale on fresh files\n{live_env}"
        );

        assert_eq!(
            result_sites(&live_env),
            result_sites(&index_env),
            "RUST-ANALYZER-UPGRADE CANARY (A2-S3): live def({name}) diverged \
             from the index def on FRESH files. The live rust-analyzer now \
             answers differently from the recorded SCIP ground truth — an \
             upstream behavior change, not a flake. Re-verify the golden \
             expectations against the new rust-analyzer before trusting \
             either side.\nlive: {live_env}\nindex: {index_env}"
        );

        // -- refs: occurrence sets must agree, modulo ONE pinned semantic
        // difference: LSP `textDocument/references` on a trait method also
        // returns the impl declaration sites, which SCIP models as separate
        // impl symbols (verified empirically 2026-06-11 — the only
        // live/index divergence across the whole golden set). For symbols
        // carrying a recorded `impls` array, the expected live set is the
        // index set plus EXACTLY those impl positions as references;
        // anything beyond that is the canary firing. --
        let out = cq(home.path(), repo.path(), &["refs", &selector]);
        assert_exit(&out, 0);
        let index_env = stdout_json(&out);
        assert_eq!(index_env["source"], "index", "{name}: {index_env}");

        let out = cq(home.path(), repo.path(), &["refs", &selector, "--live"]);
        assert_exit(&out, 0);
        let live_env = stdout_json(&out);
        assert_eq!(live_env["source"], "live", "{name}: {live_env}");

        let mut want = result_sites(&index_env);
        if let Some(impls) = sym.get("impls").and_then(|i| i.as_array()) {
            for imp in impls {
                want.insert((
                    imp["path"].as_str().unwrap().to_string(),
                    imp["line"].as_u64().unwrap(),
                    imp["col"].as_u64().unwrap(),
                    "reference".to_string(),
                ));
            }
        }
        assert_eq!(
            result_sites(&live_env),
            want,
            "RUST-ANALYZER-UPGRADE CANARY (A2-S3): live refs({name}) diverged \
             from the index refs set on FRESH files (after accounting for \
             the pinned trait-method/impl-sites difference). The live \
             rust-analyzer now answers differently from the recorded SCIP \
             ground truth — e.g. it may have started (or stopped) seeing \
             macro-expanded occurrences or impl declarations. Re-verify the \
             golden expectations against the new rust-analyzer before \
             trusting either side.\nlive: {live_env}\nindex: {index_env}"
        );
    }
}

// ---------------------------------------------------------------------------
// A2-S2: live-on-stale in a linked worktree of the fixture
// ---------------------------------------------------------------------------

/// Edit a linked git worktree of the indexed fixture: auto-escalation must
/// return the brand-new call site with `source:"live"`, and `--no-live`
/// must BOTH miss it and flag the file stale — asserting the two sides
/// together pins exactly what escalation buys.
#[test]
fn worktree_edit_escalates_live_while_no_live_misses_and_flags_stale() {
    let home = tempfile::tempdir().unwrap();
    let repo = golden_repo();
    register_and_index(home.path(), repo.path());

    let wt_parent = tempfile::tempdir().unwrap();
    let wt = wt_parent.path().join("wt");
    run_cmd(
        repo.path(),
        "git",
        &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "live-edit"],
    );
    let new_line = append_brand_new_caller(&wt);

    let _daemon = spawn_scipd(home.path());
    wait_daemon_ready(home.path());

    // --no-live side first (daemon up but must never be consulted): the
    // pure index answer misses the new call site AND flags the stale file.
    let out = cq(home.path(), &wt, &["refs", "double", "--no-live"]);
    assert_exit(&out, 2);
    let env = stdout_json(&out);
    assert_eq!(env["source"], "index", "{env}");
    assert!(env.get("escalated").is_none(), "--no-live never escalates: {env}");
    assert_eq!(
        env["stale_files"],
        serde_json::json!(["src/ops.rs"]),
        "A2-S2: --no-live must flag the edited worktree file stale\n{env}"
    );
    assert!(
        !env["results"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["path"] == "src/ops.rs" && r["line"] == new_line),
        "A2-S2: the index cannot know the brand-new call site at \
         src/ops.rs:{new_line} — --no-live must miss it\n{env}"
    );

    // Auto side: poll the SAME query through warming until live answers.
    let deadline = Instant::now() + READY_BUDGET;
    let env = loop {
        let out = cq(home.path(), &wt, &["refs", "double"]);
        let env = stdout_json(&out);
        if env["source"] == "live" {
            assert_exit(&out, 0);
            break env;
        }
        assert_eq!(
            env["escalated"]["reason"], "warming",
            "daemon is up; only warming may defer the live answer: {env}"
        );
        assert!(
            Instant::now() < deadline,
            "live instance never became ready within {READY_BUDGET:?}; last: {env}"
        );
        std::thread::sleep(std::time::Duration::from_millis(500));
    };
    assert_eq!(env["stale_files"], serde_json::json!([]), "{env}");
    assert!(env.get("escalated").is_none(), "{env}");
    assert!(
        env["results"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["path"] == "src/ops.rs" && r["line"] == new_line),
        "A2-S2: auto-escalation must return the brand-new call site at \
         src/ops.rs:{new_line} with source:\"live\"\n{env}"
    );
}

// ---------------------------------------------------------------------------
// A2-S4 (pinned limitation): rename `double` → `twice` is exactly 3 edits
// ---------------------------------------------------------------------------

/// The live rename plan for `double` → `twice` is EXACTLY the 3 token
/// sites the index also knows (def + 2 call sites) and does NOT touch the
/// `crate::ops::double($x)` token inside the `macro_rules!` body — live
/// rust-analyzer is macro-body-blind exactly like the index (T4 empirical
/// finding). Per the amended A2-S4 this is asserted as an edit COUNT, not
/// a compile check: applying this rename WOULD break compilation (the
/// macro body still calls `double`), which is the documented limitation.
///
/// The macro-free success path (`generic_max` → `generic_maximum` with
/// `--apply`, then `cargo check` green) is covered by
/// `tests/cli_live.rs::rename_plan_then_apply_then_compiles`.
#[test]
fn rename_double_plan_pins_macro_body_blindness_at_three_edits() {
    let home = tempfile::tempdir().unwrap();
    let repo = golden_repo();
    register(home.path(), repo.path());

    let _daemon = spawn_scipd(home.path());
    wait_daemon_ready(home.path());

    let exp = expectations();
    let double = exp["symbols"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "double")
        .expect("expectations cover `double`");
    let selector = def_selector(double);
    let ops = repo.path().join("src/ops.rs");
    let before = std::fs::read_to_string(&ops).unwrap();

    // Poll through warming (rename is live-only: exit 7 until primed).
    let deadline = Instant::now() + READY_BUDGET;
    let plan = loop {
        let out = cq(home.path(), repo.path(), &["rename", &selector, "twice"]);
        match out.status.code() {
            Some(0) => break stdout_json(&out),
            Some(7) => {
                let err = stderr_json(&out);
                assert_eq!(err["error"]["code"], "LIVE_UNAVAILABLE", "{err}");
            }
            other => panic!(
                "unexpected rename exit {other:?}\nstderr: {}",
                String::from_utf8_lossy(&out.stderr)
            ),
        }
        assert!(Instant::now() < deadline, "rename never got past warming");
        std::thread::sleep(std::time::Duration::from_millis(500));
    };

    assert_eq!(plan["applied"], false, "{plan}");
    let edits = plan["edits"].as_array().unwrap();
    assert_eq!(
        edits.len(),
        3,
        "PINNED LIMITATION (A2-S4): the live rename plan for `double` must \
         be EXACTLY 3 edits — def + the 2 macro-free call sites. Live \
         rust-analyzer does NOT rename the `crate::ops::double($x)` token \
         inside the `macro_rules!` body (macro-body-blind, like the index), \
         so applying this rename breaks compilation. A different count \
         means upstream rust-analyzer changed rename-through-macro \
         behavior: re-verify and update docs/code-intel.md's rename \
         warning.\n{plan}"
    );

    // The 3 edit sites are exactly the golden refs occurrences of `double`
    // (the only sites the macro-body-blind view can know about).
    let got: BTreeSet<(String, u64, u64)> = edits
        .iter()
        .map(|e| {
            assert_eq!(e["old_text"], "double", "{plan}");
            assert_eq!(e["new_text"], "twice", "{plan}");
            (
                e["path"].as_str().unwrap().to_string(),
                e["line"].as_u64().unwrap(),
                e["col"].as_u64().unwrap(),
            )
        })
        .collect();
    let want: BTreeSet<(String, u64, u64)> = double["refs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["path"].as_str().unwrap().to_string(),
                r["line"].as_u64().unwrap(),
                r["col"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(got, want, "rename edits must hit the golden refs sites\n{plan}");

    // Belt and braces: the macro_rules! body line carries no edit.
    let macro_line = before
        .lines()
        .position(|l| l.contains("macro_rules!"))
        .expect("fixture has the macro_rules! line") as u64
        + 1;
    assert!(
        !edits
            .iter()
            .any(|e| e["path"] == "src/ops.rs" && e["line"] == macro_line),
        "an edit landed on the macro_rules! body line {macro_line} — \
         rust-analyzer started renaming inside macro bodies; the pinned \
         limitation no longer holds\n{plan}"
    );

    // Plan-only: nothing written.
    assert_eq!(
        std::fs::read_to_string(&ops).unwrap(),
        before,
        "plan-only rename must write nothing"
    );
}
