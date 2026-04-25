#!/usr/bin/env python3
"""
hex-coordinator-score.py — Priority scorer for BOI specs.

Scores a spec file 0-100 based on initiative tier, KR urgency, and work type.

Usage:
  hex-coordinator-score.py <spec-file> [--initiatives-dir DIR] [--json]
  hex-coordinator-score.py --help
"""

import argparse
import json
import os
import re
import sys
from datetime import date, datetime
from pathlib import Path

try:
    import yaml
    HAS_YAML = True
except ImportError:
    HAS_YAML = False


# ── Tier scoring ─────────────────────────────────────────────────────────────

TIER_SCORES = {
    "system health": 40,
    "system_health": 40,
    "infrastructure": 35,
    "user experience": 25,
    "user_experience": 25,
    "content": 15,
    "brand": 15,
    "growth": 15,
    "experiment": 10,
    "research": 5,
}

TIER_KEYWORDS = {
    "system health": ["harness", "wake", "queue", "daemon", "worker", "boot", "crash",
                      "critical", "recovery", "health check", "watchdog"],
    "infrastructure": ["coordinator", "manifest", "deploy", "pipeline", "migration",
                       "database", "schema", "config", "install", "upgrade", "memory"],
    "user experience": ["pulse", "ui", "dashboard", "visibility", "latency", "onboard",
                        "notify", "message", "display", "render"],
    "content": ["brand", "content", "post", "article", "social", "caption",
                "x-twitter", "linkedin", "newsletter"],
    "experiment": ["experiment", "test hypothesis", "a/b", "measure", "validate idea"],
    "research": ["research", "analyze", "survey", "explore", "investigate"],
}

WORK_TYPE_SCORES = {
    "fix": 20,
    "repair": 20,
    "bug": 20,
    "feature": 15,
    "implement": 15,
    "build": 15,
    "experiment": 10,
    "explore": 5,
    "research": 5,
    "analyze": 5,
}

WORK_TYPE_KEYWORDS = {
    "fix": ["fix", "repair", "bug", "broken", "error", "crash", "regression", "issue"],
    "feature": ["implement", "build", "create", "add", "ship", "deploy", "generate"],
    "experiment": ["experiment", "hypothesis", "a/b test", "try"],
    "research": ["research", "analyze", "investigate", "explore", "survey", "audit"],
}


def _load_yaml_file(path: Path) -> dict:
    if not path.exists():
        return {}
    text = path.read_text(errors="replace")
    if HAS_YAML:
        try:
            return yaml.safe_load(text) or {}
        except Exception:
            pass
    # Minimal key: value fallback parser for flat YAML
    result = {}
    for line in text.splitlines():
        if ":" in line and not line.strip().startswith("#"):
            k, _, v = line.partition(":")
            result[k.strip()] = v.strip().strip('"').strip("'")
    return result


def _infer_tier_from_text(text: str) -> tuple[str, int]:
    text_lower = text.lower()
    for tier, keywords in TIER_KEYWORDS.items():
        for kw in keywords:
            if kw in text_lower:
                return tier, TIER_SCORES[tier]
    return "infrastructure", TIER_SCORES["infrastructure"]


def _infer_work_type_from_text(text: str) -> tuple[str, int]:
    text_lower = text.lower()
    for wtype, keywords in WORK_TYPE_KEYWORDS.items():
        for kw in keywords:
            if kw in text_lower:
                return wtype, WORK_TYPE_SCORES[wtype]
    return "feature", WORK_TYPE_SCORES["feature"]


def _parse_initiative_id(spec_text: str) -> str | None:
    """Extract initiative ID referenced in spec content."""
    patterns = [
        r"\binit-[\w-]+",
        r"initiative[:\s]+([a-z][\w-]+)",
        r"initiative_id[:\s]+([a-z][\w-]+)",
    ]
    for pat in patterns:
        m = re.search(pat, spec_text, re.IGNORECASE)
        if m:
            return m.group(0) if m.lastindex is None else m.group(1)
    return None


def _load_initiative(initiative_id: str, initiatives_dir: Path) -> dict:
    """Load initiative YAML by id."""
    for f in initiatives_dir.glob("*.yaml"):
        data = _load_yaml_file(f)
        if str(data.get("id", "")).strip() == initiative_id:
            return data
    return {}


