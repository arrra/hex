#!/usr/bin/env python3
"""
hex-coordinator-throttle.py — Pre-dispatch throttle hook for BOI specs.

Advisory in v1: logs warnings and recommendations, does not hard-block.

Subcommands:
  --status                         Show current throttle state
  --check <spec> [--agent AGENT]   Check if spec would be throttled before dispatch
  --register <spec> [--agent AGENT] [--score N]
                                   Register spec as active in manifest
  --complete <spec-id>             Mark spec complete (removes from active manifest)
  --clean                          Remove stale completed/canceled entries

Manifest: ~/.hex/audit/active-work-manifest.jsonl
Each line: {spec_id, spec_path, agent, status, priority_score, dispatched_at, completed_at, files_touched}

Exit codes (--check):
  0  CLEAR       — under both per-agent and system limits
  1  THROTTLED   — at or over limit (advisory warning returned)
  2  BUMPED      — lower-priority spec yielded to make room
  3  ERROR       — bad arguments or unreadable manifest
"""

import argparse
import json
import os
import re
import sys
from datetime import datetime, timezone
from pathlib import Path

MANIFEST_PATH = Path.home() / ".hex" / "audit" / "active-work-manifest.jsonl"
BOI_DB = Path.home() / ".boi" / "boi.db"

PER_AGENT_LIMIT = 3
SYSTEM_LIMIT = 10


# ── Manifest I/O ─────────────────────────────────────────────────────────────

def _load_manifest(path: Path = MANIFEST_PATH) -> list[dict]:
    if not path.exists():
        return []
    entries = []
    for line in path.read_text(errors="replace").splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            entries.append(json.loads(line))
        except json.JSONDecodeError:
            pass
    return entries


def _save_manifest(entries: list[dict], path: Path = MANIFEST_PATH) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(".tmp")
    tmp.write_text("\n".join(json.dumps(e) for e in entries) + "\n")
    tmp.replace(path)


def _active_entries(entries: list[dict]) -> list[dict]:
    return [e for e in entries if e.get("status") in ("running", "queued", "active")]


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


# ── BOI DB query (optional, graceful fallback) ───────────────────────────────

def _boi_active_count() -> int | None:
    """Return count of running/queued specs from BOI db, or None if unavailable."""
    if not BOI_DB.exists():
        return None
    try:
        import sqlite3
        con = sqlite3.connect(f"file:{BOI_DB}?mode=ro&immutable=1", uri=True,
                              timeout=2.0)
        cur = con.execute(
            "SELECT COUNT(*) FROM specs WHERE status IN ('running','queued')"
        )
        row = cur.fetchone()
        con.close()
        return row[0] if row else 0
    except Exception:
        return None


# ── Spec file helpers ────────────────────────────────────────────────────────

FILE_PATH_RE = re.compile(
    r"""(?:^|[\s`'\"(])((?:~/|\.\.?/|/)?[\w.\-]+(?:/[\w.\-]+)*\.(?:yaml|yml|py|rs|md|json|sh|toml|ts|js|go|sql|txt))(?:$|[\s`'\")])""",
    re.MULTILINE,
)


def _extract_files(spec_text: str) -> list[str]:
    files = set()
    for m in FILE_PATH_RE.finditer(spec_text):
        p = m.group(1).strip()
        if len(p) > 3:
            files.add(p)
    return sorted(files)


def _spec_id_from_path(spec_path: Path) -> str:
    m = re.search(r"q-\d+", spec_path.name)
    return m.group(0) if m else spec_path.stem


def _spec_title(spec_text: str, spec_path: Path) -> str:
    m = re.search(r"^#\s+(.+)$", spec_text, re.MULTILINE)
    return m.group(1).strip() if m else spec_path.name


# ── Subcommand: --status ─────────────────────────────────────────────────────

