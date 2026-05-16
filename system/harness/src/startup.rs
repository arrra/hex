/// Port of .hex/scripts/startup.sh
///
/// Runs the full hex session startup checklist in the same step order as the
/// original shell script. External helpers (parse-transcripts, memory_index,
/// hex-doctor, etc.) are invoked via std::process::Command — same as the shell
/// script calling them.

use chrono::Local;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

// ── ANSI codes mirroring startup.sh ─────────────────────────────────────────
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RED: &str = "\x1b[31m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

struct State {
    warnings: u32,
    failures: u32,
    session_id: String,
    is_solo: bool,
    privacy_mode: bool,
}

fn pass(msg: &str) {
    println!("  {}[PASS]{} {}", GREEN, RESET, msg);
}

fn warn_line(msg: &str, state: &mut State) {
    println!("  {}[WARN]{} {}", YELLOW, RESET, msg);
    state.warnings += 1;
}

fn fail_line(msg: &str, state: &mut State) {
    println!("  {}[FAIL]{} {}", RED, RESET, msg);
    state.failures += 1;
}

fn info(msg: &str) {
    println!("  {}→{} {}", DIM, RESET, msg);
}

fn header(msg: &str) {
    println!("\n{}{}{}", BOLD, msg, RESET);
}

// ── Public API ───────────────────────────────────────────────────────────────

pub struct StartupArgs {
    pub quick: bool,
    pub step: Option<String>,
    pub status: bool,
}

/// Run `hex startup`. Returns the exit code (0 = pass/warn, 1 = failures).
pub fn run(hex_dir: &Path, args: StartupArgs) -> i32 {
    let hex_system_dir = hex_dir.join(".hex");
    let scripts_dir = hex_system_dir.join("scripts");
    let memory_scripts = hex_system_dir.join("skills/memory/scripts");
    let today = Local::now().format("%Y-%m-%d").to_string();

    // Set TZ from .hex/timezone if not already set
    if std::env::var("TZ").is_err() {
        let tz_file = hex_system_dir.join("timezone");
        if let Ok(tz) = std::fs::read_to_string(&tz_file) {
            let tz = tz.trim().to_string();
            if !tz.is_empty() {
                std::env::set_var("TZ", &tz);
            }
        }
    }

    let mut state = State {
        warnings: 0,
        failures: 0,
        session_id: String::new(),
        is_solo: true,
        privacy_mode: std::env::var("HEX_PRIVACY").as_deref() == Ok("1"),
    };

    // --status: just show step list and exit
    if args.status {
        print_status();
        return 0;
    }

    // --step NAME: run a single step and exit
    if let Some(ref step_name) = args.step {
        return run_single_step(step_name, hex_dir, &hex_system_dir, &scripts_dir, &memory_scripts, &today, &mut state, args.quick);
    }

    // ── Full startup sequence ────────────────────────────────────────────────

    // Step 1: Header banner
    step_banner();

    // Step 2: Background update check (non-blocking)
    step_update_check_bg(&scripts_dir);

    // Step 3: Privacy notice
    if state.privacy_mode {
        println!("  [PRIVACY MODE] Transcripts and session data are not sent externally.");
    }

    // Step 4: Environment detection
    step_env(hex_dir, &mut state);

    // Step 5: Session management
    step_session(&scripts_dir, &mut state);

    // Step 6: Doctor alert check (no header — runs between session and transcripts)
    step_doctor_alert(&hex_system_dir, &mut state);

    // Step 7: Parse transcripts
    step_transcripts(&scripts_dir);

    // Step 8: Memory index (solo or quick)
    if state.is_solo || args.quick {
        step_index(&memory_scripts, hex_dir, &mut state);
    }

    // Step 9: Memory health (solo or quick)
    if state.is_solo || args.quick {
        step_health(&memory_scripts, &mut state);
    }

    // Step 10: Integrations (solo, not quick)
    if state.is_solo && !args.quick {
        step_integrations(hex_dir, &mut state);
    }

    // Step 11: Evolution engine (solo, not quick)
    if state.is_solo && !args.quick {
        step_evolution(hex_dir, &scripts_dir, &hex_system_dir, &mut state);
    }

    // Step 12: Priority scoring (solo, not quick)
    if state.is_solo && !args.quick {
        step_priorities(hex_dir, &mut state);
    }

    // Step 13: Daemon status (solo or quick)
    if state.is_solo || args.quick {
        step_daemon_status(&scripts_dir, &mut state);
    }

    // Step 14: hex-events telemetry (always)
    step_hex_events(&mut state);

    // Step 15: Emit session.started (always)
    step_emit_session_started(hex_dir, &today);

    // Step 16: Update notice (always)
    step_update_notice(&hex_system_dir);

    // Step 17: Summary + exit
    step_summary(&state)
}

