#!/usr/bin/env python3
"""
Red tests for M2 weekly quiet-failure rollup collector.

Task T3pan20r0 / plan §Task 5. Verifies:
  5.1 --force bypasses Monday-guard (script does real work and emits)
  5.2 Non-Monday UTC date → no-op, clean exit 0, ZERO emit subprocess calls
  5.3 Reads v_c3_quiet_failure_weekly VIEW; emits one
      hex.c3.quiet_failure.weekly_count event per category for the
      last completed week
  5.4 If the hex-emit.sh subprocess returns non-zero, the script exits
      LOUDLY with status 2 (S6 "no quiet failures")

These tests must FAIL now (the script does not yet exist at
system/scripts/c3-quiet-failure-snapshot.py).
"""

from __future__ import annotations

import os
import sqlite3
import subprocess
import sys
import textwrap
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPT_PATH = REPO_ROOT / "system" / "scripts" / "c3-quiet-failure-snapshot.py"


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _make_events_db(db_path: Path) -> None:
    """Create an events.db with the schema the collector expects, including
    the v_c3_quiet_failure_weekly VIEW with a couple of category rows for the
    last completed ISO week."""
    con = sqlite3.connect(str(db_path))
    cur = con.cursor()
    cur.executescript(
        """
        CREATE TABLE IF NOT EXISTS events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            event_type TEXT NOT NULL,
            payload TEXT,
            source TEXT,
            received_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
        );

        -- Minimal VIEW shape the collector reads. The real migration
        -- defines this view; the test fixture stubs it with a static
        -- result so the script's read+emit path is exercisable in isolation.
        DROP VIEW IF EXISTS v_c3_quiet_failure_weekly;
        CREATE VIEW v_c3_quiet_failure_weekly AS
        SELECT
            'malformed_yaml'    AS category, 7 AS week_count, '2026-W20' AS iso_week
        UNION ALL
        SELECT
            'unreplied_timeout' AS category, 3 AS week_count, '2026-W20' AS iso_week
        UNION ALL
        SELECT
            'silent_skip'       AS category, 0 AS week_count, '2026-W20' AS iso_week;
        """
    )
    con.commit()
    con.close()


def _make_fake_emit(bin_dir: Path, log_path: Path, exit_code: int = 0) -> Path:
    """Install a fake hex-emit.sh that logs every invocation to `log_path` and
    exits with `exit_code`. Returns the path to the fake binary."""
    bin_dir.mkdir(parents=True, exist_ok=True)
    emit_path = bin_dir / "hex-emit.sh"
    emit_path.write_text(
        textwrap.dedent(
            f"""\
            #!/usr/bin/env bash
            # Fake hex-emit.sh used by the M2 red tests.
            # Logs every call as a single TSV line: event_type<TAB>payload<TAB>source
            printf '%s\\t%s\\t%s\\n' "${{1:-}}" "${{2:-}}" "${{3:-}}" >> "{log_path}"
            exit {exit_code}
            """
        )
    )
    emit_path.chmod(0o755)
    return emit_path


def _run_script(
    hex_dir: Path,
    extra_args: list[str] | None = None,
    env_overrides: dict[str, str] | None = None,
) -> subprocess.CompletedProcess:
    env = os.environ.copy()
    env["HEX_DIR"] = str(hex_dir)
    env["HEX_ROOT"] = str(hex_dir)
    # Put the fake bin first so subprocess discovery finds it via PATH if used.
    env["PATH"] = f"{hex_dir}/.hex/bin:{env.get('PATH', '')}"
    if env_overrides:
        env.update(env_overrides)
    cmd = [sys.executable, str(SCRIPT_PATH)] + (extra_args or [])
    return subprocess.run(cmd, env=env, capture_output=True, text=True, timeout=30)


@pytest.fixture
def hex_dir(tmp_path: Path) -> Path:
    """Build a tmp HEX_DIR layout: .hex/telemetry/events.db plus .hex/bin."""
    root = tmp_path / "hex"
    (root / ".hex" / "telemetry").mkdir(parents=True)
    (root / ".hex" / "bin").mkdir(parents=True)
    _make_events_db(root / ".hex" / "telemetry" / "events.db")
    return root


# ---------------------------------------------------------------------------
# Pre-flight: the script must exist (red until implementation lands)
# ---------------------------------------------------------------------------


def test_script_exists_and_is_executable():
    assert SCRIPT_PATH.is_file(), (
        f"Expected M2 collector at {SCRIPT_PATH}; "
        "implementation not yet present (this red test asserts the file lands)."
    )


# ---------------------------------------------------------------------------
# 5.1 --force bypasses the Monday-guard and triggers real work + emits
# ---------------------------------------------------------------------------


