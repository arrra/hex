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
import re
import sqlite3
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Iterable, List, Optional, Tuple


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
# Producer loop — emit JSONL line per action_log row past the watermark
# (Task Tr92rrn58 / §Task 6 — AMENDED contract conformance)
# ---------------------------------------------------------------------------

HEX_C3_EVENTS_DB_ENV = "HEX_C3_EVENTS_DB"
DEFAULT_EVENTS_DB = Path.home() / ".hex-events" / "events.db"

# Substrings (case-insensitive) that trigger conservative key-substring
# redaction on nested dict keys. We err on the side of redacting too much
# rather than too little — S6 / Standing Order #1.
_REDACT_KEY_MARKERS = ("secret", "private")
_REDACT_PLACEHOLDER = "[REDACTED]"

# Inventory: every mirror-* policy that lives under ~/.hex-events/policies/
# is recognised. Anything else means the operator landed an out-of-band
# mirror producer we have NOT audited, and the producer halts so the new
# pipeline is reviewed (test 6.13). Matches the v0.28 inventory.
_RECOGNISED_MIRROR_POLICIES = frozenset({
    "events-telemetry-mirror.yaml",
    "boi-telemetry-mirror.yaml",
    "c3-mirror-sink.yaml",
})


class _CorruptRecordError(Exception):
    """Raised internally when action_detail or action_result JSON is corrupt.

    Caught inside the per-row loop; the row is replaced by a degraded record
    via ``_corrupt_record`` and the watermark still advances so the producer
    never wedges on a single bad row (amended contract §4.5).
    """


# --- record-building helpers ------------------------------------------------

_TS_RE_NAIVE = re.compile(r"^\d{4}-\d{2}-\d{2}[ T]\d{2}:\d{2}:\d{2}$")


def _normalize_ts(executed_at: Optional[str]) -> str:
    """Return ``executed_at`` reshaped to ``YYYY-MM-DDTHH:MM:SSZ``.

    SQLite's ``datetime('now')`` default emits ``YYYY-MM-DD HH:MM:SS`` (no
    timezone, space separator). The amended contract requires second
    precision + trailing ``Z``. We never pad microseconds, never carry a
    ``+00:00`` suffix. Anything we cannot parse falls back to the current
    UTC second — a LOUD fallback rather than a crash, since dropping a row
    over a malformed timestamp would be worse than recording it with ``now``.
    """
    if not executed_at:
        return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    raw = executed_at.strip()
    if _TS_RE_NAIVE.match(raw):
        return raw.replace(" ", "T") + "Z"
    try:
        if raw.endswith("Z"):
            dt = datetime.fromisoformat(raw[:-1] + "+00:00")
        else:
            dt = datetime.fromisoformat(raw)
    except (ValueError, AttributeError):
        print(
            f"[c3-mirror-sink] WARN: could not parse executed_at={raw!r}; "
            "falling back to current UTC second.",
            file=sys.stderr,
        )
        return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    if dt.tzinfo is None:
        dt = dt.replace(tzinfo=timezone.utc)
    else:
        dt = dt.astimezone(timezone.utc)
    return dt.strftime("%Y-%m-%dT%H:%M:%SZ")


def _redact(value: Any) -> Any:
    """Recursively redact dict values whose keys contain redact markers.

    Conservative key-substring match — any nested key whose lowercased name
    contains ``secret`` or ``private`` has its VALUE replaced by
    ``[REDACTED]``. Non-matching keys recurse normally so deep structures
    are still walked. Lists and scalars pass through unchanged.
    """
    if isinstance(value, dict):
        out = {}
        for k, v in value.items():
            key_lower = k.lower() if isinstance(k, str) else ""
            if any(marker in key_lower for marker in _REDACT_KEY_MARKERS):
                out[k] = _REDACT_PLACEHOLDER
            else:
                out[k] = _redact(v)
        return out
    if isinstance(value, list):
        return [_redact(v) for v in value]
    return value


