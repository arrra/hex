#!/usr/bin/env python3
"""Compute P0-P5 refactoring priorities from assessment data.

Usage:
    python3 compute_priorities.py <output_dir>

Reads:
    <output_dir>/raw/complexity.json
    <output_dir>/raw/maintainability.json
    <output_dir>/raw/change_freq.txt
    <output_dir>/raw/security.json (optional)
    <output_dir>/raw/dead_code.txt (optional)

Writes:
    <output_dir>/priorities.json
    <output_dir>/priorities.md (human-readable)
"""
import json
import os
import sys
from pathlib import Path


def load_json(path):
    try:
        with open(path) as f:
            return json.load(f)
    except FileNotFoundError:
        print(f"ERROR: Required file not found: {path}", file=sys.stderr)
        sys.exit(1)
    except json.JSONDecodeError as e:
        print(f"ERROR: Invalid JSON in {path}: {e}", file=sys.stderr)
        sys.exit(1)


def load_change_freq(path):
    freq = {}
    if not os.path.exists(path):
        return freq
    with open(path) as f:
        for line in f:
            parts = line.strip().split(None, 1)
            if len(parts) == 2:
                freq[parts[1]] = int(parts[0])
    return freq


def load_security_modules(path):
    modules = set()
    if not os.path.exists(path):
        return modules
    data = load_json(path)
    for result in data.get("results", []):
        if result.get("issue_severity") in ("HIGH", "MEDIUM"):
            modules.add(result.get("filename", ""))
    return modules


def compute(output_dir):
    output_dir = Path(output_dir)
    raw = output_dir / "raw"

    complexity = load_json(raw / "complexity.json")
    maintainability = load_json(raw / "maintainability.json")
    change_freq = load_change_freq(raw / "change_freq.txt")
    security_modules = load_security_modules(raw / "security.json")

    # Top-20% threshold for change frequency
    freqs = sorted(change_freq.values(), reverse=True)
    top_20_threshold = freqs[max(0, len(freqs) // 5 - 1)] if freqs else 0

    priorities = []
    for module, cc_data in complexity.items():
        if not cc_data:
            continue
        max_cc = max((f["complexity"] for f in cc_data), default=0)
        mi_data = maintainability.get(module, {})
        mi = mi_data.get("mi", 100) if isinstance(mi_data, dict) else mi_data
        if isinstance(mi, str):
            mi = float(mi) if mi.replace(".", "").replace("-", "").isdigit() else 100
        freq = change_freq.get(module, 0)
        is_hot = freq >= top_20_threshold and freq > 0
        has_security = any(module.endswith(s) or s.endswith(module) for s in security_modules)

        if max_cc >= 16 and is_hot and (has_security or mi <= 10):
            priority = "P1"
        elif max_cc >= 16 and is_hot:
            priority = "P2"
        elif max_cc >= 16:
            priority = "P3"
        elif max_cc >= 11:
            priority = "P4"
        else:
            priority = "P5"

        # Security findings upgrade by one level
        if has_security and priority in ("P3", "P4"):
            priority = {"P3": "P2", "P4": "P3"}[priority]

        priorities.append({
            "module": module,
            "priority": priority,
            "max_cc": max_cc,
            "mi": round(mi, 1) if isinstance(mi, float) else mi,
            "change_freq": freq,
            "has_security_issues": has_security,
        })

    order = {"P0": 0, "P1": 1, "P2": 2, "P3": 3, "P4": 4, "P5": 5}
    priorities.sort(key=lambda x: (order.get(x["priority"], 9), -x["max_cc"]))

    # Write JSON
    with open(output_dir / "priorities.json", "w") as f:
        json.dump(priorities, f, indent=2)

    # Write human-readable
    with open(output_dir / "priorities.md", "w") as f:
        f.write("# Refactoring Priorities\n\n")
        f.write("| Priority | Module | CC | MI | Freq | Security |\n")
        f.write("|----------|--------|-----|-----|------|----------|\n")
        for p in priorities:
            sec = "YES" if p["has_security_issues"] else ""
            f.write(f"| {p['priority']} | {p['module']} | {p['max_cc']} | {p['mi']} | {p['change_freq']} | {sec} |\n")

    print(f"Wrote {len(priorities)} modules to {output_dir / 'priorities.json'}")
    print(f"Wrote {output_dir / 'priorities.md'}")

    # Summary
    by_priority = {}
    for p in priorities:
        by_priority.setdefault(p["priority"], []).append(p["module"])
    for pri in ("P0", "P1", "P2", "P3", "P4", "P5"):
        if pri in by_priority:
            print(f"  {pri}: {len(by_priority[pri])} modules")


if __name__ == "__main__":
    if len(sys.argv) < 2:
        print("Usage: python3 compute_priorities.py <output_dir>")
        sys.exit(1)
    compute(sys.argv[1])
