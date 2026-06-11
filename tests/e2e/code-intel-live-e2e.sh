#!/usr/bin/env bash
# code-intel-live-e2e.sh — SPEC-A2 §6 criteria S5/S7/S8 for cq+scipd against
# hex-foundation itself (the REAL repo, not the golden fixture).
#
# Gated E2E (not unit CI): pays one real `cq index` (~40s) and one real
# rust-analyzer prime of this repo inside scipd (~60-120s, ~2GB footprint).
# Run from anywhere inside the repo:
#
#   bash tests/e2e/code-intel-live-e2e.sh
#
# Hermetic: clones the repo (at the current HEAD) to a /tmp workspace, uses a
# throwaway CODEINTEL_HOME under /tmp, and starts scipd MANUALLY (not via
# launchd) against that home. Never touches ~/.codeintel, the launchd agent,
# or the checkout it runs from. Cleans up its own clones/worktrees/daemon on
# exit.
#
# Sections (SPEC-A2 §6):
#   1. live escalation: warming is loud+bounded, then live answers   (S5)
#   2. SIGTERM scipd: A1 intact, loud degradation, no orphans        (S7)
#   3. cq check: clean / injected error / concurrent worktrees       (S8)
set -euo pipefail

# --- locate repo / binaries --------------------------------------------------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
HEAD_SHA="$(git -C "$REPO_ROOT" rev-parse HEAD)"
COMMON_GIT_DIR="$(git -C "$REPO_ROOT" rev-parse --path-format=absolute --git-common-dir)"

WORK="$(mktemp -d /tmp/cq-live-e2e.XXXXXX)"
export CODEINTEL_HOME="$WORK/codeintel-home"
CLONE="$WORK/repo"
WT_LIVE="$WORK/wt-live"
WT_A="$WORK/wt-check-a"
WT_B="$WORK/wt-check-b"
WT_C="$WORK/wt-check-c"
SCIPD_PID=""

cleanup() {
    if [[ -n "$SCIPD_PID" ]] && kill -0 "$SCIPD_PID" 2>/dev/null; then
        kill -TERM "$SCIPD_PID" 2>/dev/null || true
        for _ in 1 2 3 4 5 6 7 8 9 10; do
            kill -0 "$SCIPD_PID" 2>/dev/null || break
            sleep 1
        done
        kill -KILL "$SCIPD_PID" 2>/dev/null || true
    fi
    for wt in "$WT_LIVE" "$WT_A" "$WT_B" "$WT_C"; do
        git -C "$CLONE" worktree remove --force "$wt" >/dev/null 2>&1 || true
    done
    rm -rf "$WORK"
}
trap cleanup EXIT

if [[ -n "${CQ_BIN:-}" ]]; then
    CQ="$CQ_BIN"
else
    echo "==> building cq + scipd (cargo build -p scipd)"
    (cd "$REPO_ROOT" && cargo build -p scipd --quiet)
    CQ="$REPO_ROOT/target/debug/cq"
fi
SCIPD="${SCIPD_BIN:-$(dirname "$CQ")/scipd}"
[[ -x "$CQ" ]] || { echo "FATAL: cq binary not found at $CQ"; exit 1; }
[[ -x "$SCIPD" ]] || { echo "FATAL: scipd binary not found at $SCIPD"; exit 1; }
command -v rust-analyzer >/dev/null || { echo "FATAL: rust-analyzer not on PATH"; exit 1; }
command -v cargo >/dev/null || { echo "FATAL: cargo not on PATH (cq check needs it)"; exit 1; }

# --- helpers (A1 script conventions) -----------------------------------------
FAILED_SECTIONS=()
SECTION_RESULTS=()
ASSERT_FAILS=0

section() { echo; echo "================ SECTION $1: $2 ================"; }

pass() { echo "  PASS: $1"; }
fail() { echo "  FAIL: $1"; ASSERT_FAILS=$((ASSERT_FAILS + 1)); }

assert_eq() { # actual expected label
    if [[ "$1" == "$2" ]]; then pass "$3 (= $1)"; else fail "$3 (got '$1', want '$2')"; fi
}
assert_le() { # actual max label   (float-safe)
    if python3 -c "import sys; sys.exit(0 if float('$1') <= float('$2') else 1)"; then
        pass "$3 ($1 <= $2)"
    else
        fail "$3 ($1 > $2)"
    fi
}

rc=0 # assigned indirectly by run_cq/timed_cq (printf -v); init for shellcheck

