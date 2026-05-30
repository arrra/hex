// Red test for budget-rip task Tvsz1h4f0.
//
// Asserts the post-conditions for the fixture deletion + test cleanup step
// of the budget rip:
//
//   - `tests/fixtures/bad-budget-charter.yaml` is gone.
//   - `tests/fixtures/zero-budget-charter.yaml` is gone.
//   - `tests/fixtures/valid-charter.yaml` contains no `budget:` block.
//   - `tests/charter_test.rs` has no test that asserts on budget parsing
//     (`test_reject_charter_negative_budget`, `test_zero_budget_is_unlimited`,
//     `test_zero_shift_budget_skips_enforcement`) and no `charter.budget` /
//     `CostPeriod` / `current_period` references.
//   - `tests/state_test.rs` has no `cost.current_period.budget_usd` access
//     and no `state::initialize(<id>, <budget>)` two-arg call (the budget
//     param is gone).
//   - `tests/prompt_test.rs` has no `budget:` block or `current_period`
//     reference.
//   - `tests/charter_triggers_test.rs` has no `budget:` block embedded in
//     its synthetic charter helper.
//   - No test file references the deleted fixture filenames.
//
// This file inspects the source on disk (CARGO_MANIFEST_DIR-relative) rather
// than importing Rust symbols — the whole point of the task is that those
// fixtures and test assertions are gone, so a symbol-level test would simply
// fail to compile after the rip.

use std::fs;
use std::path::{Path, PathBuf};

fn manifest_path(rel: &str) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push(rel);
    p
}

fn read_required(p: &Path) -> String {
    fs::read_to_string(p)
        .unwrap_or_else(|e| panic!("could not read {}: {e}", p.display()))
}

fn read_if_exists(p: &Path) -> Option<String> {
    fs::read_to_string(p).ok()
}

// ── fixtures ────────────────────────────────────────────────────────────────

#[test]
fn bad_budget_charter_fixture_is_gone() {
    let p = manifest_path("tests/fixtures/bad-budget-charter.yaml");
    assert!(
        !p.exists(),
        "tests/fixtures/bad-budget-charter.yaml must be deleted after budget rip \
         (still present at {})",
        p.display()
    );
}

#[test]
fn zero_budget_charter_fixture_is_gone() {
    let p = manifest_path("tests/fixtures/zero-budget-charter.yaml");
    assert!(
        !p.exists(),
        "tests/fixtures/zero-budget-charter.yaml must be deleted after budget rip \
         (still present at {})",
        p.display()
    );
}

#[test]
fn valid_charter_fixture_has_no_budget_block() {
    let p = manifest_path("tests/fixtures/valid-charter.yaml");
    let src = read_required(&p);
    // The contract's verify-gate uses `! grep -q '^budget:'` — match that
    // exact predicate (a `budget:` key at column 0, i.e. the YAML top-level
    // mapping key).
    for (lineno, line) in src.lines().enumerate() {
        assert!(
            !line.starts_with("budget:"),
            "tests/fixtures/valid-charter.yaml must not carry a top-level `budget:` \
             block after budget rip; offending line {}:{}",
            lineno + 1,
            line
        );
    }
}

// ── charter_test.rs ─────────────────────────────────────────────────────────

#[test]
fn charter_test_rs_has_no_budget_assertions() {
    let p = manifest_path("tests/charter_test.rs");
    let src = read_required(&p);

    // The three pre-rip tests that asserted on budget parsing / enforcement
    // must be removed wholesale.
    for offender in [
        "test_reject_charter_negative_budget",
        "test_zero_budget_is_unlimited",
        "test_zero_shift_budget_skips_enforcement",
    ] {
        assert!(
            !src.contains(offender),
            "tests/charter_test.rs must not define `{offender}` after budget rip — \
             the budget concept is gone, so the test no longer has a subject"
        );
    }

    // No surviving assertion may reach into `charter.budget` (the field is
    // gone from `Charter`), `CostPeriod`, or `current_period` (gone from
    // `Cost`).
    for offender in ["charter.budget", "CostPeriod", "current_period"] {
        assert!(
            !src.contains(offender),
            "tests/charter_test.rs must not reference `{offender}` after budget rip"
        );
    }
}

// ── state_test.rs ───────────────────────────────────────────────────────────

