#!/usr/bin/env python3
"""PreToolUse router hook.

Reads a single PreToolUse JSON payload from stdin (see docs/ground-truth in
the spec: {session_id, transcript_path, cwd, permission_mode, hook_event_name,
tool_name, tool_input}), evaluates it against the rules in
`.hex/hooks/router-rules.json`, and emits at most one combined decision on
stdout: any "deny" beats any "ask" beats the first "prior" (in router-rules.json
array order). Every rule that matches ("fires") is appended to the ledger
regardless of whether it won the combined decision; an abstain (no rule
matches) writes nothing and prints nothing.

Fail-open by design: this hook must never block a tool because of a bug in
the router itself. Any internal error (malformed stdin, missing rules file,
bad regex, etc.) is swallowed into exactly one stderr line and the process
still exits 0 with empty stdout.

router-rules.json field reference (JSON has no comment syntax, so the schema
is documented here instead):
  id, tool, match, decision, message, source  -- as before.
  unless_match (optional)  -- a regex on the same canonical text; when it
    matches, the rule does not fire.
  unless_scope (optional)  -- governs WHERE `unless_match` is checked:
    "invocation" - checked CONTIGUOUSLY (re.match, not re.search) starting
      at the tail word of EACH occurrence of `match` (e.g. right where
      "stash" begins in "git-cmd ... stash") — never searched across the rest
      of that "same command" window, so an unrelated unquoted argument
      elsewhere in the SAME invocation (an echoed string, an unquoted `-m`
      message) can never suppress a real stash invocation by accident
      (e.g. `git-stash-shared-checkout`; G3, review_b round 1).
    "shell" - judged from the WHOLE canonical text, for every occurrence,
      because the exemption reflects shell state that protects every later
      pipeline in the same shell (e.g. `pipe-tail-masks-exit`: `set -o
      pipefail` exempts every subsequent piped test command, not just the
      first one) -- but ORDER-sensitive per `unless_match` alternative
      (G2, review_b round 1): an occurrence of `unless_match` tagged with
      the named group `(?P<before>...)` only counts if it appears AT OR
      BEFORE the candidate (persistent state like `pipefail` can never
      retroactively protect a pipeline that already ran unsafely); an
      occurrence NOT tagged `before` (e.g. reading `PIPESTATUS` right
      after the pipe it inspects) still counts anywhere in the text,
      since that idiom is read AFTER the pipe by design.
    unset - default, today's behavior: a single occurrence of `match` uses
      the whole-text check; more than one occurrence uses a per-occurrence
      window (search, not anchored — distinct from "invocation" above,
      only when there are multiple occurrences).
  unless_cwd (optional)  -- a regex on the payload's `cwd`; when it
    matches, the rule does not fire.
  Bash rules write `@PREFIX@`/`@GITOPTS@` placeholders in `match`/
  `unless_match` instead of duplicating the command-position anchor and
  git-global-options skip inline; see `_expand_placeholders` below for the
  one shared definition both expand to.
"""
import json
import os
import re
import sys
from datetime import datetime, timezone

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
RULES_PATH = os.path.join(SCRIPT_DIR, "..", "router-rules.json")
LEDGER_FILENAME = "router-fires.jsonl"
MATCH_TRUNCATE = 200

TEXT_TOOLS = ("Edit", "Write", "MultiEdit", "NotebookEdit")

# Command-boundary characters used to scope `unless_match` when a rule's
# `match` regex has more than one occurrence in one canonical text (e.g. a
# chained shell invocation combining a safe subcommand with a dangerous
# bare one, separated by `;`). Mirrors the anchor tokens already used by
# rule `match` patterns for command-position detection (`[;&|({]` etc,
# plus newline).
_SEPARATOR_CHARS = ";&|(){}\n"

