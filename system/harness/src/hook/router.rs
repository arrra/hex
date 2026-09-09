//! Claude Code `PreToolUse` router hook — Rust port of the reference
//! implementation `system/hooks/scripts/pretooluse-router.py`.
//!
//! Reads a single PreToolUse JSON payload from stdin, evaluates it against
//! the rules in `.hex/hooks/router-rules.json`, and emits at most one
//! combined decision on stdout: any "deny" beats any "ask" beats the first
//! "prior" (in router-rules.json array order). Every rule that matches
//! ("fires") is appended to the ledger regardless of whether it won the
//! combined decision; an abstain (no rule matches) writes nothing.
//!
//! Fail-open by design: this hook must never block a tool because of a bug
//! in the router itself. Any internal error (malformed stdin, missing rules
//! file, bad pattern, etc.) is swallowed into exactly one stderr line
//! (`[router] error: ...`) and the process still exits 0 with empty stdout.
//!
//! Implementation notes: `decide` is pure (no IO) so it can be exercised
//! directly by the unit tests below; `run` is the thin IO wrapper that reads
//! stdin, resolves `router-rules.json` via `super::resolve_hex_dir`, appends
//! the ledger, and prints stdout — mirroring the Python reference's
//! `main()`/`evaluate()` split exactly (see the module-level differential
//! test at the bottom of this file, which runs both implementations side by
//! side on every probe fixture).

use regex::{Regex, RegexBuilder};
use serde::Deserialize;
use serde_json::Value;
use std::io::Read;
use std::path::PathBuf;

/// One rule as it appears in `router-rules.json`. `match` is a Rust keyword,
/// hence the field rename.
#[derive(Debug, Clone, Deserialize)]
pub struct Rule {
    pub id: String,
    pub tool: String,
    #[serde(rename = "match")]
    pub match_pattern: String,
    #[serde(default)]
    pub unless_cwd: Option<String>,
    #[serde(default)]
    pub unless_match: Option<String>,
    pub decision: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub source: Option<String>,
}

/// A single rule that matched the canonical text ("fired"), independent of
/// whether it won the combined decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fire {
    pub rule_id: String,
    /// "deny" | "ask" | "prior"
    pub decision: String,
    pub message: String,
    /// The matched substring, truncated to 200 chars (ledger `match` field).
    pub matched: String,
}

/// Ledger context shared by every fire produced from one invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerContext {
    pub session_id: String,
    pub tool: String,
    pub cwd: String,
    /// First 300 chars of the canonical text (ledger `preview` field).
    pub preview: String,
}

/// The pure result of evaluating one PreToolUse payload against a rule set.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// No rule matched. Nothing is printed, nothing is ledgered.
    Abstain,
    /// At least one rule matched. `fires` holds every match (ledger writes
    /// one line per entry); `winner` is the single fire that decides stdout
    /// (deny beats ask beats first-prior-in-file-order).
    Fired {
        fires: Vec<Fire>,
        winner: Fire,
        ctx: LedgerContext,
    },
    /// Internal error (malformed stdin, bad regex, unreadable rules, etc).
    /// The caller (`run`) must fail open: one stderr line, exit 0, no
    /// stdout, nothing ledgered.
    Error(String),
}

/// Match-string truncation, ledger `match` field (Python `MATCH_TRUNCATE`).
const MATCH_TRUNCATE: usize = 200;
/// Canonical-text truncation, ledger `preview` field (Python's `text[:300]`).
const PREVIEW_TRUNCATE: usize = 300;
/// Tool names whose canonical text is `file_path` + edited content, joined
/// by `\n` (Python `TEXT_TOOLS`).
const TEXT_TOOLS: [&str; 4] = ["Edit", "Write", "MultiEdit", "NotebookEdit"];

/// Truncate to at most `max_chars` Unicode scalar values (never split a
/// multi-byte char mid-sequence — `[lints.clippy] string_slice = "warn"`
/// forbids raw byte slicing here anyway).
fn truncate_chars(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

/// Python's `str(value)` for the handful of JSON scalar shapes the rules
/// actually carry. Missing keys are handled by the caller (default `""`,
/// matching `tool_input.get(key, "")`), not here.
fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Number(n) => n.to_string(),
        Value::Array(_) | Value::Object(_) => v.to_string(),
    }
}

fn get_str_field(map: &serde_json::Map<String, Value>, key: &str) -> String {
    map.get(key).map(py_str).unwrap_or_default()
}

/// Python's `payload.get("tool_input") or {}` — falsy JSON values (missing
/// key, `null`, `false`, `0`/`0.0`, `""`, `[]`, `{}`) all default to `{}`;
/// any other value (including a truthy non-object, e.g. a non-empty array
/// or string) survives as-is. It's only an error later — inside
/// `canonical_text` — if the surviving value doesn't behave like a dict for
/// the tool actually in play. Mirroring Python's own truthiness rules here
/// (rather than coercing eagerly) is what lets a wrongly-typed `tool_input`
/// produce the same fail-open `Outcome::Error` Python raises via
/// `AttributeError` on `.get()`, instead of being silently swallowed into an
/// empty object the way a naive `.as_object().unwrap_or_default()` would.
fn py_or_default_object(v: Option<&Value>) -> Value {
    fn is_truthy(v: &Value) -> bool {
        match v {
            Value::Null => false,
            Value::Bool(b) => *b,
            Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
            Value::String(s) => !s.is_empty(),
            Value::Array(a) => !a.is_empty(),
            Value::Object(m) => !m.is_empty(),
        }
    }
    match v {
        Some(val) if is_truthy(val) => val.clone(),
        _ => Value::Object(serde_json::Map::new()),
    }
}

/// Canonical text a rule's `match` regex is applied to. Mirrors
/// `canonical_text()` in the Python reference exactly, including its
/// failure mode: the Bash/`TEXT_TOOLS` branches call `.get()` on
/// `tool_input`, which Python raises `AttributeError` for whenever
/// `tool_input` isn't actually a dict (e.g. a rule payload where
/// `tool_input` is a JSON array or string) — mirrored here as `Err` so the
/// caller can fail open exactly like Python does, rather than silently
/// treating a non-object `tool_input` as empty.
fn canonical_text(tool_name: &str, tool_input: &Value) -> Result<String, String> {
    if tool_name == "Bash" {
        let map = tool_input.as_object().ok_or_else(|| {
            "tool_input must be an object for tool_name=Bash (Python: AttributeError in \
             tool_input.get(\"command\"))"
                .to_string()
        })?;
        return Ok(get_str_field(map, "command"));
    }
    if TEXT_TOOLS.contains(&tool_name) {
        let map = tool_input.as_object().ok_or_else(|| {
            format!(
                "tool_input must be an object for tool_name={tool_name} (Python: \
                 AttributeError in tool_input.get(\"file_path\"))"
            )
        })?;
        let mut parts = vec![get_str_field(map, "file_path")];
        if map.contains_key("new_string") {
            parts.push(get_str_field(map, "new_string"));
        }
        if map.contains_key("content") {
            parts.push(get_str_field(map, "content"));
        }
        if let Some(Value::Array(edits)) = map.get("edits") {
            for edit in edits {
                if let Value::Object(obj) = edit {
                    parts.push(get_str_field(obj, "new_string"));
                }
            }
        }
        return Ok(parts.join("\n"));
    }
    // Compact, sorted-key, ASCII-only JSON — matches Python's
    // `json.dumps(tool_input, sort_keys=True, separators=(",", ":"))` for
    // an arbitrary JSON value (not just a dict), byte-for-byte. Key sorting
    // and compactness come for free from `serde_json`'s `Map` (BTreeMap-
    // backed — no `preserve_order` feature in this crate graph, verified
    // against Cargo.lock) and its default (no-whitespace) `Display`. The
    // one thing plain `serde_json::to_string` does NOT reproduce is
    // Python's default `ensure_ascii=True`, which escapes every non-ASCII
    // character to `\uXXXX` (review finding G2) — `serde_json` only escapes
    // control characters, quotes and backslashes, leaving other bytes (e.g.
    // multi-byte UTF-8) raw. `python_ascii_json` below reproduces Python's
    // escaping exactly so the ledger `match`/`preview` fields — and any
    // future rule matched against this branch's canonical text — see
    // identical bytes in both implementations.
    let mut out = String::new();
    python_ascii_json(tool_input, &mut out);
    Ok(out)
}

