use crate::registry::load_allowlist;
use std::path::Path;

/// Static body-scan: rejects dangerous patterns before a script is persisted.
///
/// Checked patterns (each is a hard deny):
/// - Network egress: curl, wget, nc, http:// or https://
/// - Secrets access: .hex/secrets
/// - Destructive: rm -rf
/// - Pipe-to-shell: | sh  or  | bash
pub fn check_body_safe(body: &str) -> Result<(), String> {
    // Each pattern is (label, check_fn).  We use simple substring/word checks;
    // no regex dependency required.

    // Network egress: bare command words at word boundary
    for cmd in &["curl", "wget", "nc"] {
        if contains_command_word(body, cmd) {
            return Err(format!(
                "script body rejected: contains forbidden command '{cmd}' (network egress)"
            ));
        }
    }

    // HTTP/HTTPS scheme anywhere in the body
    if body.contains("http://") || body.contains("https://") {
        return Err(
            "script body rejected: contains http:// or https:// URL (network egress)".to_string(),
        );
    }

    // Secrets access
    if body.contains(".hex/secrets") {
        return Err("script body rejected: accesses .hex/secrets".to_string());
    }

    // Destructive: rm -rf (any variant of spacing)
    if body.contains("rm -rf") || body.contains("rm  -rf") || body.contains("rm\t-rf") {
        return Err("script body rejected: contains 'rm -rf'".to_string());
    }

    // Pipe-to-shell
    if pipe_to_shell(body) {
        return Err("script body rejected: pipe-to-shell pattern (... | sh/bash)".to_string());
    }

    Ok(())
}

/// Check that `agent_id` is allowed to perform `action` ("add" or "call").
///
/// Reads the allowlist from <hex_dir>/.hex/registry/allowlist.json.
/// Unknown actions are always rejected.
pub fn check_allowed(hex_dir: &Path, agent_id: &str, action: &str) -> Result<(), String> {
    match action {
        "add" | "call" => {}
        other => {
            return Err(format!(
                "capability_guard: unknown action '{other}' — must be 'add' or 'call'"
            ))
        }
    }

    let allowlist = load_allowlist(hex_dir)
        .map_err(|e| format!("capability_guard: could not load allowlist: {e}"))?;

    if allowlist.iter().any(|a| a == agent_id) {
        Ok(())
    } else {
        Err(format!(
            "capability_guard: agent '{agent_id}' is not in the pilot allowlist (action={action})"
        ))
    }
}

/// Write-once guard: rejects an add when functions/<id>.json already exists.
///
/// `registry_dir` is <hex_dir>/.hex/registry or the test-scoped equivalent.
pub fn check_immutable(registry_dir: &Path, capability_id: &str) -> Result<(), String> {
    let fn_path = registry_dir
        .join("functions")
        .join(format!("{capability_id}.json"));
    if fn_path.exists() {
        Err(format!(
            "capability_guard: capability '{capability_id}' already exists (write-once)"
        ))
    } else {
        Ok(())
    }
}

// ── private helpers ───────────────────────────────────────────────────────────

/// Returns true if `word` appears as a whole command word in `body`.
/// Matches at start-of-line or after whitespace, followed by whitespace, end,
/// or special shell chars.
fn contains_command_word(body: &str, word: &str) -> bool {
    // Split on any whitespace/newline and check for exact word match.
    // Also match if the word appears at the start of a line.
    for line in body.lines() {
        let trimmed = line.trim_start();
        if trimmed == word
            || trimmed.starts_with(&format!("{word} "))
            || trimmed.starts_with(&format!("{word}\t"))
        {
            return true;
        }
        // Also catch cases like "sudo curl" or "exec curl"
        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if parts.contains(&word) {
            return true;
        }
    }
    false
}

/// Returns true if the body contains a pipe-to-shell pattern: `| sh` or `| bash`.
fn pipe_to_shell(body: &str) -> bool {
    for line in body.lines() {
        let s = line;
        // Check for | sh or | bash (with optional spaces around)
        if pipe_to_cmd(s, "sh") || pipe_to_cmd(s, "bash") {
            return true;
        }
    }
    false
}

// Boundary proof: `line[pos..]` and `line[abs_idx + 1..]` slice only at char
// boundaries. `pos` is 0 or `abs_idx + 1`, and `abs_idx` is the byte index of an
// ASCII '|' (from str::find), so one past it is always a char boundary.
#[allow(clippy::string_slice)]
fn pipe_to_cmd(line: &str, shell: &str) -> bool {
    // Find '|' and check if it's followed (after whitespace) by the shell command
    let mut pos = 0;
    while let Some(pipe_idx) = line[pos..].find('|') {
        let abs_idx = pos + pipe_idx;
        let after_pipe = line[abs_idx + 1..].trim_start();
        // Match "sh" or "sh " or "sh\n" at start of after_pipe
        if after_pipe == shell
            || after_pipe.starts_with(&format!("{shell} "))
            || after_pipe.starts_with(&format!("{shell}\t"))
        {
            return true;
        }
        pos = abs_idx + 1;
    }
    false
}
