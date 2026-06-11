//! Golden acceptance suite (plan Task 10, SPEC-A1 §8 success criterion S2).
//!
//! Loads `tests/fixtures/golden-expectations.json` (positions recorded from a
//! real `rust-analyzer scip` emit of the committed fixture crate) and
//! `tests/fixtures/callers-gate.json` (the resolved callers gate, spec §8),
//! runs the FULL pipeline via the real `cq` binary (register → index → every
//! verb), and asserts every expectation exactly:
//!
//! - `def`: exact path/line/col + symbol + role + snippet per golden symbol
//! - `refs`: exact occurrence sets and counts (definitions flagged)
//! - `callers`: exact caller sets, gated macro-body cases pinned ABSENT
//! - `symbols`: exact per-file outlines in source order
//! - `search`: required hits present
//!
//! Spawn pattern and golden-fixture repo helper follow tests/cli.rs.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

// ---- harness (tests/cli.rs pattern) ----

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
    Command::new(env!("CARGO_BIN_EXE_cq"))
        .args(args)
        .current_dir(cwd)
        .env("CODEINTEL_HOME", home)
        .env("PATH", full_path_env())
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

fn assert_exit(out: &Output, want: i32, what: &str) {
    assert_eq!(
        out.status.code().expect("cq terminated by signal"),
        want,
        "exit code for {what}\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
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

/// Golden fixture crate copied to a tempdir, git-initialized + committed.
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

/// Full pipeline front half: `cq register` + `cq index` on the fixture repo.
fn register_and_index(home: &Path, repo: &Path) {
    let out = cq(home, repo, &["register", repo.to_str().unwrap()]);
    assert_exit(&out, 0, "register");
    let out = cq(home, repo, &["index"]);
    assert_exit(&out, 0, "index");
    let report = stdout_json(&out);
    assert_eq!(report["emit_exit_code"], 0, "{report}");
}

// ---- expectation fixtures ----

fn load_fixture_json(rel: &str) -> serde_json::Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()))
}

fn expectations() -> serde_json::Value {
    load_fixture_json("tests/fixtures/golden-expectations.json")
}

fn callers_gate() -> serde_json::Value {
    load_fixture_json("tests/fixtures/callers-gate.json")
}

/// `FILE:LINE:COL` selector for a symbol's recorded definition site —
/// position-based selection is unambiguous even for symbols sharing a
/// display name (the three `area` definitions).
fn def_selector(sym: &serde_json::Value) -> String {
    format!(
        "{}:{}:{}",
        sym["def"]["path"].as_str().unwrap(),
        sym["def"]["line"].as_u64().unwrap(),
        sym["def"]["col"].as_u64().unwrap()
    )
}

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

/// The exact 1-based source line from the fixture repo (snippet ground truth).
fn source_line(repo: &Path, rel_path: &str, line: u64) -> String {
    let content = std::fs::read_to_string(repo.join(rel_path)).unwrap();
    content
        .lines()
        .nth(usize::try_from(line).unwrap() - 1)
        .unwrap_or_else(|| panic!("{rel_path} has no line {line}"))
        .to_string()
}

/// Expectation kinds are written snake_case ("trait_method"); the envelope
/// lowercases the SCIP enum variant name ("traitmethod"). Same kind, two
/// spellings — normalize by dropping underscores. NOT a relaxation: every
/// distinct SCIP kind still maps to a distinct normalized string.
fn norm_kind(k: &str) -> String {
    k.replace('_', "")
}

// ---- S2: def + refs + callers over every golden symbol ----

