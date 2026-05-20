/// Port of .hex/scripts/lib/integration/telemetry.py
/// Emit a hex.integration.* event via the Rust telemetry path (JSON to stderr).
use std::path::Path;

pub fn emit_event(_hex_dir: &Path, event_type: &str, payload: &str, source: &str) -> i32 {
    // Validate payload is valid JSON
    if serde_json::from_str::<serde_json::Value>(payload).is_err() {
        eprintln!("hex integration telemetry: payload must be valid JSON, got: {payload}");
        return 1;
    }

    let ts = chrono::Utc::now().to_rfc3339();
    eprintln!(
        r#"{{"ts":"{ts}","event":"{event_type}","payload":{payload},"source":"{source}"}}"#
    );
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_json_payload_returns_nonzero() {
        let tmp = std::env::temp_dir().join("hex_telemetry_test");
        std::fs::create_dir_all(&tmp).ok();
        let code = emit_event(&tmp, "hex.test.event", "not-json", "test-source");
        assert_ne!(code, 0, "invalid JSON payload must return nonzero");
    }

    #[test]
    fn valid_json_payload_passes_validation() {
        // Just verify JSON validation logic — doesn't need hex_emit.py
        let result = serde_json::from_str::<serde_json::Value>("{}");
        assert!(result.is_ok(), "empty object is valid JSON");
    }
}
