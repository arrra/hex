//! `claude-cli` transport for the LLM provider seam.
//!
//! Spec Sbe8m4886, task T0mnyfwa7. Shells out to a headless `claude -p`
//! process authenticated via the macOS login keychain (NOT the setup-token —
//! see decision record `memory-cli-transport-no-setup-token-2026-06-10.md`).
//! Used as an alternative to the HTTP transport for any LLM use case whose
//! `[use_cases.<name>].transport = "claude-cli"` in llm.toml.
//!
//! Verified recipe (live box 2026-06-10, claude 2.1.170). DO NOT change any
//! of these arg flags or settings keys without re-verifying on a real box —
//! several of them silently void the entire payload if mis-typed (the most
//! treacherous: `disableDeepLinkRegistration` takes the STRING `"disable"`;
//! a boolean voids the whole settings JSON with zero stderr).
//!
//! ## Caveats
//! * `max_tokens` is NOT enforceable via `claude -p` — the CLI has no flag for
//!   it. Callers wanting a hard cap must use the HTTP transport. The argument
//!   is accepted here for API symmetry with `provider::generate_inner` but
//!   discarded.
//! * Auth is keychain-only by design. The harness self-loads the
//!   setup-token into its own process env; we strip
//!   `CLAUDE_CODE_OAUTH_TOKEN`, `ANTHROPIC_API_KEY`, and `ANTHROPIC_AUTH_TOKEN`
//!   from the child env so claude falls through to the login keychain
//!   (verified working from this harness context). Any of those vars in the
//!   child env would SHADOW the keychain per claude's auth precedence.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use super::provider::ProviderError;

/// Default wall-clock timeout for a single `claude -p` invocation.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(600);

/// Inline default `--settings` JSON, passed as the argument value of
/// `--settings <json>` when no `claude_settings_file` is configured. Kept as
/// a single-line constant so the verified field set is unambiguous.
///
/// CRITICAL: `disableDeepLinkRegistration` takes the STRING `"disable"` —
/// passing a boolean silently voids the ENTIRE settings payload with zero
/// stderr (verified). Every other field below was likewise verified on a live
/// box on 2026-06-10.
pub const DEFAULT_SETTINGS_JSON: &str = r#"{"disableAllHooks":true,"autoMemoryEnabled":false,"disableBundledSkills":true,"disableWorkflows":true,"disableAgentView":true,"disableRemoteControl":true,"disableSkillShellExecution":true,"disableDeepLinkRegistration":"disable","includeCoAuthoredBy":false,"includeGitInstructions":false,"spinnerTipsEnabled":false,"awaySummaryEnabled":false,"showTurnDuration":false,"showThinkingSummaries":false,"alwaysThinkingEnabled":false,"enableAllProjectMcpServers":false,"enabledPlugins":{},"env":{"DISABLE_AUTOUPDATER":"1","DISABLE_TELEMETRY":"1","DISABLE_ERROR_REPORTING":"1","DISABLE_NON_ESSENTIAL_MODEL_CALLS":"1","CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC":"1","CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY":"1"}}"#;

/// Map an OpenRouter-style model id to the form `claude -p --model` wants.
///
/// * "anthropic/claude-sonnet-4.5" → "claude-sonnet-4-5":
///   strip the "anthropic/" prefix, then replace `.` with `-` ONLY in the
///   trailing version segment (everything after the last `-`).
/// * Models without an "anthropic/" prefix pass through verbatim — lets
///   llm.toml specify CLI aliases like "sonnet" directly.
pub fn map_model_to_cli(model: &str) -> String {
    let Some(rest) = model.strip_prefix("anthropic/") else {
        return model.to_string();
    };
    match rest.rfind('-') {
        Some(idx) => {
            let (head, tail) = rest.split_at(idx);
            format!("{head}{}", tail.replace('.', "-"))
        }
        None => rest.replace('.', "-"),
    }
}

