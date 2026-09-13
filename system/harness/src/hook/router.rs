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
//! file, bad pattern, etc.) is swallowed into exactly ONE stderr line
//! (`[router] error: ...`, line breaks in the message escaped) and the
//! process still exits 0 with empty stdout. Output writes are best-effort:
//! a closed stdout/stderr pipe never turns into a panic or a non-zero exit.
//!
//! # Parity contract
//!
//! The Python script is the oracle. This file mirrors it function for
//! function (same names, minus the leading underscore), on the same data
//! model:
//!
//! * **Text is a sequence of Unicode code points** (`PyStr = Vec<u32>`),
//!   exactly like a Python `str`, including lone surrogates that Python's
//!   `json.loads` accepts from `\udXXX` escapes. Every offset the reference
//!   computes (match spans, quote spans, separator positions, heredoc
//!   bounds) is a code-point index here too; byte offsets only exist at the
//!   regex boundary (`CpText` maps between the two).
//! * **JSON is parsed by a small Python-compatible parser** (`parse_json`),
//!   not `serde_json`: integers keep arbitrary precision as decimal text,
//!   floats render with Python's `repr` rules (`1e-7` → `1e-07`, `100` from
//!   `1E2` → `100.0`), `NaN`/`Infinity` literals are accepted, lone
//!   surrogate escapes survive, duplicate keys keep the first position and
//!   the last value. `dumps` reproduces `json.dumps` byte-for-byte
//!   (`ensure_ascii`, `sort_keys`, both separator styles).
//! * **Regex semantics are the ASCII subset shared by both engines.** The
//!   reference compiles every pattern with `re.MULTILINE | re.ASCII`; this
//!   port rewrites each Python pattern into `regex` syntax with the same
//!   ASCII classes (`translate_py_regex`): `\b`/`\B` → `(?-u:\b)`, `\w`/`\d`/
//!   `\s` and their negations → explicit ASCII classes, `\Z` → `\z`, `(?i)`
//!   → per-letter ASCII case expansion. Python's `re.match(text, pos)` is
//!   an anchored search on the slice starting at `pos` (`\A`-prefixed
//!   pattern), `re.search(text, pos)` is `find_at`.
//! * **Ledger lines and stdout are `json.dumps` output**: default
//!   separators (`, ` / `: `), `ensure_ascii`, sorted keys for the ledger,
//!   insertion order for stdout — the same bytes the Python script writes.
//!
//! `decide` is pure (no IO) so it can be exercised directly by the unit
//! tests; `run` is the thin IO wrapper (stdin, rules file, ledger, stdout)
//! mirroring the reference's `main()`/`evaluate()` split. The required
//! current-build differential test lives in
//! `tests/hook_router_cli.rs` (it runs the built `hex` binary against the
//! Python reference over the shared probe fixtures).

use regex::{Regex, RegexBuilder};
use std::io::Read;
use std::path::PathBuf;

/// Ledger filename, shared with the Python reference.
const LEDGER_FILENAME: &str = "router-fires.jsonl";
/// Match-string truncation, ledger `match` field (Python `MATCH_TRUNCATE`).
const MATCH_TRUNCATE: usize = 200;
/// Canonical-text truncation, ledger `preview` field (Python's `text[:300]`).
const PREVIEW_TRUNCATE: usize = 300;
/// Tool names whose canonical text is `file_path` + edited content, joined
/// by `\n` (Python `TEXT_TOOLS`).
const TEXT_TOOLS: [&str; 4] = ["Edit", "Write", "MultiEdit", "NotebookEdit"];

// ---------------------------------------------------------------------------
// Python-compatible values
// ---------------------------------------------------------------------------

/// A Python `str`: a sequence of Unicode code points (lone surrogates
/// allowed, exactly like CPython's internal representation).
pub type PyStr = Vec<u32>;

fn pystr(s: &str) -> PyStr {
    s.chars().map(|c| c as u32).collect()
}

/// Where lone surrogate code points land when a `PyStr` has to become a
/// Rust `String` for the regex engine: U+F0000 + (cp - D800), inside
/// Supplementary Private Use Area-A. Every regex this router runs is
/// ASCII-class based, so a PUA char and a surrogate are indistinguishable to
/// it (both are "not a word/space/numeral char, matched by `.`"). The `String`
/// view is READ-ONLY: no text is ever converted back from it (PR #6 round-2
/// F14) — every value that leaves the router (matches, previews, resolved
/// paths, heredoc delimiters) is sliced or rebuilt from the original code
/// points using the engine's offsets, so a genuine U+F0000 and a lone U+D800
/// never collapse into each other.
const SURROGATE_PUA_BASE: u32 = 0xF0000;

fn cp_to_char(cp: u32) -> char {
    if (0xD800..=0xDFFF).contains(&cp) {
        char::from_u32(SURROGATE_PUA_BASE + (cp - 0xD800)).unwrap_or('\u{FFFD}')
    } else {
        char::from_u32(cp).unwrap_or('\u{FFFD}')
    }
}

fn cps_to_string(cps: &[u32]) -> String {
    cps.iter().map(|&c| cp_to_char(c)).collect()
}

/// The subset of Python's object model a JSON document can produce.
#[derive(Debug, Clone, PartialEq)]
pub enum PyValue {
    Null,
    Bool(bool),
    /// Arbitrary-precision integer, canonical decimal text (no leading
    /// zeros, `-0` normalized to `0`) — Python `int`.
    Int(String),
    Float(f64),
    Str(PyStr),
    List(Vec<PyValue>),
    /// Insertion-ordered, unique keys — Python `dict`.
    Dict(Vec<(PyStr, PyValue)>),
}

impl PyValue {
    pub fn str(s: &str) -> PyValue {
        PyValue::Str(pystr(s))
    }

    /// Python truthiness (`bool(v)`).
    fn truthy(&self) -> bool {
        match self {
            PyValue::Null => false,
            PyValue::Bool(b) => *b,
            PyValue::Int(d) => d != "0",
            PyValue::Float(f) => *f != 0.0,
            PyValue::Str(s) => !s.is_empty(),
            PyValue::List(l) => !l.is_empty(),
            PyValue::Dict(d) => !d.is_empty(),
        }
    }

    fn as_pystr(&self) -> Option<&PyStr> {
        match self {
            PyValue::Str(s) => Some(s),
            _ => None,
        }
    }

    /// `dict.get(key)` — `None` when the value is not a dict or the key is
    /// absent (callers decide whether a non-dict is an error, as Python's
    /// `AttributeError` would be).
    fn get(&self, key: &str) -> Option<&PyValue> {
        let k = pystr(key);
        match self {
            PyValue::Dict(items) => items.iter().find(|(kk, _)| *kk == k).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Python's `key in value` for a str key: dict membership, list
    /// membership, substring for a str; `TypeError` (Err) otherwise.
    fn contains_key(&self, key: &str) -> Result<bool, String> {
        let k = pystr(key);
        match self {
            PyValue::Dict(items) => Ok(items.iter().any(|(kk, _)| *kk == k)),
            PyValue::List(items) => Ok(items.iter().any(|v| v.as_pystr() == Some(&k))),
            PyValue::Str(s) => Ok(find_sub(s, &k, 0).is_some()),
            other => Err(format!(
                "argument of type '{}' is not iterable (Python: TypeError)",
                other.type_name()
            )),
        }
    }

    fn type_name(&self) -> &'static str {
        match self {
            PyValue::Null => "NoneType",
            PyValue::Bool(_) => "bool",
            PyValue::Int(_) => "int",
            PyValue::Float(_) => "float",
            PyValue::Str(_) => "str",
            PyValue::List(_) => "list",
            PyValue::Dict(_) => "dict",
        }
    }
}

impl PartialEq<str> for PyValue {
    fn eq(&self, other: &str) -> bool {
        matches!(self, PyValue::Str(s) if *s == pystr(other))
    }
}

impl PartialEq<&str> for PyValue {
    fn eq(&self, other: &&str) -> bool {
        self == *other
    }
}

// ---- json.loads --------------------------------------------------------------

struct JsonParser<'a> {
    b: &'a [u8],
    i: usize,
    depth: usize,
}

/// Python's `json.loads` (the C scanner's accept set): standard JSON plus
/// the `NaN`/`Infinity`/`-Infinity` literals, lone surrogate escapes kept
/// as-is, control characters inside strings rejected (`strict=True`),
/// trailing garbage rejected ("Extra data").
pub fn parse_json(raw: &str) -> Result<PyValue, String> {
    let mut p = JsonParser {
        b: raw.as_bytes(),
        i: 0,
        depth: 0,
    };
    p.ws();
    if p.i >= p.b.len() {
        return Err("Expecting value: line 1 column 1 (char 0)".to_string());
    }
    let v = p.value()?;
    p.ws();
    if p.i != p.b.len() {
        return Err(format!("Extra data: char {}", p.i));
    }
    Ok(v)
}

impl JsonParser<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn starts(&self, lit: &str) -> bool {
        self.b[self.i..].starts_with(lit.as_bytes())
    }

    fn value(&mut self) -> Result<PyValue, String> {
        if self.i >= self.b.len() {
            return Err(format!("Expecting value: char {}", self.i));
        }
        match self.b[self.i] {
            b'{' => self.object(),
            b'[' => self.array(),
            b'"' => Ok(PyValue::Str(self.string()?)),
            b'n' if self.starts("null") => {
                self.i += 4;
                Ok(PyValue::Null)
            }
            b't' if self.starts("true") => {
                self.i += 4;
                Ok(PyValue::Bool(true))
            }
            b'f' if self.starts("false") => {
                self.i += 5;
                Ok(PyValue::Bool(false))
            }
            b'N' if self.starts("NaN") => {
                self.i += 3;
                Ok(PyValue::Float(f64::NAN))
            }
            b'I' if self.starts("Infinity") => {
                self.i += 8;
                Ok(PyValue::Float(f64::INFINITY))
            }
            b'-' if self.starts("-Infinity") => {
                self.i += 9;
                Ok(PyValue::Float(f64::NEG_INFINITY))
            }
            b'-' | b'0'..=b'9' => self.number(),
            _ => Err(format!("Expecting value: char {}", self.i)),
        }
    }

    fn enter(&mut self) -> Result<(), String> {
        self.depth += 1;
        // CPython's default recursion limit (1000) turns a deeper document
        // into RecursionError → the script fails open. Same outcome here.
        if self.depth > 900 {
            return Err("maximum recursion depth exceeded while decoding a JSON document".into());
        }
        Ok(())
    }

    fn object(&mut self) -> Result<PyValue, String> {
        self.enter()?;
        self.i += 1; // {
        let mut items: Vec<(PyStr, PyValue)> = Vec::new();
        self.ws();
        if self.i < self.b.len() && self.b[self.i] == b'}' {
            self.i += 1;
            self.depth -= 1;
            return Ok(PyValue::Dict(items));
        }
        loop {
            self.ws();
            if self.i >= self.b.len() || self.b[self.i] != b'"' {
                return Err(format!(
                    "Expecting property name enclosed in double quotes: char {}",
                    self.i
                ));
            }
            let key = self.string()?;
            self.ws();
            if self.i >= self.b.len() || self.b[self.i] != b':' {
                return Err(format!("Expecting ':' delimiter: char {}", self.i));
            }
            self.i += 1;
            self.ws();
            let val = self.value()?;
            // dict semantics: an existing key keeps its position, takes the
            // new value.
            match items.iter_mut().find(|(k, _)| *k == key) {
                Some(slot) => slot.1 = val,
                None => items.push((key, val)),
            }
            self.ws();
            if self.i >= self.b.len() {
                return Err(format!("Expecting ',' delimiter: char {}", self.i));
            }
            match self.b[self.i] {
                b',' => {
                    self.i += 1;
                }
                b'}' => {
                    self.i += 1;
                    self.depth -= 1;
                    return Ok(PyValue::Dict(items));
                }
                _ => return Err(format!("Expecting ',' delimiter: char {}", self.i)),
            }
        }
    }

    fn array(&mut self) -> Result<PyValue, String> {
        self.enter()?;
        self.i += 1; // [
        let mut items = Vec::new();
        self.ws();
        if self.i < self.b.len() && self.b[self.i] == b']' {
            self.i += 1;
            self.depth -= 1;
            return Ok(PyValue::List(items));
        }
        loop {
            self.ws();
            items.push(self.value()?);
            self.ws();
            if self.i >= self.b.len() {
                return Err(format!("Expecting ',' delimiter: char {}", self.i));
            }
            match self.b[self.i] {
                b',' => {
                    self.i += 1;
                }
                b']' => {
                    self.i += 1;
                    self.depth -= 1;
                    return Ok(PyValue::List(items));
                }
                _ => return Err(format!("Expecting ',' delimiter: char {}", self.i)),
            }
        }
    }

    fn hex4(&mut self) -> Option<u32> {
        if self.i + 4 > self.b.len() {
            return None;
        }
        let s = std::str::from_utf8(&self.b[self.i..self.i + 4]).ok()?;
        let v = u32::from_str_radix(s, 16).ok()?;
        self.i += 4;
        Some(v)
    }

    fn string(&mut self) -> Result<PyStr, String> {
        self.i += 1; // opening quote
        let mut out: PyStr = Vec::new();
        loop {
            if self.i >= self.b.len() {
                return Err(format!("Unterminated string starting at: char {}", self.i));
            }
            let c = self.b[self.i];
            match c {
                b'"' => {
                    self.i += 1;
                    return Ok(out);
                }
                b'\\' => {
                    self.i += 1;
                    if self.i >= self.b.len() {
                        return Err(format!("Unterminated string starting at: char {}", self.i));
                    }
                    let e = self.b[self.i];
                    self.i += 1;
                    match e {
                        b'"' => out.push(0x22),
                        b'\\' => out.push(0x5c),
                        b'/' => out.push(0x2f),
                        b'b' => out.push(0x08),
                        b'f' => out.push(0x0c),
                        b'n' => out.push(0x0a),
                        b'r' => out.push(0x0d),
                        b't' => out.push(0x09),
                        b'u' => {
                            let hi = self.hex4().ok_or_else(|| {
                                format!("Invalid \\uXXXX escape: char {}", self.i)
                            })?;
                            if (0xD800..=0xDBFF).contains(&hi)
                                && self.b[self.i..].starts_with(b"\\u")
                            {
                                let save = self.i;
                                self.i += 2;
                                match self.hex4() {
                                    Some(lo) if (0xDC00..=0xDFFF).contains(&lo) => {
                                        out.push(0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00));
                                    }
                                    _ => {
                                        // Not a low surrogate: keep the lone
                                        // upper half; rescan from the `\u`.
                                        self.i = save;
                                        out.push(hi);
                                    }
                                }
                            } else {
                                out.push(hi);
                            }
                        }
                        _ => return Err(format!("Invalid \\escape: char {}", self.i - 2)),
                    }
                }
                c if c < 0x20 => {
                    return Err(format!("Invalid control character at: char {}", self.i));
                }
                b0 => {
                    // Decode ONE UTF-8 scalar from its lead byte (the input
                    // is a `&str`, so the sequence is well-formed). Validating
                    // the whole remaining document per character made string
                    // decoding quadratic (PR #6 round-2 F17).
                    let len = match b0 {
                        0x00..=0x7f => 1,
                        0xc0..=0xdf => 2,
                        0xe0..=0xef => 3,
                        _ => 4,
                    };
                    let end = (self.i + len).min(self.b.len());
                    let s = std::str::from_utf8(&self.b[self.i..end]).map_err(|e| e.to_string())?;
                    let ch = s.chars().next().ok_or("unexpected end")?;
                    out.push(ch as u32);
                    self.i += ch.len_utf8();
                }
            }
        }
    }

    fn number(&mut self) -> Result<PyValue, String> {
        let start = self.i;
        if self.b[self.i] == b'-' {
            self.i += 1;
        }
        let int_start = self.i;
        if self.i < self.b.len() && self.b[self.i] == b'0' {
            self.i += 1;
        } else if self.i < self.b.len() && self.b[self.i].is_ascii_digit() {
            while self.i < self.b.len() && self.b[self.i].is_ascii_digit() {
                self.i += 1;
            }
        } else {
            return Err(format!("Expecting value: char {start}"));
        }
        let int_end = self.i;
        let mut is_float = false;
        if self.i < self.b.len() && self.b[self.i] == b'.' {
            let save = self.i;
            self.i += 1;
            if self.i < self.b.len() && self.b[self.i].is_ascii_digit() {
                while self.i < self.b.len() && self.b[self.i].is_ascii_digit() {
                    self.i += 1;
                }
                is_float = true;
            } else {
                self.i = save;
            }
        }
        if self.i < self.b.len() && (self.b[self.i] == b'e' || self.b[self.i] == b'E') {
            let save = self.i;
            self.i += 1;
            if self.i < self.b.len() && (self.b[self.i] == b'+' || self.b[self.i] == b'-') {
                self.i += 1;
            }
            if self.i < self.b.len() && self.b[self.i].is_ascii_digit() {
                while self.i < self.b.len() && self.b[self.i].is_ascii_digit() {
                    self.i += 1;
                }
                is_float = true;
            } else {
                self.i = save;
            }
        }
        let text = std::str::from_utf8(&self.b[start..self.i]).map_err(|e| e.to_string())?;
        if is_float {
            // Python `float(text)`: overflow → inf.
            let f: f64 = text.parse().map_err(|e| format!("{e}"))?;
            Ok(PyValue::Float(f))
        } else {
            let digits =
                std::str::from_utf8(&self.b[int_start..int_end]).map_err(|e| e.to_string())?;
            let negative = self.b[start] == b'-' && digits != "0";
            Ok(PyValue::Int(if negative {
                format!("-{digits}")
            } else {
                digits.to_string()
            }))
        }
    }
}

// ---- json.dumps / str() / repr() -------------------------------------------------

