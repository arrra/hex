"""Red tests for Task T6_0 (Tfdmjdcqn) — mirror-sink startup-audit helpers.

These tests target the shared producer-helper code that lives in
``system/scripts/c3-mirror-sink.py`` per the AMENDED mirror-sink contract
(docs/superpowers/specs/2026-05-24-iii-hex-mirror-sink-contract.md).

The full producer behaviour is exercised by ``tests/c3/test_mirror_sink.py``
(owned by Task Tr92rrn58 / §Task 6). The tests in *this* file are scoped to
the helper-symbol audit landed by T6_0:

    * EXPECTED_ACTION_LOG_COLUMNS  — the 9-column schema constant
    * assert_action_log_schema(con) — exits 1 on drift, returns None on match
    * classify_error(status, error_message, returncode) — controlled vocab
    * read_last_line_mirror_id(path) — last-line mirror_id from JSONL file
    * read_watermark(con)            — cold-start = MAX(action_log.id)

All tests deliberately fail today because ``system/scripts/c3-mirror-sink.py``
does not exist yet. They will go green once T6_0 lands the helpers.
"""

from __future__ import annotations

import importlib.util
import json
import sqlite3
import sys
from pathlib import Path

import pytest


REPO_ROOT = Path(__file__).resolve().parents[2]
MIRROR_SINK_PATH = REPO_ROOT / "system" / "scripts" / "c3-mirror-sink.py"

