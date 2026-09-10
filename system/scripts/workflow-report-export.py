#!/usr/bin/env python3
"""Export every Claude Code workflow run's final result into the hex workspace.

The Workflow harness persists one JSON record per run at
~/.claude/projects/<project-key>/workflows/wf_<id>.json (or ~/.codex/projects/...
when $HEX_RUNTIME=codex) — name, status, result, narrator logs, script source,
token/duration counters. Nothing in hex reads those: the transcript parser
drops tool results, so workflow outcomes only reached memory through whatever
the operator session said about them.

This exporter writes one markdown report per run to
    $HEX_DIR/projects/<project>/workflow-reports/<date>-<workflow>-<runId>.md
so `hex memory index` picks it up like any other workspace file. Idempotent:
a run whose report file already exists is skipped. Runs whose project cannot
be inferred land in projects/_unmapped/workflow-reports/ and are reported on
stderr — never silently dropped.

Project mapping is optional and instance-specific: this script ships with no
hardcoded project names. To name projects explicitly, create
    $HEX_DIR/.hex/config/workflow-projects.toml
as an ordered list of substring-to-project rules:

    [[map]]
    match = "acme-widgets"
    project = "acme-widgets"

    [[map]]
    match = "internal-tools"
    project = "platform"

Rules are checked in order against the run's result JSON, script source, and
workflow name (case-insensitive substring search). The rule with the most
hits wins; ties go to the earliest rule in the file. If the mapping file is
absent, or present but nothing matches, the project is inferred from the
first absolute path found in the run's result, resolved to its containing
repository (on-disk `.git`, a `<host>/<owner>/<repo>` clone layout, or the
path with trailing file / src-style directories stripped) and then its
basename. If that also
finds nothing, the run lands in projects/_unmapped/.

Usage: workflow-report-export.py [--dry-run] [--hex-dir DIR] [--claude-projects DIR]
Exit 0 on success (summary line on stdout), 1 on any error — including an
unparsable run record (reported on stderr, then the scan continues so the
readable records still get their reports before the run fails).
"""
from __future__ import annotations

import argparse
import glob
import json
import os
import re
import sys
import tomllib
from datetime import datetime, timezone

TERMINAL = {"completed", "failed", "killed"}
RESULT_CAP = 60_000  # chars of rendered result before an explicit truncation marker
LOG_CAP = 150

SECRET_RE = re.compile(
    r"(sk-[A-Za-z0-9]{20,}|ghp_[A-Za-z0-9]{20,}|AKIA[A-Z0-9]{12,}|xox[bp]-[A-Za-z0-9-]{10,}"
    r"|Bearer [A-Za-z0-9._-]{20,}|pit-[a-f0-9-]{20,})"
)

# Absolute paths with at least two segments, e.g. /home/x/repo or /home/x/proj/src.
ABS_PATH_RE = re.compile(r"/[\w][\w.\-]*(?:/[\w][\w.\-]*)+")

# A short alnum extension at the end of a path segment, e.g. "main.py", "data.json".
FILE_EXT_RE = re.compile(r"\.[A-Za-z0-9]{1,5}$")


# Directories that are never a repository root themselves — stripped from the
# tail of a path when no on-disk `.git` can be found (spec-review finding G2:
# `/tmp/acme-repo/src/main.py` must resolve to `acme-repo`, not `src`).
NON_REPO_DIRS = frozenset(
    {
        "src", "lib", "libs", "tests", "test", "app", "apps", "packages", "pkg",
        "scripts", "docs", "bin", "dist", "build", "target", "node_modules",
        "system", "cmd", "internal", "modules", "components", "public",
    }
)
CLONE_HOSTS = ("github.com", "gitlab.com", "bitbucket.org")


def repo_root_of(path: str) -> str | None:
    """Resolve an absolute path to its containing repository root.

    1. On disk: the nearest ancestor that contains `.git` (dir or worktree file).
    2. Clone layout `<host>/<owner>/<repo>/...` (e.g. ~/github.com/acme/widgets/src).
    3. Otherwise strip a trailing file segment, then well-known non-repo
       directories, and take what is left — never the immediate parent of a
       file blindly.
    """
    p = path.rstrip("/")
    cur = p
    while cur and cur != "/":
        marker = os.path.join(cur, ".git")
        if os.path.isdir(marker) or os.path.isfile(marker):
            return cur
        cur = os.path.dirname(cur)
    parts = p.split("/")
    for i, seg in enumerate(parts):
        if seg in CLONE_HOSTS and len(parts) > i + 2:
            return "/".join(parts[: i + 3])
    while len(parts) > 2 and FILE_EXT_RE.search(parts[-1]):
        parts.pop()
    while len(parts) > 2 and parts[-1] in NON_REPO_DIRS:
        parts.pop()
    if len(parts) < 2:
        return None
    return "/".join(parts)


