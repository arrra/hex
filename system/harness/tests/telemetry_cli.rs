// Red test for `hex telemetry` CLI subcommand — task Trcvb0pev.
//
// Verifies that the binary exposes a `telemetry` subcommand mirroring the
// Memory/Iii enum+dispatch pattern, with record + recent round-tripping
// through the SQLite store at $HEX_DIR/.hex/telemetry/events.db.
//
// MUST fail until TelemetryCommands is wired into main.rs.

use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_hex"))
}

#[test]
fn telemetry_help_lists_subcommand() {
    let out = bin().args(["telemetry", "--help"]).output().expect("spawn hex");
    assert!(
        out.status.success(),
        "`hex telemetry --help` should succeed; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    for sub in ["recent", "failures", "status", "record", "prune"] {
        assert!(
            combined.contains(sub),
            "`hex telemetry --help` missing subcommand `{sub}`; got:\n{combined}"
        );
    }
}

#[test]
fn telemetry_record_then_recent_roundtrip() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let rec = bin()
        .env("HEX_DIR", tmp.path())
        .args([
            "telemetry",
            "record",
            "--source",
            "test",
            "--event",
            "hex::test::cli",
            "--status",
            "ok",
            "--duration-ms",
            "5",
        ])
        .output()
        .expect("spawn hex telemetry record");
    assert!(
        rec.status.success(),
        "record failed: stderr={}",
        String::from_utf8_lossy(&rec.stderr)
    );

    let recent = bin()
        .env("HEX_DIR", tmp.path())
        .args(["telemetry", "recent", "--limit", "5"])
        .output()
        .expect("spawn hex telemetry recent");
    assert!(
        recent.status.success(),
        "recent failed: stderr={}",
        String::from_utf8_lossy(&recent.stderr)
    );
    let stdout = String::from_utf8_lossy(&recent.stdout);
    assert!(
        stdout.contains("hex::test::cli"),
        "recent output should contain the recorded event id; got:\n{stdout}"
    );
}
