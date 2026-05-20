/// Port of the /hex-checkpoint slash-command procedure.
///
/// Runs the mechanical steps that don't require conversation context:
/// creates the handoff directory, dispatches background reflection,
/// appends a changelog entry to today's landings file, and prints the
/// compact suggestion. Steps that require reading the conversation
/// (distill pass, todo.md update) must still be performed by the AI;
/// the binary prints guidance for those steps.

use chrono::Local;
use std::fs;
use std::io::Write as _;
use std::path::Path;

const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

pub struct CheckpointArgs {
    /// Optional focus directive (what to work on next)
    pub focus: Option<String>,
}

/// Run `hex checkpoint`. Returns exit code (0 = success).
pub fn run(hex_dir: &Path, args: CheckpointArgs) -> i32 {
    let now = Local::now();
    let date_str = now.format("%Y-%m-%d").to_string();
    let datetime_str = now.format("%Y-%m-%d-%H%M%S").to_string();
    let time_str = now.format("%H:%M").to_string();

    println!("\n{}Checkpoint — {}{}", BOLD, time_str, RESET);

    // ── Step 1: Quick distill pass (AI-driven) ───────────────────────────────
    println!(
        "\n  {}Step 1:{} Quick distill pass {}(AI-driven — scan conversation for unpersisted context){}",
        BOLD, RESET, DIM, RESET
    );
    println!(
        "  {}→{} Write decisions → me/decisions/ or projects/*/decisions/",
        DIM, RESET
    );
    println!("  {}→{} Update todo.md with new tasks / priority changes", DIM, RESET);
    println!(
        "  {}→{} Update evolution/observations.md with patterns noticed",
        DIM, RESET
    );

    // ── Step 2: Dispatch background reflection ───────────────────────────────
    println!("\n  {}Step 2:{} Dispatching background reflection…", BOLD, RESET);
    let reflect_status = dispatch_background_reflection(hex_dir);
    if reflect_status {
        println!("  {}[OK]{} session-reflect dispatched", GREEN, RESET);
    } else {
        println!("  {}[SKIP]{} session-reflect script not found — skipping", YELLOW, RESET);
    }

    // ── Step 3: Write handoff file ───────────────────────────────────────────
    println!("\n  {}Step 3:{} Writing handoff file…", BOLD, RESET);
    let handoff_path = hex_dir.join("raw/handoffs").join(format!("{}.md", datetime_str));
    if let Some(parent) = handoff_path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            eprintln!("checkpoint: cannot create handoffs dir: {e}");
            return 1;
        }
    }
    let focus_line = args.focus.as_deref().unwrap_or("(to be determined)");
    let handoff_content = format!(
        "# Session Handoff — {} {}\n\n## What We Did\n- \n\n## Key Decisions\n- \n\n## Open Threads\n- \n\n## Next Focus\n- {}\n\n## Files Modified This Session\n- \n",
        date_str, time_str, focus_line
    );
    if let Err(e) = atomic_write(&handoff_path, &handoff_content) {
        eprintln!("checkpoint: cannot write handoff: {e}");
        return 1;
    }
    println!("  {}[OK]{} {}", GREEN, RESET, handoff_path.display());

    // ── Step 4: Update todo.md (AI-driven) ──────────────────────────────────
    println!(
        "\n  {}Step 4:{} Update todo.md {}(AI-driven — move completed items, add new ones){}",
        BOLD, RESET, DIM, RESET
    );

    // ── Step 5: Update daily landings ────────────────────────────────────────
    println!("\n  {}Step 5:{} Updating daily landings…", BOLD, RESET);
    let landings_path = hex_dir.join("landings").join(format!("{}.md", date_str));
    if landings_path.exists() {
        let entry = format!(
            "- {} — Checkpoint: {}\n",
            time_str,
            args.focus.as_deref().unwrap_or("session checkpointed")
        );
        match fs::OpenOptions::new().append(true).open(&landings_path) {
            Ok(mut f) => {
                if let Err(e) = f.write_all(entry.as_bytes()) {
                    eprintln!("checkpoint: cannot append to landings: {e}");
                } else {
                    println!("  {}[OK]{} appended to {}", GREEN, RESET, landings_path.display());
                }
            }
            Err(e) => eprintln!("checkpoint: cannot open landings: {e}"),
        }
    } else {
        println!(
            "  {}[SKIP]{} no landings file for today ({})",
            YELLOW, RESET, date_str
        );
    }

    // ── Step 6: Suggest compact ──────────────────────────────────────────────
    println!(
        "\n{}Checkpointed.{} Reflection dispatched to background.",
        GREEN, RESET
    );
    println!("\nIf you want a fresh context window, run:");
    let handoff_rel = format!("raw/handoffs/{}.md", datetime_str);
    let compact_focus = args.focus.as_deref().unwrap_or("next focus");
    println!(
        "  /compact {}. Handoff at {}. Re-read: todo.md, landings/{}.md",
        compact_focus, handoff_rel, date_str
    );

    0
}

fn dispatch_background_reflection(hex_dir: &Path) -> bool {
    // Call session_reflect Rust module directly in a background thread.
    let hex_dir = hex_dir.to_path_buf();
    std::thread::spawn(move || {
        std::env::set_var("HEX_DIR", &hex_dir);
        crate::session_reflect::run(None, true);
    });
    true
}

fn atomic_write(path: &Path, content: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(content.as_bytes())?;
        f.flush()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn setup_hex_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(dir.path().join("raw/handoffs")).unwrap();
        fs::create_dir_all(dir.path().join("landings")).unwrap();
        dir
    }

    #[test]
    fn run_creates_handoff_file() {
        let dir = setup_hex_dir();
        let code = run(
            dir.path(),
            CheckpointArgs { focus: Some("test focus".to_string()) },
        );
        assert_eq!(code, 0);
        let handoffs: Vec<_> = fs::read_dir(dir.path().join("raw/handoffs"))
            .unwrap()
            .collect();
        assert!(!handoffs.is_empty(), "handoff file must be created");
        let entry = handoffs.into_iter().next().unwrap().unwrap();
        let contents = fs::read_to_string(entry.path()).unwrap();
        assert!(contents.contains("test focus"), "handoff must contain focus");
    }

    #[test]
    fn run_appends_to_existing_landings() {
        let dir = setup_hex_dir();
        let today = Local::now().format("%Y-%m-%d").to_string();
        let landings_path = dir.path().join("landings").join(format!("{}.md", today));
        fs::write(&landings_path, "## Landings\n").unwrap();
        let code = run(dir.path(), CheckpointArgs { focus: None });
        assert_eq!(code, 0);
        let contents = fs::read_to_string(&landings_path).unwrap();
        assert!(contents.contains("Checkpoint"), "landings must have checkpoint entry");
    }

    #[test]
    fn run_skips_landings_when_missing() {
        let dir = setup_hex_dir();
        let code = run(dir.path(), CheckpointArgs { focus: None });
        assert_eq!(code, 0);
    }
}
