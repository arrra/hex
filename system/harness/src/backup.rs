//! `hex backup` — daily sqlite snapshots with rotation. Invoked by the
//! hex-backup cron worker (modules/backup.worker.rs, 04:00Z) which existed
//! and fired for weeks before this subcommand did (FIX-010).

use std::path::Path;

pub const KEEP_DAYS: usize = 7;
const SOURCES: &[&str] = &[
    ".hex/memory.db",
    ".hex/telemetry/events.db",
    ".hex/ledger/ledger.db",
];

pub fn run(hex_dir: &Path) -> i32 {
    let stamp = chrono::Local::now().format("%Y-%m-%d").to_string();
    let out_dir = hex_dir.join(".hex/backups").join(&stamp);
    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        eprintln!("hex backup: create {}: {e}", out_dir.display());
        return 1;
    }
    let mut failures = 0;
    for rel in SOURCES {
        let src = hex_dir.join(rel);
        if !src.is_file() {
            println!("hex backup: {rel} absent — skipped");
            continue;
        }
        let dst = out_dir.join(src.file_name().unwrap());
        match snapshot(&src, &dst) {
            Ok(()) => println!("hex backup: {rel} -> {}", dst.display()),
            Err(e) => {
                eprintln!("hex backup: {rel} FAILED: {e}");
                failures += 1;
            }
        }
    }
    prune(&hex_dir.join(".hex/backups"));
    crate::telemetry::record_loud(&crate::telemetry::TelemetryEvent {
        source: "backup".into(),
        event: "backup::daily".into(),
        status: if failures == 0 { "ok".into() } else { "error".into() },
        duration_ms: None,
        exit_code: Some(if failures == 0 { 0 } else { 1 }),
        detail: Some(format!("dir={} failures={failures}", out_dir.display())),
    });
    if failures == 0 { 0 } else { 1 }
}

