//! Verify-gate FOOTGUN linter — shadow mode (spec S253fety6, task T33capr0f).
//!
//! Parses a BOI v2 TOML spec, extracts every verification command (contract
//! and per-task), then runs the footgun ruleset against each command. The 8
//! rules are ported faithfully from the CLAUDE.md verify-gate footgun table
//! (and bakeoff5/harness.py FOOTGUN_RULES, lines 149–203):
//!
//!   1. `path-127`             — non-coreutils binary without exported PATH
//!   2. `pipe-tail-exitcode`   — piping through tail/head before checking exit
//!   3. `deployed-binary`      — checking `.hex/bin/hex` rather than `target/`
//!   4. `hex-from-worker`      — verifying derived state via a `hex` subcommand
//!   5. `inverted-grep-v`      — `grep -q -v` (or any -qv / -v -q combo)
//!   6. `macos-wc-whitespace`  — `... | wc -l | grep -q "^N$"` (mac pads)
//!   7. `python-c-indent`      — `python3 -c "…"` body with leading indent
//!   8. `stderr-swallow`       — `2>/dev/null` (SO S6: no quiet failures)
//!
//! ## Shadow mode (default)
//!
//! Output is one summary line: `N gates, M flagged, shadow mode — predictions
//! logged, not advice`. NO per-gate advice. Each gate is written as a single
//! `intent` row in the ledger keyed by `content_hash(normalize_command(cmd))`
//! with payload `{predicted, rules_fired, shadow:true, command, spec_id?}`.
//! `--spec-id <id>` is supported as an amend hook for after dispatch.

use regex::Regex;
use serde::Serialize;
use sha2::{Digest, Sha256};

/// Prediction emitted by [`analyze_command`].
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub enum Prediction {
    Pass,
    Fail,
}

/// Result of running every footgun rule against one command.
#[derive(Debug, Clone, Serialize)]
pub struct Verdict {
    pub predicted: Prediction,
    /// IDs of the footgun rules that fired. Empty ⇒ `Prediction::Pass`.
    pub rules_fired: Vec<String>,
    /// The canonical (whitespace-collapsed) command we evaluated.
    pub normalized: String,
    /// Content-addressable hash of `normalized`. 64 hex chars.
    pub content_hash: String,
}

/// Collapse runs of whitespace and trim — gates that differ only in spacing
/// hash identically. This is the canonical form used for `content_hash`.
pub fn normalize_command(cmd: &str) -> String {
    cmd.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Sha256 hex digest of [`normalize_command`]`(cmd)`. 64 chars.
pub fn content_hash(cmd: &str) -> String {
    let n = normalize_command(cmd);
    let mut h = Sha256::new();
    h.update(n.as_bytes());
    format!("{:x}", h.finalize())
}

// ---------------------------------------------------------------------------
// Footgun rules (8) — each is a closure `(&str) -> bool` over the RAW command,
// keyed by a short stable id. The id is what lands in `Verdict.rules_fired`.
// ---------------------------------------------------------------------------

fn rule_path_127(cmd: &str) -> bool {
    // Non-coreutils binary invoked without an `export PATH=` prefix.
    const BINS: &[&str] = &[
        "cargo ", "node ", "pnpm ", "npm ", "yarn ", "rustc ", "rustup ", "deno ", "bun ",
    ];
    let hit = BINS
        .iter()
        .any(|b| cmd.contains(b) || cmd.trim_start().starts_with(b.trim_end()));
    if !hit {
        return false;
    }
    !cmd.contains("export PATH=")
}

fn rule_pipe_tail_exitcode(cmd: &str) -> bool {
    // `... | tail ...` or `... | head ...` swallows the upstream exit code.
    let bytes = cmd.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'|' && bytes.get(i + 1) != Some(&b'|') {
            // i indexes the ASCII '|' byte ⇒ i+1 is a char boundary.
            #[allow(clippy::string_slice)]
            let rest = &cmd[i + 1..].trim_start();
            if rest.starts_with("tail ")
                || rest.starts_with("tail\t")
                || rest.starts_with("head ")
                || rest.starts_with("head\t")
            {
                return true;
            }
        }
        i += 1;
    }
    false
}