// ── Step implementations ─────────────────────────────────────────────────────

fn step_banner() {
    let now = Local::now().format("%Y-%m-%d %H:%M").to_string();
    let separator = "=".repeat(55);
    println!("{}", separator);
    println!(" Hex Startup \u{2014} {}", now);
    println!("{}", separator);
}

fn step_update_check_bg(scripts_dir: &Path) {
    let script = scripts_dir.join("check-update.sh");
    if script.exists() {
        // Fire and forget — matches shell `check-update.sh &`
        let _ = Command::new("bash")
            .arg(&script)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
}

fn step_env(hex_dir: &Path, state: &mut State) {
    header("1. Environment Detection");
    let ostype = std::env::consts::OS;
    match ostype {
        "macos" | "linux" => pass(&format!("OS: {}", ostype)),
        other => warn_line(&format!("Unknown OS: {}", other), state),
    }
    info(&format!("HEX_DIR: {}", hex_dir.display()));
}

fn step_session(scripts_dir: &Path, state: &mut State) {
    header("2. Session Management");

    let script = scripts_dir.join("session.sh");
    if !script.exists() {
        info("session.sh not found — skipping session management");
        return;
    }

    // (a) Cleanup stale sessions — || true semantics
    let _ = Command::new("bash")
        .arg(&script)
        .arg("cleanup")
        .env("SCRIPTS_DIR", scripts_dir)
        .status();
    info("Cleaned up stale sessions");

    // (b) Check for other active sessions
    let check = Command::new("bash")
        .arg(&script)
        .arg("check")
        .env("SCRIPTS_DIR", scripts_dir)
        .output();
    match check {
        Ok(out) if out.status.success() => {
            pass("Solo session");
            // is_solo stays true
        }
        Ok(_) => {
            warn_line("Multiple sessions active", state);
            state.is_solo = false;
        }
        Err(_) => {
            // treat error as solo — best effort
            pass("Solo session (check skipped)");
        }
    }

    // (c) Register this session
    let start = Command::new("bash")
        .arg(&script)
        .arg("start")
        .arg("startup-script")
        .env("SCRIPTS_DIR", scripts_dir)
        .output();
    match start {
        Ok(out) if out.status.success() => {
            let sid = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !sid.is_empty() {
                state.session_id = sid.clone();
                pass(&format!("Session registered: {}", sid));
            } else {
                pass("Session registered");
            }
        }
        Ok(_) => info("Session registration skipped"),
        Err(_) => info("Session registration unavailable"),
    }
}

fn step_doctor_alert(hex_system_dir: &Path, state: &mut State) {
    let alert_path = hex_system_dir.join("doctor-alert");
    if alert_path.exists() {
        if let Ok(contents) = std::fs::read_to_string(&alert_path) {
            warn_line("Doctor alert:", state);
            for line in contents.lines().take(20) {
                println!("    {}", line);
            }
            let _ = std::fs::remove_file(&alert_path);
        }
    }
}

fn step_transcripts(scripts_dir: &Path) {
    header("3. Parse Transcripts");

    let emit_script = scripts_dir.join("parse-transcripts-and-emit.sh");
    let py_script = scripts_dir.join("parse_transcripts.py");

    let output = if emit_script.exists() && is_executable(&emit_script) {
        Command::new("bash")
            .arg(&emit_script)
            .output()
            .ok()
    } else if py_script.exists() {
        Command::new("python3")
            .arg(&py_script)
            .output()
            .ok()
    } else {
        info("Transcript parser not found — skipping");
        return;
    };

    match output {
        Some(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            for line in stdout.lines() {
                info(line);
            }
            if stdout.contains("No new transcripts") || stdout.contains("No .jsonl files") {
                pass("Transcripts: no new files");
            } else {
                pass("Transcripts parsed");
            }
        }
        None => info("Transcript parser unavailable"),
    }
}

fn step_index(memory_scripts: &Path, hex_dir: &Path, state: &mut State) {
    header("4. Memory Index");

    let script = memory_scripts.join("memory_index.py");
    if !script.exists() {
        warn_line("memory_index.py not found", state);
        return;
    }

    let output = Command::new("python3")
        .arg(&script)
        .env("HEX_DIR", hex_dir)
        .output();

    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            for line in stdout.lines() {
                if line.starts_with("Done:") || line.starts_with("Indexed") {
                    info(line);
                }
            }
            pass("Memory index updated");
        }
        Err(_) => {
            // Shell script silently passes — mirror that
            pass("Memory index (skipped)");
        }
    }
}