# --- Shared Bash-rule prefix normalization (F3, F9) ------------------------
#
# Every Bash rule anchors its `match` pattern with the placeholder `@PREFIX@`
# in router-rules.json, expanded here to ONE shared fragment (F3: "apply
# across every Bash rule via one normalization step") that skips, before the
# real subcommand is matched: leading whitespace / a command-boundary
# separator, then any mix of `NAME=value` assignment prefixes and
# `time`/`env`/`command`/`exec`/`sudo` wrappers (`env` may itself be
# followed by more assignments, e.g. `env FOO=1 BAR=2 cmd`).
#
# The assignment-name character class is deliberately restricted to
# `[A-Za-z_][A-Za-z0-9_]*` (F9) rather than the old ambiguous `\S+=\S*`:
# `\S+` and `\S*` could both absorb `=` characters, so a token like
# `A=B=C` had more than one way to split across the pattern, and Python's
# backtracking engine explored every combination on adversarial input
# (`env A=B=C A=B=C ... true`). Restricting the name to an unambiguous class
# removes the ambiguity, so each iteration matches exactly one way.
_ASSIGN = r"[A-Za-z_][A-Za-z0-9_]*=\S*"
_WRAPPER_SKIP = (
    r"(?:(?:" + _ASSIGN + r"\s+)*(?:time|env|command|exec|sudo)\s+)*"
    r"(?:" + _ASSIGN + r"\s+)*"
)
_CMD_PREFIX = r"(?:^|[;&|({]\s*|\$\(\s*|\n\s*)\s*" + _WRAPPER_SKIP

# Git global options (F3: "most Git rules also miss global options") skipped
# between `git` and its subcommand: `-C <path>`, `-c k=v`, `--git-dir=...`,
# `--work-tree=...`, any number of times.
_GIT_GLOBAL_OPTS = r"(?:(?:-C\s+\S+|-c\s+\S+|--git-dir=\S+|--work-tree=\S+)\s+)*"


def _expand_placeholders(pattern):
    return pattern.replace("@PREFIX@", _CMD_PREFIX).replace("@GITOPTS@", _GIT_GLOBAL_OPTS)


# --- Executable-region scanner (F2, F13) -----------------------------------
#
# Bash rules must only fire on text the shell actually EXECUTES as a command,
# not on literal quoted text or heredoc bodies that merely happen to look
# like one. `executable_mask` returns a same-length copy of the canonical
# Bash command text with single-quoted spans, double-quoted spans, `#`
# comments, and heredoc bodies replaced by spaces — except `$(...)` and
# backtick command substitutions, which stay visible wherever they occur
# (including inside double quotes and unquoted heredoc bodies), because the
# shell genuinely executes those. Heredoc bodies are bounded to their own
# delimiter (F13: never scanned "past the terminator" into whatever trailing
# commands follow). ONE exception to "same-length": an unquoted heredoc that
# never finds its terminator gets a synthetic `\n<delim>\n` appended to the
# END of the returned text (F8) — real shells consume such a heredoc to EOF,
# and rule regexes must end at an ACTUAL terminator line (never a bare
# `|\Z` escape hatch, which could otherwise match past a REAL terminator
# into unrelated trailing code). This is safe because scan_text is only
# ever indexed/sliced against itself, never against the original text.
#
# This is a quote/heredoc/comment-aware scanner, not a full shell grammar:
# it tracks one quoting/heredoc state at a time in a single left-to-right
# pass and does not attempt nested quoting inside `$(...)`, arithmetic
# expansion, or process substitution. That is sufficient for every rule and
# fixture in this router; a construct needing more than that is out of scope
# (spec STOP condition).

_HEREDOC_START_RE = re.compile(r"<<(-)?\s*(?:'([^'\n]*)'|\"([^\"\n]*)\"|([A-Za-z_][A-Za-z0-9_]*))")


def _find_matching_paren(text, open_idx):
    """`text[open_idx]` is '('; return the index just past its matching ')'
    (or len(text) if unterminated). Quote-aware (G1, review_b round 1): a
    `)` inside a single- or double-quoted span does not count toward the
    depth — a real shell parses nested quoting when it looks for a `$(...)`
    substitution's true closing paren, so a quoted `)` earlier in the
    substitution (e.g. `$(echo ")")`) must never be mistaken for the real
    close. Mutually recursive with `_skip_double_quoted` so a NESTED
    `$(...)` inside that quoted span is itself parsed the same way. Still a
    flat scanner, not a full shell/paren grammar — sufficient for every
    rule and fixture in this router (spec STOP condition already covers
    that boundary)."""
    depth = 0
    n = len(text)
    i = open_idx
    while i < n:
        ch = text[i]
        if ch == "\\" and i + 1 < n:
            i += 2
            continue
        if ch == "'":
            j = text.find("'", i + 1)
            i = (j + 1) if j != -1 else n
            continue
        if ch == '"':
            i = _skip_double_quoted(text, i)
            continue
        if ch == "`":
            j = text.find("`", i + 1)
            i = (j + 1) if j != -1 else n
            continue
        if ch == "(":
            depth += 1
        elif ch == ")":
            depth -= 1
            if depth == 0:
                return i + 1
        i += 1
    return n


