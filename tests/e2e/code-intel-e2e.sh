#!/usr/bin/env bash
# code-intel-e2e.sh — spec S3-S8 acceptance for cq against hex-foundation itself.
#
# Gated E2E (not unit CI): emits a real SCIP index of this repo, which takes
# ~40s and ~3GB RSS for rust-analyzer. Run from anywhere inside the repo:
#
#   bash tests/e2e/code-intel-e2e.sh
#
# Hermetic: clones the repo (at the current HEAD) to a /tmp workspace and uses
# a throwaway CODEINTEL_HOME under /tmp. Never touches ~/.codeintel or the
# checkout it runs from. Cleans up its own clones/worktrees on exit.
#
# Sections (spec §8):
#   1. register + index the repo            (S3)
#   2. 5 known-symbol queries, grep-checked (S3)
#   3. fresh-worktree cold start <2s        (S4)
#   4. stale flagging + --strict + overhead (S5)
#   5. query latency p95 <500ms             (S7)
#   6. 8 parallel readers during reindex    (S8)
set -euo pipefail

# --- locate repo / binary ----------------------------------------------------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
HEAD_SHA="$(git -C "$REPO_ROOT" rev-parse HEAD)"
COMMON_GIT_DIR="$(git -C "$REPO_ROOT" rev-parse --path-format=absolute --git-common-dir)"

WORK="$(mktemp -d /tmp/cq-e2e.XXXXXX)"
export CODEINTEL_HOME="$WORK/codeintel-home"
CLONE="$WORK/repo"

cleanup() {
    git -C "$CLONE" worktree remove --force "$WORK/wt-cold" >/dev/null 2>&1 || true
    git -C "$CLONE" worktree remove --force "$WORK/wt-stale" >/dev/null 2>&1 || true
    rm -rf "$WORK"
}
trap cleanup EXIT

if [[ -n "${CQ_BIN:-}" ]]; then
    CQ="$CQ_BIN"
else
    echo "==> building cq (cargo build -p scipd)"
    (cd "$REPO_ROOT" && cargo build -p scipd --quiet)
    CQ="$REPO_ROOT/target/debug/cq"
fi
[[ -x "$CQ" ]] || { echo "FATAL: cq binary not found at $CQ"; exit 1; }
command -v rust-analyzer >/dev/null || { echo "FATAL: rust-analyzer not on PATH (cq doctor would tell you the same)"; exit 1; }

# --- helpers -----------------------------------------------------------------
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

