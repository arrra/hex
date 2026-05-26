#!/usr/bin/env python3
"""C3 / M3 — audit-stream completeness collector.

Globs the canonical audit JSONL files for the live wake events and computes
the per-(agent, date) completeness ratio:

    ratio = (wake-complete + wake-skip + wake-skip-llm) / wake-start

Sources (per plan §Task 3.1):

  1. ``$HEX_DIR/.hex/audit/actions.jsonl`` — the main hex audit stream.
  2. ``$HEX_DIR/<runtime>/worktrees/agent-*/.hex/audit/actions.jsonl`` —
     per-agent worktree mirrors (one file per active worktree).

Timestamps:
  Records carry either ``...Z`` or ``...+00:00`` formats. ``parse_audit_ts``
  normalises both into timezone-aware UTC datetimes so bucketing stays
  deterministic across DST / TZ boundaries.

Bucketing & status:
  Records are grouped by ``(agent, date_utc)``. For each bucket:

    * ``started > 0`` → ``status='ok'``, ``ratio = ended / started``.
    * ``started == 0`` → ``status='no_data'``, ``ratio=None`` (emitted as
      JSON ``null``). This distinguishes a quiet pipeline (legitimate
      ``no_data``) from a broken denominator (a synthetic ``1.0`` would
      hide the gap).

  ``wake-skip-llm`` MUST count in the denominator alongside ``wake-skip``
  and ``wake-complete`` (v4 fix per plan §3.1) — otherwise LLM-budget-gated
  wakes would falsely depress completeness.

Output:

  --json / --dry-run / HEX_C3_AUDIT_COMPLETENESS_DRY_RUN=1
      Write the per-bucket report as JSON to stdout, exit 0, and skip
      the ``hex-emit.sh`` invocation entirely. Used by unit tests and
      by operators sanity-checking the payload.

  (default)
      Compute the report, invoke ``hex-emit.sh hex.c3.audit.completeness``
      ONCE PER BUCKET, and exit 0 (or non-zero on emit failure;
      S6: no quiet failures).

  --days N
      Only emit / report buckets whose UTC date is within the last N days
      (default 7). ``--days 0`` reports every bucket discovered.
"""

from __future__ import annotations

import argparse
import glob
import json
import os
import subprocess
import sys
from collections import defaultdict
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any, Dict, Iterable, List, Optional


EVENT_TYPE = "hex.c3.audit.completeness"
SOURCE = "c3-audit-completeness"
EMIT_TIMEOUT_SECONDS = 10

# Action labels — denominator vs. numerator membership is fixed here so the
# v4 wake-skip-llm fix is one line, not scattered through the codebase.
WAKE_START = "wake-start"
WAKE_ENDED_ACTIONS = frozenset({"wake-complete", "wake-skip", "wake-skip-llm"})


# ---------------------------------------------------------------------------
# Pure helpers (covered by tests/c3/test_audit_completeness.py)
# ---------------------------------------------------------------------------

def parse_audit_ts(ts: str) -> datetime:
    """Parse an audit-record timestamp into a timezone-aware UTC datetime.

    Accepts both formats observed in the live audit stream:

        * ``2026-05-24T12:34:56Z`` (trailing-Z legacy emitters)
        * ``2026-05-24T12:34:56+00:00`` (current ``datetime.isoformat()`` shape)
        * ``2026-05-24T12:34:56.123456+00:00`` (with subsecond precision)

    Always returns a UTC-normalised, tz-aware ``datetime`` so the bucketing
    logic never silently miscompares across DST / TZ boundaries.

    Raises ``ValueError`` if the input is not a parseable ISO-8601 timestamp;
    the caller is responsible for deciding whether to drop the record or
    surface a loud error.
    """
    if not isinstance(ts, str):
        raise ValueError(f"audit ts must be str, got {type(ts).__name__}")

    # ``datetime.fromisoformat`` does not accept trailing ``Z`` until 3.11,
    # so normalise it explicitly. Replace only the FINAL ``Z`` to leave the
    # rest of the string intact.
    if ts.endswith("Z"):
        normalised = ts[:-1] + "+00:00"
    else:
        normalised = ts

    parsed = datetime.fromisoformat(normalised)
    if parsed.tzinfo is None:
        # Defensive: a naive datetime would silently miscompare against the
        # tz-aware buckets. Treat as UTC and surface in logs upstream.
        parsed = parsed.replace(tzinfo=timezone.utc)
    return parsed.astimezone(timezone.utc)


