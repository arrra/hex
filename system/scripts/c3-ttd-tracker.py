#!/usr/bin/env python3
"""C3 / M4 — time-to-detect (TTD) state-machine tracker.

Reads ``$HEX_DIR/.hex/telemetry/events.db`` forward of the persisted
watermark and maintains an incident ledger at
``$HEX_DIR/.hex/telemetry/c3-ttd-state.json``.

Behaviour (plan §Task 4.1):

* **Cold-start** (no state file): emit ``hex.c3.ttd.bootstrap`` so
  operators can see the tracker initialised, then create an empty
  state file. Subsequent runs are idempotent — the persisted
  ``last_processed_event_id`` watermark advances forward-only.

* **Failure patterns** (open a new incident):

      hex.alert.error
      hex.alert.critical
      hex.boi.integrity.violation
      hex.policy.*.failed

* **Clean signals** (close the matching open incident):

      hex.alert.cleared                       → closes hex.alert.error/critical
      hex.boi.integrity.cleared/resolved      → closes hex.boi.integrity.violation
      hex.policy.<name>.recovered/succeeded   → closes hex.policy.<name>.failed

  When an incident is closed we emit one ``hex.c3.ttd.sample`` event
  carrying ``detection_seconds`` (the elapsed time between opener and
  closer timestamps).

* **Stale incidents** (open >24h with no close signal) are flipped to
  ``status='still_open_24h'`` so operators get a loud signal that
  detection never converged. Production has zero
  ``hex.alert.cleared`` today (acknowledged in the task contract) so
  this branch will fire as soon as a stale alert is observed.

S6 loud failures:
  Any sqlite/IO error, missing events.db, or hex-emit.sh non-zero
  return is logged to stderr. The tracker continues processing the
  remaining events so a single bad emission does not silently lose
  the ledger.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import sqlite3
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Iterable

EVENT_BOOTSTRAP = "hex.c3.ttd.bootstrap"
EVENT_SAMPLE = "hex.c3.ttd.sample"
SOURCE = "c3-ttd-tracker"
EMIT_TIMEOUT_SECONDS = 10
STALE_THRESHOLD_SECONDS = 24 * 60 * 60

# Failure-pattern openers. The first three match exactly; the fourth
# matches any policy name (the ``.+`` glob).
OPENER_PATTERNS: list[re.Pattern[str]] = [
    re.compile(r"^hex\.alert\.error$"),
    re.compile(r"^hex\.alert\.critical$"),
    re.compile(r"^hex\.boi\.integrity\.violation$"),
    re.compile(r"^hex\.policy\..+\.failed$"),
]

# Closers that exactly match a specific opener event_type.
_ALERT_CLOSER = "hex.alert.cleared"
_BOI_INTEGRITY_CLOSERS = {"hex.boi.integrity.cleared", "hex.boi.integrity.resolved"}
_POLICY_CLOSER_RE = re.compile(r"^hex\.policy\.(.+)\.(recovered|succeeded)$")


# ---------------------------------------------------------------------------
# Environment / paths
# ---------------------------------------------------------------------------

def _resolve_hex_root() -> Path:
    root = os.environ.get("HEX_ROOT") or os.environ.get("HEX_DIR")
    if not root:
        print(
            "[c3-ttd-tracker] ERROR: neither HEX_ROOT nor HEX_DIR is set",
            file=sys.stderr,
        )
        sys.exit(2)
    return Path(root)


def _state_path(hex_root: Path) -> Path:
    return hex_root / ".hex" / "telemetry" / "c3-ttd-state.json"


def _events_db_path(hex_root: Path) -> Path:
    return hex_root / ".hex" / "telemetry" / "events.db"


def _emit_script_path(hex_root: Path) -> Path:
    return hex_root / ".hex" / "bin" / "hex-emit.sh"


# ---------------------------------------------------------------------------
# Opener / closer classification
# ---------------------------------------------------------------------------

def _is_opener(event_type: str) -> bool:
    return any(p.match(event_type) for p in OPENER_PATTERNS)


def _closer_targets(event_type: str) -> set[str]:
    """Return the set of opener event_types this closer can terminate.

    Empty set ⇒ the event is not a known closer.
    """
    if event_type == _ALERT_CLOSER:
        return {"hex.alert.error", "hex.alert.critical"}
    if event_type in _BOI_INTEGRITY_CLOSERS:
        return {"hex.boi.integrity.violation"}
    match = _POLICY_CLOSER_RE.match(event_type)
    if match:
        return {f"hex.policy.{match.group(1)}.failed"}
    return set()


def _is_closer(event_type: str) -> bool:
    return bool(_closer_targets(event_type))


# ---------------------------------------------------------------------------
# State I/O
# ---------------------------------------------------------------------------

def _load_state(state_path: Path) -> dict[str, Any] | None:
    if not state_path.exists():
        return None
    try:
        data = json.loads(state_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        print(
            f"[c3-ttd-tracker] ERROR: state file at {state_path} is corrupt: {exc}",
            file=sys.stderr,
        )
        sys.exit(2)
    if not isinstance(data, dict):
        print(
            f"[c3-ttd-tracker] ERROR: state file at {state_path} is not a JSON object",
            file=sys.stderr,
        )
        sys.exit(2)
    return data


def _save_state(state_path: Path, state: dict[str, Any]) -> None:
    state_path.parent.mkdir(parents=True, exist_ok=True)
    tmp = state_path.with_suffix(state_path.suffix + ".tmp")
    tmp.write_text(json.dumps(state, indent=2, sort_keys=True), encoding="utf-8")
    tmp.replace(state_path)


# ---------------------------------------------------------------------------
# Emission
# ---------------------------------------------------------------------------

def _emit(emit_script: Path, hex_root: Path, event_type: str, payload: dict) -> int:
    """Invoke ``hex-emit.sh``. Return its exit code (0 on success)."""
    target = emit_script
    if not target.exists():
        # Fall back to a PATH-resolvable ``hex-emit.sh`` (the test harness
        # installs a stub on PATH so the script under test can be exercised
        # without touching the real deployed binary).
        resolved = shutil.which("hex-emit.sh")
        if resolved:
            target = Path(resolved)
        else:
            print(
                f"[c3-ttd-tracker] ERROR: hex-emit.sh not found at {emit_script} "
                "and no fallback on PATH",
                file=sys.stderr,
            )
            return 1
    cmd = [
        "bash",
        str(target),
        event_type,
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
            f"[c3-ttd-tracker] ERROR: hex-emit.sh invocation raised "
            f"{type(exc).__name__}: {exc}",
            file=sys.stderr,
        )
        return 1
    if completed.returncode != 0:
        print(
            f"[c3-ttd-tracker] hex-emit.sh exited {completed.returncode} for "
            f"event_type={event_type!r}; stderr={completed.stderr.strip()!r}",
            file=sys.stderr,
        )
    return completed.returncode


# ---------------------------------------------------------------------------
# Event helpers
# ---------------------------------------------------------------------------

def _parse_iso(ts: str) -> datetime:
    """Accept ISO-8601 with either ``Z`` or ``+00:00`` UTC suffix."""
    s = ts.strip()
    if s.endswith("Z"):
        s = s[:-1] + "+00:00"
    dt = datetime.fromisoformat(s)
    if dt.tzinfo is None:
        dt = dt.replace(tzinfo=timezone.utc)
    return dt


def _payload_key(payload: Any) -> str | None:
    """Best-effort correlation key extracted from an event payload.

    The conventional field is ``key`` but we also accept the common
    aliases used across hex emitters so open↔close pairs reliably
    correlate.
    """
    if not isinstance(payload, dict):
        return None
    for field in (
        "key",
        "incident_key",
        "alert_key",
        "policy",
        "policy_name",
        "rule",
    ):
        value = payload.get(field)
        if isinstance(value, str) and value:
            return value
    return None


def _read_events(db_path: Path, since_event_id: int) -> list[dict[str, Any]]:
    """Return rows from events.db with id > ``since_event_id``.

    Ordered by ``(ts, id)`` so opener events always precede their
    closers in the iteration even when both arrived in the same run.
    """
    if not db_path.exists():
        print(
            f"[c3-ttd-tracker] ERROR: events.db missing at {db_path}",
            file=sys.stderr,
        )
        sys.exit(2)
    con = sqlite3.connect(str(db_path))
    try:
        con.row_factory = sqlite3.Row
        cur = con.cursor()
        cur.execute(
            "SELECT id, ts, event_type, source, payload FROM events "
            "WHERE id > ? ORDER BY ts ASC, id ASC",
            (since_event_id,),
        )
        out: list[dict[str, Any]] = []
        for row in cur.fetchall():
            raw_payload = row["payload"]
            try:
                payload = json.loads(raw_payload) if raw_payload else {}
            except (TypeError, json.JSONDecodeError):
                payload = {"_raw": raw_payload}
            out.append(
                {
                    "id": int(row["id"]),
                    "ts": row["ts"],
                    "event_type": row["event_type"],
                    "source": row["source"] or "",
                    "payload": payload,
                }
            )
        return out
    finally:
        con.close()


# ---------------------------------------------------------------------------
# State-machine logic
# ---------------------------------------------------------------------------

def _existing_open_match(
    open_incidents: list[dict[str, Any]],
    *,
    event_type: str,
    payload_key: str | None,
) -> dict[str, Any] | None:
    """Find an existing open incident that should dedupe a new opener.

    Two openers with the same ``opener_event_type`` AND the same
    correlation key represent the same incident continuing to fire; we
    keep the original incident_id rather than spawning a duplicate.
    """
    for inc in open_incidents:
        if inc.get("status") not in {"open", "still_open_24h"}:
            continue
        if inc.get("opener_event_type") != event_type and inc.get("event_type") != event_type:
            continue
        inc_key = inc.get("payload_key")
        if payload_key is None and inc_key is None:
            return inc
        if payload_key is not None and inc_key is not None and payload_key == inc_key:
            return inc
    return None


def _find_closeable(
    open_incidents: list[dict[str, Any]],
    *,
    closer_event_type: str,
    payload_key: str | None,
) -> dict[str, Any] | None:
    """Pick the oldest open incident a closer should resolve.

    Closer ↔ opener mapping is exact:

        hex.alert.cleared                    → hex.alert.{error,critical}
        hex.boi.integrity.{cleared,resolved} → hex.boi.integrity.violation
        hex.policy.<n>.{recovered,succeeded} → hex.policy.<n>.failed

    When both sides expose a correlation key we require the keys to
    match; otherwise we accept any open incident with a matching
    opener_event_type. Picking the oldest (FIFO) keeps the metric
    biased toward the worst-case detection time when multiple alerts
    of the same family are in flight.
    """
    targets = _closer_targets(closer_event_type)
    if not targets:
        return None
    candidates: list[dict[str, Any]] = []
    for inc in open_incidents:
        if inc.get("status") not in {"open", "still_open_24h"}:
            continue
        opener = inc.get("opener_event_type") or inc.get("event_type")
        if opener not in targets:
            continue
        inc_key = inc.get("payload_key")
        if payload_key is not None and inc_key is not None and inc_key != payload_key:
            continue
        candidates.append(inc)
    if not candidates:
        return None
    # FIFO — oldest opened_at first.
    candidates.sort(key=lambda i: i.get("opened_at") or "")
    return candidates[0]


def _process_events(
    events: list[dict[str, Any]],
    state: dict[str, Any],
    *,
    emit_script: Path,
    hex_root: Path,
) -> int:
    """Mutate ``state`` in place by replaying ``events``.

    Returns the highest event id observed (or the current watermark
    when no events were processed).
    """
    open_incidents: list[dict[str, Any]] = state["open_incidents"]
    closed_incidents: list[dict[str, Any]] = state["closed_incidents"]
    max_id_seen = int(state.get("last_processed_event_id") or 0)

    for ev in events:
        max_id_seen = max(max_id_seen, ev["id"])
        evt = ev["event_type"]
        payload = ev["payload"]
        key = _payload_key(payload)

        if _is_opener(evt):
            if _existing_open_match(
                open_incidents, event_type=evt, payload_key=key
            ) is not None:
                # Same alert family already open with this correlation key;
                # don't spawn a duplicate incident.
                continue
            incident = {
                "incident_id": f"inc-{ev['id']}-{evt}",
                "event_type": evt,
                "opener_event_type": evt,
                "opened_at": ev["ts"],
                "opened_by": {
                    "event_type": evt,
                    "event_id": ev["id"],
                    "source": ev.get("source", ""),
                },
                "status": "open",
                "payload_key": key,
            }
            open_incidents.append(incident)
            continue

        if _is_closer(evt):
            target = _find_closeable(
                open_incidents, closer_event_type=evt, payload_key=key
            )
            if target is None:
                # Spurious closer with no matching open incident — record
                # nothing rather than emit a noise sample.
                continue
            try:
                opened_dt = _parse_iso(target["opened_at"])
                closed_dt = _parse_iso(ev["ts"])
                detection = max(0.0, (closed_dt - opened_dt).total_seconds())
            except (KeyError, ValueError) as exc:
                print(
                    f"[c3-ttd-tracker] WARN: failed to compute detection "
                    f"seconds for incident {target.get('incident_id')!r}: {exc}",
                    file=sys.stderr,
                )
                detection = 0.0
            target["status"] = "closed"
            target["closed_at"] = ev["ts"]
            target["closed_by"] = {
                "event_type": evt,
                "event_id": ev["id"],
                "source": ev.get("source", ""),
            }
            target["detection_seconds"] = detection
            sample_payload = {
                "incident_id": target["incident_id"],
                "opener_event_type": target.get("opener_event_type"),
                "closer_event_type": evt,
                "opened_at": target.get("opened_at"),
                "closed_at": ev["ts"],
                "detection_seconds": detection,
                "payload_key": target.get("payload_key"),
            }
            rc = _emit(emit_script, hex_root, EVENT_SAMPLE, sample_payload)
            if rc != 0:
                print(
                    "[c3-ttd-tracker] WARN: hex.c3.ttd.sample emit failed "
                    f"for incident_id={target['incident_id']!r}; continuing",
                    file=sys.stderr,
                )
            continue

        # Not an opener and not a known closer — ignore.

    # Migrate closed incidents out of open_incidents.
    still_open: list[dict[str, Any]] = []
    for inc in open_incidents:
        if inc.get("status") == "closed":
            closed_incidents.append(inc)
        else:
            still_open.append(inc)

    state["open_incidents"] = still_open
    state["closed_incidents"] = closed_incidents
    return max_id_seen


def _mark_stale(state: dict[str, Any], *, now: datetime) -> None:
    """Mark any open incident older than 24h as ``still_open_24h``."""
    for inc in state["open_incidents"]:
        if inc.get("status") == "still_open_24h":
            continue
        opened_iso = inc.get("opened_at")
        if not opened_iso:
            continue
        try:
            opened_dt = _parse_iso(opened_iso)
        except ValueError:
            continue
        age = (now - opened_dt).total_seconds()
        if age >= STALE_THRESHOLD_SECONDS:
            inc["status"] = "still_open_24h"
            inc["stale_marked_at"] = now.strftime("%Y-%m-%dT%H:%M:%S.%fZ")
            inc["age_seconds_at_mark"] = age


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="C3/M4 time-to-detect state-machine tracker."
    )
    parser.parse_args(argv)

    hex_root = _resolve_hex_root()
    state_path = _state_path(hex_root)
    db_path = _events_db_path(hex_root)
    emit_script = _emit_script_path(hex_root)

    now = datetime.now(tz=timezone.utc)
    now_iso = now.strftime("%Y-%m-%dT%H:%M:%S.%fZ")

    # Cold-start ----------------------------------------------------------
    state = _load_state(state_path)
    cold_start = state is None
    if cold_start:
        bootstrap_payload = {
            "bootstrapped_at": now_iso,
            "reason": "cold_start_no_state_file",
            "stale_threshold_seconds": STALE_THRESHOLD_SECONDS,
        }
        rc = _emit(emit_script, hex_root, EVENT_BOOTSTRAP, bootstrap_payload)
        if rc != 0:
            print(
                "[c3-ttd-tracker] WARN: hex.c3.ttd.bootstrap emit failed; "
                "continuing so the state file is still initialised.",
                file=sys.stderr,
            )
        state = {
            "version": 1,
            "last_processed_event_id": 0,
            "open_incidents": [],
            "closed_incidents": [],
            "bootstrapped_at": now_iso,
        }

    # Schema-normalise so older state files stay readable.
    state.setdefault("version", 1)
    state.setdefault("last_processed_event_id", 0)
    state.setdefault("open_incidents", [])
    state.setdefault("closed_incidents", [])

    # Process new events --------------------------------------------------
    new_events = _read_events(db_path, int(state["last_processed_event_id"] or 0))
    max_id_seen = _process_events(
        new_events, state, emit_script=emit_script, hex_root=hex_root
    )

    # Stale sweep ---------------------------------------------------------
    _mark_stale(state, now=now)

    # Persist -------------------------------------------------------------
    state["last_processed_event_id"] = max_id_seen
    state["last_run_at"] = now_iso
    if cold_start:
        state["bootstrapped_at"] = state.get("bootstrapped_at", now_iso)
    _save_state(state_path, state)

    return 0


if __name__ == "__main__":  # pragma: no cover
    sys.exit(main())