/// Build the argv (excluding the binary name) for a `claude -p` invocation.
/// The argument order is the exact verified recipe from the spec — do not
/// reorder without re-verifying on a live box.
pub fn build_args(prompt: &str, settings_arg: &str, cli_model: &str) -> Vec<String> {
    vec![
        "-p".into(),
        prompt.into(),
        "--strict-mcp-config".into(),
        "--mcp-config".into(),
        r#"{"mcpServers":{}}"#.into(),
        "--no-session-persistence".into(),
        "--setting-sources".into(),
        String::new(),
        "--disable-slash-commands".into(),
        "--settings".into(),
        settings_arg.into(),
        "--model".into(),
        cli_model.into(),
        "--output-format".into(),
        "json".into(),
    ]
}

/// Decide the value to pass after `--settings`: a file path when
/// `claude_settings_file` is `Some(...)` and the file exists, otherwise the
/// inline `DEFAULT_SETTINGS_JSON`. A configured-but-missing settings file is
/// a Deferred error (S6 — never silently fall back).
fn settings_arg_value(claude_settings_file: Option<&str>) -> Result<String, ProviderError> {
    match claude_settings_file {
        Some(path) => {
            let p = Path::new(path);
            if !p.exists() {
                return Err(ProviderError::Deferred(format!(
                    "claude_settings_file does not exist: {path}"
                )));
            }
            Ok(path.to_string())
        }
        None => Ok(DEFAULT_SETTINGS_JSON.to_string()),
    }
}

/// RAII pidfile for a spawned `claude -p` child. The child runs in its own
/// process group (see `build_command`), so if the harness's group is killed
/// (launchd stop) the child survives, orphaned to PID 1, with no timeout
/// enforcement left. The pidfile makes it findable by `reaper::sweep` at the
/// next serve startup; Drop removes the file on EVERY exit path (success,
/// timeout-kill, error) so live runs never leave stale entries.
struct PidfileGuard(Option<std::path::PathBuf>);

impl PidfileGuard {
    fn new(child_pid: u32) -> Self {
        let p = std::env::var("HEX_DIR").ok().map(|d| {
            let dir = std::path::Path::new(&d).join(".hex/run/distill");
            let _ = std::fs::create_dir_all(&dir);
            let p = dir.join(format!("distill-{child_pid}.pid"));
            if let Err(e) = std::fs::write(&p, b"") {
                eprintln!("claude_cli: pidfile write failed ({}): {e}", p.display());
            }
            p
        });
        PidfileGuard(p)
    }
}

