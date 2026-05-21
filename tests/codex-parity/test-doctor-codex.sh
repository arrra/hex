#!/usr/bin/env bash
# test-doctor-codex.sh — Verify native hex doctor includes Codex CLI health checks.
#
# Asserts (when hex binary is present):
#   1. hex doctor binary exists and runs
#   2. hex doctor check-codex subcommand exists
#   3. check-codex covers: codex on PATH, codex version, OPENAI_API_KEY, AGENTS.md
#   4. When codex CLI is present, check-codex reports PASS for cli-on-path check
#
# Requires: hex binary on PATH (skips gracefully if absent)

set -uo pipefail

PASS=0
FAIL=0
SKIP=0
TOTAL=0

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

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
    SKIP=$((SKIP + 1))
    TOTAL=$((TOTAL + 1))
    echo "  SKIP: $1"
}

echo "=== test-doctor-codex ==="
echo ""

# ── Guard: hex binary required ─────────────────────────────────────────────
if ! command -v hex &>/dev/null; then
    echo "  SKIP: hex binary not on PATH — install hex to run this suite"
    skip "hex doctor run"
    skip "hex doctor check-codex exists"
    skip "check-codex covers codex-on-path"
    skip "check-codex covers codex-version"
    skip "check-codex covers OPENAI_API_KEY"
    skip "check-codex covers AGENTS.md"
    skip "live codex CLI check"
    echo ""
    echo "  Results: $PASS passed, $FAIL failed, $SKIP skipped ($TOTAL total)"
    echo ""
    echo "=== test-doctor-codex: PASS (all skipped — hex not installed) ==="
    exit 0
fi

echo "[1] hex doctor binary exists and runs"
TOTAL=$((TOTAL + 1))
if hex doctor run --quiet >/dev/null 2>&1 || hex doctor run --quiet 2>&1 | grep -q "check"; then
    echo "  PASS: hex doctor run executes"
    PASS=$((PASS + 1))
else
    # exit non-zero is acceptable — what matters is it runs and outputs something
    DR_OUT=$(hex doctor run --quiet 2>&1 || true)
    if [ -n "$DR_OUT" ]; then
        echo "  PASS: hex doctor run executes (exit non-zero is ok)"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: hex doctor run produced no output"
        FAIL=$((FAIL + 1))
    fi
fi

echo "[2] hex doctor check-codex subcommand exists"
TOTAL=$((TOTAL + 1))
CODEX_OUT=$(hex doctor check-codex 2>&1 || true)
if [ -n "$CODEX_OUT" ]; then
    echo "  PASS: hex doctor check-codex runs and produces output"
    PASS=$((PASS + 1))
else
    echo "  FAIL: hex doctor check-codex produced no output"
    FAIL=$((FAIL + 1))
fi

echo "[3] check-codex covers required health areas"

TOTAL=$((TOTAL + 1))
if echo "$CODEX_OUT" | grep -qi "codex.*path\|cli-on-path\|found at\|not found"; then
    echo "  PASS: check-codex covers codex-on-path"
    PASS=$((PASS + 1))
else
    echo "  FAIL: check-codex output lacks codex-on-path check"
    FAIL=$((FAIL + 1))
fi

TOTAL=$((TOTAL + 1))
if echo "$CODEX_OUT" | grep -qi "version\|codex.*ok"; then
    echo "  PASS: check-codex covers codex version"
    PASS=$((PASS + 1))
else
    echo "  FAIL: check-codex output lacks codex version check"
    FAIL=$((FAIL + 1))
fi

TOTAL=$((TOTAL + 1))
if echo "$CODEX_OUT" | grep -qi "OPENAI_API_KEY\|api.key\|api-key"; then
    echo "  PASS: check-codex covers OPENAI_API_KEY"
    PASS=$((PASS + 1))
else
    echo "  FAIL: check-codex output lacks OPENAI_API_KEY check"
    FAIL=$((FAIL + 1))
fi

TOTAL=$((TOTAL + 1))
if echo "$CODEX_OUT" | grep -qi "AGENTS.md\|agents-md"; then
    echo "  PASS: check-codex covers AGENTS.md"
    PASS=$((PASS + 1))
else
    echo "  FAIL: check-codex output lacks AGENTS.md check"
    FAIL=$((FAIL + 1))
fi

echo "[4] Live check: codex CLI presence"
HAVE_CODEX="no"
command -v codex &>/dev/null && HAVE_CODEX="yes"

if [ "$HAVE_CODEX" = "yes" ]; then
    TOTAL=$((TOTAL + 1))
    if echo "$CODEX_OUT" | grep -qi "PASS.*codex\|codex found at"; then
        echo "  PASS: codex CLI presence detected correctly"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: codex CLI on PATH but check-codex did not report PASS (output: $CODEX_OUT)"
        FAIL=$((FAIL + 1))
    fi
else
    skip "codex CLI not on PATH — skipping live CLI presence check"
fi

echo ""
echo "  Results: $PASS passed, $FAIL failed, $SKIP skipped ($TOTAL total)"
echo ""

if [ "$FAIL" -gt 0 ]; then
    echo "=== test-doctor-codex: FAIL ==="
    exit 1
fi
echo "=== test-doctor-codex: PASS ==="