# run_cq <rc-var-name> <stdout-file> <stderr-file> <args...> — never trips -e
run_cq() {
    local __rc_var="$1" __out="$2" __err="$3"; shift 3
    local __rc_local=0
    "$CQ" "$@" >"$__out" 2>"$__err" || __rc_local=$?
    printf -v "$__rc_var" '%s' "$__rc_local"
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

# --- hermetic clone ----------------------------------------------------------
echo "==> cloning $REPO_ROOT @ ${HEAD_SHA:0:12} -> $CLONE"
git clone --quiet --no-checkout "$COMMON_GIT_DIR" "$CLONE"
git -C "$CLONE" checkout --quiet --detach "$HEAD_SHA"
git -C "$CLONE" config user.email cq-e2e@hex.local
git -C "$CLONE" config user.name "cq e2e"
mkdir -p "$CODEINTEL_HOME"

# =============================================================================
section 1 "register + index (S3)"
# =============================================================================
run_cq rc "$WORK/reg.json" "$WORK/reg.err" register "$CLONE"
assert_eq "$rc" 0 "cq register exit code"
WSID="$(jget "$WORK/reg.json" "d['registered']")"
echo "  workspace_id: $WSID"

T0="$(python3 -c 'import time; print(time.time())')"
run_cq rc "$WORK/index1.json" "$WORK/index1.err" index --workspace "$CLONE"
T1="$(python3 -c 'import time; print(time.time())')"
assert_eq "$rc" 0 "cq index exit code"
EMIT_SECS="$(jget "$WORK/index1.json" "d['emit_duration_secs']")"
WALL_SECS="$(python3 -c "print(round($T1 - $T0, 1))")"
GEN1="$(jget "$WORK/index1.json" "d['generation']")"
echo "  emit_duration_secs: $EMIT_SECS (index wall: ${WALL_SECS}s) generation: $GEN1"
assert_eq "$(jget "$WORK/index1.json" "d['commit_sha']")" "$HEAD_SHA" "indexed commit_sha == clone HEAD"
assert_eq "$(jget "$WORK/index1.json" "d['emit_exit_code']")" 0 "emit_exit_code"

run_cq rc "$WORK/doctor.json" "$WORK/doctor.err" doctor
assert_eq "$rc" 0 "cq doctor green after index"
end_section 1

# =============================================================================
section 2 "5 known-symbol def queries, grep-verified (S3)"
# =============================================================================
# symbol|file — expected line is computed by grep in the clone, so the
# assertion self-adjusts if the source moves but still pins symbol->file:line.
GOLDEN="parse_proposal|system/harness/src/gatekeeper.rs
load_canaries|system/harness/src/gatekeeper.rs
tighten_parent_dir|system/harness/src/ledger.rs
should_throttle|system/harness/src/throttle.rs
lower_to_background|system/harness/src/throttle.rs"

while IFS='|' read -r sym file; do
    expected_line="$(grep -n "fn ${sym}[(<]" "$CLONE/$file" | head -1 | cut -d: -f1)"
    [[ -n "$expected_line" ]] || { fail "grep precondition: fn $sym in $file"; continue; }
    run_cq rc "$WORK/def.json" "$WORK/def.err" def "$sym" --workspace "$CLONE"
    assert_eq "$rc" 0 "def $sym exit code"
    got="$(jget "$WORK/def.json" "[(r['path'],r['line']) for r in d['results'] if r['role']=='definition'][0]")"
    assert_eq "$got" "('$file', $expected_line)" "def $sym -> $file:$expected_line"
done <<<"$GOLDEN"

# refs sanity on one symbol: definition flagged, >1 site total
run_cq rc "$WORK/refs.json" "$WORK/refs.err" refs should_throttle --workspace "$CLONE"
assert_eq "$rc" 0 "refs should_throttle exit code"
NREFS="$(jget "$WORK/refs.json" "len(d['results'])")"
if [[ "$NREFS" -gt 1 ]]; then pass "refs should_throttle has $NREFS sites"; else fail "refs should_throttle only $NREFS site(s)"; fi
end_section 2

# =============================================================================
section 3 "fresh worktree cold start <2s, no new generation, clean teardown (S4)"
# =============================================================================
gen_count() { find "$CODEINTEL_HOME/$WSID" -maxdepth 1 -type d -name '2*' | wc -l | tr -d ' '; }
GENS_BEFORE="$(gen_count)"
HOME_BEFORE="$(find "$CODEINTEL_HOME" -mindepth 1 -maxdepth 1 | sort)"

git -C "$CLONE" worktree add --quiet "$WORK/wt-cold" --detach "$HEAD_SHA"
T0="$(python3 -c 'import time; print(time.time())')"
run_cq rc "$WORK/cold.json" "$WORK/cold.err" def parse_proposal --workspace "$WORK/wt-cold"
T1="$(python3 -c 'import time; print(time.time())')"
COLD_MS="$(python3 -c "print(round(($T1 - $T0) * 1000))")"
assert_eq "$rc" 0 "worktree first-query exit code"
assert_le "$COLD_MS" 2000 "worktree cold start ms"
assert_eq "$(jget "$WORK/cold.json" "d['workspace_id']")" "$WSID" "worktree resolves to parent workspace_id"
assert_eq "$(gen_count)" "$GENS_BEFORE" "no new generation created by worktree query"

git -C "$CLONE" worktree remove --force "$WORK/wt-cold"
assert_eq "$(find "$CODEINTEL_HOME" -mindepth 1 -maxdepth 1 | sort)" "$HOME_BEFORE" "no residue in CODEINTEL_HOME after teardown"
end_section 3

# =============================================================================
section 4 "stale flagging, --strict exit 2, freshness overhead <150ms p95 (S5)"
# =============================================================================
git -C "$CLONE" worktree add --quiet "$WORK/wt-stale" --detach "$HEAD_SHA"
echo "// cq-e2e stale marker" >>"$WORK/wt-stale/system/harness/src/throttle.rs"

run_cq rc "$WORK/stale.json" "$WORK/stale.err" def should_throttle --workspace "$WORK/wt-stale"
assert_eq "$rc" 2 "stale (non-strict) exit code 2"
assert_eq "$(jget "$WORK/stale.json" "'system/harness/src/throttle.rs' in d['stale_files']")" "True" "edited file listed in stale_files"

run_cq rc "$WORK/strict.json" "$WORK/strict.err" def should_throttle --strict --workspace "$WORK/wt-stale"
assert_eq "$rc" 2 "--strict exit code 2"
assert_eq "$(jget "$WORK/strict.err" "d['error']['code']")" "STALE_RESULTS" "--strict stderr error.code"

# Freshness overhead = the git work freshness::check adds per query
# (ls-files -s + diff --name-only over the result paths), measured directly.
FRESH_P95="$(python3 - "$WORK/wt-stale" <<'PYEOF'
import subprocess, sys, time
root = sys.argv[1]
paths = ["system/harness/src/throttle.rs", "system/harness/src/gatekeeper.rs",
         "system/harness/src/ledger.rs"]
samples = []
for _ in range(20):
    t0 = time.monotonic()
    subprocess.run(["git", "-C", root, "ls-files", "-s", "-z", "--", *paths],
                   check=True, capture_output=True)
    subprocess.run(["git", "-C", root, "diff", "--name-only", "-z", "--", *paths],
                   check=True, capture_output=True)
    samples.append((time.monotonic() - t0) * 1000)
samples.sort()
print(round(samples[int(len(samples) * 0.95) - 1], 1))
PYEOF
)"
assert_le "$FRESH_P95" 150 "freshness overhead p95 ms (20 runs)"
git -C "$CLONE" worktree remove --force "$WORK/wt-stale"
end_section 4

# =============================================================================
section 5 "query latency p95 <500ms over 20 mixed queries (S7)"
# =============================================================================
QUERY_P95="$(CQ="$CQ" CLONE="$CLONE" python3 <<'PYEOF'
import json, os, subprocess, sys, time
cq, clone = os.environ["CQ"], os.environ["CLONE"]
queries = [
    ["def", "parse_proposal"], ["def", "load_canaries"], ["def", "tighten_parent_dir"],
    ["def", "should_throttle"], ["def", "lower_to_background"],
    ["refs", "should_throttle"], ["refs", "load_canaries"], ["refs", "tighten_parent_dir"],
    ["callers", "should_throttle"], ["callers", "sha256_hex"], ["callers", "parse_proposal"],
    ["symbols", "system/harness/src/throttle.rs"], ["symbols", "system/harness/src/ledger.rs"],
    ["search", "throttle"], ["search", "gatekeep"], ["search", "ledger"],
    ["def", "sha256_hex"], ["refs", "parse_auditor_verdicts"],
    ["search", "consolid"], ["symbols", "system/harness/src/dial.rs"],
]
samples = []
for q in queries:
    t0 = time.monotonic()
    r = subprocess.run([cq, *q, "--workspace", clone], capture_output=True)
    ms = (time.monotonic() - t0) * 1000
    if r.returncode not in (0, 2):
        print(f"query {q} exited {r.returncode}: {r.stderr.decode()[:200]}", file=sys.stderr)
        sys.exit(1)
    samples.append(ms)
samples.sort()
print(round(samples[int(len(samples) * 0.95) - 1], 1))
PYEOF
)"
assert_le "$QUERY_P95" 500 "mixed-query wall p95 ms (20 queries)"
end_section 5

