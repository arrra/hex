"""Red test for Task Tr92rrn58 — mirror-sink producer ``main()`` loop.

T6_0 landed the helper symbols in ``system/scripts/c3-mirror-sink.py``
(``assert_action_log_schema``, ``classify_error``, ``read_last_line_mirror_id``,
``read_watermark``). The ``main()`` producer loop — the part that actually
reads new ``action_log`` rows past the watermark and emits JSONL lines to
``~/.hex-events/mirror/YYYY-MM-DD.jsonl`` per the AMENDED contract
(docs/superpowers/specs/2026-05-24-iii-hex-mirror-sink-contract.md) — is
this task's responsibility.

This single focused red test pins the *core* producer behaviour:

    Given an events.db with one action_log row past the watermark,
    invoking ``main()`` writes exactly one JSONL line whose shape matches
    the AMENDED contract §3.1: ``mirror_id`` equals the action_log row id,
    ``ts`` is second-precision UTC with a trailing ``Z`` (no microsecond
    padding, no ``+00:00`` suffix), and ``outcome.error_class`` is either
    ``None`` or a member of the controlled vocabulary.

The test fails today because the ``__main__`` block in
``system/scripts/c3-mirror-sink.py`` is a no-op stub — there is no
``main()`` function, no JSONL emission, and no events.db lookup. It will
go green once Tr92rrn58 lands the producer loop per plan §6.1.

The test deliberately drives the script via env-var overrides
(``HEX_C3_MIRROR_DIR`` is already honoured by the helper module;
``HEX_C3_EVENTS_DB`` is the parallel hook ``main()`` must honour so this
test — and the broader 6.2–6.13 suite — can point at a throw-away DB).
"""

from __future__ import annotations

import importlib.util
import json
import sqlite3
import sys
from datetime import datetime, timezone
from pathlib import Path

import pytest


REPO_ROOT = Path(__file__).resolve().parents[2]
MIRROR_SINK_PATH = REPO_ROOT / "system" / "scripts" / "c3-mirror-sink.py"

VALID_ERROR_CLASSES = {
    "io_error",
    "timeout",
    "command_not_found",
    "non_zero_exit",
    "unknown",
}

ACTION_LOG_DDL = """
CREATE TABLE action_log (
    id INTEGER,
    event_id INTEGER,
    recipe TEXT,
    action_type TEXT,
    action_detail TEXT,
    status TEXT,
    error_message TEXT,
    executed_at TEXT,
    action_result TEXT
)
"""


def _load_mirror_sink_module():
    """Import ``c3-mirror-sink.py`` despite the hyphen in the filename."""
    if not MIRROR_SINK_PATH.is_file():
        pytest.fail(
            f"system/scripts/c3-mirror-sink.py is missing at {MIRROR_SINK_PATH}."
        )
    spec = importlib.util.spec_from_file_location("c3_mirror_sink", MIRROR_SINK_PATH)
    assert spec and spec.loader, "failed to build importlib spec"
    module = importlib.util.module_from_spec(spec)
    sys.modules["c3_mirror_sink"] = module
    spec.loader.exec_module(module)
    return module


def _seed_events_db(db_path: Path) -> None:
    """Build a minimal events.db with one successful action_log row (id=1)."""
    con = sqlite3.connect(db_path)
    try:
        con.execute(ACTION_LOG_DDL)
        con.execute(
            "INSERT INTO action_log "
            "(id, event_id, recipe, action_type, action_detail, status, "
            " error_message, executed_at, action_result) "
            "VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            (
                1,
                1001,
                "demo-recipe",
                "shell",
                json.dumps({"cmd": "echo hi"}),
                "ok",
                None,
                "2026-05-24T21:00:00Z",
                json.dumps({"stdout": "hi"}),
            ),
        )
        con.commit()
    finally:
        con.close()