fn rule_deployed_binary(cmd: &str) -> bool {
    // Checking the *deployed* `.hex/bin/hex` binary after a build (rather than
    // `target/release/hex`) verifies yesterday's binary, not the fresh build.
    cmd.contains(".hex/bin/hex")
}

/// Split a command line into top-level segments on shell separators
/// (`&&`, `||`, `;`, `|`, `&`, newline, `(`, `)`). Not a full shell parser —
/// just enough to tell a token in COMMAND position from one used as an argument.
/// Returns trimmed, non-empty segments.
///
/// Note: treating a lone `&` as a separator also splits a `2>&1` redirection
/// into `2>` and `1` fragments — benign here, since neither fragment begins with
/// `hex`, and it lets us catch backgrounded `… & hex <sub>`.
fn command_segments(cmd: &str) -> Vec<&str> {
    let bytes = cmd.as_bytes();
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        let is_two = i + 1 < bytes.len()
            && ((bytes[i] == b'&' && bytes[i + 1] == b'&')
                || (bytes[i] == b'|' && bytes[i + 1] == b'|'));
        let is_one = matches!(bytes[i], b';' | b'|' | b'&' | b'\n' | b'(' | b')');
        if is_two || is_one {
            // start and i both index ASCII separator bytes (byte-scan) ⇒ both are char boundaries.
            #[allow(clippy::string_slice)]
            let seg = cmd[start..i].trim();
            if !seg.is_empty() {
                out.push(seg);
            }
            i += if is_two { 2 } else { 1 };
            start = i;
        } else {
            i += 1;
        }
    }
    // start indexes 0 or an ASCII separator byte (byte-scan) ⇒ it is a char boundary.
    #[allow(clippy::string_slice)]
    let seg = cmd[start..].trim();
    if !seg.is_empty() {
        out.push(seg);
    }
    out
}

