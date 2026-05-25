"""Red tests for Task Tehrmnh5e (§Task 4) — M4 time-to-detect (TTD) tracker.

These tests target the state-machine collector that lands at
``system/scripts/c3-ttd-tracker.py`` per the C3 instrumentation plan §4.1.

Contracted behaviours (from the task contract / plan §4.1):

* State file at ``$HEX_DIR/.hex/telemetry/c3-ttd-state.json`` is the
  authoritative incident ledger.
* Opens an incident on **failure patterns**:
    - ``hex.alert.error``
    - ``hex.alert.critical``
    - ``hex.policy.*.failed`` (any policy name)
    - ``hex.boi.integrity.violation``
* Closes the incident on a matching **clean signal** (e.g. ``hex.alert.cleared``
  for ``hex.alert.*`` openers; ``hex.policy.<name>.recovered`` /
  ``...succeeded`` for ``hex.policy.<name>.failed`` openers — exact label is
  implementation-defined as long as the closer reliably terminates the open
  incident).
* On close, emits ``hex.c3.ttd.sample`` with the elapsed detection seconds.
* On **cold-start** (no state file present), the tracker initialises the
  state file AND emits a ``hex.c3.ttd.bootstrap`` event so operators can see
  the tracker has started.
* Any incident that remains open beyond **24h** is reported with
  ``status='still_open_24h'`` (production has zero ``hex.alert.cleared``
  today — acknowledged in the task contract).

All tests deliberately fail today because ``system/scripts/c3-ttd-tracker.py``
does not exist yet. They will go green once §Task 4.1 lands the collector.
"""

from __future__ import annotations

import json
import os
import sqlite3
import subprocess
import sys
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Iterable

import pytest


REPO_ROOT = Path(__file__).resolve().parents[2]
TTD_TRACKER_PATH = REPO_ROOT / "system" / "scripts" / "c3-ttd-tracker.py"

# Failure-pattern event types that MUST open an incident. The first three are
# verbatim from the task contract; the fourth uses a representative policy
# name to prove the ``hex.policy.*.failed`` glob.
FAILURE_OPEN_EVENTS = [
    "hex.alert.error",
    "hex.alert.critical",
    "hex.boi.integrity.violation",
    "hex.policy.demo-policy.failed",
]


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _require_script_exists() -> None:
    if not TTD_TRACKER_PATH.is_file():
        pytest.fail(
            f"system/scripts/c3-ttd-tracker.py is missing at {TTD_TRACKER_PATH}. "
            "Task §4.1 must create this collector script."
        )


def _make_events_db(db_path: Path) -> sqlite3.Connection:
    """Create the canonical telemetry ``events`` table used by hex-events."""
    con = sqlite3.connect(db_path)
    con.execute(
        """
        CREATE TABLE events (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            ts          TEXT NOT NULL,
            event_type  TEXT NOT NULL,
            source      TEXT NOT NULL DEFAULT '',
            payload     TEXT NOT NULL DEFAULT '{}',
            context     TEXT DEFAULT NULL
        )
        """
    )
    con.execute("CREATE INDEX idx_events_ts_event_type ON events (ts, event_type)")
    con.execute("CREATE INDEX idx_events_event_type     ON events (event_type)")
    con.commit()
    return con


def _insert_event(
    con: sqlite3.Connection,
    *,
    ts: str,
    event_type: str,
    payload: dict | None = None,
    source: str = "test-fixture",
) -> int:
    cur = con.execute(
        "INSERT INTO events (ts, event_type, source, payload) VALUES (?, ?, ?, ?)",
        (ts, event_type, source, json.dumps(payload or {})),
    )
    con.commit()
    return cur.lastrowid


def _emit_capture_dir(stub_root: Path) -> Path:
    """Capture file where the stubbed ``hex-emit.sh`` records every emission.

    The script under test calls ``hex-emit.sh <event_type> <payload> [source]``
    to surface bootstrap / sample events. We swap that binary for a thin shell
    stub that appends every invocation to ``emitted.jsonl`` so the tests can
    assert which events were emitted.
    """
    return stub_root / "emitted.jsonl"


