//! `hex hook router` — CLI-level tests against the executable built from the
//! CURRENT sources (`env!("CARGO_BIN_EXE_hex")`, populated for integration
//! targets only — PR #6 review F1). Covers:
//!
//!  * the shared acceptance probe (`system/hooks/scripts/router-probe.sh`)
//!    in rust mode, which asserts exit 0 + empty stderr + the expected
//!    decision + one correctly-tagged ledger line per positive fixture, and a
//!    clean abstain per near miss (F10);
//!  * a byte-for-byte differential against the Python reference — stdout,
//!    stderr emptiness, ledger lines — over the seed fixtures and the
//!    finding-specific inputs (F3 numbers, F4 container-valued fields, F5
//!    non-string metadata, F13 Unicode word boundaries, F14 lone surrogate
//!    escapes, F7 JSON-colon secrets);
//!  * output-pipe failures (F8), workspace resolution via the CLI (F16), and
//!    the one-line diagnostic contract for a malformed rules file (F7).
//!
//! Every child gets an explicit environment (`env_clear`) so an ambient
//! `HEX_DIR`/`CLAUDE_PROJECT_DIR` on the developer machine can never leak
//! into a case.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_hex")
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn rules_path() -> PathBuf {
    repo_root().join("system/hooks/router-rules.json")
}

fn python_script() -> PathBuf {
    repo_root().join("system/hooks/scripts/pretooluse-router.py")
}

/// A throwaway hex workspace: `.hex/hooks/router-rules.json` (a copy of the
/// shared rules file, or `rules_json` when given) plus the `.hex/version.txt`
/// marker `resolve_hex_dir` looks for.
fn staged_workspace(rules_json: Option<&str>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let hooks = dir.path().join(".hex/hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    match rules_json {
        Some(raw) => std::fs::write(hooks.join("router-rules.json"), raw).unwrap(),
        None => {
            std::fs::copy(rules_path(), hooks.join("router-rules.json")).unwrap();
        }
    }
    std::fs::write(dir.path().join(".hex/version.txt"), "0.0.0-test\n").unwrap();
    dir
}

struct Run {
    code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    ledger: Vec<u8>,
}

fn ledger_bytes(dir: &Path) -> Vec<u8> {
    std::fs::read(dir.join("router-fires.jsonl")).unwrap_or_default()
}

