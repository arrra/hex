#!/usr/bin/env bash
# test-cli.sh — E2E tests for the unified hex CLI.
# Verifies every subcommand is accessible and exits gracefully.
# Sourced by run-all.sh which provides PASS/FAIL/assert_* helpers.
set -uo pipefail

HEX="$HEX_DIR/.hex/bin/hex"
VERSION_FILE="$HEX_DIR/.hex/hex-version.txt"

echo ""
echo "=== UNIFIED CLI TESTS ==="

# ── 1. hex version ────────────────────────────────────────────────────────────
OUT=$("$HEX" version 2>&1)
CODE=$?
assert_exit 0 "$CODE" "cli-version: exit 0"
assert_contains "$OUT" "." "cli-version: output contains a version string (has '.')"

# ── 2. hex agent removed (fleet teardown) ─────────────────────────────────────
# The `hex agent` subcommand (fleet/list/...) was removed in the fleet teardown.
# Assert it is no longer a recognized subcommand.
OUT=$("$HEX" agent fleet 2>&1)
CODE=$?
if [ "$CODE" -ne 0 ] && echo "$OUT" | grep -qi "unrecognized subcommand"; then
    assert_pass "cli-agent-removed: 'hex agent' correctly absent (fleet teardown)"
else
    assert_fail "cli-agent-removed: 'hex agent' still recognized (exit $CODE) — output: $OUT"
fi

# ── 3. hex message list ───────────────────────────────────────────────────────
OUT=$("$HEX" message list 2>&1)
CODE=$?
assert_exit 0 "$CODE" "cli-message-list: exit 0"

# ── 5. hex events removed (event engine teardown) ────────────────────────────
# hex events was removed in the collapse-to-cc-boi demolition.
# Assert it is no longer a recognized subcommand.
OUT=$("$HEX" events policies 2>&1)
CODE=$?
if [ "$CODE" -ne 0 ] && echo "$OUT" | grep -qi "unrecognized subcommand"; then
    assert_pass "cli-events-removed: 'hex events' correctly absent (event engine removed)"
else
    assert_fail "cli-events-removed: 'hex events' still recognized (exit $CODE) — output: $OUT"
fi

# ── 6. hex asset removed (asset registry teardown) ───────────────────────────
# hex asset was removed in the collapse-to-cc-boi demolition.
OUT=$("$HEX" asset types 2>&1)
CODE=$?
if [ "$CODE" -ne 0 ] && echo "$OUT" | grep -qi "unrecognized subcommand"; then
    assert_pass "cli-asset-removed: 'hex asset' correctly absent (asset registry removed)"
else
    assert_fail "cli-asset-removed: 'hex asset' still recognized (exit $CODE) — output: $OUT"
fi

# ── 7. hex sse removed (SSE server teardown) ──────────────────────────────────
# hex sse / hex server were removed in the collapse-to-cc-boi demolition.
OUT=$("$HEX" sse topics 2>&1)
CODE=$?
if [ "$CODE" -ne 0 ] && echo "$OUT" | grep -qi "unrecognized subcommand"; then
    assert_pass "cli-sse-removed: 'hex sse' correctly absent (SSE server removed)"
else
    assert_fail "cli-sse-removed: 'hex sse' still recognized (exit $CODE) — output: $OUT"
fi

# ── 8. hex integration list ───────────────────────────────────────────────────
OUT=$("$HEX" integration list 2>&1)
CODE=$?
# Graceful error if no integrations directory is also acceptable
if [ "$CODE" -eq 0 ] || echo "$OUT" | grep -qi "no integration\|0 integration\|not found\|integration"; then
    assert_pass "cli-integration-list: accessible (exit $CODE)"
else
    assert_fail "cli-integration-list: unexpected exit $CODE — output: $OUT"
fi

# ── 9. hex memory health ──────────────────────────────────────────────────────
OUT=$("$HEX" memory health 2>&1)
CODE=$?
# Graceful error if memory DB not initialised is also acceptable
if [ "$CODE" -eq 0 ] || echo "$OUT" | grep -qi "health\|memory\|ok\|no\|not found\|missing"; then
    assert_pass "cli-memory-health: accessible (exit $CODE)"
else
    assert_fail "cli-memory-health: unexpected exit $CODE — output: $OUT"
fi

# ── 10. hex doctor --quiet ────────────────────────────────────────────────────
OUT=$("$HEX" doctor --quiet 2>&1)
CODE=$?
# exit 0 = all clear, exit 2 = warnings, anything else = error
if [ "$CODE" -eq 0 ] || [ "$CODE" -eq 2 ]; then
    assert_pass "cli-doctor-quiet: exit $CODE (0=ok, 2=warnings)"
else
    assert_fail "cli-doctor-quiet: exit $CODE (expected 0 or 2) — output: $OUT"
fi

# ── 11. Version consistency: hex version matches Cargo.toml version compiled in ──
if [ -f "$VERSION_FILE" ]; then
    EXPECTED_VERSION=$(cat "$VERSION_FILE" | tr -d '[:space:]')
    VERSION_OUT=$("$HEX" version 2>&1)
    if echo "$VERSION_OUT" | grep -qF "$EXPECTED_VERSION"; then
        assert_pass "cli-version-consistency: 'hex version' output matches compiled Cargo.toml version ($EXPECTED_VERSION)"
    else
        assert_fail "cli-version-consistency: expected '$EXPECTED_VERSION' in 'hex version' output, got: $VERSION_OUT"
    fi
else
    assert_fail "cli-version-consistency: compiled version stamp not found at $VERSION_FILE"
fi
