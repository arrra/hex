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
#
# G2 (review_b round 1): a QUOTED *argument* to an already-real, already-
# anchored invocation is not inert the way a quoted standalone COMMAND
# mention is — the shell still passes that exact literal text to the
# command (quoting only suppresses expansion, it doesn't change the
# argument). Blanking a quoted span down to pure spaces erased that
# argument entirely, so a push refspec's quoted leading `+` and a reset's
# quoted `--hard` abstained even if the shell would still force-push /
# hard-reset exactly as it would with the flag unquoted. `_mask_literal_span`
# (below) blanks only the quote delimiters themselves and any
# `_SEPARATOR_CHARS` character found INSIDE the span — the only characters
# that could fake a new command boundary (F2's actual concern, e.g. the `;`
# in a `printf` call whose quoted string mentions a stash invocation) — and
# leaves ordinary argument content (letters, digits, `+`, `-`, `:`, `~`,
# ...) visible. A quoted MENTION like an `echo` of "please do not run a
# stash here" still abstains under this: the protection there has never
# come from blanking the word "stash" — it comes from the mentioned
# subcommand not sitting at a valid `@PREFIX@` command-position anchor
# (`echo`/"please do not run" aren't one of the wrapper keywords `@PREFIX@`
# skips), independent of whether the letters are visible.

_HEREDOC_START_RE = re.compile(r"<<(-)?\s*(?:'([^'\n]*)'|\"([^\"\n]*)\"|([A-Za-z_][A-Za-z0-9_]*))")


def _find_matching_paren(text, open_idx):
    """`text[open_idx]` is '('; return `(index_just_past_close, terminated)`
    -- `terminated` is False when the substitution never closes (matching
    real shell EOF behavior), mirroring `_consume_heredoc_body`'s
    `(index, terminated)` convention so callers can tell a genuine close
    from a truncated one (G1, review_b round 2: `_mask_double_quoted` needs
    this to know whether to exclude a trailing ')' from the body it
    recurses into). Quote-aware (G1, review_b round 1): a `)` inside a
    single- or double-quoted span does not count toward the depth — a real
    shell parses nested quoting when it looks for a `$(...)` substitution's
    true closing paren, so a quoted `)` earlier in the substitution (e.g.
    `$(echo ")")`) must never be mistaken for the real close. Mutually
    recursive with `_skip_double_quoted` so a NESTED `$(...)` inside that
    quoted span is itself parsed the same way. Still a flat scanner, not a
    full shell/paren grammar — sufficient for every rule and fixture in
    this router (spec STOP condition already covers that boundary)."""
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
                return i + 1, True
        i += 1
    return n, False


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
            i, _ = _find_matching_paren(text, i + 1)
            continue
        if ch == "`":
            j = text.find("`", i + 1)
            i = (j + 1) if j != -1 else n
            continue
        i += 1
    return n


def _mask_literal_span(text, start, end, result, quote_char):
    """Blank `text[start:end)` to spaces in `result`, but ONLY the quote
    delimiter itself (`quote_char`) and any `_SEPARATOR_CHARS` character
    (G2, review_b round 1) — ordinary argument content stays visible. See
    the executable-region-scanner comment above for why this is safe.

    A REAL newline inside the span is a `_SEPARATOR_CHARS` member too and
    gets blanked like any other (review R4 regression from G2): leaving it
    visible put a fresh line-start inside quoted text, and `_CMD_PREFIX`'s
    `\\n\\s*` alternative then anchored the next line as if it were a brand
    new command — e.g. a multi-line commit message merely mentioning the
    stash rule's keyword denied, and a multi-line quoted `echo` argument
    mentioning a force-push flag asked. Length-preserving (a blanked
    newline is still one character), so all offsets stay identical; this
    function is never called on heredoc bodies (see
    `_consume_heredoc_body`), so they are unaffected."""
    for k in range(start, end):
        ch = text[k]
        if ch == quote_char or ch in _SEPARATOR_CHARS:
            result[k] = " "


