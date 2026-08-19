//! `hex hook secret-scan` — a staged-diff secret scanner and pre-commit shim
//! installer.
//!
//! Two modes:
//!   * `hex hook secret-scan` — scans the staged diff (`git diff --cached`,
//!     ADDED lines only) of the repo in the current working directory for a
//!     fixed set of secret patterns. On any hit it prints the file, line, and
//!     pattern NAME with the matched value REDACTED (the value is never echoed)
//!     and exits 1. A clean diff exits 0.
//!   * `hex hook secret-scan --install` — writes a `pre-commit` shim into the
//!     current repo's hooks dir (resolved via `git rev-parse --git-common-dir`,
//!     because `.git` is a FILE in linked worktrees) that invokes
//!     `hex hook secret-scan`. If a pre-commit hook already exists and DIFFERS
//!     from the shim, it refuses loudly with instructions rather than
//!     overwriting.
//!
//! Design notes:
//!   * Redaction is BY CONSTRUCTION: a [`Finding`] carries only the match
//!     LENGTH, never the matched bytes. It is therefore structurally impossible
//!     for any formatter to leak a secret (Standing Order S6 — but stronger:
//!     the secret never enters the data model at all).
//!   * The scanner and installer are split into pure functions ([`scan_diff`],
//!     [`format_findings`], [`install_hook`]) so they are unit-testable without
//!     spawning a process or touching a live repo.
//!   * No config, no network, no new crate dependency — `regex` is already in
//!     the dependency tree.

use regex::Regex;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A single secret hit. Deliberately stores the match LENGTH, not the matched
/// value — redaction is by construction, so nothing that prints a `Finding`
/// can leak the secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub file: String,
    /// 1-based line number in the new (post-image) file.
    pub line: u64,
    /// Human-readable pattern class name.
    pub pattern: &'static str,
    /// Length (in chars) of the matched secret — for a redaction hint only.
    pub matched_len: usize,
}

/// The fixed pattern set (spec Syk2ms1yv, TASK 3). No config, no allowlist.
/// Compiled once per scan. Every pattern here is a valid regex literal, so the
/// `unwrap`s cannot fire (covered by `patterns_compile` test).
fn patterns() -> Vec<(&'static str, Regex)> {
    vec![
        (
            "AWS access key id",
            Regex::new(r"AKIA[0-9A-Z]{16}").unwrap(),
        ),
        (
            "GitHub token",
            Regex::new(r"(?:ghp_|gho_|ghs_|github_pat_)[A-Za-z0-9_]{16,}").unwrap(),
        ),
        (
            "Anthropic API key",
            Regex::new(r"sk-ant-[A-Za-z0-9_-]{10,}").unwrap(),
        ),
        (
            "OpenRouter API key",
            Regex::new(r"sk-or-[A-Za-z0-9_-]{10,}").unwrap(),
        ),
        (
            "OpenAI API key",
            Regex::new(r"sk-proj-[A-Za-z0-9_-]{10,}").unwrap(),
        ),
        (
            "Tailscale key",
            Regex::new(r"tskey-[A-Za-z0-9_-]{10,}").unwrap(),
        ),
        (
            "Slack token",
            Regex::new(r"xox[bpars]-[A-Za-z0-9-]{8,}").unwrap(),
        ),
        (
            "PEM private key",
            Regex::new(r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----").unwrap(),
        ),
    ]
}

/// Scan a unified diff for secret patterns in ADDED lines only.
///
/// Parses the diff itself (rather than the whole file) so we flag only what the
/// commit introduces. Tracks the new-file line number across hunks. Only lines
/// beginning with a single `+` (added content) are scanned; `+++ ` headers,
/// removed lines, and context lines are not.
///
/// Assumes the diff was produced with `--no-prefix` (so `+++ ` is followed by
/// the bare path, not `b/path`) — see [`staged_diff`].
pub fn scan_diff(diff: &str) -> Vec<Finding> {
    let pats = patterns();
    let hunk_re = Regex::new(r"^@@ -\d+(?:,\d+)? \+(\d+)(?:,\d+)? @@").unwrap();

    let mut findings = Vec::new();
    let mut cur_file: Option<String> = None;
    let mut new_line: u64 = 0;

    for line in diff.lines() {
        // New-file header (with --no-prefix the path is bare). `/dev/null` means
        // the file was deleted — nothing added, so drop it.
        if let Some(path) = line.strip_prefix("+++ ") {
            cur_file = if path == "/dev/null" {
                None
            } else {
                Some(path.to_string())
            };
            continue;
        }
        // Old-file header — ignore (never scanned).
        if line.starts_with("--- ") {
            continue;
        }
        // Hunk header resets the new-file line counter.
        if let Some(caps) = hunk_re.captures(line) {
            new_line = caps[1].parse().unwrap_or(0);
            continue;
        }

        // Within a hunk: classify by the leading diff marker.
        if let Some(content) = line.strip_prefix('+') {
            // Added line at `new_line`.
            if let Some(file) = &cur_file {
                for (name, re) in &pats {
                    for m in re.find_iter(content) {
                        findings.push(Finding {
                            file: file.clone(),
                            line: new_line,
                            pattern: *name,
                            matched_len: m.as_str().chars().count(),
                        });
                    }
                }
            }
            new_line += 1;
        } else if line.starts_with('-') {
            // Removed line — does not advance the new-file counter.
        } else if line.starts_with(' ') {
            // Context line — advances the new-file counter.
            new_line += 1;
        }
        // Anything else (`diff --git`, `index`, `\ No newline...`, blank
        // separators, mode/rename metadata) is skipped without advancing.
    }

    findings
}

/// Render findings for humans, with the secret VALUE redacted. Because
/// [`Finding`] never carries the matched bytes, this string cannot contain a
/// secret — the redaction is a property of the data model, not this function.
pub fn format_findings(findings: &[Finding]) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "secret-scan: {} potential secret(s) found in the staged diff\n",
        findings.len()
    ));
    for f in findings {
        out.push_str(&format!(
            "  {}:{}: {} [REDACTED {} chars]\n",
            f.file, f.line, f.pattern, f.matched_len
        ));
    }
    out.push_str("Refusing the commit. Remove the secret(s) above, or bypass with `git commit --no-verify` if this is a false positive.\n");
    out
}

