// Red tests for `hex lint-gates` (task T33capr0f, spec S253fety6).
//
// Pins the contract for the `lint_gates` module:
//   • A `analyze_command(&str)` function returns a verdict whose `predicted`
//     is `Fail` for known footgun patterns (e.g. swallowing stderr with
//     `2>/dev/null`) and `Pass` for a clean command, with non-empty
//     `rules_fired` on the footgun case.
//   • `normalize_command` collapses surrounding whitespace so two
//     semantically-equal commands hash identically.
//   • `content_hash` returns the same hex hash for two whitespace-different
//     forms of the same normalized command (gate content-hash keying).
//   • A shadow-mode summary line never leaks per-gate advice; it carries
//     the literal token "shadow" and a flagged count.
//
// These tests are expected to FAIL until src/lint_gates.rs is implemented and
// exposed via lib.rs.

use hex::lint_gates;

#[test]
fn lint_flags_stderr_swallow_footgun() {
    // `2>/dev/null` hides stderr and is one of the CLAUDE.md verify-gate
    // footguns — the rule MUST fire and the prediction MUST be Fail.
    let v = lint_gates::analyze_command("cargo test --quiet 2>/dev/null && grep -q foo src/bar.rs");
    assert!(
        matches!(v.predicted, lint_gates::Prediction::Fail),
        "stderr-swallowing footgun must predict Fail, got {:?}",
        v.predicted,
    );
    assert!(
        !v.rules_fired.is_empty(),
        "at least one footgun rule must fire on `2>/dev/null` pattern"
    );
}

#[test]
fn lint_passes_a_clean_command() {
    // A vanilla `test -f some/path` has no footgun marker; it must
    // predict Pass with no rules fired.
    let v = lint_gates::analyze_command("test -f system/harness/Cargo.toml");
    assert!(
        matches!(v.predicted, lint_gates::Prediction::Pass),
        "clean command must predict Pass, got {:?}",
        v.predicted,
    );
    assert!(
        v.rules_fired.is_empty(),
        "clean command must fire zero rules, got {:?}",
        v.rules_fired,
    );
}

#[test]
fn lint_normalize_collapses_whitespace_for_hash() {
    let a = lint_gates::normalize_command("  cargo   test   --quiet   ");
    let b = lint_gates::normalize_command("cargo test --quiet");
    assert_eq!(a, b, "normalization must collapse surrounding whitespace");

    let ha = lint_gates::content_hash("  cargo   test   --quiet   ");
    let hb = lint_gates::content_hash("cargo test --quiet");
    assert_eq!(
        ha, hb,
        "content_hash must be invariant under whitespace normalization"
    );
    assert_eq!(
        ha.len(),
        64,
        "content_hash must be a 64-char sha256 hex digest"
    );
}

#[test]
fn lint_shadow_summary_has_no_per_gate_advice() {
    // The shadow-mode default output is a single summary line. It MUST
    // carry "shadow" and a flagged count, and MUST NOT leak per-gate advice.
    let gates = vec![
        "cargo test 2>/dev/null".to_string(),
        "test -f Cargo.toml".to_string(),
    ];
    let summary = lint_gates::shadow_summary(&gates);
    assert_eq!(
        summary.lines().count(),
        1,
        "shadow summary must be exactly one line, got: {:?}",
        summary
    );
    assert!(
        summary.to_lowercase().contains("shadow"),
        "shadow summary must announce shadow mode: {:?}",
        summary
    );
    assert!(
        !summary.to_lowercase().contains("advice"),
        "shadow mode must not emit per-gate advice text: {:?}",
        summary
    );
}
