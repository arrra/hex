"""Red tests for Task Tcpd22fqb — M3 audit-stream completeness collector.

Target script:  ``system/scripts/c3-audit-completeness.py``
Plan reference: §Task 3 of
    docs/superpowers/plans/2026-05-24-iii-hex-instrumentation-v4-final.md

The collector globs the canonical audit JSONL files
(``~/hex/.hex/audit/actions.jsonl`` and the per-agent worktree mirrors at
``~/hex/.claude/worktrees/agent-*/.hex/audit/actions.jsonl``), parses both
``Z`` and ``+00:00`` timestamp formats, groups records by ``(agent, date)``
and emits the completeness ratio:

    ratio = (wake-complete + wake-skip + wake-skip-llm) / wake-start

When ``wake-start`` is zero, status is ``no_data`` and ratio is ``None``
(NULL in the emitted JSON) per plan §3.1.

These tests deliberately FAIL today because
``system/scripts/c3-audit-completeness.py`` does not exist yet. They will
go green once the collector is implemented in the write_green phase.
"""

from __future__ import annotations

import importlib.util
import sys
from datetime import datetime, timezone
from pathlib import Path

import pytest


REPO_ROOT = Path(__file__).resolve().parents[2]
AUDIT_COMPLETENESS_PATH = (
    REPO_ROOT / "system" / "scripts" / "c3-audit-completeness.py"
)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _load_audit_completeness_module():
    """Import ``c3-audit-completeness.py`` as a module despite the hyphen."""
    if not AUDIT_COMPLETENESS_PATH.is_file():
        pytest.fail(
            "system/scripts/c3-audit-completeness.py is missing at "
            f"{AUDIT_COMPLETENESS_PATH}. Task Tcpd22fqb must create this file "
            "with the M3 collector helpers (parse_audit_ts, summarize_completeness)."
        )
    spec = importlib.util.spec_from_file_location(
        "c3_audit_completeness", AUDIT_COMPLETENESS_PATH
    )
    assert spec and spec.loader, (
        "failed to build importlib spec for c3-audit-completeness.py"
    )
    module = importlib.util.module_from_spec(spec)
    sys.modules["c3_audit_completeness"] = module
    spec.loader.exec_module(module)
    return module


def _wake_record(ts: str, agent: str, action: str) -> dict:
    """Mint a minimal audit record matching the live actions.jsonl shape."""
    return {"ts": ts, "agent": agent, "action": action, "detail": {}}


# ---------------------------------------------------------------------------
# 3.1 — module imports and exposes the documented helper symbols
# ---------------------------------------------------------------------------

def test_module_exposes_required_symbols():
    module = _load_audit_completeness_module()
    for attr in ("parse_audit_ts", "summarize_completeness"):
        assert hasattr(module, attr), (
            "c3-audit-completeness.py is missing required symbol "
            f"{attr!r} — M3 collector contract violation. The script must "
            "expose pure helpers so unit tests can exercise ratio/ts logic "
            "without touching the live audit stream."
        )


# ---------------------------------------------------------------------------
# 3.2 — parse_audit_ts accepts the trailing-Z UTC format
# ---------------------------------------------------------------------------

def test_parse_audit_ts_accepts_trailing_z_format():
    module = _load_audit_completeness_module()
    parsed = module.parse_audit_ts("2026-05-24T12:34:56Z")
    expected = datetime(2026, 5, 24, 12, 34, 56, tzinfo=timezone.utc)
    assert isinstance(parsed, datetime), (
        "parse_audit_ts must return a datetime so groupby-date logic is "
        "straightforward; got "
        f"{type(parsed).__name__}."
    )
    assert parsed.tzinfo is not None, (
        "parse_audit_ts must return a timezone-aware datetime; naive "
        "datetimes silently miscompare across DST/UTC boundaries."
    )
    assert parsed == expected, (
        f"parse_audit_ts('2026-05-24T12:34:56Z') expected {expected!r}, "
        f"got {parsed!r}."
    )


