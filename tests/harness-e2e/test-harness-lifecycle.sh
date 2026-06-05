#!/usr/bin/env bash
# tests/harness-e2e/test-harness-lifecycle.sh
#
# E2E proof of the `hex harness` at-most-once lifecycle against a LIVE
# iii engine. Designed to run inside the harness-e2e container (see
# tests/harness-e2e/Dockerfile), but is self-contained enough to run on
# any host where:
#   - `hex` is on PATH
#   - the iii engine binary is either on PATH or at the vendor path
#     resolved below (VENDOR_III)
#   - $HEX_DIR points at a writable hex workspace
#
# Coverage (each "CORE:" comment maps to a task-contract requirement):
#   (1) `hex harness start` (or hidden serve) brings the runtime up
#   (2) `hex triggers emit` fires a registered handler (marker file)
#   (3) CORE-DRAIN:    emit slow handler -> SIGTERM -> assert handler
#                      RAN TO COMPLETION (graceful drain)
#                      and its in-drain emit landed in the outbox jsonl
#   (4) CORE-REPLAY:   restart -> assert the deferred outbox emission
#                      replayed EXACTLY ONCE (marker present once)
#
# Per S6 ("no quiet failures"): if the vendored iii binary is absent we
# print a loud SKIP banner and exit 0 WITHOUT asserting any of the
# coverage above. We never silently report this suite as "covered".

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VENDOR_III_DEFAULT="${SCRIPT_DIR}/../harness-e2e/vendor/iii"
VENDOR_III="${VENDOR_III:-${VENDOR_III_DEFAULT}}"
# In the container we also accept the canonical install path.
if [ ! -x "$VENDOR_III" ] && [ -x /opt/iii-vendor/iii ]; then
    VENDOR_III=/opt/iii-vendor/iii
fi

red()    { printf '\033[31m%s\033[0m\n' "$*"; }
green()  { printf '\033[32m%s\033[0m\n' "$*"; }
yellow() { printf '\033[33m%s\033[0m\n' "$*"; }
bold()   { printf '\033[1m%s\033[0m\n' "$*"; }

# ── SKIP GUARD ────────────────────────────────────────────────────────────────
if ! command -v iii >/dev/null 2>&1 && [ ! -x "$VENDOR_III" ]; then
    yellow "════════════════════════════════════════════════════════════════════"
    yellow "  SKIP: harness-e2e/test-harness-lifecycle.sh"
    yellow "  Reason: vendored iii binary not found."
    yellow "    looked for: $VENDOR_III"
    yellow "    and:        \$(command -v iii)"
    yellow ""
    yellow "  Stage the binary before building/running this suite:"
    yellow "    mkdir -p tests/harness-e2e/vendor"
    yellow "    cp ~/.local/bin/iii tests/harness-e2e/vendor/iii"
    yellow "    chmod +x tests/harness-e2e/vendor/iii"
    yellow ""
    yellow "  This suite is NOT being reported as covered."
    yellow "  (S6: no quiet failures — a missing vendor is a real gap.)"
    yellow "════════════════════════════════════════════════════════════════════"
    exit 0
fi

if ! command -v iii >/dev/null 2>&1; then
    export PATH="$(dirname "$VENDOR_III"):$PATH"
fi

if ! command -v hex >/dev/null 2>&1; then
    red "FAIL: \`hex\` binary not on PATH"
    exit 1
fi

: "${HEX_DIR:?HEX_DIR must be set to a writable hex workspace}"

HARNESS_DIR="$HEX_DIR/.hex/harness"
OUTBOX="$HARNESS_DIR/outbox.jsonl"
MARKERS_DIR="$HARNESS_DIR/markers"
LOG="$HARNESS_DIR/harness.log"

