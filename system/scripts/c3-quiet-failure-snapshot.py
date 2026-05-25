#!/usr/bin/env python3
"""C3 / M2 — weekly quiet-failure rollup snapshot.

Reads the ``v_c3_quiet_failure_weekly`` VIEW from
``$HEX_DIR/.hex/telemetry/events.db`` and emits one
``hex.c3.quiet_failure.weekly_count`` event per category for the last
completed ISO week.

Monday-guard:
  This script is fired daily by ``c3-quiet-failure-weekly-snapshot.yaml``
  but only does work on Monday UTC. On other days it exits 0 cleanly
  without emitting anything.

Override / testing:
  --force            Bypass the Monday-guard and emit unconditionally.
  C3_TODAY=YYYY-MM-DD
                     Pin the script's notion of "today" (UTC). Used by
                     the test suite to exercise the non-Monday branch.

Loud failure (S6):
  If ANY ``hex-emit.sh`` invocation returns a non-zero exit code, the
  script logs to stderr and exits with status 2. Silent swallowing is
  a bug; downstream policies depend on this signal.
"""

from __future__ import annotations

import argparse
import json
import os
import sqlite3
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

EVENT_TYPE = "hex.c3.quiet_failure.weekly_count"
SOURCE = "c3-quiet-failure-snapshot"
EMIT_TIMEOUT_SECONDS = 10


def _resolve_hex_root() -> Path:
    """Return the hex root directory (parent of ``.hex/``)."""
    root = os.environ.get("HEX_ROOT") or os.environ.get("HEX_DIR")
    if not root:
        print(
            "[c3-quiet-failure-snapshot] ERROR: neither HEX_ROOT nor HEX_DIR set",
            file=sys.stderr,
        )
        sys.exit(2)
    return Path(root)


def _resolve_today_utc() -> datetime:
    """Resolve 'today' (UTC). Honour ``C3_TODAY`` env override for tests."""
    pinned = os.environ.get("C3_TODAY")
    if pinned:
        try:
            return datetime.strptime(pinned, "%Y-%m-%d").replace(tzinfo=timezone.utc)
        except ValueError:
            print(
                f"[c3-quiet-failure-snapshot] ERROR: bad C3_TODAY={pinned!r}; "
                "expected YYYY-MM-DD",
                file=sys.stderr,
            )
            sys.exit(2)
    return datetime.now(tz=timezone.utc)


def _is_monday_utc(today: datetime) -> bool:
    # Python datetime.weekday(): Monday=0 .. Sunday=6.
    return today.weekday() == 0


def _read_view_rows(db_path: Path) -> list[dict]:
    """Read every row from ``v_c3_quiet_failure_weekly``.

    Returns a list of dicts (one per category). Schema-agnostic — selects
    every column the VIEW exposes and converts each row to a dict so the
    collector keeps working whether the migration ships ``count`` /
    ``week`` columns (current production schema) or ``week_count`` /
    ``iso_week`` (legacy / test-fixture schema). The ``category`` column
    is the only invariant the emitter relies on.

    Raises if the DB or VIEW is missing — that is a loud failure, not a
    quiet one.
    """
    if not db_path.exists():
        raise FileNotFoundError(f"events.db missing at {db_path}")
    con = sqlite3.connect(str(db_path))
    try:
        con.row_factory = sqlite3.Row
        cur = con.cursor()
        cur.execute("SELECT * FROM v_c3_quiet_failure_weekly")
        rows = [dict(r) for r in cur.fetchall()]
        if rows and "category" not in rows[0]:
            raise sqlite3.Error(
                "v_c3_quiet_failure_weekly is missing required 'category' column; "
                f"actual columns={list(rows[0].keys())!r}"
            )
        return rows
    finally:
        con.close()


def _emit_one(emit_script: Path, payload: dict, hex_root: Path) -> int:
    """Invoke hex-emit.sh once for ``payload``; return its exit code.

    The deployed hex-emit.sh defaults ``HEX_ROOT`` from ``HEX_DIR`` but
    does NOT export it before launching its inner python3, so we set
    BOTH explicitly here to keep the inner emitter able to locate
    ``.hex/telemetry/emit.py`` regardless of how the caller invoked us.
    """
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
            f"[c3-quiet-failure-snapshot] ERROR: hex-emit.sh invocation raised "
            f"{type(exc).__name__}: {exc}",
            file=sys.stderr,
        )
        return 1
    if completed.returncode != 0:
        # Surface stderr from the failing emit so downstream debugging
        # has the actual reason, not just an opaque exit code.
        print(
            f"[c3-quiet-failure-snapshot] hex-emit.sh exited "
            f"{completed.returncode} for category={payload.get('category')!r}; "
            f"stderr={completed.stderr.strip()!r}",
            file=sys.stderr,
        )
    return completed.returncode


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="C3/M2 weekly quiet-failure rollup snapshot."
    )
    parser.add_argument(
        "--force",
        action="store_true",
        help="Bypass the Monday-guard and emit immediately.",
    )
    args = parser.parse_args(argv)

    hex_root = _resolve_hex_root()
    today_utc = _resolve_today_utc()

    if not args.force and not _is_monday_utc(today_utc):
        print(
            f"[c3-quiet-failure-snapshot] non-Monday "
            f"({today_utc.strftime('%Y-%m-%d')}, weekday={today_utc.weekday()}); "
            "no-op (use --force to override).",
            file=sys.stderr,
        )
        return 0

    db_path = hex_root / ".hex" / "telemetry" / "events.db"
    emit_script = hex_root / ".hex" / "bin" / "hex-emit.sh"

    if not emit_script.exists():
        print(
            f"[c3-quiet-failure-snapshot] ERROR: hex-emit.sh not found at "
            f"{emit_script}",
            file=sys.stderr,
        )
        return 2

    try:
        rows = _read_view_rows(db_path)
    except (FileNotFoundError, sqlite3.Error) as exc:
        print(
            f"[c3-quiet-failure-snapshot] ERROR: cannot read "
            f"v_c3_quiet_failure_weekly: {exc}",
            file=sys.stderr,
        )
        return 2

    if not rows:
        # No rows is not a failure; record forward-only state in stderr
        # so the operator can see the script ran but had nothing to emit.
        print(
            "[c3-quiet-failure-snapshot] v_c3_quiet_failure_weekly returned "
            "zero rows; nothing to emit.",
            file=sys.stderr,
        )
        return 0

    snapshot_date = today_utc.strftime("%Y-%m-%d")
    failures = 0
    for row in rows:
        # Pass every VIEW column through to the payload so this collector
        # is robust against schema evolution (e.g. count vs week_count,
        # week vs iso_week). The 'category' key is the only invariant.
        payload = dict(row)
        payload["snapshot_date_utc"] = snapshot_date
        payload["forced"] = bool(args.force)
        rc = _emit_one(emit_script, payload, hex_root)
        if rc != 0:
            failures += 1

    if failures:
        print(
            f"[c3-quiet-failure-snapshot] ERROR: {failures}/{len(rows)} emits "
            "failed; exiting 2 LOUDLY (S6: no quiet failures).",
            file=sys.stderr,
        )
        return 2

    return 0


if __name__ == "__main__":
    sys.exit(main())
