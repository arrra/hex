#!/usr/bin/env bash
set -euo pipefail

SKILL_DIR="$(cd "$(dirname "$0")/.." && pwd)"
SERVER="$SKILL_DIR/scripts/server.py"
PIDFILE="/tmp/hex-secret-intake.pid"
PORT="${PORT:-9877}"

if [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
  echo "Already running (pid $(cat "$PIDFILE")) on :$PORT"
  echo "https://mac-mini.tailbd5748.ts.net:$PORT"
  exit 0
fi

PORT=$PORT python3 "$SERVER" &
PID=$!
echo "$PID" > "$PIDFILE"

sleep 0.5
if kill -0 "$PID" 2>/dev/null; then
  echo "secret-intake running (pid $PID) on :$PORT"
  echo "https://mac-mini.tailbd5748.ts.net:$PORT"
else
  echo "ERR: server failed to start" >&2
  rm -f "$PIDFILE"
  exit 1
fi
