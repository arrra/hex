#!/usr/bin/env bash
set -uo pipefail

# test_codex_onboarding.sh — Smoke test for Codex CLI integration with hex v2.
#
# Runs structural checks unconditionally (no API key needed).
# Runs a live Codex session only when OPENAI_API_KEY is set (CI / local with key).
#
# Usage:
#   bash tests/test_codex_onboarding.sh           # auto-detect key
#   OPENAI_API_KEY=sk-... bash tests/test_codex_onboarding.sh

PASS=0
FAIL=0
TOTAL=0

TS=$(date +%s)
HEX_INSTANCE="/tmp/hex-codex-test-${TS}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

# ── Helper ──────────────────────────────────────────────────────────────────
check() {
    TOTAL=$((TOTAL + 1))
    local name="$1"
    shift
    if "$@" >/dev/null 2>&1; then
        echo "  PASS: $name"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: $name"
        FAIL=$((FAIL + 1))
    fi
}

skip() {
    echo "  SKIP: $1"
}

echo "=== Codex Onboarding Test ==="
echo ""

# ── Load OPENAI_API_KEY from ~/.hex-test.env if not already set ──────────────
if [ -z "${OPENAI_API_KEY:-}" ] && [ -f "$HOME/.hex-test.env" ]; then
    OPENAI_API_KEY=$(grep "^OPENAI_API_KEY=" "$HOME/.hex-test.env" | cut -d= -f2- | tr -d '"' | tr -d "'" || true)
    export OPENAI_API_KEY
fi
HAVE_KEY="${OPENAI_API_KEY:+yes}"
HAVE_CODEX="no"
if command -v codex &>/dev/null; then
    HAVE_CODEX="yes"
fi

# ── [1] Fresh install ────────────────────────────────────────────────────────
echo "[1] Fresh install"
if bash "$REPO_DIR/install.sh" "$HEX_INSTANCE" >/dev/null 2>&1; then
    echo "  PASS: install.sh completed"
    PASS=$((PASS + 1))
else
    echo "  FAIL: install.sh failed"
    FAIL=$((FAIL + 1))
fi
TOTAL=$((TOTAL + 1))

# ── [2] AGENTS.md present ────────────────────────────────────────────────────
echo "[2] AGENTS.md present"
check "AGENTS.md exists" test -f "$HEX_INSTANCE/AGENTS.md"

# ── [3] .codex/config.toml created by doctor --fix ──────────────────────────
echo "[3] .codex/config.toml"
check ".codex/config.toml exists" test -f "$HEX_INSTANCE/.codex/config.toml"
check "model set in config" grep -q 'model' "$HEX_INSTANCE/.codex/config.toml"

# ── [4] Model value is codex-mini-latest ────────────────────────────────────
echo "[4] Correct model in config"
check "codex-mini-latest" grep -q 'codex-mini-latest' "$HEX_INSTANCE/.codex/config.toml"

# ── [5] Live Codex session (only when key + CLI available) ──────────────────
echo "[5] Live Codex session"
if [ "$HAVE_KEY" != "yes" ]; then
    skip "OPENAI_API_KEY not set — skipping live session"
elif [ "$HAVE_CODEX" != "yes" ]; then
    skip "codex CLI not on PATH — skipping live session"
else
    CODEX_OUT=$(cd "$HEX_INSTANCE" && \
        OPENAI_API_KEY="$OPENAI_API_KEY" \
        codex exec --model codex-mini-latest \
        "Read AGENTS.md and list the top 3 operating principles." \
        2>&1 || true)

    TOTAL=$((TOTAL + 1))
    if [ -n "$CODEX_OUT" ]; then
        echo "  PASS: codex exec returned non-empty output"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: codex exec returned empty output"
        FAIL=$((FAIL + 1))
    fi

    TOTAL=$((TOTAL + 1))
    if echo "$CODEX_OUT" | grep -qiE "Compound|Anticipate|Evolve|compound|anticipate|evolve"; then
        echo "  PASS: output references AGENTS.md principles"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: output did not reference known AGENTS.md principles"
        echo "  Output snippet: $(echo "$CODEX_OUT" | head -5)"
        FAIL=$((FAIL + 1))
    fi
fi

# ── [6] Cleanup ──────────────────────────────────────────────────────────────
echo "[6] Cleanup"
if rm -rf "$HEX_INSTANCE" 2>/dev/null; then
    echo "  PASS: test directory cleaned up"
    PASS=$((PASS + 1))
else
    echo "  FAIL: cleanup failed (manual removal needed: $HEX_INSTANCE)"
    FAIL=$((FAIL + 1))
fi
TOTAL=$((TOTAL + 1))

# ── Summary ──────────────────────────────────────────────────────────────────
echo ""
echo "========================================="
echo " Results: $PASS passed, $FAIL failed ($TOTAL total)"
if [ "${HAVE_KEY}" != "yes" ] || [ "${HAVE_CODEX}" != "yes" ]; then
    echo " Note: live Codex session skipped (key or CLI missing)"
fi
echo "========================================="

if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
echo ""
echo "=== Codex onboarding: PASS ==="