def _install_hex_emit_stub(hex_dir: Path) -> Path:
    """Drop a fake ``hex-emit.sh`` into the temp HEX_DIR.

    The real binary lives at ``$HEX_DIR/.hex/bin/hex-emit.sh``. Replace it
    with a stub that captures invocations to ``emitted.jsonl`` (one JSON
    record per call). Returns the capture path so callers can read it.
    """
    bin_dir = hex_dir / ".hex" / "bin"
    bin_dir.mkdir(parents=True, exist_ok=True)
    capture = _emit_capture_dir(hex_dir)
    stub = bin_dir / "hex-emit.sh"
    stub.write_text(
        "#!/usr/bin/env bash\n"
        "set -euo pipefail\n"
        f"capture={capture!s}\n"
        "event_type=${1:-}\n"
        "payload=${2:-'{}'}\n"
        "source=${3:-shell}\n"
        # Use python so we get a real JSON line even if payload was already JSON
        "python3 - \"$event_type\" \"$payload\" \"$source\" \"$capture\" <<'PYEOF'\n"
        "import json, sys\n"
        "event_type, payload, source, capture = sys.argv[1:5]\n"
        "try:\n"
        "    parsed = json.loads(payload)\n"
        "except Exception:\n"
        "    parsed = {'raw': payload}\n"
        "rec = {'event_type': event_type, 'payload': parsed, 'source': source}\n"
        "with open(capture, 'a', encoding='utf-8') as f:\n"
        "    f.write(json.dumps(rec) + '\\n')\n"
        "PYEOF\n",
        encoding="utf-8",
    )
    stub.chmod(0o755)
    return capture


def _read_emissions(capture: Path) -> list[dict]:
    if not capture.is_file():
        return []
    out: list[dict] = []
    for line in capture.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line:
            continue
        out.append(json.loads(line))
    return out


def _setup_temp_hex_dir(tmp_path: Path) -> tuple[Path, Path]:
    """Build a throw-away ``$HEX_DIR`` skeleton + return ``(hex_dir, events_db)``.

    Layout:
        tmp/hex/
          .hex/
            bin/hex-emit.sh   (stub installed by _install_hex_emit_stub)
            telemetry/events.db
    """
    hex_dir = tmp_path / "hex"
    telemetry_dir = hex_dir / ".hex" / "telemetry"
    telemetry_dir.mkdir(parents=True, exist_ok=True)
    db_path = telemetry_dir / "events.db"
    con = _make_events_db(db_path)
    con.close()
    return hex_dir, db_path


