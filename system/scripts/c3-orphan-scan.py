#!/usr/bin/env python3
"""C3 / M1 — orphan-detection rate collector.

Scans four scopes for broken references and emits one
``hex.c3.orphan.scan`` event per run via ``hex-emit.sh`` (skipped in
dry-run mode).

Scopes (per plan §Task 2.1):

  1. ``$HOME/.hex-events/policies/*.yaml`` (hex_events_policies):
     parses each policy, walks every shell-action command, and counts
     references to files that do not exist. Common shapes captured:

         bash <path>
         python3 <path>
         <path>/some-script.sh
         "$HEX_DIR/.hex/scripts/foo.py"

     Tokens that look like file paths (contain ``/`` or end in a
     known script extension) are environment-expanded and existence-
     checked. Tokens that are not resolvable (e.g. raw shell builtins)
     are skipped — broken refs require a real path that fails to
     resolve.

  2. ``$HOME/hex/projects/*/charter.yaml`` (hex_project_charters):
     parses each charter and counts any string field whose value
     resolves to a path-like token (contains ``/``, has a known
     extension, or starts with ``$``) that does not exist after
     expansion. Minimal charters (no path fields) count as
     scanned=1, broken=0.

  3. ``$HOME/.boi/v2/boi.db`` table ``spec_versions`` (boi_v2_specs):
     counts the rows. If the table is empty, the collector emits the
     cold-start warning ``boi_v2_specs_empty`` and reports
     ``scanned=0, broken=0`` for the scope — this is NOT a crash
     (B4: forward-only semantics; a fresh DB is genuinely empty).
     When rows are present and the table exposes a ``path`` column,
     each row's path is existence-checked and counted as broken if
     missing.

  4. ``$HOME/github.com/mrap/boi/specs/*.yaml`` (boi_repo_specs):
     globs the dir and counts each parseable yaml as scanned. A file
     that fails to parse counts as broken.

Output:

  --json / --dry-run / HEX_C3_ORPHAN_SCAN_DRY_RUN=1
      Write the report as JSON to stdout, exit 0, and skip the
      ``hex-emit.sh`` invocation entirely. Used by tests and by
      operators sanity-checking the payload.

  (default)
      Compute the report, invoke ``hex-emit.sh hex.c3.orphan.scan``
      with the payload, and exit 0 (or non-zero on emit failure;
      S6: no quiet failures).
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shlex
import sqlite3
import subprocess
import sys
from pathlib import Path
from typing import Any, Dict, Iterable, List, Tuple

try:
    import yaml  # type: ignore
except ImportError as exc:  # pragma: no cover - environment guard
    print(
        "[c3-orphan-scan] ERROR: PyYAML is required to parse policies and "
        f"charters: {exc}",
        file=sys.stderr,
    )
    sys.exit(2)


EVENT_TYPE = "hex.c3.orphan.scan"
SOURCE = "c3-orphan-scan"
EMIT_TIMEOUT_SECONDS = 10

SCOPE_POLICIES = "hex_events_policies"
SCOPE_CHARTERS = "hex_project_charters"
SCOPE_BOI_V2 = "boi_v2_specs"
SCOPE_BOI_REPO = "boi_repo_specs"

ALL_SCOPES = (SCOPE_POLICIES, SCOPE_CHARTERS, SCOPE_BOI_V2, SCOPE_BOI_REPO)

# Heuristic: tokens that look like file paths. We treat a token as a
# path candidate if it contains a slash OR ends with one of these
# extensions. Pure command names (``bash``, ``true``, ``python3``) do
# not qualify and are not counted as broken.
_PATH_EXT_RE = re.compile(r"\.(sh|py|yaml|yml|json|toml|sql|md|txt|cfg|ini)\Z")


# ---------------------------------------------------------------------------
# Path / env resolution
# ---------------------------------------------------------------------------

def _resolve_home() -> Path:
    """Return the effective ``$HOME``. Tests override via env."""
    home = os.environ.get("HOME")
    if not home:
        print("[c3-orphan-scan] ERROR: HOME not set", file=sys.stderr)
        sys.exit(2)
    return Path(home)


def _expand(token: str, home: Path) -> Path | None:
    """Expand a token to a Path or return ``None`` if it doesn't look
    like a real path candidate (no slash, no known extension).

    Substitutes ``$HOME``, ``${HOME}``, ``~``, and any other env-var
    references via ``os.path.expandvars``. ``$HEX_DIR`` / ``$HEX_ROOT``
    fall back to ``$HOME/hex`` if unset so we can still detect missing
    refs in policies that lean on those vars.
    """
    if not token:
        return None
    if "/" not in token and not _PATH_EXT_RE.search(token):
        return None

    env = os.environ.copy()
    env.setdefault("HEX_DIR", str(home / "hex"))
    env.setdefault("HEX_ROOT", env["HEX_DIR"])
    env.setdefault("HOME", str(home))

    # os.path.expandvars uses the process env; we want a sandboxed
    # expansion using the env we just built, so do it by hand.
    def _sub(match: re.Match[str]) -> str:
        name = match.group(1) or match.group(2)
        return env.get(name, match.group(0))

    expanded = re.sub(r"\$\{([A-Za-z_][A-Za-z0-9_]*)\}|\$([A-Za-z_][A-Za-z0-9_]*)", _sub, token)
    expanded = os.path.expanduser(expanded)
    return Path(expanded)


# ---------------------------------------------------------------------------
# Scope: hex-events policies
# ---------------------------------------------------------------------------

def _walk_commands(node: Any) -> Iterable[str]:
    """Yield every ``command:`` string value within a parsed policy tree."""
    if isinstance(node, dict):
        for key, value in node.items():
            if key == "command" and isinstance(value, str):
                yield value
            else:
                yield from _walk_commands(value)
    elif isinstance(node, list):
        for item in node:
            yield from _walk_commands(item)


def _extract_path_candidates(command: str) -> List[str]:
    """Pull path-like tokens out of a shell command string.

    Uses ``shlex.split`` (POSIX) to preserve quoted segments. Each
    token is then individually filtered by ``_expand`` (which decides
    whether the token looks pathy at all).
    """
    try:
        tokens = shlex.split(command, posix=True)
    except ValueError:
        # Unbalanced quotes etc. — keep the raw string as a single
        # token so we don't silently drop broken references hiding in
        # malformed commands.
        tokens = [command]
    return tokens


def _scan_policies(home: Path) -> Tuple[Dict[str, int], List[str]]:
    """Scan ``$HOME/.hex-events/policies/*.yaml`` for broken script refs."""
    policies_dir = home / ".hex-events" / "policies"
    scanned = 0
    broken = 0
    notes: List[str] = []

    if not policies_dir.exists():
        return {"scanned": 0, "broken": 0}, notes

    for path in sorted(policies_dir.glob("*.yaml")):
        scanned += 1
        try:
            data = yaml.safe_load(path.read_text(encoding="utf-8"))
        except yaml.YAMLError as exc:
            broken += 1
            notes.append(f"yaml_parse_error:{path.name}:{exc.__class__.__name__}")
            continue
        if not isinstance(data, dict):
            # Empty or non-mapping policy is malformed; count as broken.
            broken += 1
            notes.append(f"malformed_policy:{path.name}")
            continue
        for command in _walk_commands(data):
            for token in _extract_path_candidates(command):
                candidate = _expand(token, home)
                if candidate is None:
                    continue
                # Only flag absolute paths or paths that look home-rooted;
                # relative tokens (``./foo``) are best-effort skipped.
                if not candidate.is_absolute():
                    continue
                if not candidate.exists():
                    broken += 1
                    notes.append(f"missing_path:{path.name}:{candidate}")
    return {"scanned": scanned, "broken": broken}, notes


# ---------------------------------------------------------------------------
# Scope: project charters
# ---------------------------------------------------------------------------

def _walk_strings(node: Any) -> Iterable[str]:
    if isinstance(node, dict):
        for value in node.values():
            yield from _walk_strings(value)
    elif isinstance(node, list):
        for item in node:
            yield from _walk_strings(item)
    elif isinstance(node, str):
        yield node


def _scan_charters(home: Path) -> Tuple[Dict[str, int], List[str]]:
    """Scan ``$HOME/hex/projects/*/charter.yaml`` for broken path refs."""
    projects_dir = home / "hex" / "projects"
    scanned = 0
    broken = 0
    notes: List[str] = []

    if not projects_dir.exists():
        return {"scanned": 0, "broken": 0}, notes

    for charter in sorted(projects_dir.glob("*/charter.yaml")):
        scanned += 1
        try:
            data = yaml.safe_load(charter.read_text(encoding="utf-8"))
        except yaml.YAMLError as exc:
            broken += 1
            notes.append(f"yaml_parse_error:{charter}:{exc.__class__.__name__}")
            continue
        if data is None:
            continue
        for value in _walk_strings(data):
            candidate = _expand(value, home)
            if candidate is None:
                continue
            if not candidate.is_absolute():
                continue
            if not candidate.exists():
                broken += 1
                notes.append(f"missing_path:{charter.name}:{candidate}")
    return {"scanned": scanned, "broken": broken}, notes


# ---------------------------------------------------------------------------
# Scope: BOI v2 snapshot DB
# ---------------------------------------------------------------------------

def _scan_boi_v2_specs(home: Path) -> Tuple[Dict[str, int], List[str], List[str]]:
    """Scan the BOI v2 snapshot DB. Returns ``(stats, warnings, notes)``.

    Cold-start (zero rows) → ``warnings = ["boi_v2_specs_empty"]`` per
    plan §2.1 / B4 forward-only semantics. Missing DB file is ALSO a
    cold-start signal — the snapshot DB may simply not be initialised
    yet on a fresh box.
    """
    db_path = home / ".boi" / "v2" / "boi.db"
    warnings: List[str] = []
    notes: List[str] = []

    if not db_path.exists():
        warnings.append("boi_v2_specs_empty")
        notes.append(f"missing_db:{db_path}")
        return {"scanned": 0, "broken": 0}, warnings, notes

    try:
        con = sqlite3.connect(str(db_path))
    except sqlite3.Error as exc:
        notes.append(f"db_open_error:{exc}")
        return {"scanned": 0, "broken": 0}, warnings, notes
    try:
        cur = con.cursor()
        try:
            cur.execute(
                "SELECT name FROM sqlite_master WHERE type='table' AND name='spec_versions'"
            )
        except sqlite3.Error as exc:
            notes.append(f"db_query_error:{exc}")
            return {"scanned": 0, "broken": 0}, warnings, notes
        if cur.fetchone() is None:
            warnings.append("boi_v2_specs_empty")
            notes.append("missing_table:spec_versions")
            return {"scanned": 0, "broken": 0}, warnings, notes

        cur.execute("PRAGMA table_info(spec_versions)")
        cols = {row[1] for row in cur.fetchall()}

        cur.execute("SELECT COUNT(*) FROM spec_versions")
        total = int(cur.fetchone()[0])
        if total == 0:
            warnings.append("boi_v2_specs_empty")
            return {"scanned": 0, "broken": 0}, warnings, notes

        broken = 0
        if "path" in cols:
            cur.execute("SELECT path FROM spec_versions")
            for (raw,) in cur.fetchall():
                if not isinstance(raw, str) or not raw:
                    broken += 1
                    notes.append("null_path_row")
                    continue
                candidate = _expand(raw, home) or Path(raw)
                if not candidate.is_absolute():
                    continue
                if not candidate.exists():
                    broken += 1
                    notes.append(f"missing_path:{candidate}")
        return {"scanned": total, "broken": broken}, warnings, notes
    finally:
        con.close()


# ---------------------------------------------------------------------------
# Scope: boi repo specs
# ---------------------------------------------------------------------------

def _scan_boi_repo_specs(home: Path) -> Tuple[Dict[str, int], List[str]]:
    """Scan ``$HOME/github.com/mrap/boi/specs/*.yaml`` for parse errors."""
    specs_dir = home / "github.com" / "mrap" / "boi" / "specs"
    scanned = 0
    broken = 0
    notes: List[str] = []
    if not specs_dir.exists():
        return {"scanned": 0, "broken": 0}, notes
    for path in sorted(specs_dir.glob("*.yaml")):
        scanned += 1
        try:
            data = yaml.safe_load(path.read_text(encoding="utf-8"))
        except yaml.YAMLError as exc:
            broken += 1
            notes.append(f"yaml_parse_error:{path.name}:{exc.__class__.__name__}")
            continue
        if data is None:
            broken += 1
            notes.append(f"empty_spec:{path.name}")
    return {"scanned": scanned, "broken": broken}, notes


# ---------------------------------------------------------------------------
# Build report
# ---------------------------------------------------------------------------

def build_report(home: Path) -> Dict[str, Any]:
    per_scope: Dict[str, Dict[str, int]] = {}
    notes: List[str] = []
    warnings: List[str] = []

    stats, scope_notes = _scan_policies(home)
    per_scope[SCOPE_POLICIES] = stats
    notes.extend(scope_notes)

    stats, scope_notes = _scan_charters(home)
    per_scope[SCOPE_CHARTERS] = stats
    notes.extend(scope_notes)

    stats, scope_warnings, scope_notes = _scan_boi_v2_specs(home)
    per_scope[SCOPE_BOI_V2] = stats
    warnings.extend(scope_warnings)
    notes.extend(scope_notes)

    stats, scope_notes = _scan_boi_repo_specs(home)
    per_scope[SCOPE_BOI_REPO] = stats
    notes.extend(scope_notes)

    total_scanned = sum(s["scanned"] for s in per_scope.values())
    total_broken = sum(s["broken"] for s in per_scope.values())
    orphan_rate = (total_broken / total_scanned) if total_scanned else 0.0

    return {
        "event_type": EVENT_TYPE,
        "source": SOURCE,
        "per_scope": per_scope,
        "totals": {
            "scanned": total_scanned,
            "broken": total_broken,
            "orphan_rate": orphan_rate,
        },
        "warnings": warnings,
        "notes": notes,
    }


# ---------------------------------------------------------------------------
# Emit
# ---------------------------------------------------------------------------

def _emit(report: Dict[str, Any]) -> int:
    hex_root = os.environ.get("HEX_ROOT") or os.environ.get("HEX_DIR")
    if not hex_root:
        print(
            "[c3-orphan-scan] ERROR: HEX_ROOT/HEX_DIR not set; cannot locate "
            "hex-emit.sh. Refusing to silently drop telemetry (S6).",
            file=sys.stderr,
        )
        return 2
    emit_script = Path(hex_root) / ".hex" / "bin" / "hex-emit.sh"
    if not emit_script.exists():
        print(
            f"[c3-orphan-scan] ERROR: hex-emit.sh not found at {emit_script}",
            file=sys.stderr,
        )
        return 2

    # Strip the verbose notes list before emitting — keep the payload
    # small for the telemetry pipeline; operators can re-run with
    # --json to inspect raw notes if needed.
    payload = {
        "per_scope": report["per_scope"],
        "totals": report["totals"],
        "warnings": report["warnings"],
    }
    cmd = ["bash", str(emit_script), EVENT_TYPE, json.dumps(payload, sort_keys=True), SOURCE]
    env = os.environ.copy()
    env["HEX_DIR"] = hex_root
    env["HEX_ROOT"] = hex_root
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
            f"[c3-orphan-scan] ERROR: hex-emit.sh invocation raised "
            f"{type(exc).__name__}: {exc}",
            file=sys.stderr,
        )
        return 2
    if completed.returncode != 0:
        print(
            f"[c3-orphan-scan] ERROR: hex-emit.sh exited {completed.returncode}; "
            f"stderr={completed.stderr.strip()!r}",
            file=sys.stderr,
        )
        return 2
    return 0


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="C3 / M1 orphan-detection rate collector."
    )
    parser.add_argument(
        "--json",
        action="store_true",
        help="Write the report as JSON to stdout and skip telemetry emission.",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Alias for --json (operator-friendly).",
    )
    args = parser.parse_args(argv)

    dry_run = (
        args.json
        or args.dry_run
        or os.environ.get("HEX_C3_ORPHAN_SCAN_DRY_RUN") == "1"
    )

    home = _resolve_home()
    report = build_report(home)

    if dry_run:
        json.dump(report, sys.stdout, sort_keys=True)
        sys.stdout.write("\n")
        return 0

    return _emit(report)


if __name__ == "__main__":
    sys.exit(main())
