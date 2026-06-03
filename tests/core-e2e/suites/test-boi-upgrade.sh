#!/usr/bin/env bash
# test-boi-upgrade.sh — Containerized upgrade E2E for BOI.
#
# Catches the stale-symlink bug where upgrading hex doesn't rebuild the
# binary, leaving the installed boi binary at the old version.  This is the
# test that would have caught the 2026-04-29 session's stale-binary incident.
#
# BOI NOTE: canonical engine is mrap/boi. The v2.0.0 → v3.0.0 jump is a
# CLI-breaking change *within* boi-v2 (v3 dropped `status`/`bench`, made
# `daemon` a blocking foreground command, and switched specs to TOML). So the
# post-upgrade assertions check only the v3.0.0 (HEAD-target) surface —
# version bumped to 3.0.0, binary rebuilt (newer mtime), and the v3
# subcommands (`daemon dispatch dashboard cancel`) present. We do NOT assert
# the baseline's subcommands match HEAD's, since they legitimately differ.
#
# Flow in container:
#   1. Clone hex-foundation (HEAD)
#   2. Install with BOI pinned to the baseline version (v2.0.0)
#   3. Capture version + help-line count + binary mtime
#   4. Restore HEAD's BOI_VERSION (v3.0.0); re-run install.sh — upgrade
#   5. Assert: version bumped to 3.0.0, binary mtime newer, v3 subcommands present
#   6a. Smoke dispatch (optional, requires ANTHROPIC_API_KEY)
#   6b. BAD case: corrupt symlink → run doctor → assert caught
#
# Usage:
#   Standalone:    bash tests/core-e2e/suites/test-boi-upgrade.sh
#   With dispatch: ANTHROPIC_API_KEY=<key> bash tests/core-e2e/suites/test-boi-upgrade.sh
#   Via run-all.sh: sourced automatically (shares global PASS/FAIL counters)

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"

if ! declare -f assert_pass >/dev/null 2>&1; then
    # shellcheck source=../helpers.sh
    source "$SCRIPT_DIR/../helpers.sh"
fi

echo ""
echo "=== BOI UPGRADE E2E (containerized) ==="

# ── Docker availability ───────────────────────────────────────────────────────
if ! command -v docker >/dev/null 2>&1; then
    assert_fail "boi-upgrade-prereq: docker not installed — cannot run containerized test"
    return 0 2>/dev/null || exit 1
fi
if ! docker info >/dev/null 2>&1; then
    assert_fail "boi-upgrade-prereq: docker daemon not running"
    return 0 2>/dev/null || exit 1
fi
assert_pass "boi-upgrade-prereq: docker available"

# ── Build or reuse Docker image ───────────────────────────────────────────────
# Reuse the same image as test-boi-install.sh — no redundant build.
IMAGE_TAG="hex-boi-install-test:latest"
BUILD_LOG="/tmp/boi-upgrade-build-$$.log"

if ! docker image inspect "$IMAGE_TAG" >/dev/null 2>&1; then
    yellow "  Building Docker image $IMAGE_TAG (first run — ~2-3 min)..."
    docker build -t "$IMAGE_TAG" - > "$BUILD_LOG" 2>&1 << 'DOCKERFILE_EOF'