/// CPython `float.__repr__` (shortest round-trip digits; scientific
/// notation when the decimal exponent is < -4 or >= 16; exponent at least
/// two digits with an explicit sign; `.0` appended to integral values).
fn py_float_repr(x: f64) -> String {
    if x.is_nan() {
        return "nan".to_string();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    // Rust's `{:e}` prints the shortest round-trip mantissa: `1.5e-7`,
    // `1e16`, `-0e0`.
    let sci = format!("{x:e}");
    let (mant, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let negative = mant.starts_with('-');
    let digits: String = mant.chars().filter(|c| c.is_ascii_digit()).collect();
    let decpt = exp + 1; // position of the decimal point relative to `digits`
    let body = if decpt <= -4 || decpt > 16 {
        let mut m = String::new();
        m.push_str(&digits[..1]);
        if digits.len() > 1 {
            m.push('.');
            m.push_str(&digits[1..]);
        }
        let e = decpt - 1;
        format!("{m}e{}{:02}", if e < 0 { '-' } else { '+' }, e.abs())
    } else if decpt <= 0 {
        format!("0.{}{}", "0".repeat((-decpt) as usize), digits)
    } else if (decpt as usize) >= digits.len() {
        format!("{}{}.0", digits, "0".repeat(decpt as usize - digits.len()))
    } else {
        format!(
            "{}.{}",
            &digits[..decpt as usize],
            &digits[decpt as usize..]
        )
    };
    if negative {
        format!("-{body}")
    } else {
        body
    }
}

/// `json.dumps` float rendering: `repr`, except non-finite values become
/// the `NaN`/`Infinity`/`-Infinity` literals (`allow_nan=True` default).
fn json_float(x: f64) -> String {
    if x.is_nan() {
        "NaN".into()
    } else if x.is_infinite() {
        if x > 0.0 {
            "Infinity".into()
        } else {
            "-Infinity".into()
        }
    } else {
        py_float_repr(x)
    }
}

/// `json.dumps(..., ensure_ascii=True)` string escaping, straight from code
/// points: a lone surrogate renders as its own `\udXXX`, astral characters
/// as a surrogate pair, everything outside printable ASCII as `\uXXXX`.
fn json_escape_into(cps: &[u32], out: &mut String) {
    out.push('"');
    for &cp in cps {
        match cp {
            0x22 => out.push_str("\\\""),
            0x5c => out.push_str("\\\\"),
            0x0a => out.push_str("\\n"),
            0x0d => out.push_str("\\r"),
            0x09 => out.push_str("\\t"),
            0x08 => out.push_str("\\b"),
            0x0c => out.push_str("\\f"),
            0x20..=0x7e => out.push(cp as u8 as char),
            cp if cp > 0xFFFF => {
                let v = cp - 0x10000;
                let hi = 0xD800 + (v >> 10);
                let lo = 0xDC00 + (v & 0x3FF);
                out.push_str(&format!("\\u{hi:04x}\\u{lo:04x}"));
            }
            cp => out.push_str(&format!("\\u{cp:04x}")),
        }
    }
    out.push('"');
}

/// `json.dumps(value, sort_keys=..., separators=...)` with `ensure_ascii`.
/// `compact` selects `(",", ":")`; otherwise the default `(", ", ": ")`.
pub fn dumps(v: &PyValue, sort_keys: bool, compact: bool) -> String {
    let mut out = String::new();
    dumps_into(v, sort_keys, compact, &mut out);
    out
}

fn dumps_into(v: &PyValue, sort_keys: bool, compact: bool, out: &mut String) {
    let (item_sep, key_sep) = if compact { (",", ":") } else { (", ", ": ") };
    match v {
        PyValue::Null => out.push_str("null"),
        PyValue::Bool(true) => out.push_str("true"),
        PyValue::Bool(false) => out.push_str("false"),
        PyValue::Int(d) => out.push_str(d),
        PyValue::Float(f) => out.push_str(&json_float(*f)),
        PyValue::Str(s) => json_escape_into(s, out),
        PyValue::List(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(item_sep);
                }
                dumps_into(item, sort_keys, compact, out);
            }
            out.push(']');
        }
        PyValue::Dict(items) => {
            out.push('{');
            let mut order: Vec<&(PyStr, PyValue)> = items.iter().collect();
            if sort_keys {
                // Python compares str by code point — the same order as
                // comparing the code-point vectors.
                order.sort_by(|a, b| a.0.cmp(&b.0));
            }
            for (i, (k, val)) in order.into_iter().enumerate() {
                if i > 0 {
                    out.push_str(item_sep);
                }
                json_escape_into(k, out);
                out.push_str(key_sep);
                dumps_into(val, sort_keys, compact, out);
            }
            out.push('}');
        }
    }
}

/// Python `str.isprintable()` for one code point: false for the general
/// categories Cc, Cf, Cs, Co, Cn, Zl, Zp and Zs — except U+0020, which
/// `repr` keeps as a literal space. Backed by the regex crate's Unicode
/// tables (Unicode 16, the same version as the Python 3.14 reference).
fn py_isprintable(cp: u32) -> bool {
    if cp == 0x20 {
        return true;
    }
    if (0xD800..=0xDFFF).contains(&cp) {
        return false; // Cs
    }
    let Some(ch) = char::from_u32(cp) else {
        return false;
    };
    static NONPRINTABLE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = NONPRINTABLE.get_or_init(|| {
        Regex::new(r"[\p{Cc}\p{Cf}\p{Co}\p{Cn}\p{Zl}\p{Zp}\p{Zs}]").expect("static pattern")
    });
    !re.is_match(ch.encode_utf8(&mut [0u8; 4]))
}

/// CPython `unicode_repr`: single quotes unless the text contains `'` and
/// no `"`; `\\`, the chosen quote, `\n`/`\r`/`\t` escaped; non-printable
/// code points as `\xhh` / `\uhhhh` / `\Uhhhhhhhh`.
fn py_str_repr(cps: &[u32]) -> String {
    let has_sq = cps.contains(&0x27);
    let has_dq = cps.contains(&0x22);
    let quote = if has_sq && !has_dq { '"' } else { '\'' };
    let mut out = String::new();
    out.push(quote);
    for &cp in cps {
        match cp {
            0x5c => out.push_str("\\\\"),
            0x0a => out.push_str("\\n"),
            0x0d => out.push_str("\\r"),
            0x09 => out.push_str("\\t"),
            c if c == quote as u32 => {
                out.push('\\');
                out.push(quote);
            }
            c if py_isprintable(c) => out.push(cp_to_char(c)),
            c if c < 0x100 => out.push_str(&format!("\\x{c:02x}")),
            c if c < 0x10000 => out.push_str(&format!("\\u{c:04x}")),
            c => out.push_str(&format!("\\U{c:08x}")),
        }
    }
    out.push(quote);
    out
}

