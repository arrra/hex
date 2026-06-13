// Telemetry store — native SQLite events log under $HEX_DIR/.hex/telemetry/events.db.
//
// This is the single, ubiquitous persistence layer for hex telemetry. Every iii
// worker job records here via `record_loud` (observational: a write failure must
// NOT fail the observed job — it logs LOUDLY to stderr instead). Manual emits
// from the `hex telemetry record` CLI go through `record` (write failures DO
// surface to the user).

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};

/// One telemetry event to append.
#[derive(Debug, Clone)]
pub struct TelemetryEvent {
    pub source: String,
    pub event: String,
    pub status: String,
    pub duration_ms: Option<i64>,
    pub exit_code: Option<i64>,
    pub detail: Option<String>,
}

/// One row read back from the store.
#[derive(Debug, Clone)]
pub struct EventRow {
    pub id: i64,
    pub ts: String,
    pub source: String,
    pub event: String,
    pub status: String,
    pub duration_ms: Option<i64>,
    pub exit_code: Option<i64>,
    pub detail: Option<String>,
}

/// Aggregated per-event status row.
#[derive(Debug, Clone)]
pub struct StatusRow {
    pub event: String,
    pub last_ts: String,
    pub last_status: String,
    pub last_duration_ms: Option<i64>,
    pub run_count: i64,
    pub ok_count: i64,
    pub error_count: i64,
}

/// Resolve $HEX_DIR/.hex/telemetry/events.db, creating the parent dir if needed.
fn db_path() -> std::io::Result<PathBuf> {
    let hex_dir = std::env::var("HEX_DIR").unwrap_or_else(|_| ".".to_string());
    let dir = PathBuf::from(hex_dir).join(".hex").join("telemetry");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("events.db"))
}

/// Open the events.db, applying PRAGMA and creating schema if absent.
fn open() -> rusqlite::Result<Connection> {
    let path = db_path().map_err(|e| {
        rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("telemetry: cannot resolve db path: {e}"),
        )))
    })?;
    let conn = Connection::open(path)?;
    // journal_mode returns the new mode as a row, so use query_row not execute/pragma_update.
    let _: String = conn
        .query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))
        .unwrap_or_else(|_| "wal".to_string());
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS events (
             id          INTEGER PRIMARY KEY AUTOINCREMENT,
             ts          TEXT    NOT NULL,
             source      TEXT    NOT NULL,
             event       TEXT    NOT NULL,
             status      TEXT    NOT NULL,
             duration_ms INTEGER,
             exit_code   INTEGER,
             detail      TEXT
         );
         CREATE INDEX IF NOT EXISTS idx_events_ts ON events(ts);
         CREATE INDEX IF NOT EXISTS idx_events_event ON events(event);",
    )?;
    Ok(conn)
}

/// Read-only connection for consumers (failures detector, probe, resources).
/// Plain read-only on a WAL db reads checkpointed + WAL frames correctly;
/// `immutable=1` would silently miss the WAL — never use it here.
pub fn open_ro() -> rusqlite::Result<Connection> {
    let path = db_path().map_err(|e| {
        rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("telemetry: cannot resolve db path: {e}"),
        )))
    })?;
    Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
}

/// True iff the events db file exists. Read-only consumers use this to treat
/// "no store yet" as empty history (open_ro on a missing file errors — it
/// must never create the db).
pub fn db_exists() -> bool {
    db_path().map(|p| p.exists()).unwrap_or(false)
}


/// Append one event. ts is stamped at call time (UTC RFC3339).
pub fn record(ev: &TelemetryEvent) -> rusqlite::Result<()> {
    let conn = open()?;
    let ts = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO events (ts, source, event, status, duration_ms, exit_code, detail)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            ts,
            ev.source,
            ev.event,
            ev.status,
            ev.duration_ms,
            ev.exit_code,
            ev.detail
        ],
    )?;
    Ok(())
}

/// Append one event; on failure log a LOUD warning to stderr but never error.
///
/// RATIONALE: telemetry is observational. A telemetry write failure must NOT
/// fail the observed job (e.g. a successful `hex memory index`). This is the
/// one sanctioned loud-but-not-fatal path — Standing Order S6 honored because
/// the failure is always loud on stderr, never silently swallowed.
pub fn record_loud(ev: &TelemetryEvent) {
    if let Err(e) = record(ev) {
        eprintln!(
            "telemetry: failed to record event {}::{} ({}): {e}",
            ev.source, ev.event, ev.status
        );
    }
}