def _safe_load_json(blob: Optional[str]) -> Any:
    """Parse a JSON blob, returning a sentinel on corrupt input.

    ``None``/empty → ``None``. Valid JSON → the parsed value. Invalid JSON →
    raises ``_CorruptRecordError`` so the caller can decide whether the row
    should be degraded (action_detail/action_result) or fall back to the raw
    string (event payload).
    """
    if blob is None:
        return None
    if isinstance(blob, (dict, list)):
        return blob
    if not isinstance(blob, str):
        return blob
    if not blob.strip():
        return None
    try:
        return json.loads(blob)
    except (json.JSONDecodeError, TypeError) as exc:
        raise _CorruptRecordError(str(exc))


def _extract_returncode(result: Any) -> Optional[int]:
    """Pull a numeric returncode out of an action_result dict (best-effort)."""
    if not isinstance(result, dict):
        return None
    rc = result.get("returncode")
    if isinstance(rc, bool):
        return None
    if isinstance(rc, int):
        return rc
    return None


def _now_ts() -> str:
    """Current UTC second, formatted ``YYYY-MM-DDTHH:MM:SSZ`` (amended §3.1)."""
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def _build_record(row: sqlite3.Row, mirror_id: int) -> dict:
    """Build a JSONL record matching the amended contract §3.1.

    ``ts`` is the WRITE TIME (when the mirror sink emits the line), at second
    precision UTC with trailing ``Z`` — the producer's ordering authority
    alongside ``mirror_id``. The row's own ``action_log.executed_at`` is
    preserved separately under ``action_executed_at`` (normalised to the same
    second-Z shape) so downstream consumers can recover the original action
    timing.

    Corrupt ``action_detail`` or ``action_result`` JSON raises
    ``_CorruptRecordError`` so ``main()`` can swap in ``_corrupt_record``
    while still advancing the watermark.
    """
    detail = _safe_load_json(row["action_detail"])
    if isinstance(detail, (dict, list)):
        detail = _redact(detail)

    try:
        result = _safe_load_json(row["action_result"])
    except _CorruptRecordError:
        result = None
        result_redacted = None
        returncode = None
    else:
        returncode = _extract_returncode(result)
        if isinstance(result, (dict, list)):
            result_redacted = _redact(result)
        else:
            result_redacted = result

    # event_type / payload come from the LEFT JOIN to events. If the events
    # row has been pruned the join columns are NULL — preserve as null.
    event_type = row["event_type"] if "event_type" in row.keys() else None
    try:
        payload = _safe_load_json(row["payload"]) if "payload" in row.keys() else None
    except _CorruptRecordError:
        # Payload corruption shouldn't crash the row; fall back to raw text.
        payload = row["payload"]
    if isinstance(payload, (dict, list)):
        payload = _redact(payload)

    return {
        "mirror_id": mirror_id,
        "ts": _now_ts(),
        "action_executed_at": _normalize_ts(row["executed_at"]),
        "event_id": row["event_id"],
        "event_type": event_type,
        "payload": payload,
        "recipe": row["recipe"],
        "action_type": row["action_type"],
        "action_detail": detail,
        "status": row["status"],
        "outcome": {
            "error_class": classify_error(
                row["status"], row["error_message"], returncode
            ),
            "error_message": row["error_message"],
            "action_result": result_redacted,
        },
    }


def _corrupt_record(row: sqlite3.Row, mirror_id: int, reason: str) -> dict:
    """Build a degraded record for rows whose JSON blobs would not parse.

    Per amended contract §4.5: never wedge on a single bad row — emit a
    placeholder, mark ``degraded=True``, set ``outcome.error_class='unknown'``
    so downstream consumers can audit, then advance the watermark.
    """
    return {
        "mirror_id": mirror_id,
        "ts": _now_ts(),
        "action_executed_at": _normalize_ts(row["executed_at"]),
        "event_id": row["event_id"],
        "event_type": row["event_type"] if "event_type" in row.keys() else None,
        "payload": None,
        "recipe": row["recipe"],
        "action_type": row["action_type"],
        "action_detail": None,
        "status": row["status"],
        "degraded": True,
        "degraded_reason": reason,
        "outcome": {
            "error_class": ERROR_CLASS_UNKNOWN,
            "error_message": row["error_message"],
            "action_result": None,
        },
    }


# --- IO helpers -------------------------------------------------------------