def _skip_double_quoted(text, start):
    """`text[start]` is '"'; return the index just past the matching
    closing quote. Recurses into `$(...)` (via `_find_matching_paren`) so a
    `)` inside a NESTED substitution can never be mistaken for the end of
    an ENCLOSING one either (G1)."""
    n = len(text)
    i = start + 1
    while i < n:
        ch = text[i]
        if ch == "\\" and i + 1 < n:
            i += 2
            continue
        if ch == '"':
            return i + 1
        if ch == "$" and i + 1 < n and text[i + 1] == "(":
            i = _find_matching_paren(text, i + 1)
            continue
        if ch == "`":
            j = text.find("`", i + 1)
            i = (j + 1) if j != -1 else n
            continue
        i += 1
    return n


def _mask_span_preserving_substitutions(text, start, end, result):
    """Mask text[start:end) to spaces (newlines untouched), except `$(...)`
    and backtick spans, which stay visible because the shell still executes
    them there (inside double quotes or an unquoted heredoc body)."""
    i = start
    while i < end:
        ch = text[i]
        if ch == "$" and i + 1 < end and text[i + 1] == "(":
            i = min(_find_matching_paren(text, i + 1), end)
            continue
        if ch == "`":
            j = text.find("`", i + 1)
            i = (j + 1) if (j != -1 and j < end) else end
            continue
        if ch != "\n":
            result[i] = " "
        i += 1


def _mask_double_quoted(text, start, result):
    """`text[start]` is the opening '"'; mask the double-quoted span,
    preserving `$(...)`/backtick substitutions. Returns the index just past
    the closing quote (or len(text) if unterminated)."""
    n = len(text)
    result[start] = " "
    i = start + 1
    while i < n:
        ch = text[i]
        if ch == "\\" and i + 1 < n:
            if text[i + 1] != "\n":
                result[i] = " "
                result[i + 1] = " "
            i += 2
            continue
        if ch == '"':
            result[i] = " "
            return i + 1
        if ch == "$" and i + 1 < n and text[i + 1] == "(":
            i = _find_matching_paren(text, i + 1)
            continue
        if ch == "`":
            j = text.find("`", i + 1)
            i = (j + 1) if j != -1 else n
            continue
        if ch != "\n":
            result[i] = " "
        i += 1
    return n


def _consume_heredoc_body(text, start, delim, quoted, strip_tabs, result):
    """Mask the heredoc body starting at `start` (just after the opener's
    newline) up to and including the line that is exactly `delim` (F13: the
    ACTUAL delimiter bounds the body, never `[\\s\\S]*` to end-of-string).
    A quoted delimiter (`<<'EOF'`/`<<"EOF"`) makes the whole body inert; an
    unquoted one still allows `$(...)`/backtick substitution in the body.
    Returns `(index, terminated)`: index is just past the terminator line
    (or len(text) if the heredoc is never terminated, matching real shell
    behavior of consuming to EOF); `terminated` is False only in that
    never-closed case (F8: callers use it to represent the EOF-close in
    scan_text without ever scanning past a REAL terminator)."""
    n = len(text)
    i = start
    while True:
        nl = text.find("\n", i)
        line_end = nl if nl != -1 else n
        line = text[i:line_end]
        check_line = line.lstrip("\t") if strip_tabs else line
        if check_line == delim:
            return (n if nl == -1 else nl + 1), True
        if quoted:
            for k in range(i, line_end):
                if text[k] != "\n":
                    result[k] = " "
        else:
            _mask_span_preserving_substitutions(text, i, line_end, result)
        if nl == -1:
            return n, False
        i = nl + 1