/// Python `repr(value)`.
fn py_repr(v: &PyValue) -> String {
    match v {
        PyValue::Null => "None".into(),
        PyValue::Bool(true) => "True".into(),
        PyValue::Bool(false) => "False".into(),
        PyValue::Int(d) => d.clone(),
        PyValue::Float(f) => py_float_repr(*f),
        PyValue::Str(s) => py_str_repr(s),
        PyValue::List(items) => {
            let parts: Vec<String> = items.iter().map(py_repr).collect();
            format!("[{}]", parts.join(", "))
        }
        PyValue::Dict(items) => {
            let parts: Vec<String> = items
                .iter()
                .map(|(k, val)| format!("{}: {}", py_str_repr(k), py_repr(val)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
    }
}

/// Python `str(value)`, as a code-point sequence: a `str` is itself; every
/// other type renders via `repr` (containers included — review F4).
fn py_str(v: &PyValue) -> PyStr {
    match v {
        PyValue::Str(s) => s.clone(),
        other => pystr(&py_repr(other)),
    }
}

// ---------------------------------------------------------------------------
// Code-point text with a byte-offset view for the regex engine
// ---------------------------------------------------------------------------

/// A `PyStr` plus its `String` rendering and the two offset maps. All
/// router logic works in code-point offsets (`cps`); the regex engine gets
/// `s` and returns byte offsets, which `cp()` maps back.
pub struct CpText {
    cps: PyStr,
    s: String,
    /// `c2b[i]` = byte offset of code point `i` (len + 1 entries).
    c2b: Vec<usize>,
    /// `b2c[b]` = code-point index of the character starting at byte `b`
    /// (only meaningful at char boundaries; len(s) + 1 entries).
    b2c: Vec<usize>,
}

impl CpText {
    pub fn new(cps: PyStr) -> CpText {
        let mut s = String::with_capacity(cps.len());
        let mut c2b = Vec::with_capacity(cps.len() + 1);
        let mut b2c = Vec::new();
        for (i, &cp) in cps.iter().enumerate() {
            c2b.push(s.len());
            let ch = cp_to_char(cp);
            for _ in 0..ch.len_utf8() {
                b2c.push(i);
            }
            s.push(ch);
        }
        c2b.push(s.len());
        b2c.push(cps.len());
        CpText { cps, s, c2b, b2c }
    }

    fn len(&self) -> usize {
        self.cps.len()
    }

    fn byte(&self, cp: usize) -> usize {
        self.c2b[cp.min(self.cps.len())]
    }

    fn cp(&self, byte: usize) -> usize {
        self.b2c[byte.min(self.s.len())]
    }

    /// `text[a:b]` as `&str` (Python slice semantics: clamped).
    fn slice(&self, a: usize, b: usize) -> &str {
        let a = a.min(self.cps.len());
        let b = b.max(a).min(self.cps.len());
        &self.s[self.c2b[a]..self.c2b[b]]
    }
}

/// `text.find(sub, start)` on code points; `None` for -1.
fn find_sub(text: &[u32], sub: &[u32], start: usize) -> Option<usize> {
    if sub.is_empty() {
        return Some(start.min(text.len()));
    }
    if sub.len() > text.len() {
        return None;
    }
    (start..=text.len() - sub.len()).find(|&i| &text[i..i + sub.len()] == sub)
}

fn find_char(text: &[u32], ch: char, start: usize) -> Option<usize> {
    let c = ch as u32;
    (start..text.len()).find(|&i| text[i] == c)
}

fn starts_with_at(text: &[u32], sub: &str, at: usize) -> bool {
    let sub = pystr(sub);
    at + sub.len() <= text.len() && text[at..at + sub.len()] == sub[..]
}

fn is_ch(cp: u32, ch: char) -> bool {
    cp == ch as u32
}

fn in_set(cp: u32, set: &str) -> bool {
    set.chars().any(|c| c as u32 == cp)
}

/// Python `str.isalnum()` for one code point: general categories L* or N*.
fn py_isalnum(cp: u32) -> bool {
    let Some(ch) = char::from_u32(cp) else {
        return false;
    };
    static ALNUM: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = ALNUM.get_or_init(|| Regex::new(r"^[\p{L}\p{N}]$").expect("static pattern"));
    re.is_match(ch.encode_utf8(&mut [0u8; 4]))
}

// ---------------------------------------------------------------------------
// Regex semantics shared with the Python reference (review F13)
// ---------------------------------------------------------------------------

/// Rewrite a Python `re` pattern (as compiled by the reference with
/// `re.MULTILINE | re.ASCII`) into `regex` syntax with identical meaning.
///
/// The shared semantics are the ASCII ones: `\w` = `[0-9A-Za-z_]`, `\d` =
/// `[0-9]`, `\s` = `[\t\n\v\f\r ]`, `\b`/`\B` boundaries between those word
/// characters and anything else, and case-insensitivity (`(?i)`) folding
/// ASCII letters only. Python's `re.ASCII` gives exactly that; the `regex`
/// crate's Unicode defaults do not (its `\w` includes combining marks, its
/// `(?i)` folds `K` to the Kelvin sign), so every class is spelled out.
/// `\Z` becomes `\z`; a literal `{` that is not a quantifier and a literal
/// `[`/`&`/`~` inside a class are escaped; lookaround and backreferences
/// are rejected (neither exists in the `regex` crate).
pub fn translate_py_regex(pat: &str) -> Result<String, String> {
    let chars: Vec<char> = pat.chars().collect();
    let n = chars.len();
    let mut i = 0;
    let mut out = String::with_capacity(pat.len() + 16);
    let mut ci = false; // ASCII case-insensitive expansion (leading `(?i)`)
    if pat.starts_with("(?i)") {
        ci = true;
        i = 4;
    }
    let ws_class = "\\t\\n\\x0b\\x0c\\r ";
    let word_class = "0-9A-Za-z_";
    let nonword_ranges = "\\x00-\\x2f\\x3a-\\x40\\x5b-\\x5e\\x60\\x7b-\\x{10FFFF}";
    let nondigit_ranges = "\\x00-\\x2f\\x3a-\\x{10FFFF}";
    let nonspace_ranges = "\\x00-\\x08\\x0e-\\x1f\\x21-\\x{10FFFF}";
    let mut in_class = false;
    let mut class_first = false; // right after `[` or `[^`
    while i < n {
        let c = chars[i];
        if in_class {
            if c == '\\' {
                let e = *chars.get(i + 1).ok_or("pattern ends with a backslash")?;
                i += 2;
                match e {
                    'w' => out.push_str(word_class),
                    'W' => out.push_str(nonword_ranges),
                    'd' => out.push_str("0-9"),
                    'D' => out.push_str(nondigit_ranges),
                    's' => out.push_str(ws_class),
                    'S' => out.push_str(nonspace_ranges),
                    'b' => out.push_str("\\x08"),
                    'u' => {
                        let hex: String = chars[i..(i + 4).min(n)].iter().collect();
                        i += 4;
                        push_hex_escape(&mut out, &hex, 4, ci, true);
                    }
                    'U' => {
                        let hex: String = chars[i..(i + 8).min(n)].iter().collect();
                        i += 8;
                        push_hex_escape(&mut out, &hex, 8, ci, true);
                    }
                    'A' | 'Z' | 'B' => return Err(format!("bad escape \\{e} inside a class")),
                    'x' => {
                        let hex: String = chars[i..(i + 2).min(n)].iter().collect();
                        i += 2;
                        push_hex_escape(&mut out, &hex, 2, ci, true);
                    }
                    other => {
                        out.push('\\');
                        out.push(other);
                        if ci && other.is_ascii_alphabetic() && !"ntrfva".contains(other) {
                            out.push_str(&other.to_ascii_uppercase().to_string());
                        }
                    }
                }
                class_first = false;
                continue;
            }
            if c == ']' && !class_first {
                in_class = false;
                out.push(']');
                i += 1;
                continue;
            }
            class_first = false;
            match c {
                '[' => out.push_str("\\["),
                '&' => out.push_str("\\&"),
                '~' => out.push_str("\\~"),
                c if ci && c.is_ascii_alphabetic() => {
                    // `a-z` / `A-Z` ranges get their counterpart; single
                    // letters get the other case.
                    if i + 2 < n && chars[i + 1] == '-' && chars[i + 2].is_ascii_alphabetic() {
                        let lo = c;
                        let hi = chars[i + 2];
                        out.push(lo);
                        out.push('-');
                        out.push(hi);
                        out.push(swap_case(lo));
                        out.push('-');
                        out.push(swap_case(hi));
                        i += 3;
                        continue;
                    }
                    out.push(c);
                    out.push(swap_case(c));
                }
                c => out.push(c),
            }
            i += 1;
            continue;
        }
        match c {
            '\\' => {
                let e = *chars.get(i + 1).ok_or("pattern ends with a backslash")?;
                i += 2;
                match e {
                    'b' => out.push_str("(?-u:\\b)"),
                    'B' => out.push_str("(?-u:\\B)"),
                    'w' => out.push_str(&format!("[{word_class}]")),
                    'W' => out.push_str(&format!("[^{word_class}]")),
                    'd' => out.push_str("[0-9]"),
                    'D' => out.push_str("[^0-9]"),
                    's' => out.push_str(&format!("[{ws_class}]")),
                    'S' => out.push_str(&format!("[^{ws_class}]")),
                    'Z' => out.push_str("\\z"),
                    'A' => out.push_str("\\A"),
                    'u' => {
                        let hex: String = chars[i..(i + 4).min(n)].iter().collect();
                        i += 4;
                        push_hex_escape(&mut out, &hex, 4, ci, false);
                    }
                    'U' => {
                        let hex: String = chars[i..(i + 8).min(n)].iter().collect();
                        i += 8;
                        push_hex_escape(&mut out, &hex, 8, ci, false);
                    }
                    '0' => out.push_str("\\x00"),
                    'x' => {
                        let hex: String = chars[i..(i + 2).min(n)].iter().collect();
                        i += 2;
                        push_hex_escape(&mut out, &hex, 2, ci, false);
                    }
                    d if d.is_ascii_digit() => {
                        return Err(format!("backreference \\{d} is not supported"))
                    }
                    other => {
                        if ci && other.is_ascii_alphabetic() && !"ntrfvax".contains(other) {
                            return Err(format!("unsupported escape \\{other}"));
                        }
                        out.push('\\');
                        out.push(other);
                    }
                }
            }
            '[' => {
                in_class = true;
                class_first = true;
                out.push('[');
                i += 1;
                if i < n && chars[i] == '^' {
                    out.push('^');
                    i += 1;
                }
                // A leading `]` is a literal in Python; escape it for regex.
                if i < n && chars[i] == ']' {
                    out.push_str("\\]");
                    i += 1;
                    class_first = false;
                }
            }
            '(' => {
                if i + 1 < n && chars[i + 1] == '?' {
                    let rest: String = chars[i..(i + 4).min(n)].iter().collect();
                    if rest.starts_with("(?=")
                        || rest.starts_with("(?!")
                        || rest.starts_with("(?<=")
                        || rest.starts_with("(?<!")
                    {
                        return Err("lookaround is not supported".into());
                    }
                    if rest.starts_with("(?P=") || rest.starts_with("(?#") {
                        return Err(format!("unsupported group syntax {rest}"));
                    }
                }
                out.push('(');
                i += 1;
            }
            '{' => {
                // Keep a well-formed quantifier `{n}`, `{n,}`, `{n,m}`;
                // escape any other `{` (a literal in Python, an error in
                // `regex`).
                let mut j = i + 1;
                let mut ok = false;
                let mut saw_num = false;
                while j < n && chars[j].is_ascii_digit() {
                    j += 1;
                    saw_num = true;
                }
                if saw_num {
                    if j < n && chars[j] == '}' {
                        ok = true;
                    } else if j < n && chars[j] == ',' {
                        j += 1;
                        while j < n && chars[j].is_ascii_digit() {
                            j += 1;
                        }
                        if j < n && chars[j] == '}' {
                            ok = true;
                        }
                    }
                }
                if ok {
                    out.extend(chars[i..=j].iter());
                    i = j + 1;
                } else {
                    out.push_str("\\{");
                    i += 1;
                }
            }
            '}' => {
                out.push_str("\\}");
                i += 1;
            }
            c if ci && c.is_ascii_alphabetic() => {
                out.push('[');
                out.push(c);
                out.push(swap_case(c));
                out.push(']');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    if in_class {
        return Err("unterminated character set".into());
    }
    Ok(out)
}

/// Emit a decoded `\x`/`\u`/`\U` escape. Under a leading `(?i)`, an escape
/// that names an ASCII letter gets the same case expansion as a literal
/// letter (PR #10 round-3 F16: Python's `(?i)\x61` matches `A`; so must the
/// translation, outside and inside a class).
fn push_hex_escape(out: &mut String, hex: &str, width: usize, ci: bool, in_class: bool) {
    if ci && hex.len() == width {
        if let Some(ch) = u32::from_str_radix(hex, 16)
            .ok()
            .and_then(char::from_u32)
            .filter(|c| c.is_ascii_alphabetic())
        {
            if !in_class {
                out.push('[');
            }
            out.push(ch);
            out.push(swap_case(ch));
            if !in_class {
                out.push(']');
            }
            return;
        }
    }
    if width == 2 {
        out.push_str(&format!("\\x{hex}"));
    } else {
        out.push_str(&format!("\\x{{{hex}}}"));
    }
}

fn swap_case(c: char) -> char {
    if c.is_ascii_lowercase() {
        c.to_ascii_uppercase()
    } else {
        c.to_ascii_lowercase()
    }
}

/// Compile a Python-syntax pattern with `re.MULTILINE | re.ASCII`
/// semantics.
fn build_regex(py_pattern: &str) -> Result<Regex, String> {
    let translated = translate_py_regex(py_pattern)?;
    RegexBuilder::new(&translated)
        .multi_line(true)
        .build()
        .map_err(|e| e.to_string())
}

/// The anchored twin of `build_regex` — Python's `pattern.match(text, pos)`
/// is `anchored.find(&text[pos..])`.
fn build_anchored(py_pattern: &str) -> Result<Regex, String> {
    let translated = translate_py_regex(py_pattern)?;
    RegexBuilder::new(&format!("\\A(?:{translated})"))
        .multi_line(true)
        .build()
        .map_err(|e| e.to_string())
}

fn static_re(py_pattern: &str) -> Regex {
    build_regex(py_pattern).expect("static router pattern must compile")
}

fn static_anchored(py_pattern: &str) -> Regex {
    build_anchored(py_pattern).expect("static router pattern must compile")
}

// ---------------------------------------------------------------------------
// Shared secret-redaction policy (F7) — the reference's `_REDACT_PATTERNS`
// ---------------------------------------------------------------------------

/// `(python pattern, replacement)` pairs, byte-identical to the reference's
/// `_REDACT_PATTERNS` (which is itself duplicated verbatim into
/// `posttoolusefailure-incident.py`; the Python test suite pins that copy).
/// Replacements use Python's `\1` group syntax and are rewritten to `$1`.
const REDACT_PATTERNS: &[(&str, &str)] = &[
    (r"sk-ant-[A-Za-z0-9\-_]{8,}", "sk-ant-***REDACTED***"),
    (r"sk-[A-Za-z0-9\-_]{8,}", "sk-***REDACTED***"),
    (r"ghp_[A-Za-z0-9]{16,}", "***REDACTED-GH-TOKEN***"),
    (r"github_pat_[A-Za-z0-9_]{16,}", "***REDACTED-GH-TOKEN***"),
    (r"xox[abp]-[A-Za-z0-9\-]{8,}", "***REDACTED-SLACK-TOKEN***"),
    (r"AKIA[A-Z0-9]{16}", "***REDACTED-AWS-KEY***"),
    (r"(?i)\bpit-[A-Za-z0-9\-_]{8,}", "pit-***REDACTED***"),
    (r"(?i)bearer\s+\S+", "Bearer ***REDACTED***"),
    (
        r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z0-9 ]*PRIVATE KEY-----",
        "***REDACTED-PEM-BLOCK***",
    ),
    (
        r#"(?i)"(password|token|secret|api[_-]?key)\s*=\s*(?:[^"\\]|\\[\s\S])*""#,
        r#""\1=***REDACTED***""#,
    ),
    (
        r"(?i)'(password|token|secret|api[_-]?key)\s*=\s*[^']*'",
        r"'\1=***REDACTED***'",
    ),
    (
        r"(?i)\b(password|token|secret|api[_-]?key)\s*=\s*\$'(?:[^'\\]|\\.)*'",
        r"\1=***REDACTED***",
    ),
    (
        r#"(?i)\b(password|token|secret|api[_-]?key)\s*=\s*(\\"(?:[^"\\]|\\[\s\S])*\\"|"(?:[^"\\]|\\[\s\S])*"|'[^']*'|(?:[^\s\\]|\\.)+)"#,
        r"\1=***REDACTED***",
    ),
    (
        r#"(?i)"(password|token|secret|api[_-]?key)"\s*:\s*"(?:[^"\\]|\\[\s\S])*""#,
        r#""\1":"***REDACTED***""#,
    ),
    (
        r#"(?i)(\\+)"(password|token|secret|api[_-]?key)\\+"\s*:\s*\\+"(?:[^"\\]|\\[\s\S])*\\+""#,
        r#"\1"\2\1":\1"***REDACTED***\1""#,
    ),
];

/// One piece of a parsed Python replacement template (`\1` = group 1).
enum ReplPiece {
    Lit(PyStr),
    Group(usize),
}

fn redact_patterns() -> &'static [(Regex, Vec<ReplPiece>)] {
    static COMPILED: std::sync::OnceLock<Vec<(Regex, Vec<ReplPiece>)>> = std::sync::OnceLock::new();
    COMPILED.get_or_init(|| {
        REDACT_PATTERNS
            .iter()
            .map(|(pat, repl)| {
                let re = static_re(pat);
                let mut pieces: Vec<ReplPiece> = Vec::new();
                let mut lit: PyStr = Vec::new();
                let mut chars = repl.chars().peekable();
                while let Some(c) = chars.next() {
                    if c == '\\' {
                        if let Some(d) = chars.peek().copied().filter(|d| d.is_ascii_digit()) {
                            chars.next();
                            if !lit.is_empty() {
                                pieces.push(ReplPiece::Lit(std::mem::take(&mut lit)));
                            }
                            pieces.push(ReplPiece::Group(d.to_digit(10).unwrap_or(0) as usize));
                            continue;
                        }
                    }
                    lit.push(c as u32);
                }
                if !lit.is_empty() {
                    pieces.push(ReplPiece::Lit(lit));
                }
                (re, pieces)
            })
            .collect()
    })
}

/// Scrub every known secret shape out of `cps` (Python `redact`): a no-op
/// for empty input. Matching runs on the `String` view, but the output is
/// rebuilt from the ORIGINAL code points using the match/group offsets —
/// never converted back from the view — so lone surrogates and genuine
/// private-use characters both survive exactly (PR #6 round-2 F14).
fn redact_cps(cps: &[u32]) -> PyStr {
    if cps.is_empty() {
        return Vec::new();
    }
    let mut cur: PyStr = cps.to_vec();
    for (re, template) in redact_patterns() {
        let view = CpText::new(cur.clone());
        let mut out: PyStr = Vec::new();
        let mut last = 0usize;
        let mut matched_any = false;
        for caps in re.captures_iter(&view.s) {
            matched_any = true;
            let m = caps.get(0).expect("group 0");
            let (ms, me) = (view.cp(m.start()), view.cp(m.end()));
            out.extend_from_slice(&cur[last..ms]);
            for piece in template {
                match piece {
                    ReplPiece::Lit(lit) => out.extend_from_slice(lit),
                    // An unmatched group expands to nothing (Python ≥ 3.5).
                    ReplPiece::Group(n) => {
                        if let Some(g) = caps.get(*n) {
                            out.extend_from_slice(&cur[view.cp(g.start())..view.cp(g.end())]);
                        }
                    }
                }
            }
            last = me;
        }
        if !matched_any {
            continue;
        }
        out.extend_from_slice(&cur[last..]);
        cur = out;
    }
    cur
}

/// `redact()` on a Python value the way the reference calls it on a payload
/// field: a falsy value is returned as-is (`if not text: return text`), a
/// truthy non-`str` raises `TypeError` in `pattern.sub` — fail-open.
fn redact_value(v: &PyValue) -> Result<PyValue, String> {
    if !v.truthy() {
        return Ok(v.clone());
    }
    match v {
        PyValue::Str(s) => Ok(PyValue::Str(redact_cps(s))),
        other => Err(format!(
            "expected string or bytes-like object, got '{}' (Python: TypeError in redact)",
            other.type_name()
        )),
    }
}

// ---------------------------------------------------------------------------
// Shared Bash-rule prefix normalization (F3, F9) — see the reference for the
// full history of each fragment.
// ---------------------------------------------------------------------------

const SEPARATOR_CHARS: &str = ";&|(){}\n";
const RUN_CHAR: &str = r#"(?:[^\s'"$]|\$[^'"\s])"#;
const RESERVED_LEADIN: &str = r"(?:if|then|elif|else|while|until|do)\s+";
const GIT_GLOBAL_OPTS: &str = r"(?:(?:-C\s+\S+|-c\s+\S+|--git-dir=\S+|--work-tree=\S+)\s+)*";
const GIT_OPT_TAKING_ARG: [&str; 4] = ["-C", "-c", "--git-dir=", "--work-tree="];
const ASSIGN_QUOTE_LOOKBACK: usize = 64;
/// A fixed, scanner-emitted marker meaning "some unquoted heredoc body in
/// this command contains a live backtick" (reference
/// `_HEREDOC_BACKTICK_MARKER`).
const HEREDOC_BACKTICK_MARKER: u32 = 0x02;

fn assign_value() -> String {
    format!(
        "{RUN_CHAR}*(?:(?:'[^']*'|\"(?:[^\"\\\\]|\\\\[\\s\\S])*\"|\\$'(?:[^'\\\\]|\\\\.)*'){RUN_CHAR}*)*"
    )
}

fn assign() -> String {
    format!("[A-Za-z_][A-Za-z0-9_]*={}\\$?", assign_value())
}

fn wrapper_skip() -> String {
    let a = assign();
    format!("(?:(?:{a}\\s+)*(?:time|env|command|exec|sudo|builtin)\\s+)*(?:{a}\\s+)*")
}

fn cmd_prefix() -> String {
    format!(
        "(?:^|[;&|({{]\\s*|\\$\\(\\s*|`\\s*|\\n\\s*|{RESERVED_LEADIN})\\s*{}",
        wrapper_skip()
    )
}

fn expand_placeholders(pattern: &str) -> String {
    pattern
        .replace("@PREFIX@", &cmd_prefix())
        .replace("@GITOPTS@", GIT_GLOBAL_OPTS)
}

struct StaticRes {
    assign_quote_prefix: Regex, // search on the lookback slice
    heredoc_start: Regex,       // anchored
    dash_c_locate: Regex,
    git_dir_locate: Regex,
    cd_locate: Regex,
    or_guard: Regex, // anchored
    loop_token: Regex,
    tail_word: Regex,
}

fn statics() -> &'static StaticRes {
    static S: std::sync::OnceLock<StaticRes> = std::sync::OnceLock::new();
    S.get_or_init(|| StaticRes {
        assign_quote_prefix: static_re(r#"[A-Za-z_][A-Za-z0-9_]*=\$?(?:[^\s'"]*)\Z"#),
        heredoc_start: static_anchored(
            r#"<<(-)?\s*(?:'([^'\n]*)'|"([^"\n]*)"|([A-Za-z_][A-Za-z0-9_]*))"#,
        ),
        dash_c_locate: static_re(r"-C\s"),
        git_dir_locate: static_re(r"--git-dir="),
        cd_locate: static_re(&format!(
            "(?:^|[;&|(){{}}\\n]\\s*|{RESERVED_LEADIN}){}(cd|pushd|popd)\\b",
            wrapper_skip()
        )),
        or_guard: static_anchored(r"[ \t]*\|\|"),
        loop_token: static_re(r"\b(?:do|done)\b"),
        tail_word: static_re(r"\w+\Z"),
    })
}

// ---------------------------------------------------------------------------
// Quote/live span bookkeeping and the git-global-option widening (F2/F3)
// ---------------------------------------------------------------------------

type Span = (usize, usize);

/// Every quoted span and every live substitution body the scanner walked
/// past, in code-point offsets (reference `quote_spans` / `live_spans`).
#[derive(Default, Debug)]
pub struct Spans {
    pub quote: Vec<Span>,
    pub live: Vec<Span>,
}

fn innermost_span_is_live(position: usize, spans: &Spans) -> (i64, bool) {
    let mut innermost_start: i64 = -1;
    let mut innermost_is_live = false;
    for &(q_start, q_end) in &spans.quote {
        if q_start <= position && position < q_end && (q_start as i64) > innermost_start {
            innermost_start = q_start as i64;
            innermost_is_live = false;
        }
    }
    for &(l_start, l_end) in &spans.live {
        if l_start <= position && position < l_end && (l_start as i64) > innermost_start {
            innermost_start = l_start as i64;
            innermost_is_live = true;
        }
    }
    (innermost_start, innermost_is_live)
}

fn find_gitopt_quote_targets(text: &[u32], quote_spans: &[Span]) -> Vec<Span> {
    let mut targets = Vec::new();
    for opt in GIT_OPT_TAKING_ARG {
        let opt_cps = pystr(opt);
        let mut search_from = 0;
        while let Some(idx) = find_sub(text, &opt_cps, search_from) {
            search_from = idx + opt_cps.len();
            if idx > 0 && (py_isalnum(text[idx - 1]) || in_set(text[idx - 1], "-_")) {
                continue;
            }
            let mut j = idx + opt_cps.len();
            if opt == "-C" || opt == "-c" {
                while j < text.len() && in_set(text[j], " \t") {
                    j += 1;
                }
            }
            for &(q_start, q_end) in quote_spans {
                if q_start == j {
                    targets.push((q_start, q_end));
                    break;
                }
            }
        }
    }
    targets
}

fn widen_targets(scan_text: &[u32], targets: &[Span]) -> PyStr {
    let mut out = scan_text.to_vec();
    for &(q_start, q_end) in targets {
        for k in q_start..q_end.min(out.len()) {
            if out[k] == ' ' as u32 {
                out[k] = 0x01;
            }
        }
    }
    out
}

fn gitopts_scan_variants(text: &[u32], scan_text: &[u32], quote_spans: &[Span]) -> Vec<PyStr> {
    let targets = find_gitopt_quote_targets(text, quote_spans);
    // Group by ancestor set, preserving first-seen group order (Python dict).
    let mut groups: Vec<(Vec<usize>, Vec<Span>)> = Vec::new();
    for (i, t) in targets.iter().enumerate() {
        let mut key: Vec<usize> = targets
            .iter()
            .enumerate()
            .filter(|(j, other)| *j != i && other.0 <= t.0 && t.1 <= other.1)
            .map(|(j, _)| j)
            .collect();
        key.sort_unstable();
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some(g) => g.1.push(*t),
            None => groups.push((key, vec![*t])),
        }
    }
    groups
        .iter()
        .map(|(_, group)| widen_targets(scan_text, group))
        .collect()
}

// ---------------------------------------------------------------------------
// Executable-region scanner (F2, F13) — see the reference for the design
// notes on every branch; the logic here is a 1:1 port.
// ---------------------------------------------------------------------------

fn is_assignment_value_quote(text: &CpText, quote_idx: usize) -> bool {
    let lo = quote_idx.saturating_sub(ASSIGN_QUOTE_LOOKBACK);
    statics()
        .assign_quote_prefix
        .is_match(text.slice(lo, quote_idx))
}

fn single_quote_span_end(text: &[u32], i: usize, limit: usize) -> usize {
    if i > 0 && is_ch(text[i - 1], '$') {
        let mut j = i + 1;
        while j < limit {
            let ch = text[j];
            if is_ch(ch, '\\') && j + 1 < limit {
                j += 2;
                continue;
            }
            if is_ch(ch, '\'') {
                return j + 1;
            }
            j += 1;
        }
        return limit;
    }
    match find_char(text, '\'', i + 1) {
        Some(j) if j < limit => j + 1,
        _ => limit,
    }
}

/// One matched heredoc opener: `(end_cp, delim, quoted, strip_tabs)`.
struct HeredocStart {
    end: usize,
    delim: PyStr,
    quoted: bool,
    strip_tabs: bool,
}

fn heredoc_start_match(text: &CpText, pos: usize) -> Option<HeredocStart> {
    let hay = text.slice(pos, text.len());
    let caps = statics().heredoc_start.captures(hay)?;
    let m = caps.get(0)?;
    let end = text.cp(text.byte(pos) + m.end());
    let strip_tabs = caps.get(1).is_some();
    // The delimiter is sliced from the ORIGINAL code points via the capture
    // offsets, never taken from the `String` view (PR #6 round-2 F14): a
    // lone-surrogate delimiter would otherwise come back as its private-use
    // placeholder and never match its own terminator line.
    let base = text.byte(pos);
    let slice_cps =
        |g: regex::Match<'_>| text.cps[text.cp(base + g.start())..text.cp(base + g.end())].to_vec();
    let (delim, quoted) = if let Some(g) = caps.get(2) {
        (slice_cps(g), true)
    } else if let Some(g) = caps.get(3) {
        (slice_cps(g), true)
    } else {
        let g = caps.get(4)?;
        // Unquoted delimiter immediately followed by a backslash: not
        // resolvable by this scanner — don't treat as a heredoc opener.
        if end < text.len() && is_ch(text.cps[end], '\\') {
            return None;
        }
        (slice_cps(g), false)
    };
    Some(HeredocStart {
        end,
        delim,
        quoted,
        strip_tabs,
    })
}

fn is_comment_start(text: &[u32], i: usize) -> bool {
    if i == 0 {
        return true;
    }
    let prev = text[i - 1];
    if is_ch(prev, '{') && i >= 2 && is_ch(text[i - 2], '$') {
        return false;
    }
    in_set(prev, " \t\n;&|(){}")
}

fn find_matching_paren(text: &[u32], open_idx: usize) -> (usize, bool) {
    let mut depth: i64 = 0;
    let n = text.len();
    let mut i = open_idx;
    while i < n {
        let ch = text[i];
        if is_ch(ch, '#') && is_comment_start(text, i) {
            i = find_char(text, '\n', i).unwrap_or(n);
            continue;
        }
        if is_ch(ch, '\\') && i + 1 < n {
            i += 2;
            continue;
        }
        if is_ch(ch, '\'') {
            i = single_quote_span_end(text, i, n);
            continue;
        }
        if is_ch(ch, '"') {
            i = skip_double_quoted(text, i);
            continue;
        }
        if is_ch(ch, '`') {
            i = find_char(text, '`', i + 1).map(|j| j + 1).unwrap_or(n);
            continue;
        }
        if is_ch(ch, '(') {
            depth += 1;
        } else if is_ch(ch, ')') {
            depth -= 1;
            if depth == 0 {
                return (i + 1, true);
            }
        }
        i += 1;
    }
    (n, false)
}

fn skip_double_quoted(text: &[u32], start: usize) -> usize {
    let n = text.len();
    let mut i = start + 1;
    while i < n {
        let ch = text[i];
        if is_ch(ch, '\\') && i + 1 < n {
            i += 2;
            continue;
        }
        if is_ch(ch, '"') {
            return i + 1;
        }
        if is_ch(ch, '$') && i + 1 < n && is_ch(text[i + 1], '(') {
            i = find_matching_paren(text, i + 1).0;
            continue;
        }
        if is_ch(ch, '`') {
            i = find_char(text, '`', i + 1).map(|j| j + 1).unwrap_or(n);
            continue;
        }
        i += 1;
    }
    n
}

fn mask_literal_span(
    text: &[u32],
    start: usize,
    end: usize,
    result: &mut [u32],
    quote_char: char,
    mask_delims: bool,
) {
    for k in start..end.min(text.len()) {
        let ch = text[k];
        if is_ch(ch, quote_char) && !mask_delims {
            continue;
        }
        if is_ch(ch, quote_char) || in_set(ch, SEPARATOR_CHARS) || is_ch(ch, '`') {
            result[k] = ' ' as u32;
        }
    }
}

fn mask_span_preserving_substitutions(
    text: &CpText,
    start: usize,
    end: usize,
    result: &mut Vec<u32>,
    spans: &mut Spans,
) {
    let t = &text.cps;
    let mut i = start;
    while i < end {
        let ch = t[i];
        if is_ch(ch, '$') && i + 1 < end && is_ch(t[i + 1], '(') {
            let (raw_close, terminated) = find_matching_paren(t, i + 1);
            let (close, body_end) = if terminated && raw_close <= end {
                (raw_close, raw_close - 1)
            } else {
                let c = raw_close.min(end);
                (c, c)
            };
            mask_quotes_recursive(text, i + 2, body_end, result, spans);
            i = close;
            continue;
        }
        if is_ch(ch, '`') {
            let j = find_char(t, '`', i + 1);
            let (close, body_end) = match j {
                Some(j) if j < end => {
                    result[j] = ' ' as u32;
                    (j + 1, j)
                }
                _ => (end, end),
            };
            mask_quotes_recursive(text, i + 1, body_end, result, spans);
            i = close;
            continue;
        }
        if !is_ch(ch, '\n') {
            result[i] = ' ' as u32;
        }
        i += 1;
    }
}

fn mask_quotes_recursive(
    text: &CpText,
    start: usize,
    end: usize,
    result: &mut Vec<u32>,
    spans: &mut Spans,
) {
    let t = &text.cps;
    let mut i = start;
    let mut pending_heredoc: Option<(PyStr, bool, bool)> = None;
    while i < end {
        let ch = t[i];
        if is_ch(ch, '\\') && i + 1 < end {
            result[i] = ' ' as u32;
            result[i + 1] = ' ' as u32;
            i += 2;
            continue;
        }
        if is_ch(ch, '#') && is_comment_start(t, i) {
            let comment_end = match find_char(t, '\n', i) {
                Some(j) if j < end => j,
                _ => end,
            };
            for k in i..comment_end {
                result[k] = ' ' as u32;
            }
            i = comment_end;
            continue;
        }
        if is_ch(ch, '<') && starts_with_at(t, "<<", i) && !starts_with_at(t, "<<<", i) {
            if let Some(m) = heredoc_start_match(text, i) {
                if m.end <= end {
                    pending_heredoc = Some((m.delim, m.quoted, m.strip_tabs));
                    i = m.end;
                    continue;
                }
            }
            i += 1;
            continue;
        }
        if is_ch(ch, '\n') && pending_heredoc.is_some() {
            let (delim, quoted, strip_tabs) = pending_heredoc.take().unwrap();
            let (close, _terminated) = consume_heredoc_body(
                text,
                i + 1,
                &delim,
                quoted,
                strip_tabs,
                result,
                Some(end),
                spans,
            );
            i = close.min(end);
            continue;
        }
        if is_ch(ch, '\'') {
            let start_q = i;
            let close = single_quote_span_end(t, i, end);
            let keep_delims = is_assignment_value_quote(text, i);
            mask_literal_span(t, i, close, result, '\'', !keep_delims);
            spans.quote.push((start_q, close));
            i = close;
            continue;
        }
        if is_ch(ch, '"') {
            let start_q = i;
            let keep_delims = is_assignment_value_quote(text, i);
            i = mask_double_quoted(text, i, result, !keep_delims, spans).min(end);
            spans.quote.push((start_q, i));
            continue;
        }
        if is_ch(ch, '$') && i + 1 < end && is_ch(t[i + 1], '(') {
            let (raw_close, terminated) = find_matching_paren(t, i + 1);
            let (close, body_end) = if terminated && raw_close <= end {
                (raw_close, raw_close - 1)
            } else {
                let c = raw_close.min(end);
                (c, c)
            };
            spans.live.push((i, body_end));
            mask_quotes_recursive(text, i + 2, body_end, result, spans);
            i = close;
            continue;
        }
        if is_ch(ch, '`') {
            let j = find_char(t, '`', i + 1);
            let (close, body_end) = match j {
                Some(j) if j + 1 <= end => {
                    result[j] = ' ' as u32;
                    (j + 1, j)
                }
                Some(j) => {
                    let c = (j + 1).min(end);
                    (c, c)
                }
                None => {
                    let c = t.len().min(end);
                    (c, c)
                }
            };
            spans.live.push((i, body_end));
            mask_quotes_recursive(text, i + 1, body_end, result, spans);
            i = close;
            continue;
        }
        i += 1;
    }
}

fn mask_double_quoted(
    text: &CpText,
    start: usize,
    result: &mut Vec<u32>,
    mask_delims: bool,
    spans: &mut Spans,
) -> usize {
    let t = &text.cps;
    let n = t.len();
    if mask_delims {
        result[start] = ' ' as u32;
    }
    let mut i = start + 1;
    while i < n {
        let ch = t[i];
        if is_ch(ch, '\\') && i + 1 < n {
            result[i] = ' ' as u32;
            result[i + 1] = ' ' as u32;
            i += 2;
            continue;
        }
        if is_ch(ch, '"') {
            if mask_delims {
                result[i] = ' ' as u32;
            }
            return i + 1;
        }
        if is_ch(ch, '$') && i + 1 < n && is_ch(t[i + 1], '(') {
            let (raw_close, terminated) = find_matching_paren(t, i + 1);
            let (close, body_end) = if terminated {
                (raw_close, raw_close - 1)
            } else {
                (raw_close, raw_close)
            };
            spans.live.push((i, body_end));
            mask_quotes_recursive(text, i + 2, body_end, result, spans);
            i = close;
            continue;
        }
        if is_ch(ch, '`') {
            let (close, body_end) = match find_char(t, '`', i + 1) {
                Some(j) => {
                    result[j] = ' ' as u32;
                    (j + 1, j)
                }
                None => (n, n),
            };
            spans.live.push((i, body_end));
            mask_quotes_recursive(text, i + 1, body_end, result, spans);
            i = close;
            continue;
        }
        if in_set(ch, SEPARATOR_CHARS) {
            result[i] = ' ' as u32;
        }
        i += 1;
    }
    n
}

#[allow(clippy::too_many_arguments)]
fn consume_heredoc_body(
    text: &CpText,
    start: usize,
    delim: &[u32],
    quoted: bool,
    strip_tabs: bool,
    result: &mut Vec<u32>,
    end: Option<usize>,
    spans: &mut Spans,
) -> (usize, bool) {
    let t = &text.cps;
    let n = t.len();
    let mut i = start;
    let (body_end, end_index, terminated);
    loop {
        let nl = find_char(t, '\n', i);
        let line_end = nl.unwrap_or(n);
        let line = &t[i.min(n)..line_end.max(i.min(n))];
        let check_line: &[u32] = if strip_tabs {
            let k = line.iter().take_while(|&&c| c == '\t' as u32).count();
            &line[k..]
        } else {
            line
        };
        if check_line == delim {
            body_end = i;
            end_index = match nl {
                None => n,
                Some(nl) => nl + 1,
            };
            terminated = true;
            break;
        }
        match nl {
            None => {
                body_end = n;
                end_index = n;
                terminated = false;
                break;
            }
            Some(nl) => i = nl + 1,
        }
    }
    let mask_limit = match end {
        None => body_end,
        Some(e) => body_end.min(e),
    };
    if quoted {
        for k in start..mask_limit {
            if !is_ch(t[k], '\n') {
                result[k] = ' ' as u32;
            }
        }
    } else {
        mask_span_preserving_substitutions(text, start, mask_limit, result, spans);
        if result[start.min(result.len())..mask_limit.min(result.len())]
            .iter()
            .any(|&c| is_ch(c, '`'))
        {
            result.push(HEREDOC_BACKTICK_MARKER);
        }
    }
    (end_index, terminated)
}

/// Same-length copy of the canonical Bash text with literal quoted text,
/// comments and heredoc bodies masked so rules only ever see EXECUTABLE
/// text (reference `executable_mask`). Records every quoted span and every
/// live substitution body into `spans`.
pub fn executable_mask(text: &CpText, spans: &mut Spans) -> PyStr {
    let t = &text.cps;
    let n = t.len();
    let mut result: Vec<u32> = t.clone();
    let mut i = 0;
    let mut pending_heredocs: std::collections::VecDeque<(PyStr, bool, bool)> =
        std::collections::VecDeque::new();
    let mut in_backtick = false;
    while i < n {
        let ch = t[i];
        if is_ch(ch, '\\') && i + 1 < n {
            result[i] = ' ' as u32;
            result[i + 1] = ' ' as u32;
            i += 2;
            continue;
        }
        if is_ch(ch, '`') {
            if in_backtick {
                result[i] = ' ' as u32;
            }
            in_backtick = !in_backtick;
            i += 1;
            continue;
        }
        if is_ch(ch, '#') && is_comment_start(t, i) {
            let mut end = find_char(t, '\n', i).unwrap_or(n);
            if in_backtick {
                if let Some(bt) = find_char(t, '`', i) {
                    if bt < end {
                        end = bt;
                    }
                }
            }
            for k in i..end {
                result[k] = ' ' as u32;
            }
            i = end;
            continue;
        }
        if is_ch(ch, '\'') {
            let end = single_quote_span_end(t, i, n);
            let keep_delims = is_assignment_value_quote(text, i);
            mask_literal_span(t, i, end, &mut result, '\'', !keep_delims);
            spans.quote.push((i, end));
            i = end;
            continue;
        }
        if is_ch(ch, '"') {
            let start = i;
            let keep_delims = is_assignment_value_quote(text, i);
            i = mask_double_quoted(text, i, &mut result, !keep_delims, spans);
            spans.quote.push((start, i));
            continue;
        }
        if is_ch(ch, '<') && starts_with_at(t, "<<", i) && !starts_with_at(t, "<<<", i) {
            if let Some(m) = heredoc_start_match(text, i) {
                pending_heredocs.push_back((m.delim, m.quoted, m.strip_tabs));
                i = m.end;
                continue;
            }
            i += 1;
            continue;
        }
        if is_ch(ch, '\n') && !pending_heredocs.is_empty() {
            i += 1;
            while let Some((delim, quoted, strip_tabs)) = pending_heredocs.pop_front() {
                let (next, terminated) = consume_heredoc_body(
                    text,
                    i,
                    &delim,
                    quoted,
                    strip_tabs,
                    &mut result,
                    None,
                    spans,
                );
                i = next;
                if !terminated && !quoted {
                    // F8: an unquoted heredoc that never terminates consumes
                    // to EOF; append a synthetic terminator line to
                    // scan_text only.
                    result.push('\n' as u32);
                    result.extend_from_slice(&delim);
                    result.push('\n' as u32);
                }
            }
            continue;
        }
        i += 1;
    }
    result
}

// ---------------------------------------------------------------------------
// Command windows, effective checkout (F4), polling-loop bounds (F3)
// ---------------------------------------------------------------------------

fn bisect_right(values: &[usize], x: usize) -> usize {
    values.partition_point(|&v| v <= x)
}

fn bisect_left(values: &[usize], x: usize) -> usize {
    values.partition_point(|&v| v < x)
}

fn window_bounds(sep_positions: &[usize], text_len: usize, start: usize, end: usize) -> Span {
    let idx = bisect_right(sep_positions, start);
    let window_start = if idx >= 1 {
        sep_positions[idx - 1] + 1
    } else {
        0
    };
    let idx2 = bisect_left(sep_positions, end);
    let window_end = if idx2 < sep_positions.len() {
        sep_positions[idx2]
    } else {
        text_len
    };
    (window_start, window_end)
}

fn looks_like_resolvable_path(token: &[u32]) -> bool {
    if token.is_empty() || is_ch(token[0], '-') {
        return false;
    }
    !token.iter().any(|&c| in_set(c, "$`*~"))
}

/// Read one shell argument token at `pos` in the UNMASKED text (reference
/// `_read_token`): `(value_or_None, end_pos)`.
fn read_token(text: &[u32], pos: usize) -> (Option<PyStr>, usize) {
    let n = text.len();
    // `pos` can lie PAST the unmasked text: `executable_mask` appends a
    // synthetic terminator line for an unterminated heredoc, and a `cd` in
    // that synthetic line is located in scan_text coordinates (PR #6
    // round-2 F18). Python's slicing clamps silently (`text[16:16] == ""`,
    // an unresolvable token); do the same instead of indexing out of range.
    let mut pos = pos.min(n);
    while pos < n {
        if in_set(text[pos], " \t") {
            pos += 1;
            continue;
        }
        if is_ch(text[pos], '\\') && pos + 1 < n && is_ch(text[pos + 1], '\n') {
            pos += 2;
            continue;
        }
        break;
    }
    if pos < n && is_ch(text[pos], '\'') {
        return match find_char(text, '\'', pos + 1) {
            None => (None, n),
            Some(end) => (Some(text[pos + 1..end].to_vec()), end + 1),
        };
    }
    if pos < n && is_ch(text[pos], '"') {
        return match find_char(text, '"', pos + 1) {
            None => (None, n),
            Some(end) => {
                let value = &text[pos + 1..end];
                let v = if value.iter().any(|&c| in_set(c, "$`")) {
                    None
                } else {
                    Some(value.to_vec())
                };
                (v, end + 1)
            }
        };
    }
    let start = pos;
    while pos < n && !in_set(text[pos], " \t\n;&|)") {
        if is_ch(text[pos], '$') && pos + 1 < n && is_ch(text[pos + 1], '(') {
            pos = find_matching_paren(text, pos + 1).0;
            continue;
        }
        if is_ch(text[pos], '`') {
            pos = find_char(text, '`', pos + 1).map(|c| c + 1).unwrap_or(n);
            continue;
        }
        pos += 1;
    }
    let token = &text[start..pos];
    (
        if looks_like_resolvable_path(token) {
            Some(token.to_vec())
        } else {
            None
        },
        pos,
    )
}

const SLASH: u32 = '/' as u32;
const DOT: u32 = '.' as u32;

/// `posixpath.normpath` — purely lexical, on code points (so a path holding
/// a lone surrogate or a private-use character is normalized without ever
/// passing via the placeholder `String` view).
fn posix_normpath(path: &[u32]) -> PyStr {
    if path.is_empty() {
        return vec![DOT];
    }
    let mut initial_slashes = usize::from(path[0] == SLASH);
    if initial_slashes == 1 && path.get(1) == Some(&SLASH) && path.get(2) != Some(&SLASH) {
        initial_slashes = 2;
    }
    let mut new_comps: Vec<&[u32]> = Vec::new();
    for comp in path.split(|&c| c == SLASH) {
        if comp.is_empty() || comp == [DOT] {
            continue;
        }
        let is_dotdot = comp == [DOT, DOT];
        if !is_dotdot
            || (initial_slashes == 0 && new_comps.is_empty())
            || new_comps.last().is_some_and(|l| *l == [DOT, DOT])
        {
            new_comps.push(comp);
        } else if !new_comps.is_empty() {
            new_comps.pop();
        }
    }
    let mut out: PyStr = vec![SLASH; initial_slashes];
    for (i, comp) in new_comps.iter().enumerate() {
        if i > 0 {
            out.push(SLASH);
        }
        out.extend_from_slice(comp);
    }
    if out.is_empty() {
        vec![DOT]
    } else {
        out
    }
}

/// `posixpath.join(a, b)` for two components, on code points.
fn posix_join(a: &[u32], b: &[u32]) -> PyStr {
    if b.first() == Some(&SLASH) {
        b.to_vec()
    } else if a.is_empty() || a.last() == Some(&SLASH) {
        [a, b].concat()
    } else {
        let mut v = a.to_vec();
        v.push(SLASH);
        v.extend_from_slice(b);
        v
    }
}

/// The directory a `cd`/`-C` resolved to, or the payload's raw `cwd` value
/// (which need not be a string — the reference only fails on it when a
/// string operation actually touches it).
type Cwd = PyValue;

/// Reference `_resolve_against_cwd`: absolute → normpath; no usable base →
/// the path itself; a truthy non-str base → `TypeError` (fail-open).
fn resolve_against_cwd(path: &[u32], base: Option<&Cwd>) -> Result<PyStr, String> {
    if path.first() == Some(&SLASH) {
        return Ok(posix_normpath(path));
    }
    let Some(base) = base else {
        return Ok(path.to_vec());
    };
    if !base.truthy() {
        return Ok(path.to_vec());
    }
    match base {
        PyValue::Str(b) => Ok(posix_normpath(&posix_join(b, path))),
        other => Err(format!(
            "expected str, bytes or os.PathLike object, not {} (Python: TypeError in os.path.join)",
            other.type_name()
        )),
    }
}

fn paren_depths(scan_text: &[u32]) -> Vec<i64> {
    let mut depths = vec![0i64; scan_text.len() + 1];
    let mut d = 0i64;
    for (i, &ch) in scan_text.iter().enumerate() {
        if is_ch(ch, '(') {
            d += 1;
        } else if is_ch(ch, ')') {
            d -= 1;
        }
        depths[i + 1] = d;
    }
    depths
}

fn skip_balanced_group(scan_text: &[u32], open_idx: usize) -> usize {
    let open_ch = scan_text[open_idx];
    let close_ch = if is_ch(open_ch, '(') {
        ')' as u32
    } else {
        '}' as u32
    };
    let mut depth = 0i64;
    let n = scan_text.len();
    let mut i = open_idx;
    while i < n {
        let ch = scan_text[i];
        if ch == open_ch {
            depth += 1;
        } else if ch == close_ch {
            depth -= 1;
            if depth == 0 {
                return i + 1;
            }
        }
        i += 1;
    }
    n
}

fn or_operand_end(scan_text: &[u32], start: usize) -> usize {
    let n = scan_text.len();
    let mut i = start;
    while i < n {
        let ch = scan_text[i];
        if in_set(ch, ";&\n(){}") {
            return i;
        }
        if is_ch(ch, '|') {
            if i + 1 < n && is_ch(scan_text[i + 1], '|') {
                return i;
            }
            i += if i + 1 < n && is_ch(scan_text[i + 1], '&') {
                2
            } else {
                1
            };
            continue;
        }
        i += 1;
    }
    n
}

fn next_lower_paren_depth(depths: &[i64]) -> Vec<Option<usize>> {
    let n = depths.len();
    let mut next_lower = vec![None; n];
    let mut stack: Vec<usize> = Vec::new();
    for j in (0..n).rev() {
        let d = depths[j];
        while let Some(&top) = stack.last() {
            if depths[top] >= d {
                stack.pop();
            } else {
                break;
            }
        }
        next_lower[j] = stack.last().copied();
        stack.push(j);
    }
    next_lower
}

/// Per-`cd` reach data (reference `infos` tuples).
#[derive(Clone)]
struct CdInfo {
    resolved: Option<Cwd>,
    guard_end: Option<usize>,
    break_pos: Option<usize>,
    ceiling: Option<usize>,
}

struct CdReach {
    starts: Vec<usize>,
    infos: Vec<CdInfo>,
}

fn base_cwd_before(position: usize, reach: &CdReach, payload_cwd: &Cwd) -> Option<Cwd> {
    let mut idx = bisect_left(&reach.starts, position) as i64 - 1;
    while idx >= 0 {
        let info = &reach.infos[idx as usize];
        if info.guard_end.is_none_or(|g| position >= g)
            && info.break_pos.is_none_or(|b| b > position)
            && info.ceiling.is_none_or(|c| position < c)
        {
            return info.resolved.clone();
        }
        idx -= 1;
    }
    Some(payload_cwd.clone())
}

fn precompute_cd_reach_info(
    text: &CpText,
    scan: &CpText,
    depths: &[i64],
    payload_cwd: &Cwd,
) -> Result<CdReach, String> {
    let mut reach = CdReach {
        starts: Vec::new(),
        infos: Vec::new(),
    };
    let mut operand_windows: Vec<Span> = Vec::new();
    let mut next_lower: Option<Vec<Option<usize>>> = None;
    // Union-find over dead entries (reference `parent`/`_find`).
    let mut parent: std::collections::HashMap<i64, i64> = std::collections::HashMap::new();
    fn find(parent: &mut std::collections::HashMap<i64, i64>, mut idx: i64) -> i64 {
        let mut path = Vec::new();
        while let Some(&p) = parent.get(&idx) {
            path.push(idx);
            idx = p;
        }
        for p in path {
            parent.insert(p, idx);
        }
        idx
    }

    let st = statics();
    for cd_match in st.cd_locate.captures_iter(&scan.s) {
        let whole = cd_match.get(0).expect("group 0");
        let keyword = cd_match.get(1).expect("group 1");
        let token_start = scan.cp(whole.end());
        let (mut value, token_end) = read_token(&text.cps, token_start);
        if keyword.as_str() == "popd" {
            value = None;
        }
        let position = scan.cp(whole.start());

        // `_self_base_cwd_before(position)`
        let base_for_this_cd: Option<Cwd> = {
            let mut idx = find(&mut parent, bisect_left(&reach.starts, position) as i64 - 1);
            let mut found: Option<Option<Cwd>> = None;
            while idx >= 0 {
                let info = &reach.infos[idx as usize];
                let dead_forever = info.break_pos.is_some_and(|b| b <= position)
                    || info.ceiling.is_some_and(|c| c <= position);
                if !dead_forever && info.guard_end.is_none_or(|g| position >= g) {
                    found = Some(info.resolved.clone());
                    break;
                }
                if dead_forever {
                    parent.insert(idx, idx - 1);
                    idx = find(&mut parent, idx - 1);
                } else {
                    idx = find(&mut parent, idx - 1);
                }
            }
            match found {
                Some(r) => r,
                None => Some(payload_cwd.clone()),
            }
        };
        let resolved: Option<Cwd> = match value {
            Some(v) => Some(PyValue::Str(resolve_against_cwd(
                &v,
                base_for_this_cd.as_ref(),
            )?)),
            None => None,
        };

        let mut ceiling: Option<usize> = None;
        let keyword_start = scan.cp(keyword.start());
        for &(op_start, op_end) in &operand_windows {
            if op_start <= keyword_start
                && keyword_start < op_end
                && ceiling.is_none_or(|c| op_end < c)
            {
                ceiling = Some(op_end);
            }
        }

        let mut guard_end: Option<usize> = None;
        let tail = scan.slice(token_end, scan.len());
        if let Some(or_m) = st.or_guard.find(tail) {
            let or_end = scan.cp(scan.byte(token_end) + or_m.end());
            let mut g = or_operand_end(&scan.cps, or_end);
            if g < scan.len() && in_set(scan.cps[g], "({") {
                g = skip_balanced_group(&scan.cps, g);
            }
            guard_end = Some(g);
            operand_windows.push((or_end, g));
        }

        let enclosing = depths[token_start];
        let break_pos: Option<usize> = if depths[token_end] == enclosing {
            let nl = next_lower.get_or_insert_with(|| next_lower_paren_depth(depths));
            nl[token_end]
        } else {
            (token_end..depths.len()).find(|&j| depths[j] < enclosing)
        };
        reach.starts.push(position);
        reach.infos.push(CdInfo {
            resolved,
            guard_end,
            break_pos,
            ceiling,
        });
    }
    Ok(reach)
}

fn effective_checkout(
    text: &CpText,
    scan: &CpText,
    match_start: usize,
    match_end: usize,
    payload_cwd: &Cwd,
    reach: &CdReach,
) -> Result<Option<Cwd>, String> {
    let st = statics();
    let invocation = scan.slice(match_start, match_end);
    if st.git_dir_locate.is_match(invocation) {
        return Ok(None);
    }
    let mut base_cwd: Option<Cwd> = base_cwd_before(match_start, reach, payload_cwd);
    let inv_byte0 = scan.byte(match_start);
    for c_locate in st.dash_c_locate.find_iter(invocation) {
        let at = scan.cp(inv_byte0 + c_locate.end());
        let (value, _) = read_token(&text.cps, at);
        let Some(value) = value else {
            return Ok(None);
        };
        if value.first() == Some(&SLASH) {
            base_cwd = Some(PyValue::Str(posix_normpath(&value)));
        } else if base_cwd.is_some() {
            base_cwd = Some(PyValue::Str(resolve_against_cwd(
                &value,
                base_cwd.as_ref(),
            )?));
        } else {
            return Ok(None);
        }
    }
    Ok(base_cwd)
}

fn is_command_position(text: &[u32], idx: usize) -> bool {
    let mut j = idx as i64 - 1;
    while j >= 0 && in_set(text[j as usize], " \t") {
        j -= 1;
    }
    j < 0 || in_set(text[j as usize], SEPARATOR_CHARS)
}

#[derive(PartialEq, Debug, Clone, Copy)]
enum LoopExtent {
    Closed,
    Unclosed,
    Crossed,
}

/// Reference `_polling_loop_extent` over `scan[start:end]`.
fn polling_loop_extent(scan: &CpText, start: usize, end: usize) -> LoopExtent {
    let hay = scan.slice(start, end);
    let hay_cps = &scan.cps[start.min(scan.len())..end.max(start).min(scan.len())];
    let base = scan.byte(start);
    let tokens: Vec<(usize, bool)> = statics()
        .loop_token
        .find_iter(hay)
        .map(|m| (scan.cp(base + m.start()) - start, m.as_str() == "do"))
        .filter(|&(pos, _)| is_command_position(hay_cps, pos))
        .collect();
    if tokens.is_empty() {
        return LoopExtent::Crossed;
    }
    let mut depth = 0i64;
    let last = tokens.len() - 1;
    for (i, &(_, is_do)) in tokens.iter().enumerate() {
        if is_do {
            depth += 1;
        } else {
            depth -= 1;
            if depth < 0 {
                return LoopExtent::Crossed;
            }
            if depth == 0 && i != last {
                return LoopExtent::Crossed;
            }
        }
    }
    if depth == 0 {
        LoopExtent::Closed
    } else {
        LoopExtent::Unclosed
    }
}

// ---------------------------------------------------------------------------
// Canonical text and rules
// ---------------------------------------------------------------------------

fn get_str_field(map: &PyValue, key: &str) -> Result<PyStr, String> {
    // `tool_input.get(key, "")` → AttributeError when tool_input is not a
    // dict.
    if !matches!(map, PyValue::Dict(_)) {
        return Err(format!(
            "'{}' object has no attribute 'get' (Python: AttributeError in tool_input.get({key:?}))",
            map.type_name()
        ));
    }
    Ok(map.get(key).map(py_str).unwrap_or_default())
}

/// Canonical text a rule's `match` regex is applied to (reference
/// `canonical_text`).
fn canonical_text(tool_name: &PyValue, tool_input: &PyValue) -> Result<PyStr, String> {
    if *tool_name == "Bash" {
        return get_str_field(tool_input, "command");
    }
    if *tool_name == "NotebookEdit" {
        let mut parts: Vec<PyStr> = Vec::new();
        if tool_input.contains_key("notebook_path")? {
            parts.push(get_str_field(tool_input, "notebook_path")?);
        }
        if tool_input.contains_key("new_source")? {
            parts.push(get_str_field(tool_input, "new_source")?);
        }
        return Ok(join_nl(&parts));
    }
    if TEXT_TOOLS.iter().any(|t| *tool_name == *t) {
        let mut parts: Vec<PyStr> = vec![get_str_field(tool_input, "file_path")?];
        if tool_input.contains_key("new_string")? {
            parts.push(get_str_field(tool_input, "new_string")?);
        }
        if tool_input.contains_key("content")? {
            parts.push(get_str_field(tool_input, "content")?);
        }
        if let Some(PyValue::List(edits)) = tool_input.get("edits") {
            for edit in edits {
                if matches!(edit, PyValue::Dict(_)) {
                    parts.push(get_str_field(edit, "new_string")?);
                }
            }
        }
        return Ok(join_nl(&parts));
    }
    Ok(pystr(&dumps(tool_input, true, true)))
}

fn join_nl(parts: &[PyStr]) -> PyStr {
    let mut out = Vec::new();
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            out.push('\n' as u32);
        }
        out.extend_from_slice(p);
    }
    out
}

/// One rule as it appears in `router-rules.json`, kept as raw Python values
/// so a non-string field behaves exactly as it does in the reference.
#[derive(Debug, Clone)]
pub struct Rule {
    pub id: PyValue,
    pub tool: PyValue,
    pub match_pattern: PyValue,
    pub unless_cwd: Option<PyValue>,
    pub unless_match: Option<PyValue>,
    pub unless_scope: Option<PyValue>,
    pub decision: PyValue,
    pub message: PyValue,
}

impl Rule {
    /// Test/convenience constructor for an all-string rule.
    #[cfg(test)]
    pub fn simple(
        id: &str,
        tool: &str,
        match_pattern: &str,
        unless_match: Option<&str>,
        decision: &str,
        message: &str,
    ) -> Rule {
        Rule {
            id: PyValue::str(id),
            tool: PyValue::str(tool),
            match_pattern: PyValue::str(match_pattern),
            unless_cwd: None,
            unless_match: unless_match.map(PyValue::str),
            unless_scope: None,
            decision: PyValue::str(decision),
            message: PyValue::str(message),
        }
    }
}

/// Parse the rules file the way the reference's `load_rules` reads it:
/// `rule["id"]`/`["tool"]`/`["match"]`/`["decision"]` must exist (KeyError
/// otherwise), `unless_*` and `message` are optional.
pub fn parse_rules(raw: &str) -> Result<Vec<Rule>, String> {
    let v = parse_json(raw)?;
    let PyValue::List(items) = v else {
        return Err("router-rules.json must be a JSON array".into());
    };
    let mut rules = Vec::new();
    for item in items {
        if !matches!(item, PyValue::Dict(_)) {
            return Err("string indices must be integers (Python: TypeError)".into());
        }
        let req = |k: &str| -> Result<PyValue, String> {
            item.get(k)
                .cloned()
                .ok_or_else(|| format!("KeyError: '{k}'"))
        };
        rules.push(Rule {
            id: req("id")?,
            tool: req("tool")?,
            match_pattern: req("match")?,
            unless_cwd: item.get("unless_cwd").cloned(),
            unless_match: item.get("unless_match").cloned(),
            unless_scope: item.get("unless_scope").cloned(),
            decision: req("decision")?,
            message: item
                .get("message")
                .cloned()
                .unwrap_or_else(|| PyValue::str("")),
        });
    }
    Ok(rules)
}

struct CompiledRule {
    id: PyValue,
    tool_re: Regex,
    match_re: Regex,
    uses_gitopts: bool,
    unless_cwd_re: Option<Regex>,
    unless_match_re: Option<Regex>,
    unless_match_anchored: Option<Regex>,
    unless_scope: Option<PyValue>,
    decision: PyValue,
    message: PyValue,
}

fn pattern_text(v: &PyValue, what: &str) -> Result<String, String> {
    match v {
        PyValue::Str(s) => Ok(cps_to_string(s)),
        other => Err(format!(
            "first argument must be string or compiled pattern, got {} (rule `{what}`)",
            other.type_name()
        )),
    }
}

fn compile_rules(rules: &[Rule]) -> Result<Vec<CompiledRule>, String> {
    let mut out = Vec::with_capacity(rules.len());
    for r in rules {
        let id_text = match &r.id {
            PyValue::Str(s) => cps_to_string(s),
            other => py_repr(other),
        };
        let raw_match = pattern_text(&r.match_pattern, "match")?;
        let match_pattern = expand_placeholders(&raw_match);
        let unless_match_pattern = match &r.unless_match {
            Some(v) if v.truthy() => Some(expand_placeholders(&pattern_text(v, "unless_match")?)),
            _ => None,
        };
        let unless_cwd_pattern = match &r.unless_cwd {
            Some(v) if v.truthy() => Some(pattern_text(v, "unless_cwd")?),
            _ => None,
        };
        out.push(CompiledRule {
            id: r.id.clone(),
            tool_re: build_regex(&pattern_text(&r.tool, "tool")?)
                .map_err(|e| format!("rule {id_text}: bad `tool` pattern: {e}"))?,
            match_re: build_regex(&match_pattern)
                .map_err(|e| format!("rule {id_text}: bad `match` pattern: {e}"))?,
            uses_gitopts: raw_match.contains("@GITOPTS@"),
            unless_cwd_re: unless_cwd_pattern
                .as_deref()
                .map(build_regex)
                .transpose()
                .map_err(|e| format!("rule {id_text}: bad `unless_cwd` pattern: {e}"))?,
            unless_match_re: unless_match_pattern
                .as_deref()
                .map(build_regex)
                .transpose()
                .map_err(|e| format!("rule {id_text}: bad `unless_match` pattern: {e}"))?,
            unless_match_anchored: unless_match_pattern
                .as_deref()
                .map(build_anchored)
                .transpose()
                .map_err(|e| format!("rule {id_text}: bad `unless_match` pattern: {e}"))?,
            unless_scope: r.unless_scope.clone(),
            decision: r.decision.clone(),
            message: r.message.clone(),
        });
    }
    Ok(out)
}

fn extend_end_past_quote(quote_spans: &[Span], end: usize) -> usize {
    for &(q_start, q_end) in quote_spans {
        if q_start < end && end < q_end {
            return q_end;
        }
    }
    end
}

// ---------------------------------------------------------------------------
// Decision
// ---------------------------------------------------------------------------

/// A single rule that matched the canonical text ("fired"), independent of
/// whether it won the combined decision.
#[derive(Debug, Clone, PartialEq)]
pub struct Fire {
    pub rule_id: PyValue,
    /// "deny" | "ask" | "prior" (any other value never wins, as in Python).
    pub decision: PyValue,
    pub message: PyValue,
    /// The matched substring, redacted then truncated to 200 code points
    /// (ledger `match` field).
    pub matched: PyStr,
}

/// Ledger context shared by every fire produced from one invocation — the
/// RAW payload values (`redact` is applied at write time, exactly where the
/// reference applies it).
#[derive(Debug, Clone, PartialEq)]
pub struct LedgerContext {
    pub session_id: PyValue,
    pub tool: PyValue,
    pub cwd: PyValue,
    /// Redacted canonical text, first 300 code points (ledger `preview`).
    pub preview: PyStr,
}

/// The pure result of evaluating one PreToolUse payload against a rule set.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// No rule matched. Nothing is printed, nothing is ledgered.
    Abstain,
    /// At least one rule matched. `fires` holds every match (ledger writes
    /// one line per entry); `winner` is the single fire that decides stdout
    /// (deny beats ask beats first-prior-in-file-order). `winner` is `None`
    /// when no fire carries a recognized decision — the reference then
    /// ledgers the fires and prints nothing.
    Fired {
        fires: Vec<Fire>,
        winner: Option<Fire>,
        ctx: LedgerContext,
    },
    /// Internal error (malformed stdin, bad regex, unreadable rules, etc).
    /// The caller (`run`) must fail open: one stderr line, exit 0, no
    /// stdout, nothing ledgered.
    Error(String),
}

