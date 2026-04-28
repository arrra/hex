#!/usr/bin/env bash
# Auto-deploy cc-connect when fork/main diverges from the running binary.
# Called by hex-events on timer.tick.5m. Designed to be idempotent and non-fatal.
set -uo pipefail

REPO="/Users/sagarsingh/github.com/mrap/cc-connect"
BINARY="/opt/homebrew/bin/cc-connect"
SERVICE="com.cc-connect.service"
SOCKET="$HOME/.cc-connect/data/run/api.sock"
LOG_DIR="$HOME/.hex-events/logs"
LOG_FILE="$LOG_DIR/cc-connect-deploy.log"
OPS_LOG="$HOME/.boi/ops-actions.log"

mkdir -p "$LOG_DIR" "$(dirname "$OPS_LOG")"

_log() {
  local step="$1" msg="$2"
  local ts; ts="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
  printf '[%s] [%s] %s\n' "$ts" "$step" "$msg" >> "$LOG_FILE"
}

_fail() {
  local step="$1" detail="$2"
  _log "$step" "FAIL: $detail"
  exit 1
}

_ops_log() {
  local old_sha="$1" new_sha="$2" status="$3"
  local ts; ts="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
  printf '[%s] cc-connect-auto-deploy old=%s new=%s status=%s\n' \
    "$ts" "$old_sha" "$new_sha" "$status" >> "$OPS_LOG"
}

# Step 1: verify repo
if ! git -C "$REPO" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  exit 0
fi

# Step 2: find the remote that tracks arrra/cc-connect
REMOTE=""
while IFS= read -r line; do
  if [[ "$line" =~ arrra/cc-connect && "$line" =~ \(fetch\) ]]; then
    REMOTE="$(echo "$line" | awk '{print $1}')"
    break
  fi
done < <(git -C "$REPO" remote -v 2>/dev/null)

if [[ -z "$REMOTE" ]]; then
  _log "fetch" "no remote tracking arrra/cc-connect found"
  exit 0
fi

if ! git -C "$REPO" fetch "$REMOTE" main 2>&1 | tail -1 >> "$LOG_FILE"; then
  _fail "fetch" "git fetch $REMOTE main failed (see log)"
fi

# Step 3: compute SHAs
REMOTE_SHA="$(git -C "$REPO" rev-parse --short=7 "$REMOTE/main" 2>/dev/null)" || {
  _fail "sha-remote" "could not resolve $REMOTE/main"
}

# Binary version line: "cc-connect v1.2.3-N-gABCDEFG" or "commit:  ABCDEFG"
BINARY_SHA=""
if [[ -x "$BINARY" ]]; then
  VERSION_OUTPUT="$("$BINARY" --version 2>/dev/null || true)"
  # Prefer "commit: <sha>" line if present
  COMMIT_LINE="$(printf '%s\n' "$VERSION_OUTPUT" | grep '^commit:' | head -1)"
  if [[ -n "$COMMIT_LINE" ]]; then
    BINARY_SHA="$(printf '%s\n' "$COMMIT_LINE" | awk '{print $2}' | tr -d '[:space:]')"
  else
    # Fall back to trailing -g<sha> in version string
    BINARY_SHA="$(printf '%s\n' "$VERSION_OUTPUT" | head -1 | grep -oE 'g[0-9a-f]{7,}$' | sed 's/^g//')"
  fi
fi

if [[ -z "$BINARY_SHA" ]]; then
  _log "sha-binary" "could not determine running binary SHA; proceeding with deploy"
  BINARY_SHA="unknown"
fi

# Step 4: up to date — nothing to do
if [[ "$BINARY_SHA" != "unknown" && "$BINARY_SHA" == "$REMOTE_SHA" ]]; then
  exit 0
fi

_log "deploy" "diverged: binary=$BINARY_SHA remote=$REMOTE_SHA — starting build"

# Step 5: pull and build
if ! git -C "$REPO" checkout main 2>&1 | tail -1 >> "$LOG_FILE"; then
  _fail "checkout" "git checkout main failed"
fi

if ! git -C "$REPO" pull --ff-only "$REMOTE" main 2>&1 | tail -1 >> "$LOG_FILE"; then
  _fail "pull" "git pull --ff-only failed"
fi

if ! (cd "$REPO" && make build AGENTS=claudecode PLATFORMS_INCLUDE=slack) >> "$LOG_FILE" 2>&1; then
  _ops_log "$BINARY_SHA" "$REMOTE_SHA" "build-failed"
  _fail "build" "make build failed"
fi

# Step 6: atomic binary swap
NEW_BIN="$REPO/cc-connect"
if [[ ! -x "$NEW_BIN" ]]; then
  _fail "swap" "built binary not found at $NEW_BIN"
fi

cp "$NEW_BIN" /tmp/cc-connect.new || _fail "swap" "cp to /tmp failed"
mv /tmp/cc-connect.new "$BINARY"   || _fail "swap" "mv to $BINARY failed"

_log "swap" "binary swapped: $BINARY_SHA -> $REMOTE_SHA"

# Step 7: reload launchd (kickstart -k kills + restarts without requiring the plist path)
UID_VAL="$(id -u)"
if ! launchctl kickstart -k "gui/$UID_VAL/$SERVICE" >> "$LOG_FILE" 2>&1; then
  _fail "launchd" "launchctl kickstart failed"
fi

# Step 8: wait ≤10s for socket to reappear
WAITED=0
while [[ ! -S "$SOCKET" && $WAITED -lt 10 ]]; do
  sleep 1
  WAITED=$((WAITED + 1))
done

if [[ ! -S "$SOCKET" ]]; then
  _log "socket" "socket did not reappear after ${WAITED}s — daemon may still be starting"
fi

_ops_log "$BINARY_SHA" "$REMOTE_SHA" "deployed"
_log "deploy" "done: $BINARY_SHA -> $REMOTE_SHA"
exit 0