# run_cq <rc-var-name> <stdout-file> <stderr-file> <args...> — never trips -e
run_cq() {
    local __rc_var="$1" __out="$2" __err="$3"; shift 3
    local __rc_local=0
    "$CQ" "$@" >"$__out" 2>"$__err" || __rc_local=$?
    printf -v "$__rc_var" '%s' "$__rc_local"
}

# timed_cq <rc-var> <ms-var> <stdout-file> <stderr-file> <args...>
timed_cq() {
    local __rc_var="$1" __ms_var="$2" __out="$3" __err="$4"; shift 4
    local __t0 __t1 __rc_local=0
    __t0="$(python3 -c 'import time; print(time.time())')"
    "$CQ" "$@" >"$__out" 2>"$__err" || __rc_local=$?
    __t1="$(python3 -c 'import time; print(time.time())')"
    printf -v "$__rc_var" '%s' "$__rc_local"
    printf -v "$__ms_var" '%s' "$(python3 -c "print(round(($__t1 - $__t0) * 1000))")"
}

jget() { # file python-expr-over-d
    python3 -c "import json,sys; d=json.load(open(sys.argv[1])); print(eval(sys.argv[2]))" "$1" "$2"
}

end_section() { # number
    if [[ "$ASSERT_FAILS" -eq 0 ]]; then
        SECTION_RESULTS+=("Section $1: PASS")
    else
        SECTION_RESULTS+=("Section $1: FAIL ($ASSERT_FAILS assertion(s))")
        FAILED_SECTIONS+=("$1")
    fi
    ASSERT_FAILS=0
}

# --- hermetic clone + index + manual scipd -----------------------------------
echo "==> cloning $REPO_ROOT @ ${HEAD_SHA:0:12} -> $CLONE"
git clone --quiet --no-checkout "$COMMON_GIT_DIR" "$CLONE"
git -C "$CLONE" checkout --quiet --detach "$HEAD_SHA"
git -C "$CLONE" config user.email cq-e2e@hex.local
git -C "$CLONE" config user.name "cq e2e"
mkdir -p "$CODEINTEL_HOME"

run_cq rc "$WORK/reg.json" "$WORK/reg.err" register "$CLONE"
[[ "$rc" -eq 0 ]] || { echo "FATAL: cq register exited $rc: $(cat "$WORK/reg.err")"; exit 1; }

echo "==> indexing the clone (real emit, ~40s)"
T0="$(python3 -c 'import time; print(time.time())')"
run_cq rc "$WORK/index.json" "$WORK/index.err" index --workspace "$CLONE"
T1="$(python3 -c 'import time; print(time.time())')"
[[ "$rc" -eq 0 ]] || { echo "FATAL: cq index exited $rc: $(cat "$WORK/index.err")"; exit 1; }
INDEX_SECS="$(python3 -c "print(round($T1 - $T0, 1))")"
echo "    index wall: ${INDEX_SECS}s"

echo "==> starting scipd manually (hermetic home, NOT launchd)"
"$SCIPD" >"$WORK/scipd.out.log" 2>"$WORK/scipd.err.log" &
SCIPD_PID=$!
for _ in $(seq 1 100); do
    [[ -S "$CODEINTEL_HOME/scipd.sock" ]] && break
    kill -0 "$SCIPD_PID" 2>/dev/null || { echo "FATAL: scipd died at startup: $(cat "$WORK/scipd.err.log")"; exit 1; }
    sleep 0.1
done
[[ -S "$CODEINTEL_HOME/scipd.sock" ]] || { echo "FATAL: scipd socket never appeared"; exit 1; }
echo "    scipd pid $SCIPD_PID, socket up"

# =============================================================================
section 1 "live escalation: warming loud+bounded, then live (S5)"
# =============================================================================
git -C "$CLONE" worktree add --quiet "$WT_LIVE" --detach "$HEAD_SHA"
EDIT_FILE="system/harness/src/throttle.rs"
NEW_LINE="$(($(wc -l <"$WT_LIVE/$EDIT_FILE") + 1))"
printf 'pub fn cq_e2e_brand_new() -> bool { should_throttle(true) }\n' >>"$WT_LIVE/$EDIT_FILE"
echo "  appended brand-new should_throttle call site at $EDIT_FILE:$NEW_LINE"