def repo_dir_basename(path: str) -> str | None:
    """Basename of the repository that contains `path` (see repo_root_of)."""
    root = repo_root_of(path)
    if not root:
        return None
    return os.path.basename(root) or None


MAPPING_FILE_HELP = """
Project mapping (optional):
  Create $HEX_DIR/.hex/config/workflow-projects.toml to name projects instead
  of falling back to a guessed repo basename. Format:

    [[map]]
    match = "substring-to-search-for"
    project = "project-name"

    [[map]]
    match = "another-substring"
    project = "other-project"

  Rules are checked in order against the run's result JSON, script source,
  and workflow name (case-insensitive substring search). The rule with the
  most hits wins; ties go to the earliest rule in the file. If the file is
  missing, or present but nothing matches, the project is inferred as the
  basename of the first absolute path found in the run's result, falling
  back to "_unmapped".
"""


def default_claude_projects() -> str:
    """~/.claude/projects, or ~/.codex/projects under the Codex runtime."""
    runtime_dir = ".codex" if os.environ.get("HEX_RUNTIME") == "codex" else ".claude"
    return os.path.expanduser(f"~/{runtime_dir}/projects")


def slug(s: str) -> str:
    s = re.sub(r"[^a-z0-9]+", "-", (s or "workflow").lower()).strip("-")
    return s[:60] or "workflow"


def load_project_map(hex_dir: str) -> list[tuple[str, str]]:
    """Load ordered (match, project) rules from the optional TOML mapping file."""
    path = os.path.join(hex_dir, ".hex", "config", "workflow-projects.toml")
    if not os.path.isfile(path):
        return []
    with open(path, "rb") as fh:
        data = tomllib.load(fh)
    out = []
    for entry in data.get("map", []):
        match = entry.get("match")
        project = entry.get("project")
        if match and project:
            out.append((str(match), str(project)))
    return out


def infer_project(rec: dict, project_map: list[tuple[str, str]]) -> str | None:
    blob = " ".join(
        [
            json.dumps(rec.get("result"), ensure_ascii=False),
            (rec.get("script") or "")[:8000],
            rec.get("workflowName") or "",
            rec.get("scriptPath") or "",
        ]
    ).lower()
    if project_map:
        counts = [(blob.count(k.lower()), i, p) for i, (k, p) in enumerate(project_map)]
        counts = [c for c in counts if c[0] > 0]
        if counts:
            counts.sort(key=lambda c: (-c[0], c[1]))
            return counts[0][2]
    result_blob = json.dumps(rec.get("result"), ensure_ascii=False)
    m = ABS_PATH_RE.search(result_blob)
    if m:
        return repo_dir_basename(m.group(0))
    return None


def redact(text: str, warnings: list[str], run_id: str) -> str:
    out, n = SECRET_RE.subn("[REDACTED]", text)
    if n:
        warnings.append(f"{run_id}: redacted {n} credential-shaped string(s)")
    return out


def render_result(result) -> str:
    if result is None:
        return "_(no result recorded)_"
    if isinstance(result, str):
        return result.strip() or "_(empty string)_"
    if isinstance(result, dict):
        lines = []
        for k, v in result.items():
            if v is None or isinstance(v, (str, int, float, bool)):
                sv = str(v).strip()
                if "\n" in sv:
                    lines.append(f"- **{k}:**\n\n  " + sv.replace("\n", "\n  ") + "\n")
                else:
                    lines.append(f"- **{k}:** {sv}")
            else:
                lines.append(f"- **{k}:**\n\n```json\n{json.dumps(v, indent=1, ensure_ascii=False)}\n```\n")
        return "\n".join(lines)
    return f"```json\n{json.dumps(result, indent=1, ensure_ascii=False)}\n```"


