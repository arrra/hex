#!/usr/bin/env python3
"""C3 mirror-sink producer — shared helper module (Task T6_0).

This file lands the *shared producer-helper code* required by the AMENDED
mirror-sink contract:

    docs/superpowers/specs/2026-05-24-iii-hex-mirror-sink-contract.md

Task T6_0 (this file) lands ONLY the helpers — Task Tr92rrn58 / §Task 6 fills
in ``main()`` (the actual JSONL mirror loop, watermark advance, redaction,
schema-drift halt, day-rollover, etc.).

Helpers exposed for the rest of the producer and for unit tests
(``tests/c3/test_mirror_sink_helpers.py``):

    * ``EXPECTED_ACTION_LOG_COLUMNS`` — the documented 9-column action_log
      schema (V0.29) as ``[(name, type), ...]`` in declaration order.
    * ``assert_action_log_schema(con)`` — runs ``PRAGMA table_info(action_log)``
      against the connection and exits 1 LOUDLY (S6) on any drift. Returns
      ``None`` on match.
    * ``classify_error(status, error_message, returncode)`` — maps an
      action-log row's failure information to the controlled error_class
      vocabulary {io_error, timeout, command_not_found, non_zero_exit,
      unknown}; returns ``None`` on success.
    * ``read_last_line_mirror_id(path)`` — tails the latest JSONL line and
      returns its ``mirror_id`` (used for post-crash resume per §4.4); returns
      ``None`` if the file is missing or empty.
    * ``read_watermark(con)`` — returns the highest already-processed
      mirror_id. Cold-start initialiser is ``MAX(action_log.id)`` — explicit
      forward-only stderr log so operators know historical rows are
      intentionally NOT backfilled (B4).

The full ``main()`` producer loop lives in Task Tr92rrn58; this file's
``__main__`` block is intentionally a no-op stub so importing the module never
has side effects and ``python3 c3-mirror-sink.py`` exits cleanly with a
breadcrumb pointing operators at the helper module status.
"""

from __future__ import annotations

import json
import os
import sqlite3
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Iterable, List, Optional, Tuple


# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

# Mirror file directory env var — tests override this with monkeypatch.setenv.
HEX_C3_MIRROR_DIR_ENV = "HEX_C3_MIRROR_DIR"
DEFAULT_MIRROR_DIR = Path.home() / ".hex-events" / "mirror"

# Documented 9-column action_log schema, in declaration order.
# Mirrors V0.29 of the amended mirror-sink contract. Any drift (added,
# removed, renamed, or retyped column) MUST halt the producer (S6).
EXPECTED_ACTION_LOG_COLUMNS: List[Tuple[str, str]] = [
    ("id", "INTEGER"),
    ("event_id", "INTEGER"),
    ("recipe", "TEXT"),
    ("action_type", "TEXT"),
    ("action_detail", "TEXT"),
    ("status", "TEXT"),
    ("error_message", "TEXT"),
    ("executed_at", "TEXT"),
    ("action_result", "TEXT"),
]

# Controlled error_class vocabulary — see amended contract §3.1 outcome.error_class.
# Historical rows written before this producer existed carry null; new failures
# MUST classify into one of these labels (never an ad-hoc string).
ERROR_CLASS_IO_ERROR = "io_error"
ERROR_CLASS_TIMEOUT = "timeout"
ERROR_CLASS_COMMAND_NOT_FOUND = "command_not_found"
ERROR_CLASS_NON_ZERO_EXIT = "non_zero_exit"
ERROR_CLASS_UNKNOWN = "unknown"

VALID_ERROR_CLASSES = frozenset({
    ERROR_CLASS_IO_ERROR,
    ERROR_CLASS_TIMEOUT,
    ERROR_CLASS_COMMAND_NOT_FOUND,
    ERROR_CLASS_NON_ZERO_EXIT,
    ERROR_CLASS_UNKNOWN,
})


# ---------------------------------------------------------------------------
# Helper: schema-drift assertion (S6 loud-fail)
# ---------------------------------------------------------------------------