def executable_mask(text):
    n = len(text)
    result = list(text)
    i = 0
    pending_heredocs = []
    while i < n:
        ch = text[i]
        if ch == "#" and (i == 0 or text[i - 1] in " \t\n;&|(){}"):
            j = text.find("\n", i)
            end = j if j != -1 else n
            for k in range(i, end):
                result[k] = " "
            i = end
            continue
        if ch == "'":
            j = text.find("'", i + 1)
            end = (j + 1) if j != -1 else n
            for k in range(i, end):
                if text[k] != "\n":
                    result[k] = " "
            i = end
            continue
        if ch == '"':
            i = _mask_double_quoted(text, i, result)
            continue
        if ch == "<" and text.startswith("<<", i) and not text.startswith("<<<", i):
            m = _HEREDOC_START_RE.match(text, i)
            if m:
                strip_tabs = m.group(1) == "-"
                if m.group(2) is not None:
                    delim, quoted = m.group(2), True
                elif m.group(3) is not None:
                    delim, quoted = m.group(3), True
                else:
                    delim, quoted = m.group(4), False
                pending_heredocs.append((delim, quoted, strip_tabs))
                i = m.end()
                continue
            i += 1
            continue
        if ch == "\n" and pending_heredocs:
            i += 1
            while pending_heredocs:
                delim, quoted, strip_tabs = pending_heredocs.pop(0)
                i, terminated = _consume_heredoc_body(text, i, delim, quoted, strip_tabs, result)
                if not terminated and not quoted:
                    # F8: an unquoted heredoc that never finds its terminator
                    # still consumes to EOF in real shells, so its backtick
                    # substitutions run for the whole body. The
                    # backticks-in-unquoted-heredoc rule's match regex must
                    # end at an ACTUAL `\1` terminator line (F8: no bare
                    # `|\Z` fallback, or it can scan past a REAL terminator
                    # into unrelated trailing code). Append a synthetic
                    # terminator line to scan_text only (never to the raw
                    # command text used elsewhere) so that regex still
                    # fires for the genuinely-unterminated case.
                    result.append("\n")
                    result.extend(delim)
                    result.append("\n")
            continue
        i += 1
    return "".join(result)


def _bisect_right(values, x):
    """Insertion point for `x` in sorted `values`, after any equal entries
    (stdlib `bisect.bisect_right`, reimplemented — the hook scripts stay
    pure-stdlib with an explicit import allowlist that doesn't include
    `bisect`)."""
    lo, hi = 0, len(values)
    while lo < hi:
        mid = (lo + hi) // 2
        if values[mid] <= x:
            lo = mid + 1
        else:
            hi = mid
    return lo


def _bisect_left(values, x):
    """Insertion point for `x` in sorted `values`, before any equal entries
    (stdlib `bisect.bisect_left`, reimplemented — see `_bisect_right`)."""
    lo, hi = 0, len(values)
    while lo < hi:
        mid = (lo + hi) // 2
        if values[mid] < x:
            lo = mid + 1
        else:
            hi = mid
    return lo


def _window_bounds(sep_positions, text_len, start, end):
    """The substring boundaries of the "same command" as the occurrence at
    [start, end): from just after the nearest separator at or before
    `start` to just before the nearest separator at or after `end` (or the
    string edges). Comparing with `<=`/`>=` (not strict `<`/`>`) matters: a
    rule's `match` regex often captures its own leading separator as part of
    the anchor (e.g. a `;`-prefixed subcommand), so the separator can sit
    exactly at `start` and must still bound the window rather than being
    skipped over.

    F14: `sep_positions` (the sorted separator offsets in the canonical
    text) is computed ONCE per `evaluate()` call and looked up here via
    binary search, not rebuilt by scanning the whole text for every
    candidate occurrence — the old approach made a large all-exempt command
    (thousands of chained invocations) quadratic."""
    idx = _bisect_right(sep_positions, start) - 1
    window_start = (sep_positions[idx] + 1) if idx >= 0 else 0
    idx2 = _bisect_left(sep_positions, end)
    window_end = sep_positions[idx2] if idx2 < len(sep_positions) else text_len
    return window_start, window_end