def test_main_emits_jsonl_line_matching_amended_contract(tmp_path, monkeypatch, capsys):
    """Producer ``main()`` must emit a contract-shaped JSONL line per action_log row.

    Red because today the script's ``__main__`` block is the T6_0 no-op stub —
    there is no ``main()`` function and no JSONL is ever written. Goes green
    once Tr92rrn58 lands the producer loop per plan §6.1.
    """
    module = _load_mirror_sink_module()

    # Surface check first so the failure mode is unambiguous when main() is
    # still the T6_0 stub: AttributeError → "main is missing", not a
    # confusing FileNotFoundError on the mirror file.
    assert hasattr(module, "main") and callable(module.main), (
        "c3-mirror-sink.py must expose a callable main() — the Tr92rrn58 "
        "producer loop. T6_0 left __main__ as a no-op stub."
    )

    # Throw-away events.db with a single action_log row at id=1.
    events_db = tmp_path / "events.db"
    _seed_events_db(events_db)

    # Throw-away mirror dir; the helper module's _mirror_dir() already honours
    # HEX_C3_MIRROR_DIR, and main() must use the same hook so this points the
    # whole producer at the test sandbox.
    mirror_dir = tmp_path / "mirror"
    mirror_dir.mkdir()

    monkeypatch.setenv("HEX_C3_MIRROR_DIR", str(mirror_dir))
    # Parallel hook for the events.db path so tests don't have to write to
    # ~/.hex-events/events.db (which is shared with the live daemon).
    monkeypatch.setenv("HEX_C3_EVENTS_DB", str(events_db))

    # Cold-start with an empty mirror dir + empty table would set watermark to
    # MAX(id)=0. Since we seeded id=1 BEFORE main() runs, the cold-start
    # branch initialises watermark=1 and emits zero lines (forward-only, B4).
    # So we pre-seed the watermark file at 0 to force main() to process id=1.
    (mirror_dir / ".watermark").write_text("0", encoding="utf-8")

    rc = module.main()

    # main() should exit cleanly on the happy path.
    assert rc in (None, 0), f"main() returned non-zero exit code {rc!r}"

    # Exactly one JSONL file should exist for today (UTC), and it must
    # contain exactly one line for the single action_log row.
    today = datetime.now(timezone.utc).strftime("%Y-%m-%d")
    jsonl_path = mirror_dir / f"{today}.jsonl"
    assert jsonl_path.is_file(), (
        f"main() did not create the mirror JSONL file at {jsonl_path}. "
        "The producer must emit to ~/.hex-events/mirror/YYYY-MM-DD.jsonl "
        "(honouring HEX_C3_MIRROR_DIR) per amended contract §4.1."
    )

    lines = [ln for ln in jsonl_path.read_text(encoding="utf-8").splitlines() if ln.strip()]
    assert len(lines) == 1, (
        f"expected exactly 1 JSONL line for the single action_log row, "
        f"got {len(lines)} lines: {lines!r}"
    )

    record = json.loads(lines[0])

    # --- AMENDED contract §3.1 field-presence schema ----------------------
    # mirror_id MUST equal the action_log row id (ordering authority).
    assert record.get("mirror_id") == 1, (
        f"mirror_id must equal the action_log row id (1), got {record.get('mirror_id')!r}. "
        "Amended contract §3.1: mirror_id is the ordering authority."
    )

    # ts MUST be second-precision UTC with a trailing 'Z'. No '.000000'
    # microsecond padding and no '+00:00' offset suffix. (Amended §3.1.)
    ts = record.get("ts")
    assert isinstance(ts, str) and ts, "ts must be a non-empty string"
    assert ts.endswith("Z"), (
        f"ts must end with 'Z' (UTC), got {ts!r}. No '+00:00' offset suffix."
    )
    assert "+00:00" not in ts, f"ts must NOT carry a '+00:00' suffix, got {ts!r}"
    assert "." not in ts, (
        f"ts must be SECOND precision per amended contract — no microsecond "
        f"padding. Got {ts!r}; expected shape 'YYYY-MM-DDTHH:MM:SSZ'."
    )
    # Sanity-check the shape parses as second-precision ISO 8601.
    datetime.strptime(ts, "%Y-%m-%dT%H:%M:%SZ")

    # outcome.error_class is null on success or a member of the controlled
    # vocabulary on failure. This row is status='ok', so error_class MUST be
    # null (None in JSON parsed form).
    outcome = record.get("outcome")
    assert isinstance(outcome, dict), (
        f"outcome must be an object, got {type(outcome).__name__}: {outcome!r}"
    )
    assert "error_class" in outcome, (
        "outcome.error_class is a REQUIRED field per amended contract §3.1 "
        "(null on success, controlled vocab on failure)."
    )
    error_class = outcome["error_class"]
    assert error_class is None or error_class in VALID_ERROR_CLASSES, (
        f"outcome.error_class={error_class!r} must be None (success) or one "
        f"of {sorted(VALID_ERROR_CLASSES)} on failure."
    )
    assert error_class is None, (
        f"This action_log row has status='ok'; outcome.error_class must be "
        f"null, got {error_class!r}."
    )