def summarize_completeness(records: Iterable[Dict[str, Any]]) -> List[Dict[str, Any]]:
    """Bucket ``records`` by ``(agent, date_utc)`` and compute completeness.

    Returns a list of dicts, one per ``(agent, date)`` bucket, each with:

        * ``agent``   — string agent name.
        * ``date``    — UTC date ``YYYY-MM-DD`` string.
        * ``started`` — count of ``wake-start`` records in the bucket.
        * ``ended``   — count of ``wake-complete + wake-skip + wake-skip-llm``.
        * ``status``  — ``"ok"`` when ``started > 0`` else ``"no_data"``.
        * ``ratio``   — ``ended / started`` when ``started > 0`` else ``None``.

    The list is sorted by ``(agent, date)`` for deterministic emission order.
    Records missing ``agent`` or ``ts`` or an unparseable ``ts`` are skipped
    silently — they are not part of M3's denominator. Operators relying on
    audit-row well-formedness should hook a separate validator; this
    collector's job is the completeness ratio.
    """
    buckets: Dict[tuple, Dict[str, int]] = defaultdict(
        lambda: {"started": 0, "ended": 0}
    )

    for rec in records:
        if not isinstance(rec, dict):
            continue
        agent = rec.get("agent")
        action = rec.get("action")
        ts = rec.get("ts")
        if not agent or not ts or not action:
            continue
        try:
            parsed_ts = parse_audit_ts(ts)
        except (ValueError, TypeError):
            continue
        date_str = parsed_ts.strftime("%Y-%m-%d")
        key = (agent, date_str)
        if action == WAKE_START:
            buckets[key]["started"] += 1
        elif action in WAKE_ENDED_ACTIONS:
            buckets[key]["ended"] += 1
        # Other actions (decision, wake-agent, ...) are ignored for M3.

    out: List[Dict[str, Any]] = []
    for (agent, date_str), counts in sorted(buckets.items()):
        started = counts["started"]
        ended = counts["ended"]
        if started > 0:
            status = "ok"
            ratio: Optional[float] = ended / started
        else:
            status = "no_data"
            ratio = None
        out.append({
            "agent": agent,
            "date": date_str,
            "started": started,
            "ended": ended,
            "status": status,
            "ratio": ratio,
        })
    return out


# ---------------------------------------------------------------------------
# Audit-file discovery and loading
# ---------------------------------------------------------------------------

def _resolve_hex_root() -> Path:
    root = os.environ.get("HEX_ROOT") or os.environ.get("HEX_DIR")
    if not root:
        # Default to resolved hex dir if neither var is set; the live policy exports
        # HEX_DIR so this branch mostly serves tests that don't set env.
        return Path.home() / "hex"
    return Path(root)


def discover_audit_files(hex_root: Path) -> List[Path]:
    """Return every audit JSONL file the collector should ingest."""
    candidates: List[Path] = []
    main_file = hex_root / ".hex" / "audit" / "actions.jsonl"
    if main_file.is_file():
        candidates.append(main_file)

    worktree_glob = str(
        hex_root / ".claude" / "worktrees" / "agent-*" / ".hex" / "audit" / "actions.jsonl"
    )
    for matched in sorted(glob.glob(worktree_glob)):
        p = Path(matched)
        if p.is_file():
            candidates.append(p)
    return candidates


def _iter_records(path: Path) -> Iterable[Dict[str, Any]]:
    """Yield decoded JSON records from ``path``, skipping unparseable lines."""
    try:
        with path.open("r", encoding="utf-8") as fh:
            for line in fh:
                line = line.strip()
                if not line:
                    continue
                try:
                    yield json.loads(line)
                except json.JSONDecodeError:
                    # Bad JSONL lines are ignored — the audit stream is
                    # append-only and an occasional truncated line at
                    # process death must not crash the collector.
                    continue
    except OSError as exc:
        print(
            f"[c3-audit-completeness] WARN: cannot read {path}: {exc}",
            file=sys.stderr,
        )


def load_records(files: Iterable[Path]) -> List[Dict[str, Any]]:
    out: List[Dict[str, Any]] = []
    for f in files:
        out.extend(_iter_records(f))
    return out


