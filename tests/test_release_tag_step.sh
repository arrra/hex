#!/usr/bin/env bash
# test_release_tag_step.sh — Tests for release.sh Tag step (OBS-017 bug)
#
# The Tag step (release.sh ~lines 279-291) currently silently skips
# pushing the version tag whenever it exists locally — which is ALWAYS
# true after `release.sh bump-version` creates it. Result: pipeline
# prints "Tag v$VERSION already exists ✓" (green) but the tag never
# reaches origin. Same class as OBS-017 (no quiet failures, SO S6).
#
# This test exercises the REMOTE-aware behavior the Tag step must
# implement:
#   1. Tag absent on remote → push it, even if it already exists locally.
#   2. Tag present on remote AND matches local SHA → green-check.
#   3. Tag present on remote BUT differs from local SHA → red + exit 1.
#      Do NOT silently overwrite or skip a divergent remote tag.
#
# The test extracts ONLY the Tag step block from release.sh, runs it
# in an isolated harness with a fake `git` shim that records calls
# and serves canned `ls-remote` output.

set -uo pipefail

PASS=0
FAIL=0
TOTAL=0
pass() { echo "  PASS: $1"; PASS=$((PASS + 1)); TOTAL=$((TOTAL + 1)); }
fail() { echo "  FAIL: $1"; FAIL=$((FAIL + 1)); TOTAL=$((TOTAL + 1)); }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
RELEASE_SH="$REPO_DIR/system/scripts/release.sh"

LOCAL_SHA="1111111111111111111111111111111111111111"
DIVERGENT_REMOTE_SHA="ffffffffffffffffffffffffffffffffffffffff"
TEST_VERSION="9.9.9"

# Build a harness directory: fake git shim + extracted Tag step.
# Arg 1: remote_sha — what `git ls-remote origin vX.Y.Z` should return.
#         Empty string = tag absent on origin.
make_harness() {
    local outdir="$1"
    local remote_sha="$2"

    mkdir -p "$outdir/bin"

    # Fake git: log every call to git.log, serve canned responses.
    cat > "$outdir/bin/git" <<GITSHIM
#!/usr/bin/env bash
printf 'git %s\n' "\$*" >> "$outdir/git.log"
case "\$1 \$2" in
    "ls-remote origin")
        if [ -n "$remote_sha" ]; then
            printf '%s\trefs/tags/%s\n' "$remote_sha" "\$3"
        fi
        exit 0
        ;;
    "rev-parse "*)
        # Local tag DOES exist (simulates state after bump-version).
        echo "$LOCAL_SHA"
        exit 0
        ;;
    "tag "*)
        exit 0
        ;;
    "push origin")
        exit 0
        ;;
esac
exit 0
GITSHIM
    chmod +x "$outdir/bin/git"

    # Extract ONLY the Tag step block, with stubbed helpers and vars.
    {
        echo "set -uo pipefail"
        echo "VERSION=\"$TEST_VERSION\""
        echo "FULL_SHA=\"$LOCAL_SHA\""
        echo 'bold()  { echo "BOLD: $*"; }'
        echo 'green() { echo "GREEN: $*"; }'
        echo 'red()   { echo "RED: $*" >&2; }'
        # The Tag step lives between `# ── Tag` and `# ── Fleet notification`.
        # sed prints both delimiters; drop the final line to exclude the next header.
        sed -n '/^# ── Tag /,/^# ── Fleet notification/p' "$RELEASE_SH" | sed '$d'
    } > "$outdir/runner.sh"
}

echo "=== release.sh Tag step tests ==="
echo ""

# ── Sanity: the Tag step block extracts to something runnable ────────
echo "[0] Tag step block extracts non-empty"
TMP0=$(mktemp -d)
make_harness "$TMP0" ""
if [ -s "$TMP0/runner.sh" ] && grep -q 'Tag' "$TMP0/runner.sh"; then
    pass "Tag step extracted"
else
    fail "Tag step did not extract"
fi
rm -rf "$TMP0"

# ── Test 1: Silent-skip branch removed ───────────────────────────────
# The old code `if git rev-parse "v$VERSION" >/dev/null 2>&1; then` is
# the silent-skip branch — it returns green without checking origin.
echo "[1] Silent-skip branch removed from release.sh"
if grep -qE 'if git rev-parse "v\$VERSION" >/dev/null 2>&1; then$' "$RELEASE_SH"; then
    fail "Old silent-skip branch still present in release.sh"