def _mask_span_preserving_substitutions(text, start, end, result):
    """Mask text[start:end) to spaces (newlines untouched), except `$(...)`
    and backtick spans, which stay visible because the shell still executes
    them there (inside double quotes or an unquoted heredoc body)."""
    i = start
    while i < end:
        ch = text[i]
        if ch == "$" and i + 1 < end and text[i + 1] == "(":
            close, _ = _find_matching_paren(text, i + 1)
            i = min(close, end)
            continue
        if ch == "`":
            j = text.find("`", i + 1)
            i = (j + 1) if (j != -1 and j < end) else end
            continue
        if ch != "\n":
            result[i] = " "
        i += 1


def _mask_quotes_recursive(text, start, end, result):
    """Mask single-/double-quoted literal spans within text[start:end),
    leaving executable text visible, and recurse into any `$(...)`/backtick
    substitution found in that range — so a quoted literal several
    substitutions deep is masked too, while the substitution's own
    executable structure (and anything nested inside IT) stays visible.
    Used by `_mask_double_quoted` for the body of a `$(...)`/backtick
    substitution it finds inside a double-quoted span (G1, review_b round
    2): that body was previously left untouched by `_find_matching_paren`
    alone (only its true end was located, quote-aware), so a quoted
    literal nested inside it (e.g. a `printf '%s' '...'` argument quoting a
    fake stash-invocation string) stayed fully visible to Bash rules and
    fired a false deny."""
    i = start
    while i < end:
        ch = text[i]
        if ch == "\\" and i + 1 < end:
            i += 2
            continue
        if ch == "'":
            j = text.find("'", i + 1)
            close = (j + 1) if (j != -1 and j < end) else end
            _mask_literal_span(text, i, close, result, "'")
            i = close
            continue
        if ch == '"':
            i = min(_mask_double_quoted(text, i, result), end)
            continue
        if ch == "$" and i + 1 < end and text[i + 1] == "(":
            raw_close, terminated = _find_matching_paren(text, i + 1)
            if terminated and raw_close <= end:
                close, body_end = raw_close, raw_close - 1
            else:
                close = body_end = min(raw_close, end)
            _mask_quotes_recursive(text, i + 2, body_end, result)
            i = close
            continue
        if ch == "`":
            j = text.find("`", i + 1)
            if j != -1 and j + 1 <= end:
                close, body_end = j + 1, j
            else:
                close = body_end = min((j + 1) if j != -1 else len(text), end)
            _mask_quotes_recursive(text, i + 1, body_end, result)
            i = close
            continue
        i += 1


def _mask_double_quoted(text, start, result):
    """`text[start]` is the opening '"'; mask the double-quoted span,
    preserving `$(...)`/backtick substitutions' executable structure while
    recursively masking any quoted literal NESTED inside one of them (G1,
    review_b round 2 — see `_mask_quotes_recursive`). Returns the index
    just past the closing quote (or len(text) if unterminated).

    A REAL newline inside the span is blanked too (G4, review_b round 2):
    `_mask_literal_span` (single quotes) already blanks it for the same
    reason the R4 fix documents there — leaving it visible put a fresh
    line-start inside quoted text, and `_CMD_PREFIX`'s `\\n\\s*` alternative
    anchored the next line as if it were a brand new command (e.g. a
    double-quoted, multi-line `cd /worktrees/x` mention got picked up by
    `_effective_checkout` as a REAL `cd`). This function had its own
    inline masking loop and was missed by that fix."""
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
            raw_close, terminated = _find_matching_paren(text, i + 1)
            if terminated:
                close, body_end = raw_close, raw_close - 1
            else:
                close = body_end = raw_close
            _mask_quotes_recursive(text, i + 2, body_end, result)
            i = close
            continue
        if ch == "`":
            j = text.find("`", i + 1)
            if j != -1:
                close, body_end = j + 1, j
            else:
                close = body_end = n
            _mask_quotes_recursive(text, i + 1, body_end, result)
            i = close
            continue
        if ch in _SEPARATOR_CHARS:
            # G2: only a shell-metacharacter gets blanked here -- ordinary
            # argument content inside the double-quoted span stays visible
            # (see `_mask_literal_span`/executable-region-scanner comment).
            # G4: a real newline is one of those metacharacters too and
            # must be blanked like any other (see this function's
            # docstring) -- it is never excluded.
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
            _mask_literal_span(text, i, end, result, "'")
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