def cmd_status(args) -> int:
    entries = _load_manifest(Path(args.manifest))
    active = _active_entries(entries)

    boi_count = _boi_active_count()

    # Per-agent breakdown
    by_agent: dict[str, list[dict]] = {}
    for e in active:
        ag = e.get("agent", "unknown")
        by_agent.setdefault(ag, []).append(e)

    total_active = len(active)
    system_limit_hit = total_active >= SYSTEM_LIMIT

    print(f"Hex Coordinator — Throttle Status")
    print(f"  Manifest active:  {total_active} / {SYSTEM_LIMIT} system limit"
          + (" [AT LIMIT]" if system_limit_hit else ""))
    if boi_count is not None:
        print(f"  BOI db active:    {boi_count}")
    print(f"  Manifest path:    {args.manifest}")
    print()

    if not by_agent:
        print("  No active specs registered in manifest.")
    else:
        for agent, specs in sorted(by_agent.items()):
            at_limit = len(specs) >= PER_AGENT_LIMIT
            print(f"  Agent: {agent}  ({len(specs)}/{PER_AGENT_LIMIT})"
                  + (" [AT LIMIT]" if at_limit else ""))
            for s in specs:
                print(f"    {s.get('spec_id','?'):12s}  score={s.get('priority_score','?'):>3}  {s.get('name','')[:50]}")

    return 0


# ── Subcommand: --check ──────────────────────────────────────────────────────

def cmd_check(args) -> int:
    spec_path = Path(args.check).expanduser().resolve()
    if not spec_path.exists():
        print(json.dumps({"status": "ERROR", "message": f"Spec not found: {spec_path}"}))
        return 3

    spec_text = spec_path.read_text(errors="replace")
    spec_id = _spec_id_from_path(spec_path)
    title = _spec_title(spec_text, spec_path)
    agent = args.agent or "unknown"

    entries = _load_manifest(Path(args.manifest))
    active = _active_entries(entries)

    # Per-agent count
    agent_active = [e for e in active if e.get("agent") == agent
                    and e.get("spec_id") != spec_id]
    agent_count = len(agent_active)

    # System count
    system_count = len([e for e in active if e.get("spec_id") != spec_id])

    warnings = []
    status = "CLEAR"
    exit_code = 0

    if agent_count >= PER_AGENT_LIMIT:
        status = "THROTTLED"
        warnings.append(
            f"Agent '{agent}' has {agent_count} active specs (limit: {PER_AGENT_LIMIT}). "
            f"Recommend queueing with throttled=true."
        )
        exit_code = 1

    if system_count >= SYSTEM_LIMIT:
        status = "THROTTLED"
        # Check if this spec could bump a lower-priority one
        incoming_score = args.score if args.score is not None else 50
        lowest = min(active, key=lambda e: e.get("priority_score", 0), default=None)
        if lowest and lowest.get("priority_score", 100) < incoming_score:
            status = "BUMPED"
            warnings.append(
                f"System at {system_count}/{SYSTEM_LIMIT} active specs. "
                f"Incoming score {incoming_score} > lowest active score "
                f"{lowest.get('priority_score',0)} ({lowest.get('spec_id','?')}). "
                f"Recommend preempting {lowest.get('spec_id','?')}."
            )
            exit_code = 2
        else:
            warnings.append(
                f"System at {system_count}/{SYSTEM_LIMIT} active specs. "
                f"Recommend queueing with throttled=true."
            )
            exit_code = 1

    result = {
        "status": status,
        "spec_id": spec_id,
        "title": title,
        "agent": agent,
        "agent_active_count": agent_count,
        "agent_limit": PER_AGENT_LIMIT,
        "system_active_count": system_count,
        "system_limit": SYSTEM_LIMIT,
        "warnings": warnings,
        "recommendation": (
            "proceed" if status == "CLEAR"
            else "queue_throttled" if status == "THROTTLED"
            else "bump_lowest"
        ),
    }
    print(json.dumps(result, indent=2))
    return exit_code


# ── Subcommand: --register ───────────────────────────────────────────────────

def cmd_register(args) -> int:
    spec_path = Path(args.register).expanduser().resolve()
    if not spec_path.exists():
        print(json.dumps({"status": "ERROR", "message": f"Spec not found: {spec_path}"}))
        return 3

    spec_text = spec_path.read_text(errors="replace")
    spec_id = _spec_id_from_path(spec_path)
    title = _spec_title(spec_text, spec_path)
    agent = args.agent or "unknown"
    score = args.score if args.score is not None else 50
    files = _extract_files(spec_text)

    manifest_path = Path(args.manifest)
    entries = _load_manifest(manifest_path)

    # Upsert: remove existing entry for this spec_id
    entries = [e for e in entries if e.get("spec_id") != spec_id]

    entry = {
        "spec_id": spec_id,
        "spec_path": str(spec_path),
        "name": title,
        "agent": agent,
        "status": "active",
        "priority_score": score,
        "files_touched": files,
        "dispatched_at": _now_iso(),
        "completed_at": None,
    }
    entries.append(entry)
    _save_manifest(entries, manifest_path)

    print(json.dumps({"status": "registered", "spec_id": spec_id, "agent": agent,
                      "score": score, "files": files}))
    return 0