def _run_tracker(hex_dir: Path, *, env_extra: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()
    env["HEX_DIR"] = str(hex_dir)
    env["HEX_ROOT"] = str(hex_dir)
    # Put our stub bin first on PATH so `hex-emit.sh` resolves to the capture.
    stub_bin = str((hex_dir / ".hex" / "bin").resolve())
    env["PATH"] = stub_bin + os.pathsep + env.get("PATH", "")
    if env_extra:
        env.update(env_extra)
    return subprocess.run(
        [sys.executable, str(TTD_TRACKER_PATH)],
        env=env,
        capture_output=True,
        text=True,
        timeout=60,
    )


# ---------------------------------------------------------------------------
# 1. Cold-start → state file + bootstrap emission
# ---------------------------------------------------------------------------

def test_cold_start_creates_state_file_and_emits_bootstrap(tmp_path):
    """First-ever run with no state file → init state + emit hex.c3.ttd.bootstrap."""
    _require_script_exists()
    hex_dir, _ = _setup_temp_hex_dir(tmp_path)
    capture = _install_hex_emit_stub(hex_dir)

    result = _run_tracker(hex_dir)
    assert result.returncode == 0, (
        f"c3-ttd-tracker.py must exit 0 on a clean cold-start, got "
        f"{result.returncode}.\nstdout={result.stdout!r}\nstderr={result.stderr!r}"
    )

    state_path = hex_dir / ".hex" / "telemetry" / "c3-ttd-state.json"
    assert state_path.is_file(), (
        "Cold-start MUST create the state file at "
        "$HEX_DIR/.hex/telemetry/c3-ttd-state.json per plan §4.1."
    )

    emissions = _read_emissions(capture)
    bootstrap_emits = [e for e in emissions if e["event_type"] == "hex.c3.ttd.bootstrap"]
    assert bootstrap_emits, (
        "Cold-start MUST emit exactly one hex.c3.ttd.bootstrap event so "
        "operators can see the tracker initialised. Observed emissions: "
        f"{[e['event_type'] for e in emissions]}"
    )


# ---------------------------------------------------------------------------
# 2. Failure events open an incident in the state file
# ---------------------------------------------------------------------------

@pytest.mark.parametrize("failure_event_type", FAILURE_OPEN_EVENTS)
def test_failure_event_opens_incident_in_state(tmp_path, failure_event_type):
    """Every documented failure pattern MUST open an incident."""
    _require_script_exists()
    hex_dir, db_path = _setup_temp_hex_dir(tmp_path)
    _install_hex_emit_stub(hex_dir)

    # Inject one failure event into the events table.
    now_iso = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%fZ")
    con = sqlite3.connect(db_path)
    try:
        _insert_event(con, ts=now_iso, event_type=failure_event_type)
    finally:
        con.close()

    result = _run_tracker(hex_dir)
    assert result.returncode == 0, (
        f"Tracker must exit 0 after processing a single failure event "
        f"({failure_event_type}).\nstderr={result.stderr!r}"
    )

    state_path = hex_dir / ".hex" / "telemetry" / "c3-ttd-state.json"
    assert state_path.is_file(), "State file must exist after first run"
    state = json.loads(state_path.read_text(encoding="utf-8"))

    # Find the open incidents — schema is loosely defined; accept either a
    # top-level ``open_incidents`` list or a generic ``incidents`` list whose
    # items have a status/closed field. The structural assertion is: there is
    # AT LEAST ONE recorded incident whose event_type matches the failure we
    # injected and which is NOT yet closed.
    open_incidents = _extract_open_incidents(state)
    matching = [
        inc for inc in open_incidents
        if inc.get("event_type") == failure_event_type
        or inc.get("opener_event_type") == failure_event_type
        or inc.get("opened_by", {}).get("event_type") == failure_event_type
    ]
    assert matching, (
        f"State file must record an open incident for {failure_event_type!r}. "
        f"Got state={state!r}"
    )


# ---------------------------------------------------------------------------
# 3. Close → hex.c3.ttd.sample with detection seconds
# ---------------------------------------------------------------------------

def test_failure_then_clean_signal_emits_ttd_sample(tmp_path):
    """Open hex.alert.error → close via hex.alert.cleared → emit hex.c3.ttd.sample."""
    _require_script_exists()
    hex_dir, db_path = _setup_temp_hex_dir(tmp_path)
    capture = _install_hex_emit_stub(hex_dir)

    # Inject an opening failure 5 minutes ago, then a cleared signal now.
    opened_at = datetime.now(timezone.utc) - timedelta(minutes=5)
    cleared_at = datetime.now(timezone.utc)
    opened_iso = opened_at.strftime("%Y-%m-%dT%H:%M:%S.%fZ")
    cleared_iso = cleared_at.strftime("%Y-%m-%dT%H:%M:%S.%fZ")

    con = sqlite3.connect(db_path)
    try:
        _insert_event(con, ts=opened_iso, event_type="hex.alert.error",
                      payload={"key": "k1"})
        _insert_event(con, ts=cleared_iso, event_type="hex.alert.cleared",
                      payload={"key": "k1"})
    finally:
        con.close()

    result = _run_tracker(hex_dir)
    assert result.returncode == 0, (
        f"Tracker must exit 0 after observing open+clear pair.\n"
        f"stderr={result.stderr!r}"
    )

    emissions = _read_emissions(capture)
    samples = [e for e in emissions if e["event_type"] == "hex.c3.ttd.sample"]
    assert samples, (
        "Closing an incident MUST emit a hex.c3.ttd.sample event per plan §4.1. "
        f"Observed emissions: {[e['event_type'] for e in emissions]}"
    )

    # The sample payload must surface a detection-seconds figure so M4 can
    # actually be measured. Accept any non-negative numeric value under a
    # documented key — ``detection_seconds`` is the canonical name in the plan.
    sample = samples[-1]
    payload = sample.get("payload", {})
    detection = (
        payload.get("detection_seconds")
        or payload.get("ttd_seconds")
        or payload.get("seconds_to_detect")
    )
    assert isinstance(detection, (int, float)) and detection >= 0, (
        "hex.c3.ttd.sample payload must include a numeric detection-seconds "
        f"field (detection_seconds / ttd_seconds). Got payload={payload!r}"
    )


# ---------------------------------------------------------------------------
# 4. Stale incident (>24h still open) → status='still_open_24h'
# ---------------------------------------------------------------------------

def test_stale_incident_marked_still_open_24h(tmp_path):
    """An incident opened >24h ago that has NO close signal gets status='still_open_24h'."""
    _require_script_exists()
    hex_dir, db_path = _setup_temp_hex_dir(tmp_path)
    _install_hex_emit_stub(hex_dir)

    # Pre-seed an open incident from 30h ago directly in the state file so we
    # are independent of how the tracker discovers events.
    state_path = hex_dir / ".hex" / "telemetry" / "c3-ttd-state.json"
    opened_at = datetime.now(timezone.utc) - timedelta(hours=30)
    opened_iso = opened_at.strftime("%Y-%m-%dT%H:%M:%S.%fZ")
    seeded_state = {
        "version": 1,
        "open_incidents": [
            {
                "incident_id": "seed-stale-1",
                "event_type": "hex.alert.critical",
                "opener_event_type": "hex.alert.critical",
                "opened_at": opened_iso,
                "status": "open",
                "payload_key": "stale-key",
            }
        ],
        "closed_incidents": [],
    }
    state_path.write_text(json.dumps(seeded_state), encoding="utf-8")

    # Also seed the original failure event so any tracker that re-derives state
    # from events.db still finds the open incident.
    con = sqlite3.connect(db_path)
    try:
        _insert_event(con, ts=opened_iso, event_type="hex.alert.critical",
                      payload={"key": "stale-key"})
    finally:
        con.close()

    result = _run_tracker(hex_dir)
    assert result.returncode == 0, (
        f"Tracker must exit 0 even when a stale incident is present.\n"
        f"stderr={result.stderr!r}"
    )

    state_after = json.loads(state_path.read_text(encoding="utf-8"))
    incidents = _extract_open_incidents(state_after) + _extract_closed_incidents(state_after)
    stale = [
        inc for inc in incidents
        if inc.get("status") == "still_open_24h"
        or inc.get("state") == "still_open_24h"
    ]
    assert stale, (
        "An incident opened >24h ago with no close signal MUST be marked "
        "status='still_open_24h' per task contract. State after run: "
        f"{state_after!r}"
    )


# ---------------------------------------------------------------------------
# Internal helpers
# ---------------------------------------------------------------------------

def _extract_open_incidents(state: dict) -> list[dict]:
    """Pull open incidents from the state JSON, tolerating layout variation."""
    raw = state.get("open_incidents")
    if isinstance(raw, list):
        return [r for r in raw if isinstance(r, dict)]
    raw = state.get("incidents")
    if isinstance(raw, list):
        return [
            r for r in raw
            if isinstance(r, dict)
            and r.get("status") not in {"closed", "resolved"}
        ]
    return []


def _extract_closed_incidents(state: dict) -> list[dict]:
    raw = state.get("closed_incidents")
    if isinstance(raw, list):
        return [r for r in raw if isinstance(r, dict)]
    raw = state.get("incidents")
    if isinstance(raw, list):
        return [
            r for r in raw
            if isinstance(r, dict)
            and r.get("status") in {"closed", "resolved", "still_open_24h"}
        ]
    return []