/// Pure decision function — no IO. Mirrors `evaluate()` in the Python
/// reference step for step, in the same order (so an error surfaces from
/// the same stage it would there).
pub fn decide(rules: &[Rule], raw_stdin: &str) -> Outcome {
    match decide_inner(rules, raw_stdin) {
        Ok(o) => o,
        Err(e) => Outcome::Error(e),
    }
}

fn decide_inner(rules: &[Rule], raw_stdin: &str) -> Result<Outcome, String> {
    let payload = parse_json(raw_stdin)?;
    if !matches!(payload, PyValue::Dict(_)) {
        return Err(format!(
            "'{}' object has no attribute 'get' (Python: AttributeError in payload.get)",
            payload.type_name()
        ));
    }
    let tool_name = payload
        .get("tool_name")
        .cloned()
        .unwrap_or_else(|| PyValue::str(""));
    let tool_input = match payload.get("tool_input") {
        Some(v) if v.truthy() => v.clone(),
        _ => PyValue::Dict(Vec::new()),
    };
    let cwd = payload
        .get("cwd")
        .cloned()
        .unwrap_or_else(|| PyValue::str(""));
    let session_id = payload
        .get("session_id")
        .cloned()
        .unwrap_or_else(|| PyValue::str(""));

    let text_cps = canonical_text(&tool_name, &tool_input)?;
    let text = CpText::new(text_cps);
    let is_bash = tool_name == "Bash";

    let mut spans = Spans::default();
    let scan_cps = if is_bash {
        executable_mask(&text, &mut spans)
    } else {
        text.cps.clone()
    };
    let scan = CpText::new(scan_cps);
    let gitopts_variants: Vec<CpText> = if is_bash {
        gitopts_scan_variants(&text.cps, &scan.cps, &spans.quote)
            .into_iter()
            .map(CpText::new)
            .collect()
    } else {
        Vec::new()
    };
    let sep_positions: Vec<usize> = scan
        .cps
        .iter()
        .enumerate()
        .filter(|(_, &c)| in_set(c, SEPARATOR_CHARS))
        .map(|(i, _)| i)
        .collect();
    let depths = paren_depths(&scan.cps);
    let reach = precompute_cd_reach_info(&text, &scan, &depths, &cwd)?;
    let compiled = compile_rules(rules)?;

    let match_starts_inside_quoted_literal_text = |start: usize| -> bool {
        let (innermost_start, innermost_is_live) = innermost_span_is_live(start, &spans);
        innermost_start >= 0 && !innermost_is_live
    };

    let mut fires: Vec<Fire> = Vec::new();
    for rule in &compiled {
        let Some(tool_name_str) = tool_name.as_pystr() else {
            return Err(format!(
                "expected string or bytes-like object, got '{}' (Python: TypeError in tool_re.search)",
                tool_name.type_name()
            ));
        };
        if !rule.tool_re.is_match(&cps_to_string(tool_name_str)) {
            continue;
        }
        // Candidates in scan_text code-point spans.
        let mut candidates: Vec<Span> = rule
            .match_re
            .find_iter(&scan.s)
            .map(|m| (scan.cp(m.start()), scan.cp(m.end())))
            .collect();
        if rule.uses_gitopts && !gitopts_variants.is_empty() {
            let mut seen: std::collections::HashSet<Span> = candidates.iter().copied().collect();
            for variant in &gitopts_variants {
                for m in rule.match_re.find_iter(&variant.s) {
                    let span = (variant.cp(m.start()), variant.cp(m.end()));
                    if seen.insert(span) {
                        candidates.push(span);
                    }
                }
            }
        }
        let all_matches: Vec<Span> = candidates
            .into_iter()
            .filter(|&(s, _)| !match_starts_inside_quoted_literal_text(s))
            .collect();
        if all_matches.is_empty() {
            continue;
        }

        let cwd_exempts = |cand: Span| -> Result<bool, String> {
            let Some(re) = &rule.unless_cwd_re else {
                return Ok(false);
            };
            let eff = effective_checkout(&text, &scan, cand.0, cand.1, &cwd, &reach)?;
            match eff {
                None => Ok(false),
                Some(PyValue::Str(s)) => Ok(re.is_match(&cps_to_string(&s))),
                Some(other) => Err(format!(
                    "expected string or bytes-like object, got '{}' (Python: TypeError in unless_cwd_re.search)",
                    other.type_name()
                )),
            }
        };

        let scope = rule.unless_scope.as_ref();
        let mut match_override_span: Option<Span> = None;
        let m: Option<Span>;
        if rule.unless_match_re.is_none() {
            let mut chosen = None;
            if rule.id == "gh-fast-polling" {
                let mut search_pos = 0usize;
                loop {
                    let Some(cm) = rule.match_re.find_at(&scan.s, scan.byte(search_pos)) else {
                        break;
                    };
                    let cand = (scan.cp(cm.start()), scan.cp(cm.end()));
                    if cwd_exempts(cand)? {
                        search_pos = (cand.0 + 1).max(cand.1);
                        continue;
                    }
                    let mut end = cand.1;
                    let mut extent = polling_loop_extent(&scan, cand.0, end);
                    while extent == LoopExtent::Unclosed {
                        let mut next_done = statics().loop_token.find_at(&scan.s, scan.byte(end));
                        while let Some(nd) = next_done {
                            if is_command_position(&scan.cps, scan.cp(nd.start())) {
                                break;
                            }
                            next_done = statics().loop_token.find_at(&scan.s, nd.end());
                        }
                        let Some(nd) = next_done else {
                            break;
                        };
                        end = scan.cp(nd.end());
                        extent = polling_loop_extent(&scan, cand.0, end);
                    }
                    if extent != LoopExtent::Closed {
                        search_pos = cand.0 + 1;
                        continue;
                    }
                    chosen = Some(cand);
                    match_override_span = Some((cand.0, end));
                    break;
                }
            } else {
                for &cand in &all_matches {
                    if cwd_exempts(cand)? {
                        continue;
                    }
                    chosen = Some(cand);
                    break;
                }
            }
            m = chosen;
        } else if scope.is_some_and(|s| *s == "shell") {
            let unless_re = rule.unless_match_re.as_ref().expect("checked");
            let mut chosen = None;
            for &cand in &all_matches {
                if cwd_exempts(cand)? {
                    continue;
                }
                let mut exempted = false;
                for um in unless_re.captures_iter(&scan.s) {
                    let whole = um.get(0).expect("group 0");
                    if um.name("before").is_some() && scan.cp(whole.end()) > cand.0 {
                        continue;
                    }
                    exempted = true;
                    break;
                }
                if !exempted {
                    chosen = Some(cand);
                    break;
                }
            }
            m = chosen;
        } else if scope.is_some_and(|s| *s == "invocation") {
            let anchored = rule.unless_match_anchored.as_ref().expect("checked");
            let mut chosen = None;
            for &cand in &all_matches {
                if cwd_exempts(cand)? {
                    continue;
                }
                let matched_text = scan.slice(cand.0, cand.1);
                let anchor = match statics().tail_word.find(matched_text) {
                    Some(t) => scan.cp(scan.byte(cand.0) + t.start()),
                    None => cand.1,
                };
                if anchored.find(scan.slice(anchor, scan.len())).is_none() {
                    chosen = Some(cand);
                    break;
                }
            }
            m = chosen;
        } else if all_matches.len() == 1 {
            let unless_re = rule.unless_match_re.as_ref().expect("checked");
            if cwd_exempts(all_matches[0])? || unless_re.is_match(&scan.s) {
                continue;
            }
            m = Some(all_matches[0]);
        } else {
            let unless_re = rule.unless_match_re.as_ref().expect("checked");
            let mut chosen = None;
            for &cand in &all_matches {
                if cwd_exempts(cand)? {
                    continue;
                }
                let (start, end) = window_bounds(&sep_positions, scan.len(), cand.0, cand.1);
                if !unless_re.is_match(scan.slice(start, end)) {
                    chosen = Some(cand);
                    break;
                }
            }
            m = chosen;
        }
        let Some(m) = m else {
            continue;
        };

        let (raw_start, raw_end) = match_override_span.unwrap_or(m);
        let raw_end = extend_end_past_quote(&spans.quote, raw_end);
        let raw_matched =
            &text.cps[raw_start.min(text.len())..raw_end.max(raw_start).min(text.len())];
        let matched: PyStr = redact_cps(raw_matched)
            .into_iter()
            .take(MATCH_TRUNCATE)
            .collect();
        fires.push(Fire {
            rule_id: rule.id.clone(),
            decision: rule.decision.clone(),
            message: rule.message.clone(),
            matched,
        });
    }

    if fires.is_empty() {
        return Ok(Outcome::Abstain);
    }

    // Ledger context — `redact()` is applied to the persisted copies of
    // session_id/cwd at write time in the reference (`redact_value`).
    let preview: PyStr = redact_cps(&text.cps)
        .into_iter()
        .take(PREVIEW_TRUNCATE)
        .collect();
    let winner = ["deny", "ask", "prior"]
        .iter()
        .find_map(|want| fires.iter().find(|f| f.decision == *want).cloned());
    Ok(Outcome::Fired {
        fires,
        winner,
        ctx: LedgerContext {
            session_id,
            tool: tool_name,
            cwd,
            preview,
        },
    })
}

