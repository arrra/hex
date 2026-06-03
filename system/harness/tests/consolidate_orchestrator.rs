// Red test for task Tx8a72zfh: top-level `hex consolidate` orchestrator
// with `full` and `quick` subcommands.
//
// Quick mode must run Layer 1 (doctor::consolidate) + Layer 2 (memory::consolidate)
// deterministically with no network. Full mode must be wired (help lists it) but
// is NOT executed here — we never make a live LLM/provider call in tests.

use std::fs;
use std::process::Command;
use tempfile::TempDir;

fn build_fake_hex_dir() -> TempDir {
    let dir = tempfile::tempdir().expect("create tempdir");
    let p = dir.path();

    fs::write(p.join("CLAUDE.md"), "").unwrap();

    let evo = p.join("evolution");
    fs::create_dir_all(&evo).unwrap();
    fs::write(evo.join("observations.md"), "").unwrap();
    fs::write(evo.join("suggestions.md"), "").unwrap();
    fs::write(evo.join("changelog.md"), "").unwrap();

    fs::create_dir_all(p.join("projects")).unwrap();

    let me = p.join("me");
    fs::create_dir_all(&me).unwrap();
    fs::write(me.join("learnings.md"), "").unwrap();

    dir
}

#[test]
fn consolidate_help_lists_full_and_quick_modes() {
    let bin = env!("CARGO_BIN_EXE_hex");
    let output = Command::new(bin)
        .args(["consolidate", "--help"])
        .output()
        .expect("hex binary must run");

    assert!(
        output.status.success(),
        "`hex consolidate --help` must succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let help = String::from_utf8_lossy(&output.stdout).to_lowercase();
    assert!(help.contains("full"), "help must list 'full' mode; got:\n{help}");
    assert!(help.contains("quick"), "help must list 'quick' mode; got:\n{help}");
}

#[test]
fn consolidate_quick_runs_deterministically_with_no_network() {
    let hex_dir = build_fake_hex_dir();
    let bin = env!("CARGO_BIN_EXE_hex");

    let output = Command::new(bin)
        .args(["consolidate", "quick"])
        .env("HEX_DIR", hex_dir.path())
        // Force any accidental provider call to fail loudly — quick must not need it.
        .env_remove("OPENROUTER_API_KEY")
        .output()
        .expect("hex binary must run");

    let code = output.status.code().unwrap_or(2);
    assert!(
        code == 0 || code == 1,
        "unexpected exit code {code} from `hex consolidate quick`; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Layer 1 must write the structural log.
    let log_path = hex_dir
        .path()
        .join("evolution")
        .join("consolidation-latest.log");
    assert!(
        log_path.exists(),
        "consolidation-latest.log must be written by quick mode (Layer 1)"
    );

    // Quick must NOT have written an LLM audit file (that's full-only, Layer 3).
    let evo = hex_dir.path().join("evolution");
    if let Ok(rd) = fs::read_dir(&evo) {
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            assert!(
                !name.starts_with("consolidation-audit-"),
                "quick mode must NOT write a consolidation-audit-*.md file (LLM-only); found: {name}"
            );
        }
    }
}
