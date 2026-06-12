// Red test for `hex memory recent` (task T62tqhhwv).
//
// Builds a fake $HEX_DIR with two project dirs (one newer mtime, one older),
// a `boi:`-prefixed project that must be filtered out, a few decision files,
// and a todo.md with a `## Now` section. Then runs `hex memory recent` and
// asserts:
//   1. it exits 0
//   2. produces non-empty output
//   3. lists both project pointers and the newer one comes first (recency)
//   4. excludes the `boi:`-prefixed project (noise filter)
//
// This will fail until MemoryCommands::Recent + memory/recent.rs land.

use std::fs;
use std::process::Command;
use std::thread::sleep;
use std::time::Duration;
use tempfile::TempDir;

fn build_fake_hex_dir() -> TempDir {
    let dir = tempfile::tempdir().expect("create tempdir");
    let p = dir.path();

    fs::write(p.join("CLAUDE.md"), "").unwrap();
    fs::write(
        p.join("todo.md"),
        "## Now\n- first now item\n- second now item\n- third now item\n\n## Later\n- not now\n",
    )
    .unwrap();

    // Older project — created first.
    fs::create_dir_all(p.join("projects/older-proj")).unwrap();
    fs::write(p.join("projects/older-proj/context.md"), "older body\n").unwrap();

    // Noise: a `boi:`-prefixed project that must be filtered out.
    fs::create_dir_all(p.join("projects/boi:scratch")).unwrap();
    fs::write(p.join("projects/boi:scratch/context.md"), "boi noise\n").unwrap();

    // Force newer mtime on newer-proj by sleeping briefly.
    sleep(Duration::from_millis(1100));
    fs::create_dir_all(p.join("projects/newer-proj")).unwrap();
    fs::write(p.join("projects/newer-proj/context.md"), "newer body\n").unwrap();

    // A decision file.
    fs::create_dir_all(p.join("me/decisions")).unwrap();
    fs::write(
        p.join("me/decisions/some-decision-2026-06-05.md"),
        "# Decision: x\n",
    )
    .unwrap();

    dir
}

#[test]
fn memory_recent_lists_pointers_in_recency_order_and_filters_boi() {
    let hex_dir = build_fake_hex_dir();
    let bin = env!("CARGO_BIN_EXE_hex");

    let output = Command::new(bin)
        .args(["memory", "recent"])
        .env("HEX_DIR", hex_dir.path())
        .output()
        .expect("hex binary must run");

    let code = output.status.code().unwrap_or(2);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    assert_eq!(
        code, 0,
        "`hex memory recent` must exit 0; stderr: {stderr}\nstdout: {stdout}"
    );

    assert!(
        !stdout.trim().is_empty(),
        "`hex memory recent` must produce non-empty output; stderr: {stderr}"
    );

    let newer_idx = stdout.find("newer-proj");
    let older_idx = stdout.find("older-proj");
    assert!(
        newer_idx.is_some(),
        "output must include newer project pointer; got:\n{stdout}"
    );
    assert!(
        older_idx.is_some(),
        "output must include older project pointer; got:\n{stdout}"
    );
    assert!(
        newer_idx.unwrap() < older_idx.unwrap(),
        "newer project must appear before older (recency-ordered); got:\n{stdout}"
    );

    assert!(
        !stdout.contains("boi:scratch"),
        "`boi:`-prefixed projects must be filtered out; got:\n{stdout}"
    );

    // Pointers-only: full file bodies must not be dumped.
    assert!(
        !stdout.contains("older body") && !stdout.contains("newer body"),
        "output must be pointers only, not file bodies; got:\n{stdout}"
    );
}