// ---------------------------------------------------------------------------
// IO: ledger, stdout, fail-open
// ---------------------------------------------------------------------------

/// Python `datetime.now(timezone.utc).isoformat()`: microseconds when
/// non-zero, `+00:00` offset.
fn python_isoformat_now() -> String {
    let now = chrono::Utc::now();
    if now.timestamp_subsec_micros() == 0 {
        now.format("%Y-%m-%dT%H:%M:%S+00:00").to_string()
    } else {
        now.format("%Y-%m-%dT%H:%M:%S%.6f+00:00").to_string()
    }
}

/// Resolve `$HEX_LEDGER_DIR`, else `~/.hex/ledger`, creating it 0700 and
/// forcing 0700 on a pre-existing directory (reference `_ensure_private_dir`).
fn ledger_path() -> Result<PathBuf, String> {
    let dir = match std::env::var("HEX_LEDGER_DIR") {
        Ok(d) if !d.is_empty() => PathBuf::from(d),
        _ => dirs::home_dir()
            .ok_or_else(|| "could not resolve the home directory".to_string())?
            .join(".hex")
            .join("ledger"),
    };
    ensure_private_dir(&dir)?;
    Ok(dir.join(LEDGER_FILENAME))
}

#[cfg(unix)]
fn ensure_private_dir(dir: &std::path::Path) -> Result<(), String> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| format!("failed to create {}: {e}", dir.display()))?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("failed to chmod {}: {e}", dir.display()))
}

