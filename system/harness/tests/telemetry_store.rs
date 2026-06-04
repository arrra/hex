// Red test for telemetry module — task Trbx52r52.
//
// Verifies that the telemetry store at system/harness/src/telemetry/mod.rs
// exposes record/recent/status/prune with the documented schema, and that
// events round-trip through a real SQLite file at $HEX_DIR/.hex/telemetry/events.db.
//
// This MUST fail until the module is created and wired into lib.rs.

use hex::telemetry::{self, TelemetryEvent};

fn with_hex_dir<F: FnOnce()>(f: F) {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("HEX_DIR", tmp.path());
    f();
}

#[test]
fn record_then_recent_roundtrip() {
    with_hex_dir(|| {
        let ev = TelemetryEvent {
            source: "test-worker".to_string(),
            event: "hex::test::roundtrip".to_string(),
            status: "ok".to_string(),
            duration_ms: Some(42),
            exit_code: Some(0),
            detail: Some("hello".to_string()),
        };
        telemetry::record(&ev).expect("record should succeed");

        let rows = telemetry::recent(10).expect("recent should succeed");
        assert_eq!(rows.len(), 1, "expected exactly one row");
        let row = &rows[0];
        assert_eq!(row.source, "test-worker");
        assert_eq!(row.event, "hex::test::roundtrip");
        assert_eq!(row.status, "ok");
        assert_eq!(row.duration_ms, Some(42));
        assert_eq!(row.exit_code, Some(0));
    });
}

#[test]
fn status_aggregates_ok_and_error_counts() {
    with_hex_dir(|| {
        for status in ["ok", "ok", "error"] {
            telemetry::record(&TelemetryEvent {
                source: "w".to_string(),
                event: "hex::agg::x".to_string(),
                status: status.to_string(),
                duration_ms: Some(1),
                exit_code: Some(0),
                detail: None,
            })
            .unwrap();
        }
        let status_rows = telemetry::status().expect("status should succeed");
        let row = status_rows
            .iter()
            .find(|r| r.event == "hex::agg::x")
            .expect("agg event present");
        assert_eq!(row.run_count, 3);
        assert_eq!(row.ok_count, 2);
        assert_eq!(row.error_count, 1);
    });
}

#[test]
fn prune_removes_old_rows_when_keep_days_zero() {
    with_hex_dir(|| {
        telemetry::record(&TelemetryEvent {
            source: "w".to_string(),
            event: "hex::prune::x".to_string(),
            status: "ok".to_string(),
            duration_ms: None,
            exit_code: None,
            detail: None,
        })
        .unwrap();
        let removed = telemetry::prune(0).expect("prune should succeed");
        assert!(removed >= 1, "expected at least one row pruned, got {removed}");
        let rows = telemetry::recent(10).expect("recent after prune");
        assert!(rows.is_empty(), "expected empty after prune, got {} rows", rows.len());
    });
}