def build_report(rec: dict, path: str, warnings: list[str]) -> str:
    run_id = rec.get("runId") or os.path.basename(path).removesuffix(".json")
    name = rec.get("workflowName") or "workflow"
    ts = rec.get("timestamp") or ""
    dur = rec.get("durationMs")
    dur_s = f"{round(dur / 60000, 1)} min" if isinstance(dur, (int, float)) else "unknown"
    session = os.path.basename(os.path.dirname(os.path.dirname(path)))

    body = render_result(rec.get("result"))
    if len(body) > RESULT_CAP:
        cut = len(body) - RESULT_CAP
        body = body[:RESULT_CAP] + f"\n\n**[truncated {cut} chars — full record at {path}]**"

    logs = rec.get("logs") or []
    log_lines = [f"- {str(l).strip()}" for l in logs[:LOG_CAP]]
    if len(logs) > LOG_CAP:
        log_lines.append(f"- [{len(logs) - LOG_CAP} more lines in the record]")

    phases = rec.get("phases") or []
    phase_line = ", ".join(p.get("title", "?") for p in phases if isinstance(p, dict)) or "(none declared)"

    md = [
        f"# Workflow report: {name} ({run_id})",
        "",
        f"- **Status:** {rec.get('status')}",
        f"- **Completed:** {ts}",
        f"- **Duration:** {dur_s}",
        f"- **Agents:** {rec.get('agentCount')}  ·  **Tokens:** {rec.get('totalTokens')}  ·  **Tool calls:** {rec.get('totalToolCalls')}",
        f"- **Phases:** {phase_line}",
        f"- **Session:** {session}",
        f"- **Script:** {rec.get('scriptPath') or '(inline)'}",
        f"- **Record:** {path}",
        "",
        "## What this workflow does",
        "",
        (rec.get("summary") or "_(no description)_").strip(),
        "",
        "## Result",
        "",
        body,
        "",
        "## Narrator log",
        "",
        "\n".join(log_lines) or "_(none)_",
        "",
    ]
    return redact("\n".join(md), warnings, run_id)


def main() -> int:
    ap = argparse.ArgumentParser(
        description=(
            "Export every finished Claude Code Workflow run record to markdown under "
            "$HEX_DIR/projects/<project>/workflow-reports/."
        ),
        epilog=MAPPING_FILE_HELP,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    ap.add_argument("--dry-run", action="store_true")
    ap.add_argument("--hex-dir", default=os.environ.get("HEX_DIR") or os.path.expanduser("~/hex"))
    ap.add_argument("--claude-projects", default=default_claude_projects())
    a = ap.parse_args()

    project_map = load_project_map(a.hex_dir)

    # <claude-projects>/<project-key>/<session-id>/workflows/wf_<id>.json
    records = sorted(glob.glob(os.path.join(a.claude_projects, "*", "*", "workflows", "wf_*.json")))
    wrote = skipped = unmapped = nonterminal = unreadable = 0
    warnings: list[str] = []

    for path in records:
        try:
            rec = json.load(open(path))
        except Exception as e:  # loud: counted below and fails the run, but keep scanning the rest
            unreadable += 1
            warnings.append(f"{path}: unreadable ({e})")
            continue
        status = rec.get("status")
        if status not in TERMINAL:
            nonterminal += 1
            continue
        run_id = rec.get("runId") or os.path.basename(path).removesuffix(".json")
        ts = rec.get("timestamp")
        try:
            date = datetime.fromisoformat(ts.replace("Z", "+00:00")).astimezone(timezone.utc).date().isoformat()
        except Exception:
            date = "undated"
            warnings.append(f"{run_id}: no usable timestamp")
        project = infer_project(rec, project_map)
        if project is None:
            project = "_unmapped"
            unmapped += 1
            warnings.append(f"{run_id} ({rec.get('workflowName')}): project not inferable -> projects/_unmapped/")
        out_dir = os.path.join(a.hex_dir, "projects", project, "workflow-reports")
        out = os.path.join(out_dir, f"{date}-{slug(rec.get('workflowName'))}-{run_id}.md")
        if os.path.exists(out):
            skipped += 1
            continue
        if a.dry_run:
            print(f"would write {out}")
            wrote += 1
            continue
        os.makedirs(out_dir, exist_ok=True)
        tmp = out + ".tmp"
        with open(tmp, "w") as fh:
            fh.write(build_report(rec, path, warnings))
        os.replace(tmp, out)
        wrote += 1

    for w in warnings:
        print(f"workflow-report-export: WARN {w}", file=sys.stderr)
    print(
        f"workflow-report-export: scanned={len(records)} wrote={wrote} skipped_existing={skipped} "
        f"unmapped={unmapped} nonterminal={nonterminal} unreadable={unreadable} warnings={len(warnings)}"
        + (" (dry-run)" if a.dry_run else "")
    )
    if unreadable:
        print(
            f"workflow-report-export: FATAL {unreadable} record(s) could not be parsed",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as e:
        print(f"workflow-report-export: FATAL {type(e).__name__}: {e}", file=sys.stderr)
        sys.exit(1)
