// Red tests for src/ledger.rs (task Tqvtgkm9d).
//
// Pins the core behavior promised by the task contract:
//   • append-only hash-chained SQLite ledger (row_hash chains via prev_hash)
//   • `verify` walks the chain end-to-end and returns Err on any break
//   • out-of-band (direct sqlite3) writes are TAMPER-EVIDENT — detected by verify
//   • only validated row kinds are accepted by `append`
//
// These tests are expected to FAIL until src/ledger.rs is implemented.

use hex::ledger;
use serde_json::json;

fn tmp_db() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ledger.db");
    (dir, path)
}

#[test]
fn ledger_append_then_verify_ok_on_clean_chain() {
    let (_keep, path) = tmp_db();
    let l = ledger::Ledger::open(&path).expect("open ledger");

    l.append("agent-a", "test.class", "heartbeat", &json!({"ok": true}))
        .expect("append #1");
    l.append("agent-a", "test.class", "intent", &json!({"x": 1}))
        .expect("append #2");
    l.append("agent-a", "test.class", "outcome", &json!({"ok": true}))
        .expect("append #3");

    ledger::verify(&path).expect("clean chain must verify");
}

#[test]
fn ledger_rejects_unknown_kind() {
    let (_keep, path) = tmp_db();
    let l = ledger::Ledger::open(&path).expect("open ledger");
    let res = l.append("agent-a", "test.class", "garbage-kind", &json!({}));
    assert!(
        res.is_err(),
        "append must reject kinds outside intent|action|outcome|heartbeat|alert"
    );
}

#[test]
fn ledger_verify_detects_direct_out_of_band_write() {
    // Write-probe self-test (per task contract): a direct sqlite3-level
    // INSERT bypassing the append path MUST be caught by `verify`.
    let (_keep, path) = tmp_db();
    {
        let l = ledger::Ledger::open(&path).expect("open ledger");
        l.append("agent-a", "c", "heartbeat", &json!({"n": 1}))
            .expect("append seed row");
    }

    // Sneak a row in via raw sqlite — no row_hash / prev_hash linkage.
    let conn = rusqlite::Connection::open(&path).expect("raw open");
    conn.execute_batch(
        "INSERT INTO ledger (ts, agent, action_class, kind, payload, prev_hash, row_hash) \
         VALUES (0, 'attacker', 'c', 'heartbeat', '{}', 'deadbeef', 'cafebabe');",
    )
    .expect("raw insert");
    drop(conn);

    let res = ledger::verify(&path);
    assert!(
        res.is_err(),
        "verify must detect tampering / chain break from out-of-band insert"
    );
}

#[test]
fn ledger_verify_detects_mutated_payload() {
    // Mutating an existing row's payload must break the row_hash chain.
    let (_keep, path) = tmp_db();
    {
        let l = ledger::Ledger::open(&path).expect("open ledger");
        l.append("agent-a", "c", "intent", &json!({"v": "original"}))
            .expect("append");
        l.append("agent-a", "c", "outcome", &json!({"ok": true}))
            .expect("append");
    }

    let conn = rusqlite::Connection::open(&path).expect("raw open");
    conn.execute(
        "UPDATE ledger SET payload = ?1 WHERE id = 1",
        rusqlite::params!["{\"v\":\"tampered\"}"],
    )
    .expect("raw mutate");
    drop(conn);

    assert!(
        ledger::verify(&path).is_err(),
        "verify must detect mutated payload via broken row_hash"
    );
}