def canonical_text(tool_name, tool_input):
    """Canonical arg text a rule's `match` regex is applied to."""
    if tool_name == "Bash":
        return str(tool_input.get("command", ""))
    if tool_name in TEXT_TOOLS:
        parts = [str(tool_input.get("file_path", ""))]
        if "new_string" in tool_input:
            parts.append(str(tool_input.get("new_string", "")))
        if "content" in tool_input:
            parts.append(str(tool_input.get("content", "")))
        edits = tool_input.get("edits")
        if isinstance(edits, list):
            for edit in edits:
                if isinstance(edit, dict):
                    parts.append(str(edit.get("new_string", "")))
        return "\n".join(parts)
    return json.dumps(tool_input, sort_keys=True, separators=(",", ":"))


def load_rules():
    with open(RULES_PATH, "r") as f:
        raw_rules = json.load(f)
    compiled = []
    for rule in raw_rules:
        unless_cwd = rule.get("unless_cwd")
        unless_match = rule.get("unless_match")
        match_pattern = _expand_placeholders(rule["match"])
        unless_match_pattern = _expand_placeholders(unless_match) if unless_match else None
        compiled.append(
            {
                "id": rule["id"],
                "tool_re": re.compile(rule["tool"], re.MULTILINE),
                "match_re": re.compile(match_pattern, re.MULTILINE),
                "unless_cwd_re": re.compile(unless_cwd, re.MULTILINE) if unless_cwd else None,
                "unless_match_re": re.compile(unless_match_pattern, re.MULTILINE) if unless_match_pattern else None,
                # F1/F10: per-rule exemption scope. "invocation" = unless_match
                # is judged from THIS occurrence's own command window only
                # (e.g. the stash rule: a safe-looking subcommand elsewhere in
                # the text must never suppress a real invocation). "shell" =
                # unless_match is judged from the whole canonical text, for
                # every occurrence, because the exempting shell state (e.g.
                # `set -o pipefail`) protects every later pipeline in the same
                # shell, not just the first one a window happens to cover.
                # Default (unset) = today's behavior: a single occurrence uses
                # the whole-text check, multiple occurrences use per-window.
                "unless_scope": rule.get("unless_scope"),
                "decision": rule["decision"],
                "message": rule.get("message", ""),
            }
        )
    return compiled


def ledger_path():
    ledger_dir = os.environ.get("HEX_LEDGER_DIR") or os.path.join(
        os.path.expanduser("~"), ".hex", "ledger"
    )
    os.makedirs(ledger_dir, exist_ok=True)
    return os.path.join(ledger_dir, LEDGER_FILENAME)


