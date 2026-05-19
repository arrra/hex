/// Port of .hex/scripts/lib/integration/telemetry.py
/// CLI wrapper: emit a hex.integration.* event via hex_emit.py.
use std::path::Path;

pub fn emit_event(hex_dir: &Path, event_type: &str, payload: &str, source: &str) -> i32 {
    let home = std::env::var("HOME").unwrap_or_default();
    let hex_emit = std::path::PathBuf::from(&home).join(".hex-events/hex_emit.py");

    if !hex_emit.is_file() {
        eprintln!("hex integration telemetry: hex_emit.py not found at {}", hex_emit.display());
        return 1;
    }

    // Validate payload is valid JSON
    if serde_json::from_str::<serde_json::Value>(payload).is_err() {
        eprintln!("hex integration telemetry: payload must be valid JSON, got: {payload}");
        return 1;
    }

    let status = std::process::Command::new("python3")
        .arg(&hex_emit)
        .arg(event_type)
        .arg(payload)
        .arg(source)
        .env("HEX_DIR", hex_dir)
        .status()
        .unwrap_or_else(|e| {
            eprintln!("hex integration telemetry: failed to exec hex_emit.py: {e}");
            std::process::exit(1);
        });

    status.code().unwrap_or(1)
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