impl Drop for PidfileGuard {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// Generate a completion via the `claude -p` headless transport.
///
/// `use_case` flows through to the cost-telemetry log line only.
pub fn generate(
    use_case: &str,
    prompt: &str,
    model: &str,
    _max_tokens: u32,
    claude_settings_file: Option<&str>,
) -> Result<String, ProviderError> {
    let settings = settings_arg_value(claude_settings_file)?;
    let cli_model = map_model_to_cli(model);
    let args = build_args(prompt, &settings, &cli_model);

    // CLAUDE.md auto-discovery is cwd-based; spawning from any workspace cwd
    // WILL slurp CLAUDE.md (verified). A fresh tempdir is clean. Hold the
    // TempDir guard alive for the duration of the call.
    let cwd_guard = tempfile::tempdir()
        .map_err(|e| ProviderError::Upstream(format!("tempdir for claude cwd: {e}")))?;

    let child = build_command(&args, cwd_guard.path())
        .spawn()
        .map_err(|e| ProviderError::Upstream(format!("spawn `claude` failed: {e}")))?;

    // Pidfile so reaper::sweep can find this child if WE die before it does.
    let _pidfile = PidfileGuard::new(child.id());

    let (status, stdout, stderr) = run_with_timeout(child, DEFAULT_TIMEOUT)?;

    // Hold cwd_guard alive until after the child finishes.
    drop(cwd_guard);

    if !status.success() {
        let combined = format!(
            "claude -p exited {:?}; stderr_tail={}; stdout_tail={}",
            status.code(),
            tail(&stderr, 800),
            tail(&stdout, 800),
        );
        return Err(classify_error(&stdout, &stderr, &combined));
    }

    // `--output-format json` emits one envelope object on stdout.
    let envelope: serde_json::Value = serde_json::from_str(stdout.trim()).map_err(|e| {
        ProviderError::Upstream(format!(
            "claude -p stdout not parseable as JSON: {e}; stdout_tail={}; stderr_tail={}",
            tail(&stdout, 800),
            tail(&stderr, 800),
        ))
    })?;

    if envelope.get("is_error").and_then(|v| v.as_bool()) == Some(true) {
        let result_text = envelope.get("result").and_then(|v| v.as_str()).unwrap_or("");
        let combined = format!(
            "claude -p reported is_error=true; result={result_text}; envelope={envelope}; stderr_tail={}",
            tail(&stderr, 800),
        );
        return Err(classify_error(result_text, &stderr, &combined));
    }

    // Cost telemetry seam (OBS-024 will pick this up later — do not build
    // telemetry plumbing here).
    if let Some(usage) = envelope.get("usage") {
        let in_tok = usage
            .get("input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let out_tok = usage
            .get("output_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let cost = envelope
            .get("total_cost_usd")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        eprintln!("claude-cli[{use_case}]: in={in_tok} out={out_tok} cost_usd={cost}");
    }

    let result = envelope
        .get("result")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            ProviderError::Upstream(format!(
                "claude -p envelope missing `result` string: {envelope}"
            ))
        })?;
    Ok(result.to_string())
}

/// Assemble the `Command` that `generate` will spawn. Split out so unit tests
/// can inspect the argv, env stripping, and cwd without actually invoking the
/// child.
fn build_command(args: &[String], cwd: &Path) -> Command {
    let mut cmd = Command::new("claude");
    cmd.args(args)
        .current_dir(cwd)
        // These three SHADOW the macOS login keychain per claude's auth
        // precedence. The harness loads the setup-token into its own env;
        // scrubbing them from the child env forces claude to fall through to
        // the keychain (verified working from this harness context).
        .env_remove("CLAUDE_CODE_OAUTH_TOKEN")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());
    // Put the child in its own process group so that on timeout we can SIGKILL
    // the entire tree (claude -p may fork helpers; killing only the top-level
    // pid would leave them holding the pipes open, and our read threads would
    // block on EOF — verified via the timeout shim test).
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd
}

/// Map an error to Deferred (auth-shaped, retry after config fix) vs Upstream
/// (transport / model failure). Every failure message includes claude's
/// stderr/stdout tail (S6 — no quiet failures).
fn classify_error(stdout: &str, stderr: &str, combined: &str) -> ProviderError {
    let lower = format!("{stdout}\n{stderr}").to_lowercase();
    if lower.contains("not logged in")
        || lower.contains("invalid api key")
        || lower.contains("401")
    {
        ProviderError::Deferred(combined.to_string())
    } else {
        ProviderError::Upstream(combined.to_string())
    }
}

/// Kill the entire process group rooted at `child` (we set process_group(0)
/// in `build_command`, so the child's pid equals its pgid). Falls back to a
/// plain `child.kill()` if the killpg call fails. On non-Unix, behaves as a
/// plain `child.kill()`.
fn kill_process_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        // SAFETY: libc::killpg with a valid pgid + SIGKILL is sound. We
        // ignore the return value — best effort. The fallback child.kill()
        // covers the rare case the pgid setup didn't take.
        let pid = child.id() as libc::pid_t;
        let rc = unsafe { libc::killpg(pid, libc::SIGKILL) };
        if rc != 0 {
            let _ = child.kill();
        }
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
    }
}

fn tail(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        // Find a char boundary at or before (s.len() - n) so we don't slice
        // through a multi-byte UTF-8 boundary.
        let mut start = s.len() - n;
        while start > 0 && !s.is_char_boundary(start) {
            start -= 1;
        }
        format!("...{}", &s[start..])
    }
}