def test_force_flag_bypasses_monday_guard_and_emits(hex_dir: Path):
    log_path = hex_dir / "emit.log"
    _make_fake_emit(hex_dir / ".hex" / "bin", log_path, exit_code=0)

    # Even though "today" might not be Monday, --force makes the script work.
    result = _run_script(hex_dir, extra_args=["--force"])

    assert result.returncode == 0, (
        f"--force run should exit 0 on successful emit; "
        f"stdout={result.stdout!r} stderr={result.stderr!r}"
    )
    assert log_path.exists(), "Expected hex-emit.sh to have been invoked at least once."
    lines = [ln for ln in log_path.read_text().splitlines() if ln.strip()]
    # 3 rows in fixture VIEW → 3 emits (one per category)
    assert len(lines) == 3, (
        f"Expected one emit per category row (3 fixture rows); got {len(lines)}: {lines!r}"
    )
    assert all(
        ln.split("\t", 1)[0] == "hex.c3.quiet_failure.weekly_count" for ln in lines
    ), f"Every emit must be event_type=hex.c3.quiet_failure.weekly_count; saw {lines!r}"


# ---------------------------------------------------------------------------
# 5.2 Monday-guard: non-Monday → clean exit 0, NO emits
# ---------------------------------------------------------------------------


def test_non_monday_is_clean_noop(hex_dir: Path, monkeypatch: pytest.MonkeyPatch):
    """Without --force, on a non-Monday UTC date, the script must exit 0
    cleanly and MUST NOT invoke hex-emit.sh.

    We pin the script's notion of 'today' to a Tuesday via an env var that
    the production script honours for testability. If the implementation does
    not honour this env var, this test will still detect the bug (an emit on
    a non-Monday is a bug regardless)."""
    log_path = hex_dir / "emit.log"
    _make_fake_emit(hex_dir / ".hex" / "bin", log_path, exit_code=0)

    # 2026-05-26 is a Tuesday UTC. Test honours either C3_TODAY or
    # SOURCE_DATE_EPOCH-style overrides — implementation MUST provide one.
    result = _run_script(
        hex_dir,
        env_overrides={"C3_TODAY": "2026-05-26"},
    )

    assert result.returncode == 0, (
        f"Non-Monday must exit 0 cleanly; got {result.returncode}, "
        f"stderr={result.stderr!r}"
    )
    if log_path.exists():
        leftover = [ln for ln in log_path.read_text().splitlines() if ln.strip()]
        assert leftover == [], (
            f"Non-Monday run must NOT emit any events; saw {leftover!r}"
        )


# ---------------------------------------------------------------------------
# 5.3 Reads v_c3_quiet_failure_weekly VIEW; emits one event per category
# ---------------------------------------------------------------------------


def test_reads_view_and_emits_one_per_category(hex_dir: Path):
    """With --force and a 3-row VIEW fixture, expect exactly 3 emits with
    correct category names and a payload that includes the count."""
    log_path = hex_dir / "emit.log"
    _make_fake_emit(hex_dir / ".hex" / "bin", log_path, exit_code=0)

    result = _run_script(hex_dir, extra_args=["--force"])
    assert result.returncode == 0, (
        f"Expected exit 0; got {result.returncode}, stderr={result.stderr!r}"
    )

    lines = [ln for ln in log_path.read_text().splitlines() if ln.strip()]
    assert len(lines) == 3, f"Expected 3 emit lines, got {len(lines)}: {lines!r}"

    # Each line is "<event_type>\t<json_payload>\t<source>".
    payloads = [ln.split("\t")[1] for ln in lines]
    joined = "\n".join(payloads)
    for category in ("malformed_yaml", "unreplied_timeout", "silent_skip"):
        assert category in joined, (
            f"Expected category {category!r} present in emitted payloads; "
            f"payloads were: {payloads!r}"
        )


# ---------------------------------------------------------------------------
# 5.4 hex-emit.sh subprocess returns non-zero → script exits 2 LOUDLY
# ---------------------------------------------------------------------------


def test_emit_failure_exits_2_loudly(hex_dir: Path):
    """If hex-emit.sh exits non-zero, the snapshot script MUST exit 2 and
    write a loud error to stderr (S6: no quiet failures)."""
    log_path = hex_dir / "emit.log"
    # Fake emit returns failure code on every call.
    _make_fake_emit(hex_dir / ".hex" / "bin", log_path, exit_code=1)

    result = _run_script(hex_dir, extra_args=["--force"])

    # Sanity: the script must have actually run far enough to invoke the
    # fake hex-emit.sh at least once. Otherwise a missing-script error
    # would coincidentally satisfy `returncode == 2` for the wrong reason.
    assert log_path.exists(), (
        "Fake hex-emit.sh was never invoked. The script must reach the emit "
        "step before raising the loud failure (current stderr indicates the "
        f"script may not even exist): stderr={result.stderr!r}"
    )
    invocations = [ln for ln in log_path.read_text().splitlines() if ln.strip()]
    assert len(invocations) >= 1, (
        f"Expected the script to attempt at least one emit; log was {invocations!r}"
    )

    assert result.returncode == 2, (
        f"Emit-failure must surface as exit 2; got {result.returncode}. "
        f"stdout={result.stdout!r} stderr={result.stderr!r}"
    )
    assert result.stderr.strip() != "", (
        "Emit failure must produce a loud stderr message (S6 no quiet failures); "
        "stderr was empty."
    )
