//! Lean-by-default `claude -p` profile resolver.
//!
//! Headless invocations of Claude Code (harness question worker, meeting-prep
//! cron, eval harness, etc.) should NOT inherit the full workspace plugin /
//! skill / MCP / CLAUDE.md stack. This module owns the policy: a small
//! built-in registry of profiles, optionally overridden by
//! `$HEX_DIR/.hex/config/claude-runs.toml`, that resolves to a vector of CLI
//! flags suitable for prepending to a `claude -p ...` invocation.
//!
//! See spec Sf5bj7y1d.
//!
//! Lean default = `--bare` + `--strict-mcp-config --mcp-config '{}'`. Bare
//! skips plugin/skill/CLAUDE.md/auto-memory discovery; the empty strict mcp
//! config ensures no MCP server loads even if a future Claude Code version
//! changes what `--bare` covers.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Fully resolved profile — what the call site should pass to `claude -p`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRun {
    pub name: String,
    pub bare: bool,
    /// Names of MCP servers (looked up in workspace `.mcp.json`) that should
    /// be re-enabled for this run. Empty = no MCP servers.
    pub mcp_servers: Vec<String>,
    pub plugin_dirs: Vec<String>,
    pub setting_sources: Vec<String>,
    pub allowed_tools: Vec<String>,
    pub disable_slash_commands: bool,
    pub extra_flags: Vec<String>,
}

impl ResolvedRun {
    fn lean(name: &str) -> Self {
        Self {
            name: name.to_string(),
            bare: true,
            mcp_servers: Vec::new(),
            plugin_dirs: Vec::new(),
            setting_sources: Vec::new(),
            allowed_tools: Vec::new(),
            disable_slash_commands: true,
            extra_flags: Vec::new(),
        }
    }

    /// Emit the CLI flags as a `Vec<String>` ready for `Command::args` or for
    /// shell-quoted printing.
    ///
    /// Flag emission rules (from the spec contract):
    /// - `bare=true` → `--bare`
    /// - `mcp_servers` nonempty → `--strict-mcp-config --mcp-config '<inline json>'`
    ///   containing ONLY the named servers (looked up in the workspace mcp
    ///   config). Loud error if a named server isn't found — handled by
    ///   `to_cli_flags_with_mcp_lookup`.
    /// - `mcp_servers` empty/absent → `--strict-mcp-config --mcp-config '{}'`
    /// - `disable_slash_commands=true` → `--disable-slash-commands`
    /// - `plugin_dirs` → repeated `--plugin-dir <dir>`
    /// - `setting_sources` nonempty → `--setting-sources a,b,c`
    /// - `allowed_tools` nonempty → `--allowedTools "<space-joined>"`
    /// - `extra_flags` appended verbatim
    pub fn to_cli_flags(&self, mcp_config: &McpConfig) -> Result<Vec<String>, ClaudeRunsError> {
        let mut out = Vec::new();
        if self.bare {
            out.push("--bare".to_string());
        }
        // Always emit a --strict-mcp-config so MCP discovery is bounded.
        out.push("--strict-mcp-config".to_string());
        let mcp_json = if self.mcp_servers.is_empty() {
            "{}".to_string()
        } else {
            mcp_config.subset_json(&self.mcp_servers)?
        };
        out.push("--mcp-config".to_string());
        out.push(mcp_json);
        if self.disable_slash_commands {
            out.push("--disable-slash-commands".to_string());
        }
        for dir in &self.plugin_dirs {
            out.push("--plugin-dir".to_string());
            out.push(dir.clone());
        }
        if !self.setting_sources.is_empty() {
            out.push("--setting-sources".to_string());
            out.push(self.setting_sources.join(","));
        }
        if !self.allowed_tools.is_empty() {
            out.push("--allowedTools".to_string());
            out.push(self.allowed_tools.join(" "));
        }
        out.extend(self.extra_flags.iter().cloned());
        Ok(out)
    }
}

/// Workspace MCP config (subset of `.mcp.json` / .claude config we care about).
#[derive(Debug, Clone, Default)]
pub struct McpConfig {
    /// Map of server name → its raw JSON entry, as it appears in
    /// `mcpServers` of the workspace config.
    pub servers: BTreeMap<String, serde_json::Value>,
}