# --- Effective-checkout resolution for `unless_cwd` (F4) --------------------
#
# `unless_cwd` used to be judged from the hook payload's own `cwd`, blanket
# skipping the whole rule before the command was even inspected. That let a
# shell change the EFFECTIVE checkout per invocation -- via the global
# `-C <path>` option or a preceding `cd <path> &&` -- while the hook's own
# cwd stayed inside `/worktrees/`, so a stash that actually targeted a
# shared checkout escaped protection. `_effective_checkout` resolves the
# real target of ONE invocation instead: a `-C <path>` on the invocation
# itself wins first (the closest override), then the last `cd <path>` command
# anywhere EARLIER in the same shell text (a `cd` persists for every
# subsequent command until superseded, same as real shell state), and only
# falls back to the hook's payload cwd when neither is present. Returns None
# -- "uncertain" -- when the target can't be resolved as a literal path (a
# shell variable, command substitution, or glob) or when `--git-dir=` is
# present (it decouples the repo location from the working tree, so cwd
# alone no longer describes the checkout); callers must treat None as "never
# exempt" so an unresolvable target conservatively keeps protection rather
# than guessing (a cwd substring alone must never exempt another target).
# F8 (review round 2 redo): these locate the `-C`/`cd`/`--git-dir=` KEYWORD
# only -- no argument capture -- so they still match correctly against the
# MASKED scan_text (quoted argument content is blanked to spaces there, but
# the keyword itself, never being inside quotes for a real invocation, is
# untouched; a quoted mention of "cd" INSIDE a string literal is masked away
# too, so it can't be mistaken for a real `cd`). Only a SINGLE trailing
# whitespace char is consumed here (not `\s+`) -- a masked quoted argument
# is indistinguishable from real whitespace in scan_text, so a greedy `\s+`
# would swallow the whole masked span and land past the argument instead of
# at its start. `_read_token` (reading from the UNMASKED text at this same
# offset -- masking preserves length/offsets 1:1) skips any further real
# whitespace itself before parsing the argument.
_DASH_C_LOCATE_RE = re.compile(r"-C\s")
_GIT_DIR_LOCATE_RE = re.compile(r"--git-dir=")
_CD_LOCATE_RE = re.compile(r"(?:^|[;&|(){}\n])\s*cd\s")


def _looks_like_resolvable_path(token):
    if not token or token.startswith("-"):
        return False
    return not any(c in token for c in ("$", "`", "*", "~"))


def _read_token(text, pos):
    """Read one shell argument token starting at `pos` in the UNMASKED
    canonical text. Returns `(value_or_None, end_pos)` -- `end_pos` is the
    offset just past the token itself (past the closing quote, or past the
    unquoted run), so callers that need the token's own SPAN (G1, review_b
    round 1: to check whether a `cd`'s effect is scoped to a subshell that
    closes, or guarded by a `||`, before a later invocation is reached)
    don't have to re-parse it. `value` is None when the token can't be
    resolved to a literal path: an unterminated quote, a double-quoted
    value that still contains `$`/backtick (may expand to anything at
    runtime), or an unquoted token with `$`/backtick/`*`/`~`/a leading `-`
    (a flag, not a path). A single-quoted value is always literal -- single
    quotes suppress all shell expansion, so its content is exactly the
    path."""
    n = len(text)
    while pos < n and text[pos] in " \t":
        pos += 1
    if pos < n and text[pos] == "'":
        end = text.find("'", pos + 1)
        if end == -1:
            return None, n
        return text[pos + 1 : end], end + 1
    if pos < n and text[pos] == '"':
        end = text.find('"', pos + 1)
        if end == -1:
            return None, n
        value = text[pos + 1 : end]
        return (None if any(c in value for c in ("$", "`")) else value), end + 1
    start = pos
    while pos < n and text[pos] not in " \t\n;&|)":
        pos += 1
    token = text[start:pos]
    return (token if _looks_like_resolvable_path(token) else None), pos