/// Online-safe snapshot via the sqlite backup API (correct under WAL with
/// live writers — a plain fs::copy of a hot WAL db can capture a torn state).
fn snapshot(src: &Path, dst: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let src_conn = rusqlite::Connection::open_with_flags(
        src,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let mut dst_conn = rusqlite::Connection::open(dst)?;
    let bk = rusqlite::backup::Backup::new(&src_conn, &mut dst_conn)?;
    bk.run_to_completion(100, std::time::Duration::from_millis(50), None)?;
    Ok(())
}

// ---- off-site encrypted backup (restic → mounted gdrive) -------------------
//
// `hex backup offsite` ships the whole operating layer off-machine via restic:
// client-side encrypted, deduplicated, retained, integrity-checked. One hex
// worker (modules/backup_offsite.worker.rs, 04:30) drives it. Logic lives here
// in typed Rust (no shell script — the missing `backup-to-gdrive.sh` is exactly
// what silently killed the old gdrive worker); the only external surface is the
// `restic` binary, invoked via std::process like the rest of the harness.
//
// Repo + key come from restic-native env (RESTIC_REPOSITORY +
// RESTIC_PASSWORD_COMMAND/RESTIC_PASSWORD), set in the harness env out-of-band
// (Keychain). When RESTIC_REPOSITORY is unset the job is a deliberate no-op so
// it never false-alarms before the repo is initialized.

/// Retention: plenty of recent granularity, a long thin tail. Mirrors the
/// design (keep-daily 7 / keep-weekly 4 / keep-monthly 6).
const KEEP_DAILY: &str = "7";
const KEEP_WEEKLY: &str = "4";
const KEEP_MONTHLY: &str = "6";

/// Regenerable / re-derivable paths excluded from the off-site set. Typed
/// const rather than a shipped excludes file — keeps the whole policy in the
/// compiled harness (no loose data artifact to drift). restic matches a
/// slash-less pattern against the basename anywhere; a pattern with a slash
/// against the path tail.
const EXCLUDES: &[&str] = &[
    ".hex/.upgrade-cache",
    ".hex/.upgrade-backup-*",
    ".hex/bin/.fastembed_cache",
    // Live DBs are captured via the consistent .hex/backups/<today> snapshot
    // (taken just below); excluding the hot files avoids a torn WAL copy.
    ".hex/memory.db",
    ".hex/telemetry/events.db",
    ".hex/ledger/ledger.db",
    "target",
    "node_modules",
];

/// `hex backup offsite` — consistent DB snapshot, then encrypted restic backup
/// of the operating layer to the configured repo. Returns a process exit code.
pub fn run_offsite(hex_dir: &Path) -> i32 {
    let repo = std::env::var("RESTIC_REPOSITORY").unwrap_or_default();
    if repo.trim().is_empty() {
        println!(
            "hex backup offsite: RESTIC_REPOSITORY unset — off-site backup not \
             configured, skipping (set it + a Keychain password to enable)"
        );
        return 0;
    }

    // Source set. HEX_BACKUP_SOURCE overrides it to a single path (tests scope
    // to a tmp dir); otherwise back up the whole operating layer and take a
    // fresh consistent DB snapshot first so restic ships clean copies.
    let sources: Vec<std::path::PathBuf> = match std::env::var("HEX_BACKUP_SOURCE") {
        Ok(s) if !s.trim().is_empty() => vec![std::path::PathBuf::from(s)],
        _ => {
            let rc = run(hex_dir); // consistent sqlite snapshot into .hex/backups/<today>
            if rc != 0 {
                eprintln!(
                    "hex backup offsite: pre-snapshot returned {rc} — proceeding \
                     with the latest good snapshot on disk"
                );
            }
            operating_layer_sources(hex_dir)
        }
    };
    let existing: Vec<String> = sources
        .iter()
        .filter(|p| {
            let ok = p.exists();
            if !ok {
                println!("hex backup offsite: {} absent — skipped", p.display());
            }
            ok
        })
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    if existing.is_empty() {
        return fail_offsite("no backup sources exist on disk");
    }

    // Clear any stale lock left by a previous run on the eventually-consistent
    // gdrive mount. Best-effort — a missing/!restic env surfaces at `backup`.
    let _ = restic_step(&["unlock".into()]);

    // 1) backup
    let mut backup_args: Vec<String> = vec!["backup".into()];
    backup_args.extend(existing.iter().cloned());
    for e in EXCLUDES {
        backup_args.push("--exclude".into());
        backup_args.push((*e).into());
    }
    backup_args.push("--tag".into());
    backup_args.push("hex-offsite".into());
    if let Err(e) = restic_step(&backup_args) {
        return fail_offsite(&format!("restic backup failed: {e}"));
    }

    // 2) retention
    let forget = vec![
        "forget".into(),
        "--keep-daily".into(), KEEP_DAILY.into(),
        "--keep-weekly".into(), KEEP_WEEKLY.into(),
        "--keep-monthly".into(), KEEP_MONTHLY.into(),
        "--prune".into(),
    ];
    if let Err(e) = restic_step(&forget) {
        return fail_offsite(&format!("restic forget/prune failed: {e}"));
    }

    // 3) integrity (metadata only — never --read-data the whole repo nightly)
    if let Err(e) = restic_step(&["check".into()]) {
        return fail_offsite(&format!("restic check failed: {e}"));
    }

    crate::telemetry::record_loud(&crate::telemetry::TelemetryEvent {
        source: "backup".into(),
        event: "backup::offsite".into(),
        status: "ok".into(),
        duration_ms: None,
        exit_code: Some(0),
        detail: Some(format!("repo={repo} sources={}", existing.len())),
    });
    println!("hex backup offsite: ok ({} source(s) → {repo})", existing.len());
    0
}

/// The whole operating layer: workspace + non-git runtime state that git never
/// tracks (so a fresh-machine rebuild is possible).
fn operating_layer_sources(hex_dir: &Path) -> Vec<std::path::PathBuf> {
    let mut v = vec![hex_dir.to_path_buf()];
    if let Some(home) = std::env::var_os("HOME") {
        let home = std::path::PathBuf::from(home);
        v.push(home.join(".boi/v2/boi.db")); // BOI engine state
        v.push(home.join(".claude/projects")); // subagent transcripts exist nowhere else
    }
    v
}

/// Run one `restic <args>` invocation, inheriting env (repo + password) and
/// stdio. Err carries a human reason for both spawn failure (restic absent)
/// and a non-zero exit — never a silent success.
fn restic_step(args: &[String]) -> Result<(), String> {
    let status = std::process::Command::new("restic").args(args).status();
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("`restic {}` exited {}", args.join(" "), s)),
        Err(e) => Err(format!("could not spawn restic (installed + on PATH?): {e}")),
    }
}

/// Loud failure path (SO-S6): telemetry error row + deduped alert + non-zero.
fn fail_offsite(reason: &str) -> i32 {
    eprintln!("hex backup offsite: FAILED — {reason}");
    crate::telemetry::record_loud(&crate::telemetry::TelemetryEvent {
        source: "backup".into(),
        event: "backup::offsite".into(),
        status: "error".into(),
        duration_ms: None,
        exit_code: Some(1),
        detail: Some(reason.into()),
    });
    crate::alert::notify("backup-offsite", "Off-site backup FAILED", reason);
    1
}