EXPECTED_COLUMNS_TUPLES = [
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

VALID_ERROR_CLASSES = {
    "io_error",
    "timeout",
    "command_not_found",
    "non_zero_exit",
    "unknown",
}


def _load_mirror_sink_module():
    """Import ``c3-mirror-sink.py`` as a module despite the hyphen in the name."""
    if not MIRROR_SINK_PATH.is_file():
        pytest.fail(
            f"system/scripts/c3-mirror-sink.py is missing at {MIRROR_SINK_PATH}. "
            "T6_0 must create this file with the helper symbols."
        )
    spec = importlib.util.spec_from_file_location("c3_mirror_sink", MIRROR_SINK_PATH)
    assert spec and spec.loader, "failed to build importlib spec for c3-mirror-sink.py"
    module = importlib.util.module_from_spec(spec)
    sys.modules["c3_mirror_sink"] = module
    spec.loader.exec_module(module)
    return module


def _make_action_log_db(tmp_path: Path, *, drop_column: str | None = None) -> Path:
    """Build a throw-away events.db with the canonical action_log schema.

    If ``drop_column`` is supplied, that column is omitted — used to assert
    that ``assert_action_log_schema`` fails LOUDLY on drift.
    """
    columns = [(name, typ) for name, typ in EXPECTED_COLUMNS_TUPLES if name != drop_column]
    ddl_cols = ", ".join(f"{name} {typ}" for name, typ in columns)
    db_path = tmp_path / "events.db"
    con = sqlite3.connect(db_path)
    con.execute(f"CREATE TABLE action_log ({ddl_cols})")
    con.commit()
    con.close()
    return db_path


# ---------------------------------------------------------------------------
# 1. Module + symbol presence
# ---------------------------------------------------------------------------

def test_mirror_sink_module_exposes_required_symbols():
    module = _load_mirror_sink_module()
    for attr in (
        "EXPECTED_ACTION_LOG_COLUMNS",
        "assert_action_log_schema",
        "classify_error",
        "read_last_line_mirror_id",
        "read_watermark",
    ):
        assert hasattr(module, attr), (
            f"c3-mirror-sink.py is missing required symbol {attr!r} — "
            "T6_0 contract violation."
        )


def test_expected_action_log_columns_matches_documented_9_columns():
    module = _load_mirror_sink_module()
    actual = [tuple(item) for item in module.EXPECTED_ACTION_LOG_COLUMNS]
    assert actual == EXPECTED_COLUMNS_TUPLES, (
        "EXPECTED_ACTION_LOG_COLUMNS must mirror the documented 9-column "
        "action_log schema (V0.29) in order."
    )


# ---------------------------------------------------------------------------
# 2. assert_action_log_schema(con) — exits 1 on drift, returns on match
# ---------------------------------------------------------------------------

def test_assert_action_log_schema_passes_on_canonical_schema(tmp_path):
    module = _load_mirror_sink_module()
    db_path = _make_action_log_db(tmp_path)
    con = sqlite3.connect(db_path)
    try:
        # Should NOT raise; return value is unimportant but exit must not fire.
        module.assert_action_log_schema(con)
    finally:
        con.close()


def test_assert_action_log_schema_exits_one_on_drift(tmp_path):
    module = _load_mirror_sink_module()
    db_path = _make_action_log_db(tmp_path, drop_column="error_message")
    con = sqlite3.connect(db_path)
    try:
        with pytest.raises(SystemExit) as excinfo:
            module.assert_action_log_schema(con)
        assert excinfo.value.code == 1, (
            "assert_action_log_schema must sys.exit(1) on schema drift per S6 "
            "(no quiet failures)."
        )
    finally:
        con.close()


# ---------------------------------------------------------------------------
# 3. classify_error — controlled vocabulary matrix
# ---------------------------------------------------------------------------

@pytest.mark.parametrize(
    "status,error_message,returncode,expected",
    [
        ("ok",     None,                                       0,    None),
        ("error",  "BrokenPipeError: [Errno 32] Broken pipe",  None, "io_error"),
        ("error",  "subprocess.TimeoutExpired: timed out",     None, "timeout"),
        ("error",  "/bin/sh: nope: command not found",         127,  "command_not_found"),
        ("error",  "exited with status 2",                     2,    "non_zero_exit"),
        ("error",  "something weird happened",                 None, "unknown"),
    ],
)
def test_classify_error_returns_controlled_vocabulary(status, error_message, returncode, expected):
    module = _load_mirror_sink_module()
    result = module.classify_error(status, error_message, returncode)
    if expected is None:
        assert result is None, (
            "classify_error must return None on success (status='ok') so "
            "outcome.error_class is null in the JSONL line."
        )
    else:
        assert result in VALID_ERROR_CLASSES, (
            f"classify_error returned {result!r} which is NOT in the "
            f"controlled vocabulary {VALID_ERROR_CLASSES}."
        )
        assert result == expected, (
            f"classify_error({status!r}, {error_message!r}, {returncode!r}) "
            f"expected {expected!r}, got {result!r}."
        )


# ---------------------------------------------------------------------------
# 4. read_last_line_mirror_id — JSONL tail parsing
# ---------------------------------------------------------------------------

def test_read_last_line_mirror_id_returns_last_id(tmp_path):
    module = _load_mirror_sink_module()
    jsonl = tmp_path / "2026-05-24.jsonl"
    lines = [
        {"mirror_id": 100, "ts": "2026-05-24T12:00:00Z"},
        {"mirror_id": 101, "ts": "2026-05-24T12:00:01Z"},
        {"mirror_id": 102, "ts": "2026-05-24T12:00:02Z"},
    ]
    jsonl.write_text("\n".join(json.dumps(r) for r in lines) + "\n", encoding="utf-8")
    assert module.read_last_line_mirror_id(jsonl) == 102, (
        "read_last_line_mirror_id must return the mirror_id from the LAST "
        "JSONL line so post-crash resume sees the highest already-written id."
    )


def test_read_last_line_mirror_id_returns_none_for_missing_file(tmp_path):
    module = _load_mirror_sink_module()
    missing = tmp_path / "no-such-file.jsonl"
    # Per amended contract §4.4: a missing file means no prior progress today;
    # the helper must NOT crash — it should return None so the caller can fall
    # back to the watermark file or cold-start MAX(action_log.id).
    assert module.read_last_line_mirror_id(missing) is None


# ---------------------------------------------------------------------------
# 5. read_watermark — cold-start = MAX(action_log.id)
# ---------------------------------------------------------------------------

def test_read_watermark_cold_start_equals_max_action_log_id(tmp_path, monkeypatch):
    module = _load_mirror_sink_module()
    db_path = _make_action_log_db(tmp_path)
    con = sqlite3.connect(db_path)
    try:
        # Seed three rows so MAX(action_log.id) is well-defined.
        con.executemany(
            "INSERT INTO action_log (id, event_id, recipe, action_type, action_detail, "
            "status, error_message, executed_at, action_result) VALUES "
            "(?, ?, ?, ?, ?, ?, ?, ?, ?)",
            [
                (10, 1, "r", "shell", "{}", "ok", None, "2026-05-24T00:00:00Z", "{}"),
                (11, 1, "r", "shell", "{}", "ok", None, "2026-05-24T00:00:01Z", "{}"),
                (42, 2, "r", "shell", "{}", "ok", None, "2026-05-24T00:00:02Z", "{}"),
            ],
        )
        con.commit()

        # Point watermark-file location at an empty dir so this is a cold start.
        cold_dir = tmp_path / "mirror"
        cold_dir.mkdir()
        monkeypatch.setenv("HEX_C3_MIRROR_DIR", str(cold_dir))

        watermark = module.read_watermark(con)
        assert watermark == 42, (
            "Cold-start watermark must initialise to MAX(action_log.id) per "
            "amended contract §4 — forward-only, no historical backfill."
        )
    finally:
        con.close()
