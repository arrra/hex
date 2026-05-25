"""Red tests for Task T215w7yv6 — M1 orphan-detection rate collector.

Target: ``system/scripts/c3-orphan-scan.py`` (does NOT exist yet — these
tests fail today by design and go green when Task T215w7yv6 lands the
collector per the v4-final plan §Task 2).

Behaviour pinned by the spec contract:

    * Scans four scopes:
        1. ``$HOME/.hex-events/policies/*.yaml``
        2. ``$HOME/hex/projects/*/charter.yaml``
        3. ``$HOME/.boi/v2/boi.db`` table ``spec_versions``
        4. ``$HOME/github.com/mrap/boi/specs/*.yaml``
    * Emits ``hex.c3.orphan.scan`` via ``hex-emit.sh`` with per-scope
      breakdowns of scanned vs broken refs.
    * Cold-start contract: when the BOI v2 snapshot DB has zero
      ``spec_versions`` rows, the emitted payload's ``warnings`` array
      MUST contain ``"boi_v2_specs_empty"`` (cold-start signal, NOT a
      crash — the DB is genuinely fresh).

To keep the test deterministic and free of side effects on the
real user's ``$HOME``, we run the script as a subprocess against a
fully isolated tmp HOME and ask for the report on stdout via a
``--json`` (or equivalent dry-run) flag. The collector MUST support a
non-emitting mode so operators and tests can sanity-check the payload
without firing real telemetry — this is also what the C3 plan §2.2
fixtures rely on.
"""

from __future__ import annotations

import json
import os
import sqlite3
import subprocess
import sys
from pathlib import Path

import pytest


REPO_ROOT = Path(__file__).resolve().parents[2]
ORPHAN_SCAN_PATH = REPO_ROOT / "system" / "scripts" / "c3-orphan-scan.py"

# Scope keys the collector must report a per-scope breakdown for. Each
# scope's breakdown is at minimum a count of refs scanned and a count of
# broken refs found (precise sub-key naming is left to the implementer,
# but these scope buckets MUST be present so the M1 rate is computable
# per-source).
EXPECTED_SCOPES = {
    "hex_events_policies",
    "hex_project_charters",
    "boi_v2_specs",
    "boi_repo_specs",
}


# ---------------------------------------------------------------------------
# Fixture helpers
# ---------------------------------------------------------------------------

def _build_fake_home(tmp_path: Path, *, seed_boi_v2: bool, planted_broken_policy: bool) -> Path:
    """Build a minimal fake $HOME with the four scan-scope locations.

    ``seed_boi_v2`` controls whether the BOI v2 snapshot DB has any rows in
    ``spec_versions`` — when False, the collector must emit the cold-start
    ``boi_v2_specs_empty`` warning.

    ``planted_broken_policy`` controls whether the policies dir contains a
    yaml file with a deliberately broken ref (a recipe/action target that
    points at a path the collector cannot resolve).
    """
    home = tmp_path / "home"
    (home / ".hex-events" / "policies").mkdir(parents=True)
    (home / "hex" / "projects" / "demo").mkdir(parents=True)
    (home / ".boi" / "v2").mkdir(parents=True)
    (home / "github.com" / "mrap" / "boi" / "specs").mkdir(parents=True)

    # 1. Always-valid baseline policy — gives the collector a non-empty
    #    scope so it must distinguish "scanned" from "broken" rather than
    #    short-circuiting on an empty dir.
    (home / ".hex-events" / "policies" / "ok.yaml").write_text(
        "name: ok\n"
        "description: baseline ok policy\n"
        "trigger:\n"
        "  event: timer.tick.hourly\n"
        "action:\n"
        "  type: shell\n"
        "  command: 'true'\n",
        encoding="utf-8",
    )

    if planted_broken_policy:
        # A policy whose `action.command` references a script path that
        # cannot be resolved from $HOME — this is the canonical "broken
        # ref" the M1 collector must count.
        (home / ".hex-events" / "policies" / "broken.yaml").write_text(
            "name: broken\n"
            "description: deliberately broken ref for the M1 fixture\n"
            "trigger:\n"
            "  event: timer.tick.hourly\n"
            "action:\n"
            "  type: shell\n"
            "  command: 'bash $HOME/.hex/scripts/this-script-does-not-exist.sh'\n",
            encoding="utf-8",
        )

    # 2. Minimal valid charter so the project-charters scope is exercised.
    (home / "hex" / "projects" / "demo" / "charter.yaml").write_text(
        "name: demo\n"
        "owner: c3-test\n"
        "status: active\n",
        encoding="utf-8",
    )

    # 3. BOI v2 snapshot DB — present always (cold start = empty rows,
    #    not missing file).
    boi_db = home / ".boi" / "v2" / "boi.db"
    con = sqlite3.connect(boi_db)
    try:
        # Minimal spec_versions shape — id + spec slug + path so the
        # collector has something to validate broken-ref logic against.
        con.execute(
            "CREATE TABLE spec_versions ("
            "id INTEGER PRIMARY KEY, "
            "spec_id TEXT NOT NULL, "
            "path TEXT NOT NULL, "
            "created_at TEXT NOT NULL"
            ")"
        )
        if seed_boi_v2:
            con.execute(
                "INSERT INTO spec_versions (spec_id, path, created_at) VALUES (?, ?, ?)",
                ("demo-spec", str(home / "github.com" / "mrap" / "boi" / "specs" / "demo.yaml"),
                 "2026-05-24T00:00:00Z"),
            )
            # And land the matching boi/specs file so the ref resolves.
            (home / "github.com" / "mrap" / "boi" / "specs" / "demo.yaml").write_text(
                "title: demo-spec\nmode: execute\ntasks: []\n",
                encoding="utf-8",
            )
        con.commit()
    finally:
        con.close()

    return home


