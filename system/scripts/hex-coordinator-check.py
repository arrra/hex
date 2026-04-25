#!/usr/bin/env python3
"""
hex-coordinator-check.py — Conflict and dependency checker for BOI specs.

Checks a spec file against the active-work manifest for file-path conflicts
and unmet dependencies.

Usage:
  hex-coordinator-check.py <spec-file> [--manifest PATH] [--json]
  hex-coordinator-check.py --help

Exit codes:
  0  CLEAR — no conflicts, all dependencies met
  1  CONFLICT — overlapping file found in manifest
  2  BLOCKED — dependency signal references unfinished spec/feature
  3  ERROR — spec file not found or manifest unreadable
"""

import argparse
import json
import os
import re
import sys
from datetime import datetime, timezone
from pathlib import Path

DEFAULT_MANIFEST = Path.home() / ".hex" / "audit" / "active-work-manifest.jsonl"

# File-path regex: matches relative or short absolute paths with known extensions
FILE_PATH_RE = re.compile(
    r"""
    (?:^|[\s`'"\(])                          # word boundary
    (                                         # capture group
      (?:~/|\.\.?/|/)?                        # optional prefix
      [\w.\-]+(?:/[\w.\-]+)*                  # path segments
      \.(?:yaml|yml|py|rs|md|json|sh|toml|ts|js|go|sql|txt)  # extension
    )
    (?:$|[\s`'"\)])                           # word boundary
    """,
    re.VERBOSE | re.MULTILINE,
)

# Directory reference regex
DIR_REF_RE = re.compile(
    r"\b(projects/[\w\-]+/|initiatives/|experiments/|\.hex/[\w/]+|raw/[\w/]+)"
)

# Dependency signals — matches q-NNN IDs or multi-char named features (min 3 chars, no common words)
_DEP_STOPWORDS = {"the", "a", "an", "on", "to", "in", "of", "be", "or", "and", "is",
                  "it", "as", "at", "by", "for", "up", "do", "if", "so", "no", "its",
                  "but", "not", "yet", "all", "any", "use", "via", "new", "old"}

DEP_RE = re.compile(
    r"""
    (?:depends\s+on|blocked\s+by|requires|needs)\s+
    (q-\d+|[A-Z][\w\-]+|[\w][\w\-]{2,}(?:\s+CLI)?)
    """,
    re.VERBOSE | re.IGNORECASE,
)


def _extract_files(spec_text: str) -> list[str]:
    """Extract file paths and directory references from spec content."""
    files = set()

    # Explicit file paths
    for m in FILE_PATH_RE.finditer(spec_text):
        p = m.group(1).strip()
        if len(p) > 3:  # filter out junk like ".py"
            files.add(p)

    # Directory references (normalize to dir prefix)
    for m in DIR_REF_RE.finditer(spec_text):
        files.add(m.group(1).rstrip("/"))

    return sorted(files)


def _extract_dependencies(spec_text: str) -> list[str]:
    """Extract dependency signals from spec content."""
    deps = set()
    for m in DEP_RE.finditer(spec_text):
        token = m.group(1).strip()
        if token.lower() not in _DEP_STOPWORDS and len(token) >= 2:
            deps.add(token)
    return sorted(deps)


def _load_manifest(manifest_path: Path) -> list[dict]:
    """Load active-work-manifest.jsonl, ignoring malformed lines."""
    if not manifest_path.exists():
        return []
    entries = []
    for line in manifest_path.read_text(errors="replace").splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            entries.append(json.loads(line))
        except json.JSONDecodeError:
            pass
    return entries


def _normalize_path(p: str) -> str:
    """Normalize path for comparison (strip ./ prefix, lowercase)."""
    return p.lstrip("./").lower()


def _paths_overlap(a: str, b: str) -> bool:
    """Return True if two path strings refer to the same file or one is prefix of the other."""
    na, nb = _normalize_path(a), _normalize_path(b)
    return na == nb or na.startswith(nb + "/") or nb.startswith(na + "/")