/// Blank the wall-clock `ts` value so two ledgers can be compared.
fn strip_ts(bytes: &[u8]) -> Vec<u8> {
    let s = String::from_utf8_lossy(bytes);
    let re = regex::Regex::new(r#""ts": "[^"]*""#).unwrap();
    re.replace_all(&s, r#""ts": "TS""#)
        .into_owned()
        .into_bytes()
}

fn run_with(mut cmd: Command, payload: &[u8]) -> (Option<i32>, Vec<u8>, Vec<u8>) {
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    child.stdin.take().unwrap().write_all(payload).unwrap();
    let out = child.wait_with_output().expect("wait");
    (out.status.code(), out.stdout, out.stderr)
}

fn run_rust(payload: &[u8], env: &[(&str, &str)]) -> Run {
    let ledger = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(bin());
    cmd.args(["hook", "router"])
        .env_clear()
        .env("HEX_LEDGER_DIR", ledger.path());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let (code, stdout, stderr) = run_with(cmd, payload);
    Run {
        code,
        stdout,
        stderr,
        ledger: ledger_bytes(ledger.path()),
    }
}

fn run_python(payload: &[u8]) -> Run {
    let ledger = tempfile::tempdir().unwrap();
    let mut cmd = Command::new("python3");
    cmd.args(["-I", "-S"])
        .arg(python_script())
        .env("HEX_LEDGER_DIR", ledger.path());
    let (code, stdout, stderr) = run_with(cmd, payload);
    Run {
        code,
        stdout,
        stderr,
        ledger: ledger_bytes(ledger.path()),
    }
}

fn payload(tool_name: &str, tool_input: &str, cwd: &str) -> Vec<u8> {
    format!(
        r#"{{"session_id": "sess-1", "transcript_path": "/tmp/transcript.jsonl", "cwd": "{cwd}", "permission_mode": "default", "hook_event_name": "PreToolUse", "tool_name": "{tool_name}", "tool_input": {tool_input}}}"#
    )
    .into_bytes()
}

const DEFAULT_CWD: &str = "/tmp/hex-home/hex";

/// (label, raw payload, expected decision or "abstain", expected ledger
/// rule_id when firing)
fn differential_cases() -> Vec<(String, Vec<u8>, &'static str, &'static str)> {
    let mut v: Vec<(String, Vec<u8>, &str, &str)> = Vec::new();
    let mut add =
        |label: &str, tool: &str, input: &str, expected: &'static str, rule: &'static str| {
            v.push((
                label.to_string(),
                payload(tool, input, DEFAULT_CWD),
                expected,
                rule,
            ));
        };
    // --- seed fixtures (one positive + one near miss per rule, mirrors router-probe.sh) ---
    add(
        "gh-pr-merge +",
        "Bash",
        r#"{"command": "gh pr merge 123 --squash"}"#,
        "prior",
        "gh-pr-merge-ci-green",
    );
    add(
        "gh-pr-merge -",
        "Bash",
        r#"{"command": "gh pr checks 123 --watch"}"#,
        "abstain",
        "",
    );
    add(
        "vitest-spawnsync +",
        "Edit",
        r#"{"file_path": "src/components/foo.test.ts", "old_string": "x", "new_string": "const r = spawnSync('ls', []);"}"#,
        "prior",
        "vitest-spawnsync",
    );
    add(
        "vitest-spawnsync -",
        "Edit",
        r#"{"file_path": "src/components/foo.ts", "old_string": "x", "new_string": "const r = spawnSync('ls', []);"}"#,
        "abstain",
        "",
    );
    add(
        "stash +",
        "Bash",
        r#"{"command": "git stash"}"#,
        "deny",
        "git-stash-shared-checkout",
    );
    add(
        "stash -",
        "Bash",
        r#"{"command": "git stash list"}"#,
        "abstain",
        "",
    );
    add(
        "push-force +",
        "Bash",
        r#"{"command": "git push origin main --force"}"#,
        "ask",
        "git-push-force",
    );
    add(
        "push-force -",
        "Bash",
        r#"{"command": "git push origin main"}"#,
        "abstain",
        "",
    );
    add(
        "push-lease +",
        "Bash",
        r#"{"command": "git push origin main --force-with-lease"}"#,
        "ask",
        "git-push-force-with-lease",
    );
    add(
        "destructive +",
        "Bash",
        r#"{"command": "git reset --hard HEAD~1"}"#,
        "ask",
        "git-destructive-ask",
    );
    add(
        "destructive -",
        "Bash",
        r#"{"command": "git reset --soft HEAD~1"}"#,
        "abstain",
        "",
    );
    add(
        "boi-dispatch +",
        "Bash",
        r#"{"command": "boi dispatch spec.toml"}"#,
        "prior",
        "boi-dispatch-spec-priors",
    );
    add(
        "boi-dispatch -",
        "Bash",
        r#"{"command": "boi dashboard"}"#,
        "abstain",
        "",
    );
    add(
        "gh-fast-polling +",
        "Bash",
        r#"{"command": "while true; do gh pr checks 123; sleep 5; done"}"#,
        "ask",
        "gh-fast-polling",
    );
    add(
        "gh-fast-polling -",
        "Bash",
        r#"{"command": "while true; do gh pr checks 123; sleep 90; done"}"#,
        "abstain",
        "",
    );
    add(
        "pipe-tail +",
        "Bash",
        r#"{"command": "pytest -q | tail -20"}"#,
        "prior",
        "pipe-tail-masks-exit",
    );
    add(
        "pipe-tail -",
        "Bash",
        r#"{"command": "pytest -q"}"#,
        "abstain",
        "",
    );
    add(
        "flat-policy +",
        "Write",
        r#"{"file_path": "/tmp/hex-home/.hex-events/policies/my-policy.yaml", "content": "name: my-policy\ntrigger:\n  event: foo\naction:\n  type: shell\n  command: echo hi\n"}"#,
        "prior",
        "hex-events-flat-policy",
    );
    add(
        "flat-policy -",
        "Write",
        r#"{"file_path": "/tmp/hex-home/.hex-events/policies/my-policy.yaml", "content": "name: my-policy\nrules:\n  - name: r1\n    trigger:\n      event: foo\n"}"#,
        "abstain",
        "",
    );
    add(
        "index-full +",
        "Bash",
        r#"{"command": "hex memory index --full"}"#,
        "ask",
        "hex-memory-index-full",
    );
    add(
        "index-full -",
        "Bash",
        r#"{"command": "hex memory index"}"#,
        "abstain",
        "",
    );
    add(
        "scheduler +",
        "CronCreate",
        r#"{"schedule": "* * * * *", "command": "echo hi"}"#,
        "ask",
        "builtin-scheduler-tools",
    );
    add(
        "scheduler -",
        "CronList",
        r#"{"filter": "*"}"#,
        "abstain",
        "",
    );
    add(
        "heredoc-backtick +",
        "Bash",
        r#"{"command": "python3 - <<PYEOF\nprint('run `boi start` now')\nPYEOF"}"#,
        "prior",
        "backticks-in-unquoted-heredoc",
    );
    add(
        "heredoc-backtick -",
        "Bash",
        r#"{"command": "python3 - <<'PYEOF'\nprint('run `boi start` now')\nPYEOF"}"#,
        "abstain",
        "",
    );
    add(
        "websearch +",
        "WebSearch",
        r#"{"query": "test"}"#,
        "ask",
        "builtin-websearch",
    );
    add(
        "websearch -",
        "WebFetch",
        r#"{"url": "https://example.com"}"#,
        "abstain",
        "",
    );
    // --- F3: Python canonical JSON number formatting in the preview ---
    add(
        "F3 numbers",
        "WebSearch",
        r#"{"query": "x", "tiny": 1e-7, "big": 123456789012345678901234567890, "exp": 1E2, "negzero": -0, "half": 0.5, "huge": 1e400}"#,
        "ask",
        "builtin-websearch",
    );
    add(
        "F3 nan literal",
        "WebSearch",
        r#"{"query": NaN}"#,
        "ask",
        "builtin-websearch",
    );
    // --- F4: container-valued fields render with Python str() semantics ---
    add(
        "F4 list file_path",
        "Write",
        r#"{"file_path": ["foo.test.ts", null, true], "content": "spawnSync('ls')"}"#,
        "prior",
        "vitest-spawnsync",
    );
    add(
        "F4 dict new_string",
        "Edit",
        r#"{"file_path": "a.test.ts", "new_string": {"k": [1, 2.5, "spawnSync"], "z": null}}"#,
        "prior",
        "vitest-spawnsync",
    );
    add(
        "F4 list command",
        "Bash",
        r#"{"command": ["echo", "hi"]}"#,
        "abstain",
        "",
    );
    // --- F13: ASCII word-boundary semantics on Unicode input ---
    add(
        "F13 combining mark",
        "Bash",
        "{\"command\": \"git stash\u{301}\"}",
        "deny",
        "git-stash-shared-checkout",
    );
    add(
        "F13 e-acute",
        "Bash",
        "{\"command\": \"git stash\u{e9}\"}",
        "deny",
        "git-stash-shared-checkout",
    );
    add(
        "F13 unicode path exempts",
        "Bash",
        "{\"command\": \"cd /worktrees/\u{e9}t\u{e9} && git stash\"}",
        "abstain",
        "",
    );
    add(
        "F13 unicode path denies",
        "Bash",
        "{\"command\": \"cd /shared/\u{e9}t\u{e9} && git stash\"}",
        "deny",
        "git-stash-shared-checkout",
    );
    add(
        "F13 unicode mention",
        "Bash",
        "{\"command\": \"echo 'gít stash \u{1F600}'\"}",
        "abstain",
        "",
    );
    // --- F14: lone surrogate escapes survive canonicalization ---
    add(
        "F14 websearch surrogate",
        "WebSearch",
        r#"{"query": "\ud800 x \udfff"}"#,
        "ask",
        "builtin-websearch",
    );
    add(
        "F14 bash surrogate",
        "Bash",
        r#"{"command": "echo \ud83d; git stash"}"#,
        "deny",
        "git-stash-shared-checkout",
    );
    add(
        "F14 surrogate pair",
        "WebSearch",
        r#"{"query": "😀"}"#,
        "ask",
        "builtin-websearch",
    );
    // --- F7: JSON-colon credential in the preview is redacted identically ---
    add(
        "F7 json colon secret",
        "Bash",
        r#"{"command": "curl -d '{\"password\":\"hunter two\"}' x; git stash"}"#,
        "deny",
        "git-stash-shared-checkout",
    );
    add(
        "F7 assignment secret",
        "Bash",
        r#"{"command": "TOKEN=\"alpha bravo\" git push origin main --force"}"#,
        "ask",
        "git-push-force",
    );
    v
}

fn check_pair(label: &str, raw: &[u8], expected: &str, rule: &str) {
    let py = run_python(raw);
    let rs = run_rust(raw, &[("HEX_DIR", STAGED.path().to_str().unwrap())]);
    let raw_s = String::from_utf8_lossy(raw);
    assert_eq!(py.code, Some(0), "{label}: python exit ({raw_s})");
    assert_eq!(
        rs.code,
        Some(0),
        "{label}: rust exit ({raw_s}) stderr={}",
        String::from_utf8_lossy(&rs.stderr)
    );
    assert!(
        py.stderr.is_empty() && rs.stderr.is_empty(),
        "{label}: valid fixtures must produce no stderr; python={:?} rust={:?}",
        String::from_utf8_lossy(&py.stderr),
        String::from_utf8_lossy(&rs.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&rs.stdout),
        String::from_utf8_lossy(&py.stdout),
        "{label}: stdout bytes differ ({raw_s})"
    );
    assert_eq!(
        String::from_utf8_lossy(&strip_ts(&rs.ledger)),
        String::from_utf8_lossy(&strip_ts(&py.ledger)),
        "{label}: ledger bytes differ ({raw_s})"
    );
    // Independent expectation (F10): the decision and ledger shape, not just
    // equality between the two implementations.
    let out = String::from_utf8_lossy(&rs.stdout);
    if expected == "abstain" {
        assert!(out.is_empty(), "{label}: expected abstain, got {out}");
        assert!(rs.ledger.is_empty(), "{label}: abstain must not ledger");
    } else {
        let doc: serde_json::Value = serde_json::from_str(out.trim()).expect("stdout is JSON");
        let hso = &doc["hookSpecificOutput"];
        let got = match hso["permissionDecision"].as_str() {
            Some(d) => d.to_string(),
            None if hso["additionalContext"].is_string() => "prior".to_string(),
            None => "?".to_string(),
        };
        assert_eq!(got, expected, "{label}: decision");
        let lines: Vec<&str> = std::str::from_utf8(&rs.ledger).unwrap().lines().collect();
        assert_eq!(
            lines.len(),
            1,
            "{label}: exactly one ledger line, got {lines:?}"
        );
        // The ledger is `json.dumps(entry, sort_keys=True)` output; a line
        // may carry a lone-surrogate escape (F14) that strict JSON parsers
        // reject, so the shape is checked on the rendered text.
        let line = lines[0];
        assert!(
            line.starts_with('{') && line.ends_with('}'),
            "{label}: {line}"
        );
        assert!(
            line.contains(&format!(r#""rule_id": "{rule}""#)),
            "{label}: ledger rule_id in {line}"
        );
        assert!(
            line.contains(&format!(r#""decision": "{expected}""#)),
            "{label}: ledger decision in {line}"
        );
        for key in [
            "\"cwd\": ",
            "\"match\": ",
            "\"preview\": ",
            "\"session_id\": ",
            "\"tool\": ",
            "\"ts\": ",
        ] {
            assert!(
                line.contains(key),
                "{label}: ledger key {key} missing in {line}"
            );
        }
    }
}

static STAGED: std::sync::LazyLock<tempfile::TempDir> =
    std::sync::LazyLock::new(|| staged_workspace(None));

#[test]
fn probe_passes_in_rust_mode_against_the_current_build() {
    let probe = repo_root().join("system/hooks/scripts/router-probe.sh");
    let out = Command::new("bash")
        .arg(&probe)
        .env_remove("HEX_DIR")
        .env_remove("CLAUDE_PROJECT_DIR")
        .env("ROUTER_IMPL", "rust")
        .env("HEX_ROUTER_BIN", bin())
        .output()
        .expect("run router-probe.sh");
    assert!(
        out.status.success(),
        "router-probe.sh (rust mode, current build) failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("ledger-count expected="),
        "probe output lacks the ledger tally:\n{stdout}"
    );
    assert!(
        !stdout.contains(" FAIL"),
        "probe reported a FAIL line:\n{stdout}"
    );
}

#[test]
fn seed_and_finding_fixtures_match_the_python_reference_byte_for_byte() {
    for (label, raw, expected, rule) in differential_cases() {
        check_pair(&label, &raw, expected, rule);
    }
}

/// F5: non-string metadata. Falsy values are ledgered raw (`"cwd": 0`,
/// `"session_id": null`); a truthy non-string makes the reference fail open
/// at `redact()` — no stdout, one stderr line, nothing ledgered. Both
/// behaviours are compared against the live Python script.
#[test]
fn non_string_metadata_matches_the_python_reference() {
    let falsy = br#"{"session_id": null, "cwd": 0, "tool_name": "CronCreate", "tool_input": {"schedule": "* * * * *", "command": "echo hi"}}"#;
    let py = run_python(falsy);
    let rs = run_rust(falsy, &[("HEX_DIR", STAGED.path().to_str().unwrap())]);
    assert_eq!(
        String::from_utf8_lossy(&rs.stdout),
        String::from_utf8_lossy(&py.stdout)
    );
    assert_eq!(
        String::from_utf8_lossy(&strip_ts(&rs.ledger)),
        String::from_utf8_lossy(&strip_ts(&py.ledger))
    );
    let line = String::from_utf8_lossy(&rs.ledger);
    assert!(
        line.contains(r#""cwd": 0"#),
        "raw falsy cwd must be preserved: {line}"
    );
    assert!(
        line.contains(r#""session_id": null"#),
        "raw null session_id must be preserved: {line}"
    );

    for truthy in [
        br#"{"session_id": "s", "cwd": 123, "tool_name": "CronCreate", "tool_input": {"schedule": "* * * * *", "command": "echo hi"}}"#.as_slice(),
        br#"{"session_id": ["x"], "cwd": "/tmp", "tool_name": "CronCreate", "tool_input": {"schedule": "* * * * *", "command": "echo hi"}}"#.as_slice(),
        br#"{"session_id": "s", "cwd": 123, "tool_name": "Bash", "tool_input": {"command": "git stash"}}"#.as_slice(),
        br#"{"session_id": "s", "cwd": "/tmp", "tool_name": 42, "tool_input": {"command": "git stash"}}"#.as_slice(),
        br#"{"session_id": "s", "cwd": "/tmp", "tool_name": "Bash", "tool_input": ["git", "stash"]}"#.as_slice(),
    ] {
        let py = run_python(truthy);
        let rs = run_rust(truthy, &[("HEX_DIR", STAGED.path().to_str().unwrap())]);
        let raw_s = String::from_utf8_lossy(truthy);
        assert_eq!(py.code, Some(0), "{raw_s}");
        assert_eq!(rs.code, Some(0), "{raw_s}");
        assert!(py.stdout.is_empty() && rs.stdout.is_empty(), "{raw_s}: both must fail open (no stdout)");
        assert!(!py.stderr.is_empty() && !rs.stderr.is_empty(), "{raw_s}: both must report on stderr");
        assert_eq!(String::from_utf8_lossy(&rs.stderr).lines().count(), 1, "{raw_s}: exactly one stderr line");
        assert!(py.ledger.is_empty() && rs.ledger.is_empty(), "{raw_s}: nothing ledgered");
    }
}

/// F8: a consumer that closed its end of stdout (or stderr) must not turn
/// into a panic / non-zero exit. The ledger is still appended.
#[test]
fn closed_output_pipes_still_exit_zero() {
    let ledger = tempfile::tempdir().unwrap();
    let mut child = Command::new(bin())
        .args(["hook", "router"])
        .env_clear()
        .env("HEX_DIR", STAGED.path())
        .env("HEX_LEDGER_DIR", ledger.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take()); // close our read end before the child writes
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&payload("Bash", r#"{"command": "git stash"}"#, DEFAULT_CWD))
        .unwrap();
    let status = child.wait().unwrap();
    assert_eq!(
        status.code(),
        Some(0),
        "closed stdout must not change the exit status"
    );
    assert_eq!(
        std::str::from_utf8(&ledger_bytes(ledger.path()))
            .unwrap()
            .lines()
            .count(),
        1,
        "the fire is still ledgered when stdout is gone"
    );

    // Closed stderr on the fail-open path (malformed stdin).
    let mut child = Command::new(bin())
        .args(["hook", "router"])
        .env_clear()
        .env("HEX_DIR", STAGED.path())
        .env("HEX_LEDGER_DIR", ledger.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stderr.take());
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"{not valid json")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "closed stderr must not change the exit status"
    );
    assert!(out.stdout.is_empty());
}

/// F16: workspace resolution through the CLI. No-op paths: exit 0, empty
/// stdout, exactly one stderr line naming the reason, no ledger write.
#[test]
fn workspace_resolution_via_the_cli() {
    let firing = payload("Bash", r#"{"command": "git stash"}"#, DEFAULT_CWD);
    let foreign = tempfile::tempdir().unwrap(); // a checkout with no .hex/version.txt

    // (a) both variables unset
    let rs = run_rust(&firing, &[]);
    assert_eq!(rs.code, Some(0));
    assert!(rs.stdout.is_empty(), "no workspace → no decision");
    let err = String::from_utf8_lossy(&rs.stderr);
    assert_eq!(
        err.lines().count(),
        1,
        "exactly one diagnostic line: {err:?}"
    );
    assert!(err.contains("both unset"), "{err}");
    assert!(rs.ledger.is_empty(), "no-op path must not ledger");

    // (b) foreign CLAUDE_PROJECT_DIR (not a hex workspace)
    let rs = run_rust(
        &firing,
        &[("CLAUDE_PROJECT_DIR", foreign.path().to_str().unwrap())],
    );
    assert_eq!(rs.code, Some(0));
    assert!(rs.stdout.is_empty());
    let err = String::from_utf8_lossy(&rs.stderr);
    assert_eq!(err.lines().count(), 1, "{err:?}");
    assert!(err.contains("not a hex workspace"), "{err}");
    assert!(rs.ledger.is_empty());

    // (c) marked workspace via CLAUDE_PROJECT_DIR
    let rs = run_rust(
        &firing,
        &[("CLAUDE_PROJECT_DIR", STAGED.path().to_str().unwrap())],
    );
    assert_eq!(rs.code, Some(0));
    assert!(
        rs.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&rs.stderr)
    );
    assert!(String::from_utf8_lossy(&rs.stdout).contains(r#""permissionDecision": "deny""#));
    assert_eq!(std::str::from_utf8(&rs.ledger).unwrap().lines().count(), 1);

    // (d) explicit HEX_DIR wins over a foreign CLAUDE_PROJECT_DIR
    let rs = run_rust(
        &firing,
        &[
            ("HEX_DIR", STAGED.path().to_str().unwrap()),
            ("CLAUDE_PROJECT_DIR", foreign.path().to_str().unwrap()),
        ],
    );
    assert_eq!(rs.code, Some(0));
    assert!(rs.stderr.is_empty());
    assert!(String::from_utf8_lossy(&rs.stdout).contains(r#""permissionDecision": "deny""#));
    assert_eq!(std::str::from_utf8(&rs.ledger).unwrap().lines().count(), 1);
}

/// F7: a malformed rule (unbalanced group) yields exit 0, empty stdout and
/// EXACTLY one diagnostic line even though the regex engine's own error
/// text spans several lines.
#[test]
fn malformed_rule_yields_exactly_one_diagnostic_line() {
    let bad = staged_workspace(Some(
        r#"[{"id": "broken", "tool": "^Bash$", "match": "(unclosed", "decision": "ask", "message": "m"}]"#,
    ));
    let rs = run_rust(
        &payload("Bash", r#"{"command": "echo hi"}"#, DEFAULT_CWD),
        &[("HEX_DIR", bad.path().to_str().unwrap())],
    );
    assert_eq!(rs.code, Some(0));
    assert!(rs.stdout.is_empty());
    let err = String::from_utf8_lossy(&rs.stderr);
    assert_eq!(
        err.lines().count(),
        1,
        "diagnostic must be one line: {err:?}"
    );
    assert!(err.starts_with("[router] error: "), "{err}");
    assert!(err.contains("broken"), "the rule id must be named: {err}");
    assert!(rs.ledger.is_empty());

    // Same contract when the rules file is not JSON at all.
    let not_json = staged_workspace(Some("{not json"));
    let rs = run_rust(
        &payload("Bash", r#"{"command": "echo hi"}"#, DEFAULT_CWD),
        &[("HEX_DIR", not_json.path().to_str().unwrap())],
    );
    assert_eq!(rs.code, Some(0));
    assert!(rs.stdout.is_empty());
    assert_eq!(String::from_utf8_lossy(&rs.stderr).lines().count(), 1);
}

/// Default ledger location and permissions match the reference: with
/// `HEX_LEDGER_DIR` unset both writers use `$HOME/.hex/ledger/`, create the
/// directory 0700 and the file 0600, and produce the same bytes.
#[cfg(unix)]
#[test]
fn default_ledger_location_and_permissions_match_the_python_reference() {
    use std::os::unix::fs::PermissionsExt;
    let raw = payload("Bash", r#"{"command": "git stash"}"#, DEFAULT_CWD);
    let home_py = tempfile::tempdir().unwrap();
    let home_rs = tempfile::tempdir().unwrap();

    let mut cmd = Command::new("python3");
    cmd.args(["-I", "-S"])
        .arg(python_script())
        .env_remove("HEX_LEDGER_DIR")
        .env("HOME", home_py.path());
    let (code_py, out_py, _) = run_with(cmd, &raw);
    let mut cmd = Command::new(bin());
    cmd.args(["hook", "router"])
        .env_clear()
        .env("HEX_DIR", STAGED.path())
        .env("HOME", home_rs.path());
    let (code_rs, out_rs, err_rs) = run_with(cmd, &raw);
    assert_eq!(code_py, Some(0));
    assert_eq!(code_rs, Some(0), "{}", String::from_utf8_lossy(&err_rs));
    assert_eq!(
        String::from_utf8_lossy(&out_rs),
        String::from_utf8_lossy(&out_py)
    );

    for home in [home_py.path(), home_rs.path()] {
        let dir = home.join(".hex/ledger");
        let file = dir.join("router-fires.jsonl");
        assert!(
            file.exists(),
            "ledger must land in $HOME/.hex/ledger ({})",
            home.display()
        );
        assert_eq!(
            std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let py_led =
        strip_ts(&std::fs::read(home_py.path().join(".hex/ledger/router-fires.jsonl")).unwrap());
    let rs_led =
        strip_ts(&std::fs::read(home_rs.path().join(".hex/ledger/router-fires.jsonl")).unwrap());
    assert_eq!(
        String::from_utf8_lossy(&rs_led),
        String::from_utf8_lossy(&py_led)
    );
}

/// Round 2, F7/F8: the workspace resolver's diagnostics go through the same
/// one-line, best-effort writer as the router's own — a foreign
/// `CLAUDE_PROJECT_DIR` containing a newline still yields exactly one line,
/// and a closed stderr on the no-op path still exits 0.
#[test]
fn workspace_resolver_diagnostics_are_one_line_and_best_effort() {
    let base = tempfile::tempdir().unwrap();
    let weird = base.path().join("with\nnewline");
    std::fs::create_dir_all(&weird).unwrap();
    let firing = payload("Bash", r#"{"command": "git stash"}"#, DEFAULT_CWD);
    let rs = run_rust(&firing, &[("CLAUDE_PROJECT_DIR", weird.to_str().unwrap())]);
    assert_eq!(rs.code, Some(0));
    assert!(rs.stdout.is_empty());
    let err = String::from_utf8_lossy(&rs.stderr);
    assert_eq!(
        err.lines().count(),
        1,
        "F7: one diagnostic line, got {err:?}"
    );
    assert!(
        err.contains("not a hex workspace") && err.contains("\\n"),
        "{err:?}"
    );
    assert!(rs.ledger.is_empty());

    // Closed stderr, both variables unset (the resolver's own no-op path).
    let ledger = tempfile::tempdir().unwrap();
    let mut child = Command::new(bin())
        .args(["hook", "router"])
        .env_clear()
        .env("HEX_LEDGER_DIR", ledger.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stderr.take());
    child.stdin.take().unwrap().write_all(&firing).unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "F8: closed stderr on the resolver path must exit 0"
    );
    assert!(out.stdout.is_empty());
    assert!(ledger_bytes(ledger.path()).is_empty());
}

/// Round 2, F14 + F18 through the CLI: a lone-surrogate heredoc delimiter
/// terminates like the reference (both deny, same bytes), and an
/// unterminated heredoc whose delimiter is `cd` neither panics nor diverges.
#[test]
fn round_two_surrogate_delimiter_and_synthetic_cd_terminator_match_the_reference() {
    let cases: [(&str, &[u8], &str); 3] = [
        (
            "F14 surrogate delimiter",
            br#"{"tool_name":"Bash","cwd":"/shared","tool_input":{"command":"cat <<'\ud800'\nbody\n\ud800\ngit stash"}}"#,
            "deny",
        ),
        (
            "F18 synthetic cd terminator",
            br#"{"tool_name":"Bash","tool_input":{"command":"cat <<cd\nbody"}}"#,
            "abstain",
        ),
        (
            "F18 synthetic cd then a real invocation",
            br#"{"tool_name":"Bash","tool_input":{"command":"cat <<cd\nbody\ngit stash"}}"#,
            "abstain",
        ),
    ];
    for (label, raw, expected) in cases {
        let py = run_python(raw);
        let rs = run_rust(raw, &[("HEX_DIR", STAGED.path().to_str().unwrap())]);
        assert_eq!(
            rs.code,
            Some(0),
            "{label}: exit; stderr={}",
            String::from_utf8_lossy(&rs.stderr)
        );
        assert!(
            rs.stderr.is_empty(),
            "{label}: {}",
            String::from_utf8_lossy(&rs.stderr)
        );
        assert_eq!(py.code, Some(0), "{label}: python exit");
        assert_eq!(
            String::from_utf8_lossy(&rs.stdout),
            String::from_utf8_lossy(&py.stdout),
            "{label}: stdout"
        );
        assert_eq!(
            String::from_utf8_lossy(&strip_ts(&rs.ledger)),
            String::from_utf8_lossy(&strip_ts(&py.ledger)),
            "{label}: ledger"
        );
        let out = String::from_utf8_lossy(&rs.stdout);
        if expected == "abstain" {
            assert!(out.is_empty(), "{label}: expected abstain, got {out}");
        } else {
            assert!(
                out.contains(&format!(r#""permissionDecision": "{expected}""#)),
                "{label}: {out}"
            );
        }
    }
}