fn step_health(memory_scripts: &Path, state: &mut State) {
    header("5. Memory Health");

    let script = memory_scripts.join("memory_health.py");
    if !script.exists() {
        info("memory_health.py not found — skipping");
        return;
    }

    let output = Command::new("python3")
        .arg(&script)
        .arg("--quiet")
        .output();

    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let mut has_fail = false;
            let mut has_warn = false;
            for line in stdout.lines() {
                if line.contains("FAIL") {
                    fail_line(line, state);
                    has_fail = true;
                } else if line.contains("WARN") {
                    warn_line(line, state);
                    has_warn = true;
                }
            }
            if !has_fail && !has_warn {
                pass("Memory health OK");
            }
        }
        Err(_) => info("Memory health check unavailable"),
    }
}

fn step_integrations(hex_dir: &Path, _state: &mut State) {
    header("6. Integrations");

    let integrations_file = hex_dir.join("integrations.json");
    if !integrations_file.exists() {
        info("integrations.json not found — skipping");
        return;
    }

    // Use Python one-liner to parse JSON, mirroring the shell script
    let output = Command::new("python3")
        .arg("-c")
        .arg(format!(
            "import json; data = json.load(open('{}'));\n\
             items = data if isinstance(data, list) else data.get('integrations', [])\n\
             for i in items:\n\
               name = i.get('name', i.get('id', '?'))\n\
               enabled = i.get('enabled', True)\n\
               print(f'{{name}}:{{\"1\" if enabled else \"0\"}}')",
            integrations_file.display()
        ))
        .output();

    match output {
        Ok(out) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            for line in stdout.lines() {
                let parts: Vec<&str> = line.splitn(2, ':').collect();
                if parts.len() == 2 {
                    let name = parts[0];
                    let enabled = parts[1] == "1";
                    if enabled {
                        pass(name);
                    } else {
                        info(&format!("{} (disabled)", name));
                    }
                }
            }
        }
        _ => {
            // || true — tolerate failure
            info("Integrations check unavailable");
        }
    }
}

fn step_evolution(hex_dir: &Path, scripts_dir: &Path, hex_system_dir: &Path, state: &mut State) {
    header("7. Improvement Engine");

    // (a) check-evolution.sh if present
    let evo_script = scripts_dir.join("check-evolution.sh");
    if evo_script.exists() {
        let _ = Command::new("bash")
            .arg(&evo_script)
            .env("HEX_DIR", hex_dir)
            .status(); // || true
    }

    // (b) Count "Status: proposed" in suggestions.md
    let suggestions = hex_dir.join("evolution/suggestions.md");
    if suggestions.exists() {
        let count = std::fs::read_to_string(&suggestions)
            .unwrap_or_default()
            .lines()
            .filter(|l| l.starts_with("Status: proposed"))
            .count();
        if count > 0 {
            warn_line(&format!("{} pending improvement(s)", count), state);
        } else {
            pass("No pending improvements");
        }
    } else {
        info("suggestions.md not found");
    }

    // (c) generate-performance-context.py if both script and memory.db exist
    let perf_script = hex_dir.join("evolution/eval/generate-performance-context.py");
    let memory_db = hex_system_dir.join("memory.db");
    if perf_script.exists() && memory_db.exists() {
        let output_path = hex_dir.join("evolution/eval/latest-performance-context.md");
        let _ = Command::new("python3")
            .arg(&perf_script)
            .arg("--db")
            .arg(&memory_db)
            .arg("--output")
            .arg(&output_path)
            .env("HEX_DIR", hex_dir)
            .status(); // || true
    }
}

fn step_priorities(hex_dir: &Path, state: &mut State) {
    header("8. Priority Scoring");

    let script = hex_dir.join("evolution/priority-score.py");
    if !script.exists() {
        info("priority-score.py not found — skipping");
        return;
    }

    let output_path = hex_dir.join("evolution/priority-ranked.yaml");
    let status = Command::new("python3")
        .arg(&script)
        .arg("--top")
        .arg("3")
        .arg("--output")
        .arg(&output_path)
        .env("HEX_DIR", hex_dir)
        .status();

    match status {
        Ok(s) if s.success() => pass("Priority scoring complete"),
        _ => warn_line("Priority scoring failed (non-fatal)", state),
    }
}

