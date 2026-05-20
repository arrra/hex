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

    let sessions_dir = hex_dir.join(".hex/sessions");
    match &args.session_id {
        Some(id) => {
            let session_file = sessions_dir.join(id);
            if session_file.exists() {
                match std::fs::remove_file(&session_file) {
                    Ok(_) => println!("  {}[OK]{} session {} deregistered", GREEN, RESET, id),
                    Err(e) => {
                        eprintln!("  {}[WARN]{} failed to remove session file: {}", YELLOW, RESET, e);
                    }
                }
            } else {
                println!("  {}[OK]{} session {} (already deregistered or not tracked)", GREEN, RESET, id);
            }
        }
        None => {
            println!(
                "  {}[INFO]{} No session ID provided. To deregister manually:",
                DIM, RESET
            );
            println!("    ls $HEX_DIR/.hex/sessions/    # list active sessions");
            println!("    hex shutdown --session-id <SESSION_ID>");
        }
    }

    // ── Step 3: Report ───────────────────────────────────────────────────────
    println!(
        "\n{}Session closed.{} Reflection and cleanup will run in the background.",
        GREEN, RESET
    );
    println!(
        "{}Background (Stop hooks):{} session-reflect, transcript parsing, memory index",
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