def _urgency_score(horizon_str: str | None) -> int:
    if not horizon_str:
        return 10
    try:
        if isinstance(horizon_str, date):
            horizon = horizon_str
        else:
            horizon = datetime.strptime(str(horizon_str).strip(), "%Y-%m-%d").date()
        days = (horizon - date.today()).days
        if days <= 3:
            return 30
        elif days <= 7:
            return 20
        elif days <= 14:
            return 10
        else:
            return 5
    except ValueError:
        return 10


def _dependency_score(spec_text: str) -> int:
    """Bonus: +5 if spec was previously throttled, +10 if it unblocks others."""
    score = 0
    if "throttled: true" in spec_text:
        score += 5
    if re.search(r"unblocks|required by|produces|dependency for", spec_text, re.IGNORECASE):
        score += 10
    return score


def score_spec(spec_path: Path, initiatives_dir: Path) -> dict:
    """Compute priority score for a spec file."""
    if not spec_path.exists():
        return {"error": f"Spec file not found: {spec_path}", "score": 0}

    spec_text = spec_path.read_text(errors="replace")

    # Extract spec title (first # heading)
    title_match = re.search(r"^#\s+(.+)$", spec_text, re.MULTILINE)
    title = title_match.group(1).strip() if title_match else spec_path.name

    # Initiative lookup
    initiative_id = _parse_initiative_id(spec_text)
    initiative_data = {}
    tier = None
    horizon = None

    if initiative_id and initiatives_dir.exists():
        initiative_data = _load_initiative(initiative_id, initiatives_dir)

    if initiative_data:
        tier = str(initiative_data.get("tier", "")).strip().lower() or None
        horizon = initiative_data.get("horizon")

    # Tier score
    if tier and tier in TIER_SCORES:
        tier_score = TIER_SCORES[tier]
        tier_label = tier
    else:
        tier_label, tier_score = _infer_tier_from_text(spec_text)
        if initiative_id and not initiative_data:
            # Initiative referenced but not found — warn and default
            tier_label = "infrastructure"
            tier_score = TIER_SCORES["infrastructure"]

    # Urgency score
    urgency = _urgency_score(horizon)

    # Work type
    work_type, work_score = _infer_work_type_from_text(spec_text)

    # Dependency bonus
    dep_score = _dependency_score(spec_text)

    total = min(100, tier_score + urgency + work_score + dep_score)

    return {
        "spec": str(spec_path),
        "title": title,
        "score": total,
        "breakdown": {
            "tier": {"label": tier_label, "score": tier_score},
            "urgency": {"horizon": str(horizon) if horizon else None, "score": urgency},
            "work_type": {"label": work_type, "score": work_score},
            "dependency_bonus": dep_score,
        },
        "initiative_id": initiative_id,
        "initiative_found": bool(initiative_data),
    }


def main():
    parser = argparse.ArgumentParser(
        description="Score a BOI spec file for dispatch priority (0-100).",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
  hex-coordinator-score.py ~/.boi/queue/q-775.spec.md
  hex-coordinator-score.py spec.md --initiatives-dir ./initiatives --json
""",
    )
    parser.add_argument("spec", nargs="?", help="Path to spec file")
    parser.add_argument(
        "--initiatives-dir",
        default=None,
        help="Directory containing initiative YAML files (default: auto-detect)",
    )
    parser.add_argument("--json", action="store_true", help="Output JSON (default)")
    args = parser.parse_args()

    if not args.spec:
        parser.print_help()
        sys.exit(1)

    spec_path = Path(args.spec).expanduser().resolve()

    # Auto-detect initiatives dir
    if args.initiatives_dir:
        initiatives_dir = Path(args.initiatives_dir).expanduser().resolve()
    else:
        # Walk up from spec location to find initiatives/
        search = spec_path.parent
        initiatives_dir = None
        for _ in range(6):
            candidate = search / "initiatives"
            if candidate.is_dir():
                initiatives_dir = candidate
                break
            search = search.parent
        if not initiatives_dir:
            # Fall back to cwd
            initiatives_dir = Path.cwd() / "initiatives"

    result = score_spec(spec_path, initiatives_dir)

    print(json.dumps(result, indent=2))
    sys.exit(0 if "error" not in result else 1)


if __name__ == "__main__":
    main()