# ---------------------------------------------------------------------------
# 3.3 — parse_audit_ts accepts the +00:00 ISO offset format
# ---------------------------------------------------------------------------

def test_parse_audit_ts_accepts_plus_offset_format():
    module = _load_audit_completeness_module()
    parsed = module.parse_audit_ts("2026-05-24T12:34:56+00:00")
    expected = datetime(2026, 5, 24, 12, 34, 56, tzinfo=timezone.utc)
    assert parsed == expected, (
        "parse_audit_ts must treat '...+00:00' and '...Z' as the same UTC "
        f"instant. Got {parsed!r} for '...+00:00' vs expected {expected!r}. "
        "Both formats appear in the live audit stream per plan §3.1."
    )


# ---------------------------------------------------------------------------
# 3.4 — wake-skip is counted in the denominator alongside wake-complete
# ---------------------------------------------------------------------------

def test_summarize_completeness_counts_wake_skip_in_denominator():
    module = _load_audit_completeness_module()
    records = [
        _wake_record("2026-05-24T00:00:00Z", "hex-ops", "wake-start"),
        _wake_record("2026-05-24T00:00:01Z", "hex-ops", "wake-start"),
        _wake_record("2026-05-24T00:00:02Z", "hex-ops", "wake-start"),
        _wake_record("2026-05-24T00:00:03Z", "hex-ops", "wake-start"),
        _wake_record("2026-05-24T00:01:00Z", "hex-ops", "wake-complete"),
        _wake_record("2026-05-24T00:01:01Z", "hex-ops", "wake-complete"),
        _wake_record("2026-05-24T00:01:02Z", "hex-ops", "wake-skip"),
        _wake_record("2026-05-24T00:01:03Z", "hex-ops", "wake-skip"),
    ]
    summary = module.summarize_completeness(records)
    # The summary should bucket (hex-ops, 2026-05-24) with 4 starts and 4 ended,
    # giving ratio == 1.0 because wake-skip MUST count alongside wake-complete.
    bucket = _extract_single_bucket(summary, agent="hex-ops", date="2026-05-24")
    assert bucket["started"] == 4, (
        f"started count wrong: expected 4, got {bucket['started']!r}. "
        "wake-start must increment the denominator."
    )
    assert bucket["ended"] == 4, (
        f"ended count wrong: expected 4 (2 complete + 2 skip), got "
        f"{bucket['ended']!r}. wake-skip MUST be counted as an ended outcome "
        "per plan §3.1 — otherwise the ratio under-reports a healthy agent "
        "that legitimately skipped wakes."
    )
    assert bucket["status"] == "ok", (
        f"status wrong: expected 'ok' (started > 0), got {bucket['status']!r}."
    )
    assert bucket["ratio"] == pytest.approx(1.0), (
        f"ratio wrong: expected 1.0, got {bucket['ratio']!r}. "
        "Denominator includes wake-skip."
    )


# ---------------------------------------------------------------------------
# 3.5 — status='no_data' with ratio=None when no wake-start records present
# ---------------------------------------------------------------------------

def test_summarize_completeness_no_data_when_started_zero():
    module = _load_audit_completeness_module()
    # Two completes with NO matching wake-start (audit gap / data shift).
    records = [
        _wake_record("2026-05-24T00:00:00Z", "hex-ops", "wake-complete"),
        _wake_record("2026-05-24T00:00:01Z", "hex-ops", "wake-complete"),
    ]
    summary = module.summarize_completeness(records)
    bucket = _extract_single_bucket(summary, agent="hex-ops", date="2026-05-24")
    assert bucket["started"] == 0, (
        f"started count wrong: expected 0, got {bucket['started']!r}."
    )
    assert bucket["status"] == "no_data", (
        "When wake-start count is zero, status MUST be 'no_data' per plan "
        f"§3.1 to distinguish 'no activity' from 'broken pipeline'. Got "
        f"{bucket['status']!r}."
    )
    assert bucket["ratio"] is None, (
        "When wake-start count is zero, ratio MUST be None (emitted as JSON "
        "null) per plan §3.1 — dividing by zero must NOT produce a synthetic "
        f"100% or NaN. Got {bucket['ratio']!r}."
    )


