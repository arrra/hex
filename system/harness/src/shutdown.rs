/// Port of the /hex-shutdown slash-command procedure.
///
/// Fire-and-forget session close. The inline steps are minimal:
/// quick distill guidance (AI-driven) + deregister the session.
/// Heavy work (reflection, transcript parsing, memory index) runs
/// automatically via Stop hooks after the session ends — not inline.

use std::path::Path;

const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

pub struct ShutdownArgs {
    /// Session ID to deregister. If None, prints instructions for finding it.
    pub session_id: Option<String>,
}

/// Run `hex shutdown`. Returns exit code (0 = success).
pub fn run(hex_dir: &Path, args: ShutdownArgs) -> i32 {
    println!("\n{}Shutdown — closing session{}", BOLD, RESET);

    // ── Step 1: Quick distill pass (AI-driven) ───────────────────────────────
    println!(
        "\n  {}Step 1:{} Quick distill pass {}(AI-driven — scan for unpersisted context){}",
        BOLD, RESET, DIM, RESET
    );
    println!(
        "  {}→{} Write decisions → me/decisions/ or projects/*/decisions/",
        DIM, RESET
    );
    println!("  {}→{} Update todo.md with new tasks / action items", DIM, RESET);
    println!(
        "  {}→{} Write person info → people/ if any mentioned",
        DIM, RESET
    );
    println!(
        "  {}→{} Skip if session < 5 exchanges with no meaningful context",
        DIM, RESET
    );

    // ── Step 2: Deregister session ───────────────────────────────────────────
    println!("\n  {}Step 2:{} Deregistering session…", BOLD, RESET);
    let session_script = hex_dir.join(".hex/scripts/session.sh");

    if !session_script.exists() {
        println!(
            "  {}[SKIP]{} session.sh not found at {} — skipping deregister",
            YELLOW, RESET, session_script.display()
        );
    } else {
        match &args.session_id {
            Some(id) => {
                let status = std::process::Command::new("bash")
                    .arg(&session_script)
                    .arg("stop")
                    .arg(id)
                    .env("HEX_DIR", hex_dir)
                    .status();
                match status {
                    Ok(s) if s.success() => {
                        println!("  {}[OK]{} session {} deregistered", GREEN, RESET, id);
                    }
                    Ok(s) => {
                        let code = s.code().unwrap_or(1);
                        eprintln!(
                            "  {}[WARN]{} session.sh stop exited with code {}",
                            YELLOW, RESET, code
                        );
                    }
                    Err(e) => {
                        eprintln!("  {}[WARN]{} failed to run session.sh: {}", YELLOW, RESET, e);
                    }
                }
            }
            None => {
                println!(
                    "  {}[INFO]{} No session ID provided. To deregister manually:",
                    DIM, RESET
                );
                println!(
                    "    bash $HEX_DIR/.hex/scripts/session.sh check   # find your session",

                );
                println!(
                    "    bash $HEX_DIR/.hex/scripts/session.sh stop <SESSION_ID>"
                );
            }
        }
    }

    // ── Step 3: Report ───────────────────────────────────────────────────────
    println!(
        "\n{}Session closed.{} Reflection and cleanup will run in the background.",
        GREEN, RESET
    );
    println!(
        "{}Background (Stop hooks):{} backup_session.sh, session-reflect.sh",
        DIM, RESET
    );

    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_no_session_id_succeeds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let code = run(dir.path(), ShutdownArgs { session_id: None });
        assert_eq!(code, 0);
    }

    #[test]
    fn run_with_missing_session_script_succeeds() {
        let dir = tempfile::tempdir().expect("tempdir");
        // No session.sh in the dir — should skip gracefully
        let code = run(
            dir.path(),
            ShutdownArgs { session_id: Some("test-session-123".to_string()) },
        );
        assert_eq!(code, 0);
    }
}