def _resolve_against_cwd(path, payload_cwd):
    """A relative literal path (no leading `/`) is resolvable against the
    hook payload's own cwd -- it isn't "uncertain", it just needs joining
    (F4: a bare `cd sub` from a /worktrees/ cwd stays inside that same
    worktree checkout and must not be treated as an unresolvable target)."""
    if path.startswith("/") or not payload_cwd:
        return path
    return os.path.normpath(os.path.join(payload_cwd, path))


def _paren_depths(scan_text):
    """`depths[i]` = net unmatched `(` count over `scan_text[0:i]`, computed
    ONCE per `evaluate()` call (same pattern as `sep_positions`/F14) rather
    than re-walked per candidate. Masking already blanks quoted/commented
    parens to spaces (they aren't real subshells), so only genuine `(...)`
    subshells and the always-visible `$(...)`/backtick substitutions
    (themselves real subshells) are counted here. Used by
    `_effective_checkout` (G1, review_b round 1) to tell whether a `cd`'s
    own enclosing subshell has already closed by the time a later
    invocation is reached."""
    depths = [0] * (len(scan_text) + 1)
    d = 0
    for i, ch in enumerate(scan_text):
        if ch == "(":
            d += 1
        elif ch == ")":
            d -= 1
        depths[i + 1] = d
    return depths


_OR_GUARD_RE = re.compile(r"[ \t]*\|\|")


def _cd_reaches(scan_text, paren_depths, token_start, token_end, target_pos):
    """A `cd` whose own argument occupies `scan_text[token_start:token_end)`
    actually changes the cwd by the time `target_pos` is reached only if
    (G1, review_b round 1): (a) its own enclosing subshell -- if any -- is
    still open at `target_pos`: `(cd /worktrees/x); <stash invocation>`
    must not inherit the subshell-local `cd`, because the `)` closes it
    before the stash ever runs; and (b) it isn't immediately guarded by
    `||`: `cd /worktrees/x || <stash invocation>` only reaches that
    right-hand side when the `cd` FAILED, meaning the directory never
    actually changed. `paren_depths[token_start]` -- not
    `paren_depths[cd_match.start()]` -- is the depth that governs: it's
    measured AFTER the leading separator/`(` that opened `cd`'s own
    enclosing scope has already been counted."""
    if _OR_GUARD_RE.match(scan_text, token_end):
        return False
    enclosing = paren_depths[token_start]
    return min(paren_depths[token_end : target_pos + 1]) >= enclosing


def _effective_checkout(text, scan_text, match_start, match_end, payload_cwd, paren_depths):
    invocation = scan_text[match_start:match_end]
    if _GIT_DIR_LOCATE_RE.search(invocation):
        return None
    c_locates = list(_DASH_C_LOCATE_RE.finditer(invocation))
    if c_locates:
        value, _ = _read_token(text, match_start + c_locates[-1].end())
        return _resolve_against_cwd(value, payload_cwd) if value is not None else None
    cd_locates = list(_CD_LOCATE_RE.finditer(scan_text[:match_start]))
    for cd_match in reversed(cd_locates):
        token_start = cd_match.end()
        value, token_end = _read_token(text, token_start)
        if not _cd_reaches(scan_text, paren_depths, token_start, token_end, match_start):
            # G1: this `cd` never actually took effect by match_start (its
            # subshell closed, or it's guarded by `||`) -- try whatever `cd`
            # came before it instead of falling straight to payload_cwd.
            continue
        return _resolve_against_cwd(value, payload_cwd) if value is not None else None
    return payload_cwd