/// The pre-commit shim body written by `--install`. Kept byte-stable so the
/// "already installed" check is an exact-content comparison.
const SHIM: &str = "#!/bin/sh\n# Installed by `hex hook secret-scan --install`. Do not edit; delete to uninstall.\nexec hex hook secret-scan\n";

/// The loud, actionable message printed when `--install` refuses to overwrite a
/// differing pre-commit hook. Factored out as a pure function so the "refuses
/// LOUDLY" contract (spec Syk2ms1yv, TASK 3) is unit-testable without spawning a
/// process — the message text, not just the silent [`InstallOutcome::Refused`]
/// enum, is asserted on.
pub fn refusal_message(existing: &Path) -> String {
    format!(
        "secret-scan --install: REFUSING to overwrite an existing, different pre-commit \
         hook at {}.\n\
         To install the hex secret-scan shim: back up or remove that hook, then re-run \
         `hex hook secret-scan --install`.\n\
         Or wire it into your existing hook manually by adding a line: `hex hook secret-scan`.",
        existing.display()
    )
}

/// Outcome of an install attempt.
#[derive(Debug, PartialEq, Eq)]
pub enum InstallOutcome {
    /// Shim freshly written.
    Written,
    /// Our exact shim was already present — idempotent no-op.
    AlreadyInstalled,
    /// A DIFFERENT pre-commit hook exists; refused without overwriting. Carries
    /// the path of the existing hook for the operator message.
    Refused(PathBuf),
}

/// Install the pre-commit shim into `hooks_dir` (pure filesystem logic; no git).
///
/// * Absent hook  → write shim (0755), return [`InstallOutcome::Written`].
/// * Identical    → [`InstallOutcome::AlreadyInstalled`] (idempotent).
/// * Different    → [`InstallOutcome::Refused`], nothing written.
pub fn install_hook(hooks_dir: &Path) -> std::io::Result<InstallOutcome> {
    let pre_commit = hooks_dir.join("pre-commit");
    if pre_commit.exists() {
        let existing = std::fs::read_to_string(&pre_commit).unwrap_or_default();
        if existing == SHIM {
            return Ok(InstallOutcome::AlreadyInstalled);
        }
        return Ok(InstallOutcome::Refused(pre_commit));
    }
    std::fs::create_dir_all(hooks_dir)?;
    std::fs::write(&pre_commit, SHIM)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&pre_commit, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(InstallOutcome::Written)
}