/// A `VAR=val` env-assignment token (leading `KEY=` with an identifier key),
/// e.g. `HEX_DIR=/tmp/x`. Used to skip env prefixes before the real command.
fn is_env_assignment(tok: &str) -> bool {
    match tok.split_once('=') {
        Some((key, _)) => {
            !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        None => false,
    }
}

fn rule_hex_from_worker(cmd: &str) -> bool {
    // A `hex <subcommand>` invocation inside a verify-gate reads $HEX_DIR from
    // the main workspace and ignores worktree edits. The worker should check
    // artifacts it produced, not derived state via `hex`.
    //
    // Fire only when `hex` is the COMMAND of a segment, NOT when it appears as an
    // argument — `cargo run --bin hex -- backup` builds and runs the hex binary,
    // it does not verify state via a `hex` subcommand. The old ` hex ` substring
    // match false-positived on exactly that (2026-06-12 shadow ledger: the only
    // `fail` prediction in 245 was this FP). Skip leading `VAR=val` env prefixes
    // so `HEX_DIR=x hex doctor` still fires (that IS the footgun).
    command_segments(cmd).iter().any(|seg| {
        let first_cmd = seg.split_whitespace().find(|t| !is_env_assignment(t));
        matches!(first_cmd, Some("hex"))
    })
}

fn rule_inverted_grep_v(cmd: &str) -> bool {
    cmd.contains("grep -q -v") || cmd.contains("grep -qv") || cmd.contains("grep -v -q")
}

fn rule_macos_wc_whitespace(cmd: &str) -> bool {
    // `wc -l | grep -q "^N$"` — macOS pads with whitespace; the regex misses.
    if !cmd.contains("wc -l") {
        return false;
    }
    // Look for `wc -l ... | ... grep -q "^<digit>`.
    let Some(wc_idx) = cmd.find("wc -l") else {
        return false;
    };
    // wc_idx is the byte index of the ASCII literal "wc -l" found by find ⇒ char boundary.
    #[allow(clippy::string_slice)]
    let after = &cmd[wc_idx..];
    let Some(pipe_idx) = after.find('|') else {
        return false;
    };
    // pipe_idx indexes the ASCII '|' found by find ⇒ pipe_idx+1 is a char boundary.
    #[allow(clippy::string_slice)]
    let tail = &after[pipe_idx + 1..];
    // Allow any whitespace between `|` and `grep`.
    let tail = tail.trim_start();
    if !tail.starts_with("grep ") {
        return false;
    }
    // Look for `-q ... "^<digit>` in the tail.
    tail.contains("-q") && (tail.contains("\"^") || tail.contains("'^"))
}

fn rule_python_c_indent(cmd: &str) -> bool {
    // `python3 -c "..."` where the literal body has a leading-indent line.
    let Some(idx) = cmd
        .find("python3 -c \"")
        .or_else(|| cmd.find("python -c \""))
    else {
        return false;
    };
    // idx is the byte index of an ASCII literal ("python3 -c \"" / "python -c \"") from find ⇒ char boundary.
    #[allow(clippy::string_slice)]
    let rest = &cmd[idx..];
    let body_start = match rest.find('"') {
        Some(p) => p + 1,
        None => return false,
    };
    // body_start is one past an ASCII '"' ⇒ a char boundary.
    #[allow(clippy::string_slice)]
    let body_end = match rest[body_start..].find('"') {
        Some(p) => p,
        None => return false,
    };
    // body_start is a boundary (above); body_start+body_end indexes the closing ASCII '"' found by find ⇒ char boundary.
    #[allow(clippy::string_slice)]
    let body = &rest[body_start..body_start + body_end];
    // The body uses literal `\n` between lines in shell-source form.
    let mut parts = body.split("\\n");
    let _ = parts.next(); // first line — leading indent is fine
    for ln in parts {
        if ln.starts_with(' ') || ln.starts_with('\t') {
            return true;
        }
    }
    false
}

fn rule_stderr_swallow(cmd: &str) -> bool {
    // `2>/dev/null` (and the spaced/&-variants) hides stderr — SO S6 violation
    // for a verify gate, which is exactly the place a loud failure matters.
    cmd.contains("2>/dev/null")
        || cmd.contains("2> /dev/null")
        || cmd.contains("&>/dev/null")
        || cmd.contains("> /dev/null 2>&1")
        || cmd.contains(">/dev/null 2>&1")
}

/// Static list of `(rule_id, predicate)` pairs. Order is the canonical
/// reporting order in `rules_fired`.
pub fn footgun_rules() -> Vec<(&'static str, fn(&str) -> bool)> {
    vec![
        ("path-127", rule_path_127 as fn(&str) -> bool),
        ("pipe-tail-exitcode", rule_pipe_tail_exitcode),
        ("deployed-binary", rule_deployed_binary),
        ("hex-from-worker", rule_hex_from_worker),
        ("inverted-grep-v", rule_inverted_grep_v),
        ("macos-wc-whitespace", rule_macos_wc_whitespace),
        ("python-c-indent", rule_python_c_indent),
        ("stderr-swallow", rule_stderr_swallow),
    ]
}

/// Run every footgun rule against `cmd`. Any hit ⇒ `Prediction::Fail`.
///
/// Implemented in terms of [`analyze_command_with`] with an empty `extra`
/// slice — behavior is byte-for-byte unchanged (P2 applier deliverable 2:
/// "existing `analyze_command()` behavior UNCHANGED").
pub fn analyze_command(cmd: &str) -> Verdict {
    analyze_command_with(&[], cmd)
}

/// One runtime-landed rule ready for matching: a stable `rule_id` (as
/// recorded in the rule registry) plus its already-compiled `Regex`.
/// Compiling up front means a malformed pattern in the registry is caught
/// once, loudly, by the caller building this list — never silently at
/// match time.
#[derive(Debug)]
pub struct CompiledRule {
    pub rule_id: String,
    pub regex: Regex,
}

impl CompiledRule {
    /// Compile a `(rule_id, pattern)` pair. Errs with the rule_id in the
    /// message so a malformed registry entry can be pinpointed (S6 — loud).
    pub fn compile(rule_id: &str, pattern: &str) -> Result<Self, String> {
        Regex::new(pattern)
            .map(|regex| CompiledRule {
                rule_id: rule_id.to_string(),
                regex,
            })
            .map_err(|e| {
                format!("rule '{rule_id}': pattern '{pattern}' does not compile as regex: {e}")
            })
    }
}

/// Run the builtin 8 footgun rules PLUS every rule in `extra` (runtime-landed
/// rules from the rule registry) against `cmd`. `extra` rules match via
/// `Regex::is_match` on the RAW command — same substrate the builtin rules
/// see. A landed rule's id lands in `rules_fired` exactly like a builtin id.
///
/// `analyze_command(cmd)` is `analyze_command_with(&[], cmd)` — passing an
/// empty slice reproduces the original, unchanged behavior exactly.
pub fn analyze_command_with(extra: &[CompiledRule], cmd: &str) -> Verdict {
    let mut fired = Vec::new();
    for (id, pred) in footgun_rules() {
        if pred(cmd) {
            fired.push(id.to_string());
        }
    }
    for rule in extra {
        if rule.regex.is_match(cmd) {
            fired.push(rule.rule_id.clone());
        }
    }
    let predicted = if fired.is_empty() {
        Prediction::Pass
    } else {
        Prediction::Fail
    };
    Verdict {
        predicted,
        rules_fired: fired,
        normalized: normalize_command(cmd),
        content_hash: content_hash(cmd),
    }
}

/// One-line shadow-mode summary for a batch of gate commands. The output is
/// *deliberately* free of per-gate advice — shadow mode logs predictions to
/// the ledger; advice stays silent until the disclosed bar clears.
pub fn shadow_summary(gates: &[String]) -> String {
    let n = gates.len();
    let m = gates
        .iter()
        .filter(|g| matches!(analyze_command(g).predicted, Prediction::Fail))
        .count();
    format!(
        "{} gates, {} flagged, shadow mode — predictions logged silently",
        n, m
    )
}

// ---------------------------------------------------------------------------
// BOI v2 TOML spec parsing — extract every verification command in document order.
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Deserialize)]
struct VerifEntry {
    /// Shell gate — the lintable kind. Exactly one of `command`/`intent` must
    /// be present (BOI v2 schema, enforced by BOI's own validator too).
    #[serde(default)]
    command: Option<String>,
    /// LLM-judged gate. Valid BOI v2, but carries no shell command — the
    /// linter skips it rather than rejecting the spec. The old `command:
    /// String` field made every intent-gated spec a parse error, which the
    /// PreToolUse hook turned into a blocked dispatch and which silently
    /// zeroed lint coverage for the whole spec (OBS-034).
    #[serde(default)]
    intent: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct ContractBlock {
    #[serde(default)]
    verifications: Vec<VerifEntry>,
}

#[derive(Debug, serde::Deserialize)]
struct TaskBlock {
    #[serde(default)]
    verifications: Vec<VerifEntry>,
}

#[derive(Debug, serde::Deserialize)]
struct SpecToml {
    #[serde(default)]
    contract: Option<ContractBlock>,
    #[serde(default)]
    tasks: Vec<TaskBlock>,
}

/// Errors surfaced by spec parsing. Loud per SO S6.
#[derive(Debug)]
pub enum LintError {
    Io(std::io::Error),
    Toml(String),
}

impl std::fmt::Display for LintError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LintError::Io(e) => write!(f, "lint-gates io error: {}", e),
            LintError::Toml(e) => write!(f, "lint-gates toml error: {}", e),
        }
    }
}

