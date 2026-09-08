#!/usr/bin/env python3
"""PostToolUseFailure hook: append one incident record per tool failure.

Reads the PostToolUseFailure stdin JSON payload once and appends a single
line to ${HEX_LEDGER_DIR:-$HOME/.hex/ledger}/incidents.jsonl. Never writes to
stdout, never exits non-zero (fail-open: a bug here must never block a tool
or surface as a hook failure to Claude Code). Any error -> exactly one stderr
line, exit 0.
"""
import hashlib
import json
import os
import sys
from datetime import datetime, timezone
from pathlib import Path

ERROR_MAX = 2000
PREVIEW_MAX = 200


def ledger_dir() -> Path:
    raw = os.environ.get("HEX_LEDGER_DIR")
    if raw:
        return Path(raw)
    return Path.home() / ".hex" / "ledger"


def build_record(payload: dict) -> dict:
    tool_input = payload.get("tool_input", {})
    canonical = json.dumps(tool_input, sort_keys=True)
    args_hash = hashlib.sha256(canonical.encode()).hexdigest()[:12]
    error = str(payload.get("error", ""))[:ERROR_MAX]
    return {
        "ts": datetime.now(timezone.utc).isoformat(),
        "session_id": payload.get("session_id"),
        "tool": payload.get("tool_name"),
        "args_hash": args_hash,
        "args_preview": canonical[:PREVIEW_MAX],
        "error": error,
        "cwd": payload.get("cwd"),
        "tool_use_id": payload.get("tool_use_id"),
        "duration_ms": payload.get("duration_ms"),
        "is_interrupt": payload.get("is_interrupt"),
    }


def main() -> int:
    try:
        raw = sys.stdin.buffer.read()
        if not raw.strip():
            raise ValueError("empty stdin")
        payload = json.loads(raw)
        if not isinstance(payload, dict):
            raise ValueError("stdin payload is not a JSON object")

        record = build_record(payload)

        out_dir = ledger_dir()
        out_dir.mkdir(parents=True, exist_ok=True)
        with open(out_dir / "incidents.jsonl", "a") as f:
            f.write(json.dumps(record) + "\n")
        return 0
    except Exception as exc:  # fail-open: never block, never crash loudly to stdout
        sys.stderr.write(f"[incident] error: {exc}\n")
        return 0


if __name__ == "__main__":
    sys.exit(main())