# =============================================================================
section 6 "8 parallel readers during in-flight reindex (S8)"
# =============================================================================
# New commit in the clone so the republished generation has a DIFFERENT
# indexed_commit — making generation consistency observable.
echo "// cq-e2e reindex marker" >>"$CLONE/system/harness/src/throttle.rs"
git -C "$CLONE" -c commit.gpgsign=false commit --quiet -am "e2e: reindex marker"
NEW_SHA="$(git -C "$CLONE" rev-parse HEAD)"
echo "  old commit ${HEAD_SHA:0:12} -> new commit ${NEW_SHA:0:12}"

"$CQ" index --workspace "$CLONE" >"$WORK/index2.json" 2>"$WORK/index2.err" &
INDEX_PID=$!

reader_loop() { # id — query until the background index finishes
    local i=0
    while kill -0 "$INDEX_PID" 2>/dev/null && [[ $i -lt 400 ]]; do
        local rc=0
        "$CQ" def should_throttle --workspace "$CLONE" >"$WORK/r$1.json" 2>/dev/null || rc=$?
        if [[ "$rc" -eq 0 || "$rc" -eq 2 ]]; then
            jget "$WORK/r$1.json" "d['indexed_commit']" >>"$WORK/commits.$1" 2>/dev/null || echo "JSON_PARSE_FAIL" >>"$WORK/commits.$1"
        else
            echo "RC_$rc" >>"$WORK/commits.$1"
        fi
        i=$((i + 1))
    done
}
for n in 1 2 3 4 5 6 7 8; do reader_loop "$n" & done
wait "$INDEX_PID" && INDEX_RC=0 || INDEX_RC=$?
wait
assert_eq "$INDEX_RC" 0 "background cq index exit code"

TOTAL_RESPONSES="$(cat "$WORK"/commits.* | wc -l | tr -d ' ')"
BAD="$(cat "$WORK"/commits.* | grep -cv -e "^$HEAD_SHA\$" -e "^$NEW_SHA\$" || true)"
echo "  $TOTAL_RESPONSES responses across 8 readers during reindex"
if [[ "$TOTAL_RESPONSES" -ge 8 ]]; then pass "every reader got responses"; else fail "too few responses ($TOTAL_RESPONSES)"; fi
assert_eq "$BAD" 0 "all responses exit 0/2 with indexed_commit in {old,new}"

run_cq rc "$WORK/post.json" "$WORK/post.err" def should_throttle --workspace "$CLONE"
assert_eq "$rc" 0 "post-reindex query exit code (fresh again)"
assert_eq "$(jget "$WORK/post.json" "d['indexed_commit']")" "$NEW_SHA" "post-reindex indexed_commit == new HEAD"
end_section 6

# =============================================================================
echo
echo "================ SUMMARY ================"
for line in "${SECTION_RESULTS[@]}"; do echo "  $line"; done
echo "  timings: index emit ${EMIT_SECS}s | worktree cold start ${COLD_MS}ms | freshness p95 ${FRESH_P95}ms | query p95 ${QUERY_P95}ms"
if [[ "${#FAILED_SECTIONS[@]}" -gt 0 ]]; then
    echo "RESULT: FAIL (sections: ${FAILED_SECTIONS[*]})"
    exit 1
fi
echo "RESULT: PASS (S3-S8)"