impl std::error::Error for LintError {}

impl From<std::io::Error> for LintError {
    fn from(e: std::io::Error) -> Self {
        LintError::Io(e)
    }
}

/// Extract every lintable verification command (contract + per-task) from a
/// BOI v2 TOML spec, in document order. `{name, intent}` gates are valid spec
/// content with nothing to lint — skipped, never an error. A verification with
/// BOTH or NEITHER of command/intent is a spec error named after the gate.
pub fn extract_gates_from_spec(toml_src: &str) -> Result<Vec<String>, LintError> {
    let spec: SpecToml = toml::from_str(toml_src).map_err(|e| LintError::Toml(e.to_string()))?;
    let mut gates = Vec::new();
    if let Some(c) = spec.contract {
        collect_command_gates(c.verifications, &mut gates)?;
    }
    for t in spec.tasks {
        collect_command_gates(t.verifications, &mut gates)?;
    }
    Ok(gates)
}

/// Push the command gates from one verification list, enforcing the
/// command-XOR-intent contract per entry.
fn collect_command_gates(entries: Vec<VerifEntry>, out: &mut Vec<String>) -> Result<(), LintError> {
    for v in entries {
        let label = v.name.as_deref().unwrap_or("<unnamed>").to_owned();
        match (v.command, v.intent) {
            (Some(cmd), None) => out.push(cmd),
            (None, Some(_)) => {} // intent gate: LLM-judged, nothing to lint
            (Some(_), Some(_)) => {
                return Err(LintError::Toml(format!(
                    "verification '{label}': has BOTH command and intent — exactly one is allowed"
                )));
            }
            (None, None) => {
                return Err(LintError::Toml(format!(
                    "verification '{label}': has NEITHER command nor intent — exactly one is required"
                )));
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests — per-rule positive + negative pair, hash + summary contract.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Per-rule pairs (8 rules × 2 cases each) -----------------------------

    #[test]
    fn lint_path_127_positive_and_negative() {
        assert!(rule_path_127("cargo build && test -f target/release/hex"));
        assert!(!rule_path_127(
            "export PATH=/opt/homebrew/bin:$PATH && cargo build"
        ));
    }

    #[test]
    fn lint_pipe_tail_exitcode_positive_and_negative() {
        assert!(rule_pipe_tail_exitcode(
            "cargo build 2>&1 | tail -5 | grep warn"
        ));
        assert!(!rule_pipe_tail_exitcode(
            "cargo build > /tmp/log && grep warn /tmp/log"
        ));
    }

    #[test]
    fn lint_deployed_binary_positive_and_negative() {
        assert!(rule_deployed_binary(
            "test -x .hex/bin/hex && .hex/bin/hex --version"
        ));
        assert!(!rule_deployed_binary("test -x target/release/hex"));
    }

    #[test]
    fn lint_hex_from_worker_positive_and_negative() {
        // command-position hex (start, or after a separator) → fires
        assert!(rule_hex_from_worker("hex doctor && echo ok"));
        assert!(rule_hex_from_worker("echo start && hex backup --check"));
        assert!(rule_hex_from_worker("ls; hex stats"));
        assert!(rule_hex_from_worker("echo $(hex recent)"));
        // env-prefixed hex still reads $HEX_DIR → must still fire (review fix)
        assert!(rule_hex_from_worker("HEX_DIR=/tmp/x hex doctor"));
        assert!(rule_hex_from_worker(
            "PATH=/opt/homebrew/bin:$PATH A=1 hex stats"
        ));
        // backgrounded hex after a lone & (review fix)
        assert!(rule_hex_from_worker("echo done & hex backup"));
        // not a hex subcommand → does not fire
        assert!(!rule_hex_from_worker("test -f system/harness/Cargo.toml"));
        // regression: `hex` as an argument, not a command (2026-06-12 shadow FP).
        // `cargo run --bin hex` builds+runs the binary; it is not state-via-hex.
        assert!(!rule_hex_from_worker("cargo run --bin hex -- doctor"));
        assert!(!rule_hex_from_worker(
            "export PATH=\"/opt/homebrew/bin:$PATH\" && cargo run -p hex-harness --bin hex -- \
             backup --help > /tmp/l 2>&1 && grep -qi 'backup' /tmp/l"
        ));
    }

    #[test]
    fn lint_inverted_grep_v_positive_and_negative() {
        assert!(rule_inverted_grep_v("grep -q -v ERROR build.log"));
        assert!(rule_inverted_grep_v("grep -qv ERROR build.log"));
        assert!(!rule_inverted_grep_v("! grep -q ERROR build.log"));
    }

    #[test]
    fn lint_macos_wc_whitespace_positive_and_negative() {
        assert!(rule_macos_wc_whitespace("ls | wc -l | grep -q \"^14$\""));
        let neg = "count=$(ls | wc -l | tr -d ' '); test \"$count\" = \"14\"";
        assert!(!rule_macos_wc_whitespace(neg));
    }

    #[test]
    fn lint_python_c_indent_positive_and_negative() {
        let pos = "python3 -c \"import x\\n    print(x)\"";
        assert!(rule_python_c_indent(pos));
        let neg = "python3 -c \"import x; print(x)\"";
        assert!(!rule_python_c_indent(neg));
    }

    #[test]
    fn lint_stderr_swallow_positive_and_negative() {
        assert!(rule_stderr_swallow("cargo test --quiet 2>/dev/null"));
        assert!(rule_stderr_swallow("cargo test >/dev/null 2>&1"));
        assert!(!rule_stderr_swallow("cargo test --quiet"));
    }

    // -- Aggregate analyze_command + summary ---------------------------------

    #[test]
    fn lint_analyze_aggregates_multiple_rules() {
        let v = analyze_command("cargo test 2>/dev/null && grep -q -v ERROR /tmp/log");
        assert!(matches!(v.predicted, Prediction::Fail));
        // Should fire at least the path-127, stderr-swallow, and inverted-grep-v rules.
        assert!(v.rules_fired.iter().any(|r| r == "stderr-swallow"));
        assert!(v.rules_fired.iter().any(|r| r == "inverted-grep-v"));
        assert!(v.rules_fired.iter().any(|r| r == "path-127"));
    }

    #[test]
    fn lint_extract_gates_from_minimal_spec() {
        let src = r#"
title = "x"
pipeline = "standard"

[contract]
verifications = [
  { command = "test -f foo" },
  { command = "test -f bar" },
]

[[tasks]]
ref = "a"
behavior = "do a"
verifications = [{ command = "cargo test 2>/dev/null" }]
"#;
        let gates = extract_gates_from_spec(src).unwrap();
        assert_eq!(gates.len(), 3);
        assert_eq!(gates[2], "cargo test 2>/dev/null");
    }

    // -- command-XOR-intent contract (the 2026-06-10 E0-smoke bug) ----------

    #[test]
    fn lint_extract_skips_intent_gates_in_mixed_spec() {
        // Mixed spec: intent gates are valid BOI v2 and must be skipped, not
        // rejected — the old `command: String` model exit-2'd the whole spec.
        let src = r#"
title = "x"
pipeline = "standard"

[contract]
verifications = [
  { name = "shell", command = "test -f foo" },
  { name = "judged", intent = "unit tests prove the watermark cannot regress" },
]

[[tasks]]
ref = "a"
behavior = "do a"
verifications = [
  { name = "judged-2", intent = "the docs match the implementation" },
  { name = "shell-2", command = "test -d src" },
]
"#;
        let gates = extract_gates_from_spec(src).unwrap();
        assert_eq!(
            gates,
            vec!["test -f foo".to_string(), "test -d src".to_string()]
        );
    }

    #[test]
    fn lint_extract_intent_only_spec_is_ok_with_zero_gates() {
        let src = r#"
title = "x"
pipeline = "standard"

[contract]
verifications = [{ name = "judged", intent = "a free-form claim" }]
"#;
        let gates = extract_gates_from_spec(src).unwrap();
        assert!(gates.is_empty());
    }

    #[test]
    fn lint_extract_rejects_both_command_and_intent_naming_the_gate() {
        let src = r#"
title = "x"
pipeline = "standard"

[contract]
verifications = [{ name = "greedy-gate", command = "true", intent = "also a claim" }]
"#;
        let err = extract_gates_from_spec(src).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("greedy-gate"),
            "error must name the gate: {msg}"
        );
        assert!(
            msg.contains("BOTH"),
            "error must say both were given: {msg}"
        );
    }

    #[test]
    fn lint_extract_rejects_neither_command_nor_intent_naming_the_gate() {
        let src = r#"
title = "x"
pipeline = "standard"

[[tasks]]
ref = "a"
behavior = "do a"
verifications = [{ name = "empty-gate" }]
"#;
        let err = extract_gates_from_spec(src).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("empty-gate"),
            "error must name the gate: {msg}"
        );
        assert!(
            msg.contains("NEITHER"),
            "error must say neither was given: {msg}"
        );
    }

    #[test]
    fn lint_extract_skips_intent_gates_but_keeps_commands() {
        // intent entries are valid BOI v2 (intent-XOR-command); they must not
        // fail parsing — that would zero lint coverage for the spec (OBS-034).
        let src = r#"
title = "x"
pipeline = "standard"

[contract]
verifications = [
  { name = "file", command = "test -f foo" },
  { name = "claim", intent = "the worker never prunes volumes" },
]

[[tasks]]
ref = "a"
behavior = "do a"
verifications = [
  { intent = "tests are meaningful" },
  { command = "cargo test 2>/dev/null" },
]
"#;
        let gates = extract_gates_from_spec(src).unwrap();
        assert_eq!(gates, vec!["test -f foo", "cargo test 2>/dev/null"]);
    }

    #[test]
    fn lint_extract_intent_only_spec_yields_no_gates() {
        let src = r#"
title = "x"

[contract]
verifications = [{ intent = "it works" }]

[[tasks]]
behavior = "do a"
verifications = [{ intent = "it really works" }]
"#;
        let gates = extract_gates_from_spec(src).unwrap();
        assert!(gates.is_empty());
    }

    // -- rule_registry merge (P2 applier deliverable 2) ----------------------

    #[test]
    fn rule_registry_analyze_command_with_empty_extra_matches_analyze_command() {
        // Byte-for-byte unchanged builtin behavior when extra is empty.
        let cmds = [
            "cargo test 2>/dev/null && grep -q -v ERROR /tmp/log",
            "test -f Cargo.toml",
            "ls | wc -l | grep -q \"^14$\"",
        ];
        for cmd in cmds {
            let a = analyze_command(cmd);
            let b = analyze_command_with(&[], cmd);
            assert_eq!(a.predicted, b.predicted);
            assert_eq!(a.rules_fired, b.rules_fired);
            assert_eq!(a.content_hash, b.content_hash);
        }
    }

    #[test]
    fn rule_registry_analyze_command_with_fires_landed_rule() {
        let rule = CompiledRule::compile("wc-l-grep-no-tr", r"wc -l[^|]*\|\s*grep").unwrap();
        let cmd = "results=$(ls -1 | wc -l | grep '^14$')";
        // Builtin 8 alone: does not fire (this exact footgun isn't builtin).
        let builtin_only = analyze_command(cmd);
        assert!(!builtin_only
            .rules_fired
            .contains(&"wc-l-grep-no-tr".to_string()));
        // Merged: the landed rule fires and is named in rules_fired.
        let merged = analyze_command_with(&[rule], cmd);
        assert!(matches!(merged.predicted, Prediction::Fail));
        assert!(merged.rules_fired.contains(&"wc-l-grep-no-tr".to_string()));
    }

    #[test]
    fn rule_registry_analyze_command_with_preserves_builtin_fires_alongside_landed() {
        let rule = CompiledRule::compile("landed-extra", "never-matches-anything-zzz").unwrap();
        let cmd = "cargo test 2>/dev/null";
        let merged = analyze_command_with(&[rule], cmd);
        assert!(merged.rules_fired.contains(&"stderr-swallow".to_string()));
        assert!(merged.rules_fired.contains(&"path-127".to_string()));
        assert!(!merged.rules_fired.contains(&"landed-extra".to_string()));
    }

    #[test]
    fn rule_registry_compiled_rule_invalid_regex_is_loud_error() {
        let err = CompiledRule::compile("bad-rule", "(unclosed[").unwrap_err();
        assert!(err.contains("bad-rule"));
        assert!(err.contains("does not compile as regex"));
    }

    #[test]
    fn lint_shadow_summary_single_line_no_advice() {
        let s = shadow_summary(&vec![
            "cargo test 2>/dev/null".to_string(),
            "test -f Cargo.toml".to_string(),
        ]);
        assert_eq!(s.lines().count(), 1);
        assert!(s.contains("shadow"));
        assert!(!s.to_lowercase().contains("advice — "));
        assert!(s.contains("1 flagged"));
    }
}