# --- Loop-body bound for gh-fast-polling (F3) --------------------------
#
# gh-fast-polling's `match` spans a while/until keyword, the CLI call, the
# sleep call, and a closing `\bdone\b`, joined by lazy `[\s\S]*?` gaps --
# lazy, but not BOUNDED to the loop's own `done`: the gaps can skip
# straight past an earlier, unrelated loop's closing `done` (e.g. a
# `while read` loop that has nothing to do with polling) while hunting for
# a CLI-call+sleep pair that actually belongs to a LATER loop. The natural
# fix is a negative lookahead on `done` inside each gap, but Rust's
# `regex` crate (the byte-identical port target) has no lookaround at
# all, so that can't live in the JSON `match` field (see
# TestNoLookaroundInRules). Same shape as F4's `_effective_checkout`: keep
# the JSON regex simple and lookaround-free, and validate the candidate in
# Python -- a match is only genuine if its own span contains exactly one
# `done` word boundary (the one that closes it); two or more means an
# earlier loop's `done` already ended the body before the CLI+sleep pair
# was found.
#
# G3 (review_b round 1): counting `done` tokens and requiring AT MOST ONE
# rejects a genuine outer loop that merely CONTAINS a fully-closed nested
# loop (e.g. `while true; do for i in 1 2; do echo; done; <CLI call>;
# sleep 5; done` has two `done`s -- one for the inner bounded `for`, one
# for the outer `while` -- and both are legitimate). What actually
# distinguishes that from the unrelated-sibling-loops bug this function
# exists to reject is NESTING: a `do`/`done` depth count starting at 0
# must return to exactly 0 for the FIRST time only at matched_text's own
# final loop token. Two sibling loops concatenated (`while A; do...done
# while B; do...done`) touch depth 0 again in the MIDDLE, after the first
# closes, before the second even opens -- that mid-span return to 0 is
# exactly the "crossed into an unrelated loop" case F3/F7 rejects.
#
# G5 (review_b round 2): when the nested bounded loop sits AFTER the
# CLI+sleep pair instead of before it (G3's case), the lazy regex's
# candidate span stops at the NESTED loop's own `done` -- the first `done`
# that satisfies the CLI+sleep requirement -- never reaching the outer
# loop's real terminator further out in scan_text. That candidate's depth
# is still positive at its own end (UNCLOSED, not the mid-span-return-to-0
# CROSSED case), so it is a genuinely too-short match at an otherwise
# correct start position, not a wrong start position at all. The caller
# (`evaluate`) tells these apart via `_polling_loop_extent` and extends an
# UNCLOSED candidate to the next `done` in scan_text instead of discarding
# it; a CROSSED candidate is still rejected outright.
_LOOP_TOKEN_RE = re.compile(r"\b(?:do|done)\b")