def evaluate(payload):
    tool_name = payload.get("tool_name", "")
    tool_input = payload.get("tool_input") or {}
    cwd = payload.get("cwd", "")
    session_id = payload.get("session_id", "")

    text = canonical_text(tool_name, tool_input)
    # F2/F13: Bash command rules only ever see EXECUTABLE text — literal
    # quoted/commented/heredoc-body text is masked to spaces first (real
    # `$(...)`/backtick substitutions stay visible). Other tools' canonical
    # text (file paths/content) isn't Bash syntax, so it is used as-is.
    scan_text = executable_mask(text) if tool_name == "Bash" else text
    sep_positions = [i for i, ch in enumerate(scan_text) if ch in _SEPARATOR_CHARS]
    rules = load_rules()

    fires = []
    for rule in rules:
        if not rule["tool_re"].search(tool_name):
            continue
        if rule["unless_cwd_re"] is not None and rule["unless_cwd_re"].search(cwd):
            continue
        all_matches = list(rule["match_re"].finditer(scan_text))
        if not all_matches:
            continue
        unless_re = rule["unless_match_re"]
        scope = rule["unless_scope"]
        if unless_re is None:
            m = all_matches[0]
        elif scope == "shell":
            # Shell-wide, ORDER-sensitive (G2, review_b round 1): the
            # exemption is checked against the WHOLE text, for every
            # occurrence, because it reflects shell state (e.g. `set -o
            # pipefail`) that protects every later pipeline in the same
            # shell, not just the first one a window happens to cover — BUT
            # a `unless_match` occurrence tagged with the named group
            # `before` only counts if it appears AT OR BEFORE this
            # candidate: persistent state like `pipefail` must already be
            # in effect, it can never retroactively protect a pipeline that
            # already ran unsafely. An `unless_match` occurrence that is
            # NOT tagged `before` (e.g. reading `PIPESTATUS` right after
            # the pipe it's inspecting) keeps the original anywhere-in-text
            # check, since that idiom is read AFTER the pipe by design.
            m = None
            for candidate in all_matches:
                exempted = False
                for um in unless_re.finditer(scan_text):
                    if um.lastgroup == "before" and um.end() > candidate.start():
                        continue  # set too late to protect this occurrence
                    exempted = True
                    break
                if not exempted:
                    m = candidate
                    break
            if m is None:
                continue
        elif scope == "invocation":
            # Invocation-local, ANCHORED not searched (G3, review_b round
            # 1): the exemption must be checked CONTIGUOUSLY from the tail
            # word of THIS occurrence's own match (e.g. "stash"), never
            # searched across the rest of the "same command" window —
            # otherwise an unrelated, unquoted argument elsewhere in the
            # SAME invocation (e.g. an unquoted `-m` message) could contain
            # safe-looking text and wrongly exempt a genuinely dangerous
            # subcommand (a `stash push -m stash pop` invocation).
            m = None
            for candidate in all_matches:
                matched_text = candidate.group(0)
                tail = re.search(r"\w+\Z", matched_text)
                anchor = candidate.start() + (tail.start() if tail else len(matched_text))
                if unless_re.match(scan_text, anchor) is None:
                    m = candidate
                    break
            if m is None:
                continue
        elif len(all_matches) == 1:
            # Default, single occurrence: unchanged whole-text check (some
            # rules rely on a safety marker appearing ANYWHERE in the text,
            # not just next to the match).
            if unless_re.search(scan_text):
                continue
            m = all_matches[0]
        else:
            # Default, multiple occurrences in one canonical text (e.g. a
            # chained Bash command with both a safe and a dangerous
            # invocation): judge each occurrence by its own "same command"
            # window so one safe occurrence can't blanket-suppress a
            # dangerous sibling.
            m = None
            for candidate in all_matches:
                start, end = _window_bounds(
                    sep_positions, len(scan_text), candidate.start(), candidate.end()
                )
                if not unless_re.search(scan_text[start:end]):
                    m = candidate
                    break
            if m is None:
                continue
        matched = m.group(0)[:MATCH_TRUNCATE]
        fires.append(
            {
                "id": rule["id"],
                "decision": rule["decision"],
                "message": rule["message"],
                "match": matched,
            }
        )

    if fires:
        ts = datetime.now(timezone.utc).isoformat()
        lines = []
        for fire in fires:
            entry = {
                "ts": ts,
                "session_id": session_id,
                "rule_id": fire["id"],
                "tool": tool_name,
                "decision": fire["decision"],
                "match": fire["match"],
                # First 300 chars of the canonical text (Bash: the command). The bare regex
                # match (e.g. just the subcommand that tripped a rule) cannot explain a
                # prompt after the fact, nor let step 3 cluster fires by context
                # (2026-09-06 04:06Z operator question).
                "preview": text[:300],
                "cwd": cwd,
            }
            lines.append(json.dumps(entry, sort_keys=True))
        with open(ledger_path(), "a") as lf:
            lf.write("\n".join(lines) + "\n")

    winner = None
    for decision in ("deny", "ask", "prior"):
        for fire in fires:
            if fire["decision"] == decision:
                winner = fire
                break
        if winner is not None:
            break

    return winner


def main():
    raw = sys.stdin.read()
    payload = json.loads(raw)

    winner = evaluate(payload)
    if winner is None:
        return  # abstain: no stdout

    hso = {"hookEventName": "PreToolUse"}
    if winner["decision"] in ("deny", "ask"):
        hso["permissionDecision"] = winner["decision"]
        hso["permissionDecisionReason"] = winner["message"]
    else:
        hso["additionalContext"] = winner["message"]

    sys.stdout.write(json.dumps({"hookSpecificOutput": hso}))
    sys.stdout.write("\n")


if __name__ == "__main__":
    try:
        main()
    except Exception as exc:  # fail-open: never block a tool on our own bug
        sys.stderr.write(f"[router] error: {exc}\n")
    sys.exit(0)
