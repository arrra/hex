#!/usr/bin/env bash
# hex-emit.sh — fire-and-forget shell wrapper for the Python telemetry emitter
# Usage: hex-emit.sh <event_type> <json_payload> [source]
set -euo pipefail

export HEX_ROOT="${HEX_ROOT:-$HEX_DIR}"
TELEMETRY_DIR="${HEX_ROOT}/.hex/telemetry"

EVENT_TYPE="${1:-}"
PAYLOAD="${2:-'{}'}"
SOURCE="${3:-${HEX_TELEMETRY_SOURCE:-shell}}"

if [[ -z "${EVENT_TYPE}" ]]; then
    echo "[hex-emit] ERROR: event_type required" >&2
    exit 1
fi

# Pass values via sys.argv so no shell interpolation inside Python code
python3 - "$EVENT_TYPE" "$PAYLOAD" "$SOURCE" <<'PYEOF'
import sys
import os

telemetry_dir = os.path.join(os.path.dirname(os.path.abspath(sys.argv[0])), '..', '.hex', 'telemetry')

# Resolve the telemetry dir from the env var set by the shell
import importlib.util, pathlib

# Find emit.py
hex_root = os.environ.get('HEX_ROOT', '$HEX_DIR')
emit_path = os.path.join(hex_root, '.hex', 'telemetry')
sys.path.insert(0, emit_path)

from emit import emit

event_type = sys.argv[1]
payload_str = sys.argv[2]
source = sys.argv[3]

import json
try:
    payload = json.loads(payload_str)
except json.JSONDecodeError:
    payload = {"raw": payload_str}

emit(event_type, payload, source=source)
PYEOF