fn step_daemon_status(scripts_dir: &Path, state: &mut State) {
    header("9. Daemon Status");

    let script = scripts_dir.join("hex-daemons.sh");
    if !script.exists() {
        info("hex-daemons.sh not found — skipping");
        return;
    }

    let output = Command::new("bash")
        .arg(&script)
        .arg("status")
        .output();

    match output {
        Ok(out) => {
            // Strip ANSI escape codes from daemon output
            let raw = String::from_utf8_lossy(&out.stdout);
            let stripped = strip_ansi(&raw);
            let mut any_down = false;
            for line in stripped.lines() {
                if line.contains("[WARN]") || line.contains("[FAIL]") || line.contains("[DOWN]") {
                    warn_line(line.trim(), state);
                    any_down = true;
                } else if line.contains("[OK]") || line.contains("[PASS]") {
                    pass(line.trim());
                } else if !line.trim().is_empty() {
                    info(line.trim());
                }
            }
            if any_down {
                info("Run 'hex-daemons.sh start' to start daemons manually");
            }
        }
        Err(_) => {
            // || true — tolerate
            info("Daemon status unavailable");
        }
    }
}

fn step_hex_events(state: &mut State) {
    header("10. hex-events Telemetry");

    let home = std::env::var("HOME").unwrap_or_default();
    let cli_path = PathBuf::from(&home).join(".hex-events/hex_events_cli.py");
    let venv_python = PathBuf::from(&home).join(".hex-events/venv/bin/python");

    let python_bin = if venv_python.exists() {
        venv_python
    } else {
        PathBuf::from("python3")
    };

    if !cli_path.exists() {
        pass("hex-events not installed (OK)");
        return;
    }

    // Query actions_failed in last 24h
    let output = Command::new(&python_bin)
        .arg(&cli_path)
        .arg("telemetry")
        .arg("--json")
        .stderr(Stdio::null())
        .output();

    let actions_failed: u64 = match output {
        Ok(out) if out.status.success() => {
            let stdout = out.stdout;
            // Pipe JSON via stdin to avoid shell quoting issues with string interpolation
            let mut child = Command::new("python3")
                .arg("-c")
                .arg("import json,sys; d=json.load(sys.stdin); print(d.get('actions_failed', 0))")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .ok();
            if let Some(ref mut c) = child {
                if let Some(stdin) = c.stdin.take() {
                    let mut stdin = stdin;
                    let _ = stdin.write_all(&stdout);
                }
            }
            match child.and_then(|c| c.wait_with_output().ok()) {
                Some(e) => String::from_utf8_lossy(&e.stdout)
                    .trim()
                    .parse()
                    .unwrap_or(0),
                None => 0,
            }
        }
        _ => 0,
    };

    if actions_failed > 0 {
        warn_line(&format!("{} action failure(s) in last 24h", actions_failed), state);
    } else {
        pass("hex-events telemetry OK");
    }
}

fn step_emit_session_started(hex_dir: &Path, today: &str) {
    let home = std::env::var("HOME").unwrap_or_default();
    let emit_path = PathBuf::from(&home).join(".hex-events/hex_emit.py");
    let venv_python = PathBuf::from(&home).join(".hex-events/venv/bin/python");

    if !emit_path.exists() {
        return;
    }

    let python_bin = if venv_python.exists() {
        venv_python
    } else {
        PathBuf::from("python3")
    };

    let payload = format!(
        "{{\"hex_dir\":\"{}\",\"today\":\"{}\"}}",
        hex_dir.display(),
        today
    );

    // || true — always tolerated
    let _ = Command::new(&python_bin)
        .arg(&emit_path)
        .arg("session.started")
        .arg(&payload)
        .arg("startup.sh")
        .stderr(Stdio::null())
        .stdout(Stdio::null())
        .status();
}

fn step_update_notice(hex_system_dir: &Path) {
    let update_file = hex_system_dir.join(".update-available");
    if update_file.exists() {
        println!(
            "  {}[UPDATE]{} A new version of hex is available. Run /hex-upgrade to update.",
            YELLOW, RESET
        );
    }
}

