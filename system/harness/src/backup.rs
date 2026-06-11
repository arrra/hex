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
}