impl McpConfig {
    /// Empty config (no MCP servers known) — used when no workspace config
    /// file is present. Any non-empty `mcp_servers` request will then error
    /// loudly.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Load from a workspace path. Looks for `<workspace>/.mcp.json` first,
    /// falling back to `<workspace>/.claude/mcp.json`. Returns
    /// `McpConfig::empty()` if neither exists. Errors loudly on malformed
    /// JSON.
    pub fn load(workspace: &Path) -> Result<Self, ClaudeRunsError> {
        let candidates = [
            workspace.join(".mcp.json"),
            workspace.join(".claude").join("mcp.json"),
        ];
        for path in &candidates {
            if path.exists() {
                let raw = std::fs::read_to_string(path).map_err(|e| {
                    ClaudeRunsError::McpConfigUnreadable {
                        path: path.clone(),
                        source: e.to_string(),
                    }
                })?;
                let v: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
                    ClaudeRunsError::McpConfigMalformed {
                        path: path.clone(),
                        source: e.to_string(),
                    }
                })?;
                let mut servers = BTreeMap::new();
                if let Some(map) = v.get("mcpServers").and_then(|m| m.as_object()) {
                    for (k, val) in map {
                        servers.insert(k.clone(), val.clone());
                    }
                }
                return Ok(Self { servers });
            }
        }
        Ok(Self::empty())
    }

    /// Build the inline `--mcp-config` JSON containing ONLY the named
    /// servers. Loud error if any name is missing from the workspace config.
    pub fn subset_json(&self, names: &[String]) -> Result<String, ClaudeRunsError> {
        let mut subset = serde_json::Map::new();
        for name in names {
            match self.servers.get(name) {
                Some(v) => {
                    subset.insert(name.clone(), v.clone());
                }
                None => {
                    return Err(ClaudeRunsError::UnknownMcpServer {
                        name: name.clone(),
                        known: self.servers.keys().cloned().collect(),
                    });
                }
            }
        }
        let mut root = serde_json::Map::new();
        root.insert("mcpServers".to_string(), serde_json::Value::Object(subset));
        Ok(serde_json::Value::Object(root).to_string())
    }
}

/// Errors from profile resolution / flag emission. All map to LOUD failures
/// (Standing Order #6).
#[derive(Debug)]
pub enum ClaudeRunsError {
    UnknownProfile { name: String, known: Vec<String> },
    ConfigUnreadable { path: PathBuf, source: String },
    ConfigMalformed { path: PathBuf, source: String },
    McpConfigUnreadable { path: PathBuf, source: String },
    McpConfigMalformed { path: PathBuf, source: String },
    UnknownMcpServer { name: String, known: Vec<String> },
}

impl std::fmt::Display for ClaudeRunsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownProfile { name, known } => write!(
                f,
                "claude-runs: unknown profile {:?}. Known profiles: {}",
                name,
                known.join(", ")
            ),
            Self::ConfigUnreadable { path, source } => write!(
                f,
                "claude-runs: cannot read config file {}: {}",
                path.display(),
                source
            ),
            Self::ConfigMalformed { path, source } => write!(
                f,
                "claude-runs: malformed config file {}: {}",
                path.display(),
                source
            ),
            Self::McpConfigUnreadable { path, source } => write!(
                f,
                "claude-runs: cannot read mcp config {}: {}",
                path.display(),
                source
            ),
            Self::McpConfigMalformed { path, source } => write!(
                f,
                "claude-runs: malformed mcp config {}: {}",
                path.display(),
                source
            ),
            Self::UnknownMcpServer { name, known } => write!(
                f,
                "claude-runs: mcp server {:?} not found in workspace mcp config. Known servers: [{}]",
                name,
                known.join(", ")
            ),
        }
    }
}

impl std::error::Error for ClaudeRunsError {}

/// Built-in lean profiles. Returned when no config file is present, or used
/// as the fallback when the config file omits a profile.
fn builtin(name: &str) -> Option<ResolvedRun> {
    match name {
        "default" | "harness_worker" | "eval" => Some(ResolvedRun::lean(name)),
        "meeting_prep" => {
            let mut r = ResolvedRun::lean(name);
            // Meeting prep needs access to Google Calendar via MCP.
            // Server name matches the workspace .mcp.json entry.
            r.mcp_servers = vec!["google-calendar".to_string()];
            Some(r)
        }
        _ => None,
    }
}

fn known_profile_names() -> Vec<String> {
    vec![
        "default".to_string(),
        "harness_worker".to_string(),
        "meeting_prep".to_string(),
        "eval".to_string(),
    ]
}