else
    pass "Silent-skip branch removed"
fi

# ── Test 2: REMOTE_TAG_SHA variable present ──────────────────────────
echo "[2] Tag step uses REMOTE_TAG_SHA variable"
if grep -qE 'REMOTE_TAG_SHA|remote_tag_sha' "$RELEASE_SH"; then
    pass "REMOTE_TAG_SHA referenced"
else
    fail "REMOTE_TAG_SHA missing — Tag step is not remote-aware"
fi

# ── Test 3: Divergent remote tag → exit non-zero + red error ─────────
echo "[3] Divergent remote tag → loud error + exit 1"
TMP_DIV=$(mktemp -d)
make_harness "$TMP_DIV" "$DIVERGENT_REMOTE_SHA"
set +e
OUTPUT_DIV=$(PATH="$TMP_DIV/bin:$PATH" bash "$TMP_DIV/runner.sh" 2>&1)
EXIT_DIV=$?
set -e

OK_DIV=true
if [ "$EXIT_DIV" -eq 0 ]; then
    fail "Divergent tag: expected non-zero exit, got 0"
    OK_DIV=false
fi
if ! echo "$OUTPUT_DIV" | grep -q "^RED:"; then
    fail "Divergent tag: expected red error message, got none"
    OK_DIV=false
fi
if echo "$OUTPUT_DIV" | grep -q "^GREEN:.*$TEST_VERSION"; then
    fail "Divergent tag: must NOT print green success checkmark"
    OK_DIV=false
fi
# Must NOT silently overwrite by pushing a different SHA's tag.
if grep -q "push origin $TEST_VERSION" "$TMP_DIV/git.log" 2>/dev/null \
   || grep -q "push origin v$TEST_VERSION" "$TMP_DIV/git.log" 2>/dev/null; then
    fail "Divergent tag: must NOT silently push over a divergent remote tag"
    OK_DIV=false
fi
$OK_DIV && pass "Divergent tag: exited $EXIT_DIV with red error, no silent overwrite"
if ! $OK_DIV; then
    echo "    --- runner output ---"
    echo "$OUTPUT_DIV" | sed 's/^/    /'
    echo "    --- git calls ---"
    sed 's/^/    /' "$TMP_DIV/git.log" 2>/dev/null || echo "    (no git calls logged)"
fi
rm -rf "$TMP_DIV"

# ── Test 4: Tag local-only (absent on remote) → push attempted ───────
# This is the actual OBS-017 bug: after bump-version creates the tag
# locally, the old code's `git rev-parse` succeeds and silently skips
# the push. The new code must check origin and push when absent.
echo "[4] Tag local-only, absent on remote → push to origin"
TMP_PUSH=$(mktemp -d)
make_harness "$TMP_PUSH" ""
set +e
OUTPUT_PUSH=$(PATH="$TMP_PUSH/bin:$PATH" bash "$TMP_PUSH/runner.sh" 2>&1)
EXIT_PUSH=$?
set -e

OK_PUSH=true
if [ "$EXIT_PUSH" -ne 0 ]; then
    fail "Local-only tag: expected exit 0, got $EXIT_PUSH"
    OK_PUSH=false
fi
if ! grep -qE "push origin v?$TEST_VERSION" "$TMP_PUSH/git.log" 2>/dev/null; then
    fail "Local-only tag: expected 'git push origin v$TEST_VERSION', not recorded"
    OK_PUSH=false
fi
$OK_PUSH && pass "Local-only tag: pushed to origin (exit $EXIT_PUSH)"
if ! $OK_PUSH; then
    echo "    --- runner output ---"
    echo "$OUTPUT_PUSH" | sed 's/^/    /'
    echo "    --- git calls ---"
    sed 's/^/    /' "$TMP_PUSH/git.log" 2>/dev/null || echo "    (no git calls logged)"
fi
rm -rf "$TMP_PUSH"

# ── Summary ──────────────────────────────────────────────────────────
echo ""
echo "==============================="
echo " Results: $PASS passed, $FAIL failed ($TOTAL total)"
echo "==============================="
if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
echo ""
echo "=== ALL TAG STEP TESTS PASSED ==="