def _run_collector(home: Path) -> dict:
    """Invoke c3-orphan-scan.py against the fake HOME and return the
    parsed JSON report. The collector MUST support a non-emitting
    JSON-on-stdout mode for operator / test usage; we try ``--json``
    first and fall back to ``--dry-run`` so the implementer has room.
    """
    if not ORPHAN_SCAN_PATH.is_file():
        pytest.fail(
            f"system/scripts/c3-orphan-scan.py missing at {ORPHAN_SCAN_PATH}. "
            "Task T215w7yv6 must land this collector per plan §2.1."
        )

    env = {**os.environ, "HOME": str(home)}
    # Strip any caller-side HEX_DIR override so the collector resolves
    # paths from the fake HOME we just built, not the worker's real hex.
    env.pop("HEX_DIR", None)
    env.pop("HEX_ROOT", None)
    # Point telemetry emission at a tmp dir so the collector does not
    # write to real telemetry when run from CI / dev machines.
    env["HEX_C3_ORPHAN_SCAN_DRY_RUN"] = "1"

    for flag in ("--json", "--dry-run"):
        result = subprocess.run(
            [sys.executable, str(ORPHAN_SCAN_PATH), flag],
            env=env,
            capture_output=True,
            text=True,
            timeout=30,
        )
        if result.returncode == 0 and result.stdout.strip():
            try:
                return json.loads(result.stdout)
            except json.JSONDecodeError:
                continue
    pytest.fail(
        "c3-orphan-scan.py did not return a parseable JSON report under "
        "--json or --dry-run. stdout:\n"
        f"{result.stdout!r}\nstderr:\n{result.stderr!r}"
    )


# ---------------------------------------------------------------------------
# 1. Module presence
# ---------------------------------------------------------------------------

def test_orphan_scan_script_exists():
    assert ORPHAN_SCAN_PATH.is_file(), (
        f"Expected collector at {ORPHAN_SCAN_PATH}. Task T215w7yv6 must "
        "land system/scripts/c3-orphan-scan.py per plan §2.1."
    )


# ---------------------------------------------------------------------------
# 2. Per-scope breakdown shape (M1 contract)
# ---------------------------------------------------------------------------

def test_scan_report_has_per_scope_breakdown(tmp_path):
    home = _build_fake_home(tmp_path, seed_boi_v2=True, planted_broken_policy=False)
    report = _run_collector(home)

    # The collector emits hex.c3.orphan.scan; the JSON report on stdout
    # should mirror that event's payload (or wrap it). Either way, the
    # per-scope breakdown must be reachable from the top-level object.
    scopes = report.get("per_scope") or report.get("scopes") or {}
    assert isinstance(scopes, dict) and scopes, (
        "M1 collector must report a non-empty per-scope breakdown under "
        "'per_scope' (or 'scopes'). Got keys=%r" % list(report.keys())
    )
    missing = EXPECTED_SCOPES - set(scopes.keys())
    assert not missing, (
        "M1 per-scope breakdown is missing required scope keys: "
        f"{missing}. Plan §2.1 lists four scopes — policies, charters, "
        "boi_v2_specs, boi_repo_specs."
    )


# ---------------------------------------------------------------------------
# 3. Cold-start: empty BOI v2 spec_versions → boi_v2_specs_empty warning
# ---------------------------------------------------------------------------

def test_cold_start_warning_when_boi_v2_db_empty(tmp_path):
    home = _build_fake_home(tmp_path, seed_boi_v2=False, planted_broken_policy=False)
    report = _run_collector(home)

    warnings = report.get("warnings")
    assert isinstance(warnings, list), (
        "M1 collector must surface a top-level 'warnings' array per the "
        "cold-start contract; got %r" % (warnings,)
    )
    assert "boi_v2_specs_empty" in warnings, (
        "When ~/.boi/v2/boi.db has zero rows in spec_versions, the "
        "collector MUST emit the literal cold-start warning "
        "'boi_v2_specs_empty' (NOT crash, NOT silently zero). Got "
        f"warnings={warnings!r}."
    )


# ---------------------------------------------------------------------------
# 4. Broken-ref detection in the policies scope
# ---------------------------------------------------------------------------

def test_broken_policy_reference_is_counted(tmp_path):
    home = _build_fake_home(tmp_path, seed_boi_v2=True, planted_broken_policy=True)
    report = _run_collector(home)

    scopes = report.get("per_scope") or report.get("scopes") or {}
    policies = scopes.get("hex_events_policies") or {}
    # Accept either 'broken' or 'orphans' as the broken-ref count key —
    # the spec contract requires it be reported, not its exact name.
    broken = (
        policies.get("broken")
        if isinstance(policies, dict) and "broken" in policies
        else policies.get("orphans") if isinstance(policies, dict) else None
    )
    assert isinstance(broken, int) and broken >= 1, (
        "Planted a hex-events policy whose action.command references a "
        "script path that does not exist; M1 collector must count this "
        "as a broken ref under hex_events_policies. Got policies scope="
        f"{policies!r}."
    )