# ── Subcommand: --complete ───────────────────────────────────────────────────

def cmd_complete(args) -> int:
    spec_id = args.complete
    manifest_path = Path(args.manifest)
    entries = _load_manifest(manifest_path)

    updated = False
    for e in entries:
        if e.get("spec_id") == spec_id and e.get("status") in ("active", "running", "queued"):
            e["status"] = "completed"
            e["completed_at"] = _now_iso()
            updated = True

    if not updated:
        print(json.dumps({"status": "not_found", "spec_id": spec_id,
                          "message": "No active entry found for this spec_id"}))
        return 3

    _save_manifest(entries, manifest_path)
    print(json.dumps({"status": "completed", "spec_id": spec_id}))
    return 0


# ── Subcommand: --clean ──────────────────────────────────────────────────────

def cmd_clean(args) -> int:
    from datetime import timedelta
    manifest_path = Path(args.manifest)
    entries = _load_manifest(manifest_path)
    cutoff = datetime.now(timezone.utc) - timedelta(hours=24)

    kept = []
    removed = []
    for e in entries:
        if e.get("status") in ("completed", "canceled", "failed"):
            ts = e.get("completed_at") or e.get("dispatched_at")
            if ts:
                try:
                    dt = datetime.fromisoformat(ts)
                    if dt < cutoff:
                        removed.append(e.get("spec_id", "?"))
                        continue
                except ValueError:
                    pass
        kept.append(e)

    _save_manifest(kept, manifest_path)
    print(json.dumps({"removed": removed, "remaining": len(kept)}))
    return 0


# ── Main ─────────────────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(
        description="Throttle hook for BOI spec dispatch (advisory v1).",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Subcommands (use one):
  --status                         Show throttle dashboard
  --check <spec>                   Pre-dispatch check (exit 0=clear, 1=throttled, 2=bumped)
  --register <spec>                Register spec as active in manifest
  --complete <spec-id>             Mark spec completed (clears from active count)
  --clean                          Remove stale entries (>24h old completed/canceled)

Options:
  --agent AGENT    Agent name for per-agent throttling (default: unknown)
  --score N        Priority score for --check / --register (default: 50)
  --manifest PATH  Path to active-work-manifest.jsonl

Examples:
  hex-coordinator-throttle.py --status
  hex-coordinator-throttle.py --check ~/.boi/queue/q-800.spec.md --agent initiative-loop
  hex-coordinator-throttle.py --register ~/.boi/queue/q-800.spec.md --agent initiative-loop --score 65
  hex-coordinator-throttle.py --complete q-800
  hex-coordinator-throttle.py --clean
""",
    )

    group = parser.add_mutually_exclusive_group()
    group.add_argument("--status", action="store_true", help="Show throttle state")
    group.add_argument("--check", metavar="SPEC", help="Pre-dispatch check for spec")
    group.add_argument("--register", metavar="SPEC", help="Register spec as active")
    group.add_argument("--complete", metavar="SPEC_ID", help="Mark spec completed")
    group.add_argument("--clean", action="store_true", help="Remove stale entries")

    parser.add_argument("--agent", default=None, help="Agent name (for per-agent throttle)")
    parser.add_argument("--score", type=int, default=None,
                        help="Priority score 0-100 (default: 50)")
    parser.add_argument(
        "--manifest",
        default=str(MANIFEST_PATH),
        help=f"Path to manifest (default: {MANIFEST_PATH})",
    )
    parser.add_argument("--json", action="store_true", help="Force JSON output")

    args = parser.parse_args()

    if args.status:
        return cmd_status(args)
    elif args.check:
        return cmd_check(args)
    elif args.register:
        return cmd_register(args)
    elif args.complete:
        return cmd_complete(args)
    elif args.clean:
        return cmd_clean(args)
    else:
        parser.print_help()
        return 1


if __name__ == "__main__":
    sys.exit(main())