def filter_buckets_by_days(
    buckets: List[Dict[str, Any]], days: int, now_utc: Optional[datetime] = None
) -> List[Dict[str, Any]]:
    """Trim ``buckets`` to those whose ``date`` is within the last ``days``.

    ``days <= 0`` returns the input unchanged (no filtering).
    """
    if days <= 0:
        return list(buckets)
    today = (now_utc or datetime.now(tz=timezone.utc)).date()
    cutoff = today - timedelta(days=days - 1)
    cutoff_str = cutoff.strftime("%Y-%m-%d")
    return [b for b in buckets if b["date"] >= cutoff_str]


# ---------------------------------------------------------------------------
# Emit (one event per bucket)
# ---------------------------------------------------------------------------

def _emit_bucket(
    emit_script: Path, bucket: Dict[str, Any], hex_root: Path
) -> int:
    payload = dict(bucket)
    cmd = [
        "bash",
        str(emit_script),
        EVENT_TYPE,
        json.dumps(payload, sort_keys=True),
        SOURCE,
    ]
    env = os.environ.copy()
    env["HEX_DIR"] = str(hex_root)
    env["HEX_ROOT"] = str(hex_root)
    try:
        completed = subprocess.run(
            cmd,
            env=env,
            capture_output=True,
            text=True,
            timeout=EMIT_TIMEOUT_SECONDS,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        print(
            f"[c3-audit-completeness] ERROR: hex-emit.sh invocation raised "
            f"{type(exc).__name__}: {exc}",
            file=sys.stderr,
        )
        return 1
    if completed.returncode != 0:
        print(
            f"[c3-audit-completeness] hex-emit.sh exited "
            f"{completed.returncode} for bucket={payload!r}; "
            f"stderr={completed.stderr.strip()!r}",
            file=sys.stderr,
        )
    return completed.returncode


def _emit_no_buckets_sentinel(emit_script: Path, hex_root: Path) -> int:
    """Emit a single status=no_data event when the audit stream is empty.

    M3's verify expects at least one event per run; if the audit files are
    missing or contain no wake records in the requested window we still
    emit so downstream policies see a heartbeat.
    """
    today = datetime.now(tz=timezone.utc).strftime("%Y-%m-%d")
    return _emit_bucket(
        emit_script,
        {
            "agent": None,
            "date": today,
            "started": 0,
            "ended": 0,
            "status": "no_data",
            "ratio": None,
        },
        hex_root,
    )


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

def main(argv: Optional[List[str]] = None) -> int:
    parser = argparse.ArgumentParser(
        description="C3 / M3 audit-stream completeness collector."
    )
    parser.add_argument(
        "--days",
        type=int,
        default=7,
        help="Only emit buckets within the last N UTC days (default 7; "
             "0 disables the filter).",
    )
    parser.add_argument(
        "--json",
        action="store_true",
        help="Write the bucket report as JSON to stdout and skip emission.",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Alias for --json (operator-friendly).",
    )
    args = parser.parse_args(argv)

    dry_run = (
        args.json
        or args.dry_run
        or os.environ.get("HEX_C3_AUDIT_COMPLETENESS_DRY_RUN") == "1"
    )

    hex_root = _resolve_hex_root()
    files = discover_audit_files(hex_root)
    records = load_records(files)
    buckets = summarize_completeness(records)
    buckets = filter_buckets_by_days(buckets, args.days)

    report = {
        "event_type": EVENT_TYPE,
        "source": SOURCE,
        "hex_root": str(hex_root),
        "files_scanned": [str(p) for p in files],
        "buckets": buckets,
    }

    if dry_run:
        json.dump(report, sys.stdout, sort_keys=True, default=str)
        sys.stdout.write("\n")
        return 0

    emit_script = hex_root / ".hex" / "bin" / "hex-emit.sh"
    if not emit_script.exists():
        print(
            f"[c3-audit-completeness] ERROR: hex-emit.sh not found at "
            f"{emit_script}",
            file=sys.stderr,
        )
        return 2

    if not buckets:
        rc = _emit_no_buckets_sentinel(emit_script, hex_root)
        if rc != 0:
            print(
                "[c3-audit-completeness] ERROR: heartbeat emit failed; "
                "exiting 2 LOUDLY (S6: no quiet failures).",
                file=sys.stderr,
            )
            return 2
        return 0

    failures = 0
    for bucket in buckets:
        rc = _emit_bucket(emit_script, bucket, hex_root)
        if rc != 0:
            failures += 1

    if failures:
        print(
            f"[c3-audit-completeness] ERROR: {failures}/{len(buckets)} bucket "
            "emits failed; exiting 2 LOUDLY (S6: no quiet failures).",
            file=sys.stderr,
        )
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