#[cfg(not(unix))]
fn ensure_private_dir(dir: &std::path::Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("failed to create {}: {e}", dir.display()))
}

/// Render the ledger lines for one invocation — `json.dumps(entry,
/// sort_keys=True)` per fire, exactly the reference's bytes.
pub fn ledger_lines_for(fires: &[Fire], ctx: &LedgerContext, ts: &str) -> Result<String, String> {
    let session_id = redact_value(&ctx.session_id)?;
    let cwd = redact_value(&ctx.cwd)?;
    let mut lines: Vec<String> = Vec::with_capacity(fires.len());
    for fire in fires {
        let entry = PyValue::Dict(vec![
            (pystr("ts"), PyValue::str(ts)),
            (pystr("session_id"), session_id.clone()),
            (pystr("rule_id"), fire.rule_id.clone()),
            (pystr("tool"), ctx.tool.clone()),
            (pystr("decision"), fire.decision.clone()),
            (pystr("match"), PyValue::Str(fire.matched.clone())),
            (pystr("preview"), PyValue::Str(ctx.preview.clone())),
            (pystr("cwd"), cwd.clone()),
        ]);
        lines.push(dumps(&entry, true, false));
    }
    Ok(format!("{}\n", lines.join("\n")))
}

/// Append `batch` (one or more complete `\n`-terminated JSONL records) to
/// the ledger file under an exclusive lock, after recovering an incomplete
/// tail left by an earlier writer (review F9): if the file's last byte is
/// not `\n`, a newline is written first so the fragment becomes its own
/// (unparseable, but isolated) line instead of being glued onto the first
/// record of this batch. The file is created 0600 and forced to 0600 when
/// it already exists (reference `_open_private_append`).
pub fn append_ledger_at(path: &std::path::Path, batch: &str) -> Result<(), String> {
    use std::io::{Seek, SeekFrom, Write as _};
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true).append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts
        .open(path)
        .map_err(|e| format!("failed to open {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        f.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("failed to chmod {}: {e}", path.display()))?;
    }
    f.lock()
        .map_err(|e| format!("failed to lock {}: {e}", path.display()))?;
    let len = f
        .metadata()
        .map_err(|e| format!("failed to stat {}: {e}", path.display()))?
        .len();
    let mut out = String::new();
    if len > 0 {
        f.seek(SeekFrom::Start(len - 1))
            .map_err(|e| format!("failed to seek {}: {e}", path.display()))?;
        let mut last = [0u8; 1];
        f.read_exact(&mut last)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        if last[0] != b'\n' {
            out.push('\n');
        }
    }
    out.push_str(batch);
    // O_APPEND: the write lands at the end regardless of the seek above.
    let res = f.write_all(out.as_bytes()).map_err(|e| e.to_string());
    let _ = f.unlock();
    res
}

fn append_ledger(fires: &[Fire], ctx: &LedgerContext) -> Result<(), String> {
    let batch = ledger_lines_for(fires, ctx, &python_isoformat_now())?;
    let path = ledger_path()?;
    append_ledger_at(&path, &batch)
}

/// The stdout document for the winning fire (`json.dumps({"hookSpecificOutput": hso})`).
pub fn stdout_document(winner: &Fire) -> String {
    let mut hso: Vec<(PyStr, PyValue)> = vec![(pystr("hookEventName"), PyValue::str("PreToolUse"))];
    if winner.decision == "deny" || winner.decision == "ask" {
        hso.push((pystr("permissionDecision"), winner.decision.clone()));
        hso.push((pystr("permissionDecisionReason"), winner.message.clone()));
    } else {
        hso.push((pystr("additionalContext"), winner.message.clone()));
    }
    let doc = PyValue::Dict(vec![(pystr("hookSpecificOutput"), PyValue::Dict(hso))]);
    format!("{}\n", dumps(&doc, false, false))
}

/// One-line diagnostic formatting is shared by every hook (review F7; the
/// workspace resolver in `hook/mod.rs` uses the same function).
pub(crate) use super::one_line;

/// Best-effort single stderr line (review F8): a closed stderr must not
/// panic or change the exit status.
fn report_error(msg: &str) {
    use std::io::Write as _;
    let line = format!("[router] error: {}\n", one_line(msg));
    let stderr = std::io::stderr();
    let mut h = stderr.lock();
    let _ = h.write_all(line.as_bytes());
    let _ = h.flush();
}

/// Best-effort stdout write (review F8).
fn emit_stdout(doc: &str) {
    use std::io::Write as _;
    let stdout = std::io::stdout();
    let mut h = stdout.lock();
    let _ = h.write_all(doc.as_bytes());
    let _ = h.flush();
}