/// Resolve a profile, consulting the per-workspace config file if present.
///
/// `hex_dir` is the workspace root (where `.hex/config/claude-runs.toml`
/// lives). Pass `None` to skip the config file entirely (built-ins only).
pub fn resolve(profile: &str, hex_dir: Option<&Path>) -> Result<ResolvedRun, ClaudeRunsError> {
    let config_path =
        hex_dir.map(|d| d.join(".hex").join("config").join("claude-runs.toml"));

    // If the config file exists, parse it.
    let parsed = match &config_path {
        Some(p) if p.exists() => Some(parse_config_file(p)?),
        _ => None,
    };

    // Start from built-in if known.
    let mut resolved = builtin(profile);

    // Apply config-file overrides.
    if let Some(cfg) = &parsed {
        // Defaults section applies to every profile, even built-ins. If the
        // profile is not built-in and not present in the config, that's an
        // unknown profile (hard error).
        if let Some(run) = cfg.runs.get(profile) {
            let base = resolved.unwrap_or_else(|| ResolvedRun::lean(profile));
            resolved = Some(merge(base, &cfg.defaults, Some(run)));
        } else if resolved.is_some() {
            let base = resolved.unwrap();
            resolved = Some(merge(base, &cfg.defaults, None));
        }
    }

    resolved.ok_or_else(|| ClaudeRunsError::UnknownProfile {
        name: profile.to_string(),
        known: {
            let mut k = known_profile_names();
            if let Some(cfg) = &parsed {
                for name in cfg.runs.keys() {
                    if !k.contains(name) {
                        k.push(name.clone());
                    }
                }
            }
            k
        },
    })
}

fn merge(
    mut base: ResolvedRun,
    defaults: &ProfileFields,
    run: Option<&ProfileFields>,
) -> ResolvedRun {
    apply(&mut base, defaults);
    if let Some(r) = run {
        apply(&mut base, r);
    }
    base
}

fn apply(target: &mut ResolvedRun, src: &ProfileFields) {
    if let Some(b) = src.bare {
        target.bare = b;
    }
    if let Some(v) = &src.mcp_servers {
        target.mcp_servers = v.clone();
    }
    if let Some(v) = &src.plugin_dirs {
        target.plugin_dirs = v.clone();
    }
    if let Some(v) = &src.setting_sources {
        target.setting_sources = v.clone();
    }
    if let Some(v) = &src.allowed_tools {
        target.allowed_tools = v.clone();
    }
    if let Some(b) = src.disable_slash_commands {
        target.disable_slash_commands = b;
    }
    if let Some(v) = &src.extra_flags {
        target.extra_flags = v.clone();
    }
}

/// Per-profile field block. All-`Option` so we can distinguish "absent" from
/// "explicit empty list".
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct ProfileFields {
    bare: Option<bool>,
    mcp_servers: Option<Vec<String>>,
    plugin_dirs: Option<Vec<String>>,
    setting_sources: Option<Vec<String>>,
    allowed_tools: Option<Vec<String>>,
    disable_slash_commands: Option<bool>,
    extra_flags: Option<Vec<String>>,
}

#[derive(Debug, Default)]
struct ParsedConfig {
    defaults: ProfileFields,
    runs: BTreeMap<String, ProfileFields>,
}

fn parse_config_file(path: &Path) -> Result<ParsedConfig, ClaudeRunsError> {
    let raw = std::fs::read_to_string(path).map_err(|e| ClaudeRunsError::ConfigUnreadable {
        path: path.to_path_buf(),
        source: e.to_string(),
    })?;
    parse_config_str(&raw).map_err(|e| ClaudeRunsError::ConfigMalformed {
        path: path.to_path_buf(),
        source: e,
    })
}