/// Entry point for `hex hook secret-scan [--install]`.
pub fn run(install: bool) {
    if install {
        run_install();
    } else {
        run_scan();
    }
}

fn run_scan() {
    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            // A pre-commit hook always runs inside a repo; an unusable cwd is a
            // genuinely broken invocation, so fail loud + nonzero rather than
            // silently letting the commit through (S6). `--no-verify` is the
            // operator's escape hatch.
            eprintln!("secret-scan: cannot determine current directory: {e}");
            std::process::exit(2);
        }
    };
    let diff = match staged_diff(&cwd) {
        Some(d) => d,
        None => {
            eprintln!(
                "secret-scan: could not read the staged diff via `git diff --cached` in {} \
                 (not a git repo, or git failed). Cannot scan; failing loud.",
                cwd.display()
            );
            std::process::exit(2);
        }
    };

    let findings = scan_diff(&diff);
    if findings.is_empty() {
        std::process::exit(0);
    }
    eprint!("{}", format_findings(&findings));
    std::process::exit(1);
}

fn run_install() {
    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("secret-scan --install: cannot determine current directory: {e}");
            std::process::exit(2);
        }
    };
    let common = match git_common_dir(&cwd) {
        Some(c) => c,
        None => {
            eprintln!(
                "secret-scan --install: could not resolve the git common dir via \
                 `git rev-parse --git-common-dir` in {}. Are you inside a git repo?",
                cwd.display()
            );
            std::process::exit(2);
        }
    };
    let hooks_dir = common.join("hooks");

    // Loud, non-fatal warning if core.hooksPath would make our shim dead code.
    if let Some(hp) = git_config(&cwd, "core.hooksPath") {
        let hp_abs = {
            let p = PathBuf::from(hp.trim());
            let joined = if p.is_absolute() { p } else { cwd.join(p) };
            joined.canonicalize().unwrap_or(joined)
        };
        let hooks_canon = hooks_dir.canonicalize().unwrap_or_else(|_| hooks_dir.clone());
        if hp_abs != hooks_canon {
            eprintln!(
                "secret-scan --install: WARNING — core.hooksPath is set to '{}', so git will \
                 NOT run hooks in '{}'. Installing the shim there anyway (as instructed), but it \
                 will not fire until core.hooksPath is unset or points at that dir.",
                hp_abs.display(),
                hooks_dir.display()
            );
        }
    }

    match install_hook(&hooks_dir) {
        Ok(InstallOutcome::Written) => {
            println!(
                "secret-scan --install: wrote pre-commit shim to {}",
                hooks_dir.join("pre-commit").display()
            );
            std::process::exit(0);
        }
        Ok(InstallOutcome::AlreadyInstalled) => {
            println!(
                "secret-scan --install: pre-commit shim already installed at {} (no change)",
                hooks_dir.join("pre-commit").display()
            );
            std::process::exit(0);
        }
        Ok(InstallOutcome::Refused(path)) => {
            eprintln!("{}", refusal_message(&path));
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("secret-scan --install: failed to write the shim: {e}");
            std::process::exit(2);
        }
    }
}

/// Staged diff of the repo at `cwd`, ADDED-line-oriented. `--no-prefix` drops
/// the `a/`,`b/` path prefixes (robust against `diff.mnemonicPrefix` /
/// `diff.noprefix` config); `--no-ext-diff` and `--no-color` keep the output
/// machine-parseable. Returns `None` on any git failure.
fn staged_diff(cwd: &Path) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args([
            "diff",
            "--cached",
            "--no-color",
            "--no-ext-diff",
            "--no-prefix",
        ])
        .output()
        .ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        None
    }
}