# Immediate stale query: index answer + escalated.warming, returned fast —
# never queued behind the rust-analyzer prime (SPEC-A2 §6 S5).
timed_cq rc WARM_MS "$WORK/warm.json" "$WORK/warm.err" refs should_throttle --workspace "$WT_LIVE"
assert_eq "$rc" 2 "stale query during prime exit code (index answer, stale)"
assert_le "$WARM_MS" 2000 "stale query during prime wall ms"
assert_eq "$(jget "$WORK/warm.json" "d['source']")" "index" "warming envelope source"
assert_eq "$(jget "$WORK/warm.json" "d['escalated']['reason']")" "warming" "escalated.reason"
assert_eq "$(jget "$WORK/warm.json" "'$EDIT_FILE' in d['stale_files']")" "True" "edited file in stale_files"
assert_eq "$(jget "$WORK/warm.json" "isinstance(d['escalated']['elapsed_secs'], int)")" "True" "escalated.elapsed_secs present"

# Poll the SAME query until the instance is ready and the answer goes live.
# Budget 240s: this is the real repo (prime measured 41-120s, smoke #3).
echo "  polling until source:\"live\" (budget 240s)..."
POLL_T0="$(date +%s)"
DEADLINE=$((POLL_T0 + 240))
LIVE_RC=""
while :; do
    run_cq LIVE_RC "$WORK/live.json" "$WORK/live.err" refs should_throttle --workspace "$WT_LIVE"
    SRC="$(jget "$WORK/live.json" "d['source']" 2>/dev/null || echo "parse-fail")"
    [[ "$SRC" == "live" ]] && break
    if [[ "$(date +%s)" -ge "$DEADLINE" ]]; then
        fail "instance never went live within 240s (last source: $SRC)"
        break
    fi
    sleep 2
done
WARMUP_SECS="$(($(date +%s) - POLL_T0))"
echo "  time to live answer: ${WARMUP_SECS}s"
if [[ "$SRC" == "live" ]]; then
    assert_eq "$LIVE_RC" 0 "live query exit code"
    assert_eq "$(jget "$WORK/live.json" "d['source']")" "live" "envelope source live"
    assert_eq "$(jget "$WORK/live.json" "d.get('escalated') is None")" "True" "no escalated on a live answer"
    assert_eq "$(jget "$WORK/live.json" "any(r['path'] == '$EDIT_FILE' and r['line'] == $NEW_LINE for r in d['results'])")" \
        "True" "live refs include the brand-new call site at $EDIT_FILE:$NEW_LINE"
fi
end_section 1

# =============================================================================
section 2 "SIGTERM scipd: A1 intact, loud degradation, no orphans (S7)"
# =============================================================================
RA_PIDS="$(pgrep -P "$SCIPD_PID" || true)"
if [[ -n "$RA_PIDS" ]]; then
    pass "scipd has live children before SIGTERM (pids: $(echo "$RA_PIDS" | tr '\n' ' '))"
else
    fail "expected at least one rust-analyzer child of scipd before SIGTERM"
fi

kill -TERM "$SCIPD_PID"
SCIPD_DEAD=0
for _ in $(seq 1 20); do
    if ! kill -0 "$SCIPD_PID" 2>/dev/null; then SCIPD_DEAD=1; break; fi
    sleep 0.5
done
assert_eq "$SCIPD_DEAD" 1 "scipd exited within 10s of SIGTERM"

# No orphan rust-analyzer: every former child must be gone (graceful
# shutdown -> exit -> SIGKILL per instance is scipd's job, not launchd's).
ORPHANS=""
for pid in $RA_PIDS; do
    for _ in $(seq 1 20); do
        kill -0 "$pid" 2>/dev/null || break
        sleep 0.5
    done
    kill -0 "$pid" 2>/dev/null && ORPHANS="$ORPHANS $pid"
done
assert_eq "${ORPHANS:-none}" "none" "no orphan rust-analyzer after SIGTERM"
SCIPD_PID=""

# Fresh query, daemon down: byte-identical A1 surface, time-bounded.
timed_cq rc DOWN_FRESH_MS "$WORK/dfresh.json" "$WORK/dfresh.err" def parse_proposal --workspace "$CLONE"
assert_eq "$rc" 0 "fresh query exit code with daemon down"
assert_le "$DOWN_FRESH_MS" 3000 "fresh query wall ms with daemon down"
assert_eq "$(jget "$WORK/dfresh.json" "d['source']")" "index" "fresh envelope source"
assert_eq "$(jget "$WORK/dfresh.json" "d.get('escalated') is None")" "True" "fresh query never consults the socket"