/// Thin IO wrapper: read stdin, resolve the rules file, call `decide`, write
/// the ledger, print stdout — or fail open on any error. Always exits 0.
pub fn run() {
    let mut raw = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut raw) {
        report_error(&format!("failed to read stdin: {e}"));
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
    let rules = match std::fs::read_to_string(&rules_path) {
        Ok(raw_rules) => match parse_rules(&raw_rules) {
            Ok(r) => r,
            Err(e) => {
                report_error(&e);
                std::process::exit(0);
            }
        },
        Err(e) => {
            report_error(&format!("failed to read {}: {e}", rules_path.display()));
            std::process::exit(0);
        }
    };

    match decide(&rules, &raw) {
        Outcome::Abstain => {}
        Outcome::Error(e) => report_error(&e),
        Outcome::Fired { fires, winner, ctx } => {
            if let Err(e) = append_ledger(&fires, &ctx) {
                report_error(&e);
                std::process::exit(0);
            }
            if let Some(w) = winner {
                emit_stdout(&stdout_document(&w));
            }
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

    fn make_payload(
        tool_name: &str,
        tool_input: serde_json::Value,
        cwd: &str,
        session_id: &str,
    ) -> String {
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

    fn make_payload_default(tool_name: &str, tool_input: serde_json::Value) -> String {
        make_payload(tool_name, tool_input, DEFAULT_CWD, "sess-1")
    }

    /// Load the real, shared rules file — the same one the Python reference
    /// reads. Keeping one source of truth avoids fixture drift between the
    /// two implementations.
    fn load_seed_rules() -> Vec<Rule> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../hooks/router-rules.json");
        let raw = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("router-rules.json must exist at {path:?}: {e}"));
        parse_rules(&raw).unwrap_or_else(|e| panic!("router-rules.json must parse: {e}"))
    }

    fn s(v: &PyValue) -> String {
        match v {
            PyValue::Str(c) => cps_to_string(c),
            other => panic!("expected a str value, got {other:?}"),
        }
    }

    fn winner_of(outcome: Outcome) -> Fire {
        match outcome {
            Outcome::Fired {
                winner: Some(w), ..
            } => w,
            other => panic!("expected a Fired outcome with a winner, got {other:?}"),
        }
    }

    struct RuleFixture {
        id: &'static str,
        decision: &'static str,
        tool_name: &'static str,
        positive: serde_json::Value,
        near_miss_tool_name: &'static str,
        near_miss: serde_json::Value,
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
        let rule_ids: std::collections::BTreeSet<_> = rules.iter().map(|r| s(&r.id)).collect();
        let fixture_ids: std::collections::BTreeSet<_> =
            rule_fixtures().iter().map(|f| f.id.to_string()).collect();
        assert_eq!(rule_ids, fixture_ids, "fixture/rules-file id drift");
    }

    #[test]
    fn each_seed_rule_fires_with_expected_decision_on_its_positive_fixture() {
        let rules = load_seed_rules();
        for fx in rule_fixtures() {
            let raw = make_payload_default(fx.tool_name, fx.positive.clone());
            match decide(&rules, &raw) {
                Outcome::Fired {
                    fires,
                    winner: Some(winner),
                    ctx,
                } => {
                    assert_eq!(fires.len(), 1, "{}: expected exactly one fire", fx.id);
                    assert_eq!(winner.rule_id, fx.id, "{}: wrong winner", fx.id);
                    assert_eq!(winner.decision, fx.decision, "{}: wrong decision", fx.id);
                    assert!(!winner.matched.is_empty(), "{}: empty match", fx.id);
                    assert!(
                        winner.matched.len() <= 200,
                        "{}: match not truncated",
                        fx.id
                    );
                    assert_eq!(ctx.tool, fx.tool_name, "{}: wrong tool in ctx", fx.id);
                    assert_eq!(ctx.cwd, DEFAULT_CWD, "{}: wrong cwd in ctx", fx.id);
                    assert_eq!(ctx.session_id, "sess-1", "{}: wrong session_id", fx.id);
                    assert!(ctx.preview.len() <= 300, "{}: preview not truncated", fx.id);
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
            assert_eq!(
                decide(&rules, &raw),
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

    /// `unless_cwd` tracks the EFFECTIVE checkout (a `-C` or a preceding
    /// `cd`), not the hook's own payload cwd — the reference's F4 family.
    #[test]
    fn effective_checkout_governs_unless_cwd() {
        let rules = load_seed_rules();
        let wt = "/tmp/hex-home/.boi/v2/worktrees/Scnfz8k1f/T7w2t3bzf";
        for (cmd, cwd, expect_deny) in [
            ("git -C /shared/checkout stash", wt, true),
            (
                "git -C /tmp/hex-home/.boi/v2/worktrees/Scnfz8k1f/T7w2t3bzf stash",
                DEFAULT_CWD,
                false,
            ),
            ("cd /shared/checkout && git stash", wt, true),
            ("cd sub && git stash", wt, false),
            ("(cd /worktrees/x); git stash", DEFAULT_CWD, true),
            ("(cd /worktrees/x; git stash)", DEFAULT_CWD, false),
            ("cd /worktrees/x || git stash", DEFAULT_CWD, true),
            ("cd '/shared/checkout' && git stash", wt, true),
            ("cd \"$DIR\" && git stash", wt, true),
            (
                "cd /worktrees/x/../../shared/checkout && git stash",
                wt,
                true,
            ),
        ] {
            let raw = make_payload("Bash", json!({"command": cmd}), cwd, "sess-1");
            let outcome = decide(&rules, &raw);
            if expect_deny {
                assert_eq!(winner_of(outcome).decision, "deny", "{cmd} @ {cwd}");
            } else {
                assert_eq!(outcome, Outcome::Abstain, "{cmd} @ {cwd}");
            }
        }
    }

    /// Review finding G1: a global `unless_match` (searching the WHOLE
    /// canonical text) let a single safe sub-invocation blanket-suppress a
    /// dangerous sibling chained in the same command string.
    #[test]
    fn safe_subcommand_does_not_shield_a_chained_dangerous_one() {
        let rules = load_seed_rules();
        let cmd = "git stash pop; git stash";
        let raw = make_payload_default("Bash", json!({"command": cmd}));
        match decide(&rules, &raw) {
            Outcome::Fired {
                winner: Some(winner),
                fires,
                ..
            } => {
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
            Outcome::Fired {
                winner: Some(winner),
                fires,
                ..
            } => {
                assert_eq!(winner.decision, "ask", "{cmd}");
                assert!(
                    fires.iter().any(|f| f.rule_id == "git-push-force"),
                    "{cmd}: expected git-push-force among fires, got {fires:?}"
                );
            }
            other => panic!("{cmd}: expected Fired(ask), got {other:?}"),
        }
    }

    /// Review F2: exemption scope is a rule property. `pipe-tail-masks-exit`
    /// is `unless_scope: shell` — one `set -o pipefail` protects EVERY later
    /// pipeline, but never one that ran before it.
    #[test]
    fn shell_scoped_exemption_covers_every_later_pipeline_but_not_earlier_ones() {
        let rules = load_seed_rules();
        let protected = "set -o pipefail; cargo test | tail -20; pytest -q | tail -5";
        let raw = make_payload_default("Bash", json!({"command": protected}));
        assert_eq!(decide(&rules, &raw), Outcome::Abstain, "{protected}");
        let too_late = "pytest -q | tail -5; set -o pipefail; cargo test | tail -20";
        let raw = make_payload_default("Bash", json!({"command": too_late}));
        assert_eq!(
            winner_of(decide(&rules, &raw)).rule_id,
            "pipe-tail-masks-exit",
            "{too_late}"
        );
        // Invocation-scoped (`git-stash-shared-checkout`): a safe-looking
        // word elsewhere in the same invocation must not exempt it.
        let smuggled = "git stash push -m 'stash pop'";
        let raw = make_payload_default("Bash", json!({"command": smuggled}));
        assert_eq!(
            winner_of(decide(&rules, &raw)).decision,
            "deny",
            "{smuggled}"
        );
    }

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

    /// Review finding G2: the compact-JSON canonical-text branch must escape
    /// non-ASCII exactly like Python's `json.dumps` default
    /// (`ensure_ascii=True`), byte-for-byte.
    #[test]
    fn canonical_text_escapes_non_ascii_like_python_ensure_ascii() {
        let tool_input =
            parse_json(r#"{"schedule": "* * * * *", "command": "echo héllo"}"#).unwrap();
        let text =
            cps_to_string(&canonical_text(&PyValue::str("CronCreate"), &tool_input).unwrap());
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
        let w = winner_of(decide(&rules, &raw));
        assert_eq!(w.decision, "ask");
        assert_eq!(w.rule_id, "gh-fast-polling");
    }

    /// The polling-loop bound: an unrelated earlier loop's `done` must not
    /// be crossed, a nested bounded loop before or after the call is fine.
    #[test]
    fn polling_loop_bounds_follow_the_reference() {
        let rules = load_seed_rules();
        let cases: &[(&str, bool)] = &[
            ("while true; do\n  gh pr checks 123\n  sleep 5\ndone", true),
            (
                "while read x; do echo $x; done < f\ngh pr checks 123\nsleep 5\nfor p in 1 2; do echo; done",
                false,
            ),
            (
                "while read x; do echo $x; done < f\nwhile true; do gh pr checks 1; sleep 5; done",
                true,
            ),
            (
                "while true; do\n  for i in 1 2; do\n    echo $i\n  done\n  gh pr checks 123\n  sleep 5\ndone",
                true,
            ),
            (
                "while true; do\n  gh pr checks 123\n  sleep 5\n  for i in 1 2; do\n    echo $i\n  done\ndone",
                true,
            ),
        ];
        for (cmd, asks) in cases {
            let raw = make_payload_default("Bash", json!({"command": cmd}));
            let outcome = decide(&rules, &raw);
            if *asks {
                assert_eq!(winner_of(outcome).rule_id, "gh-fast-polling", "{cmd:?}");
            } else {
                assert_eq!(outcome, Outcome::Abstain, "{cmd:?}");
            }
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
            "git commit -m 'fix\n\ngit stash was wrong'",
            "echo 'x\ngit push --force'",
            "printf '%s\\n' 'then a stash of it'",
            "echo \"$(printf '%s\\n' 'then a stash of it')\"",
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
            ("env FOO=1 git stash", "deny"),
            ("FOO=\"a b\" git stash", "deny"),
            ("if true; then git stash; fi", "deny"),
            ("echo \"$(git stash)\"", "deny"),
            ("git push origin '+HEAD:main'", "ask"),
            ("git reset HEAD~1 '--hard'", "ask"),
            ("git -c 'user.name=A B' stash", "deny"),
            ("git -c 'user.name=A B' -c 'user.email=x y' stash", "deny"),
            ("echo ${#HOME}; git stash", "deny"),
            ("echo \\'; git stash", "deny"),
            ("cd /tmp && git \\\nstash", "deny"),
        ];
        for (cmd, expected) in cases {
            let raw = make_payload_default("Bash", json!({"command": cmd}));
            assert_eq!(winner_of(decide(&rules, &raw)).decision, *expected, "{cmd}");
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

    /// `\A`-anchored path rules only see the canonical FIRST line (the real
    /// path), never a path-like string inside file content.
    #[test]
    fn path_rules_anchor_on_the_canonical_first_line_only() {
        let rules = load_seed_rules();
        let raw = make_payload_default(
            "Write",
            json!({"file_path": "notes.md", "content": "intro line\nfoo.test.ts\nspawnSync(cmd)"}),
        );
        assert_eq!(decide(&rules, &raw), Outcome::Abstain);
        let raw = make_payload_default(
            "Write",
            json!({"file_path": "notes.md", "content": "intro\n.hex-events/policies/foo.yaml mentioned here"}),
        );
        assert_eq!(decide(&rules, &raw), Outcome::Abstain);
        let raw = make_payload_default(
            "NotebookEdit",
            json!({"notebook_path": "nb.test.ts", "new_source": "spawnSync('ls')"}),
        );
        assert_eq!(winner_of(decide(&rules, &raw)).rule_id, "vitest-spawnsync");
    }

    #[test]
    fn plain_pipe_to_tail_still_fires() {
        let rules = load_seed_rules();
        let raw = make_payload_default("Bash", json!({"command": "pnpm test | tail -20"}));
        assert_eq!(
            winner_of(decide(&rules, &raw)).rule_id,
            "pipe-tail-masks-exit"
        );
    }

    #[test]
    fn grep_and_head_fire_like_tail() {
        let rules = load_seed_rules();
        for cmd in [
            "python3 -m unittest discover -s tests | grep -E '^(Ran|OK|FAILED)'",
            "cargo test --locked | head -40",
        ] {
            let raw = make_payload_default("Bash", json!({"command": cmd}));
            assert_eq!(
                winner_of(decide(&rules, &raw)).rule_id,
                "pipe-tail-masks-exit",
                "{cmd}"
            );
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
            Outcome::Fired {
                fires,
                winner: Some(winner),
                ..
            } => {
                assert_eq!(winner.decision, "deny");
                assert_eq!(winner.rule_id, "git-stash-shared-checkout");
                let mut rule_ids: Vec<String> = fires.iter().map(|f| s(&f.rule_id)).collect();
                rule_ids.sort();
                assert_eq!(
                    rule_ids,
                    vec!["gh-pr-merge-ci-green", "git-stash-shared-checkout"]
                );
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
            Outcome::Fired {
                fires,
                winner: Some(winner),
                ..
            } => {
                assert_eq!(winner.decision, "prior");
                assert_eq!(winner.rule_id, "gh-pr-merge-ci-green");
                let mut rule_ids: Vec<String> = fires.iter().map(|f| s(&f.rule_id)).collect();
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

    /// A wrongly-typed `cwd` reaches `unless_cwd_re.search` for the stash
    /// rule and fails open — never silently coerces to `""` (which would
    /// wrongly deny).
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

    /// Review F5: metadata is kept RAW. A rule without `unless_cwd` never
    /// touches cwd, so the decision is made — but a truthy non-string cwd
    /// then fails open at ledger-write time, exactly where the reference's
    /// `redact(cwd)` raises `TypeError`; a FALSY non-string is ledgered as
    /// its own JSON value.
    #[test]
    fn non_string_metadata_is_kept_raw_and_fails_open_only_when_redact_would() {
        let rules = load_seed_rules();
        let raw = json!({
            "session_id": "sess-1",
            "cwd": 123,
            "tool_name": "CronCreate",
            "tool_input": {"schedule": "* * * * *", "command": "echo hi"},
        })
        .to_string();
        let (fires, ctx) = match decide(&rules, &raw) {
            Outcome::Fired {
                fires,
                winner: Some(w),
                ctx,
            } => {
                assert_eq!(w.rule_id, "builtin-scheduler-tools");
                assert_eq!(ctx.cwd, PyValue::Int("123".into()), "cwd must be kept raw");
                (fires, ctx)
            }
            other => panic!("expected Fired, got {other:?}"),
        };
        assert!(
            ledger_lines_for(&fires, &ctx, "TS").is_err(),
            "a truthy non-string cwd must fail open at ledger time (Python: TypeError in redact)"
        );

        let raw = json!({
            "session_id": null,
            "cwd": 0,
            "tool_name": "CronCreate",
            "tool_input": {"schedule": "* * * * *", "command": "echo hi"},
        })
        .to_string();
        let (fires, ctx) = match decide(&rules, &raw) {
            Outcome::Fired { fires, ctx, .. } => (fires, ctx),
            other => panic!("expected Fired, got {other:?}"),
        };
        let lines = ledger_lines_for(&fires, &ctx, "TS").unwrap();
        assert_eq!(
            lines,
            "{\"cwd\": 0, \"decision\": \"ask\", \"match\": \"{\\\"command\\\":\\\"echo hi\\\",\\\"schedule\\\":\\\"* * * * *\\\"}\", \"preview\": \"{\\\"command\\\":\\\"echo hi\\\",\\\"schedule\\\":\\\"* * * * *\\\"}\", \"rule_id\": \"builtin-scheduler-tools\", \"session_id\": null, \"tool\": \"CronCreate\", \"ts\": \"TS\"}\n"
        );
    }

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

    fn run_python_reference(raw: &str, ledger_dir: &Path) -> std::process::Output {
        let python_script = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("hooks/scripts/pretooluse-router.py");
        assert!(python_script.exists());
        Command::new("python3")
            .args(["-I", "-S"])
            .arg(&python_script)
            .env("HEX_LEDGER_DIR", ledger_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                child.stdin.take().unwrap().write_all(raw.as_bytes())?;
                child.wait_with_output()
            })
            .expect("failed to run python reference router")
    }

    /// Differential companion: the SAME malformed payloads through the live
    /// Python reference also produce empty stdout + exit 0 (fail-open),
    /// pinning that `Outcome::Error` mirrors real Python behavior.
    #[test]
    fn wrong_typed_fields_match_python_reference_fail_open_behavior() {
        let payloads = [
            json!({"session_id": "sess-1", "cwd": 123, "tool_name": "Bash", "tool_input": {"command": "git stash"}}),
            json!({"session_id": "sess-1", "cwd": "/tmp", "tool_name": 42, "tool_input": {"command": "git stash"}}),
            json!({"session_id": "sess-1", "cwd": "/tmp", "tool_name": "Bash", "tool_input": ["git", "stash"]}),
            json!({"session_id": "sess-1", "cwd": 123, "tool_name": "CronCreate", "tool_input": {"schedule": "* * * * *", "command": "echo hi"}}),
        ];
        for payload in payloads {
            let raw = payload.to_string();
            let ledger_dir = tempfile::tempdir().unwrap();
            let out = run_python_reference(&raw, ledger_dir.path());
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
                !ledger_dir.path().join(LEDGER_FILENAME).exists(),
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
    /// seed rule uses it.
    #[test]
    fn unless_match_suppresses_a_fire_entirely() {
        let rules = vec![Rule::simple(
            "test-unless-match-suppression",
            "^Bash$",
            "foo",
            Some("foo-safe"),
            "ask",
            "should never surface when unless_match matches",
        )];
        let raw = make_payload_default("Bash", json!({"command": "run foo-safe now"}));
        assert_eq!(decide(&rules, &raw), Outcome::Abstain);
    }

    // ---- Review F6: no quadratic window/reach evaluation ----

    /// A large all-exempt command (thousands of chained safe invocations,
    /// each a `match` occurrence the exemption then rejects) must evaluate
    /// in bounded time: separator positions and paren depths are computed
    /// once and binary-searched, cd-reach data is precomputed with the
    /// reference's union-find. The bound is deliberately loose (a loaded
    /// shared box), but a quadratic rescan per candidate took well over a
    /// minute here at this size.
    #[test]
    fn large_repeated_exempt_command_evaluates_in_bounded_time() {
        let rules = load_seed_rules();
        let safe = "git stash list; ".repeat(4000);
        let raw = make_payload_default("Bash", json!({"command": safe}));
        let started = std::time::Instant::now();
        assert_eq!(decide(&rules, &raw), Outcome::Abstain);
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(10),
            "4000 exempt occurrences took {elapsed:?}"
        );
        // Same shape for the cd-reach precomputation: thousands of subshell
        // cds that each die immediately, then one real invocation.
        let cds = format!("cd /tmp; {}git stash", "(cd /tmp); ".repeat(4000));
        let raw = make_payload_default("Bash", json!({"command": cds}));
        let started = std::time::Instant::now();
        assert_eq!(winner_of(decide(&rules, &raw)).decision, "deny");
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(10),
            "4000 subshell cds took {elapsed:?}"
        );
    }

    // ---- Review F13: shared ASCII regex semantics ----

    #[test]
    fn translate_py_regex_produces_ascii_class_semantics() {
        assert_eq!(
            translate_py_regex(r"\bstash\b").unwrap(),
            r"(?-u:\b)stash(?-u:\b)"
        );
        assert_eq!(translate_py_regex(r"\w+\Z").unwrap(), r"[0-9A-Za-z_]+\z");
        // `&` (a set operator in `regex` classes) is escaped inside a class.
        assert_eq!(
            translate_py_regex(r"[^\s;&|]").unwrap(),
            "[^\\t\\n\\x0b\\x0c\\r ;\\&|]"
        );
        assert_eq!(translate_py_regex(r"a{").unwrap(), r"a\{");
        assert_eq!(translate_py_regex(r"a{2,}").unwrap(), r"a{2,}");
        assert_eq!(translate_py_regex(r"(?i)ab").unwrap(), r"[aA][bB]");
        assert!(
            translate_py_regex(r"(?=x)").is_err(),
            "lookahead must be rejected"
        );
        assert!(
            translate_py_regex(r"(a)\1").is_err(),
            "backreferences must be rejected"
        );

        // Behavioural parity with `re.ASCII`: a combining mark or `é` after
        // `stash` is NOT a word character, so `\b` matches there; Rust's
        // Unicode `\b` would not.
        let re = build_regex(r"\bstash\b").unwrap();
        assert!(re.is_match("git stash\u{301}"));
        assert!(re.is_match("git stash\u{e9}"));
        assert!(!re.is_match("git stashed"));
        let word = build_regex(r"^\w+$").unwrap();
        assert!(!word.is_match("caf\u{e9}"), "ASCII \\w must not match é");
        // `(?i)` folds ASCII letters only: the Kelvin sign (U+212A) is not `k`.
        let ci = build_regex(r"(?i)token").unwrap();
        assert!(ci.is_match("TOKEN"));
        assert!(!ci.is_match("to\u{212A}en"));
        // PR #10 round-3 F16: escaped letters fold too, outside and inside a
        // class, while non-letter escapes stay exact.
        assert_eq!(translate_py_regex(r"(?i)\x61").unwrap(), "[aA]");
        assert_eq!(translate_py_regex(r"(?i)[\x61B]").unwrap(), "[aABb]");
        assert!(build_regex(r"(?i)\x61").unwrap().is_match("A"));
        assert!(build_regex(r"(?i)[\x61]").unwrap().is_match("A"));
        assert!(build_regex(r"(?i)b").unwrap().is_match("B"));
        assert!(!build_regex(r"(?i)\x31").unwrap().is_match("a"));
        assert_eq!(translate_py_regex(r"(?i)\x2d").unwrap(), r"\x2d");
        // Every seed rule pattern (placeholders expanded) translates and
        // compiles.
        for rule in load_seed_rules() {
            let pat = expand_placeholders(&s(&rule.match_pattern));
            build_regex(&pat).unwrap_or_else(|e| panic!("{}: {e}", s(&rule.id)));
        }
    }

    #[test]
    fn unicode_after_a_keyword_still_fires_like_the_reference() {
        let rules = load_seed_rules();
        let wt = "/tmp/hex-home/.boi/v2/worktrees/Scnfz8k1f/T7w2t3bzf";
        for (cmd, cwd) in [
            ("git stash\u{301}", DEFAULT_CWD),
            ("git stash\u{e9}", DEFAULT_CWD),
            ("cd /shared/\u{e9}t\u{e9} && git stash", wt),
        ] {
            let raw = make_payload("Bash", json!({"command": cmd}), cwd, "sess-1");
            assert_eq!(winner_of(decide(&rules, &raw)).decision, "deny", "{cmd:?}");
        }
        // A `cd` INTO a worktree path still exempts, Unicode or not.
        let raw = make_payload_default(
            "Bash",
            json!({"command": "cd /worktrees/\u{e9}t\u{e9} && git stash"}),
        );
        assert_eq!(decide(&rules, &raw), Outcome::Abstain);
    }

    // ---- Review F3/F4/F14: Python-compatible JSON values ----

    #[test]
    fn json_numbers_render_like_python() {
        let cases = [
            ("1e-7", "1e-07"),
            ("1.5e-7", "1.5e-07"),
            ("1E2", "100.0"),
            ("0.5", "0.5"),
            ("1e16", "1e+16"),
            ("1e15", "1000000000000000.0"),
            ("123456789.0", "123456789.0"),
            ("0.0001", "0.0001"),
            ("0.00001", "1e-05"),
            ("-0.0", "-0.0"),
            ("-0", "0"),
            (
                "123456789012345678901234567890",
                "123456789012345678901234567890",
            ),
            ("-9007199254740993", "-9007199254740993"),
            ("1e400", "Infinity"),
            ("-1e400", "-Infinity"),
            ("NaN", "NaN"),
            ("Infinity", "Infinity"),
        ];
        for (raw, expected) in cases {
            let v = parse_json(raw).unwrap_or_else(|e| panic!("{raw}: {e}"));
            assert_eq!(dumps(&v, true, true), expected, "json.dumps({raw})");
        }
        assert_eq!(py_float_repr(f64::INFINITY), "inf");
        assert_eq!(py_float_repr(f64::NAN), "nan");
        assert!(parse_json("01").is_err());
        assert!(parse_json("1.").is_err());
        assert!(parse_json("[1,]").is_err());
        assert!(
            parse_json("{\"a\":1} x").is_err(),
            "extra data must be rejected"
        );
    }

    #[test]
    fn lone_surrogate_escapes_survive_parse_and_dumps() {
        let v = parse_json(r#"{"q": "\ud800 x \udfff", "pair": "\ud83d\ude00"}"#).unwrap();
        assert_eq!(
            dumps(&v, true, true),
            r#"{"pair":"\ud83d\ude00","q":"\ud800 x \udfff"}"#
        );
        let q = v.get("q").unwrap().as_pystr().unwrap();
        assert!(q.contains(&0xD800));
        // Redaction rebuilds from code points: a lone surrogate AND a genuine
        // private-use character (the placeholder's own range) both survive.
        let mixed: PyStr = vec![0xD800, 0xF0000, 'x' as u32, 0xDFFF];
        assert_eq!(redact_cps(&mixed), mixed);
        // ...and a real secret next to them is still scrubbed, exactly.
        let mut with_secret = vec![0xD800, ' ' as u32];
        with_secret.extend(pystr("sk-ant-api03-ABCDEFGHIJKLMNOP"));
        with_secret.push(0xF0000);
        let mut expected = vec![0xD800, ' ' as u32];
        expected.extend(pystr("sk-ant-***REDACTED***"));
        expected.push(0xF0000);
        assert_eq!(redact_cps(&with_secret), expected);
        // A high surrogate followed by a non-low escape stays two code points.
        let v = parse_json(r#""\ud800\u0041""#).unwrap();
        assert_eq!(v, PyValue::Str(vec![0xD800, 0x41]));
    }

    #[test]
    fn containers_render_with_python_str_semantics() {
        let v = parse_json(r#"[true, null, 1.5, "it's", {"k": [1, "a\"b"]}]"#).unwrap();
        assert_eq!(
            py_repr(&v),
            r#"[True, None, 1.5, "it's", {'k': [1, 'a"b']}]"#
        );
        let v = parse_json(r#"{"a": 1, "b": {"c": null}}"#).unwrap();
        assert_eq!(py_repr(&v), "{'a': 1, 'b': {'c': None}}");
        assert_eq!(
            py_str_repr(&pystr("tab\there\u{7f}\u{e9}")),
            "'tab\\there\\x7fé'"
        );
        assert_eq!(py_str_repr(&[0xD800]), "'\\ud800'");
        // Duplicate keys: first position, last value (dict semantics).
        let v = parse_json(r#"{"b": 1, "a": 2, "b": 3}"#).unwrap();
        assert_eq!(py_repr(&v), "{'b': 3, 'a': 2}");
        // A list-valued file_path renders via str() in the canonical text.
        let rules = load_seed_rules();
        let raw = make_payload_default(
            "Write",
            json!({"file_path": ["foo.test.ts", null, true], "content": "spawnSync('ls')"}),
        );
        match decide(&rules, &raw) {
            Outcome::Fired {
                ctx,
                winner: Some(w),
                ..
            } => {
                assert_eq!(w.rule_id, "vitest-spawnsync");
                assert_eq!(
                    cps_to_string(&ctx.preview),
                    "['foo.test.ts', None, True]\nspawnSync('ls')"
                );
            }
            other => panic!("expected Fired, got {other:?}"),
        }
    }

    #[test]
    fn stdout_document_matches_python_dumps_bytes() {
        let deny = Fire {
            rule_id: PyValue::str("r"),
            decision: PyValue::str("deny"),
            message: PyValue::str("no — stop"),
            matched: pystr("x"),
        };
        assert_eq!(
            stdout_document(&deny),
            "{\"hookSpecificOutput\": {\"hookEventName\": \"PreToolUse\", \"permissionDecision\": \"deny\", \"permissionDecisionReason\": \"no \\u2014 stop\"}}\n"
        );
        let prior = Fire {
            decision: PyValue::str("prior"),
            ..deny
        };
        assert_eq!(
            stdout_document(&prior),
            "{\"hookSpecificOutput\": {\"hookEventName\": \"PreToolUse\", \"additionalContext\": \"no \\u2014 stop\"}}\n"
        );
    }

    // ---- Review F9: ledger append recovers an incomplete tail ----

    #[test]
    fn ledger_append_isolates_an_incomplete_tail_left_by_a_short_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LEDGER_FILENAME);
        std::fs::write(&path, "{\"complete\": 1}\n{\"trunc").unwrap();
        append_ledger_at(&path, "{\"new\": 1}\n{\"new\": 2}\n").unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(
            lines,
            vec![
                "{\"complete\": 1}",
                "{\"trunc",
                "{\"new\": 1}",
                "{\"new\": 2}"
            ]
        );
        // Every line except the isolated fragment parses.
        for l in [lines[0], lines[2], lines[3]] {
            serde_json::from_str::<serde_json::Value>(l).unwrap();
        }
        // A clean tail is left alone: no blank line is inserted.
        append_ledger_at(&path, "{\"new\": 3}\n").unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(
            !content.contains("\n\n"),
            "no empty line on a clean append: {content:?}"
        );
        assert!(content.ends_with("{\"new\": 3}\n"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn one_line_escapes_every_line_break() {
        assert_eq!(one_line("a\nb\r\nc\rd"), "a\\nb\\nc\\nd");
        let err = build_regex("(unclosed").unwrap_err();
        assert!(
            err.contains('\n'),
            "the regex crate's error is multi-line: {err:?}"
        );
        assert!(!one_line(&err).contains('\n'));
    }

    #[test]
    fn posix_normpath_matches_python() {
        for (input, expected) in [
            ("/worktrees/x/../../shared/checkout", "/shared/checkout"),
            ("//a/b", "//a/b"),
            ("///a/b", "/a/b"),
            ("a/./b/../c", "a/c"),
            ("../a", "../a"),
            ("/../a", "/a"),
            ("", "."),
            ("a/", "a"),
        ] {
            assert_eq!(
                cps_to_string(&posix_normpath(&pystr(input))),
                expected,
                "{input}"
            );
        }
        let j = |a: &str, b: &str| cps_to_string(&posix_join(&pystr(a), &pystr(b)));
        assert_eq!(j("/a", "b"), "/a/b");
        assert_eq!(j("/a/", "b"), "/a/b");
        assert_eq!(j("/a", "/b"), "/b");
        assert_eq!(j("", "b"), "b");
        // Code points that have no faithful `String` form survive normpath.
        let odd: PyStr = vec![SLASH, 0xD800, SLASH, DOT, DOT, SLASH, 0xF0000];
        assert_eq!(posix_normpath(&odd), vec![SLASH, 0xF0000]);
    }

    // ---- Review round 2: F14 / F17 / F18 ----

    /// F14: a lone-surrogate heredoc delimiter must still terminate its
    /// own body — the delimiter is sliced from the original code points, so
    /// the command after the terminator line is evaluated like Python does.
    #[test]
    fn surrogate_heredoc_delimiter_terminates_like_the_reference() {
        let rules = load_seed_rules();
        let raw = r#"{"tool_name":"Bash","cwd":"/shared","tool_input":{"command":"cat <<'\ud800'\nbody\n\ud800\ngit stash"}}"#;
        let w = winner_of(decide(&rules, raw));
        assert_eq!(w.decision, "deny");
        // Differential: the live reference denies too, with identical bytes.
        let ledger_dir = tempfile::tempdir().unwrap();
        let out = run_python_reference(raw, ledger_dir.path());
        assert!(out.status.success() && out.stderr.is_empty());
        assert_eq!(String::from_utf8_lossy(&out.stdout), stdout_document(&w));
        // A genuine private-use char next to a lone surrogate keeps both in
        // the ledger preview, exactly as the reference writes them.
        let raw2 = "{\"tool_name\":\"Bash\",\"cwd\":\"/shared\",\"tool_input\":{\"command\":\"echo \\ud800\u{F0000}; git stash\"}}";
        let (fires, ctx) = match decide(&rules, raw2) {
            Outcome::Fired { fires, ctx, .. } => (fires, ctx),
            other => panic!("expected Fired, got {other:?}"),
        };
        let line = ledger_lines_for(&fires, &ctx, "TS").unwrap();
        assert!(line.contains(r#"\ud800\udb80\udc00"#), "{line}");
        let ledger_dir = tempfile::tempdir().unwrap();
        run_python_reference(raw2, ledger_dir.path());
        let py = std::fs::read_to_string(ledger_dir.path().join(LEDGER_FILENAME)).unwrap();
        let strip = |s: &str| {
            Regex::new(r#""ts": "[^"]*""#)
                .unwrap()
                .replace_all(s, "\"ts\": \"TS\"")
                .into_owned()
        };
        assert_eq!(strip(&line), strip(&py));
    }

    /// F17: string decoding is linear — a 1 MiB content field parses in
    /// well under a second instead of revalidating the whole suffix per
    /// character.
    #[test]
    fn large_json_strings_parse_in_linear_time() {
        let big = "x".repeat(1 << 20);
        let raw = format!(
            r#"{{"tool_name":"Write","tool_input":{{"file_path":"notes.md","content":"{big}"}}}}"#
        );
        let started = std::time::Instant::now();
        let v = parse_json(&raw).unwrap();
        let elapsed = started.elapsed();
        assert_eq!(
            v.get("tool_input")
                .unwrap()
                .get("content")
                .unwrap()
                .as_pystr()
                .unwrap()
                .len(),
            1 << 20
        );
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "1 MiB string took {elapsed:?}"
        );
        // Non-ASCII content decodes correctly through the lead-byte path.
        let v = parse_json("\"h\u{e9}\u{1F600}\"").unwrap();
        assert_eq!(v, PyValue::Str(vec!['h' as u32, 0xE9, 0x1F600]));
    }

    /// F18: a synthetic heredoc terminator whose delimiter is `cd` is found
    /// by the cd locator PAST the unmasked text; the token read there must
    /// clamp like Python's slicing instead of indexing out of range.
    #[test]
    fn synthetic_terminator_named_cd_does_not_panic_and_matches_the_reference() {
        let rules = load_seed_rules();
        for raw in [
            r#"{"tool_name":"Bash","tool_input":{"command":"cat <<cd\nbody"}}"#,
            r#"{"tool_name":"Bash","tool_input":{"command":"cat <<cd\nbody\ngit stash"}}"#,
            r#"{"tool_name":"Bash","cwd":"/worktrees/x","tool_input":{"command":"git stash; cat <<pushd\nbody"}}"#,
        ] {
            let outcome = decide(&rules, raw);
            assert!(!matches!(outcome, Outcome::Error(_)), "{raw}: {outcome:?}");
            let ledger_dir = tempfile::tempdir().unwrap();
            let out = run_python_reference(raw, ledger_dir.path());
            assert!(out.status.success() && out.stderr.is_empty(), "{raw}");
            let expected = match &outcome {
                Outcome::Fired {
                    winner: Some(w), ..
                } => stdout_document(w),
                _ => String::new(),
            };
            assert_eq!(String::from_utf8_lossy(&out.stdout), expected, "{raw}");
        }
    }

    /// `executable_mask` output is compared against the reference's own
    /// `executable_mask` (imported from the script) — the scanner is the
    /// largest piece of the port and its output feeds every Bash rule.
    #[test]
    fn executable_mask_matches_the_python_reference_scanner() {
        let inputs = [
            "echo 'a; b' \"c | d\" # note\ncat <<EOF\nrun `x` now\nEOF\n",
            "FOO=\"a b\" git stash; x=`echo hi # c`; echo RAN",
            "echo \"$(printf '%s\\n' 'then; a stash')\" && cd \"$DIR\" || { echo no; }",
            "cat <<-'H'\n\tquoted body; git stash\n\tH\necho after\ncat <<EOF\n$(echo `x`)\nEOF",
            "python3 - <<PYEOF\nprint('run `boi start` now')\nPYEOF",
            "echo \\'; git stash \\\n-u; git -c 'a=b c' stash",
            "cat <<EOF\nnever terminated `x`",
            "echo ${#HOME}; git stash # ${#x}",
        ];
        let script = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("hooks/scripts/pretooluse-router.py");
        let program = format!(
            "import importlib.util, json, sys\n\
             spec = importlib.util.spec_from_file_location('r', {script:?})\n\
             m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)\n\
             for text in json.load(sys.stdin):\n\
             \x20   q, l = [], []\n\
             \x20   print(json.dumps([m.executable_mask(text, q, l), q, l]))\n",
            script = script.to_string_lossy()
        );
        let out = Command::new("python3")
            .args(["-c", &program])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                child
                    .stdin
                    .take()
                    .unwrap()
                    .write_all(serde_json::to_string(&inputs).unwrap().as_bytes())?;
                child.wait_with_output()
            })
            .expect("run python");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8(out.stdout).unwrap();
        for (input, line) in inputs.iter().zip(stdout.lines()) {
            let expected: (String, Vec<(usize, usize)>, Vec<(usize, usize)>) =
                serde_json::from_str(line).unwrap();
            let text = CpText::new(pystr(input));
            let mut spans = Spans::default();
            let masked = cps_to_string(&executable_mask(&text, &mut spans));
            assert_eq!(masked, expected.0, "mask differs for {input:?}");
            assert_eq!(spans.quote, expected.1, "quote spans differ for {input:?}");
            assert_eq!(spans.live, expected.2, "live spans differ for {input:?}");
        }
        assert_eq!(stdout.lines().count(), inputs.len());
    }

    /// Structural guard: the router must stay pure regex + std — no
    /// subprocess/network calls, and no panicking output macros (review F8).
    #[test]
    fn production_code_has_no_subprocess_network_or_panicking_output_calls() {
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
            "println!",
            "eprintln!",
        ];
        for tok in forbidden {
            assert!(
                !production_only.contains(tok),
                "router.rs production code contains forbidden token {tok:?}"
            );
        }
    }

    /// Optional comparison against an EXTERNAL executable (`HEX_ROUTER_BIN`)
    /// — e.g. a release build or a previously deployed binary. Kept separate
    /// from the required current-build differential in
    /// `tests/hook_router_cli.rs` (review F1); skipped when unset.
    #[test]
    fn external_binary_matches_python_reference_when_provided() {
        let Ok(bin) = std::env::var("HEX_ROUTER_BIN") else {
            eprintln!("HEX_ROUTER_BIN unset — external-binary differential skipped");
            return;
        };
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let fake_hex_dir = tempfile::tempdir().unwrap();
        let hooks = fake_hex_dir.path().join(".hex/hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        std::fs::copy(
            repo_root.join("hooks/router-rules.json"),
            hooks.join("router-rules.json"),
        )
        .unwrap();
        for fx in rule_fixtures() {
            for (tool, input) in [
                (fx.tool_name, fx.positive.clone()),
                (fx.near_miss_tool_name, fx.near_miss.clone()),
            ] {
                let raw = make_payload_default(tool, input);
                let py_ledger = tempfile::tempdir().unwrap();
                let py_out = run_python_reference(&raw, py_ledger.path());
                let rs_ledger = tempfile::tempdir().unwrap();
                let rs_out = Command::new(&bin)
                    .args(["hook", "router"])
                    .env("HEX_DIR", fake_hex_dir.path())
                    .env("HEX_LEDGER_DIR", rs_ledger.path())
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .and_then(|mut child| {
                        use std::io::Write;
                        child.stdin.take().unwrap().write_all(raw.as_bytes())?;
                        child.wait_with_output()
                    })
                    .expect("failed to run the external router binary");
                assert_eq!(py_out.stdout, rs_out.stdout, "{}/{tool}: stdout", fx.id);
                let strip = |p: &Path| -> String {
                    let raw = std::fs::read_to_string(p.join(LEDGER_FILENAME)).unwrap_or_default();
                    Regex::new(r#""ts": "[^"]*""#)
                        .unwrap()
                        .replace_all(&raw, "\"ts\": \"TS\"")
                        .into_owned()
                };
                assert_eq!(
                    strip(py_ledger.path()),
                    strip(rs_ledger.path()),
                    "{}/{tool}: ledger",
                    fx.id
                );
            }
        }
    }
}