/// Absolute git common dir for `cwd` (resolves `.git`-as-a-FILE in worktrees).
fn git_common_dir(cwd: &Path) -> Option<PathBuf> {
    let raw = git(cwd, &["rev-parse", "--git-common-dir"])?;
    let p = PathBuf::from(raw.trim());
    let joined = if p.is_absolute() { p } else { cwd.join(p) };
    Some(joined.canonicalize().unwrap_or(joined))
}

/// `git config --get <key>` in `cwd`, or `None` if unset/failed.
fn git_config(cwd: &Path, key: &str) -> Option<String> {
    let v = git(cwd, &["config", "--get", key])?;
    let t = v.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// Run a git command in `dir`, returning trimmed-nothing stdout on exit 0.
fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Fixtures are built by CONCATENATION so no contiguous secret literal exists
    // in this source file — otherwise staging secret_scan.rs would trip the very
    // scanner it installs (spec forbids allowlists, so this is the only
    // mitigation).
    fn diff_with(added: &str) -> String {
        format!(
            "diff --git config.rs config.rs\n\
             index 0000000..1111111 100644\n\
             --- config.rs\n\
             +++ config.rs\n\
             @@ -0,0 +1,1 @@\n\
             +{added}\n"
        )
    }

    #[test]
    fn patterns_compile() {
        // Constructing them is the assertion — an invalid literal would panic.
        assert_eq!(patterns().len(), 8);
    }

    #[test]
    fn detects_aws_key() {
        let secret = format!("{}{}", "AKIA", "1234567890ABCDEF");
        let d = diff_with(&format!("const K = \"{secret}\";"));
        let f = scan_diff(&d);
        assert_eq!(f.len(), 1, "findings: {f:?}");
        assert_eq!(f[0].pattern, "AWS access key id");
        assert_eq!(f[0].line, 1);
    }

    #[test]
    fn detects_github_tokens() {
        for prefix in ["ghp_", "gho_", "ghs_", "github_pat_"] {
            let secret = format!("{}{}", prefix, "0123456789abcdefABCDEF_XYZ");
            let d = diff_with(&format!("token = {secret}"));
            let f = scan_diff(&d);
            assert_eq!(f.len(), 1, "prefix {prefix}: {f:?}");
            assert_eq!(f[0].pattern, "GitHub token");
        }
    }

    #[test]
    fn detects_anthropic_key() {
        let secret = format!("{}{}", "sk-ant-", "api03-AbCdEf0123456789");
        let d = diff_with(&format!("key: {secret}"));
        let f = scan_diff(&d);
        assert_eq!(f.len(), 1, "{f:?}");
        assert_eq!(f[0].pattern, "Anthropic API key");
    }

    #[test]
    fn detects_openrouter_key() {
        let secret = format!("{}{}", "sk-or-", "v1-0123456789abcdef");
        let d = diff_with(&format!("key: {secret}"));
        let f = scan_diff(&d);
        assert_eq!(f.len(), 1, "{f:?}");
        assert_eq!(f[0].pattern, "OpenRouter API key");
    }

    #[test]
    fn detects_openai_key() {
        let secret = format!("{}{}", "sk-proj-", "0123456789abcdefABCD");
        let d = diff_with(&format!("key: {secret}"));
        let f = scan_diff(&d);
        assert_eq!(f.len(), 1, "{f:?}");
        assert_eq!(f[0].pattern, "OpenAI API key");
    }

    #[test]
    fn detects_tailscale_key() {
        let secret = format!("{}{}", "tskey-", "auth-abcdef0123456789");
        let d = diff_with(&format!("key: {secret}"));
        let f = scan_diff(&d);
        assert_eq!(f.len(), 1, "{f:?}");
        assert_eq!(f[0].pattern, "Tailscale key");
    }

    #[test]
    fn detects_slack_token() {
        for c in ["b", "p", "a", "r", "s"] {
            let secret = format!("xox{}{}", c, "-0123456789abcdef");
            let d = diff_with(&format!("token = {secret}"));
            let f = scan_diff(&d);
            assert_eq!(f.len(), 1, "xox{c}-: {f:?}");
            assert_eq!(f[0].pattern, "Slack token");
        }
    }

    #[test]
    fn detects_pem_block() {
        let secret = format!("{}{}", "-----BEGIN RSA PRIVATE", " KEY-----");
        let d = diff_with(&secret);
        let f = scan_diff(&d);
        assert_eq!(f.len(), 1, "{f:?}");
        assert_eq!(f[0].pattern, "PEM private key");
    }

    #[test]
    fn clean_diff_exits_zero_worth_of_findings() {
        let d = diff_with("let harmless = 42; // nothing to see here");
        assert!(scan_diff(&d).is_empty());
    }

    #[test]
    fn removed_and_context_lines_are_not_scanned() {
        let secret = format!("{}{}", "AKIA", "1234567890ABCDEF");
        // Secret only on a REMOVED (`-`) and a CONTEXT (` `) line — never added.
        let d = format!(
            "diff --git a.rs a.rs\n\
             --- a.rs\n\
             +++ a.rs\n\
             @@ -1,3 +1,2 @@\n\
             -old = \"{secret}\"\n\
              ctx = \"{secret}\"\n\
             +new = 1\n"
        );
        assert!(scan_diff(&d).is_empty(), "only added lines should be scanned");
    }

    #[test]
    fn line_number_tracks_hunk_offset() {
        let secret = format!("{}{}", "AKIA", "1234567890ABCDEF");
        let d = format!(
            "diff --git a.rs a.rs\n\
             --- a.rs\n\
             +++ a.rs\n\
             @@ -0,0 +5,3 @@\n\
             +line five\n\
             +key = \"{secret}\"\n\
             +line seven\n"
        );
        let f = scan_diff(&d);
        assert_eq!(f.len(), 1, "{f:?}");
        assert_eq!(f[0].line, 6, "secret is the 2nd added line of a hunk @ +5");
    }

    #[test]
    fn output_never_contains_the_secret_value() {
        let secret = format!("{}{}", "AKIA", "1234567890ABCDEF");
        let d = diff_with(&format!("const K = \"{secret}\";"));
        let findings = scan_diff(&d);
        assert!(!findings.is_empty());
        let rendered = format_findings(&findings);
        assert!(
            !rendered.contains(&secret),
            "redacted output leaked the secret value: {rendered}"
        );
        // The pattern name and location must still be present.
        assert!(rendered.contains("AWS access key id"));
        assert!(rendered.contains("config.rs:1"));
    }

    #[test]
    fn install_writes_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let hooks = tmp.path().join("hooks");
        assert_eq!(install_hook(&hooks).unwrap(), InstallOutcome::Written);
        let written = std::fs::read_to_string(hooks.join("pre-commit")).unwrap();
        assert_eq!(written, SHIM);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(hooks.join("pre-commit"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111, "shim must be executable");
        }
    }

    #[test]
    fn install_is_idempotent_when_identical() {
        let tmp = tempfile::tempdir().unwrap();
        let hooks = tmp.path().join("hooks");
        assert_eq!(install_hook(&hooks).unwrap(), InstallOutcome::Written);
        assert_eq!(
            install_hook(&hooks).unwrap(),
            InstallOutcome::AlreadyInstalled
        );
    }

    #[test]
    fn install_refuses_differing_hook_without_overwriting() {
        let tmp = tempfile::tempdir().unwrap();
        let hooks = tmp.path().join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let existing = "#!/bin/sh\necho my own hook\n";
        std::fs::write(hooks.join("pre-commit"), existing).unwrap();

        match install_hook(&hooks).unwrap() {
            InstallOutcome::Refused(p) => assert_eq!(p, hooks.join("pre-commit")),
            other => panic!("expected Refused, got {other:?}"),
        }
        // The existing hook must be untouched.
        assert_eq!(
            std::fs::read_to_string(hooks.join("pre-commit")).unwrap(),
            existing
        );
    }

    #[test]
    fn refusal_message_is_loud_and_actionable() {
        let path = Path::new("/repo/.git/hooks/pre-commit");
        let msg = refusal_message(path);
        // Loud: names the refusal in caps.
        assert!(msg.contains("REFUSING"), "not loud: {msg}");
        // Actionable: points at the offending hook and how to proceed.
        assert!(msg.contains("/repo/.git/hooks/pre-commit"), "no path: {msg}");
        assert!(
            msg.contains("hex hook secret-scan --install"),
            "no remediation: {msg}"
        );
    }
}
