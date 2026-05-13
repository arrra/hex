#!/usr/bin/env python3
"""Compare before/after assessment metrics.

Usage:
    python3 compare_metrics.py <output_dir>

Reads:
    <output_dir>/before-metrics.json
    <output_dir>/after-metrics.json

Writes:
    <output_dir>/improvement-report.md

Supports both schema formats:
  - Nested: {"metrics": {"avg_cyclomatic_complexity": ..., "avg_maintainability_index": ...}}
    (produced by assess.spec.md t-3 and verify.spec.md t-2)
  - Flat: {"avg_complexity": ..., "avg_maintainability": ..., "security_issues": ...}
    (legacy format)
"""
import json
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


def extract_metrics(data):
    """Extract normalized metrics from either nested or flat schema."""
    nested = data.get("metrics", {})

    # avg_complexity: prefer nested avg_cyclomatic_complexity, fall back to flat avg_complexity
    avg_complexity = nested.get("avg_cyclomatic_complexity", data.get("avg_complexity", 0))

    # avg_maintainability: prefer nested avg_maintainability_index, fall back to flat avg_maintainability
    avg_maintainability = nested.get("avg_maintainability_index", data.get("avg_maintainability", 0))

    # security_issues: nested stores HIGH+MEDIUM separately, flat stores combined
    if "security_high_findings" in nested or "security_medium_findings" in nested:
        security_issues = (
            nested.get("security_high_findings", 0) + nested.get("security_medium_findings", 0)
        )
    else:
        security_issues = data.get("security_issues", 0)

    # Per-module dicts (only present in flat/legacy schema; nested schema stores only aggregates)
    complexity = data.get("complexity", {})
    maintainability = data.get("maintainability", {})

    return {
        "avg_complexity": avg_complexity,
        "avg_maintainability": avg_maintainability,
        "security_issues": security_issues,
        "complexity": complexity,
        "maintainability": maintainability,
    }


def compare(output_dir):
    output_dir = Path(output_dir)
    before_raw = load_json(output_dir / "before-metrics.json")
    after_raw = load_json(output_dir / "after-metrics.json")

    before = extract_metrics(before_raw)
    after = extract_metrics(after_raw)

    lines = ["# Improvement Report\n"]

    # Compare per-module complexity (only when per-module data is available)
    lines.append("## Complexity Changes\n")
    b_cc = before["complexity"]
    a_cc = after["complexity"]
    if b_cc or a_cc:
        lines.append("| Module | Before CC | After CC | Delta |")
        lines.append("|--------|-----------|----------|-------|")
        for module in sorted(set(list(b_cc.keys()) + list(a_cc.keys()))):
            b_val = b_cc.get(module, 0)
            a_val = a_cc.get(module, 0)
            if b_val != a_val:
                delta = a_val - b_val
                arrow = "v" if delta < 0 else "^"
                lines.append(f"| {module} | {b_val} | {a_val} | {arrow}{abs(delta)} |")
    else:
        lines.append("_Per-module complexity data not available (aggregate metrics only)_")

    # Compare per-module maintainability (only when per-module data is available)
    lines.append("\n## Maintainability Changes\n")
    b_mi = before["maintainability"]
    a_mi = after["maintainability"]
    if b_mi or a_mi:
        lines.append("| Module | Before MI | After MI | Delta |")
        lines.append("|--------|-----------|----------|-------|")
        for module in sorted(set(list(b_mi.keys()) + list(a_mi.keys()))):
            b_val = b_mi.get(module, 0)
            a_val = a_mi.get(module, 0)
            if isinstance(b_val, (int, float)) and isinstance(a_val, (int, float)):
                if abs(a_val - b_val) > 0.5:
                    delta = a_val - b_val
                    arrow = "^" if delta > 0 else "v"
                    lines.append(f"| {module} | {b_val:.1f} | {a_val:.1f} | {arrow}{abs(delta):.1f} |")
    else:
        lines.append("_Per-module maintainability data not available (aggregate metrics only)_")

    # Compare security
    lines.append("\n## Security Changes\n")
    b_sec = before["security_issues"]
    a_sec = after["security_issues"]
    lines.append(f"- Before: {b_sec} findings")
    lines.append(f"- After: {a_sec} findings")
    lines.append(f"- Delta: {a_sec - b_sec:+d}")

    # Summary
    lines.append("\n## Summary\n")
    b_avg_cc = before["avg_complexity"]
    a_avg_cc = after["avg_complexity"]
    b_avg_mi = before["avg_maintainability"]
    a_avg_mi = after["avg_maintainability"]
    lines.append("| Metric | Before | After | Change |")
    lines.append("|--------|--------|-------|--------|")
    lines.append(f"| Avg Complexity | {b_avg_cc:.1f} | {a_avg_cc:.1f} | {a_avg_cc - b_avg_cc:+.1f} |")
    lines.append(f"| Avg Maintainability | {b_avg_mi:.1f} | {a_avg_mi:.1f} | {a_avg_mi - b_avg_mi:+.1f} |")
    lines.append(f"| Security Issues | {b_sec} | {a_sec} | {a_sec - b_sec:+d} |")

    report = "\n".join(lines)
    with open(output_dir / "improvement-report.md", "w") as f:
        f.write(report)

    print(report)
    print(f"\nWrote to {output_dir / 'improvement-report.md'}")


if __name__ == "__main__":
    if len(sys.argv) < 2:
        print("Usage: python3 compare_metrics.py <output_dir>")
        sys.exit(1)
    compare(sys.argv[1])
