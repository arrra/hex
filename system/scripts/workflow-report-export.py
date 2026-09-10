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
unparsable run record, a structurally invalid record (e.g. `null`, a numeric
`summary`, a non-list `logs`), a per-record write failure, an invalid
workflow-projects.toml mapping rule, or an explicitly-supplied
--claude-projects root that does not exist. Each of these is reported on
stderr per record, then the scan continues so the unaffected records still
get their reports before the run fails. (The default --claude-projects root
being absent is not an error — it is a quiet, successful empty scan.)
"""
from __future__ import annotations

import argparse
import glob
import hashlib
import json
import os
import re
import sys
import tempfile
import tomllib
from datetime import datetime, timezone

TERMINAL = {"completed", "failed", "killed"}
RESULT_CAP = 60_000  # chars of rendered result before an explicit truncation marker
LOG_CAP = 150

# F1 — current credential shapes: sk-* (with or without a provider infix, over
# a mixed alphabet), github_pat_*, the gh[pousr]_ classic token family,
# xox[abps]- Slack tokens, AKIA AWS keys, Bearer headers, pit- tokens,
# key=/token=/password=/secret= value pairs, and PEM private-key blocks.
#
# R2 redo (F2 regression, round 2): the digit-heuristic in _redact_one() was
# not a fix — it still mangled hyphenated prose whose 20+ char tail happens
# to contain a digit ("desk-lamp-and-chair-inventory-2024") and it opened a
# hole for real all-letter keys ("sk-live-abcdefghijklmnopqrstuvwxyz"). The
# correct fix is a lookbehind that requires "sk-" to be preceded by nothing,
# or by a non-alnum character that is NOT a lowercase letter immediately
# preceding it — i.e. block only when "sk-" is glued onto another lowercase
# word ("ta"+"sk-", "ri"+"sk-", "de"+"sk-"). Legitimate ids in this codebase
# embed "sk-" directly after "_"/"-" ("wf_sk-ant-...", "deploy-sk-ant-..."),
# which this lookbehind still allows.
SECRET_RE = re.compile(
    r"(?<![A-Za-z0-9])sk-(?:ant-|proj-|live-|test-)?[A-Za-z0-9_-]{20,}"
    r"|(?<![A-Za-z0-9_-])github_pat_[A-Za-z0-9_]{20,}"
    r"|(?<![A-Za-z0-9_-])gh[pousr]_[A-Za-z0-9]{20,}"
    r"|(?<![A-Za-z0-9_-])xox[abps]-[A-Za-z0-9-]{10,}"
    r"|(?<![A-Za-z0-9_-])AKIA[A-Z0-9]{12,}"
    r"|Bearer [A-Za-z0-9._-]{20,}"
    r"|(?<![A-Za-z0-9_-])pit-[a-f0-9-]{20,}"
    r"|(?<![A-Za-z0-9_])(?:key|token|password|secret)=\S+"
    r"|-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----"
)

# Absolute paths with at least two segments, e.g. /home/x/repo or /home/x/proj/src.
ABS_PATH_RE = re.compile(r"/[\w][\w.\-]*(?:/[\w][\w.\-]*)+")

# A short alnum extension at the end of a path segment, e.g. "main.py", "data.json".
FILE_EXT_RE = re.compile(r"\.[A-Za-z0-9]{1,5}$")

# F7/F16 — a URL's "//host/path" shape reads as a plausible absolute path;
# skip anything under a scheme so a URL never gets mistaken for a filesystem
# path (e.g. "https://example.com/api/v1" must not become project "v1").
URL_RE = re.compile(r"[A-Za-z][A-Za-z0-9+.\-]*://\S*")


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

    work = list(parts)
    # F8 — a trailing segment with an extension is ambiguous on its own (it
    # could be a real file, or a dotted directory like "service.api"), so it
    # never anchors the boundary search itself; it is only consulted
    # afterwards to decide the no-boundary-found / ambiguous case below.
    dir_parts = work[:-1] if work and FILE_EXT_RE.search(work[-1]) else work

    # F15 / review_b G3 — find the recognized non-repo/source-directory
    # boundary CLOSEST to the file: the rightmost NON_REPO_DIRS segment whose
    # immediate predecessor is itself NOT a NON_REPO_DIRS segment (i.e. a
    # real name — the repo). Walking from the left and stopping at the FIRST
    # boundary breaks container layouts like
    # ".../workspace/src/acme-repo/src/main.py", where the first "src" is an
    # outer container prefix, not the repo boundary — that resolved to
    # "workspace" instead of "acme-repo". Walking from the right absolutely
    # (picking the last NON_REPO_DIRS segment full stop) breaks nested
    # source dirs like ".../acme-repo/src/lib/util.py", where "lib" is
    # itself a NON_REPO_DIRS segment with another one ("src") right before
    # it — there the boundary must be "src", not "lib". Requiring a
    # non-boundary predecessor picks the right one in both shapes; this also
    # absorbs unrecognized nested directories below it: e.g.
    # ".../acme-repo/src/auth/main.py" must resolve via the "src" boundary
    # to "acme-repo", never stop early at "auth".
    for i in range(len(dir_parts) - 1, 0, -1):
        if dir_parts[i] in NON_REPO_DIRS and dir_parts[i - 1] not in NON_REPO_DIRS:
            work = dir_parts[:i]
            break
    else:
        # R1 redo: no recognized boundary was found anywhere in the path. If
        # the tail still looks like a stray file, the path is too ambiguous
        # to resolve — report unmapped rather than guessing an unrelated
        # leaf directory (e.g. never .../acme-repo/auth/main.py -> "auth").
        if work and FILE_EXT_RE.search(work[-1]):
            return None

    if len(work) < 2:
        return None
    return "/".join(work)


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
    """Load ordered (match, project) rules from the optional TOML mapping file.

    F11 — a rule missing `match`/`project`, or with a non-string/empty value
    for either, is a configuration error: raise rather than silently drop or
    coerce it, naming the 1-based rule index and the offending field.
    """
    path = os.path.join(hex_dir, ".hex", "config", "workflow-projects.toml")
    if not os.path.isfile(path):
        return []
    with open(path, "rb") as fh:
        data = tomllib.load(fh)
    out = []
    for i, entry in enumerate(data.get("map", []), start=1):
        if not isinstance(entry, dict):
            raise ValueError(f"workflow-projects.toml: rule {i}: must be a table (got {type(entry).__name__})")
        match = entry.get("match")
        if not isinstance(match, str) or not match:
            raise ValueError(
                f"workflow-projects.toml: rule {i}: 'match' must be a nonempty string (got {match!r})"
            )
        project = entry.get("project")
        if not isinstance(project, str) or not project:
            raise ValueError(
                f"workflow-projects.toml: rule {i}: 'project' must be a nonempty string (got {project!r})"
            )
        out.append((match, project))
    return out


def validate_record(rec: object) -> str | None:
    """F6/F18 — structural validation of one workflow record. Returns a
    human-readable reason the record is invalid, or None if it is well-formed
    enough to render (a dict, with `summary` a string if present and `logs` a
    list if present)."""
    if not isinstance(rec, dict):
        return f"record is not a JSON object (got {type(rec).__name__})"
    summary = rec.get("summary")
    if summary is not None and not isinstance(summary, str):
        return f"'summary' must be a string (got {type(summary).__name__})"
    logs = rec.get("logs")
    if logs is not None and not isinstance(logs, list):
        return f"'logs' must be a list (got {type(logs).__name__})"
    # review r1 F1: a non-string runId/workflowName/script/scriptPath (e.g. a
    # raw numeric id) reached redact() or the mapping scan unchecked and
    # raised TypeError; a non-list phases reached build_report()'s iteration
    # unchecked. Both must be named and rejected here, per-record.
    for field in ("runId", "workflowName", "script", "scriptPath"):
        value = rec.get(field)
        if value is not None and not isinstance(value, str):
            return f"'{field}' must be a string (got {type(value).__name__})"
    phases = rec.get("phases")
    if phases is not None and not isinstance(phases, list):
        return f"'phases' must be a list (got {type(phases).__name__})"
    return None


# review_b G1: a name with MORE than one space ("Jane Doe Smith") was not
# caught by looking just one space-separated word ahead — "Doe Smith/..."
# has a second space before the next "/", so the old single-hop regex never
# found it. Match any run of space-separated word-tokens that eventually
# reaches a "/", however many spaces it takes.
_SPACE_CONTINUATION_RE = re.compile(r"(?: [\w.\-]+)+/")


def _looks_truncated_by_space(text: str, end: int) -> bool:
    """True if a path match ends right at a space that is followed by more
    (possibly multi-word, multi-space) path-like text — a strong signal the
    real path continued past the space(s) and the match is only a truncated
    prefix (e.g. ".../Jane Doe Smith/repo/...": the match stops at "Jane"
    but " Doe Smith/..." keeps going)."""
    if end >= len(text) or text[end] != " ":
        return False
    return bool(_SPACE_CONTINUATION_RE.match(text, end))


def _extract_repo_path(text: str) -> str | None:
    """First complete absolute filesystem path in free text (F7/F16): skip
    anything inside a URL, and reject a match that is really just the
    truncated prefix of a space-broken path rather than accepting it as-is."""
    pos = 0
    while pos < len(text):
        um = URL_RE.search(text, pos)
        pm = ABS_PATH_RE.search(text, pos)
        if pm and (not um or pm.start() < um.start()):
            if _looks_truncated_by_space(text, pm.end()):
                pos = pm.end() + 1
                continue
            return pm.group(0)
        if um:
            pos = um.end()
            continue
        break
    return None


# review_b G1 — a `result` dict can carry a path-shaped string in an
# unrelated field (e.g. "summary") that sits earlier in the JSON dump than
# the field that actually identifies the repo. These are checked, in order,
# before falling back to a free-text scan of the whole blob.
_STRUCTURED_PATH_KEYS = ("repo", "repoPath", "repository", "repoDir", "path", "cwd", "workdir", "directory")


def _structured_repo_path(result: object) -> str | None:
    """An explicit repo/path field from a structured `result` dict, if any."""
    if not isinstance(result, dict):
        return None
    for key in _STRUCTURED_PATH_KEYS:
        value = result.get(key)
        if isinstance(value, str) and value.strip():
            found = _extract_repo_path(value)
            if found:
                return found
    return None


def infer_project(rec: dict, project_map: list[tuple[str, str]]) -> str | None:
    blob = " ".join(
        [
            json.dumps(rec.get("result"), ensure_ascii=False),
            rec.get("script") or "",  # F17 — search the full script, no cutoff
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
    result = rec.get("result")
    found = _structured_repo_path(result)
    if not found:
        found = _extract_repo_path(json.dumps(result, ensure_ascii=False))
    if found:
        return repo_dir_basename(found)
    return None


def _redact_one(m: re.Match) -> str:
    return "[REDACTED]"


def redact(text: str, warnings: list[str] | None = None, label: str = "record") -> str:
    """Strip credential-shaped substrings from `text` (F1).

    `label` is used only to annotate a diagnostic in `warnings` — it must
    itself be safe to print (e.g. a file path), never the raw text being
    redacted, or the "what got redacted" note would leak the secret it is
    reporting on (F2).
    """
    if not text:
        return text
    redacted_count = 0

    def _sub(m: re.Match) -> str:
        nonlocal redacted_count
        replacement = _redact_one(m)
        if replacement != m.group(0):
            redacted_count += 1
        return replacement

    out = SECRET_RE.sub(_sub, text)
    if redacted_count and warnings is not None:
        warnings.append(f"{label}: redacted {redacted_count} credential-shaped string(s)")
    return out


def unsafe_component_reason(s: str | None) -> str | None:
    """F4 — why `s` is unsafe as a single path component, or None if safe
    (non-empty, no separators, not "." / "..", not absolute)."""
    if not s:
        return "empty"
    if os.path.isabs(s):
        return "absolute path"
    if s in (".", ".."):
        return "path traversal"
    if "/" in s or (os.sep != "/" and os.sep in s) or (os.altsep and os.altsep in s):
        return "path separator"
    return None


def _is_contained(child_dir: str, parent_dir: str) -> bool:
    """F4 — True if the resolved (symlink-following) `child_dir` is
    `parent_dir` itself or lives under it."""
    child = os.path.realpath(child_dir)
    parent = os.path.realpath(parent_dir)
    return child == parent or child.startswith(parent + os.sep)


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
    # F2 — the on-disk record path can itself embed a credential-shaped
    # runId (the harness names wf_*.json after it), so it is not safe to use
    # as a diagnostic label as-is; sanitize once and label with that.
    path_label = redact(path)
    run_id = rec.get("runId") or os.path.basename(path).removesuffix(".json")
    name = rec.get("workflowName") or "workflow"
    ts = rec.get("timestamp") or ""
    dur = rec.get("durationMs")
    dur_s = f"{round(dur / 60000, 1)} min" if isinstance(dur, (int, float)) else "unknown"
    session = os.path.basename(os.path.dirname(os.path.dirname(path)))

    body = render_result(rec.get("result"))
    body = redact(body, warnings, path_label)  # F3 — redact before truncating, not after
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
    # F2 — label with the sanitized path, never the raw run_id/path.
    return redact("\n".join(md), warnings, path_label)


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
    ap.add_argument("--claude-projects", default=None)
    a = ap.parse_args()

    # F9 — an EXPLICITLY supplied --claude-projects root that does not exist
    # is a misconfiguration (e.g. a typo) and must be diagnosed, not read as
    # a healthy empty scan; the default root missing is the ordinary case of
    # a machine that has never run a Workflow, and stays a quiet empty scan.
    claude_projects_explicit = a.claude_projects is not None
    claude_projects = a.claude_projects if claude_projects_explicit else default_claude_projects()

    project_map = load_project_map(a.hex_dir)

    source_root_missing = False
    if claude_projects_explicit and not os.path.isdir(claude_projects):
        source_root_missing = True

    # <claude-projects>/<project-key>/<session-id>/workflows/wf_<id>.json
    records = sorted(glob.glob(os.path.join(claude_projects, "*", "*", "workflows", "wf_*.json")))
    projects_root = os.path.join(a.hex_dir, "projects")
    wrote = skipped = unmapped = nonterminal = unreadable = invalid = failed = 0
    warnings: list[str] = []

    if source_root_missing:
        # G1 (review_b) — an explicitly-supplied --claude-projects value is
        # attacker/user-controlled text; redact it like any other diagnostic.
        warnings.append(f"--claude-projects root does not exist: {redact(claude_projects)}")

    for path in records:
        # F2 — the on-disk record path can itself embed a credential-shaped
        # runId (the harness names wf_*.json after it), so it is not safe to
        # use as a diagnostic label as-is; sanitize once and label with that.
        path_label = redact(path)
        try:
            with open(path) as fh:
                rec = json.load(fh)
        except Exception as e:  # loud: counted below and fails the run, but keep scanning the rest
            unreadable += 1
            warnings.append(f"{path_label}: unreadable ({redact(str(e))})")
            continue

        # F6/F18 — structurally invalid JSON (valid JSON, wrong shape: e.g.
        # `null`, a numeric `summary`, a non-list `logs`) is a per-record
        # failure, not a crash: name it, count it, and keep scanning.
        invalid_reason = validate_record(rec)
        if invalid_reason:
            invalid += 1
            # G1 (review_b) — the harness names wf_*.json after runId, so an
            # unvalidated record's filename can itself be credential-shaped;
            # redact the fallback id before it goes into a diagnostic.
            fallback_id = redact(os.path.basename(path).removesuffix(".json"))
            warnings.append(f"{fallback_id}: invalid record ({invalid_reason}) -> skipped")
            continue

        # F6/F18 — the whole record-processing path, from the status check
        # through publish, is wrapped: an OSError writing one project's
        # report (or any OTHER unforeseen exception a malformed-but-valid-
        # JSON record's field shapes provoke downstream, e.g. a TypeError
        # from a non-string runId/workflowName that slipped past
        # validate_record) must not abort the scan for unrelated records.
        # Any temp file created before the failure is cleaned up.
        tmp = None
        try:
            status = rec.get("status")
            if status not in TERMINAL:
                nonterminal += 1
                continue

            # F2 — redact credential-shaped identifiers before they ever
            # become part of a path or a diagnostic message.
            run_id_raw = rec.get("runId") or os.path.basename(path).removesuffix(".json")
            run_id = redact(run_id_raw, warnings, path_label)
            if run_id != run_id_raw:
                # review_b G2 — redact() collapses every credential-shaped
                # runId to the same literal "...[REDACTED]" string, so two
                # distinct credential-shaped runIds collided on one filename
                # and the second run was silently dropped as "already
                # exported". Append a short, non-reversible fingerprint of
                # the RAW id (never the raw id itself, which is exactly the
                # secret being redacted) so distinct runIds still land on
                # distinct files.
                fingerprint = hashlib.sha256(run_id_raw.encode("utf-8", "surrogateescape")).hexdigest()[:8]
                run_id = f"{run_id}-{fingerprint}"
            workflow_name = redact(rec.get("workflowName") or "workflow", warnings, path_label)

            ts = rec.get("timestamp")
            try:
                date = (
                    datetime.fromisoformat(ts.replace("Z", "+00:00")).astimezone(timezone.utc).date().isoformat()
                )
            except Exception:
                date = "undated"
                warnings.append(f"{run_id}: no usable timestamp")

            project = infer_project(rec, project_map)
            if project is not None:
                project = redact(project, warnings, path_label)

            # F4 — project and runId must each be safe single path
            # components; anything else routes to _unmapped with a WARN
            # naming the reason.
            routed_unmapped = False
            if project is None:
                warnings.append(f"{run_id} ({workflow_name}): project not inferable -> projects/_unmapped/")
                project = "_unmapped"
                routed_unmapped = True
            else:
                reason = unsafe_component_reason(project)
                if reason:
                    warnings.append(
                        f"{run_id} ({workflow_name}): project {project!r} rejected ({reason}) -> "
                        "projects/_unmapped/"
                    )
                    project = "_unmapped"
                    routed_unmapped = True

            run_id_component = run_id
            rid_reason = unsafe_component_reason(run_id)
            if rid_reason:
                warnings.append(f"{run_id!r}: runId rejected ({rid_reason}) -> using a sanitized id")
                run_id_component = slug(run_id) or "run"
                if not routed_unmapped:
                    project = "_unmapped"
                    routed_unmapped = True

            if routed_unmapped:
                unmapped += 1

            out_dir = os.path.join(projects_root, project, "workflow-reports")
            out = os.path.join(out_dir, f"{date}-{slug(workflow_name)}-{run_id_component}.md")

            # F4 — belt-and-braces: the resolved destination (following
            # symlinks) must stay under $HEX_DIR/projects/, even when
            # project/runId passed the string-level checks above but a
            # symlink on disk escapes.
            if not _is_contained(out_dir, projects_root):
                warnings.append(
                    f"{run_id}: destination for project {project!r} escapes {projects_root} -> _unmapped"
                )
                if not routed_unmapped:
                    unmapped += 1
                    routed_unmapped = True
                project = "_unmapped"
                out_dir = os.path.join(projects_root, project, "workflow-reports")
                out = os.path.join(out_dir, f"{date}-{slug(workflow_name)}-{run_id_component}.md")
                if not _is_contained(out_dir, projects_root):
                    warnings.append(f"{run_id}: refusing to write outside {projects_root}")
                    continue

            if os.path.exists(out):
                skipped += 1
                continue
            if a.dry_run:
                print(f"would write {out}")
                wrote += 1
                continue

            try:
                os.makedirs(out_dir, exist_ok=True)
                # F5 — a unique temp file per process, published with an
                # atomic no-clobber link+unlink so two overlapping exporters
                # can never truncate each other's in-progress write.
                fd, tmp = tempfile.mkstemp(prefix=f".{os.path.basename(out)}.", suffix=".tmp", dir=out_dir)
                with os.fdopen(fd, "w") as fh:
                    fh.write(build_report(rec, path, warnings))
                try:
                    os.link(tmp, out)
                except FileExistsError:
                    skipped += 1
                else:
                    wrote += 1
            except OSError as e:
                failed += 1
                warnings.append(f"{run_id}: write failed ({redact(str(e))}) -> {redact(out)}")
        except Exception as e:
            # Anything not already handled above (e.g. a field shape
            # validate_record() did not anticipate) is still record-scoped:
            # count it, name the record by its redacted path (safe even if
            # the failure happened before run_id was computed), and move on.
            failed += 1
            warnings.append(f"{path_label}: record failed ({type(e).__name__}: {redact(str(e))})")
        finally:
            if tmp is not None:
                try:
                    os.unlink(tmp)
                except OSError:
                    pass

    for w in warnings:
        print(f"workflow-report-export: WARN {w}", file=sys.stderr)
    print(
        f"workflow-report-export: scanned={len(records)} wrote={wrote} skipped_existing={skipped} "
        f"unmapped={unmapped} nonterminal={nonterminal} unreadable={unreadable} invalid={invalid} "
        f"failed={failed} warnings={len(warnings)}" + (" (dry-run)" if a.dry_run else "")
    )
    # F6/F18 — a per-record validation or write failure fails the overall run
    # (the aggregate count is loud, on stdout, above) without having stopped
    # the scan of the remaining records.
    if unreadable or invalid or failed:
        print(
            f"workflow-report-export: FATAL {unreadable} unreadable, {invalid} invalid, "
            f"{failed} failed to write",
            file=sys.stderr,
        )
        return 1
    # F9 — an explicitly supplied --claude-projects root that does not exist
    # is a misconfiguration, even when it produced zero warnings otherwise
    # (e.g. no records to warn about beyond the missing-root WARN above).
    if source_root_missing:
        return 1
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as e:
        print(f"workflow-report-export: FATAL {type(e).__name__}: {redact(str(e))}", file=sys.stderr)
        sys.exit(1)