def assert_action_log_schema(con: sqlite3.Connection) -> None:
    """Halt the producer LOUDLY if action_log drifts from V0.29.

    Reads ``PRAGMA table_info(action_log)`` and compares the resulting
    ``[(name, type), ...]`` list (in declaration order) to
    ``EXPECTED_ACTION_LOG_COLUMNS``. On any mismatch:

        1. Print a single-line ``[c3-mirror-sink][SCHEMA DRIFT]`` message to
           stderr with both expected and observed shapes (S6 — no quiet
           failures).
        2. ``sys.exit(1)`` so launchctl / the policy runner sees a non-zero
           exit and surfaces ``hex.policy.c3-mirror-sink.failed``.

    On match, returns ``None`` so ``main()`` can continue.
    """
    cur = con.cursor()
    cur.execute("PRAGMA table_info(action_log)")
    actual: List[Tuple[str, str]] = [(row[1], row[2]) for row in cur.fetchall()]
    if actual != EXPECTED_ACTION_LOG_COLUMNS:
        print(
            "[c3-mirror-sink][SCHEMA DRIFT] action_log schema drift detected — "
            f"expected={EXPECTED_ACTION_LOG_COLUMNS!r} actual={actual!r}. "
            "Halting producer per S6; mirror sink will resume after the schema "
            "is reconciled and EXPECTED_ACTION_LOG_COLUMNS is updated.",
            file=sys.stderr,
        )
        sys.exit(1)
    return None


# ---------------------------------------------------------------------------
# Helper: error classification (controlled vocabulary)
# ---------------------------------------------------------------------------

def classify_error(
    status: Optional[str],
    error_message: Optional[str],
    returncode: Optional[int],
) -> Optional[str]:
    """Map an action_log row's failure shape onto the controlled vocabulary.

    Returns ``None`` when ``status == 'ok'`` (success — outcome.error_class
    is null in the JSONL line). On failure, returns exactly one of:

        * ``"command_not_found"`` — shell could not find the executable
          (returncode 127 or message contains 'command not found').
        * ``"timeout"`` — message indicates a timeout
          (``TimeoutExpired`` or 'timed out').
        * ``"io_error"`` — message indicates an OS / IO failure
          (``BrokenPipe``, ``OSError``, ``IOError``, ``PermissionError``,
          ``[Errno`` prefix, 'broken pipe', 'no space left', etc.).
        * ``"non_zero_exit"`` — none of the above matched but ``returncode``
          is a non-zero integer.
        * ``"unknown"`` — everything else (preserve the LOUD-fail signal
          rather than swallowing).

    The function is deliberately a small ordered dispatch — adding a new
    bucket means adding a new branch above the ``unknown`` fallback.
    """
    if status == "ok":
        return None

    msg = (error_message or "").lower()
    raw_msg = error_message or ""

    # 1. command_not_found — shell exit code 127 or explicit message.
    if returncode == 127 or "command not found" in msg or "no such file or directory" in msg and "/bin/" in raw_msg:
        return ERROR_CLASS_COMMAND_NOT_FOUND

    # 2. timeout — explicit TimeoutExpired or 'timed out' substring.
    if "timeoutexpired" in msg or "timed out" in msg or "timeout" in msg and "expired" in msg:
        return ERROR_CLASS_TIMEOUT

    # 3. io_error — OS-level / IO-level failure shapes.
    io_markers = (
        "brokenpipe",
        "broken pipe",
        "oserror",
        "ioerror",
        "permissionerror",
        "filenotfounderror",
        "isadirectoryerror",
        "[errno ",
        "no space left",
        "disk full",
        "input/output error",
    )
    if any(marker in msg for marker in io_markers):
        return ERROR_CLASS_IO_ERROR

    # 4. non_zero_exit — anything else with a non-zero returncode.
    if isinstance(returncode, int) and returncode != 0:
        return ERROR_CLASS_NON_ZERO_EXIT

    # 5. unknown — loud fallback rather than silent miss.
    return ERROR_CLASS_UNKNOWN


# ---------------------------------------------------------------------------
# Helper: read last-line mirror_id (post-crash resume — §4.4)
# ---------------------------------------------------------------------------