fn step_summary(state: &State) -> i32 {
    let separator = "=".repeat(55);
    println!("\n{}", separator);
    if state.failures > 0 {
        println!(
            "  {}Startup complete: {} failure(s), {} warning(s){}",
            RED, state.failures, state.warnings, RESET
        );
        1
    } else if state.warnings > 0 {
        println!(
            "  {}Startup complete: {} warning(s){}",
            YELLOW, state.warnings, RESET
        );
        0
    } else {
        println!("  {}Startup complete: all checks passed{}", GREEN, RESET);
        0
    }
}

fn print_status() {
    println!("Steps available (in startup order):");
    for (i, name) in step_names().iter().enumerate() {
        println!("  {:2}. {}", i + 1, name);
    }
}

fn run_single_step(
    step_name: &str,
    hex_dir: &Path,
    hex_system_dir: &Path,
    scripts_dir: &Path,
    memory_scripts: &Path,
    _today: &str,
    state: &mut State,
    quick: bool,
) -> i32 {
    match step_name {
        "env" => step_env(hex_dir, state),
        "session" => step_session(scripts_dir, state),
        "transcripts" => step_transcripts(scripts_dir),
        "index" => step_index(memory_scripts, hex_dir, state),
        "health" => step_health(memory_scripts, state),
        "integrations" => step_integrations(hex_dir, state),
        "evolution" => step_evolution(hex_dir, scripts_dir, hex_system_dir, state),
        "priorities" => step_priorities(hex_dir, state),
        "daemon" => step_daemon_status(scripts_dir, state),
        "hex-events" => step_hex_events(state),
        other => {
            eprintln!("hex startup: unknown step '{}'. Use --status to list steps.", other);
            return 1;
        }
    }
    // Ignore `quick` warning — single-step always runs the named step regardless
    let _ = quick;
    step_summary(state)
}

// ── Utilities ────────────────────────────────────────────────────────────────

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Strip ANSI escape sequences from a string (mirrors `sed 's/\x1b\[[0-9;]*m//g'`).
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                // consume until 'm' or end
                for ch in chars.by_ref() {
                    if ch == 'm' {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Ordered list of named steps — used for --status, --step validation, and unit tests.
pub fn step_names() -> Vec<&'static str> {
    vec![
        "env",
        "session",
        "transcripts",
        "index",
        "health",
        "integrations",
        "evolution",
        "priorities",
        "daemon",
        "hex-events",
    ]
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_sequence_matches_inventory() {
        let steps = step_names();
        // 10 named steps in startup sequence order
        assert_eq!(steps.len(), 10);
        assert_eq!(steps[0], "env");
        assert_eq!(steps[1], "session");
        assert_eq!(steps[2], "transcripts");
        assert_eq!(steps[3], "index");
        assert_eq!(steps[4], "health");
        assert_eq!(steps[5], "integrations");
        assert_eq!(steps[6], "evolution");
        assert_eq!(steps[7], "priorities");
        assert_eq!(steps[8], "daemon");
        assert_eq!(steps[9], "hex-events");
    }

    #[test]
    fn strip_ansi_removes_escape_sequences() {
        let input = "\x1b[32m[PASS]\x1b[0m message";
        assert_eq!(strip_ansi(input), "[PASS] message");
    }

    #[test]
    fn strip_ansi_passthrough_plain() {
        let input = "plain text no escapes";
        assert_eq!(strip_ansi(input), input);
    }

    #[test]
    fn state_counters_increment_correctly() {
        let mut s = State {
            warnings: 0,
            failures: 0,
            session_id: String::new(),
            is_solo: true,
            privacy_mode: false,
        };
        warn_line("test warn", &mut s);
        warn_line("test warn 2", &mut s);
        fail_line("test fail", &mut s);
        assert_eq!(s.warnings, 2);
        assert_eq!(s.failures, 1);
    }

    #[test]
    fn summary_returns_1_on_failures() {
        let s = State {
            warnings: 0,
            failures: 1,
            session_id: String::new(),
            is_solo: true,
            privacy_mode: false,
        };
        assert_eq!(step_summary(&s), 1);
    }

    #[test]
    fn summary_returns_0_on_warnings_only() {
        let s = State {
            warnings: 2,
            failures: 0,
            session_id: String::new(),
            is_solo: true,
            privacy_mode: false,
        };
        assert_eq!(step_summary(&s), 0);
    }

    #[test]
    fn summary_returns_0_on_clean() {
        let s = State {
            warnings: 0,
            failures: 0,
            session_id: String::new(),
            is_solo: true,
            privacy_mode: false,
        };
        assert_eq!(step_summary(&s), 0);
    }
}