# Stale query, daemon down: A1 answer + escalated.daemon-unavailable, fast.
timed_cq rc DOWN_STALE_MS "$WORK/dstale.json" "$WORK/dstale.err" refs should_throttle --workspace "$WT_LIVE"
assert_eq "$rc" 2 "stale query exit code with daemon down"
assert_le "$DOWN_STALE_MS" 3000 "stale query wall ms with daemon down"
assert_eq "$(jget "$WORK/dstale.json" "d['source']")" "index" "stale envelope source"
assert_eq "$(jget "$WORK/dstale.json" "d['escalated']['reason']")" "daemon-unavailable" "escalated.reason daemon-unavailable"
assert_eq "$(jget "$WORK/dstale.json" "'$EDIT_FILE' in d['stale_files']")" "True" "stale_files still flagged"

# Forced --live with the daemon down: LIVE_UNAVAILABLE, exit 7, bounded.
timed_cq rc DOWN_LIVE_MS "$WORK/dlive.json" "$WORK/dlive.err" refs should_throttle --live --workspace "$WT_LIVE"
assert_eq "$rc" 7 "--live exit code with daemon down"
assert_le "$DOWN_LIVE_MS" 3000 "--live wall ms with daemon down"
assert_eq "$(jget "$WORK/dlive.err" "d['error']['code']")" "LIVE_UNAVAILABLE" "--live stderr error.code"

# Rename is live-only: daemon down -> exit 7, bounded, nothing written.
read -r RN_LINE RN_COL < <(python3 - "$WT_LIVE/$EDIT_FILE" <<'PYEOF'
import sys
for i, line in enumerate(open(sys.argv[1]), 1):
    idx = line.find("pub fn should_throttle")
    if idx >= 0:
        print(i, idx + len("pub fn ") + 1)
        break
PYEOF
)
BEFORE_SHA="$(shasum "$WT_LIVE/$EDIT_FILE" | cut -d' ' -f1)"
timed_cq rc RENAME_MS "$WORK/dren.json" "$WORK/dren.err" \
    rename "$EDIT_FILE:$RN_LINE:$RN_COL" should_throttle_renamed --workspace "$WT_LIVE"
assert_eq "$rc" 7 "rename exit code with daemon down"
assert_le "$RENAME_MS" 3000 "rename wall ms with daemon down"
assert_eq "$(jget "$WORK/dren.err" "d['error']['code']")" "LIVE_UNAVAILABLE" "rename stderr error.code"
assert_eq "$(shasum "$WT_LIVE/$EDIT_FILE" | cut -d' ' -f1)" "$BEFORE_SHA" "rename wrote nothing"
end_section 2

# =============================================================================
section 3 "cq check: clean / injected error / concurrent worktrees (S8)"
# =============================================================================
git -C "$CLONE" worktree add --quiet "$WT_A" --detach "$HEAD_SHA"

# "Clean" against the REAL repo is branch-agnostic: the checked-out commit
# may carry pre-existing warning-level diagnostics (cq check correctly exits
# 1 on ANY diagnostics, warnings included). The S8 baseline therefore pins:
# zero ERROR-level diagnostics, and an exit code consistent with the
# diagnostics list (0 iff empty, 1 iff non-empty). The injected type error
# below must then ADD an error-level diagnostic on top of that baseline.
check_errors() { jget "$1" "len([x for x in d['diagnostics'] if x['level'] == 'error'])"; }
check_consistent() { # rc json-file label: exit 0 iff diagnostics empty
    local n; n="$(jget "$2" "len(d['diagnostics'])")"
    if { [[ "$1" -eq 0 && "$n" -eq 0 ]] || [[ "$1" -eq 1 && "$n" -gt 0 ]]; }; then
        pass "$3 (exit $1, $n diagnostic(s))"
    else
        fail "$3 (exit $1 inconsistent with $n diagnostic(s))"
    fi
}

echo "  solo baseline check in $WT_A (cold cargo check of the real repo)..."
timed_cq rc SOLO_MS "$WORK/chk-clean.json" "$WORK/chk-clean.err" check --workspace "$WT_A"
SOLO_SECS="$(python3 -c "print(round($SOLO_MS / 1000, 1))")"
echo "  solo check wall: ${SOLO_SECS}s"
BASELINE_RC="$rc"
check_consistent "$rc" "$WORK/chk-clean.json" "baseline check exit code consistent"
assert_eq "$(check_errors "$WORK/chk-clean.json")" 0 "baseline check has no error-level diagnostics"
assert_eq "$(jget "$WORK/chk-clean.json" "isinstance(d['checked_in_ms'], int)")" "True" "checked_in_ms present"
if [[ -d "$WT_A/target-cq" ]]; then
    pass "check used per-worktree target dir ($WT_A/target-cq)"
