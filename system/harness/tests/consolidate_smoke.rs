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

    dir
}

#[test]
fn consolidate_writes_log_and_does_not_panic() {
    let hex_dir = build_fake_hex_dir();
    let bin = env!("CARGO_BIN_EXE_hex");

    let output = Command::new(bin)
        // Layer 1 (structural) runs as part of the unified `hex memory consolidate`
        // orchestrator; `quick` keeps it deterministic with no LLM/network.
        .args(["memory", "consolidate", "quick"])
        .env("HEX_DIR", hex_dir.path())
        .output()
        .expect("hex binary must run");

    // Exits 0 or 1 (issues found); anything else (e.g. 127, panic) is a failure.
    let code = output.status.code().unwrap_or(2);
    assert!(
        code == 0 || code == 1,
        "unexpected exit code {code}; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let log_path = hex_dir.path().join("evolution").join("consolidation-latest.log");
    assert!(log_path.exists(), "consolidation-latest.log must be written");

    let log = fs::read_to_string(&log_path).expect("read log");
    assert!(
        log.contains("Consolidation Report"),
        "log must contain 'Consolidation Report'; got:\n{log}"
    );
}