/// Wait for the child, killing it after `timeout`. Spec says simple
/// thread + child.kill() is fine.
fn run_with_timeout(
    mut child: std::process::Child,
    timeout: Duration,
) -> Result<(std::process::ExitStatus, String, String), ProviderError> {
    let stdout_handle = child.stdout.take().map(|mut r| {
        std::thread::spawn(move || {
            let mut s = String::new();
            let _ = r.read_to_string(&mut s);
            s
        })
    });
    let stderr_handle = child.stderr.take().map(|mut r| {
        std::thread::spawn(move || {
            let mut s = String::new();
            let _ = r.read_to_string(&mut s);
            s
        })
    });

    let start = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if start.elapsed() >= timeout {
                    // Kill the WHOLE process group (see build_command). A bare
                    // child.kill() only signals the top-level pid; any helper
                    // claude -p spawned would keep our stdout/stderr pipes open
                    // and the read threads would block forever on EOF.
                    kill_process_tree(&mut child);
                    let _ = child.wait();
                    let so = stdout_handle.and_then(|h| h.join().ok()).unwrap_or_default();
                    let se = stderr_handle.and_then(|h| h.join().ok()).unwrap_or_default();
                    return Err(ProviderError::Upstream(format!(
                        "claude -p timed out after {}s; stdout_tail={}; stderr_tail={}",
                        timeout.as_secs(),
                        tail(&so, 800),
                        tail(&se, 800),
                    )));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                return Err(ProviderError::Upstream(format!("waitpid failed: {e}")));
            }
        }
    };

    let stdout = stdout_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();
    let stderr = stderr_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();
    Ok((status, stdout, stderr))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // ---------- pure-function tests (no PATH mucking, no shim) ----------

    #[test]
    fn map_model_strips_anthropic_prefix_and_dot_to_dash_in_trailing_segment() {
        assert_eq!(
            map_model_to_cli("anthropic/claude-sonnet-4.5"),
            "claude-sonnet-4-5",
        );
        assert_eq!(
            map_model_to_cli("anthropic/claude-haiku-4.5"),
            "claude-haiku-4-5",
        );
    }

    #[test]
    fn map_model_passes_non_prefixed_through_verbatim() {
        assert_eq!(map_model_to_cli("sonnet"), "sonnet");
        assert_eq!(map_model_to_cli("claude-sonnet-4-5"), "claude-sonnet-4-5");
        assert_eq!(map_model_to_cli("openai/gpt-4o"), "openai/gpt-4o");
    }

    #[test]
    fn build_args_matches_verified_recipe() {
        let args = build_args("hello", "{}", "claude-sonnet-4-5");
        // The exact arg order is the verified recipe. Spot-check both
        // structure (flags present, prompt second) and value pass-through.
        assert_eq!(args[0], "-p");
        assert_eq!(args[1], "hello");
        assert!(args.iter().any(|a| a == "--strict-mcp-config"));
        // --mcp-config must be followed by the empty MCP servers JSON.
        let mcp_idx = args.iter().position(|a| a == "--mcp-config").unwrap();
        assert_eq!(args[mcp_idx + 1], r#"{"mcpServers":{}}"#);
        assert!(args.iter().any(|a| a == "--no-session-persistence"));
        // --setting-sources must be passed an EMPTY-STRING value.
        let ss_idx = args.iter().position(|a| a == "--setting-sources").unwrap();
        assert_eq!(args[ss_idx + 1], "");
        assert!(args.iter().any(|a| a == "--disable-slash-commands"));
        // --settings must be followed by the supplied value verbatim.
        let s_idx = args.iter().position(|a| a == "--settings").unwrap();
        assert_eq!(args[s_idx + 1], "{}");
        // --model must be followed by the mapped CLI model verbatim.
        let m_idx = args.iter().position(|a| a == "--model").unwrap();
        assert_eq!(args[m_idx + 1], "claude-sonnet-4-5");
        // --output-format json must close the argv.
        let of_idx = args.iter().position(|a| a == "--output-format").unwrap();
        assert_eq!(args[of_idx + 1], "json");
    }

    #[test]
    fn default_settings_json_uses_disable_string_not_boolean() {
        // The verified gotcha: a boolean here silently voids the ENTIRE
        // settings payload. The string "disable" is the correct value.
        assert!(DEFAULT_SETTINGS_JSON.contains(r#""disableDeepLinkRegistration":"disable""#));
        assert!(!DEFAULT_SETTINGS_JSON.contains(r#""disableDeepLinkRegistration":true"#));
        assert!(!DEFAULT_SETTINGS_JSON.contains(r#""disableDeepLinkRegistration":false"#));
        // And the JSON is actually well-formed.
        let _: serde_json::Value =
            serde_json::from_str(DEFAULT_SETTINGS_JSON).expect("default settings JSON parses");
    }

    #[test]
    fn settings_arg_value_uses_inline_default_when_none() {
        let v = settings_arg_value(None).expect("inline default ok");
        assert_eq!(v, DEFAULT_SETTINGS_JSON);
    }

    #[test]
    fn settings_arg_value_passes_through_existing_file_path() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        std::fs::write(&p, "{}").unwrap();
        let v =
            settings_arg_value(Some(p.to_str().unwrap())).expect("existing file path ok");
        assert_eq!(v, p.to_str().unwrap());
    }

    #[test]
    fn settings_arg_value_loud_error_when_file_missing() {
        let result = settings_arg_value(Some("/nonexistent/path/should/not/exist.json"));
        match result {
            Err(ProviderError::Deferred(msg)) => {
                assert!(
                    msg.contains("/nonexistent/path/should/not/exist.json"),
                    "error should mention the missing path, got: {msg}"
                );
            }
            other => panic!("expected Deferred for missing settings file, got: {other:?}"),
        }
    }

    #[test]
    fn classify_error_maps_auth_shapes_to_deferred() {
        match classify_error("", "Not logged in", "combined") {
            ProviderError::Deferred(_) => {}
            other => panic!("expected Deferred for 'Not logged in', got: {other:?}"),
        }
        match classify_error("", "Invalid API key", "combined") {
            ProviderError::Deferred(_) => {}
            other => panic!("expected Deferred for 'Invalid API key', got: {other:?}"),
        }
        match classify_error("", "got 401 from server", "combined") {
            ProviderError::Deferred(_) => {}
            other => panic!("expected Deferred for '401', got: {other:?}"),
        }
    }

    #[test]
    fn classify_error_maps_other_to_upstream() {
        match classify_error("", "model not found", "combined") {
            ProviderError::Upstream(_) => {}
            other => panic!("expected Upstream for non-auth error, got: {other:?}"),
        }
    }

    // ---------- shim-based end-to-end tests ----------

    /// Write `script` to `dir/claude` with mode 0o755.
    fn install_shim(dir: &Path, script: &str) {
        let p = dir.join("claude");
        std::fs::write(&p, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    /// Run `generate` with PATH prepended to point at `shim_dir`, serializing
    /// with every other env-mutating test. Restores PATH on return.
    fn generate_with_shim(
        shim_dir: &Path,
        use_case: &str,
        prompt: &str,
        model: &str,
        claude_settings_file: Option<&str>,
    ) -> Result<String, ProviderError> {
        // isolate() (not lock_env()): generate() now writes a PidfileGuard
        // under $HEX_DIR/.hex/run/distill — point HEX_DIR at a tempdir so the
        // test never touches a real workspace.
        let (_hex_tmp, _g) = crate::telemetry::test_support::isolate();
        let old_path = std::env::var("PATH").ok();
        // Prepend the shim dir so `Command::new("claude")` resolves to it.
        let new_path = match &old_path {
            Some(p) => format!("{}:{}", shim_dir.display(), p),
            None => shim_dir.display().to_string(),
        };
        std::env::set_var("PATH", &new_path);
        // Set bogus auth env so the shim can verify env_remove() stripped it.
        std::env::set_var("CLAUDE_CODE_OAUTH_TOKEN", "should-be-stripped");
        std::env::set_var("ANTHROPIC_API_KEY", "should-be-stripped");
        std::env::set_var("ANTHROPIC_AUTH_TOKEN", "should-be-stripped");
        let result = generate(use_case, prompt, model, 0, claude_settings_file);
        match old_path {
            Some(p) => std::env::set_var("PATH", p),
            None => std::env::remove_var("PATH"),
        }
        std::env::remove_var("CLAUDE_CODE_OAUTH_TOKEN");
        std::env::remove_var("ANTHROPIC_API_KEY");
        std::env::remove_var("ANTHROPIC_AUTH_TOKEN");
        result
    }

    fn dump_args_shim(out_dir: &Path) -> String {
        // Write argv + selected env vars to a known file and emit a normal
        // success envelope on stdout.
        let dump = out_dir.join("dump.txt");
        format!(
            r#"#!/bin/sh
{{
  echo "ARGV_COUNT=$#"
  i=1
  for a in "$@"; do
    echo "ARGV[$i]=$a"
    i=$((i+1))
  done
  echo "PWD=$(pwd)"
  echo "CLAUDE_CODE_OAUTH_TOKEN=${{CLAUDE_CODE_OAUTH_TOKEN-UNSET}}"
  echo "ANTHROPIC_API_KEY=${{ANTHROPIC_API_KEY-UNSET}}"
  echo "ANTHROPIC_AUTH_TOKEN=${{ANTHROPIC_AUTH_TOKEN-UNSET}}"
}} > "{}"
cat <<'JSON'
{{"type":"result","result":"shim-says-hello","usage":{{"input_tokens":7,"output_tokens":3}},"total_cost_usd":0.0001}}
JSON
"#,
            dump.display()
        )
    }

    #[test]
    fn shim_e2e_strips_auth_env_uses_tempdir_and_returns_result() {
        let shim_dir = tempfile::tempdir().unwrap();
        let dump_dir = tempfile::tempdir().unwrap();
        install_shim(shim_dir.path(), &dump_args_shim(dump_dir.path()));

        let out = generate_with_shim(
            shim_dir.path(),
            "memory_extract",
            "hello-prompt",
            "anthropic/claude-sonnet-4.5",
            None,
        )
        .expect("shim should succeed");
        assert_eq!(out, "shim-says-hello");

        let dump = std::fs::read_to_string(dump_dir.path().join("dump.txt"))
            .expect("shim wrote dump");
        // Args must include the verified flags.
        assert!(dump.contains("ARGV[1]=-p"));
        assert!(dump.contains("ARGV[2]=hello-prompt"));
        assert!(dump.contains("=--strict-mcp-config"));
        assert!(dump.contains(r#"={"mcpServers":{}}"#));
        assert!(dump.contains("=--no-session-persistence"));
        assert!(dump.contains("=--disable-slash-commands"));
        // Mapped model is passed.
        assert!(dump.contains("=claude-sonnet-4-5"));
        // Inline default settings are passed (presence is enough — quoted).
        assert!(dump.contains("disableAllHooks"));
        assert!(dump.contains("disableDeepLinkRegistration"));
        // Auth env vars MUST have been stripped from the child env.
        assert!(
            dump.contains("CLAUDE_CODE_OAUTH_TOKEN=UNSET"),
            "CLAUDE_CODE_OAUTH_TOKEN should be stripped, dump: {dump}"
        );
        assert!(
            dump.contains("ANTHROPIC_API_KEY=UNSET"),
            "ANTHROPIC_API_KEY should be stripped, dump: {dump}"
        );
        assert!(
            dump.contains("ANTHROPIC_AUTH_TOKEN=UNSET"),
            "ANTHROPIC_AUTH_TOKEN should be stripped, dump: {dump}"
        );
        // cwd must be a tempdir, not the shim_dir or test cwd.
        let pwd_line = dump
            .lines()
            .find(|l| l.starts_with("PWD="))
            .expect("PWD line present");
        let pwd = pwd_line.trim_start_matches("PWD=");
        assert_ne!(
            PathBuf::from(pwd).canonicalize().unwrap_or_else(|_| pwd.into()),
            shim_dir
                .path()
                .canonicalize()
                .unwrap_or_else(|_| shim_dir.path().into()),
            "cwd must NOT be the shim dir"
        );
    }

    #[test]
    fn shim_e2e_is_error_envelope_maps_to_upstream() {
        let shim_dir = tempfile::tempdir().unwrap();
        let script = r#"#!/bin/sh
cat <<'JSON'
{"type":"result","result":"model not found","is_error":true}
JSON
"#;
        install_shim(shim_dir.path(), script);

        let result = generate_with_shim(
            shim_dir.path(),
            "memory_extract",
            "x",
            "anthropic/claude-sonnet-4.5",
            None,
        );
        match result {
            Err(ProviderError::Upstream(msg)) => {
                assert!(
                    msg.contains("is_error=true") || msg.contains("model not found"),
                    "error must mention envelope contents, got: {msg}"
                );
            }
            other => panic!("expected Upstream, got: {other:?}"),
        }
    }

    #[test]
    fn shim_e2e_auth_failure_maps_to_deferred() {
        let shim_dir = tempfile::tempdir().unwrap();
        // Nonzero exit + auth-shaped stderr.
        let script = r#"#!/bin/sh
echo "Not logged in" >&2
exit 1
"#;
        install_shim(shim_dir.path(), script);

        let result = generate_with_shim(
            shim_dir.path(),
            "memory_extract",
            "x",
            "anthropic/claude-sonnet-4.5",
            None,
        );
        match result {
            Err(ProviderError::Deferred(msg)) => {
                assert!(
                    msg.to_lowercase().contains("not logged in"),
                    "Deferred message must include claude's stderr, got: {msg}"
                );
            }
            other => panic!("expected Deferred for auth failure, got: {other:?}"),
        }
    }

    #[test]
    fn shim_e2e_garbage_stdout_maps_to_upstream() {
        let shim_dir = tempfile::tempdir().unwrap();
        let script = r#"#!/bin/sh
echo "this is not json"
"#;
        install_shim(shim_dir.path(), script);

        let result = generate_with_shim(
            shim_dir.path(),
            "memory_extract",
            "x",
            "anthropic/claude-sonnet-4.5",
            None,
        );
        match result {
            Err(ProviderError::Upstream(msg)) => {
                assert!(
                    msg.contains("not parseable") || msg.contains("not JSON"),
                    "Upstream message must mention parse failure, got: {msg}"
                );
            }
            other => panic!("expected Upstream for garbage stdout, got: {other:?}"),
        }
    }

    #[test]
    fn shim_e2e_timeout_kills_child_and_returns_upstream() {
        // Custom path: drive run_with_timeout directly with a tiny timeout,
        // since DEFAULT_TIMEOUT (600s) would make the suite slow.
        let shim_dir = tempfile::tempdir().unwrap();
        let script = r#"#!/bin/sh
# Outlive any sane timeout for a test.
sleep 30
"#;
        install_shim(shim_dir.path(), script);

        let _g = crate::telemetry::test_support::lock_env();
        let old_path = std::env::var("PATH").ok();
        let new_path = match &old_path {
            Some(p) => format!("{}:{}", shim_dir.path().display(), p),
            None => shim_dir.path().display().to_string(),
        };
        std::env::set_var("PATH", &new_path);

        let cwd_guard = tempfile::tempdir().unwrap();
        let child = build_command(
            &build_args("x", "{}", "claude-sonnet-4-5"),
            cwd_guard.path(),
        )
        .spawn()
        .expect("spawn shim");
        let start = std::time::Instant::now();
        let result = run_with_timeout(child, Duration::from_millis(250));
        let elapsed = start.elapsed();

        match old_path {
            Some(p) => std::env::set_var("PATH", p),
            None => std::env::remove_var("PATH"),
        }

        match result {
            Err(ProviderError::Upstream(msg)) => {
                assert!(
                    msg.contains("timed out"),
                    "timeout message expected, got: {msg}"
                );
            }
            other => panic!("expected Upstream timeout error, got: {other:?}"),
        }
        // Must have killed quickly — well under the 30s shim sleep.
        assert!(
            elapsed < Duration::from_secs(5),
            "timeout should fire quickly, elapsed: {elapsed:?}"
        );
    }
}