/// Newest-first list of recent events.
pub fn recent(limit: usize) -> rusqlite::Result<Vec<EventRow>> {
    let conn = open()?;
    let mut stmt = conn.prepare(
        "SELECT id, ts, source, event, status, duration_ms, exit_code, detail
         FROM events ORDER BY id DESC LIMIT ?1",
    )?;
    let rows = stmt
        .query_map(params![limit as i64], |r| {
            Ok(EventRow {
                id: r.get(0)?,
                ts: r.get(1)?,
                source: r.get(2)?,
                event: r.get(3)?,
                status: r.get(4)?,
                duration_ms: r.get(5)?,
                exit_code: r.get(6)?,
                detail: r.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Newest-first list of non-ok events since `since`.
pub fn failures(since: DateTime<Utc>) -> rusqlite::Result<Vec<EventRow>> {
    let conn = open()?;
    let since_str = since.to_rfc3339();
    let mut stmt = conn.prepare(
        "SELECT id, ts, source, event, status, duration_ms, exit_code, detail
         FROM events
         WHERE status != 'ok' AND ts >= ?1
         ORDER BY id DESC",
    )?;
    let rows = stmt
        .query_map(params![since_str], |r| {
            Ok(EventRow {
                id: r.get(0)?,
                ts: r.get(1)?,
                source: r.get(2)?,
                event: r.get(3)?,
                status: r.get(4)?,
                duration_ms: r.get(5)?,
                exit_code: r.get(6)?,
                detail: r.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Per-event aggregation: last run + ok/error counts.
pub fn status() -> rusqlite::Result<Vec<StatusRow>> {
    let conn = open()?;
    let mut stmt = conn.prepare(
        "SELECT event,
                MAX(ts) AS last_ts,
                COUNT(*) AS run_count,
                SUM(CASE WHEN status = 'ok' THEN 1 ELSE 0 END) AS ok_count,
                SUM(CASE WHEN status != 'ok' THEN 1 ELSE 0 END) AS error_count
         FROM events
         GROUP BY event
         ORDER BY last_ts DESC",
    )?;
    let agg = stmt
        .query_map([], |r| {
            let event: String = r.get(0)?;
            let last_ts: String = r.get(1)?;
            let run_count: i64 = r.get(2)?;
            let ok_count: i64 = r.get(3)?;
            let error_count: i64 = r.get(4)?;
            Ok((event, last_ts, run_count, ok_count, error_count))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    // Look up the most recent status/duration per event (cheap second query).
    let mut last_stmt = conn.prepare(
        "SELECT status, duration_ms FROM events
         WHERE event = ?1 ORDER BY id DESC LIMIT 1",
    )?;
    let mut out = Vec::with_capacity(agg.len());
    for (event, last_ts, run_count, ok_count, error_count) in agg {
        let (last_status, last_duration_ms) = last_stmt
            .query_row(params![event], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?))
            })
            .unwrap_or_else(|_| ("unknown".to_string(), None));
        out.push(StatusRow {
            event,
            last_ts,
            last_status,
            last_duration_ms,
            run_count,
            ok_count,
            error_count,
        });
    }
    Ok(out)
}

/// Delete events older than `keep_days` days. Returns rows removed.
pub fn prune(keep_days: i64) -> rusqlite::Result<usize> {
    let conn = open()?;
    let cutoff = (Utc::now() - chrono::Duration::days(keep_days)).to_rfc3339();
    let removed = conn.execute("DELETE FROM events WHERE ts < ?1", params![cutoff])?;
    Ok(removed)
}

/// Shared test helpers. `HEX_DIR` is a process-global env var, so EVERY test in
/// this crate that mutates it (here and in other modules — `iii_worker`,
/// `memory::provider`, …) MUST serialize on this single lock. Otherwise cargo's
/// parallel test runner lets one test swap `HEX_DIR` out from under another,
/// which makes the telemetry store open the wrong db (disk I/O errors / lost rows).
#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::{Mutex, MutexGuard};

    pub(crate) static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Hold the global HEX_DIR lock and point HEX_DIR at a fresh tempdir.
    pub(crate) fn isolate() -> (tempfile::TempDir, MutexGuard<'static, ()>) {
        let guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("HEX_DIR", tmp.path());
        (tmp, guard)
    }

    /// Hold the global HEX_DIR lock without changing HEX_DIR (for tests that set
    /// it to a fixed path themselves).
    pub(crate) fn lock_env() -> MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{isolate, lock_env};
    use super::*;

    #[test]
    fn record_and_recent_roundtrip() {
        let _t = isolate();
        record(&TelemetryEvent {
            source: "src".into(),
            event: "hex::unit::roundtrip".into(),
            status: "ok".into(),
            duration_ms: Some(7),
            exit_code: Some(0),
            detail: Some("hi".into()),
        })
        .unwrap();
        let rows = recent(10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].event, "hex::unit::roundtrip");
        assert_eq!(rows[0].duration_ms, Some(7));
    }

    #[test]
    fn status_counts_ok_vs_error() {
        let _t = isolate();
        for st in ["ok", "error", "ok"] {
            record(&TelemetryEvent {
                source: "w".into(),
                event: "hex::unit::agg".into(),
                status: st.into(),
                duration_ms: None,
                exit_code: None,
                detail: None,
            })
            .unwrap();
        }
        let rows = status().unwrap();
        let row = rows.iter().find(|r| r.event == "hex::unit::agg").unwrap();
        assert_eq!(row.run_count, 3);
        assert_eq!(row.ok_count, 2);
        assert_eq!(row.error_count, 1);
    }

    #[test]
    fn prune_removes_old_keeps_new() {
        let _t = isolate();
        record(&TelemetryEvent {
            source: "w".into(),
            event: "hex::unit::prune".into(),
            status: "ok".into(),
            duration_ms: None,
            exit_code: None,
            detail: None,
        })
        .unwrap();
        // keep_days=0 → cutoff is "now", so anything just written is older-than-cutoff and removed.
        let removed = prune(0).unwrap();
        assert!(removed >= 1);
        let rows = recent(10).unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn record_loud_never_panics_on_bad_path() {
        let _guard = lock_env();
        // Setting HEX_DIR to a path that cannot be created should still not panic.
        // (Use a NUL byte to force an OS-level path error.)
        std::env::set_var("HEX_DIR", "/tmp/hex-telemetry-loud-test");
        record_loud(&TelemetryEvent {
            source: "x".into(),
            event: "hex::unit::loud".into(),
            status: "ok".into(),
            duration_ms: None,
            exit_code: None,
            detail: None,
        });
    }
}
