#!/usr/bin/env bash
# tests/harness-e2e/test-harness-lifecycle.sh
#
# E2E proof of the `hex harness` at-most-once lifecycle against a LIVE iii
# engine — which is now BAKED INTO the hex binary. `hex harness serve` starts
# the engine in-process AND hosts the typed Rust workers; there is no separate
# `iii` process (vendorless). Designed for the harness-e2e container, but runs
# on any host where `hex` is on PATH and $HEX_DIR is a writable workspace AND
# nothing else already holds the engine port (49134).
#
# Coverage (each "CORE:" maps to a task-contract requirement):
#   (1) `hex harness serve` brings the in-process engine + workers up
#   (2) `hex triggers emit` fires a registered handler (marker file)
#   (3) CORE-DRAIN:  emit slow handler -> SIGTERM -> handler RAN TO COMPLETION
#                    (graceful drain) and its in-drain ctx.emit landed in the
#                    outbox jsonl (diverted, not delivered to the engine)
#   (4) CORE-REPLAY: restart -> the deferred outbox emission replayed EXACTLY
#                    ONCE (marker line count == 1) and the outbox is drained

set -uo pipefail

red()    { printf '\033[31m%s\033[0m\n' "$*"; }
green()  { printf '\033[32m%s\033[0m\n' "$*"; }
bold()   { printf '\033[1m%s\033[0m\n' "$*"; }

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

trap 'kill ${HARNESS_PID:-0} 2>/dev/null || true' EXIT

# Wait until the harness log reports it is serving (engine bound + workers
# registered), bounded. Returns 0 on ready, 1 on timeout.
wait_serving() {
    local pid="$1" tries="${2:-40}"
    for _ in $(seq 1 "$tries"); do
        kill -0 "$pid" 2>/dev/null || return 1
        grep -q "serving" "$LOG" 2>/dev/null && return 0
        sleep 0.5
    done
    return 1
}

# ── (1) Bring up the in-process engine + worker runtime ───────────────────────
bold "▸ Starting hex harness serve (engine in-process)"
hex harness serve >"$LOG" 2>&1 &
HARNESS_PID=$!
if wait_serving "$HARNESS_PID" 40 && kill -0 "$HARNESS_PID" 2>/dev/null; then
    pass "harness serve is up (pid=$HARNESS_PID, engine in-process)"
else
    fail "harness serve did not come up; see below"
    sed 's/^/    | /' "$LOG" 2>/dev/null | tail -30
fi

# ── (2) Emit -> handler ran ───────────────────────────────────────────────────
bold "▸ Emitting basic test event (harness.e2e.ping)"
hex triggers emit "harness.e2e.ping" --data '{"v":1}' >>"$LOG" 2>&1 || red "    (emit ping returned nonzero — see $LOG)"
for _ in $(seq 1 20); do
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
hex triggers emit "harness.e2e.slow" --data '{"sleep_ms":3000}' >>"$LOG" 2>&1 || red "    (emit slow returned nonzero — see $LOG)"
sleep 1   # let the handler start (it sleeps 3s, so it's still in-flight)
kill -TERM "$HARNESS_PID" 2>/dev/null || true

# Wait for the harness to exit (drain is bounded; allow generous slack).
for _ in $(seq 1 40); do
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
for _ in $(seq 1 40); do
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
