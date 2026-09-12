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
#
# G1 (spec review round 2) — the OTHER prefixed-token alternatives each used
# a lookbehind that additionally excluded a preceding "_"/"-"
# ("(?<![A-Za-z0-9_-])"), unlike the sk- lookbehind above. The harness names
# things exactly like that ("wf_<runId>", "deploy-<name>"), so an identifier
# such as "wf_ghp_<token>" or "deploy-github_pat_<token>" was never even
# tried against the pattern — the leading "_"/"-" blocked the match and the
# complete credential leaked verbatim. Match the sk- lookbehind's boundary
# on every alternative: block only when glued onto another alnum character,
# never on "_"/"-".
#
# A-R2 (spec review round 4) — _redact_deep() redacts each string leaf
# before it is ever serialized (round 2's F1 fix), but that only helps when
# THIS exporter's own json.dumps()/str() is what introduces the escaped
# control character. A raw string leaf can ALREADY contain that escaped
# form before redact() ever sees it — the ordinary shape of a narrator log
# quoting a tool's own JSON-encoded output, e.g. the two literal characters
# backslash-n sitting directly in front of "sk-proj-..." because the log
# text itself embeds already-escaped JSON. The plain alphanumeric
# lookbehind sees the letter "n" (or "t", "r", "f", "b") from that escape
# and blocks the match identically, regardless of when the escaping
# happened. Accept the boundary either when nothing alphanumeric precedes
# (the existing rule) or when the two characters immediately before are a
# backslash followed by one of the control-character escape letters — a
# combination that does not occur in ordinary hyphenated prose next to any
# of these prefixes.
#
# A-R1 (spec review round 4 re-review) — the five backslash-letter escapes
# above (\n \t \r \f \b) are not the only pre-existing escaped form a real
# encoder leaves in front of a credential. json.dumps()/JSON.stringify emit
# a \uXXXX escape for every OTHER control character (and .NET's default
# encoder emits ' for a literal single quote); a repr()-style dump
# emits \xXX; and a URL-encoded form body spells a delimiter as a raw
# percent-triplet (no backslash at all). Every one of these ends in a hex
# digit, which the plain alphanumeric lookbehind still blocks identically
# to the letter case above. Accept those three escaped forms as boundaries
# too — each is a fixed-width lookbehind, and none occurs in ordinary
# hyphenated prose next to any of these prefixes.
_BOUNDARY = (
    r"(?:(?<![A-Za-z0-9])"
    r"|(?<=\\[ntrfb])"
    r"|(?<=\\u[0-9A-Fa-f]{4})"
    r"|(?<=\\x[0-9A-Fa-f]{2})"
    r"|(?<=%[0-9A-Fa-f]{2}))"
)
SECRET_RE = re.compile(
    _BOUNDARY + r"sk-(?:ant-|proj-|live-|test-)?[A-Za-z0-9_-]{20,}"
    r"|" + _BOUNDARY + r"github_pat_[A-Za-z0-9_]{20,}"
    r"|" + _BOUNDARY + r"gh[pousr]_[A-Za-z0-9]{20,}"
    r"|" + _BOUNDARY + r"xox[abps]-[A-Za-z0-9-]{10,}"
    r"|" + _BOUNDARY + r"AKIA[A-Z0-9]{12,}"
    r"|Bearer [A-Za-z0-9._-]{20,}"
    r"|" + _BOUNDARY + r"pit-[a-f0-9-]{20,}"
    # F1 (reviewer A) — the lookbehind excluded a preceding "_", and the
    # alternation was case-sensitive, so underscore-compound names
    # (api_key=, client_secret=, access_token=) and uppercase names
    # (API_KEY=) all survived. Allow "_" immediately before the key name
    # (block only a preceding alnum, same boundary as every other
    # alternative above) and match the key name case-insensitively.
    r"|" + _BOUNDARY + r"(?i:key|token|password|secret)=\S+"
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


# G3 (spec review round 2) — a "tests"/"test" boundary is never allowed to
# be the deciding one when an earlier (further left, i.e. closer to the
# repo root) non-test boundary also exists: a stray tests/ nested inside a
# source dir (".../src/auth/tests/x.py") is still part of that source tree,
# not a second repo boundary.
_TEST_LIKE_DIRS = frozenset({"tests", "test"})


def repo_root_of(path: str, warnings: list[str] | None = None, label: str = "record") -> str | None:
    """Resolve an absolute path to its containing repository root.

    1. On disk: the nearest ancestor that contains `.git` (dir or worktree file).
    2. Clone layout `<host>/<owner>/<repo>/...` (e.g. ~/github.com/acme/widgets/src).
    3. Otherwise strip a trailing file segment, then well-known non-repo
       directories, and take what is left — never the immediate parent of a
       file blindly.

    `warnings`/`label` (G3): when more than one recognized boundary is found,
    a WARN is appended naming both the winning and the rejected candidate.
    """
    p = path.rstrip("/")
    parts = p.split("/")
    work = list(parts)
    # F8 — a trailing segment with an extension is ambiguous on its own (it
    # could be a real file, or a dotted directory like "service.api"), so it
    # never anchors the boundary search itself; it is only consulted
    # afterwards to decide the no-boundary-found / ambiguous case below.
    dir_parts = work[:-1] if work and FILE_EXT_RE.search(work[-1]) else work

    # G1 (round 3, spec review_b's third redo) — the on-disk `.git` walk and
    # the `<host>/<owner>/<repo>` clone-layout check below both used to
    # `return` the moment they found their marker, WITHOUT ever consulting
    # the multi-boundary ambiguity check that follows. A path can carry a
    # `.git` ancestor (or sit under a recognized clone host) and STILL have
    # more than one recognized NON_REPO_DIRS boundary further down (e.g.
    # ".../acme-repo/tests/auth/src/main.py", where `.git` lives at
    # "acme-repo/"): that must be just as ambiguous as the no-shortcut case.
    # So the ambiguity check now runs first, unconditionally, before either
    # shortcut gets a chance to return early.
    #
    # F15 / review_b G3, generalized by G1 (round 3, third pass) — collect
    # EVERY recognized NON_REPO_DIRS segment anywhere in the path, full stop.
    # Earlier passes only counted a boundary whose immediate left neighbor
    # was itself NOT a NON_REPO_DIRS segment (a "predecessor filter"), meant
    # to skip an outer container prefix like ".../workspace/src/acme-repo/
    # src/main.py" (the first "src" sits right after "workspace", a real
    # name, so both occurrences still passed the filter and correctly stayed
    # ambiguous there). But that same filter silently DROPPED any boundary
    # that happened to sit directly next to another boundary with no repo
    # name in between — ".../acme-repo/tests/src/main.py",
    # ".../acme-repo/src/lib/main.py", ".../acme-repo/tests/tests/main.py" —
    # because the inner segment's predecessor was itself a boundary keyword.
    # That left only one surviving candidate, so the ambiguity check below
    # never fired and the path resolved straight to a named project. There
    # is no reliable text-only rule for telling "adjacent boundaries" apart
    # from a genuine single boundary, so every occurrence counts, unfiltered.
    non_test_candidates: list[int] = []
    test_candidates: list[int] = []
    for i in range(len(dir_parts) - 1, 0, -1):
        if dir_parts[i] in NON_REPO_DIRS:
            if dir_parts[i] in _TEST_LIKE_DIRS:
                test_candidates.append(i)
            else:
                non_test_candidates.append(i)
    # Both lists are populated right-to-left, so index 0 is the rightmost
    # (closest-to-file) candidate and index -1 is the leftmost.

    # G1 (round 3, spec review_b's second redo): every earlier round tried to
    # pick a WINNER among multiple candidates (leftmost non-test beats a
    # nested test-like one, leftmost wins on a genuine disagreement,
    # rightmost wins for a same-keyword container-prefix pair) -- but real
    # CLI probes kept finding shapes where some tie-break rule still silently
    # routed to a named project despite the ambiguity (an earlier tests/test/
    # boundary losing unconditionally to a later src/lib/ boundary). There is
    # nothing in the path text alone that reliably tells a genuine
    # disagreement apart from a container-idiom false positive, so the
    # contract is now unconditional: ANY path carrying more than one
    # recognized boundary (test-like or not, same keyword or not) is
    # ambiguous, full stop -- it is never routed to a named project. The
    # caller still gets a loud WARN naming every candidate; the caller's
    # existing "could not resolve a project" handling (routing to
    # `_unmapped`) takes it from there.
    all_candidates = non_test_candidates + test_candidates
    if len(all_candidates) > 1:
        if warnings is not None:
            named = ", ".join(
                f"'{dir_parts[i - 1]}' (boundary '{dir_parts[i]}')" for i in sorted(all_candidates)
            )
            warnings.append(
                f"{label}: multiple source boundaries in path -> ambiguous, "
                f"not routing to a named project ({named})"
            )
        return None

    cur = p
    while cur and cur != "/":
        marker = os.path.join(cur, ".git")
        if os.path.isdir(marker) or os.path.isfile(marker):
            return cur
        parent = os.path.dirname(cur)
        if parent == cur:
            # A-R1 (spec review round 4) — a structured path value is
            # consumed whole (F7/F16) and can carry 2+ leading slashes
            # verbatim (e.g. a "repo" field of "//srv/share/acme"). POSIX
            # treats exactly two or three leading slashes as fixed points
            # under os.path.dirname ("//" -> "//", "///" -> "///"), so
            # without this check the walk above never reaches "/" and loops
            # forever. Stop once dirname() stops making progress.
            break
        cur = parent
    for i, seg in enumerate(parts):
        if seg in CLONE_HOSTS and len(parts) > i + 2:
            return "/".join(parts[: i + 3])

    if non_test_candidates:
        work = dir_parts[: non_test_candidates[0]]
    elif test_candidates:
        work = dir_parts[: test_candidates[0]]
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


def repo_dir_basename(path: str, warnings: list[str] | None = None, label: str = "record") -> str | None:
    """Basename of the repository that contains `path` (see repo_root_of)."""
    root = repo_root_of(path, warnings, label)
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
    map_value = data.get("map", [])
    # F11 (round 2) — a present `map` value that is not a list (e.g. `map = ""`
    # or `map = {}`) is zero-length when enumerated either way, so it was
    # silently treated as "no rules" instead of the configuration error it
    # actually is. Validate the container's type before validating entries.
    if not isinstance(map_value, list):
        raise ValueError(f"workflow-projects.toml: 'map' must be a list (got {type(map_value).__name__})")
    out = []
    for i, entry in enumerate(map_value, start=1):
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


# review_b G1 through A-R2 (rounds 2-4) all tried to tell a genuinely
# space-broken directory name ("Jane Doe/repo" — the match is a truncated
# PREFIX, the real path keeps going) apart from a complete path followed by
# ordinary trailing prose, by counting how many further "/" characters the
# text after the match reaches: zero meant prose, one meant a coincidental
# word pair ("and/or"), two-or-more meant the path kept going. Spec review
# round 4 (ledger arrra-hex-pr-12-r4) disproved that in both directions:
#
# - refute-R1: a deep path whose SECOND component ALSO has a space
#   (".../Jane Doe/acme repo/src/main.py") defeated the counting regex (it
#   required the segment right after the first "/" to be space-free), so
#   the truncated "/Users/Jane" prefix was wrongly accepted as complete.
# - refute-R2 / new-defects-R1: the "second slash" signal fires just as
#   readily for an UNRELATED second relative path mentioned later in the
#   same sentence ("... main.py and updated packages/core/src/index.ts" —
#   a realistic, common shape, not a truncated directory name) as it does
#   for a genuine continuation. Discarding the correct match and resuming
#   the scan one character into the rejected prefix then adopted an
#   arbitrary WRONG suffix as the project ("core", "hooks", "foo", "My",
#   "projects") instead of refusing to guess.
#
# There is no text-only rule that reliably tells these shapes apart by
# counting slashes, so the contract stops trying: a candidate is trusted
# only when `_free_text_ambiguity` finds it UNAMBIGUOUS, and an ambiguous
# candidate is never "resolved" by skipping past it and re-searching — the
# whole record goes to `_unmapped` with a WARN naming both readings. See
# ConservativeFreeTextAmbiguityRouting for the reviewer's exact probes.
_CONTINUATION_TOKEN_RE = re.compile(r"\S+")


def _free_text_ambiguity(text: str, match: re.Match) -> str | None:
    """None if `match` is trusted as a complete path; otherwise a short
    description of the second (rejected) reading, for the caller's WARN.

    A match already ending in a recognized file extension (FILE_EXT_RE)
    can never be a truncated directory-name prefix — no filename is ever
    cut short mid-extension — so it is accepted outright regardless of
    what prose follows (this alone closes refute-R2/new-defects-R1: every
    probe there starts with a complete "...main.py"/"...route.ts" match).

    Otherwise walk the run of whitespace-separated tokens right after the
    match, one hop at a time:
      - The FIRST hop is the strongest signal: a "/" anywhere in it means
        the space could just as well sit inside the real (space-
        containing) directory name as it could start unrelated prose —
        ambiguous immediately, however many "/" that first hop has. This
        no longer depends on the segment after the first "/" being
        space-free, closing refute-R1's second-space-bearing-component
        case.
      - A LATER hop (reached only after at least one earlier, slash-free
        hop already read as ordinary prose) is trusted as a common
        English word-pair idiom ("and/or", "via CI/CD") unless IT ALONE
        carries two or more "/" — a real multi-segment relative path
        ("Smith/acme-repo/src/main.py"), which is still ambiguous.
    """
    if FILE_EXT_RE.search(match.group(0)):
        return None
    pos = match.end()
    first_hop = True
    while True:
        skip_start = pos
        while pos < len(text) and text[pos] == " ":
            pos += 1
        if pos == skip_start:
            break  # no (more) whitespace here -- the continuation run ends
        tm = _CONTINUATION_TOKEN_RE.match(text, pos)
        if not tm:
            break
        token = tm.group(0)
        slashes = token.count("/")
        if (first_hop and slashes >= 1) or slashes >= 2:
            return f"{match.group(0)!r} vs. a continuation through {token!r}"
        first_hop = False
        pos = tm.end()
    return None


def _extract_repo_path(text: str, warnings: list[str] | None = None, label: str = "record") -> str | None:
    """First complete absolute filesystem path in free text (F7/F16): skip
    anything inside a URL. A candidate is used only when `_free_text_ambiguity`
    finds it unambiguous; an ambiguous candidate is never disambiguated by
    skipping past it and re-searching (see the round-5 comment above) — the
    record goes to `_unmapped` instead, with a WARN naming both readings."""
    pos = 0
    while pos < len(text):
        um = URL_RE.search(text, pos)
        pm = ABS_PATH_RE.search(text, pos)
        if pm and (not um or pm.start() < um.start()):
            ambiguity = _free_text_ambiguity(text, pm)
            if ambiguity:
                if warnings is not None:
                    warnings.append(
                        f"{label}: ambiguous free-text path ({ambiguity}) -> not routing to a named project"
                    )
                return None
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
_STRUCTURED_PATH_KEYS = (
    "repo",
    "repoPath",
    "repository",
    "repoDir",
    "path",
    "cwd",
    "workdir",
    "directory",
    # review_b G1 (round 3) — the F7/F16 fix promised repo/path/cwd/
    # workspace fields are all consumed as a complete value, but
    # "workspace" itself was missing: a workspace field's value fell
    # through to the free-text tokenizer and was truncated at its first
    # space, same bug the "repo" key was already fixed for.
    "workspace",
)


# A sentinel `_structured_repo_path` returns when a recognized structured
# key IS present but its value fails the conservative rule below — distinct
# from returning None, which means no such key was present at all. The
# caller (infer_project) must not fall through to another key or down to
# free text in the sentinel case; it routes straight to `_unmapped` instead
# (a WARN naming the reason was already appended when the sentinel was
# returned).
_INVALID_STRUCTURED_PATH = object()


def _invalid_structured_path_reason(value: str) -> str | None:
    """Why a structured field's (already-stripped) string value is
    rejected, or None if it is a usable complete path."""
    if not value.startswith("/"):
        return "not an absolute path"
    if any(ord(c) < 0x20 or c == "\x7f" for c in value):
        return "contains control characters"
    return None


def _structured_repo_path(
    result: object, warnings: list[str] | None = None, label: str = "record"
) -> object:
    """An explicit repo/path field from a structured `result` dict, if any.
    Returns the path string, `_INVALID_STRUCTURED_PATH`, or None (see the
    sentinel's docstring above).

    F7/F16 (round 2) — the value of a field the record itself labeled as a
    repo/path/cwd was still being run through `_extract_repo_path()`, the
    free-text tokenizer built for scanning prose that merely CONTAINS a
    path. A structured field carries no surrounding prose to separate from
    — the whole value IS the path — so it is consumed as a complete value,
    never tokenized, never accepted as a truncated prefix.

    Round 5 (ledger arrra-hex-pr-12-r4, item 1) — CONSERVATIVE RULE: the
    first recognized key present with a value is THE structured field for
    this record, full stop. Its value is either a usable complete absolute
    path or the record is routed straight to `_unmapped` with a WARN
    naming the reason — it never silently falls through to try the next
    key (a `repo` field that turns out to be garbage does not mean "keep
    guessing at `cwd`") or down to the free-text scanner.
    """
    if not isinstance(result, dict):
        return None
    for key in _STRUCTURED_PATH_KEYS:
        value = result.get(key)
        if value is None:
            continue
        if not isinstance(value, str):
            if warnings is not None:
                warnings.append(
                    f"{label}: structured {key!r} field is not a string "
                    f"(got {type(value).__name__}) -> _unmapped"
                )
            return _INVALID_STRUCTURED_PATH
        value = value.strip()
        reason = _invalid_structured_path_reason(value)
        if reason:
            if warnings is not None:
                warnings.append(f"{label}: structured {key!r} field {value!r} rejected ({reason}) -> _unmapped")
            return _INVALID_STRUCTURED_PATH
        return value
    return None


def infer_project(
    rec: dict, project_map: list[tuple[str, str]], warnings: list[str] | None = None, label: str = "record"
) -> str | None:
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
    found = _structured_repo_path(result, warnings, label)
    if found is _INVALID_STRUCTURED_PATH:
        return None
    if not found:
        found = _extract_repo_path(json.dumps(result, ensure_ascii=False), warnings, label)
    if found:
        return repo_dir_basename(found, warnings, label)
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


def _source_marker(path: str) -> str:
    """F10 — stable id for the on-disk source record at `path`, independent
    of destination: the same file always yields the same marker, while two
    unrelated records sharing a basename (see ProjectCollisionSafety) don't."""
    return hashlib.sha256(os.path.abspath(path).encode("utf-8", "surrogateescape")).hexdigest()[:16]


_SOURCE_MARKER_RE = re.compile(r"^<!-- workflow-report-export:source=([0-9a-f]+) -->")


def _report_source_marker(file_path: str) -> str | None:
    """The F10 marker embedded in an existing report's first line, if any."""
    try:
        with open(file_path, encoding="utf-8", errors="replace") as fh:
            m = _SOURCE_MARKER_RE.match(fh.readline())
    except OSError:
        return None
    return m.group(1) if m else None


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


def _redact_deep(obj, warnings: list[str] | None = None, label: str = "record"):
    """Recursively redact every string leaf of a JSON-like structure — dict
    keys, dict values, list items, arbitrarily nested (F1, round 2; dict
    keys added in review_b G3, round 3).

    `render_result()`/log rendering used to serialize (`json.dumps`) or
    stringify (`str()`) a structure FIRST and redact the result afterward.
    Serialization escapes control characters ("\\n" becomes the two
    characters backslash-n), which changes what character sits immediately
    before a credential and can defeat the negative-lookbehind boundary
    check in `redact()`. Redacting each string leaf before it is ever
    serialized means `redact()` always sees the real characters.

    A dict KEY is a string leaf too: a credential used as a key was left
    untouched by the dict-comprehension below, reached json.dumps()/str()
    completely raw, and its escaped form defeated the post-serialization
    belt-and-braces pass exactly like an unredacted value would.
    """
    if isinstance(obj, str):
        return redact(obj, warnings, label)
    if isinstance(obj, dict):
        return {
            (redact(k, warnings, label) if isinstance(k, str) else k): _redact_deep(v, warnings, label)
            for k, v in obj.items()
        }
    if isinstance(obj, list):
        return [_redact_deep(v, warnings, label) for v in obj]
    return obj


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

    # F1 (round 2) — redact string leaves BEFORE render_result() serializes
    # nested values with json.dumps()/str(); the post-serialization redact()
    # below stays as a belt-and-braces pass over the fully rendered text.
    result = _redact_deep(rec.get("result"), warnings, path_label)
    body = render_result(result)
    body = redact(body, warnings, path_label)  # F3 — redact before truncating, not after
    if len(body) > RESULT_CAP:
        cut = len(body) - RESULT_CAP
        body = body[:RESULT_CAP] + f"\n\n**[truncated {cut} chars — full record at {path}]**"

    logs_raw = rec.get("logs") or []
    logs = [_redact_deep(l, warnings, path_label) for l in logs_raw[:LOG_CAP]]
    log_lines = [f"- {str(l).strip()}" for l in logs]
    if len(logs_raw) > LOG_CAP:
        log_lines.append(f"- [{len(logs_raw) - LOG_CAP} more lines in the record]")

    phases = rec.get("phases") or []
    phase_line = ", ".join(p.get("title", "?") for p in phases if isinstance(p, dict)) or "(none declared)"

    md = [
        f"<!-- workflow-report-export:source={_source_marker(path)} -->",  # F10
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

            project_raw = infer_project(rec, project_map, warnings, f"{run_id} ({workflow_name})")
            project = project_raw
            if project_raw is not None:
                project = redact(project_raw, warnings, path_label)
                if project != project_raw:
                    # review_b G1 (round 3) — redact() collapses every
                    # credential-shaped project basename to the same literal
                    # "[REDACTED]" string, so two records from distinct
                    # credential-shaped repos collided on one destination and
                    # the second was silently dropped as "already exported".
                    # Append a short, non-reversible fingerprint of the RAW
                    # project (never the raw value itself) so distinct raw
                    # projects still land in distinct directories.
                    fingerprint = hashlib.sha256(project_raw.encode("utf-8", "surrogateescape")).hexdigest()[:8]
                    project = f"{project}-{fingerprint}"

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
                # F19 — slug() alone can collapse two distinct unsafe ids onto
                # the same normalized string (e.g. "wf/a" and "wf//a" both
                # slug to "wf-a"), silently merging distinct records: the
                # second write hits the same destination and is counted
                # skipped_existing. Append a short, non-reversible fingerprint
                # of the raw (pre-slug) id — same helper the redaction path
                # above uses — so distinct raw ids never collapse.
                fingerprint = hashlib.sha256(run_id.encode("utf-8", "surrogateescape")).hexdigest()[:8]
                run_id_component = f"{slug(run_id) or 'run'}-{fingerprint}"
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
                    # F2 (reviewer A) — even the projects/_unmapped fallback
                    # itself escaped (e.g. it is a symlink outside
                    # $HEX_DIR/projects/). The record is dropped, not
                    # written anywhere: count it as failed so the run's
                    # exit code follows the existing exit-1 rule for
                    # per-record failures instead of silently staying 0.
                    warnings.append(f"{run_id}: refusing to write outside {projects_root}")
                    failed += 1
                    continue

            if os.path.exists(out):
                skipped += 1
                continue
            if a.dry_run:
                # G2 (spec review round 2) — `out` embeds --hex-dir verbatim,
                # which is attacker/user-controlled text just like any other
                # printed path; route it through redact() like every other
                # diagnostic instead of printing it raw.
                print(f"would write {redact(out)}")
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
                    # F10 — a same-basename, same-marker report elsewhere
                    # under projects_root is a stale copy of THIS record
                    # left at a PRIOR destination by a remap; basename alone
                    # is not enough (see ProjectCollisionSafety).
                    marker = _source_marker(path)
                    basename = os.path.basename(out)
                    # B-R2 (spec review round 4) — `basename` is used as a
                    # glob PATTERN here, not an escaped literal. A redacted
                    # run_id's basename embeds the literal text
                    # "[REDACTED]" — a glob character class — so fnmatch
                    # silently collapses it to matching exactly one
                    # character instead of the 11 literal characters
                    # actually on disk, and the stale copy never matches
                    # (no WARN, exit 0). The same escaping also stops a
                    # runId containing "*"/"?" from turning this cleanup
                    # into an over-broad match against unrelated projects'
                    # reports. glob.escape() both directory components so
                    # only real filesystem wildcards (none, here) are ever
                    # interpreted as such.
                    stale_pattern = os.path.join(
                        glob.escape(projects_root), "*", "workflow-reports", glob.escape(basename)
                    )
                    for stale in glob.glob(stale_pattern):
                        # review_b G2 (round 3) — abspath() does not resolve
                        # symlinks: a project directory reached through an
                        # internal alias symlink is a different STRING but
                        # the SAME file as `out`, so abspath() said "not the
                        # same file" and the just-written report was deleted
                        # as if it were a stale copy. realpath() resolves
                        # the symlink so the identity check is physical, not
                        # textual.
                        same = os.path.realpath(stale) == os.path.realpath(out)
                        if same or not _is_contained(stale, projects_root) or _report_source_marker(stale) != marker:
                            continue
                        try:
                            os.unlink(stale)
                            warnings.append(f"{run_id}: removed stale report {redact(stale)} (remapped)")
                        except OSError as e:
                            warnings.append(f"{run_id}: stale report cleanup failed ({redact(str(e))})")
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
        # F1 (review round 2 redo) — G2's fix only routed the --dry-run
        # preview through redact(); these WARN lines can themselves embed
        # `projects_root` (built from --hex-dir, attacker/user-controlled)
        # raw, e.g. the two containment-escape warnings above. Redact every
        # WARN on its way to stderr, same as every other printed path.
        print(f"workflow-report-export: WARN {redact(w)}", file=sys.stderr)
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
