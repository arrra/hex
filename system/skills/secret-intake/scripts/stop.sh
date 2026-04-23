#!/usr/bin/env bash
set -euo pipefail

PIDFILE="/tmp/hex-secret-intake.pid"

if [ -f "$PIDFILE" ]; then
  PID=$(cat "$PIDFILE")
  if kill -0 "$PID" 2>/dev/null; then
    kill "$PID"
    echo "Stopped (pid $PID)"
  else
    echo "Stale pidfile (pid $PID not running)"
  fi
  rm -f "$PIDFILE"
else
  pkill -f "secret-intake/scripts/server.py" 2>/dev/null && echo "Stopped" || echo "Not running"
fi