FROM rust:latest
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
    git curl ca-certificates pkg-config libssl-dev python3 python3-pip sqlite3 \
    && rm -rf /var/lib/apt/lists/*
RUN useradd -m -s /bin/bash testuser
USER testuser
WORKDIR /home/testuser
DOCKERFILE_EOF
    BUILD_EXIT=$?
    rm -f "$BUILD_LOG"
    if [ "$BUILD_EXIT" -ne 0 ]; then
        assert_fail "boi-upgrade-docker-build: image build failed (exit $BUILD_EXIT)"
        return 0 2>/dev/null || exit 1
    fi
fi
assert_pass "boi-upgrade-docker-build: image ready ($IMAGE_TAG)"

# ── Inner script (runs inside the container as testuser) ─────────────────────
# Single-quoted heredoc: no host-side expansion. All $vars expand in-container.
read -r -d '' INNER_SCRIPT << 'INNER_EOF' || true
set -uo pipefail

PASS=0; FAIL=0
pass() { PASS=$((PASS+1)); echo "  PASS: $*"; }
fail() { FAIL=$((FAIL+1)); echo "  FAIL: $*"; }

on_exit() {
    echo ""
    echo "--- inner summary: ${PASS} passed, ${FAIL} failed ---"
    if [ "${FAIL:-0}" -gt 0 ]; then
        echo "--- boi daemon log (last 50 lines) ---"
        find "$HOME/.boi/logs" -name "*.log" 2>/dev/null \
            | xargs -r tail -n 50 2>/dev/null || true
    fi
    # Clean up for reruns
    rm -rf "$HOME/.boi" /tmp/hex "$HOME/github.com" \
           "$HOME/hex-baseline" "$HOME/hex-head" 2>/dev/null || true
    exit "${FAIL}"
}
trap on_exit EXIT

export PATH="$HOME/.boi/bin:$PATH"

# Volume mounts (/repo, /boi) are owned by the host UID, not the container
# user, so git refuses to operate on them ("detected dubious ownership").
# macOS Docker remaps ownership (so this is invisible locally) but Linux CI
# does not. The wildcard fully opts out of the ownership check — correct for
# an ephemeral CI container, and immune to gitdir path-matching quirks that
# make a specific `safe.directory /repo` entry miss `/repo/.git`.
git config --global --add safe.directory '*'

# ── 1. Clone hex-foundation (HEAD) ────────────────────────────────────────────
# Baseline mechanism: rather than checking out an old hex tag (whose historical
# install.sh carries its own bugs — Python-era boi paths, `local` outside a
# function, phantom BOI_VERSION pins), we install from HEAD's *clean* install.sh
# twice: first with VERSIONS pinned to an older real boi tag, then restored to
# HEAD's target. This exercises the upgrade/rebuild path — the bug this suite
# exists to catch — without depending on buggy old releases.
echo "--- 1. clone hex-foundation (HEAD) ---"
if git clone /repo /tmp/hex > /tmp/clone.log 2>&1; then
    pass "clone: hex-foundation cloned to /tmp/hex"
else
    fail "clone: git clone failed"
    cat /tmp/clone.log
    exit 1
fi

cd /tmp/hex
export HEX_NONINTERACTIVE=1 CI=1
if [ -d /boi/.git ]; then
    export HEX_BOI_REPO="file:///boi"
fi

# HEAD's target BOI version (what a real upgrade lands on) and the older
# baseline version we upgrade *from*.
NEW_BOI_VERSION=$(grep "^BOI_VERSION=" VERSIONS | cut -d= -f2)
# Baseline = the last boi-v2 tag before the v3.0.0 cutover. v2.0.0 builds with
# the modern install.sh and reports a parseable "boi 2.0.0".
BASELINE_BOI_VERSION="v2.0.0"

# install.sh is the fresh-install entrypoint and refuses to run over an existing
# target dir. boi_src ($HOME/github.com/mrap/boi) and the binary ($HOME/.boi)
# are HOME-based and shared across installs, so we point the two installs at
# distinct target dirs: the second install proceeds and its
# install_or_upgrade_boi sees the existing boi repo → fetch + checkout + rebuild.
BASE_TARGET="$HOME/hex-baseline"
HEAD_TARGET="$HOME/hex-head"

# ── 2. Baseline install (HEAD install.sh, BOI pinned to $BASELINE_BOI_VERSION) ─
echo "--- 2. baseline install (BOI $BASELINE_BOI_VERSION) ---"
sed -i "s|^BOI_VERSION=.*|BOI_VERSION=$BASELINE_BOI_VERSION|" VERSIONS
if bash install.sh "$BASE_TARGET" > /tmp/install-old.log 2>&1; then
    pass "install-old: baseline install.sh (BOI $BASELINE_BOI_VERSION) exited 0"
else
    INSTALL_EXIT=$?
    fail "install-old: install.sh exited $INSTALL_EXIT"
    tail -50 /tmp/install-old.log
    exit 1
fi

BOI="$HOME/.boi/bin/boi"
if [ ! -x "$BOI" ]; then
    fail "baseline-binary: $BOI not executable after baseline install"
    exit 1
fi
pass "baseline-binary: $BOI is executable after baseline install"

BASELINE_VER=$("$BOI" --version 2>&1 || echo "unknown")
BASELINE_HELP_LINES=$("$BOI" --help 2>&1 | wc -l | tr -d ' ')
BINARY_MTIME_BEFORE=$(stat -c %Y "$BOI" 2>/dev/null || echo "0")
pass "baseline-captured: version='$BASELINE_VER', help-lines=$BASELINE_HELP_LINES, mtime=$BINARY_MTIME_BEFORE"

# ── 3. Restore HEAD's VERSIONS (target upgrade version) ───────────────────────
echo "--- 3. restore VERSIONS to HEAD (BOI $NEW_BOI_VERSION) ---"
sed -i "s|^BOI_VERSION=.*|BOI_VERSION=$NEW_BOI_VERSION|" VERSIONS
pass "restore-versions: BOI_VERSION restored to $NEW_BOI_VERSION"
echo "  upgrade: BOI $BASELINE_BOI_VERSION → $NEW_BOI_VERSION"

# Sleep 1 second to ensure binary mtime differs from baseline
sleep 1

# ── 4. Upgrade: install into a fresh target (shared boi_src triggers rebuild) ─
echo "--- 4. upgrade install (BOI $NEW_BOI_VERSION) ---"
if bash install.sh "$HEAD_TARGET" > /tmp/install-new.log 2>&1; then
    pass "install-new: upgrade install.sh exited 0"
else
    INSTALL_EXIT=$?
    fail "install-new: install.sh exited $INSTALL_EXIT"
    tail -50 /tmp/install-new.log
    exit 1
fi

# ── 5. Post-upgrade assertions ────────────────────────────────────────────────
echo "--- 5. post-upgrade assertions ---"

# 5a. Binary still executable
if [ -x "$BOI" ]; then
    pass "post-binary-exec: $BOI still executable after upgrade"
else
    fail "post-binary-exec: $BOI not executable after upgrade"
fi

# 5b. Symlink resolves to a real file (not dangling)
if [ -L "$BOI" ]; then
    RESOLVED=$(readlink -f "$BOI" 2>/dev/null || true)
    if [ -x "$RESOLVED" ]; then
        pass "symlink-resolve: symlink -> $RESOLVED (real executable)"
    else
        fail "symlink-resolve: symlink is dangling or non-executable: $RESOLVED"
    fi
else
    pass "symlink-resolve: $BOI is a regular (non-symlink) executable"
fi

# 5c. boi --version reflects new BOI_VERSION from VERSIONS
EXPECTED_VER="$NEW_BOI_VERSION"
EXPECTED_BARE="${EXPECTED_VER#v}"
NEW_VER=$("$BOI" --version 2>&1 || echo "unknown")
if echo "$NEW_VER" | grep -qF "$EXPECTED_BARE"; then
    pass "version-match: '$NEW_VER' contains '$EXPECTED_BARE' (VERSIONS $EXPECTED_VER)"
else
    fail "version-match: '$NEW_VER' does not contain '$EXPECTED_BARE' — stale binary?"
fi

# 5d. Version changed from baseline (guards against no-op upgrade)
if [ "$NEW_VER" != "$BASELINE_VER" ]; then
    pass "version-changed: bumped from '$BASELINE_VER' to '$NEW_VER'"
else
    fail "version-changed: version unchanged after upgrade ('$NEW_VER') — stale binary not rebuilt"
fi

# 5e. boi --help lists the v3.0.0 subcommands. A stale (un-rebuilt) binary
# would still be v2.0.0 and expose `status`/`bench` instead — so the presence
# of the v3-only surface proves the rebuild happened.
HELP_OUTPUT=$("$BOI" --help 2>&1)
for sub in daemon dispatch dashboard cancel; do
    if echo "$HELP_OUTPUT" | grep -q "$sub"; then
        pass "subcmd-present: '$sub' in --help after upgrade"
    else
        fail "subcmd-present: '$sub' NOT in --help after upgrade — stale binary?"
    fi
done

# 5f. Help line count grew or stayed same (detects shrinking = regressed binary)
NEW_HELP_LINES=$("$BOI" --help 2>&1 | wc -l | tr -d ' ')
if [ "$NEW_HELP_LINES" -ge "$BASELINE_HELP_LINES" ]; then
    pass "help-lines: help grew/stable ($BASELINE_HELP_LINES → $NEW_HELP_LINES lines)"
else
    fail "help-lines: help shrank ($BASELINE_HELP_LINES → $NEW_HELP_LINES lines) — possible regression"
fi

# 5g. Binary mtime is newer than pre-upgrade (proves binary was rebuilt, not reused)
BINARY_MTIME_AFTER=$(stat -c %Y "$BOI" 2>/dev/null || echo "0")
if [ "$BINARY_MTIME_AFTER" -gt "$BINARY_MTIME_BEFORE" ] 2>/dev/null; then
    pass "binary-rebuilt: mtime updated (before=$BINARY_MTIME_BEFORE after=$BINARY_MTIME_AFTER)"
else
    fail "binary-rebuilt: mtime NOT updated (before=$BINARY_MTIME_BEFORE after=$BINARY_MTIME_AFTER) — binary may be stale symlink"
fi

# ── 6a. Smoke dispatch after upgrade (only when ANTHROPIC_API_KEY is set) ─────
if [ -z "${ANTHROPIC_API_KEY:-}" ]; then
    echo "  (ANTHROPIC_API_KEY not set — skipping smoke dispatch)"
else
    echo "--- 6a. smoke dispatch after upgrade ---"
    # boi-v2 `daemon` blocks in the foreground; start it in the background and
    # wait for the control socket.
    BOI_SOCK="$HOME/.boi/v2/daemon.sock"
    BOI_DB="$HOME/.boi/v2/boi.db"
    "$BOI" daemon > /tmp/daemon-start.log 2>&1 &
    DAEMON_PID=$!

    daemon_ready=0
    for _i in $(seq 1 20); do
        if [ -S "$BOI_SOCK" ]; then
            daemon_ready=1; break
        fi
        sleep 0.5
    done

    if [ "$daemon_ready" -eq 1 ]; then
        pass "smoke-daemon: BOI daemon socket ready after upgrade"
    else
        fail "smoke-daemon: daemon socket not ready within 10s after upgrade"
        cat /tmp/daemon-start.log || true
    fi

    if [ "$daemon_ready" -eq 1 ]; then
        SMOKE_MARKER="/tmp/boi-upgrade-smoke-$$"
        SMOKE_SPEC="/tmp/boi-upgrade-spec-$$.toml"
        cat > "$SMOKE_SPEC" << SMOKESPEC
title = "BOI upgrade smoke test"

[contract]
scope = "Create the file ${SMOKE_MARKER} with content: boi-upgrade-smoke-ok"
base_branch = "main"
workspace = "/tmp/hex"

[[tasks]]
ref = "create-marker"
behavior = "Create the file ${SMOKE_MARKER} containing the text boi-upgrade-smoke-ok"
verifications = [
  { name = "marker-exists", command = "test -f ${SMOKE_MARKER}" },
]
SMOKESPEC
        pass "smoke-spec: written"

        DISPATCH_OUT=$("$BOI" dispatch "$SMOKE_SPEC" 2>&1)
        DISPATCH_EXIT=$?
        SPEC_ID=$(echo "$DISPATCH_OUT" | grep -oE 'S[0-9a-hjkmnp-tv-z]{8}' | head -1)
        if [ "$DISPATCH_EXIT" -eq 0 ] && [ -n "$SPEC_ID" ]; then
            pass "smoke-dispatch: spec enqueued (id: $SPEC_ID)"
        else
            fail "smoke-dispatch: boi dispatch failed (exit $DISPATCH_EXIT, output: $DISPATCH_OUT)"
        fi

        if [ "$DISPATCH_EXIT" -eq 0 ] && [ -n "$SPEC_ID" ]; then
            POLL_START=$(date +%s)
            TERMINAL=""
            while true; do
                ELAPSED=$(( $(date +%s) - POLL_START ))
                if [ "$ELAPSED" -ge 120 ]; then
                    fail "smoke-poll: timed out after 120s"
                    sqlite3 "$BOI_DB" "SELECT spec_id,status FROM spec_runtime WHERE spec_id='$SPEC_ID';" 2>&1 || true
                    break
                fi
                SPEC_STATUS=$(sqlite3 "$BOI_DB" \
                    "SELECT status FROM spec_runtime WHERE spec_id='$SPEC_ID';" 2>/dev/null || echo "")
                case "$SPEC_STATUS" in
                    completed)       TERMINAL="completed"; break ;;
                    failed|canceled) TERMINAL="$SPEC_STATUS"; break ;;
                esac
                sleep 3
            done

            if [ "$TERMINAL" = "completed" ]; then
                pass "smoke-complete: spec reached 'completed'"
            else
                fail "smoke-complete: spec reached '$TERMINAL' (expected 'completed')"
            fi

            if [ -f "$SMOKE_MARKER" ]; then
                pass "smoke-output: marker file exists"
            else
                fail "smoke-output: marker file not found at $SMOKE_MARKER"
            fi
        fi
    fi
    # Stop the background daemon before the doctor case (which corrupts the binary).
    kill "${DAEMON_PID:-0}" 2>/dev/null || true
fi

# ── 6b. BAD case: corrupt symlink → doctor must catch it ─────────────────────
echo "--- 6b. bad case: corrupt symlink + doctor check ---"

# Save resolved target so we can restore it
GOOD_TARGET=$(readlink -f "$BOI" 2>/dev/null || echo "")

# Corrupt the symlink to a nonexistent path
ln -sf /tmp/nonexistent-boi-binary-corrupt "$BOI"

if [ -L "$BOI" ] && [ ! -f "$BOI" ]; then
    pass "corrupt-symlink: $BOI is now a dangling symlink"
else
    fail "corrupt-symlink: expected dangling symlink; got something else"
fi

# Run the deployed doctor (`hex doctor run`). Its boi-health check resolves
# ~/.boi/bin/boi via is_file() (follows symlinks), so a dangling symlink is
# reported as "[WARN] boi-health: ~/.boi/bin/boi not found" and the run exits
# non-zero when any check warns/errors.
HEX_BIN="$HEAD_TARGET/.hex/bin/hex"
if [ -x "$HEX_BIN" ]; then
    pass "doctor-binary: hex installed at $HEX_BIN"
else
    fail "doctor-binary: hex NOT installed at $HEX_BIN (install.sh failed to place the binary)"
fi
DOCTOR_OUT=$(HEX_DIR="$HEAD_TARGET" "$HEX_BIN" doctor run 2>&1) || DOCTOR_RC=$?
DOCTOR_RC=${DOCTOR_RC:-0}

if echo "$DOCTOR_OUT" | grep -qi "boi-health\|\.boi/bin/boi\|dangling\|boi.*not found\|boi.*error\|boi.*symlink"; then
    pass "doctor-catch: doctor surfaced the broken boi binary (exit $DOCTOR_RC)"
else
    fail "doctor-catch: doctor did NOT surface broken boi binary"
    echo "  BOI-related doctor output:"
    echo "$DOCTOR_OUT" | grep -i boi | head -10 || echo "  (no BOI lines in doctor output)"
    echo "  Last 15 lines:"
    echo "$DOCTOR_OUT" | tail -15
fi

# doctor must reflect the problem in its exit code
if [ "$DOCTOR_RC" -ne 0 ]; then
    pass "doctor-exit: doctor exited $DOCTOR_RC (non-zero on broken boi, as expected)"
else
    fail "doctor-exit: doctor exited 0 despite broken boi — not reflected in exit code"
fi

# Restore clean symlink so on_exit cleanup is tidy
if [ -n "$GOOD_TARGET" ] && [ -f "$GOOD_TARGET" ]; then
    ln -sf "$GOOD_TARGET" "$BOI"
fi
INNER_EOF

# ── Run the container ─────────────────────────────────────────────────────────
BOI_SRC="$HOME/github.com/mrap/boi"
CONTAINER_LOG="/tmp/boi-upgrade-e2e-$$.log"

DOCKER_ARGS=(
    "--rm"
    "-v" "${REPO_ROOT}:/repo:ro"
    "-e" "HOME=/home/testuser"
)
[ -n "${ANTHROPIC_API_KEY:-}" ] && DOCKER_ARGS+=("-e" "ANTHROPIC_API_KEY=${ANTHROPIC_API_KEY}")
[ -d "${BOI_SRC}/.git" ]       && DOCKER_ARGS+=("-v" "${BOI_SRC}:/boi:ro")

echo "  Launching fresh container for upgrade test..."
docker run "${DOCKER_ARGS[@]}" "$IMAGE_TAG" bash -c "$INNER_SCRIPT" \
    > "$CONTAINER_LOG" 2>&1
CONTAINER_EXIT=$?

# Always display container output (shows PASS:/FAIL: lines)
cat "$CONTAINER_LOG"
echo ""

# ── Merge inner counters into outer helpers.sh PASS/FAIL ─────────────────────
INNER_PASS=$(grep -c "^  PASS:" "$CONTAINER_LOG" 2>/dev/null || true)
INNER_FAIL=$(grep -c "^  FAIL:" "$CONTAINER_LOG" 2>/dev/null || true)
INNER_PASS=${INNER_PASS:-0}
INNER_FAIL=${INNER_FAIL:-0}

PASS=$((PASS + INNER_PASS))
FAIL=$((FAIL + INNER_FAIL))

if [ "$CONTAINER_EXIT" -eq 0 ] && [ "$INNER_FAIL" -eq 0 ]; then
    assert_pass "boi-upgrade-e2e: containerized upgrade passed ($INNER_PASS assertions)"
else
    assert_fail "boi-upgrade-e2e: $INNER_FAIL of $((INNER_PASS + INNER_FAIL)) assertions failed (container exit: $CONTAINER_EXIT)"
    echo "  [hint] Force image rebuild: docker rmi $IMAGE_TAG"
    echo "  [hint] Smoke dispatch:      ANTHROPIC_API_KEY=<key> bash $0"
fi

rm -f "$CONTAINER_LOG"