# ---------------------------------------------------------------------------
# 3.6 — wake-skip-llm MUST count in the denominator (v4 fix)
# ---------------------------------------------------------------------------

def test_summarize_completeness_counts_wake_skip_llm_in_denominator():
    module = _load_audit_completeness_module()
    records = [
        _wake_record("2026-05-24T00:00:00Z", "hex-ops", "wake-start"),
        _wake_record("2026-05-24T00:00:01Z", "hex-ops", "wake-start"),
        _wake_record("2026-05-24T00:00:02Z", "hex-ops", "wake-start"),
        _wake_record("2026-05-24T00:00:03Z", "hex-ops", "wake-start"),
        _wake_record("2026-05-24T00:01:00Z", "hex-ops", "wake-complete"),
        _wake_record("2026-05-24T00:01:01Z", "hex-ops", "wake-complete"),
        _wake_record("2026-05-24T00:01:02Z", "hex-ops", "wake-skip"),
        _wake_record("2026-05-24T00:01:03Z", "hex-ops", "wake-skip-llm"),
    ]
    summary = module.summarize_completeness(records)
    bucket = _extract_single_bucket(summary, agent="hex-ops", date="2026-05-24")
    assert bucket["started"] == 4, (
        f"started count wrong: expected 4, got {bucket['started']!r}."
    )
    assert bucket["ended"] == 4, (
        f"ended count wrong: expected 4 (2 complete + 1 skip + 1 skip-llm), "
        f"got {bucket['ended']!r}. wake-skip-llm MUST count alongside "
        "wake-complete and wake-skip per plan §3.1 (v4 fix) — otherwise "
        "LLM-budget-gated wakes would falsely depress the completeness ratio."
    )
    assert bucket["ratio"] == pytest.approx(1.0), (
        f"ratio wrong: expected 1.0 (all 4 starts ended), got "
        f"{bucket['ratio']!r}. wake-skip-llm is in the denominator."
    )


# ---------------------------------------------------------------------------
# Internal: locate the (agent, date) bucket regardless of return shape
# ---------------------------------------------------------------------------

def _extract_single_bucket(summary, *, agent: str, date: str) -> dict:
    """Find the row for (agent, date) in summarize_completeness's output.

    The collector may return either:
      * dict keyed by ``(agent, date)`` tuple → ``{(agent, date): {...}}``
      * dict keyed by ``"agent|date"`` string
      * list of dicts each carrying ``agent`` + ``date`` fields

    Any of those shapes is acceptable; the tests just need to find the row.
    """
    if isinstance(summary, dict):
        # Tuple-keyed dict.
        if (agent, date) in summary:
            return summary[(agent, date)]
        # String-keyed dict.
        for key, value in summary.items():
            if isinstance(key, str) and agent in key and date in key:
                return value
        # Nested {agent: {date: row}}.
        if agent in summary and isinstance(summary[agent], dict):
            inner = summary[agent]
            if date in inner:
                return inner[date]
        pytest.fail(
            f"summarize_completeness returned a dict but no entry matched "
            f"agent={agent!r} date={date!r}. keys={list(summary.keys())!r}"
        )
    elif isinstance(summary, list):
        for row in summary:
            row_agent = row.get("agent")
            row_date = row.get("date")
            if row_agent == agent and (
                row_date == date or str(row_date).startswith(date)
            ):
                return row
        pytest.fail(
            f"summarize_completeness returned a list but no row matched "
            f"agent={agent!r} date={date!r}. rows={summary!r}"
        )
    else:
        pytest.fail(
            "summarize_completeness must return a dict or list, got "
            f"{type(summary).__name__}."
        )
    raise AssertionError("unreachable")  # pragma: no cover