#[test]
fn golden_defs_refs_and_callers() {
    let home = tempfile::tempdir().unwrap();
    let repo = golden_repo();
    register_and_index(home.path(), repo.path());

    let exp = expectations();
    let gate = callers_gate();
    let macro_body_visible = gate["macro_body_callers_from_index"]
        .as_bool()
        .expect("callers-gate.json: macro_body_callers_from_index must be a bool");

    let symbols = exp["symbols"].as_array().unwrap();
    assert!(
        symbols.len() >= 10,
        "spec S2 requires >=10 golden symbols, fixture has {}",
        symbols.len()
    );

    for sym in symbols {
        let name = sym["name"].as_str().unwrap();
        let scip_symbol = sym["scip_symbol"].as_str().unwrap();
        let selector = def_selector(sym);

        // -- def: exact position, symbol, role, kind, snippet --
        let out = cq(home.path(), repo.path(), &["def", &selector]);
        assert_exit(&out, 0, &format!("def {name} ({selector})"));
        let env = stdout_json(&out);
        assert_eq!(env["source"], "index", "{name}: {env}");
        assert_eq!(env["stale_files"], serde_json::json!([]), "{name}: {env}");
        let results = env["results"].as_array().unwrap();
        assert_eq!(results.len(), 1, "{name}: one definition site\n{env}");
        let r = &results[0];
        assert_eq!(r["path"], sym["def"]["path"], "{name} def path: {env}");
        assert_eq!(r["line"], sym["def"]["line"], "{name} def line: {env}");
        assert_eq!(r["col"], sym["def"]["col"], "{name} def col: {env}");
        assert_eq!(r["role"], "definition", "{name}: {env}");
        assert_eq!(r["symbol"], scip_symbol, "{name} SCIP symbol: {env}");
        assert_eq!(
            norm_kind(r["kind"].as_str().unwrap()),
            norm_kind(sym["kind"].as_str().unwrap()),
            "{name} kind: {env}"
        );
        let want_snippet = source_line(
            repo.path(),
            sym["def"]["path"].as_str().unwrap(),
            sym["def"]["line"].as_u64().unwrap(),
        );
        assert_eq!(
            r["snippet"].as_str(),
            Some(want_snippet.as_str()),
            "{name} snippet: {env}"
        );

        // -- refs: exact site set + count (definitions flagged) --
        let out = cq(home.path(), repo.path(), &["refs", &selector]);
        assert_exit(&out, 0, &format!("refs {name} ({selector})"));
        let env = stdout_json(&out);
        let got = result_sites(&env);
        let want: BTreeSet<(String, u64, u64, String)> = sym["refs"]
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
            .collect();
        assert_eq!(got, want, "{name} refs sites\n{env}");
        assert_eq!(
            got.len() as u64,
            sym["refs_count"].as_u64().unwrap(),
            "{name} refs_count\n{env}"
        );

        // -- callers: exact caller set + gated macro-body pinning --
        let Some(callers) = sym.get("callers") else {
            continue; // no callers expectation recorded for this symbol
        };
        let out = cq(home.path(), repo.path(), &["callers", &selector]);
        assert_exit(&out, 0, &format!("callers {name} ({selector})"));
        let env = stdout_json(&out);
        // Resolved gate (smoke test #2 + fixture emit, 2026-06-11): callers
        // ships from the index with NO "quality" flag.
        assert!(
            env.get("quality").is_none() || env["quality"].is_null(),
            "{name}: callers must ship without a quality flag\n{env}"
        );
        let got: BTreeSet<&str> = env["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["display_name"].as_str().unwrap())
            .collect();
        let want: BTreeSet<&str> = callers["from_index"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c.as_str().unwrap())
            .collect();
        assert_eq!(got, want, "{name} callers(from_index)\n{env}");
        // Every caller result is located at the caller's definition site.
        for r in env["results"].as_array().unwrap() {
            assert_eq!(r["role"], "definition", "{name} caller site role: {env}");
        }

        for gated in callers["gated"].as_array().unwrap() {
            let gated_name = gated["name"].as_str().unwrap();
            assert_eq!(
                gated["macro_case"], "expected_but_gated",
                "{name}: unknown gated case shape: {gated}"
            );
            if macro_body_visible {
                assert!(
                    got.contains(gated_name),
                    "{name}: callers gate says macro-body callers ARE indexed, \
                     but {gated_name} is missing from callers({name})\n{env}"
                );
            } else {
                assert!(
                    !got.contains(gated_name),
                    "{gated_name} appeared in callers({name}): rust-analyzer now \
                     emits call-site occurrences from macro_rules! bodies. This \
                     pins the known macro-body limitation (SPEC-A1 §8 callers \
                     gate) — it MUST fail loudly so we notice the behavior \
                     change. Update tests/fixtures/callers-gate.json and the \
                     docs/code-intel.md limitation note.\n{env}"
                );
            }
        }
    }
}

// ---- S2: per-file symbol outlines ----

#[test]
fn golden_file_outlines() {
    let home = tempfile::tempdir().unwrap();
    let repo = golden_repo();
    register_and_index(home.path(), repo.path());

    for (file, outline) in expectations()["file_outlines"].as_object().unwrap() {
        let out = cq(home.path(), repo.path(), &["symbols", file]);
        assert_exit(&out, 0, &format!("symbols {file}"));
        let env = stdout_json(&out);
        assert_eq!(env["stale_files"], serde_json::json!([]), "{file}: {env}");
        let got: Vec<&str> = env["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["display_name"].as_str().unwrap())
            .collect();
        let want: Vec<&str> = outline
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n.as_str().unwrap())
            .collect();
        assert_eq!(got, want, "{file} outline (source order)\n{env}");
        for r in env["results"].as_array().unwrap() {
            assert_eq!(r["path"].as_str(), Some(file.as_str()), "{file}: {env}");
            assert_eq!(r["role"], "definition", "{file}: {env}");
        }
    }
}

// ---- S2: search hits ----

#[test]
fn golden_search_hits() {
    let home = tempfile::tempdir().unwrap();
    let repo = golden_repo();
    register_and_index(home.path(), repo.path());

    for case in expectations()["search"].as_array().unwrap() {
        let query = case["query"].as_str().unwrap();
        let out = cq(home.path(), repo.path(), &["search", query]);
        assert_exit(&out, 0, &format!("search {query}"));
        let env = stdout_json(&out);
        let got: BTreeSet<&str> = env["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["display_name"].as_str().unwrap())
            .collect();
        for must in case["must_include"].as_array().unwrap() {
            let must = must.as_str().unwrap();
            assert!(
                got.contains(must),
                "search {query:?} must include {must:?}, got {got:?}\n{env}"
            );
        }
    }
}

// ---- callers gate file: shape pinned so a malformed edit fails loudly ----

#[test]
fn callers_gate_file_is_well_formed_and_resolved() {
    let gate = callers_gate();
    let obj = gate.as_object().unwrap();
    assert_eq!(obj.len(), 2, "exactly the two gate flags: {gate}");
    // Resolved values (plan Task 10 / smoke test #2, 2026-06-11): macro-BODY
    // call sites are invisible to the index; macro-ARGUMENT call sites are
    // captured. Changing either flag is a deliberate gate re-resolution.
    assert_eq!(gate["macro_body_callers_from_index"], false, "{gate}");
    assert_eq!(gate["macro_arg_callers_from_index"], true, "{gate}");

    // Consistency with the expectations file: the macro-arg case (fmt_user
    // calls double() inside a format! argument) must be expected from the
    // index, and the macro-body case (macro_caller via call_double!) must be
    // recorded as gated, not as a from_index caller.
    let exp = expectations();
    let double = exp["symbols"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "double")
        .expect("expectations cover `double`");
    let from_index: Vec<&str> = double["callers"]["from_index"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap())
        .collect();
    assert!(from_index.contains(&"fmt_user"), "macro-arg caller expected");
    assert!(
        !from_index.contains(&"macro_caller"),
        "macro-body caller must be gated, not expected from the index"
    );
    assert!(
        double["callers"]["gated"]
            .as_array()
            .unwrap()
            .iter()
            .any(|g| g["name"] == "macro_caller"),
        "macro_caller must be recorded as the gated macro-body case"
    );
}
