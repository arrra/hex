//! Consolidate surface guards:
//!  - `hex memory consolidate` is the ONE canonical orchestrator (must exist).
//!  - the old `hex doctor consolidate` fragment must stay removed.
//!  - no source file may still reference the deleted `/hex-consolidate` skill.
//!
//! (Originally task Tqnyg6qz2 folded consolidate to a top-level `hex consolidate`;
//!  it was later renested under `hex memory consolidate` — its original design
//!  home — so the memory-side guard is now an existence check, not a removal one.)

use std::path::PathBuf;
use std::process::Command;

fn src_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn read_all_rs(dir: &std::path::Path, out: &mut Vec<(PathBuf, String)>) {
    for entry in std::fs::read_dir(dir).expect("read_dir src") {
        let entry = entry.expect("dirent");
        let path = entry.path();
        if path.is_dir() {
            read_all_rs(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            let body = std::fs::read_to_string(&path).expect("read rs file");
            out.push((path, body));
        }
    }
}

#[test]
fn no_dead_skill_pointer_in_src() {
    let mut files = Vec::new();
    read_all_rs(&src_dir(), &mut files);
    let offenders: Vec<_> = files
        .iter()
        .filter(|(_, body)| body.contains("/hex-consolidate"))
        .map(|(p, _)| p.display().to_string())
        .collect();
    assert!(
        offenders.is_empty(),
        "source files still reference the removed `/hex-consolidate` skill: {offenders:?}"
    );
}

#[test]
fn memory_consolidate_is_the_canonical_subcommand() {
    let bin = env!("CARGO_BIN_EXE_hex");
    let out = Command::new(bin)
        .args(["memory", "consolidate", "--help"])
        .output()
        .expect("run hex memory consolidate --help");
    assert!(
        out.status.success(),
        "`hex memory consolidate` must be the canonical consolidate orchestrator; \
         --help failed. stdout: {} stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let help = String::from_utf8_lossy(&out.stdout).to_lowercase();
    assert!(help.contains("quick"), "help must list 'quick' mode; got:\n{help}");
    assert!(help.contains("full"), "help must list 'full' mode; got:\n{help}");
}

#[test]
fn standalone_doctor_consolidate_subcommand_removed() {
    let bin = env!("CARGO_BIN_EXE_hex");
    let out = Command::new(bin)
        .args(["doctor", "consolidate", "--help"])
        .output()
        .expect("run hex doctor consolidate --help");
    assert!(
        !out.status.success(),
        "`hex doctor consolidate` must no longer be a valid subcommand; \
         got success status. stdout: {} stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}