def _events_db_path() -> Path:
    override = os.environ.get(HEX_C3_EVENTS_DB_ENV)
    if override:
        return Path(override)
    return DEFAULT_EVENTS_DB


def _emit_failure_event(reason: str) -> None:
    """Best-effort emit ``hex.policy.c3-mirror-sink.failed`` (S6 loud-fail).

    Always logs ``reason`` to stderr; ALSO tries to invoke hex-emit.sh if
    available so downstream alerting policies fire. Swallowing exceptions
    here is intentional — failure-emission must never mask the original
    fault, but the original error message ALWAYS makes it to stderr.
    """
    print(
        f"[c3-mirror-sink][POLICY FAIL] hex.policy.c3-mirror-sink.failed: {reason}",
        file=sys.stderr,
    )
    hex_dir = os.environ.get("HEX_DIR")
    if not hex_dir:
        return
    emit_script = Path(hex_dir) / ".hex" / "bin" / "hex-emit.sh"
    if not emit_script.is_file():
        return
    try:
        subprocess.run(
            [
                "bash",
                str(emit_script),
                "hex.policy.c3-mirror-sink.failed",
                json.dumps({"reason": reason}),
                "c3-mirror-sink",
            ],
            check=False,
            timeout=5,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
    except Exception:  # pragma: no cover — best effort
        pass


def _advance_watermark(mirror_dir: Path, value: int) -> None:
    """Atomically replace the watermark file with ``value``."""
    wm_path = mirror_dir / ".watermark"
    tmp_path = mirror_dir / ".watermark.tmp"
    tmp_path.write_text(str(int(value)), encoding="utf-8")
    os.replace(tmp_path, wm_path)


def _assert_mirror_inventory() -> None:
    """Halt if an unrecognised mirror-* policy is present (test 6.13).

    Defence-in-depth: ensures we never deploy a second mirror producer that
    silently fights with this one. The check is OPT-IN — only runs when
    ``HEX_C3_MIRROR_INVENTORY_DIR`` is set OR when the daemon's standard
    policy dir exists. Missing dir is treated as 'no policies to audit'.
    """
    inv_dir_env = os.environ.get("HEX_C3_MIRROR_INVENTORY_DIR")
    if inv_dir_env:
        inv_dir = Path(inv_dir_env)
    else:
        inv_dir = Path.home() / ".hex-events" / "policies"
    if not inv_dir.is_dir():
        return
    unknown = []
    for path in sorted(inv_dir.glob("*mirror*.yaml")):
        if path.name not in _RECOGNISED_MIRROR_POLICIES:
            unknown.append(path.name)
    if unknown:
        msg = (
            "unrecognised mirror policy in "
            f"{inv_dir}: {sorted(unknown)!r}. Audit and either delete the "
            "extra producer or add it to _RECOGNISED_MIRROR_POLICIES."
        )
        print(f"[c3-mirror-sink][INVENTORY] {msg}", file=sys.stderr)
        _emit_failure_event(msg)
        sys.exit(1)


# --- main() -----------------------------------------------------------------


def main() -> int:
    """Mirror new ``action_log`` rows past the watermark into JSONL.

    Returns 0 on success (including the no-new-rows case). On any S6
    loud-fail path the function calls ``sys.exit(1)`` *after* emitting
    ``hex.policy.c3-mirror-sink.failed`` so policy alerting fires.
    """
    _assert_mirror_inventory()

    db_path = _events_db_path()
    if not db_path.is_file():
        _emit_failure_event(f"events.db not found at {db_path}")
        sys.exit(1)

    mirror_dir = _mirror_dir()
    try:
        mirror_dir.mkdir(parents=True, exist_ok=True)
    except OSError as exc:
        _emit_failure_event(f"mirror dir mkdir failed at {mirror_dir}: {exc}")
        sys.exit(1)

    try:
        con = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
        con.row_factory = sqlite3.Row
    except sqlite3.Error as exc:
        _emit_failure_event(f"events.db open failed: {exc}")
        sys.exit(1)

    try:
        # Schema-drift assertion comes first — anything beyond this point
        # assumes V0.29 action_log shape (amended contract §3.2).
        assert_action_log_schema(con)

        wm_file_existed = (mirror_dir / ".watermark").is_file()
        try:
            watermark = read_watermark(con)
        except sqlite3.Error as exc:
            _emit_failure_event(f"read_watermark failed: {exc}")
            sys.exit(1)

        # Persist the cold-start watermark immediately so subsequent runs do
        # NOT re-derive it from MAX(action_log.id) — that would silently skip
        # rows that arrived between cold-start runs (B4: forward-only is
        # advisory, not "skip everything every wake").
        if not wm_file_existed:
            try:
                _advance_watermark(mirror_dir, watermark)
            except OSError as exc:
                _emit_failure_event(
                    f"cold-start watermark persist failed: {exc}"
                )
                sys.exit(1)

        # Probe for an events table — fixtures and rare deployments may have
        # action_log without events. Without the table we still emit rows but
        # the event_type/payload fields are null.
        try:
            probe = con.cursor()
            probe.execute(
                "SELECT name FROM sqlite_master WHERE type='table' AND name='events'"
            )
            has_events_table = probe.fetchone() is not None
        except sqlite3.Error as exc:
            _emit_failure_event(f"events-table probe failed: {exc}")
            sys.exit(1)

        try:
            cur = con.cursor()
            if has_events_table:
                cur.execute(
                    "SELECT a.id AS id, a.event_id AS event_id, "
                    " a.recipe AS recipe, a.action_type AS action_type, "
                    " a.action_detail AS action_detail, a.status AS status, "
                    " a.error_message AS error_message, "
                    " a.executed_at AS executed_at, "
                    " a.action_result AS action_result, "
                    " e.event_type AS event_type, e.payload AS payload "
                    "FROM action_log a LEFT JOIN events e ON e.id = a.event_id "
                    "WHERE a.id > ? ORDER BY a.id ASC",
                    (watermark,),
                )
            else:
                cur.execute(
                    "SELECT id, event_id, recipe, action_type, action_detail, "
                    " status, error_message, executed_at, action_result, "
                    " NULL AS event_type, NULL AS payload "
                    "FROM action_log WHERE id > ? ORDER BY id ASC",
                    (watermark,),
                )
            rows = cur.fetchall()
        except sqlite3.Error as exc:
            _emit_failure_event(f"action_log query failed: {exc}")
            sys.exit(1)

        if not rows:
            return 0

        new_watermark = watermark
        open_files: dict[Path, Any] = {}
        try:
            for row in rows:
                mirror_id = int(row["id"])
                try:
                    record = _build_record(row, mirror_id)
                except _CorruptRecordError as exc:
                    record = _corrupt_record(row, mirror_id, reason=str(exc))

                # Day-boundary rollover: each row goes to its OWN UTC-date
                # file, derived from the row's ts (which honours the amended
                # second-precision Z shape). This means a long backlog
                # spanning days produces one file per UTC day per §4.3.
                day = record["ts"][:10]
                target_path = mirror_dir / f"{day}.jsonl"

                if target_path not in open_files:
                    try:
                        open_files[target_path] = target_path.open(
                            "a", encoding="utf-8"
                        )
                    except OSError as exc:
                        # Disk-write error: close cleanly and exit LOUD —
                        # the watermark is NOT advanced so the next run
                        # retries this row (test 6.5).
                        _emit_failure_event(
                            f"mirror file open failed at {target_path}: {exc}"
                        )
                        sys.exit(1)

                line = json.dumps(record, ensure_ascii=False, sort_keys=True)
                try:
                    fh = open_files[target_path]
                    fh.write(line + "\n")
                    fh.flush()
                except OSError as exc:
                    _emit_failure_event(
                        f"mirror file write failed at {target_path}: {exc}"
                    )
                    sys.exit(1)

                new_watermark = mirror_id
        finally:
            for fh in open_files.values():
                try:
                    fh.close()
                except Exception:  # pragma: no cover — best effort close
                    pass

        if new_watermark != watermark:
            try:
                _advance_watermark(mirror_dir, new_watermark)
            except OSError as exc:
                _emit_failure_event(f"watermark advance failed: {exc}")
                sys.exit(1)
    finally:
        try:
            con.close()
        except Exception:  # pragma: no cover
            pass

    return 0


if __name__ == "__main__":  # pragma: no cover
    sys.exit(main() or 0)