#[test]
fn state_test_rs_has_no_budget_field_access() {
    let p = manifest_path("tests/state_test.rs");
    let src = read_required(&p);

    // `cost.current_period.budget_usd` is the canonical pre-rip access path
    // that this test file used in `test_initialize_new_state` and
    // `test_save_and_load_roundtrip`. After the rip the field is gone.
    assert!(
        !src.contains("current_period"),
        "tests/state_test.rs must not reference `cost.current_period` after budget rip"
    );
    assert!(
        !src.contains("budget_usd"),
        "tests/state_test.rs must not reference `budget_usd` after budget rip"
    );

    // `state::initialize` no longer takes a budget arg. Catch the two-arg
    // call shape (`state::initialize("...", <number>)`) — any surviving call
    // must be the new single-arg signature.
    for line in src.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("state::initialize(") || trimmed.contains(" state::initialize(") {
            // Strip the leading prefix up to `state::initialize(`.
            let arg_region = trimmed
                .split_once("state::initialize(")
                .map(|(_, rest)| rest)
                .unwrap_or("");
            // Two-arg call has a comma between the two args before the
            // closing paren on the same line. Single-arg has no comma.
            // We approximate: if the substring up to the matching `)` on
            // this line contains a comma, it's the old two-arg call.
            if let Some(close_idx) = arg_region.find(')') {
                let inside = &arg_region[..close_idx];
                assert!(
                    !inside.contains(','),
                    "tests/state_test.rs still calls `state::initialize(<id>, <budget>)` \
                     with a budget arg after the rip — must be single-arg now: {line}"
                );
            }
        }
    }
}

// ── prompt_test.rs ──────────────────────────────────────────────────────────

#[test]
fn prompt_test_rs_has_no_budget_or_current_period_references() {
    let p = manifest_path("tests/prompt_test.rs");
    let src = read_required(&p);

    // A `budget:` YAML block embedded in a synthetic charter (heredoc /
    // raw-string literal) is the pre-rip pattern that needs to go.
    for (lineno, line) in src.lines().enumerate() {
        let trimmed = line.trim_start();
        assert!(
            !trimmed.starts_with("budget:"),
            "tests/prompt_test.rs must not embed a `budget:` block in synthetic \
             charter YAML after budget rip; offending line {}:{}",
            lineno + 1,
            line
        );
    }

    for offender in ["CostPeriod", "current_period", "budget_usd"] {
        assert!(
            !src.contains(offender),
            "tests/prompt_test.rs must not reference `{offender}` after budget rip"
        );
    }
}

// ── charter_triggers_test.rs ────────────────────────────────────────────────

#[test]
fn charter_triggers_test_rs_has_no_budget_in_synthetic_charter() {
    let p = manifest_path("tests/charter_triggers_test.rs");
    let src = read_required(&p);

    // The `make_charter` helper writes a `budget:` block into every
    // synthetic charter.yaml. After the rip the Charter struct no longer
    // has a `budget` field, and the parser ignores stray `budget:` keys —
    // but the helper should be cleaned up to reflect the new shape.
    let mut budget_lines: Vec<String> = Vec::new();
    for (lineno, line) in src.lines().enumerate() {
        if line.contains("\"budget:\"") || line.contains("\"  usd_per_day:") {
            budget_lines.push(format!("{}:{}: {}", p.display(), lineno + 1, line));
        }
    }
    assert!(
        budget_lines.is_empty(),
        "tests/charter_triggers_test.rs must not synthesize a `budget:` block in \
         test charter.yaml content after budget rip; offending lines:\n  {}",
        budget_lines.join("\n  ")
    );
}

// ── deleted-fixture references ──────────────────────────────────────────────

#[test]
fn no_test_file_references_deleted_fixtures() {
    // Walk the integration-test directory and grep for the deleted fixture
    // filenames. Hard-coded list of suspects rather than walkdir (not a
    // dev-dependency), but we glob `tests/*.rs` directly via `read_dir`.
    let tests_dir = manifest_path("tests");
    let read = fs::read_dir(&tests_dir)
        .unwrap_or_else(|e| panic!("could not read {}: {e}", tests_dir.display()));

    let mut offenders: Vec<String> = Vec::new();
    for entry in read.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("rs") {
            continue;
        }
        // Skip THIS file — it intentionally mentions the deleted fixtures.
        if path.file_name().and_then(|s| s.to_str())
            == Some("budget_rip_fixtures_and_tests_cleanup_test.rs")
        {
            continue;
        }
        let Some(src) = read_if_exists(&path) else { continue };
        for fixture in ["bad-budget-charter.yaml", "zero-budget-charter.yaml"] {
            if src.contains(fixture) {
                offenders.push(format!("{}: references `{}`", path.display(), fixture));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "no test file may reference deleted budget fixtures after budget rip:\n  {}",
        offenders.join("\n  ")
    );
}
