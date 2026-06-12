//! `hex memory parse-transcripts` must be hidden from the user-facing CLI
//! (not listed under `hex memory --help`) but still callable directly
//! (cron + internal invocations rely on it).
//!
//! Red test for Tfgrc2gny.

use std::process::Command;

#[test]
fn parse_transcripts_not_listed_in_memory_help() {
    let bin = env!("CARGO_BIN_EXE_hex");
    let out = Command::new(bin)
        .args(["memory", "--help"])
        .output()
        .expect("run hex memory --help");
    assert!(
        out.status.success(),
        "`hex memory --help` failed. stdout: {} stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(
        !help.contains("parse-transcripts"),
        "`parse-transcripts` must be hidden from `hex memory --help`; \
         got listing:\n{help}"
    );
}

#[test]
fn parse_transcripts_still_callable() {
    let bin = env!("CARGO_BIN_EXE_hex");
    let out = Command::new(bin)
        .args(["memory", "parse-transcripts", "--help"])
        .output()
        .expect("run hex memory parse-transcripts --help");
    assert!(
        out.status.success(),
        "`hex memory parse-transcripts --help` must still succeed (hidden, not removed); \
         stdout: {} stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}