/// Tiny hand-rolled TOML reader for the documented schema. Supports:
///   - section headers `[defaults]`, `[runs.NAME]`
///   - `key = true|false` (bool)
///   - `key = "string"` (string)
///   - `key = ["a", "b"]` (list of strings, on one line)
///   - `# ...` line comments
/// Anything else is a parse error (loud, with line number).
fn parse_config_str(raw: &str) -> Result<ParsedConfig, String> {
    let mut cfg = ParsedConfig::default();
    enum Section {
        None,
        Defaults,
        Run(String),
    }
    let mut section = Section::None;
    for (idx, line_raw) in raw.lines().enumerate() {
        let line_no = idx + 1;
        let line = strip_comment(line_raw).trim();
        if line.is_empty() {
            continue;
        }
        if let Some(hdr) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            let hdr = hdr.trim();
            if hdr == "defaults" {
                section = Section::Defaults;
            } else if let Some(name) = hdr.strip_prefix("runs.") {
                section = Section::Run(name.trim().to_string());
            } else {
                return Err(format!("line {line_no}: unknown section header [{hdr}]"));
            }
            continue;
        }
        // key = value
        let (key, val) = line
            .split_once('=')
            .ok_or_else(|| format!("line {line_no}: expected `key = value`"))?;
        let key = key.trim();
        let val = val.trim();
        let target = match &mut section {
            Section::None => {
                return Err(format!(
                    "line {line_no}: key {key:?} outside any [defaults] or [runs.X] section"
                ));
            }
            Section::Defaults => &mut cfg.defaults,
            Section::Run(name) => cfg.runs.entry(name.clone()).or_default(),
        };
        assign(target, key, val, line_no)?;
    }
    Ok(cfg)
}

fn strip_comment(line: &str) -> &str {
    // Naive: '#' starts a comment unless inside a string. The schema uses
    // simple strings only and the parser is internal; we avoid quoted '#'.
    let mut in_str = false;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_str = !in_str,
            '#' if !in_str => return &line[..i],
            _ => {}
        }
    }
    line
}

fn assign(fields: &mut ProfileFields, key: &str, val: &str, line_no: usize) -> Result<(), String> {
    match key {
        "bare" => fields.bare = Some(parse_bool(val, line_no)?),
        "disable_slash_commands" => fields.disable_slash_commands = Some(parse_bool(val, line_no)?),
        "mcp_servers" => fields.mcp_servers = Some(parse_str_list(val, line_no)?),
        "plugin_dirs" => fields.plugin_dirs = Some(parse_str_list(val, line_no)?),
        "setting_sources" => fields.setting_sources = Some(parse_str_list(val, line_no)?),
        "allowed_tools" => fields.allowed_tools = Some(parse_str_list(val, line_no)?),
        "extra_flags" => fields.extra_flags = Some(parse_str_list(val, line_no)?),
        other => return Err(format!("line {line_no}: unknown key {other:?}")),
    }
    Ok(())
}

fn parse_bool(val: &str, line_no: usize) -> Result<bool, String> {
    match val {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(format!("line {line_no}: expected true/false, got {val:?}")),
    }
}

fn parse_str_list(val: &str, line_no: usize) -> Result<Vec<String>, String> {
    let inner = val
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .ok_or_else(|| format!("line {line_no}: expected [...] list, got {val:?}"))?;
    let inner = inner.trim();
    if inner.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for piece in inner.split(',') {
        let p = piece.trim();
        let s = p
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .ok_or_else(|| format!("line {line_no}: list element {p:?} must be a \"string\""))?;
        out.push(s.to_string());
    }
    Ok(out)
}