/// Escapes `s` into `out` exactly like CPython's `json.encoder` does for
/// `ensure_ascii=True` (the `json.dumps` default): standard JSON escapes for
/// `"`, `\`, and the named control chars, `\u00XX` for every other char
/// below `0x20`, and `\uXXXX` (or a UTF-16 surrogate pair above the BMP) for
/// every character above `0x7e` — i.e. anything outside printable ASCII.
fn escape_python_ascii(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                let cp = c as u32;
                if cp > 0xFFFF {
                    // Outside the BMP: encode as a UTF-16 surrogate pair,
                    // same as Python's ensure_ascii encoder.
                    let v = cp - 0x10000;
                    let hi = 0xD800 + (v >> 10);
                    let lo = 0xDC00 + (v & 0x3FF);
                    out.push_str(&format!("\\u{hi:04x}\\u{lo:04x}"));
                } else {
                    out.push_str(&format!("\\u{cp:04x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Python's `json.dumps(value, sort_keys=True, separators=(",", ":"))` for
/// an arbitrary JSON value — walks arrays/objects recursively, escaping
/// every string (including object keys) via `escape_python_ascii`.
fn python_ascii_json(v: &Value, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => escape_python_ascii(s, out),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                python_ascii_json(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            // `map.keys()` is already sorted (BTreeMap-backed, see above),
            // but sort explicitly so this function's correctness never
            // depends on that crate-graph detail holding in the future.
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                escape_python_ascii(k, out);
                out.push(':');
                python_ascii_json(&map[k], out);
            }
            out.push('}');
        }
    }
}

/// One rule with its patterns compiled. `regex` has no lookaround (unlike
/// Python's `re`) — the rules file was rewritten to avoid it via
/// `unless_match`, so plain compilation is expected to succeed for every
/// rule in the real `router-rules.json`.
struct CompiledRule {
    id: String,
    tool_re: Regex,
    match_re: Regex,
    unless_cwd_re: Option<Regex>,
    unless_match_re: Option<Regex>,
    decision: String,
    message: String,
}

/// Python compiles every pattern with `re.MULTILINE` (`load_rules()`) so
/// `^`/`$` match at line boundaries, not just the start/end of the whole
/// canonical text — several rules rely on this for command-position
/// anchoring inside multi-line Bash scripts. Mirror it with `multi_line`.
fn build_regex(pattern: &str) -> Result<Regex, String> {
    RegexBuilder::new(pattern)
        .multi_line(true)
        .build()
        .map_err(|e| e.to_string())
}

fn compile_rules(rules: &[Rule]) -> Result<Vec<CompiledRule>, String> {
    rules
        .iter()
        .map(|r| {
            Ok(CompiledRule {
                id: r.id.clone(),
                tool_re: build_regex(&r.tool)
                    .map_err(|e| format!("rule {}: bad `tool` pattern: {e}", r.id))?,
                match_re: build_regex(&r.match_pattern)
                    .map_err(|e| format!("rule {}: bad `match` pattern: {e}", r.id))?,
                unless_cwd_re: r
                    .unless_cwd
                    .as_deref()
                    .map(build_regex)
                    .transpose()
                    .map_err(|e| format!("rule {}: bad `unless_cwd` pattern: {e}", r.id))?,
                unless_match_re: r
                    .unless_match
                    .as_deref()
                    .map(build_regex)
                    .transpose()
                    .map_err(|e| format!("rule {}: bad `unless_match` pattern: {e}", r.id))?,
                decision: r.decision.clone(),
                message: r.message.clone(),
            })
        })
        .collect()
}

/// Command-boundary characters used to scope `unless_match` when a rule's
/// `match` regex has more than one occurrence in one canonical text (e.g. a
/// chained shell invocation combining a safe subcommand with a dangerous
/// bare one, separated by `;`). Mirrors the anchor tokens already used by
/// rule `match` patterns for command-position detection (`[;&|({]` etc,
/// plus newline). Port of `_window_bounds` in the Python reference.
const SEPARATOR_CHARS: [char; 8] = [';', '&', '|', '(', ')', '{', '}', '\n'];

/// The substring boundaries of the "same command" as the occurrence at
/// `text[start..end]`: from just after the nearest separator at or before
/// `start` to just before the nearest separator at or after `end` (or the
/// string edges). Comparing with `<=`/`>=` (not strict `<`/`>`) matters: a
/// rule's `match` regex often captures its own leading separator as part of
/// the anchor (e.g. a `;`-prefixed subcommand), so the separator can sit
/// exactly at `start` and must still bound the window rather than being
/// skipped over.
fn window_bounds(text: &str, start: usize, end: usize) -> (usize, usize) {
    let sep_positions: Vec<usize> = text
        .char_indices()
        .filter(|(_, c)| SEPARATOR_CHARS.contains(c))
        .map(|(i, _)| i)
        .collect();
    let window_start = sep_positions
        .iter()
        .filter(|&&p| p <= start)
        .max()
        .map(|&p| p + 1)
        .unwrap_or(0);
    let window_end = sep_positions
        .iter()
        .filter(|&&p| p >= end)
        .min()
        .copied()
        .unwrap_or(text.len());
    (window_start, window_end)
}

/// Pure decision function — no IO. Mirrors `evaluate()` in the Python
/// reference exactly: same canonical-text rules, same combine order, same
/// truncation.
pub fn decide(rules: &[Rule], raw_stdin: &str) -> Outcome {
    let payload: Value = match serde_json::from_str(raw_stdin) {
        Ok(v) => v,
        Err(e) => return Outcome::Error(format!("invalid JSON stdin: {e}")),
    };
    let payload_obj = match payload.as_object() {
        Some(o) => o,
        None => return Outcome::Error("stdin JSON payload must be an object".to_string()),
    };

    // Python: `tool_name = payload.get("tool_name", "")` — a missing key
    // defaults to `""` (a valid string, never errors). A *present but
    // non-string* value (e.g. a JSON number) doesn't crash immediately
    // either — but every single rule iteration unconditionally calls
    // `rule["tool_re"].search(tool_name)`, and `re.search` raises
    // `TypeError` the instant a non-string/bytes value reaches it. As long
    // as there is at least one rule (always true for the real
    // router-rules.json), that crash is unavoidable — and it can never
    // race with a `canonical_text` error, because a non-string tool_name
    // can never equal `"Bash"` or land `in TEXT_TOOLS`, so `canonical_text`
    // itself never sees it error. We mirror the net effect (fail-open,
    // nothing decided, nothing ledgered) without needing to reproduce the
    // exact crash site.
    let tool_name_raw = payload_obj.get("tool_name");
    let tool_name_is_valid = !matches!(tool_name_raw, Some(v) if !v.is_string());
    if !tool_name_is_valid && !rules.is_empty() {
        return Outcome::Error(
            "tool_name must be a string (Python: TypeError in tool_re.search)".to_string(),
        );
    }
    let tool_name = tool_name_raw
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    let tool_input_value = py_or_default_object(payload_obj.get("tool_input"));

    // Python: `cwd = payload.get("cwd", "")` — same "wrong type is a ticking
    // time bomb, not an immediate error" shape as tool_name, except cwd is
    // only ever touched by a rule's `unless_cwd` regex — so a malformed cwd
    // is validated lazily, inside the per-rule loop below, exactly where
    // Python's `rule["unless_cwd_re"].search(cwd)` would raise. A rule with
    // no `unless_cwd` never looks at cwd at all, matching Python's own
    // laziness: `git-stash-shared-checkout` (which has `unless_cwd`) blows
    // up on a non-string cwd, but a rule with no `unless_cwd` set doesn't
    // care what type cwd is.
    let cwd_raw = payload_obj.get("cwd");
    let cwd_is_valid = !matches!(cwd_raw, Some(v) if !v.is_string());
    let cwd = cwd_raw.and_then(Value::as_str).unwrap_or("").to_string();

    let session_id = payload_obj
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    let text = match canonical_text(&tool_name, &tool_input_value) {
        Ok(t) => t,
        Err(e) => return Outcome::Error(e),
    };

    let compiled = match compile_rules(rules) {
        Ok(c) => c,
        Err(e) => return Outcome::Error(e),
    };

    let mut fires: Vec<Fire> = Vec::new();
    for rule in &compiled {
        if !rule.tool_re.is_match(&tool_name) {
            continue;
        }
        if let Some(re) = &rule.unless_cwd_re {
            if !cwd_is_valid {
                return Outcome::Error(
                    "cwd must be a string (Python: TypeError in unless_cwd_re.search)".to_string(),
                );
            }
            if re.is_match(&cwd) {
                continue;
            }
        }
        let all_matches: Vec<regex::Match> = rule.match_re.find_iter(&text).collect();
        if all_matches.is_empty() {
            continue;
        }
        let chosen = match &rule.unless_match_re {
            None => Some(all_matches[0]),
            Some(re) if all_matches.len() == 1 => {
                // Single occurrence: unchanged whole-text check (mirrors
                // the Python reference exactly) — some rules, e.g.
                // pipe-tail-masks-exit and hex-events-flat-policy, rely on
                // a safety marker appearing ANYWHERE in the text, not just
                // next to the match.
                if re.is_match(&text) {
                    None
                } else {
                    Some(all_matches[0])
                }
            }
            Some(re) => {
                // Multiple occurrences in one canonical text (review
                // finding G1: e.g. a chained shell command combining a safe
                // subcommand with a dangerous bare one) — judge each
                // occurrence by its own "same command" window (see
                // `window_bounds`) so one safe occurrence can't
                // blanket-suppress a dangerous sibling.
                all_matches.iter().copied().find(|m| {
                    let (start, end) = window_bounds(&text, m.start(), m.end());
                    text.get(start..end).is_none_or(|w| !re.is_match(w))
                })
            }
        };
        let m = match chosen {
            Some(m) => m,
            None => continue,
        };
        fires.push(Fire {
            rule_id: rule.id.clone(),
            decision: rule.decision.clone(),
            message: rule.message.clone(),
            matched: truncate_chars(m.as_str(), MATCH_TRUNCATE),
        });
    }

    if fires.is_empty() {
        return Outcome::Abstain;
    }

    // deny beats ask beats first-prior-in-file-order (fires is already in
    // rules-array order since we iterate `compiled` in that order above).
    let winner = ["deny", "ask", "prior"]
        .iter()
        .find_map(|want| fires.iter().find(|f| f.decision == *want).cloned());
    let winner = match winner {
        Some(w) => w,
        // Unreachable with the real rules file (every seed rule's decision
        // is deny/ask/prior) — fail loud rather than silently drop a fire
        // Python would (mis)handle by abstaining on stdout while still
        // ledgering; a config bug here deserves the same fail-open contract
        // (one stderr line, exit 0) as any other internal error.
        None => {
            return Outcome::Error("a fired rule had a decision outside deny/ask/prior".to_string())
        }
    };

    let ctx = LedgerContext {
        session_id,
        tool: tool_name,
        cwd,
        preview: truncate_chars(&text, PREVIEW_TRUNCATE),
    };

    Outcome::Fired { fires, winner, ctx }
}

/// Ledger filename, shared with the Python reference.
const LEDGER_FILENAME: &str = "router-fires.jsonl";

/// Resolve `$HEX_LEDGER_DIR`, else `~/.hex/ledger`, creating it if needed.
fn ledger_path() -> Option<PathBuf> {
    let dir = match std::env::var("HEX_LEDGER_DIR") {
        Ok(d) if !d.is_empty() => PathBuf::from(d),
        _ => dirs::home_dir()?.join(".hex").join("ledger"),
    };
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join(LEDGER_FILENAME))
}

/// Append one ledger line per fire (Python appends the whole batch in one
/// `write` call too — same all-or-nothing failure mode).
fn append_ledger(fires: &[Fire], ctx: &LedgerContext) -> Result<(), String> {
    let path = ledger_path().ok_or_else(|| "could not resolve ledger directory".to_string())?;
    let ts = chrono::Utc::now().to_rfc3339();

    let mut buf = String::new();
    for fire in fires {
        let mut obj = serde_json::Map::new();
        obj.insert("ts".to_string(), Value::String(ts.clone()));
        obj.insert(
            "session_id".to_string(),
            Value::String(ctx.session_id.clone()),
        );
        obj.insert("rule_id".to_string(), Value::String(fire.rule_id.clone()));
        obj.insert("tool".to_string(), Value::String(ctx.tool.clone()));
        obj.insert("decision".to_string(), Value::String(fire.decision.clone()));
        obj.insert("match".to_string(), Value::String(fire.matched.clone()));
        obj.insert("preview".to_string(), Value::String(ctx.preview.clone()));
        obj.insert("cwd".to_string(), Value::String(ctx.cwd.clone()));
        let line = serde_json::to_string(&Value::Object(obj)).map_err(|e| e.to_string())?;
        buf.push_str(&line);
        buf.push('\n');
    }

    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    f.write_all(buf.as_bytes()).map_err(|e| e.to_string())
}

/// Build the `hookSpecificOutput` object for the winning fire — deny/ask use
/// `permissionDecision`/`permissionDecisionReason`, prior uses
/// `additionalContext`. Mirrors `main()` in the Python reference.
fn build_hso(winner: &Fire) -> Value {
    let mut hso = serde_json::Map::new();
    hso.insert(
        "hookEventName".to_string(),
        Value::String("PreToolUse".to_string()),
    );
    if winner.decision == "deny" || winner.decision == "ask" {
        hso.insert(
            "permissionDecision".to_string(),
            Value::String(winner.decision.clone()),
        );
        hso.insert(
            "permissionDecisionReason".to_string(),
            Value::String(winner.message.clone()),
        );
    } else {
        hso.insert(
            "additionalContext".to_string(),
            Value::String(winner.message.clone()),
        );
    }
    Value::Object(hso)
}

/// Thin IO wrapper: read stdin, resolve the rules file, call `decide`, write
/// the ledger, print stdout — or fail open on any error.
pub fn run() {
    let mut raw = String::new();
    if std::io::stdin().read_to_string(&mut raw).is_err() {
        eprintln!("[router] error: failed to read stdin");
        std::process::exit(0);
    }

    // `super::resolve_hex_dir` already prints exactly one stderr line and
    // returns `None` when neither `HEX_DIR` nor a hex-workspace-shaped
    // `CLAUDE_PROJECT_DIR` is set — that IS this hook's "abstain with one
    // stderr line" contract for an unresolvable workspace.
    let hex_dir = match super::resolve_hex_dir("router") {
        Some(d) => d,
        None => std::process::exit(0),
    };

    let rules_path = hex_dir.join(".hex/hooks/router-rules.json");
    let rules: Vec<Rule> = match std::fs::read_to_string(&rules_path) {
        Ok(raw_rules) => match serde_json::from_str(&raw_rules) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[router] error: {e}");
                std::process::exit(0);
            }
        },
        Err(e) => {
            eprintln!(
                "[router] error: failed to read {}: {e}",
                rules_path.display()
            );
            std::process::exit(0);
        }
    };

    match decide(&rules, &raw) {
        Outcome::Abstain => {}
        Outcome::Error(e) => eprintln!("[router] error: {e}"),
        Outcome::Fired { fires, winner, ctx } => {
            if let Err(e) = append_ledger(&fires, &ctx) {
                eprintln!("[router] error: {e}");
                std::process::exit(0);
            }
            let out = serde_json::json!({ "hookSpecificOutput": build_hso(&winner) });
            println!("{out}");
        }
    }
    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::Path;
    use std::process::{Command, Stdio};

    const DEFAULT_CWD: &str = "/tmp/hex-home/hex";

    fn make_payload(tool_name: &str, tool_input: Value, cwd: &str, session_id: &str) -> String {
        json!({
            "session_id": session_id,
            "transcript_path": "/tmp/transcript.jsonl",
            "cwd": cwd,
            "permission_mode": "default",
            "hook_event_name": "PreToolUse",
            "tool_name": tool_name,
            "tool_input": tool_input,
        })
        .to_string()
    }

    fn make_payload_default(tool_name: &str, tool_input: Value) -> String {
        make_payload(tool_name, tool_input, DEFAULT_CWD, "sess-1")
    }

    /// Load the real, shared rules file — the same one the Python reference
    /// reads. Keeping one source of truth avoids fixture drift between the
    /// two implementations.
    fn load_seed_rules() -> Vec<Rule> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../hooks/router-rules.json");
        let raw = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("router-rules.json must exist at {path:?}: {e}"));
        serde_json::from_str(&raw)
            .unwrap_or_else(|e| panic!("router-rules.json must parse into Vec<Rule>: {e}"))
    }

    struct RuleFixture {
        id: &'static str,
        decision: &'static str,
        tool_name: &'static str,
        positive: Value,
        near_miss_tool_name: &'static str,
        near_miss: Value,
    }

    /// Port of `RULE_FIXTURES` in test_router.py — one positive + one
    /// near-miss payload per seed rule, in seed-rule-order.
    fn rule_fixtures() -> Vec<RuleFixture> {
        vec![
            RuleFixture {
                id: "gh-pr-merge-ci-green",
                decision: "prior",
                tool_name: "Bash",
                positive: json!({"command": "gh pr merge 123 --squash"}),
                near_miss_tool_name: "Bash",
                near_miss: json!({"command": "gh pr checks 123 --watch"}),
            },
            RuleFixture {
                id: "vitest-spawnsync",
                decision: "prior",
                tool_name: "Edit",
                positive: json!({
                    "file_path": "src/components/foo.test.ts",
                    "old_string": "x",
                    "new_string": "const r = spawnSync('ls', []);",
                }),
                near_miss_tool_name: "Edit",
                near_miss: json!({
                    "file_path": "src/components/foo.ts",
                    "old_string": "x",
                    "new_string": "const r = spawnSync('ls', []);",
                }),
            },
            RuleFixture {
                id: "git-stash-shared-checkout",
                decision: "deny",
                tool_name: "Bash",
                positive: json!({"command": "git stash"}),
                near_miss_tool_name: "Bash",
                near_miss: json!({"command": "git stash list"}),
            },
            RuleFixture {
                id: "git-push-force",
                decision: "ask",
                tool_name: "Bash",
                positive: json!({"command": "git push origin main --force"}),
                near_miss_tool_name: "Bash",
                near_miss: json!({"command": "git push origin main"}),
            },
            RuleFixture {
                id: "git-push-force-with-lease",
                decision: "ask",
                tool_name: "Bash",
                positive: json!({"command": "git push origin main --force-with-lease"}),
                near_miss_tool_name: "Bash",
                near_miss: json!({"command": "git push origin main"}),
            },
            RuleFixture {
                id: "git-destructive-ask",
                decision: "ask",
                tool_name: "Bash",
                positive: json!({"command": "git reset --hard HEAD~1"}),
                near_miss_tool_name: "Bash",
                near_miss: json!({"command": "git reset --soft HEAD~1"}),
            },
            RuleFixture {
                id: "boi-dispatch-spec-priors",
                decision: "prior",
                tool_name: "Bash",
                positive: json!({"command": "boi dispatch spec.toml"}),
                near_miss_tool_name: "Bash",
                near_miss: json!({"command": "boi dashboard"}),
            },
            RuleFixture {
                id: "gh-fast-polling",
                decision: "ask",
                tool_name: "Bash",
                positive: json!({"command": "while true; do gh pr checks 123; sleep 5; done"}),
                near_miss_tool_name: "Bash",
                near_miss: json!({"command": "while true; do gh pr checks 123; sleep 90; done"}),
            },
            RuleFixture {
                id: "pipe-tail-masks-exit",
                decision: "prior",
                tool_name: "Bash",
                positive: json!({"command": "pytest -q | tail -20"}),
                near_miss_tool_name: "Bash",
                near_miss: json!({"command": "pytest -q"}),
            },
            RuleFixture {
                id: "hex-events-flat-policy",
                decision: "prior",
                tool_name: "Write",
                positive: json!({
                    "file_path": "/tmp/hex-home/.hex-events/policies/my-policy.yaml",
                    "content": "name: my-policy\ntrigger:\n  event: foo\naction:\n  type: shell\n  command: echo hi\n",
                }),
                near_miss_tool_name: "Write",
                near_miss: json!({
                    "file_path": "/tmp/hex-home/.hex-events/policies/my-policy.yaml",
                    "content": "name: my-policy\nrules:\n  - name: r1\n    trigger:\n      event: foo\n",
                }),
            },
            RuleFixture {
                id: "hex-memory-index-full",
                decision: "ask",
                tool_name: "Bash",
                positive: json!({"command": "hex memory index --full"}),
                near_miss_tool_name: "Bash",
                near_miss: json!({"command": "hex memory index"}),
            },
            RuleFixture {
                id: "builtin-scheduler-tools",
                decision: "ask",
                tool_name: "CronCreate",
                positive: json!({"schedule": "* * * * *", "command": "echo hi"}),
                near_miss_tool_name: "CronList",
                near_miss: json!({"filter": "*"}),
            },
            RuleFixture {
                id: "builtin-websearch",
                decision: "ask",
                tool_name: "WebSearch",
                positive: json!({"query": "test"}),
                near_miss_tool_name: "WebFetch",
                near_miss: json!({"url": "https://example.com"}),
            },
            RuleFixture {
                id: "backticks-in-unquoted-heredoc",
                decision: "prior",
                tool_name: "Bash",
                positive: json!({"command": "python3 - <<PYEOF\nprint('run `boi start` now')\nPYEOF"}),
                near_miss_tool_name: "Bash",
                near_miss: json!({"command": "python3 - <<'PYEOF'\nprint('run `boi start` now')\nPYEOF"}),
            },
        ]
    }

    #[test]
    fn seed_rules_cover_exactly_14_ids_matching_fixtures() {
        let rules = load_seed_rules();
        assert_eq!(rules.len(), 14, "expected exactly 14 seed rules");
        let rule_ids: std::collections::BTreeSet<_> = rules.iter().map(|r| r.id.clone()).collect();
        let fixture_ids: std::collections::BTreeSet<_> =
            rule_fixtures().iter().map(|f| f.id.to_string()).collect();
        assert_eq!(rule_ids, fixture_ids, "fixture/rules-file id drift");
    }

    #[test]
    fn each_seed_rule_fires_with_expected_decision_on_its_positive_fixture() {
        let rules = load_seed_rules();
        for fx in rule_fixtures() {
            let raw = make_payload_default(fx.tool_name, fx.positive.clone());
            let outcome = decide(&rules, &raw);
            match outcome {
                Outcome::Fired { fires, winner, ctx } => {
                    assert_eq!(fires.len(), 1, "{}: expected exactly one fire", fx.id);
                    assert_eq!(winner.rule_id, fx.id, "{}: wrong winner", fx.id);
                    assert_eq!(winner.decision, fx.decision, "{}: wrong decision", fx.id);
                    assert!(!winner.matched.is_empty(), "{}: empty match", fx.id);
                    assert!(
                        winner.matched.len() <= 200,
                        "{}: match not truncated to 200",
                        fx.id
                    );
                    assert_eq!(ctx.tool, fx.tool_name, "{}: wrong tool in ctx", fx.id);
                    assert_eq!(ctx.cwd, DEFAULT_CWD, "{}: wrong cwd in ctx", fx.id);
                    assert_eq!(
                        ctx.session_id, "sess-1",
                        "{}: wrong session_id in ctx",
                        fx.id
                    );
                    assert!(
                        ctx.preview.len() <= 300,
                        "{}: preview not truncated to 300",
                        fx.id
                    );
                }
                other => panic!("{}: expected Fired, got {other:?}", fx.id),
            }
        }
    }

    #[test]
    fn each_seed_rule_abstains_on_its_near_miss_fixture() {
        let rules = load_seed_rules();
        for fx in rule_fixtures() {
            let raw = make_payload_default(fx.near_miss_tool_name, fx.near_miss.clone());
            let outcome = decide(&rules, &raw);
            assert_eq!(
                outcome,
                Outcome::Abstain,
                "{}: near-miss must abstain",
                fx.id
            );
        }
    }

    #[test]
    fn git_stash_unless_cwd_skips_inside_worktrees() {
        let rules = load_seed_rules();
        let raw = make_payload(
            "Bash",
            json!({"command": "git stash"}),
            "/tmp/hex-home/.boi/v2/worktrees/Scnfz8k1f/T7w2t3bzf",
            "sess-1",
        );
        assert_eq!(decide(&rules, &raw), Outcome::Abstain);
    }

    /// Review finding G1: a global `unless_match` (searching the WHOLE
    /// canonical text) let a single safe sub-invocation blanket-suppress a
    /// dangerous sibling chained in the same command string. A text with
    /// more than one occurrence of the rule's `match` pattern must judge
    /// each occurrence by its own "same command" window instead.
    #[test]
    fn safe_subcommand_does_not_shield_a_chained_dangerous_one() {
        let rules = load_seed_rules();
        let cmd = "git stash pop; git stash";
        let raw = make_payload_default("Bash", json!({"command": cmd}));
        match decide(&rules, &raw) {
            Outcome::Fired { winner, fires, .. } => {
                assert_eq!(winner.decision, "deny", "{cmd}");
                assert!(
                    fires
                        .iter()
                        .any(|f| f.rule_id == "git-stash-shared-checkout"),
                    "{cmd}"
                );
            }
            other => panic!("{cmd}: expected Fired(deny), got {other:?}"),
        }
    }

    #[test]
    fn force_with_lease_does_not_shield_a_chained_bare_force() {
        let rules = load_seed_rules();
        let cmd = "git push origin main --force-with-lease; git push origin main --force";
        let raw = make_payload_default("Bash", json!({"command": cmd}));
        match decide(&rules, &raw) {
            Outcome::Fired { winner, fires, .. } => {
                assert_eq!(winner.decision, "ask", "{cmd}");
                assert!(
                    fires.iter().any(|f| f.rule_id == "git-push-force"),
                    "{cmd}: expected git-push-force among fires, got {fires:?}"
                );
            }
            other => panic!("{cmd}: expected Fired(ask), got {other:?}"),
        }
    }

    /// Regression guard: the multi-occurrence path must never fire for a
    /// text where the rule's `match` pattern has only ONE occurrence — this
    /// is exactly the existing pipe-tail-masks-exit contract
    /// (`pipefail_or_pipestatus_abstains`) and must stay untouched.
    #[test]
    fn single_occurrence_pipefail_check_is_unaffected() {
        let rules = load_seed_rules();
        for cmd in [
            "set -o pipefail; cargo test --locked | tail -20",
            "pytest -q | tail -5; echo exit=${PIPESTATUS[0]}",
        ] {
            let raw = make_payload_default("Bash", json!({"command": cmd}));
            assert_eq!(decide(&rules, &raw), Outcome::Abstain, "{cmd}");
        }
    }

    /// Review finding G2: the compact-JSON canonical-text branch (non-Bash,
    /// non-text-tool) must escape non-ASCII exactly like Python's
    /// `json.dumps(..., sort_keys=True)` default (`ensure_ascii=True`),
    /// byte-for-byte — not just "parses to the same value".
    #[test]
    fn canonical_text_escapes_non_ascii_like_python_ensure_ascii() {
        let tool_input = json!({"schedule": "* * * * *", "command": "echo héllo"});
        let text = canonical_text("CronCreate", &tool_input).unwrap();
        assert_eq!(
            text,
            "{\"command\":\"echo h\\u00e9llo\",\"schedule\":\"* * * * *\"}"
        );
        assert!(text.is_ascii(), "canonical text must be ASCII-only: {text}");
    }

    #[test]
    fn bounded_for_loop_over_pr_list_abstains() {
        let rules = load_seed_rules();
        let cmd = "for n in 293 294 295 296 297; do t=$(gh pr view $n -R owner/repo --json title --jq .title); python3 hex_emit.py github.pr.opened \"$t\"; sleep 2; done";
        let raw = make_payload_default("Bash", json!({"command": cmd}));
        assert_eq!(decide(&rules, &raw), Outcome::Abstain);
    }

    #[test]
    fn until_loop_polling_still_asks() {
        let rules = load_seed_rules();
        let cmd = "until gh run view 123 --json status --jq .status | grep -q completed; do sleep 10; done";
        let raw = make_payload_default("Bash", json!({"command": cmd}));
        match decide(&rules, &raw) {
            Outcome::Fired { winner, .. } => {
                assert_eq!(winner.decision, "ask");
                assert_eq!(winner.rule_id, "gh-fast-polling");
            }
            other => panic!("expected Fired(ask), got {other:?}"),
        }
    }

    #[test]
    fn mentions_inside_text_abstain() {
        let rules = load_seed_rules();
        let mentions = [
            "echo \"please do not run git stash here\"",
            "git commit -m \"router: no git stash in shared checkouts\"",
            "grep -rn 'git stash' docs/",
            "python3 - <<'EOF'\nrule = r'git stash'\nprint(rule)\nEOF",
            "echo \"git push --force is bad\"",
            "cat notes.txt | grep 'gh pr merge'",
        ];
        for cmd in mentions {
            let raw = make_payload_default("Bash", json!({"command": cmd}));
            assert_eq!(
                decide(&rules, &raw),
                Outcome::Abstain,
                "mention must abstain: {cmd:?}"
            );
        }
    }

    #[test]
    fn real_invocations_still_fire_at_command_position() {
        let rules = load_seed_rules();
        let cases: &[(&str, &str)] = &[
            ("git stash", "deny"),
            ("cd /tmp/x && git stash -u", "deny"),
            ("git add -A; git stash push -m wip", "deny"),
            ("time git push origin feat -f", "ask"),
            ("git push --force-with-lease origin feat", "ask"),
            ("gh pr merge 12 --squash", "prior"),
        ];
        for (cmd, expected) in cases {
            let raw = make_payload_default("Bash", json!({"command": cmd}));
            match decide(&rules, &raw) {
                Outcome::Fired { winner, .. } => {
                    assert_eq!(&winner.decision, expected, "{cmd}");
                }
                other => panic!("{cmd}: expected Fired, got {other:?}"),
            }
        }
    }

    #[test]
    fn stash_pop_apply_drop_abstain() {
        let rules = load_seed_rules();
        for cmd in [
            "git stash pop",
            "git stash apply stash@{0}",
            "git stash drop",
            "git -C /tmp/r stash pop",
        ] {
            let raw = make_payload_default("Bash", json!({"command": cmd}));
            assert_eq!(
                decide(&rules, &raw),
                Outcome::Abstain,
                "{cmd:?} must abstain"
            );
        }
    }

    #[test]
    fn edit_of_policy_yaml_abstains() {
        let rules = load_seed_rules();
        let raw = make_payload_default(
            "Edit",
            json!({
                "file_path": "/tmp/hex-home/x/.hex-events/policies/foo.yaml",
                "old_string": "timeout: 60",
                "new_string": "timeout: 120",
            }),
        );
        assert_eq!(decide(&rules, &raw), Outcome::Abstain);
    }

    #[test]
    fn pipefail_or_pipestatus_abstains() {
        let rules = load_seed_rules();
        for cmd in [
            "set -o pipefail; cargo test --locked | tail -20",
            "pytest -q | tail -5; echo exit=${PIPESTATUS[0]}",
        ] {
            let raw = make_payload_default("Bash", json!({"command": cmd}));
            assert_eq!(decide(&rules, &raw), Outcome::Abstain, "{cmd}");
        }
    }

    #[test]
    fn plain_pipe_to_tail_still_fires() {
        let rules = load_seed_rules();
        let raw = make_payload_default("Bash", json!({"command": "pnpm test | tail -20"}));
        match decide(&rules, &raw) {
            Outcome::Fired { winner, .. } => assert_eq!(winner.rule_id, "pipe-tail-masks-exit"),
            other => panic!("expected Fired, got {other:?}"),
        }
    }

    #[test]
    fn grep_and_head_fire_like_tail() {
        let rules = load_seed_rules();
        for cmd in [
            "python3 -m unittest discover -s tests | grep -E '^(Ran|OK|FAILED)'",
            "cargo test --locked | head -40",
        ] {
            let raw = make_payload_default("Bash", json!({"command": cmd}));
            match decide(&rules, &raw) {
                Outcome::Fired { winner, .. } => {
                    assert_eq!(winner.rule_id, "pipe-tail-masks-exit", "{cmd}")
                }
                other => panic!("{cmd}: expected Fired, got {other:?}"),
            }
        }
    }

    #[test]
    fn deny_beats_prior_when_both_match() {
        let rules = load_seed_rules();
        let raw = make_payload_default(
            "Bash",
            json!({"command": "gh pr merge 42 --squash; git stash"}),
        );
        match decide(&rules, &raw) {
            Outcome::Fired { fires, winner, .. } => {
                assert_eq!(winner.decision, "deny");
                assert_eq!(winner.rule_id, "git-stash-shared-checkout");
                let mut rule_ids: Vec<_> = fires.iter().map(|f| f.rule_id.clone()).collect();
                rule_ids.sort();
                assert_eq!(
                    rule_ids,
                    vec!["gh-pr-merge-ci-green", "git-stash-shared-checkout"]
                );
                let by_id: std::collections::BTreeMap<_, _> = fires
                    .iter()
                    .map(|f| (f.rule_id.clone(), f.decision.clone()))
                    .collect();
                assert_eq!(by_id["gh-pr-merge-ci-green"], "prior");
                assert_eq!(by_id["git-stash-shared-checkout"], "deny");
            }
            other => panic!("expected Fired, got {other:?}"),
        }
    }

    #[test]
    fn only_one_prior_wins_when_multiple_priors_match_first_in_file_order() {
        let rules = load_seed_rules();
        let raw = make_payload_default(
            "Bash",
            json!({"command": "pytest -q | tail -20 && gh pr merge 7 --squash"}),
        );
        match decide(&rules, &raw) {
            Outcome::Fired { fires, winner, .. } => {
                assert_eq!(winner.decision, "prior");
                // Seed order lists gh-pr-merge-ci-green before pipe-tail-masks-exit.
                assert_eq!(winner.rule_id, "gh-pr-merge-ci-green");
                let mut rule_ids: Vec<_> = fires.iter().map(|f| f.rule_id.clone()).collect();
                rule_ids.sort();
                assert_eq!(
                    rule_ids,
                    vec!["gh-pr-merge-ci-green", "pipe-tail-masks-exit"]
                );
            }
            other => panic!("expected Fired, got {other:?}"),
        }
    }

    #[test]
    fn malformed_json_stdin_fails_open_with_error() {
        let rules = load_seed_rules();
        match decide(&rules, "{not valid json") {
            Outcome::Error(msg) => assert!(!msg.is_empty()),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    /// Review finding G1: a wrongly-typed `cwd` (e.g. a JSON number instead
    /// of a string) must fail open exactly like the Python reference, which
    /// crashes with `TypeError: expected string or bytes-like object, got
    /// 'int'` inside `unless_cwd_re.search(cwd)` for `git-stash-shared-checkout`
    /// (confirmed live: `echo '{"cwd":123,...}' | python3 ... router.py`
    /// prints `[router] error: expected string or bytes-like object, got
    /// 'int'` and exits 0 with no stdout). Silently coercing cwd to `""`
    /// would make this rule's `unless_cwd` never match and the command
    /// would wrongly fire `deny` instead of aborting to fail-open.
    #[test]
    fn wrong_typed_cwd_fails_open_with_error_not_deny() {
        let rules = load_seed_rules();
        let raw = json!({
            "session_id": "sess-1",
            "cwd": 123,
            "tool_name": "Bash",
            "tool_input": {"command": "git stash"},
        })
        .to_string();
        match decide(&rules, &raw) {
            Outcome::Error(msg) => assert!(!msg.is_empty()),
            other => panic!("expected Error (fail-open), got {other:?}"),
        }
    }

    /// A rule with no `unless_cwd` never looks at cwd in Python (short
    /// circuits on `is not None` before calling `.search`), so a
    /// wrongly-typed cwd must NOT poison a decision that never touches it.
    /// `git-stash-shared-checkout` is the ONLY seed rule with `unless_cwd`,
    /// and its `tool` pattern (`^Bash$`) only ever matches `tool_name ==
    /// "Bash"` — so this pins the claim with a non-Bash tool (`CronCreate`),
    /// which no `unless_cwd` rule's `tool` regex can match, meaning cwd is
    /// never touched at all (confirmed: the same malformed cwd against a
    /// Bash command DOES crash the live Python reference, because
    /// `git-stash-shared-checkout`'s `unless_cwd` check runs before its
    /// `match` check and applies to every Bash invocation regardless of
    /// command text — see `wrong_typed_cwd_fails_open_with_error_not_deny`).
    #[test]
    fn wrong_typed_cwd_does_not_break_rules_without_unless_cwd() {
        let rules = load_seed_rules();
        let raw = json!({
            "session_id": "sess-1",
            "cwd": 123,
            "tool_name": "CronCreate",
            "tool_input": {"schedule": "* * * * *", "command": "echo hi"},
        })
        .to_string();
        match decide(&rules, &raw) {
            Outcome::Fired { winner, .. } => {
                assert_eq!(winner.rule_id, "builtin-scheduler-tools");
            }
            other => panic!("expected Fired, got {other:?}"),
        }
    }

    /// Review finding G1's general class: a wrongly-typed `tool_name` (e.g.
    /// a JSON number) makes Python's `rule["tool_re"].search(tool_name)`
    /// raise `TypeError` on the very first rule — it must fail open, not
    /// silently coerce to `""` and evaluate as if no tool_name were given.
    #[test]
    fn wrong_typed_tool_name_fails_open_with_error() {
        let rules = load_seed_rules();
        let raw = json!({
            "session_id": "sess-1",
            "cwd": "/tmp",
            "tool_name": 42,
            "tool_input": {"command": "git stash"},
        })
        .to_string();
        match decide(&rules, &raw) {
            Outcome::Error(msg) => assert!(!msg.is_empty()),
            other => panic!("expected Error (fail-open), got {other:?}"),
        }
    }

    /// Review finding G1's general class, applied to `tool_input`: Python's
    /// canonical_text calls `tool_input.get("command", "")` for a Bash
    /// invocation, which raises `AttributeError` if `tool_input` isn't a
    /// dict (e.g. a JSON array) — must fail open, not silently treat it as
    /// an empty object.
    #[test]
    fn wrong_typed_tool_input_for_bash_fails_open_with_error() {
        let rules = load_seed_rules();
        let raw = json!({
            "session_id": "sess-1",
            "cwd": "/tmp",
            "tool_name": "Bash",
            "tool_input": ["git", "stash"],
        })
        .to_string();
        match decide(&rules, &raw) {
            Outcome::Error(msg) => assert!(!msg.is_empty()),
            other => panic!("expected Error (fail-open), got {other:?}"),
        }
    }

    /// Differential companion to the four tests above: run the SAME
    /// malformed payloads through the live Python reference and assert it
    /// also produces empty stdout + exit 0 (fail-open), pinning that our
    /// `Outcome::Error` mirrors real Python behavior rather than an assumed
    /// one. This directly reproduces the exact repro from review finding G1.
    #[test]
    fn wrong_typed_fields_match_python_reference_fail_open_behavior() {
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let python_script = repo_root.join("hooks/scripts/pretooluse-router.py");
        assert!(python_script.exists());

        let payloads = [
            json!({
                "session_id": "sess-1",
                "cwd": 123,
                "tool_name": "Bash",
                "tool_input": {"command": "git stash"},
            }),
            json!({
                "session_id": "sess-1",
                "cwd": "/tmp",
                "tool_name": 42,
                "tool_input": {"command": "git stash"},
            }),
            json!({
                "session_id": "sess-1",
                "cwd": "/tmp",
                "tool_name": "Bash",
                "tool_input": ["git", "stash"],
            }),
        ];

        for payload in payloads {
            let raw = payload.to_string();
            let ledger_dir = tempfile::tempdir().unwrap();
            let out = Command::new("python3")
                .args(["-I", "-S"])
                .arg(&python_script)
                .env("HEX_LEDGER_DIR", ledger_dir.path())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .and_then(|mut child| {
                    use std::io::Write;
                    child.stdin.take().unwrap().write_all(raw.as_bytes())?;
                    child.wait_with_output()
                })
                .expect("failed to run python reference router");
            assert!(
                out.status.success(),
                "python reference must exit 0 (fail-open) for {raw}"
            );
            assert!(
                out.stdout.is_empty(),
                "python reference must print no stdout (fail-open) for {raw}, got {:?}",
                String::from_utf8_lossy(&out.stdout)
            );
            assert!(
                !out.stderr.is_empty(),
                "python reference must log a stderr error for {raw}"
            );
            assert!(
                ledger_lines(ledger_dir.path()).is_empty(),
                "{raw}: nothing should be ledgered"
            );
        }
    }

    #[test]
    fn empty_stdin_fails_open_with_error() {
        let rules = load_seed_rules();
        match decide(&rules, "") {
            Outcome::Error(msg) => assert!(!msg.is_empty()),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    /// Pins the `unless_match` mechanism in isolation, independent of which
    /// seed rule uses it: a rule whose `match` fires but whose `unless_match`
    /// also matches the SAME canonical text must not fire at all.
    #[test]
    fn unless_match_suppresses_a_fire_entirely() {
        let rules = vec![Rule {
            id: "test-unless-match-suppression".to_string(),
            tool: "^Bash$".to_string(),
            match_pattern: "foo".to_string(),
            unless_cwd: None,
            unless_match: Some("foo-safe".to_string()),
            decision: "ask".to_string(),
            message: "should never surface when unless_match matches".to_string(),
            source: None,
        }];
        let raw = make_payload_default("Bash", json!({"command": "run foo-safe now"}));
        assert_eq!(
            decide(&rules, &raw),
            Outcome::Abstain,
            "unless_match matching the canonical text must suppress the fire entirely"
        );
    }

    /// Structural guard (spec ground truth): the router must stay pure
    /// stdlib + regex + serde — no subprocess/network calls, so it can never
    /// regress into the ~31ms-per-invocation cost the Python reference paid
    /// for spawning a fresh interpreter. Scoped to the production code above
    /// this test module (the differential test below legitimately shells
    /// out to compare against the Python reference).
    #[test]
    fn production_code_has_no_subprocess_or_network_calls() {
        let source = include_str!("router.rs");
        let production_only = source
            .split("#[cfg(test)]")
            .next()
            .expect("file must contain a #[cfg(test)] marker");
        let forbidden = [
            "std::process::Command",
            "Command::new",
            "std::net::",
            "TcpStream",
            "reqwest",
            "hyper::",
            "curl ",
            "git ",
            "gh ",
        ];
        for tok in forbidden {
            assert!(
                !production_only.contains(tok),
                "router.rs production code contains forbidden token {tok:?}"
            );
        }
    }

    // ---- Differential test against the Python reference implementation ----

    const PROBE_FIXTURE_IDS_COUNT: usize = 14;

    fn ledger_lines(dir: &Path) -> Vec<Value> {
        let path = dir.join("router-fires.jsonl");
        if !path.exists() {
            return Vec::new();
        }
        std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .collect()
    }

    /// Drops the `ts` key (wall-clock, never comparable) from a ledger entry.
    fn without_ts(mut v: Value) -> Value {
        if let Value::Object(ref mut map) = v {
            map.remove("ts");
        }
        v
    }

    /// `CARGO_BIN_EXE_<name>` is only populated for integration-test/bench
    /// targets, not for unit tests compiled inside the binary crate itself —
    /// so resolve the built binary by convention instead: `HEX_ROUTER_BIN`
    /// override, else prefer a release build (the task's declared
    /// verification runs `cargo build --release --locked` first), else the
    /// debug build a plain `cargo test` produces automatically.
    fn rust_binary_path() -> PathBuf {
        if let Ok(p) = std::env::var("HEX_ROUTER_BIN") {
            return PathBuf::from(p);
        }
        let target_dir = std::env::var("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("target"));
        let release = target_dir.join("release").join("hex");
        if release.exists() {
            release
        } else {
            target_dir.join("debug").join("hex")
        }
    }

    #[test]
    fn rust_binary_matches_python_reference_on_every_probe_fixture() {
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let python_script = repo_root.join("hooks/scripts/pretooluse-router.py");
        assert!(
            python_script.exists(),
            "python reference script must exist at {python_script:?}"
        );

        let rust_bin = rust_binary_path();
        assert!(
            rust_bin.exists(),
            "rust `hex` binary not found at {rust_bin:?} — run `cargo build [--release] --locked` first \
             (or set HEX_ROUTER_BIN)"
        );

        // `hex hook router` resolves its rules file at `$HEX_DIR/.hex/hooks/
        // router-rules.json`, matching a deployed hex workspace's layout
        // (`.hex/<x>` there == `system/<x>` in this source repo — see
        // AGENTS.md). This checkout is the source repo itself, not a
        // deployed instance, so there is no `.hex/` here for HEX_DIR to
        // point at directly. Build a throwaway workspace shape instead: copy
        // the real `system/hooks/router-rules.json` into `<tmp>/.hex/hooks/`
        // and point HEX_DIR at `<tmp>`, so the rust binary reads the exact
        // same rules file as the Python reference without changing
        // production path-resolution behavior.
        let fake_hex_dir = tempfile::tempdir().unwrap();
        let fake_hooks_dir = fake_hex_dir.path().join(".hex/hooks");
        std::fs::create_dir_all(&fake_hooks_dir).unwrap();
        std::fs::copy(
            repo_root.join("hooks/router-rules.json"),
            fake_hooks_dir.join("router-rules.json"),
        )
        .expect("failed to stage router-rules.json for the rust binary fixture");

        let fixtures = rule_fixtures();
        assert_eq!(
            fixtures.len(),
            PROBE_FIXTURE_IDS_COUNT,
            "expected 14 probe fixtures"
        );

        let mut rust_durations_ms: Vec<f64> = Vec::new();

        for fx in &fixtures {
            for (tool_name, tool_input) in [
                (fx.tool_name, fx.positive.clone()),
                (fx.near_miss_tool_name, fx.near_miss.clone()),
            ] {
                let raw = make_payload_default(tool_name, tool_input);

                let py_ledger = tempfile::tempdir().unwrap();
                let py_out = Command::new("python3")
                    .args(["-I", "-S"])
                    .arg(&python_script)
                    .env("HEX_LEDGER_DIR", py_ledger.path())
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .and_then(|mut child| {
                        use std::io::Write;
                        child.stdin.take().unwrap().write_all(raw.as_bytes())?;
                        child.wait_with_output()
                    })
                    .expect("failed to run python reference router");

                let rust_ledger = tempfile::tempdir().unwrap();
                let start = std::time::Instant::now();
                let rust_out = Command::new(&rust_bin)
                    .args(["hook", "router"])
                    // Pin HEX_DIR to the staged fixture workspace, not
                    // whatever HEX_DIR happens to be set to in the ambient
                    // test environment (e.g. a developer's live HEX_DIR
                    // workspace checkout) — otherwise the subprocess reads a
                    // different, possibly stale or lookaround-using,
                    // router-rules.json than the one this test just diffed
                    // against the Python reference.
                    .env("HEX_DIR", fake_hex_dir.path())
                    .env("HEX_LEDGER_DIR", rust_ledger.path())
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .and_then(|mut child| {
                        use std::io::Write;
                        child.stdin.take().unwrap().write_all(raw.as_bytes())?;
                        child.wait_with_output()
                    })
                    .expect("failed to run rust router binary");
                rust_durations_ms.push(start.elapsed().as_secs_f64() * 1000.0);

                let py_stdout = String::from_utf8_lossy(&py_out.stdout);
                let rust_stdout = String::from_utf8_lossy(&rust_out.stdout);
                let py_json: Option<Value> = if py_stdout.trim().is_empty() {
                    None
                } else {
                    Some(serde_json::from_str(py_stdout.trim()).unwrap())
                };
                let rust_json: Option<Value> = if rust_stdout.trim().is_empty() {
                    None
                } else {
                    Some(serde_json::from_str(rust_stdout.trim()).unwrap())
                };
                assert_eq!(
                    py_json,
                    rust_json,
                    "{}/{tool_name}: stdout mismatch (python stderr={:?}, rust stderr={:?})",
                    fx.id,
                    String::from_utf8_lossy(&py_out.stderr),
                    String::from_utf8_lossy(&rust_out.stderr),
                );

                let py_lines: Vec<Value> = ledger_lines(py_ledger.path())
                    .into_iter()
                    .map(without_ts)
                    .collect();
                let rust_lines: Vec<Value> = ledger_lines(rust_ledger.path())
                    .into_iter()
                    .map(without_ts)
                    .collect();
                assert_eq!(
                    py_lines, rust_lines,
                    "{}/{tool_name}: ledger mismatch",
                    fx.id
                );
            }
        }

        rust_durations_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let median = rust_durations_ms[rust_durations_ms.len() / 2];
        println!("\n[latency] rust `hex hook router` median over {} runs: {median:.2} ms (informational only, not asserted)", rust_durations_ms.len());
    }
}