def _polling_loop_extent(matched_text):
    """Classify `matched_text`'s `do`/`done` nesting relative to its own
    span. Returns "closed" (a genuine, fully-bounded loop -- possibly
    containing fully-nested loops of its own), "unclosed" (every token
    consumed but depth is still positive -- a nested loop's `done` closed
    before the true outer terminator, which lies further out in scan_text
    than this candidate reached; the caller should extend and re-check),
    or "crossed" (depth returns to 0 somewhere in the MIDDLE of the span --
    an earlier, unrelated loop already closed; the caller must reject this
    span outright, never extend it)."""
    tokens = list(_LOOP_TOKEN_RE.finditer(matched_text))
    if not tokens:
        return "crossed"
    depth = 0
    last = len(tokens) - 1
    for i, tok in enumerate(tokens):
        if tok.group() == "do":
            depth += 1
        else:
            depth -= 1
            if depth < 0:
                return "crossed"
            if depth == 0 and i != last:
                return "crossed"
    return "closed" if depth == 0 else "unclosed"


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
    paren_depths = _paren_depths(scan_text)
    rules = load_rules()

    fires = []
    for rule in rules:
        if not rule["tool_re"].search(tool_name):
            continue
        all_matches = list(rule["match_re"].finditer(scan_text))
        if not all_matches:
            continue

        unless_cwd_re = rule["unless_cwd_re"]

        def _cwd_exempts(candidate, _unless_cwd_re=unless_cwd_re):
            # F4: judged per-invocation from the EFFECTIVE checkout (see
            # `_effective_checkout`), never the blanket hook payload cwd --
            # an unresolved target never exempts (keeps protection).
            if _unless_cwd_re is None:
                return False
            eff_cwd = _effective_checkout(text, scan_text, candidate.start(), candidate.end(), cwd, paren_depths)
            if eff_cwd is None:
                return False
            return bool(_unless_cwd_re.search(eff_cwd))

        unless_re = rule["unless_match_re"]
        scope = rule["unless_scope"]
        match_override = None
        if unless_re is None:
            m = None
            if rule["id"] == "gh-fast-polling":
                # F7 (review round 2 redo): `finditer`'s candidates never
                # overlap, so once the FIRST candidate -- a greedy span
                # crossing an earlier, unrelated loop's own `done` -- got
                # rejected by `_polling_loop_extent`, finditer resumed
                # searching from that rejected span's END, skipping straight
                # past a real loop that started inside it. Re-search from
                # one past the REJECTED candidate's own START (not its end)
                # so every possible start position is still tried.
                search_pos = 0
                while True:
                    candidate = rule["match_re"].search(scan_text, search_pos)
                    if candidate is None:
                        break
                    if _cwd_exempts(candidate):
                        search_pos = max(candidate.start() + 1, candidate.end())
                        continue
                    end = candidate.end()
                    extent = _polling_loop_extent(scan_text[candidate.start():end])
                    # G5 (review_b round 2): a nested bounded loop AFTER the
                    # CLI+sleep pair leaves this lazy candidate UNCLOSED
                    # (its own span never reaches the outer loop's real
                    # terminator) -- extend to the next `done` in scan_text
                    # and re-check, rather than discarding this start
                    # position outright (see `_polling_loop_extent`). A
                    # "crossed" extent (an unrelated earlier loop) is never
                    # extended -- it is genuinely the wrong start.
                    while extent == "unclosed":
                        next_done = _LOOP_TOKEN_RE.search(scan_text, end)
                        if next_done is None:
                            break
                        end = next_done.end()
                        extent = _polling_loop_extent(scan_text[candidate.start():end])
                    if extent != "closed":
                        search_pos = candidate.start() + 1
                        continue
                    m = candidate
                    match_override = scan_text[candidate.start():end]
                    break
            else:
                for candidate in all_matches:
                    if _cwd_exempts(candidate):
                        continue
                    m = candidate
                    break
            if m is None:
                continue
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
                if _cwd_exempts(candidate):
                    continue
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
                if _cwd_exempts(candidate):
                    continue
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
            if _cwd_exempts(all_matches[0]) or unless_re.search(scan_text):
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
                if _cwd_exempts(candidate):
                    continue
                start, end = _window_bounds(
                    sep_positions, len(scan_text), candidate.start(), candidate.end()
                )
                if not unless_re.search(scan_text[start:end]):
                    m = candidate
                    break
            if m is None:
                continue
        # G5: gh-fast-polling's candidate may have been extended past its
        # own (too-short) regex match to reach the loop's real `done`;
        # `match_override` carries that extended span when set.
        matched = (match_override if match_override is not None else m.group(0))[:MATCH_TRUNCATE]
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