/// Quote a single shell argument with POSIX single-quote escaping. Safe for
/// eval-style substitution: `claude $(hex claude-flags X) -p ...`.
pub fn shell_quote(arg: &str) -> String {
    if !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '/' | '.' | ',' | '=' | ':'))
    {
        return arg.to_string();
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('\'');
    for c in arg.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// Render a flag vector as a single eval-safe shell line.
pub fn render_shell_line(flags: &[String]) -> String {
    flags
        .iter()
        .map(|a| shell_quote(a))
        .collect::<Vec<_>>()
        .join(" ")
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_config_uses_builtin_lean_profile() {
        let r = resolve("harness_worker", None).expect("builtin");
        assert!(r.bare);
        assert!(r.mcp_servers.is_empty());
        let flags = r.to_cli_flags(&McpConfig::empty()).expect("flags");
        assert!(flags.iter().any(|f| f == "--bare"));
        assert!(flags.iter().any(|f| f == "--strict-mcp-config"));
        // The mcp-config value immediately follows --mcp-config and must be {}
        let idx = flags.iter().position(|f| f == "--mcp-config").unwrap();
        assert_eq!(flags[idx + 1], "{}");
    }

    #[test]
    fn unknown_profile_is_a_hard_error() {
        let err = resolve("not_a_real_profile", None).expect_err("must error");
        let msg = err.to_string();
        assert!(msg.contains("unknown profile"), "got: {msg}");
    }

    #[test]
    fn meeting_prep_requests_google_calendar_mcp() {
        let r = resolve("meeting_prep", None).expect("builtin");
        assert_eq!(r.mcp_servers, vec!["google-calendar".to_string()]);
    }

    #[test]
    fn mcp_server_lookup_succeeds_for_named_server() {
        let mut servers = BTreeMap::new();
        servers.insert(
            "google-calendar".to_string(),
            serde_json::json!({"command": "calendar-mcp", "args": []}),
        );
        let cfg = McpConfig { servers };
        let r = resolve("meeting_prep", None).unwrap();
        let flags = r.to_cli_flags(&cfg).expect("flags");
        let idx = flags.iter().position(|f| f == "--mcp-config").unwrap();
        let json = &flags[idx + 1];
        assert!(json.contains("google-calendar"), "got: {json}");
        assert!(json.contains("calendar-mcp"), "got: {json}");
    }

    #[test]
    fn mcp_server_lookup_loud_error_when_missing() {
        let r = resolve("meeting_prep", None).unwrap();
        let err = r
            .to_cli_flags(&McpConfig::empty())
            .expect_err("must error — google-calendar not in empty config");
        assert!(err.to_string().contains("google-calendar"));
    }

    #[test]
    fn parses_config_file_with_named_run() {
        let toml = r#"
[defaults]
bare = true

[runs.custom_lean]
mcp_servers = ["foo", "bar"]
plugin_dirs = ["/tmp/p1"]
"#;
        let cfg = parse_config_str(toml).expect("parses");
        assert_eq!(cfg.defaults.bare, Some(true));
        let custom = cfg.runs.get("custom_lean").expect("custom run");
        assert_eq!(
            custom.mcp_servers.as_ref().unwrap(),
            &vec!["foo".to_string(), "bar".to_string()]
        );
        assert_eq!(
            custom.plugin_dirs.as_ref().unwrap(),
            &vec!["/tmp/p1".to_string()]
        );
    }

    #[test]
    fn malformed_config_is_a_hard_error() {
        let bad = "this is not valid\n";
        let err = parse_config_str(bad).expect_err("must error");
        assert!(err.contains("line 1"), "got: {err}");
    }

    #[test]
    fn resolve_reads_workspace_config_file() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_dir = tmp.path().join(".hex").join("config");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(
            cfg_dir.join("claude-runs.toml"),
            r#"
[runs.harness_worker]
mcp_servers = ["test-server"]
"#,
        )
        .unwrap();
        let r = resolve("harness_worker", Some(tmp.path())).unwrap();
        assert_eq!(r.mcp_servers, vec!["test-server".to_string()]);
        assert!(r.bare, "built-in lean bare default still applies");
    }

    #[test]
    fn resolve_user_defined_profile_via_config() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg_dir = tmp.path().join(".hex").join("config");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(
            cfg_dir.join("claude-runs.toml"),
            r#"
[runs.my_run]
bare = false
"#,
        )
        .unwrap();
        let r = resolve("my_run", Some(tmp.path())).unwrap();
        assert!(!r.bare);
    }

    #[test]
    fn shell_quote_handles_special_chars() {
        assert_eq!(shell_quote("simple"), "simple");
        assert_eq!(shell_quote("--bare"), "--bare");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
        assert_eq!(shell_quote("{}"), "'{}'");
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn render_shell_line_is_eval_safe_single_line() {
        let flags = vec![
            "--bare".to_string(),
            "--strict-mcp-config".to_string(),
            "--mcp-config".to_string(),
            "{}".to_string(),
        ];
        let line = render_shell_line(&flags);
        assert_eq!(line, "--bare --strict-mcp-config --mcp-config '{}'");
        assert!(!line.contains('\n'));
    }

    #[test]
    fn mcp_config_load_returns_empty_when_no_file() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = McpConfig::load(tmp.path()).unwrap();
        assert!(cfg.servers.is_empty());
    }

    #[test]
    fn mcp_config_load_parses_workspace_mcp_json() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join(".mcp.json"),
            r#"{"mcpServers": {"foo": {"command": "x"}}}"#,
        )
        .unwrap();
        let cfg = McpConfig::load(tmp.path()).unwrap();
        assert!(cfg.servers.contains_key("foo"));
    }
}