def read_last_line_mirror_id(path: Path | str) -> Optional[int]:
    """Return the ``mirror_id`` from the LAST JSONL line in ``path``.

    Used by ``read_watermark()`` for the post-crash resume path: if the
    producer crashed AFTER writing a JSONL line but BEFORE advancing the
    watermark file, the highest already-written id lives in the file's tail.

    Returns ``None`` when:
        * ``path`` does not exist (cold start for this day).
        * ``path`` exists but contains zero parseable JSONL lines.
        * The last line has no ``mirror_id`` field (unexpected — caller can
          treat as cold start and fall back to MAX(action_log.id)).

    The implementation is intentionally tail-only — we walk the file
    backwards in modest-size chunks to find the final newline so we do NOT
    page through gigabytes of mirror history on every wake.
    """
    p = Path(path)
    if not p.is_file():
        return None

    try:
        size = p.stat().st_size
    except OSError:
        return None
    if size == 0:
        return None

    # Tail-read: walk backwards from EOF in 4 KiB chunks until we find a
    # newline boundary that yields a non-empty trailing line.
    chunk = 4096
    try:
        with p.open("rb") as fh:
            tail = b""
            pos = size
            while pos > 0:
                read_size = min(chunk, pos)
                pos -= read_size
                fh.seek(pos)
                tail = fh.read(read_size) + tail
                # Strip a trailing newline so 'rstrip().rsplit' finds the
                # actual last record, not an empty string.
                stripped = tail.rstrip(b"\n\r")
                if b"\n" in stripped:
                    last_line = stripped.rsplit(b"\n", 1)[-1]
                    break
                if pos == 0:
                    last_line = stripped
                    break
            else:  # pragma: no cover — defensive
                return None
    except OSError:
        return None

    if not last_line:
        return None

    try:
        record = json.loads(last_line.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError):
        return None

    mirror_id = record.get("mirror_id")
    if isinstance(mirror_id, int):
        return mirror_id
    return None


# ---------------------------------------------------------------------------
# Helper: read_watermark — cold-start = MAX(action_log.id)
# ---------------------------------------------------------------------------

def _mirror_dir() -> Path:
    """Return the active mirror directory honouring HEX_C3_MIRROR_DIR."""
    override = os.environ.get(HEX_C3_MIRROR_DIR_ENV)
    if override:
        return Path(override)
    return DEFAULT_MIRROR_DIR


def _today_jsonl_path(mirror_dir: Path) -> Path:
    today = datetime.now(timezone.utc).strftime("%Y-%m-%d")
    return mirror_dir / f"{today}.jsonl"


def _read_watermark_file(mirror_dir: Path) -> Optional[int]:
    """Read the watermark file (if any). Returns None on missing/corrupt."""
    wm_path = mirror_dir / ".watermark"
    if not wm_path.is_file():
        return None
    try:
        text = wm_path.read_text(encoding="utf-8").strip()
    except OSError:
        return None
    if not text:
        return None
    try:
        return int(text)
    except ValueError:
        return None


def read_watermark(con: sqlite3.Connection) -> int:
    """Return the highest mirror_id already processed.

    Resolution order (per amended contract §4):

        1. ``max(watermark_file, last_line_mirror_id_of_today_jsonl)`` if
           either source has a value — handles the post-crash case where the
           JSONL line was written but the watermark file was not yet
           advanced.
        2. **cold-start**: no watermark file AND no JSONL — initialise at
           ``MAX(action_log.id)`` (forward-only; historical rows are NOT
           backfilled per B4). The cold-start path emits a single LOUD
           stderr breadcrumb so operators can audit that the skip was
           intentional.
        3. Empty ``action_log`` table: cold-start returns 0 so the first
           inserted row (id=1) is mirrored on the next pass.
    """
    mirror_dir = _mirror_dir()
    file_wm = _read_watermark_file(mirror_dir)
    last_line_wm = read_last_line_mirror_id(_today_jsonl_path(mirror_dir))

    candidates = [v for v in (file_wm, last_line_wm) if isinstance(v, int)]
    if candidates:
        return max(candidates)

    # cold-start branch — no prior progress on disk. Skip historical rows
    # by initialising at MAX(action_log.id); forward-only per B4.
    cur = con.cursor()
    cur.execute("SELECT COALESCE(MAX(id), 0) FROM action_log")
    row = cur.fetchone()
    max_id = int(row[0]) if row and row[0] is not None else 0
    print(
        f"[c3-mirror-sink] cold-start watermark = {max_id} "
        "(MAX(action_log.id)); forward-only, historical action_log rows are "
        "intentionally NOT backfilled per amended contract §4 / B4.",
        file=sys.stderr,
    )
    return max_id


# ---------------------------------------------------------------------------
# __main__ stub
# ---------------------------------------------------------------------------
# The real producer loop is implemented by Task Tr92rrn58 (§Task 6). Until
# that lands, executing this file as a script is a no-op that just announces
# the helper module is installed. Importing the module (the unit tests do
# this) never triggers this block.

if __name__ == "__main__":  # pragma: no cover
    print(
        "c3-mirror-sink: T6_0 helper module installed. main() producer loop "
        "lands in Task Tr92rrn58 (§Task 6). No-op exit.",
        file=sys.stderr,
    )
    sys.exit(0)