else
    fail "expected $WT_A/target-cq to exist"
fi

# Inject a type error -> exit 1 with a structured diagnostic at path:line.
BAD_LINE="$(($(wc -l <"$WT_A/$EDIT_FILE") + 1))"
printf 'pub fn cq_e2e_broken() -> i32 { let x: i32 = "s"; x }\n' >>"$WT_A/$EDIT_FILE"
run_cq rc "$WORK/chk-bad.json" "$WORK/chk-bad.err" check --workspace "$WT_A"
assert_eq "$rc" 1 "injected-error check exit code"
assert_eq "$(jget "$WORK/chk-bad.json" "[x for x in d['diagnostics'] if x['level'] == 'error'][0]['path']")" \
    "$EDIT_FILE" "diagnostic path"
assert_eq "$(jget "$WORK/chk-bad.json" "[x for x in d['diagnostics'] if x['level'] == 'error'][0]['line']")" \
    "$BAD_LINE" "diagnostic line"
assert_eq "$(jget "$WORK/chk-bad.json" "[x for x in d['diagnostics'] if x['level'] == 'error'][0]['code']")" \
    "E0308" "diagnostic code"

# Revert -> back to the baseline (incremental, fast).
git -C "$WT_A" checkout -- "$EDIT_FILE"
run_cq rc "$WORK/chk-revert.json" "$WORK/chk-revert.err" check --workspace "$WT_A"
assert_eq "$rc" "$BASELINE_RC" "check back to baseline exit after revert"
assert_eq "$(check_errors "$WORK/chk-revert.json")" 0 "no error-level diagnostics after revert"

# Two concurrent checks in two FRESH worktrees: separate target-cq dirs mean
# no lock contention -> combined wall must beat 2x the solo cold time.
git -C "$CLONE" worktree add --quiet "$WT_B" --detach "$HEAD_SHA"
git -C "$CLONE" worktree add --quiet "$WT_C" --detach "$HEAD_SHA"
echo "  two concurrent cold checks in $WT_B and $WT_C..."
T0="$(python3 -c 'import time; print(time.time())')"
"$CQ" check --workspace "$WT_B" >"$WORK/chk-b.json" 2>"$WORK/chk-b.err" &
PB=$!
"$CQ" check --workspace "$WT_C" >"$WORK/chk-c.json" 2>"$WORK/chk-c.err" &
PC=$!
RC_B=0; wait "$PB" || RC_B=$?
RC_C=0; wait "$PC" || RC_C=$?
T1="$(python3 -c 'import time; print(time.time())')"
CONC_SECS="$(python3 -c "print(round($T1 - $T0, 1))")"
echo "  concurrent wall: ${CONC_SECS}s (solo was ${SOLO_SECS}s)"
check_consistent "$RC_B" "$WORK/chk-b.json" "concurrent check B exit consistent"
check_consistent "$RC_C" "$WORK/chk-c.json" "concurrent check C exit consistent"
assert_eq "$(check_errors "$WORK/chk-b.json")" 0 "concurrent check B has no error-level diagnostics"
assert_eq "$(check_errors "$WORK/chk-c.json")" 0 "concurrent check C has no error-level diagnostics"
assert_le "$CONC_SECS" "$(python3 -c "print(2 * $SOLO_SECS)")" "concurrent wall < 2x solo (no lock contention)"
if [[ -d "$WT_B/target-cq" && -d "$WT_C/target-cq" ]]; then
    pass "each concurrent check used its own target-cq"
else
    fail "expected separate target-cq dirs in both worktrees"
fi
end_section 3

# =============================================================================
echo
echo "================ SUMMARY ================"
for line in "${SECTION_RESULTS[@]}"; do echo "  $line"; done
echo "  timings: index ${INDEX_SECS}s | warming reply ${WARM_MS}ms | time-to-live ${WARMUP_SECS}s | daemon-down fresh ${DOWN_FRESH_MS}ms / stale ${DOWN_STALE_MS}ms / rename ${RENAME_MS}ms | solo check ${SOLO_SECS}s | concurrent checks ${CONC_SECS}s"
if [[ "${#FAILED_SECTIONS[@]}" -gt 0 ]]; then
    echo "RESULT: FAIL (sections: ${FAILED_SECTIONS[*]})"
    exit 1
fi
echo "RESULT: PASS (A2-S5/S7/S8)"
