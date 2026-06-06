#!/usr/bin/env bash
# test-questions.sh — proves conversation-less question/reply + multi-conversation
# interleaving. Sourced by run-all.sh (provides PASS/FAIL/assert_* + colors).
#
# Determinism: the worker is swapped via HEX_QUESTION_WORKER (echo | fixture path),
# so no live LLM. Every ask/reply spawns its OWN `hex` process against a fresh,
# isolated HEX_DIR — cold-process statelessness is exercised by construction.
# NOTE: do NOT add `set -e`/`set -u` here — this file is SOURCED into run-all,
# and an abort would skip later asserts / corrupt the PASS/FAIL counters.

SUITE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FIX="$SUITE_DIR/../fixtures"
HEX_BIN="${HEX_BIN:-$HEX_DIR/.hex/bin/hex}"

echo ""
echo "=== QUESTIONS / REPLIES E2E (conversation-less + interleaving) ==="

# fresh, isolated workspace per scenario (own memory.db; CLAUDE.md required by get_hex_dir)
fresh_ws() { local d; d="$(mktemp -d)"; mkdir -p "$d/.hex"; : > "$d/CLAUDE.md"; echo "$d"; }
# ask <ws> <fixture> -> prints the minted question id
ask() { HEX_DIR="$1" HEX_QUESTION_WORKER="$2" "$HEX_BIN" messages submit "ask" 2>/dev/null \
        | sed -n 's/^hex asks (question \([A-Z0-9]*\)).*/\1/p'; }
# reply <ws> <qid> <sel> -> prints the echoed worker input (pin carries the chosen description)
reply() { HEX_DIR="$1" HEX_QUESTION_WORKER=echo "$HEX_BIN" messages reply "$2" "$3" 2>/dev/null; }

# ── E2E-I1: three open questions, answered out of order, no cross-contamination ──
WS=$(fresh_ws)
QI=$(ask "$WS" "$FIX/q-invest.json"); QS=$(ask "$WS" "$FIX/q-storage.json"); QC=$(ask "$WS" "$FIX/q-cal.json")
OUT_S=$(reply "$WS" "$QS" y); OUT_I=$(reply "$WS" "$QI" b); OUT_C=$(reply "$WS" "$QC" yes)
assert_contains "$OUT_S" "postgres server"     "I1: storage reply resolved to y's description"
assert_contains "$OUT_I" "sell ETH on the rebuy" "I1: invest reply resolved to b's description"
assert_contains "$OUT_C" "move the 3pm to 4pm" "I1: calendar reply resolved to yes's description"
assert_not_contains "$OUT_I" "postgres" "I1: invest reply did NOT leak storage context"

# ── E2E-I2 (HEADLINE): option-id collision across questions → per-question scoping ──
WS=$(fresh_ws)
QI=$(ask "$WS" "$FIX/q-invest.json"); QS=$(ask "$WS" "$FIX/q-storage.json")
OUT_IA=$(reply "$WS" "$QI" a); OUT_SA=$(reply "$WS" "$QS" a)
assert_contains "$OUT_IA" "keep current mix" "I2: invest 'a' resolves to invest's description"
assert_contains "$OUT_SA" "use sqlite"       "I2: storage 'a' resolves to storage's description"
assert_not_contains "$OUT_IA" "sqlite" "I2: invest 'a' did NOT bind to storage's 'a'"

# ── E2E-I3: questions interleaved with normal messages, no false binds ──
WS=$(fresh_ws)
QI=$(ask "$WS" "$FIX/q-invest.json")
NORM=$(HEX_DIR="$WS" HEX_QUESTION_WORKER=echo "$HEX_BIN" messages submit "remind me to call Sagar" 2>/dev/null)
QS=$(ask "$WS" "$FIX/q-storage.json")
OUT_I=$(reply "$WS" "$QI" b); OUT_S=$(reply "$WS" "$QS" y)
assert_contains "$NORM" "call Sagar" "I3: normal message handled normally (no false bind)"
assert_contains "$OUT_I" "sell ETH on the rebuy" "I3: Q_invest still binds after interleaving"
assert_contains "$OUT_S" "postgres server" "I3: Q_storage binds after interleaving"

# ── E2E-I4: two independent replies to the same question (no dedup/block) ──
WS=$(fresh_ws)
QI=$(ask "$WS" "$FIX/q-invest.json")
OUT_A=$(reply "$WS" "$QI" a); OUT_B=$(reply "$WS" "$QI" b)
assert_contains "$OUT_A" "keep current mix"      "I4: first reply (a) executes"
assert_contains "$OUT_B" "sell ETH on the rebuy" "I4: second reply (b) executes independently"

# ── E2E-I5 (HEADLINE): cold-process interleaved scramble (each ask/reply = own process) ──
WS=$(fresh_ws)
QI=$(ask "$WS" "$FIX/q-invest.json"); QS=$(ask "$WS" "$FIX/q-storage.json"); QC=$(ask "$WS" "$FIX/q-cal.json")
OUT_C=$(reply "$WS" "$QC" no); OUT_I=$(reply "$WS" "$QI" c); OUT_S=$(reply "$WS" "$QS" y)
assert_contains "$OUT_C" "keep the 3pm"    "I5: cold-process calendar bind correct"
assert_contains "$OUT_I" "ladder cash in"  "I5: cold-process invest bind correct"
assert_contains "$OUT_S" "postgres server" "I5: cold-process storage bind correct"

# ── Guardrails: loud failures (no worker runs on these) ──
WS=$(fresh_ws); QI=$(ask "$WS" "$FIX/q-invest.json")
HEX_DIR="$WS" "$HEX_BIN" messages reply "$QI" z >/dev/null 2>&1
assert_exit 1 $? "G1: bad option id → exit 1"
HEX_DIR="$WS" "$HEX_BIN" messages reply "ZZZNONE" b >/dev/null 2>&1
assert_exit 1 $? "G2: nonexistent question id → exit 1 (referential integrity)"
HEX_DIR="$WS" "$HEX_BIN" messages reply "$QI" a,b >/dev/null 2>&1
assert_exit 1 $? "G3: two ids for single-select → exit 1"

# ── Persistence (durable system-of-record): the question row is in memory.db ──
WS=$(fresh_ws); QI=$(ask "$WS" "$FIX/q-invest.json"); reply "$WS" "$QI" b >/dev/null
N=$(sqlite3 "$WS/.hex/memory.db" "SELECT count(*) FROM messages WHERE prompt_json IS NOT NULL;" 2>/dev/null | tr -d ' ')
if [ "$N" = "1" ]; then assert_pass "P1: exactly one question persisted in memory.db"; else assert_fail "P1: expected 1 question row, got '$N'"; fi