fn prune(backups_root: &Path) {
    let Ok(entries) = std::fs::read_dir(backups_root) else { return };
    let mut dirs: Vec<_> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.path())
        .collect();
    dirs.sort(); // YYYY-MM-DD names sort chronologically
    while dirs.len() > KEEP_DAYS {
        let victim = dirs.remove(0);
        match std::fs::remove_dir_all(&victim) {
            Ok(()) => println!("hex backup: pruned {}", victim.display()),
            Err(e) => eprintln!("hex backup: prune {} FAILED: {e}", victim.display()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn backup_snapshots_and_prunes() {
        // Isolate HEX_DIR (telemetry's record_loud resolves the events.db via
        // $HEX_DIR) — same pattern as the other env-mutating tests.
        let (_tmp_env, _guard) = crate::telemetry::test_support::isolate();
        let tmp = tempfile::TempDir::new().unwrap();
        let hex = tmp.path();
        std::fs::create_dir_all(hex.join(".hex")).unwrap();
        let conn = rusqlite::Connection::open(hex.join(".hex/memory.db")).unwrap();
        conn.execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES(1);").unwrap();
        drop(conn);
        for i in 1..=9 {
            std::fs::create_dir_all(hex.join(format!(".hex/backups/2026-01-0{i}"))).unwrap();
        }
        assert_eq!(run(hex), 0);
        let dirs: Vec<_> = std::fs::read_dir(hex.join(".hex/backups")).unwrap().collect();
        assert_eq!(dirs.len(), KEEP_DAYS);
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let snap = rusqlite::Connection::open(hex.join(format!(".hex/backups/{today}/memory.db"))).unwrap();
        let n: i64 = snap.query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    fn find_file(dir: &Path, name: &str) -> Option<std::path::PathBuf> {
        for e in std::fs::read_dir(dir).ok()?.flatten() {
            let p = e.path();
            if p.is_dir() {
                if let Some(f) = find_file(&p, name) {
                    return Some(f);
                }
            } else if p.file_name().and_then(|n| n.to_str()) == Some(name) {
                return Some(p);
            }
        }
        None
    }

    fn restic_available() -> bool {
        std::process::Command::new("restic")
            .arg("version")
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    #[test]
    fn offsite_unconfigured_is_noop() {
        let (_tmp, _g) = crate::telemetry::test_support::isolate();
        std::env::remove_var("RESTIC_REPOSITORY");
        std::env::remove_var("HEX_BACKUP_SOURCE");
        // No repo configured → clean no-op (exit 0), restic never invoked.
        assert_eq!(run_offsite(Path::new("/nonexistent-hex-dir")), 0);
    }

    #[test]
    fn offsite_loud_on_failure() {
        let (tmp, _g) = crate::telemetry::test_support::isolate();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("known.txt"), b"hello").unwrap();
        std::env::set_var("HEX_BACKUP_SOURCE", &src);
        // Unusable repo (uninitialized path) → restic backup fails; if restic is
        // absent, the spawn fails — both are the loud non-zero path.
        std::env::set_var("RESTIC_REPOSITORY", tmp.path().join("not-a-repo"));
        std::env::set_var("RESTIC_PASSWORD", "x");
        let rc = run_offsite(tmp.path());
        std::env::remove_var("HEX_BACKUP_SOURCE");
        std::env::remove_var("RESTIC_REPOSITORY");
        std::env::remove_var("RESTIC_PASSWORD");
        assert_eq!(rc, 1, "unusable repo / missing restic must be loud non-zero");
    }

    #[test]
    fn offsite_roundtrip_local_repo() {
        if !restic_available() {
            eprintln!("offsite_roundtrip_local_repo: restic not installed — skipping");
            return;
        }
        let (tmp, _g) = crate::telemetry::test_support::isolate();
        let repo = tmp.path().join("repo");
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("known.txt"), b"roundtrip-payload").unwrap();
        std::env::set_var("RESTIC_REPOSITORY", &repo);
        std::env::set_var("RESTIC_PASSWORD", "test");
        std::env::set_var("HEX_BACKUP_SOURCE", &src);

        let init = std::process::Command::new("restic").arg("init").status().unwrap();
        let rc = if init.success() { run_offsite(tmp.path()) } else { -1 };

        let restore_dir = tmp.path().join("restore");
        let restored = std::process::Command::new("restic")
            .args(["restore", "latest", "--target"])
            .arg(&restore_dir)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        std::env::remove_var("RESTIC_REPOSITORY");
        std::env::remove_var("RESTIC_PASSWORD");
        std::env::remove_var("HEX_BACKUP_SOURCE");

        assert!(init.success(), "restic init must succeed");
        assert_eq!(rc, 0, "offsite backup should succeed against a local repo");
        assert!(restored, "restic restore must succeed");
        let found = find_file(&restore_dir, "known.txt").expect("known.txt restored");
        assert_eq!(std::fs::read(found).unwrap(), b"roundtrip-payload");
    }
}
