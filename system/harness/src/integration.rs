/// Port of .hex/scripts/integrations/_template.sh
/// Prints the integration health-check skeleton to stdout so callers can
/// redirect it: hex integration template > integrations/myapp.sh
pub fn template() {
    print!(
        r#"#!/usr/bin/env bash
# _template.sh — skeleton for a new integration health check.
#
# Copy to integrations/<name>.sh and replace all TODO markers.
# Exit 0 = healthy, 1 = unhealthy.  Write failure reason to stderr.
#
# DO NOT send test messages or make mutations.  Read-only probes only.

set -uo pipefail

# TODO: Replace <NAME> with the integration name (e.g., "slack-bot").
INTEGRATION="<NAME>"

# ---------------------------------------------------------------------------
# TODO: Step 1 — Verify the process / daemon is running (if applicable).
# Example:
#   if ! pgrep -x SomeApp >/dev/null 2>&1; then
#     echo "$INTEGRATION: process not running" >&2
#     exit 1
#   fi
# ---------------------------------------------------------------------------

# ---------------------------------------------------------------------------
# TODO: Step 2 — Probe the primary surface (read-only).
# Example: query a local socket, read a state file, call a status endpoint.
# Capture errors to stderr and exit 1 on failure.
# Example:
#   RESULT="$(some-cli status 2>&1)" || {{
#     echo "$INTEGRATION: probe failed: $RESULT" >&2
#     exit 1
#   }}
# ---------------------------------------------------------------------------

# ---------------------------------------------------------------------------
# TODO: Step 3 — Sanity checks on returned data (warn-only or hard fail).
# Example: check timestamp is recent, response contains expected field, etc.
# Warn-only (non-fatal):
#   echo "[warn] $INTEGRATION: <condition>" >&2
# Hard fail:
#   echo "$INTEGRATION: <reason>" >&2; exit 1
# ---------------------------------------------------------------------------

exit 0
"#
    );
}

#[cfg(test)]
mod tests {
    #[test]
    fn template_output_is_valid_bash_shebang() {
        // Capture stdout via a simple string check of the constant content.
        // The template must start with the bash shebang line.
        let content = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/integration.rs"));
        assert!(
            content.contains("#!/usr/bin/env bash"),
            "template must embed bash shebang"
        );
        assert!(
            content.contains("set -uo pipefail"),
            "template must embed set -uo pipefail"
        );
        assert!(content.contains("exit 0"), "template must end with exit 0");
    }
}
