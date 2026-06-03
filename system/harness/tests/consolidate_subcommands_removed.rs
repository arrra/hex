//! Red test for task Tqnyg6qz2: the standalone `hex memory consolidate` and
//! `hex doctor consolidate` clap subcommands must be removed, and no source
//! file may still reference the deleted `/hex-consolidate` skill.

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
fn standalone_memory_consolidate_subcommand_removed() {
    let bin = env!("CARGO_BIN_EXE_hex");
    let out = Command::new(bin)
        .args(["memory", "consolidate", "--help"])
        .output()
        .expect("run hex memory consolidate --help");
    // clap exits non-zero (typically 2) when an unknown subcommand is given.
    assert!(
        !out.status.success(),
        "`hex memory consolidate` must no longer be a valid subcommand; \
         got success status. stdout: {} stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
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