mkdir -p "$HARNESS_DIR" "$MARKERS_DIR"
rm -f "$OUTBOX" "$LOG" "$MARKERS_DIR"/*

PASS=0
FAIL=0
pass() { PASS=$((PASS+1)); green "  ✓ $*"; }
fail() { FAIL=$((FAIL+1)); red   "  ✗ $*"; }

# ── (0) Start the iii engine ──────────────────────────────────────────────────
bold "▸ Starting iii engine"
iii start >/tmp/iii.log 2>&1 &
III_PID=$!
trap 'kill $III_PID 2>/dev/null || true; kill ${HARNESS_PID:-0} 2>/dev/null || true' EXIT
sleep 2

# ── (1) Bring up the harness runtime ──────────────────────────────────────────
bold "▸ Starting hex harness serve"
hex harness serve >"$LOG" 2>&1 &
HARNESS_PID=$!
sleep 3
if kill -0 "$HARNESS_PID" 2>/dev/null; then
    pass "harness serve is running (pid=$HARNESS_PID)"
else
    fail "harness serve died on startup; see $LOG"
fi

# ── (2) Emit -> handler ran ───────────────────────────────────────────────────
bold "▸ Emitting basic test event"
hex triggers emit "harness.e2e.ping" '{"v":1}' >/dev/null 2>&1 || true
# Give the handler time to fire and drop its marker.
for _ in 1 2 3 4 5 6 7 8 9 10; do
    [ -f "$MARKERS_DIR/ping.ran" ] && break
    sleep 0.5
done
if [ -f "$MARKERS_DIR/ping.ran" ]; then
    pass "registered test handler ran (marker present)"
else
    fail "no marker for ping handler — registered worker did not fire"
fi

# ── (3) CORE-DRAIN: slow handler + SIGTERM mid-handler ────────────────────────
bold "▸ CORE: SIGTERM mid-handler must drain to completion"
hex triggers emit "harness.e2e.slow" '{"sleep_ms":3000}' >/dev/null 2>&1 || true
sleep 1   # let the handler start
kill -TERM "$HARNESS_PID" 2>/dev/null || true

# Wait for the harness to exit (drain window is bounded; we allow generous slack).
for _ in $(seq 1 30); do
    kill -0 "$HARNESS_PID" 2>/dev/null || break
    sleep 0.5
done
if kill -0 "$HARNESS_PID" 2>/dev/null; then
    fail "harness did not exit after SIGTERM (drain hang)"
    kill -KILL "$HARNESS_PID" 2>/dev/null || true
else
    pass "harness exited after SIGTERM"
fi

# (a) handler RAN TO COMPLETION (graceful drain)
if [ -f "$MARKERS_DIR/slow.completed" ]; then
    pass "slow handler RAN TO COMPLETION after SIGTERM (graceful drain)"
else
    fail "slow handler did not complete — drain abandoned in-flight work"
fi

# (b) in-drain emit landed in the durable outbox (NOT delivered to engine)
if [ -s "$OUTBOX" ] && grep -q "harness.e2e.drained" "$OUTBOX"; then
    pass "in-drain ctx.emit was diverted to the outbox jsonl"
else
    fail "expected drain-window emission missing from outbox: $OUTBOX"
fi

# ── (4) CORE-REPLAY: restart -> outbox replays EXACTLY ONCE ───────────────────
bold "▸ CORE: restart must replay the outbox exactly once"
rm -f "$MARKERS_DIR/drained.delivered"
hex harness serve >>"$LOG" 2>&1 &
HARNESS_PID=$!
for _ in $(seq 1 20); do
    [ -f "$MARKERS_DIR/drained.delivered" ] && break
    sleep 0.5
done

if [ -f "$MARKERS_DIR/drained.delivered" ]; then
    pass "deferred outbox emission replayed on restart"
else
    fail "deferred outbox emission did NOT replay on restart"
fi

# Exactly-once: the marker file must record one and only one delivery.
DELIVERIES="$(wc -l <"$MARKERS_DIR/drained.delivered" 2>/dev/null | tr -d ' ' || echo 0)"
if [ "${DELIVERIES:-0}" = "1" ]; then
    pass "outbox replay delivered EXACTLY ONCE (at-most-once contract)"
else
    fail "outbox replay delivered $DELIVERIES times (expected exactly 1)"
fi

# Outbox must be empty after a clean replay (pop-then-deliver).
if [ ! -s "$OUTBOX" ]; then
    pass "outbox is empty after replay (entries were popped, not re-read)"
else
    fail "outbox still has entries after replay: $(wc -l <"$OUTBOX") line(s)"
fi

kill -TERM "$HARNESS_PID" 2>/dev/null || true
wait "$HARNESS_PID" 2>/dev/null || true

# ── Report ────────────────────────────────────────────────────────────────────
bold "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
bold "  harness-e2e lifecycle: $PASS passed, $FAIL failed"
bold "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
[ "$FAIL" -eq 0 ] || exit 1