def check_spec(spec_path: Path, manifest_path: Path) -> dict:
    """Run conflict and dependency check for a spec."""
    if not spec_path.exists():
        return {
            "status": "ERROR",
            "message": f"Spec file not found: {spec_path}",
            "spec": str(spec_path),
        }

    spec_text = spec_path.read_text(errors="replace")

    # Extract queue ID from filename (q-NNN) or spec content
    spec_id_match = re.search(r"q-\d+", spec_path.name)
    spec_id = spec_id_match.group(0) if spec_id_match else "unknown"

    # Title
    title_match = re.search(r"^#\s+(.+)$", spec_text, re.MULTILINE)
    title = title_match.group(1).strip() if title_match else spec_path.name

    incoming_files = _extract_files(spec_text)
    incoming_deps = _extract_dependencies(spec_text)

    manifest = _load_manifest(manifest_path)
    active = [e for e in manifest if e.get("status") in ("running", "queued", "active")]

    conflicts = []
    for entry in active:
        entry_id = entry.get("spec_id", "")
        if entry_id == spec_id:
            continue  # don't conflict with self

        entry_files = entry.get("files_touched", [])
        overlapping = [
            f for f in incoming_files
            if any(_paths_overlap(f, ef) for ef in entry_files)
        ]
        if overlapping:
            conflicts.append({
                "conflicting_spec": entry_id,
                "name": entry.get("name", ""),
                "priority_score": entry.get("priority_score", 0),
                "agent": entry.get("agent", ""),
                "overlapping_files": overlapping,
            })

    # Check dependencies
    blocked_by = []
    completed_ids = {e.get("spec_id") for e in manifest if e.get("status") == "completed"}
    active_ids = {e.get("spec_id") for e in active}

    for dep in incoming_deps:
        # Only flag q-NNN dependencies we can look up in manifest
        if re.match(r"q-\d+", dep):
            if dep not in completed_ids:
                blocked_by.append({
                    "dependency": dep,
                    "status": "active" if dep in active_ids else "not-in-manifest",
                })

    if conflicts:
        status = "CONFLICT"
        message = (
            f"{len(conflicts)} file-path conflict(s) with active specs: "
            + ", ".join(c["conflicting_spec"] for c in conflicts)
        )
        exit_code = 1
    elif blocked_by:
        status = "BLOCKED"
        message = (
            "Unmet dependencies: "
            + ", ".join(b["dependency"] for b in blocked_by)
        )
        exit_code = 2
    else:
        status = "CLEAR"
        message = "No conflicts or unmet dependencies detected."
        exit_code = 0

    return {
        "status": status,
        "message": message,
        "spec": str(spec_path),
        "spec_id": spec_id,
        "title": title,
        "files_scanned": incoming_files,
        "dependencies_found": incoming_deps,
        "conflicts": conflicts,
        "blocked_by": blocked_by,
        "manifest_active_count": len(active),
        "_exit_code": exit_code,
    }


def main():
    parser = argparse.ArgumentParser(
        description="Check a BOI spec for file-path conflicts and unmet dependencies.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Exit codes:
  0  CLEAR    — no conflicts, all dependencies met
  1  CONFLICT — overlapping file found in manifest
  2  BLOCKED  — dependency signal references unfinished spec/feature
  3  ERROR    — spec file not found or manifest unreadable

Examples:
  hex-coordinator-check.py ~/.boi/queue/q-780.spec.md
  hex-coordinator-check.py spec.md --manifest ~/.hex/audit/active-work-manifest.jsonl
""",
    )
    parser.add_argument("spec", nargs="?", help="Path to spec file")
    parser.add_argument(
        "--manifest",
        default=str(DEFAULT_MANIFEST),
        help=f"Path to active-work-manifest.jsonl (default: {DEFAULT_MANIFEST})",
    )
    parser.add_argument("--json", action="store_true", help="Output JSON (default)")
    args = parser.parse_args()

    if not args.spec:
        parser.print_help()
        sys.exit(1)

    spec_path = Path(args.spec).expanduser().resolve()
    manifest_path = Path(args.manifest).expanduser().resolve()

    result = check_spec(spec_path, manifest_path)
    exit_code = result.pop("_exit_code", 0 if result["status"] == "CLEAR" else 3)

    print(json.dumps(result, indent=2))
    sys.exit(exit_code)


if __name__ == "__main__":
    main()
