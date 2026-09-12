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

# --- Shared secret-redaction policy (F7) ------------------------------------
#
# DUPLICATED VERBATIM in posttoolusefailure-incident.py. The router runs as
# `python3 -I -S <script>` (isolated mode -- confirmed ground truth), so
# neither hook script can import a sibling module; keeping this one policy
# byte-identical in both files is the only way to apply it consistently.
# Covers current API-key/token shapes so persisted ledger text (match,
# preview, incident error/args_preview) never carries a live credential.
_REDACT_PATTERNS = [
    (re.compile(r"sk-ant-[A-Za-z0-9\-_]{8,}"), "sk-ant-***REDACTED***"),
    # G1 (review_b round 3): real OpenAI-shaped keys (sk-proj-..., sk-svcacct-...)
    # use hyphens/underscores inside the key body, not just alnum -- the old
    # alnum-only charset stopped at the first "-" and left most of the key
    # (everything after "proj"/"svcacct") unredacted.
    (re.compile(r"sk-[A-Za-z0-9\-_]{8,}"), "sk-***REDACTED***"),
    (re.compile(r"ghp_[A-Za-z0-9]{16,}"), "***REDACTED-GH-TOKEN***"),
    (re.compile(r"github_pat_[A-Za-z0-9_]{16,}"), "***REDACTED-GH-TOKEN***"),
    (re.compile(r"xox[abp]-[A-Za-z0-9\-]{8,}"), "***REDACTED-SLACK-TOKEN***"),
    (re.compile(r"AKIA[A-Z0-9]{16}"), "***REDACTED-AWS-KEY***"),
    (re.compile(r"(?i)\bpit-[A-Za-z0-9\-_]{8,}"), "pit-***REDACTED***"),
    (re.compile(r"(?i)bearer\s+\S+"), "Bearer ***REDACTED***"),
    # G2b (review_b round 3): the PEM-block pattern MUST run before the
    # generic `secret=`/`token=` pattern below -- that pattern's value is
    # `\S+` (stops at the first whitespace), so a `secret=` immediately
    # before a PEM block used to eat only "-----BEGIN" and leave the rest
    # of the (now unrecognizable) PEM body, key material included, exposed.
    (
        re.compile(
            r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----[\s\S]*?"
            r"-----END [A-Z0-9 ]*PRIVATE KEY-----"
        ),
        "***REDACTED-PEM-BLOCK***",
    ),
    # A-R6 (round 2 reopen review): the key=value patterns below all anchor
    # on `\bpassword\s*=\s*` and only look for a quote AFTER the `=` -- two
    # real shell quoting shapes never reach that far because the quote
    # comes BEFORE the key name (the whole `key=value` pair is one quoted
    # argument, e.g. a push command's `-o "password=alpha bravo charlie"`)
    # or uses `$'...'` ANSI-C quoting instead of `"..."`/`'...'`
    # (`password=$'alpha bravo charlie'`). Neither reaches any quoted-value
    # alternative below, so both drop straight to the bare `\S+` one and
    # only the first word gets redacted. These two patterns must run
    # BEFORE the generic pattern for the same reason the PEM pattern does
    # (a partial match by the generic pattern would eat the leading
    # `"`/`$'` and leave the rest unrecognizable).
    (
        re.compile(
            r"""(?i)"(password|token|secret|api[_-]?key)\s*=\s*"""
            r"""(?:[^"\\]|\\[\s\S])*\""""
        ),
        r'"\1=***REDACTED***"',
    ),
    (
        re.compile(
            r"""(?i)'(password|token|secret|api[_-]?key)\s*=\s*[^']*'"""
        ),
        r"'\1=***REDACTED***'",
    ),
    (
        re.compile(
            r"""(?i)\b(password|token|secret|api[_-]?key)\s*=\s*"""
            r"""\$'(?:[^'\\]|\\.)*'"""
        ),
        r"\1=***REDACTED***",
    ),
    # G2a (review_b round 3): the value used to be `\S+`, so a quoted value
    # containing spaces (`password="hunter two secret"`) only redacted up
    # to the first space and leaked the rest of the phrase. Prefer a
    # quoted value (single or double) when present, else fall back to the
    # original single-token match.
    # G2 (review round 3 redo): args_preview is built from
    # `redact(json.dumps(tool_input))`, so a quoted value is JSON-escaped
    # (`password=\"hunter two secret\"`) -- the bare-double-quote
    # alternation never matched that shape and dropped to the `\S+`
    # fallback, which stopped at the first space. Try the JSON-escaped
    # double-quoted form first, then the bare quoted forms, then the
    # single-token fallback.
    # G2 (review_b round 4): the bare-double-quoted alternative was
    # `"[^"]*"` -- no escape awareness, so a RAW (non-JSON) value with a
    # backslash-escaped inner quote (`password="alpha \"bravo\" charlie"`,
    # valid bash: `\"` inside double quotes is a literal quote) made
    # `[^"]*` stop at that embedded quote instead of the real closing one.
    # Only the leading fragment got redacted and the rest of the value
    # leaked in the clear. `(?:[^"\\]|\\.)*` walks past any
    # backslash-escaped character (including an escaped quote) and only
    # stops at a real, unescaped closing quote.
    # G2 continued (review_b round 5): `\\.` requires `.`, which -- with
    # no re.DOTALL on this pattern -- never matches a real newline. A
    # trailing backslash immediately before a real newline is valid bash
    # line-continuation inside a double-quoted string (folds away at
    # execution time), but the old `\\.` couldn't step past it: the
    # closure exited early, the quoted alternative failed to find its
    # closing quote there, and the whole thing dropped to the `\S+`
    # single-token fallback, leaking everything after the newline.
    # `\\[\s\S]` matches an escaped character INCLUDING a newline without
    # needing re.DOTALL (which would also loosen unrelated `.` uses
    # elsewhere in this pattern).
    # R7 (round 3): the UNQUOTED fallback was a bare `\S+`, so a bash
    # backslash-escaped SPACE (the third shell-quoting mechanism, alongside
    # single/double quotes -- `password=alpha\ bravo\ charlie` is ONE shell
    # word) stopped at the first escaped space and leaked the rest.
    # `(?:[^\s\\]|\\.)+` walks past an escaped character (including an
    # escaped space) the same way the quoted alternatives above already
    # walk past an escaped quote.
    (
        re.compile(
            r"""(?i)\b(password|token|secret|api[_-]?key)\s*=\s*"""
            r"""(\\"(?:[^"\\]|\\[\s\S])*\\"|"(?:[^"\\]|\\[\s\S])*"|'[^']*'|(?:[^\s\\]|\\.)+)"""
        ),
        r"\1=***REDACTED***",
    ),
]


def redact(text):
    """Scrub every known secret shape out of `text`. A no-op (returns the
    same string) when nothing matches -- ordinary command/path text is
    returned unmodified."""
    if not text:
        return text
    for pattern, repl in _REDACT_PATTERNS:
        text = pattern.sub(repl, text)
    return text


def _ensure_private_dir(path):
    """Create `path` (and parents) if missing, then force 0700 regardless
    of the process umask or a pre-existing directory with looser
    permissions (F7: 'existing paths handled explicitly' -- `os.makedirs`'s
    own `mode=` argument is masked by umask AND is a no-op when the
    directory already exists, so an explicit `chmod` after the fact is the
    only way to guarantee this)."""
    os.makedirs(path, mode=0o700, exist_ok=True)
    os.chmod(path, 0o700)


def _open_private_append(path):
    """Open `path` for append, creating it 0600 if new; also force 0600 on
    an already-existing file (same 'existing paths handled explicitly'
    reasoning as `_ensure_private_dir` -- the mode passed to `os.open` only
    applies when the file is newly created)."""
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
    os.chmod(path, 0o600)
    return os.fdopen(fd, "a")

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
#
# R4 (round 3): the value used to be a bare `\S*`, which can never span a
# QUOTED value containing a real space (`FOO="a b" cmd`) -- masking blanks
# the quote DELIMITERS by default (see the executable-region-scanner
# comment below), so by the time this pattern runs against scan_text the
# quote characters are already gone and only the raw spaces remain,
# indistinguishable from real argument-separating whitespace. The scanner
# special-cases an assignment's own value (`_is_assignment_value_quote`)
# to leave ITS quote delimiters visible instead, specifically so this
# pattern's quoted-value alternatives have real quote characters to match
# against; ordinary argument content stays visible either way, so the
# interior of the value is unaffected.
#
# A-REFUTE-3: real bash concatenates adjacent unquoted/quoted fragments
# with no separating whitespace into ONE word (`FOO=bar"baz qux"` assigns
# `barbaz qux`), but R4's alternation could only ever match a SINGLE
# segment -- a value built from more than one fragment left a trailing
# unmatched remainder right after the assignment, which broke
# `_WRAPPER_SKIP`'s own `(?:_ASSIGN\s+)*` iteration (no whitespace follows
# an assignment that didn't consume its own full word) and left the real
# subcommand short of command position.
#
# The value is structured as an optional leading unquoted run, then zero
# or more (quoted/ANSI-C-quoted segment, optional trailing unquoted run)
# pairs -- NOT a star wrapped directly around an alternation that
# includes the unquoted-run branch itself (`(?:[^\s'"]*|'...'|...)*`,
# tried first and reverted after the ReDoS regression below). Each
# iteration of the outer star is forced to open on an actual quote
# character, so there is exactly one way to parse any given input: an
# unquoted run can only ever be matched by the ONE `[^\s'"]*` slot
# immediately before it (the leading one, or the one right after the
# preceding quoted segment) -- unlike the reverted version, where a long
# unquoted run could be split across an arbitrary number of star
# iterations in exponentially many ways, hanging the router on adversarial
# input (F9 all over again, this time inside the value rather than the
# name -- confirmed via TestAssignmentPrefixReDoS's own adversarial
# fixture, `env ` + `A=B=C ` * 24 + `true`, which has no quotes in it at
# all and so exercises only the leading `[^\s'"]*`, matched once).
_ASSIGN_VALUE = (
    r"""[^\s'"]*"""
    r"""(?:(?:'[^']*'|"(?:[^"\\]|\\[\s\S])*"|\$'(?:[^'\\]|\\.)*')[^\s'"]*)*"""
)
_ASSIGN = r"[A-Za-z_][A-Za-z0-9_]*=" + _ASSIGN_VALUE
_WRAPPER_SKIP = (
    r"(?:(?:" + _ASSIGN + r"\s+)*(?:time|env|command|exec|sudo|builtin)\s+)*"
    r"(?:" + _ASSIGN + r"\s+)*"
)
# G3 (spec-level review, reopen generation 2): a bare backtick is a valid
# command-position anchor too, the same as `\$\(` -- an opening backtick
# genuinely starts a new backtick command substitution's script text, so
# `` `<cmd>\n<subcmd>` `` anchors `<cmd>` exactly where `$(<cmd> <subcmd>)`
# already does. (A backtick reuses the SAME character to open and close, unlike
# `$(`/`)`, so this can also anchor right after a CLOSING backtick; that
# only misfires if a substitution's output is immediately, unspacedly
# glued to a real command word, which no rule/fixture in this router
# exercises and which real shell word-concatenation makes vanishingly
# rare in practice.)
#
# R3 (round 3): a reserved word (if/then/elif/else/while/until/do) is
# ALSO a valid command-position lead-in -- a real invocation right after
# `then` genuinely starts a new command the same way one right after `;`
# does, but none of those reserved words are `_SEPARATOR_CHARS`. No
# lookaround is available (Rust `regex` port target), so this is a plain
# alternative in the same top-level group as the separator/start-of-text
# ones, not a lookbehind assertion on the reserved word's OWN position --
# a reserved word that is itself only a QUOTED MENTION (an echoed string
# containing the word `then`) will over-match here rather than abstain.
# That is the router's existing conservative bias (ask/deny-leaning over
# silent abstention): the fail-safe direction for a permission gate is an
# unnecessary prompt, not a missed dangerous command.
_RESERVED_LEADIN = r"(?:if|then|elif|else|while|until|do)\s+"
_CMD_PREFIX = (
    r"(?:^|[;&|({]\s*|\$\(\s*|`\s*|\n\s*|" + _RESERVED_LEADIN + r")\s*"
    + _WRAPPER_SKIP
)

# Git global options (F3: "most Git rules also miss global options") skipped
# between `git` and its subcommand: `-C <path>`, `-c k=v`, `--git-dir=...`,
# `--work-tree=...`, any number of times.
_GIT_GLOBAL_OPTS = r"(?:(?:-C\s+\S+|-c\s+\S+|--git-dir=\S+|--work-tree=\S+)\s+)*"

# F3 (round 2 review, major): the argument alternatives above are all
# `\S+` -- they consume only up to the first WHITESPACE character. A
# quoted argument containing a space (`-C '/shared/my repo'`,
# `-c 'user.name=A B'`) leaves the remainder of the quoted value
# unconsumed, so the compiled rule regex fails to match AT ALL from that
# point on (it can neither loop back for another global option, since the
# leftover text isn't one, nor reach the subcommand alternation, since the
# leftover text isn't that either) -- the whole invocation abstains rather
# than denying/asking. `_GIT_OPT_TAKING_ARG` names the flags this applies
# to; `_widen_quoted_global_opt_args` (below) rewrites `scan_text` so
# `\S+` can consume past such an argument's internal spaces as one run.
_GIT_OPT_TAKING_ARG = ("-C", "-c", "--git-dir=", "--work-tree=")


def _widen_quoted_global_opt_args(text, scan_text, quote_spans):
    """Returns a copy of `scan_text` with every INTERNAL space of a
    QUOTED `-C`/`-c`/`--git-dir=`/`--work-tree=` argument value replaced
    with `\\x01` -- a byte `\\S` still matches, so `_GIT_GLOBAL_OPTS`'s
    `\\S+` alternatives can consume the whole quoted value as one token
    exactly the way a real shell treats it as one argument. Never changes
    `scan_text`'s length, so every other position-based computation
    downstream (sep_positions, paren_depths, quote_spans itself) stays
    valid unchanged.

    Uses `quote_spans` (already collected by `executable_mask`) rather
    than re-deriving quote boundaries: for each occurrence of one of
    these flags in the ORIGINAL `text` (found there, not in `scan_text`,
    since a quote's own delimiter characters are already blanked to
    spaces by the time `scan_text` exists), checks whether a recorded
    span starts EXACTLY at the argument's first character — i.e. the
    argument genuinely opens with a quote, immediately after the flag
    (and its own `=`, for the two long-option forms) or after `-C`/`-c`
    plus whitespace. An unquoted argument (no span starts there) is left
    untouched; `\\S+` already handles it correctly."""
    out = list(scan_text)

    def _widen_span_if_quoted_at(arg_start):
        for q_start, q_end in quote_spans:
            if q_start == arg_start:
                for k in range(q_start, q_end):
                    if out[k] == " ":
                        out[k] = "\x01"
                return

    for opt in _GIT_OPT_TAKING_ARG:
        search_from = 0
        while True:
            idx = text.find(opt, search_from)
            if idx == -1:
                break
            search_from = idx + len(opt)
            # Boundary check: `opt` must not be the tail of a longer
            # token (e.g. the "-C" inside some other flag spelling).
            if idx > 0 and (text[idx - 1].isalnum() or text[idx - 1] in "-_"):
                continue
            j = idx + len(opt)
            if opt in ("-C", "-c"):
                # These take a SEPARATE argument after whitespace; the
                # long `--...=` forms glue the value on with no space.
                while j < len(text) and text[j] == " ":
                    j += 1
            _widen_span_if_quoted_at(j)

    return "".join(out)


def _expand_placeholders(pattern):
    return pattern.replace("@PREFIX@", _CMD_PREFIX).replace("@GITOPTS@", _GIT_GLOBAL_OPTS)


# R4 (round 3): a quote immediately preceded by `NAME=` (optionally with a
# `$` right before it, for `NAME=$'...'` ANSI-C quoting) is an assignment
# PREFIX's own value, not an ordinary quoted argument/mention -- the
# executable-mask scanner leaves such a quote's DELIMITERS visible (see
# `mask_delims` on `_mask_literal_span`/`_mask_double_quoted`) so `_ASSIGN`'s
# quoted-value alternatives can match the real characters in scan_text.
# Deliberately not anchored to "genuine command position" beyond this local
# check: no rule regex references a literal quote character, so leaving one
# extra quote visible elsewhere is inert everywhere else in this router, and
# `_ASSIGN` is itself only ever consulted at a `_CMD_PREFIX`/`_WRAPPER_SKIP`
# anchor point, so an over-permissive match here can't smuggle anything past
# that separate, still-enforced check.
#
# A-REFUTE-3: the quote need not sit DIRECTLY after `NAME=`(`$`?) -- an
# earlier unquoted fragment of the SAME concatenated value (`FOO=bar"baz
# qux"`) can sit between them, exactly the shape `_ASSIGN_VALUE`'s own
# repeated `_ASSIGN_SEGMENT` now matches. The trailing `(?:[^\s'"]*)`
# mirrors that segment's unquoted alternative so the two stay in sync.
_ASSIGN_QUOTE_PREFIX_RE = re.compile(r"[A-Za-z_][A-Za-z0-9_]*=\$?(?:[^\s'\"]*)\Z")
_ASSIGN_QUOTE_LOOKBACK = 64  # assignment names are short; bounds the check to O(1)


def _is_assignment_value_quote(text, quote_idx):
    lookback = text[max(0, quote_idx - _ASSIGN_QUOTE_LOOKBACK) : quote_idx]
    return bool(_ASSIGN_QUOTE_PREFIX_RE.search(lookback))


def _single_quote_span_end(text, i, limit):
    """`text[i]` is `'`; return the index just past the matching closing
    quote, never past `limit` (the caller's own text length or bound --
    matches each call site's existing "unterminated" convention of
    falling back to `limit` itself).

    A-REFUTE-4 / B-F1: a `$'...'` ANSI-C-quoted string supports a
    backslash-escaped `\\'` for a literal apostrophe INSIDE the string --
    unlike a plain single-quoted string, which has NO escape mechanism at
    all (a bare `\\` inside `'...'` is a literal backslash character, not
    an escape, so real bash never lets one `'` close early there). Every
    call site used to locate a single quote's end with an escape-UNAWARE
    `text.find("'", ...)`, treating both shapes identically -- for
    `$'a\\'b c'`, that finds the ESCAPED quote (the one right after `a\\`)
    as the "close", leaving `b c'` to be read as a fresh, unterminated
    single-quoted span extending to `limit`. Everything genuinely
    executable after that point (a real `;`-separated command, say) gets
    masked away as if it were still inside a quote: a silent, total
    bypass for the rest of the text.

    Detected by checking whether the character immediately before this
    opening quote is `$` -- the only way a bare `'` is ever preceded by a
    literal `$` in bash is the `$'...'` ANSI-C-quoting form itself, so
    this is an exact test, not a heuristic. A plain single-quoted span
    (no preceding `$`) keeps the original escape-UNAWARE search: single
    quotes have no escape mechanism, so treating a `\\` as ordinary
    content there is correct, not a gap."""
    if i > 0 and text[i - 1] == "$":
        j = i + 1
        while j < limit:
            ch = text[j]
            if ch == "\\" and j + 1 < limit:
                j += 2
                continue
            if ch == "'":
                return j + 1
            j += 1
        return limit
    j = text.find("'", i + 1)
    return (j + 1) if (j != -1 and j < limit) else limit


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


def _heredoc_start_match(text, pos):
    """Wraps `_HEREDOC_START_RE.match` with the conservative UNPARSEABLE
    fallback (A-REFUTE-1): an UNQUOTED delimiter word immediately followed
    by a backslash is not fully resolvable by this scanner -- a
    `\\<newline>` right there is a real bash line-continuation that FOLDS
    into the delimiter word (verified against real bash: `cat <<E\\` +
    newline + `OF` / body / `EOF` terminates on `EOF`, not the truncated
    `E` this regex alone would capture), and any other backslash there
    extends the word by escaping the next character into it (the same
    class of word-extension R7 already had to account for in assignment
    values). Guessing at the truncated delimiter silently swallowed every
    real command up to EOF-of-text as fake heredoc body -- the safe
    direction here is to not recognize this `<<` as a heredoc opener at
    all, so the rest of the command stays visible to ordinary scanning
    instead of being hidden as inert body text."""
    m = _HEREDOC_START_RE.match(text, pos)
    if m is None:
        return None
    if m.group(4) is not None and m.end() < len(text) and text[m.end()] == "\\":
        return None
    return m


def _is_comment_start(text, i):
    """True when `text[i]` (a `#`) genuinely opens a shell comment: at the
    very start of `text`, or immediately after whitespace or a real
    separator character. Shared by `executable_mask` and
    `_find_matching_paren` so both scanners agree on what a comment is
    (F11, review round 9).

    One deliberate exception: a `#` immediately after `{` is NOT a comment
    start when that `{` is itself preceded by `$` -- that is bash's
    parameter-length expansion (`${#name}`, `${#@}`, ...), which reads as
    the length of the named variable/positional-parameter count, not a
    command-grouping brace followed by a comment. A bare `{` only reserves
    as the command-grouping keyword when followed by whitespace, so a `{`
    directly followed by `#` with nothing in between is never that either
    way; the `${#` case is the one that actually shows up in real shell,
    so it is the one carved out below. Before this exception, a length
    expansion inside a `cd $(...)` argument made the shared predicate blank
    the rest of the line -- including the substitution's own real closing
    paren and any real command after it (e.g. a later stash invocation) --
    as if it were commented out, both silently bypassing policy and (via
    `_find_matching_paren`'s forced
    scan-to-end-of-text when a substitution's local depth never returns to
    zero) making thousands of such invocations quadratic again."""
    if i == 0:
        return True
    prev = text[i - 1]
    if prev == "{" and i >= 2 and text[i - 2] == "$":
        return False
    return prev in " \t\n;&|(){}"


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
    this router (spec STOP condition already covers that boundary).

    Comment-aware (G8, review_b round 6): a `#` at a word boundary (the
    exact predicate `executable_mask` uses at top level) starts a real
    shell comment that runs to the next newline, INCLUDING inside a
    `$(...)` body — a real shell parses one there too. Without this, a
    stray `(`/`)`/quote character that only ever appears in a comment
    (e.g. `$(pwd # note: unbalanced ( here\n)`, entirely valid shell) was
    counted as real syntax: an unmatched `(` in the comment meant this
    scanner's local depth never returned to 0 within the current
    substitution, forcing it to keep scanning character-by-character all
    the way to the end of `text` looking for one more `)` -- and since
    `_read_token` (G7) calls this once per `cd $(...)` occurrence, that
    full-remaining-text scan repeated for every occurrence made thousands
    of them quadratic overall. An unmatched quote in a comment caused a
    correctness bug the same way: it was treated as opening a real quoted
    span, silently pairing with an unrelated quote character much later in
    the text and mis-locating the substitution's true close -- which threw
    off `_precompute_cd_reach_info`'s reach data for that `cd` and could
    turn a `cd` that closed inside its own subshell into one the router
    thought still reached a later `<stash>` invocation, producing an
    incorrect deny."""
    depth = 0
    n = len(text)
    i = open_idx
    while i < n:
        ch = text[i]
        if ch == "#" and _is_comment_start(text, i):
            j = text.find("\n", i)
            i = j if j != -1 else n
            continue
        if ch == "\\" and i + 1 < n:
            i += 2
            continue
        if ch == "'":
            # A-REFUTE-4/B-F1: escape-aware for a `$'...'` ANSI-C string
            # (see `_single_quote_span_end`) -- a plain single-quoted `)`
            # never matters here anyway (this function only tracks paren
            # depth), but an escaped-quote-blind search inside a `$'...'`
            # nested in a `$(...)` mis-located ITS end, throwing off the
            # depth count for everything after it.
            i = _single_quote_span_end(text, i, n)
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


def _mask_literal_span(text, start, end, result, quote_char, mask_delims=True):
    """Blank `text[start:end)` to spaces in `result`, but ONLY the quote
    delimiter itself (`quote_char`), any `_SEPARATOR_CHARS` character
    (G2, review_b round 1), and a backtick (B-R1, round 2 reopen review)
    — ordinary argument content stays visible. See the
    executable-region-scanner comment above for why this is safe.

    `mask_delims=False` (R4, round 3): leave the quote CHARACTER itself
    visible -- used only for a quote that is an assignment-prefix's OWN
    value (`_is_assignment_value_quote`), so `_ASSIGN`'s quoted-value
    alternative can still see and match the real delimiters in scan_text.
    Interior separator/backtick characters are still blanked either way.

    A REAL newline inside the span is a `_SEPARATOR_CHARS` member too and
    gets blanked like any other (review R4 regression from G2): leaving it
    visible put a fresh line-start inside quoted text, and `_CMD_PREFIX`'s
    `\\n\\s*` alternative then anchored the next line as if it were a brand
    new command — e.g. a multi-line commit message merely mentioning the
    stash rule's keyword denied, and a multi-line quoted `echo` argument
    mentioning a force-push flag asked. Length-preserving (a blanked
    newline is still one character), so all offsets stay identical; this
    function is never called on heredoc bodies (see
    `_consume_heredoc_body`), so they are unaffected.

    A backtick has no special meaning here at all: this function is only
    ever called on a SINGLE-quoted span, and single quotes suppress every
    kind of shell expansion including backtick command substitution — a
    backtick inside one is a plain literal character, never an opener or
    closer. Left visible, `_CMD_PREFIX`'s bare-backtick anchor (needed for
    a REAL substitution elsewhere) mistook it for one anyway, treating a
    single-quoted mention like `'... `<dangerous command>` ...'` as if a
    new command started right after the second backtick."""
    for k in range(start, end):
        ch = text[k]
        if ch == quote_char and not mask_delims:
            continue
        if ch == quote_char or ch in _SEPARATOR_CHARS or ch == "`":
            result[k] = " "


def _mask_span_preserving_substitutions(text, start, end, result, quote_spans=None):
    """Mask text[start:end) to spaces (newlines untouched), except `$(...)`
    and backtick spans, which stay visible because the shell still executes
    them there (inside double quotes or an unquoted heredoc body).

    R5 (round 3): a substitution's CONTENT used to be preserved wholesale
    -- visible, but never itself scanned -- so a quoted literal, comment,
    or quoted heredoc genuinely nested inside one (e.g. `$(printf '%s'
    'x; <cmd> stash')`, where the `;` is just part of a literal string
    argument to printf) stayed fully visible/executable and falsely
    tripped a command-position rule. `_mask_double_quoted` already
    recurses into a nested substitution's content via
    `_mask_quotes_recursive` (G1, review_b round 2) for the identical
    nesting shape inside double quotes; doing the same here makes this
    scanner agree with that one about what stays inert, while the
    substitution's own ordinary command text (not itself quoted/commented)
    stays visible exactly as before.

    `quote_spans` (A-REFUTE-2): forwarded to `_mask_quotes_recursive` so a
    quoted secret nested in a substitution HERE (an unquoted heredoc body
    is this function's only caller) gets its span recorded exactly like a
    top-level quote does -- before this, `evaluate()`'s
    `_extend_end_past_quote` had no span to extend into for a match
    ending mid-value inside one of these substitutions, and a secret
    fragment reached the persisted ledger in the clear."""
    i = start
    while i < end:
        ch = text[i]
        if ch == "$" and i + 1 < end and text[i + 1] == "(":
            raw_close, terminated = _find_matching_paren(text, i + 1)
            if terminated and raw_close <= end:
                close, body_end = raw_close, raw_close - 1
            else:
                close = body_end = min(raw_close, end)
            _mask_quotes_recursive(text, i + 2, body_end, result, quote_spans)
            i = close
            continue
        if ch == "`":
            j = text.find("`", i + 1)
            if j != -1 and j < end:
                close, body_end = j + 1, j
                # A-R1 pattern (see `_mask_double_quoted`): blank only the
                # CLOSING backtick -- the opener still anchors real
                # substitution text, the closer must not also anchor a
                # fresh command position for whatever ordinary body text
                # follows it in the same (unquoted heredoc) span.
                result[j] = " "
            else:
                close = body_end = end
            _mask_quotes_recursive(text, i + 1, body_end, result, quote_spans)
            i = close
            continue
        if ch != "\n":
            result[i] = " "
        i += 1


def _mask_quotes_recursive(text, start, end, result, quote_spans=None):
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
    fired a false deny.

    Comment- and heredoc-aware (G3, spec-level review, reopen generation
    2), matching what the top-level `executable_mask` loop already does,
    so the two scanners agree on what stays inert: before this fix, a `#`
    that opened a real shell comment inside a `$(...)` nested in double
    quotes left everything after it (a fake `; <stash>` mention, including
    the real `;` separator character) fully visible, tripping a false
    deny; a heredoc with a QUOTED delimiter (`<<'H'`) makes its whole body
    inert the same as any other single-quoted literal, but with no
    heredoc case here at all the body text stayed visible too.

    A-R5 (round 2 reopen review): the heredoc case genuinely QUEUES the
    opener (via `pending_heredoc`) and keeps scanning the rest of the
    opener line -- via the normal quote/comment/substitution branches
    below -- exactly like the top-level `executable_mask` loop's own
    `pending_heredocs` does, instead of jumping straight from the `<<
    DELIM` match to consuming the body at the next newline. The previous
    version's docstring claimed to match that top-level behavior but
    actually didn't: text between the delimiter and the opener line's own
    newline (e.g. a real, further quoted argument on the SAME line as
    `<<EOF`) was never scanned at all. Only a SINGLE pending heredoc is
    tracked (no queueing of several heredocs sharing one upcoming
    newline, unlike `executable_mask`'s list) -- multiple heredocs opened
    on one line INSIDE a substitution nested in double quotes is a
    combination no fixture exercises.

    `quote_spans` (A-REFUTE-2): when given a list, every single-/double-
    quoted span this function itself walks past is appended to it too --
    mirroring `executable_mask`'s own top-level bookkeeping -- and the
    same list is threaded into every recursive/heredoc call so a quote
    nested arbitrarily deep (a substitution inside a heredoc body inside
    a substitution, ...) still gets its span recorded. Before this, only
    TOP-LEVEL quotes were ever recorded, so `evaluate()`'s
    `_extend_end_past_quote` had nothing to extend a mid-value match into
    for a secret quoted anywhere in here, and it leaked into the ledger."""
    i = start
    pending_heredoc = None
    while i < end:
        ch = text[i]
        if ch == "\\" and i + 1 < end:
            # R1/R8 (round 3): blank both characters, mirroring the
            # top-level `executable_mask` loop's identical fix -- an
            # escaped real newline here must stop being a newline too, or
            # a line-continuation nested inside a substitution still
            # anchors a fresh command position past it.
            result[i] = " "
            result[i + 1] = " "
            i += 2
            continue
        if ch == "#" and _is_comment_start(text, i):
            j = text.find("\n", i)
            comment_end = j if (j != -1 and j < end) else end
            for k in range(i, comment_end):
                result[k] = " "
            i = comment_end
            continue
        if ch == "<" and text.startswith("<<", i) and not text.startswith("<<<", i):
            m = _heredoc_start_match(text, i)
            if m and m.end() <= end:
                strip_tabs = m.group(1) == "-"
                if m.group(2) is not None:
                    delim, quoted = m.group(2), True
                elif m.group(3) is not None:
                    delim, quoted = m.group(3), True
                else:
                    delim, quoted = m.group(4), False
                pending_heredoc = (delim, quoted, strip_tabs)
                i = m.end()
                continue
            i += 1
            continue
        if ch == "\n" and pending_heredoc is not None:
            delim, quoted, strip_tabs = pending_heredoc
            pending_heredoc = None
            close, _terminated = _consume_heredoc_body(
                text, i + 1, delim, quoted, strip_tabs, result, end, quote_spans
            )
            i = min(close, end)
            continue
        if ch == "'":
            start_q = i
            close = _single_quote_span_end(text, i, end)
            keep_delims = _is_assignment_value_quote(text, i)
            _mask_literal_span(text, i, close, result, "'", mask_delims=not keep_delims)
            if quote_spans is not None:
                quote_spans.append((start_q, close))
            i = close
            continue
        if ch == '"':
            start_q = i
            keep_delims = _is_assignment_value_quote(text, i)
            i = min(
                _mask_double_quoted(text, i, result, mask_delims=not keep_delims, quote_spans=quote_spans),
                end,
            )
            if quote_spans is not None:
                quote_spans.append((start_q, i))
            continue
        if ch == "$" and i + 1 < end and text[i + 1] == "(":
            raw_close, terminated = _find_matching_paren(text, i + 1)
            if terminated and raw_close <= end:
                close, body_end = raw_close, raw_close - 1
            else:
                close = body_end = min(raw_close, end)
            _mask_quotes_recursive(text, i + 2, body_end, result, quote_spans)
            i = close
            continue
        if ch == "`":
            j = text.find("`", i + 1)
            if j != -1 and j + 1 <= end:
                close, body_end = j + 1, j
                # A-R1 (round 2 reopen review): blank the CLOSING backtick
                # only -- see `_mask_double_quoted`'s identical branch for
                # why (a real substitution's own opener must stay visible,
                # but the character that CLOSES it must not double as a
                # fresh `_CMD_PREFIX` command-position anchor for whatever
                # ordinary text/arguments follow it in the same command).
                result[j] = " "
            else:
                close = body_end = min((j + 1) if j != -1 else len(text), end)
            _mask_quotes_recursive(text, i + 1, body_end, result, quote_spans)
            i = close
            continue
        i += 1


def _mask_double_quoted(text, start, result, mask_delims=True, quote_spans=None):
    """`text[start]` is the opening '"'; mask the double-quoted span,
    preserving `$(...)`/backtick substitutions' executable structure while
    recursively masking any quoted literal NESTED inside one of them (G1,
    review_b round 2 — see `_mask_quotes_recursive`). Returns the index
    just past the closing quote (or len(text) if unterminated).

    `mask_delims=False` (R4, round 3): leave the opening/closing '"'
    characters themselves visible -- see `_mask_literal_span`'s matching
    parameter for why (an assignment-prefix's own quoted value).

    A REAL newline inside the span is blanked too (G4, review_b round 2):
    `_mask_literal_span` (single quotes) already blanks it for the same
    reason the R4 fix documents there — leaving it visible put a fresh
    line-start inside quoted text, and `_CMD_PREFIX`'s `\\n\\s*` alternative
    anchored the next line as if it were a brand new command (e.g. a
    double-quoted, multi-line `cd /worktrees/x` mention got picked up by
    `_effective_checkout` as a REAL `cd`). This function had its own
    inline masking loop and was missed by that fix.

    A backslash-newline pair (`\\` immediately followed by a REAL newline)
    is a shell line continuation even inside double quotes — the shell
    deletes both characters and joins the two source lines into one
    logical line, so `"x\\` + newline + `cd /worktrees/x"` is exactly
    `"xcd /worktrees/x"`, a single quoted string with no executable `cd`
    at all. The backslash-escape branch below used to special-case this
    pair by leaving BOTH characters unmasked (reasoning it should not
    "double-blank" an escape it wasn't otherwise touching), which let the
    embedded real newline slip past the `_SEPARATOR_CHARS` branch further
    down (that branch never runs here because `continue` skips it) and
    fool `_CMD_PREFIX` the same way a bare embedded newline did before the
    first G4 fix (review_b round 2, re-opened). Blank both characters like
    any other escape pair — length-preserving, so offsets stay identical."""
    n = len(text)
    if mask_delims:
        result[start] = " "
    i = start + 1
    while i < n:
        ch = text[i]
        if ch == "\\" and i + 1 < n:
            result[i] = " "
            result[i + 1] = " "
            i += 2
            continue
        if ch == '"':
            if mask_delims:
                result[i] = " "
            return i + 1
        if ch == "$" and i + 1 < n and text[i + 1] == "(":
            raw_close, terminated = _find_matching_paren(text, i + 1)
            if terminated:
                close, body_end = raw_close, raw_close - 1
            else:
                close = body_end = raw_close
            _mask_quotes_recursive(text, i + 2, body_end, result, quote_spans)
            i = close
            continue
        if ch == "`":
            j = text.find("`", i + 1)
            if j != -1:
                close, body_end = j + 1, j
                # A-R1 (round 2 reopen review): blank the CLOSING backtick
                # only. The OPENER genuinely starts real substitution
                # script text -- `_CMD_PREFIX`'s bare-backtick alternative
                # must still anchor there. The character that CLOSES the
                # substitution is a different story: a backtick both opens
                # AND closes (unlike `$(`/`)`), so a bare `` `date` git
                # stash `` (or the same thing wrapped in double quotes)
                # left the CLOSING backtick just as visible/anchor-able as
                # the opener, and `_CMD_PREFIX` treated ordinary trailing
                # text/arguments right after it as a brand new command --
                # despite that -- the shell just keeps reading the SAME
                # command/string past a substitution's output.
                result[j] = " "
            else:
                close = body_end = n
            _mask_quotes_recursive(text, i + 1, body_end, result, quote_spans)
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


def _consume_heredoc_body(text, start, delim, quoted, strip_tabs, result, end=None, quote_spans=None):
    """Mask the heredoc body starting at `start` (just after the opener's
    newline) up to and including the line that is exactly `delim` (F13: the
    ACTUAL delimiter bounds the body, never `[\\s\\S]*` to end-of-string).
    A quoted delimiter (`<<'EOF'`/`<<"EOF"`) makes the whole body inert; an
    unquoted one still allows `$(...)`/backtick substitution in the body.
    Returns `(index, terminated)`: index is just past the terminator line
    (or len(text) if the heredoc is never terminated, matching real shell
    behavior of consuming to EOF); `terminated` is False only in that
    never-closed case (F8: callers use it to represent the EOF-close in
    scan_text without ever scanning past a REAL terminator).

    `end` (B-R2, round 2 reopen review) optionally caps how far the
    MASKING itself may write into `result` -- the terminator search below
    always runs unbounded (it must, to find where the body genuinely
    ends), but `_mask_quotes_recursive` calls this from inside a bounded
    `$(...)`/backtick substitution whose own `end` was found by a scanner
    with no heredoc awareness at all (`_find_matching_paren`), which can
    locate that substitution's "close" earlier than a heredoc-aware
    parser would. Masking the full (possibly past-`end`) body in that
    case blanked real, genuinely-executable text sitting just past the
    substitution's assumed close -- the caller already clamps its own `i`
    to `end`, but the actual `result` mutation was unclamped, silently
    erasing a real trailing command. The top-level `executable_mask`
    caller has no such bound (`end=None`, its default) and is unaffected.

    The terminator search walks line-by-line (delimiter comparison is
    inherently per-line), but the actual masking of an UNQUOTED body is
    done in ONE pass over the whole body span (G3, spec-level review,
    reopen generation 2), not per physical line: a real shell command
    substitution genuinely spans multiple lines (an embedded real newline
    inside `` `...` ``/`$(...)` is just whitespace to the substitution),
    but masking line-by-line reset `_mask_span_preserving_substitutions`'s
    open-span tracking at every newline -- an unclosed backtick/`$(` at
    the end of one line was treated as closed by the time the NEXT line
    started, so that next line's own content (including a real command
    the still-open substitution was genuinely about to execute) was
    masked away as ordinary inert body text instead of staying visible.

    `quote_spans` (A-REFUTE-2): forwarded to
    `_mask_span_preserving_substitutions` for an UNQUOTED body, so a
    quoted secret inside a substitution embedded in the body gets its
    span recorded the same way a top-level quote does."""
    n = len(text)
    i = start
    while True:
        nl = text.find("\n", i)
        line_end = nl if nl != -1 else n
        line = text[i:line_end]
        check_line = line.lstrip("\t") if strip_tabs else line
        if check_line == delim:
            body_end = i
            end_index, terminated = (n if nl == -1 else nl + 1), True
            break
        if nl == -1:
            body_end = n
            end_index, terminated = n, False
            break
        i = nl + 1
    mask_limit = body_end if end is None else min(body_end, end)
    if quoted:
        for k in range(start, mask_limit):
            if text[k] != "\n":
                result[k] = " "
    else:
        _mask_span_preserving_substitutions(text, start, mask_limit, result, quote_spans)
    return end_index, terminated


def executable_mask(text, quote_spans=None):
    """`quote_spans` (R6, round 3): when given a list, every TOP-LEVEL
    single-/double-quoted span this scanner walks past is appended to it
    as `(start, end)` (end just past the closing delimiter, or len(text)
    if unterminated) -- `evaluate()` uses this to extend a rule's match
    span past a quoted secret value it ended in the middle of, before
    redact() ever sees the slice (see `_extend_end_past_quote`)."""
    n = len(text)
    result = list(text)
    i = 0
    pending_heredocs = []
    in_backtick = False
    while i < n:
        ch = text[i]
        # G3 (spec-level review, reopen generation 2): a backslash outside
        # any quoting is bash for a literal next character, not a real
        # quote/comment opener -- `_mask_quotes_recursive` (the scanner
        # used for text inside a `$(...)`/backtick nested in double
        # quotes) already skips an escaped character pair like this; this
        # top-level loop had no such case, so `echo \'; <cmd> stash` (a
        # literal escaped apostrophe, not a real single-quote span) had
        # its bare `'` mistaken for a genuine (unterminated) quote opener,
        # which masked the real `;` separator right after it to a space
        # and hid the anchored stash invocation that follows.
        #
        # R1/R8 (round 3, ledger arrra-hex-pr-5-wf-open-ledger.md): the
        # pair used to be left FULLY VISIBLE (only `i` advanced past it).
        # That's right for an ordinary escaped character (a literal `\;`
        # must not fake a real separator -- R8), but wrong for the one
        # shape where the escaped character IS a real newline: bash
        # LINE-CONTINUATION deletes a `\<newline>` pair outright, folding
        # the two source lines into one logical line (verified: `cd /tmp
        # && cd \<nl>/usr && pwd` prints /usr). A rule's own `[^\n;&|]*`
        # character class explicitly excludes a real newline, so leaving
        # one visible here broke every Bash rule whose target sat on the
        # continued line (R1). Blanking BOTH characters to spaces --
        # mirroring `_mask_double_quoted`'s identical escape branch --
        # fixes both at once: an escaped separator no longer fakes a
        # boundary (R8), and an escaped real newline is no longer a
        # newline at all, so `[^\n;&|]*` keeps matching straight through
        # it exactly as if the line had never been split.
        if ch == "\\" and i + 1 < n:
            result[i] = " "
            result[i + 1] = " "
            i += 2
            continue
        if ch == "`":
            # A-R1 (round 2 reopen review): a backtick both opens AND
            # closes a real substitution (unlike `$(`/`)`, which use
            # different characters) -- `in_backtick` toggles across
            # exactly the OPENER/CLOSER pair so only the CLOSING one gets
            # blanked. The opener stays visible (a genuine substitution
            # anchors `_CMD_PREFIX` right after it, e.g. a `backticks-in-
            # unquoted-heredoc`-style invocation), but the closer must not
            # ALSO anchor a fresh command position for whatever ordinary
            # text/arguments the shell keeps reading after the
            # substitution's output in the SAME command. Everything
            # between the two backticks is still handled by this same
            # per-character loop (quotes inside a substitution's body are
            # masked exactly like top-level text), so toggling a flag
            # here -- rather than jumping straight to the close, the way
            # the analogous branches elsewhere in this scanner do -- is
            # the only way to blank just the closer without skipping that
            # interior scanning.
            if in_backtick:
                result[i] = " "
            in_backtick = not in_backtick
            i += 1
            continue
        if ch == "#" and _is_comment_start(text, i):
            j = text.find("\n", i)
            end = j if j != -1 else n
            # A-R2 (round 2 reopen review, generation 3): a real shell
            # bounds a comment INSIDE a backtick substitution by the
            # substitution's own closing backtick, not by the next real
            # newline -- confirmed: `x=`echo hi # c`; echo RAN` prints
            # `RAN x=hi` (`echo RAN` genuinely runs; unlike `$(...)`, the
            # backtick lexer stops the comment the instant it reaches the
            # matching closer). Without this bound, a `#` on the SAME
            # physical line as the closing backtick swallowed that closer
            # -- and everything genuinely executable after it -- as if it
            # were still inert comment text. When a real newline occurs
            # first (the comment's own line ends before the substitution
            # closes), the ordinary end-of-line bound still applies.
            if in_backtick:
                bt = text.find("`", i)
                if bt != -1 and bt < end:
                    end = bt
            for k in range(i, end):
                result[k] = " "
            i = end
            continue
        if ch == "'":
            end = _single_quote_span_end(text, i, n)
            keep_delims = _is_assignment_value_quote(text, i)
            _mask_literal_span(text, i, end, result, "'", mask_delims=not keep_delims)
            if quote_spans is not None:
                quote_spans.append((i, end))
            i = end
            continue
        if ch == '"':
            start = i
            keep_delims = _is_assignment_value_quote(text, i)
            i = _mask_double_quoted(text, i, result, mask_delims=not keep_delims, quote_spans=quote_spans)
            if quote_spans is not None:
                quote_spans.append((start, i))
            continue
        if ch == "<" and text.startswith("<<", i) and not text.startswith("<<<", i):
            m = _heredoc_start_match(text, i)
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
                i, terminated = _consume_heredoc_body(
                    text, i, delim, quoted, strip_tabs, result, quote_spans=quote_spans
                )
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
# R3 (round 3): a bare separator-anchor missed a `cd`/`pushd`/`popd` right
# after a reserved word (`if`/`then`/`do`/...) or a `command`/`builtin`
# wrapper -- none of those are `_SEPARATOR_CHARS`, so the effective-checkout
# tracker never saw them at all and fell back to the (possibly exempt)
# payload cwd. Shares `_RESERVED_LEADIN`/`_WRAPPER_SKIP` with `_CMD_PREFIX`
# so the two anchors agree on what counts as command position; `pushd`
# changes the effective directory exactly like `cd` (handled identically
# below), `popd` pops a directory STACK this router doesn't track, so its
# target is always treated as uncertain (see `_precompute_cd_reach_info`).
_CD_LOCATE_RE = re.compile(
    r"(?:^|[;&|(){}\n]\s*|" + _RESERVED_LEADIN + r")"
    + _WRAPPER_SKIP
    + r"(cd|pushd|popd)\b"
)


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
    path.

    R1 (round 3): the leading-whitespace skip also steps over a
    `\\<newline>` line-continuation pair -- bash deletes it outright, so
    `cd \\` + newline + `/usr` genuinely targets `/usr`, not a literal
    backslash character (the token-start skip previously stopped at the
    very first non-space/tab character, landing ON the backslash and
    reading it as the whole token)."""
    n = len(text)
    while pos < n:
        if text[pos] in " \t":
            pos += 1
            continue
        if text[pos] == "\\" and pos + 1 < n and text[pos + 1] == "\n":
            pos += 2
            continue
        break
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
        if text[pos] == "$" and pos + 1 < n and text[pos + 1] == "(":
            # G7 (review_b round 5): treat `$(...)` as one atomic unit via
            # the quote-aware `_find_matching_paren` instead of stopping at
            # the first unescaped `)`, which used to land INSIDE the
            # substitution (one char short of its real close) and threw off
            # every caller relying on `end_pos` as a genuine token boundary
            # (see docstring above).
            pos, _terminated = _find_matching_paren(text, pos + 1)
            continue
        if text[pos] == "`":
            close = text.find("`", pos + 1)
            pos = (close + 1) if close != -1 else n
            continue
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


def _skip_balanced_group(scan_text, open_idx):
    """`scan_text[open_idx]` is `(` or `{`; return the index just past its
    matching closer. `scan_text` has already had quoted/commented/heredoc
    text masked to spaces (real `(`/`)`/`{`/`}` characters only remain
    where they are genuine shell syntax), so plain depth counting on the
    SAME bracket character as `open_idx` suffices -- an unrelated bracket
    type nested inside (e.g. a `(...)` subshell inside a `{...}` group)
    never affects this count, since a well-formed script always closes it
    before the enclosing group's own closer. Falls back to the end of
    `scan_text` when the group never closes (EOF), matching this file's
    other unterminated/EOF conventions (`_find_matching_paren`,
    `_consume_heredoc_body`)."""
    open_ch = scan_text[open_idx]
    close_ch = ")" if open_ch == "(" else "}"
    depth = 0
    n = len(scan_text)
    i = open_idx
    while i < n:
        ch = scan_text[i]
        if ch == open_ch:
            depth += 1
        elif ch == close_ch:
            depth -= 1
            if depth == 0:
                return i + 1
        i += 1
    return n


def _or_operand_end(scan_text, start):
    """Find where a `||` operand genuinely ends, starting at `start` (just
    past the `||` and its leading spaces, per `_OR_GUARD_RE`).

    new-defects R1 (round 3): the previous implementation just took the
    first `_SEPARATOR_CHARS` position at/after `start` -- but a lone pipe
    (`|`) or `|&` binds TIGHTER than `||` and stays part of the SAME
    operand (bash: `A || B | C` parses as `A || (B | C)`, confirmed: `cd
    /nonexist || true | echo RAN` prints RAN only when the `cd` failed --
    the pipeline is the whole fallback), so treating a bare `|`/`|&` as a
    terminator let a command AFTER it wrongly escape the guard and inherit
    the failed `cd`'s target. Only a REAL list separator -- `;`, `&`
    (whether alone or as the first half of `&&`), a real newline, or a
    bracket/group opener (handled by the caller's own
    `_skip_balanced_group` extension) -- ends the operand. `||` itself
    (two consecutive `|` characters) also ends it: a chained `A || B ||
    C` only guards `B` with the first `||`.

    (A backslash-newline continuation is already invisible by the time
    scan_text exists -- see `executable_mask`'s escape-pair blanking --
    so any REAL `\\n` reaching this scan genuinely is one.)"""
    n = len(scan_text)
    i = start
    while i < n:
        ch = scan_text[i]
        if ch in ";&\n(){}":
            return i
        if ch == "|":
            if i + 1 < n and scan_text[i + 1] == "|":
                return i
            # A lone pipe or `|&` -- part of the SAME pipeline, not a `||`
            # terminator. Skip past it (1 char for `|`, 2 for `|&`) and
            # keep scanning for the operand's real end.
            i += 2 if (i + 1 < n and scan_text[i + 1] == "&") else 1
            continue
        i += 1
    return n


def _next_lower_paren_depth(paren_depths):
    """`next_lower[j]` = the smallest `k > j` with `paren_depths[k] <
    paren_depths[j]`, or `None` if no such `k` exists ("next smaller
    element", computed once per `evaluate()` call in a single reverse pass
    with a monotonic stack -- O(n) total, each index pushed/popped at most
    once). Lets `_precompute_cd_reach_info` (F14 redo 2) answer "where does
    the enclosing depth at this `cd`'s token first drop below its own
    depth" in O(1) instead of a per-`cd` forward walk to the end of the
    text (see that function's docstring for why the walk was quadratic)."""
    n = len(paren_depths)
    next_lower = [None] * n
    stack = []
    for j in range(n - 1, -1, -1):
        d = paren_depths[j]
        while stack and paren_depths[stack[-1]] >= d:
            stack.pop()
        next_lower[j] = stack[-1] if stack else None
        stack.append(j)
    return next_lower


def _base_cwd_before(position, starts, infos, payload_cwd):
    """The effective directory just before `position`, found by walking
    backward over `cd` reach data (as built by
    `_precompute_cd_reach_info`) for the nearest one that still reaches
    `position` -- not confined to an OR-guard's own right-hand operand
    (`guard_end`, A-R4), not already closed by its enclosing subshell
    (`break_pos`, G1), and not itself confined to an EARLIER cd's
    OR-guard operand (`ceiling`, R2/new-defects R2, round 3 -- see
    `_precompute_cd_reach_info`). No reaching `cd` at all falls back to
    `payload_cwd`.

    Shared by `_effective_checkout` (an invocation's own base directory)
    and `_precompute_cd_reach_info` itself (A-R2, round 2 reopen review):
    a RELATIVE `cd` target must resolve against whatever directory the
    CLOSEST EARLIER `cd` already reached, never unconditionally against
    the raw payload cwd -- `cd /shared/checkout && cd . && <stash>`
    genuinely stays inside /shared/checkout (the second `cd`'s `.`
    resolves against the shell's real current directory after the first
    `cd` ran), but resolving every `cd` independently against the hook's
    own /worktrees/ payload cwd instead joined the relative target right
    back onto the exempt path."""
    idx = _bisect_left(starts, position) - 1
    while idx >= 0:
        resolved, guard_end, break_pos, ceiling = infos[idx]
        if (
            (guard_end is None or position >= guard_end)
            and (break_pos is None or break_pos > position)
            and (ceiling is None or position < ceiling)
        ):
            return resolved
        idx -= 1
    return payload_cwd


def _precompute_cd_reach_info(text, scan_text, paren_depths, payload_cwd, sep_positions):
    """F14 (review round redo, then redo 2): `_effective_checkout` used to
    re-run `_CD_LOCATE_RE.finditer(scan_text[:match_start])` (a full rescan
    of everything before the candidate) AND re-slice/re-`min()`
    `paren_depths[token_end:target_pos+1]` (the old `_cd_reaches` helper) on
    EVERY candidate occurrence of a rule with `unless_cwd` -- both
    proportional to `match_start`/`target_pos`, so a leading `cd` followed by
    thousands of exempt invocations was quadratic overall (same shape
    `_window_bounds` fixed for separator lookups). Each `cd`'s own reach
    data -- its RESOLVED target (A-R2: against whatever `cd` already
    reached ITS OWN position, via `_base_cwd_before`, never unconditionally
    payload_cwd), the extent of its own OR-guard's right-hand operand if
    any (`guard_end`; A-R4, replacing a permanent `guarded` boolean -- see
    that function's docstring), and the first position (`break_pos`) where
    its enclosing paren depth drops below the depth at the `cd`'s own
    token start (G1, review_b round 1: `(cd /worktrees/x); <stash>` must
    not inherit the subshell-local `cd`, because the `)` closes it first)
    -- depends only on the `cd` itself (and `cd`s already processed before
    it in this same left-to-right pass), never on the later candidate
    being checked. Computed ONCE per `cd` here (called once per
    `evaluate()` call, like `sep_positions`/`paren_depths`) instead of once
    per (`cd`, candidate) pair.

    The first fix (19821fc) still found `break_pos` with a forward walk
    from `token_end` to the end of `paren_depths` for EVERY `cd` -- fine for
    one `cd` followed by many candidates, but quadratic again for thousands
    of `cd`s themselves (at depth 0 with no real parens, the walk never
    terminates early). `_next_lower_paren_depth` turns that per-`cd` walk
    into an O(1) lookup: when the `cd`'s own token doesn't change the paren
    depth (the common case -- `cd` and its argument contain no unmasked
    parens, so `paren_depths[token_end] == enclosing`), `break_pos` is
    exactly `next_lower[token_end]`. Only in the rare case the token itself
    changed the depth does this fall back to the linear walk.

    `_effective_checkout` then binary-searches this list (via
    `_base_cwd_before`) for an O(1) reach check per candidate. Returns
    `(starts, infos)` -- `starts` (the sorted `cd` positions, for
    `_bisect_left`) kept separate from `infos` so callers never rebuild a
    per-candidate list just to search it."""
    starts = []
    infos = []
    operand_windows = []  # [(operand_start, operand_end), ...] -- R2/round 3
    next_lower = None
    for cd_match in _CD_LOCATE_RE.finditer(scan_text):
        keyword = cd_match.group(1)
        token_start = cd_match.end()
        value, token_end = _read_token(text, token_start)
        if keyword == "popd":
            # R3 (round 3): `popd` pops a directory STACK this router
            # doesn't track -- its real target is genuinely unknown, so
            # treat it as uncertain (never exempt) rather than resolving
            # whatever stray token happens to follow it as a path.
            value = None
        base_for_this_cd = _base_cwd_before(cd_match.start(), starts, infos, payload_cwd)
        resolved = _resolve_against_cwd(value, base_for_this_cd) if value is not None else None
        # R2 (round 3): a `cd` that is ITSELF the right-hand operand of an
        # earlier `cd`'s `||` (e.g. the second `cd` in `cd A || cd B;
        # <stash>`) only ever runs when that earlier `cd` FAILED -- it
        # must not be usable as a base for anything past the earlier
        # `cd`'s own `guard_end` (where its whole OR-compound ends),
        # because when the earlier `cd` SUCCEEDS (the common case) ITS
        # target governs there instead. `ceiling` records the nearest
        # enclosing operand's end; `_base_cwd_before` skips this entry
        # for any position at or past it and keeps walking backward --
        # which naturally finds the enclosing `cd` next, exactly the
        # fix new-defects R2 asked for as the symmetric case. Checked
        # against the KEYWORD's own start (group 1), not the overall
        # match start: `_CD_LOCATE_RE`'s separator alternative can anchor
        # on the SECOND `|` of a `||` (a lone `|` is itself a separator
        # char), which sits one character before an operand window that
        # starts right after the whole `||` -- the keyword itself is
        # still genuinely inside the operand either way.
        ceiling = None
        keyword_start = cd_match.start(1)
        for op_start, op_end in operand_windows:
            if op_start <= keyword_start < op_end and (ceiling is None or op_end < ceiling):
                ceiling = op_end
        # A-R4 (round 2 reopen review): `cd X || <fallback>` only skips the
        # `cd`'s effect for `<fallback>` itself -- the OR's own right-hand
        # operand -- never for anything after it. Once a `;`/newline/`&`/
        # etc ends that operand, a `cd` that SUCCEEDED (the common case)
        # governs every later command exactly as an unguarded `cd` would;
        # a permanent `guarded` flag (the previous design) wrongly
        # discarded the `cd` for candidates far past its own OR-compound
        # too. `guard_end` is the position where the operand ends, or
        # `None` when there is no `||` at all; a candidate at or past
        # `guard_end` is never blocked by this guard (see
        # `_base_cwd_before`).
        guard_end = None
        or_match = _OR_GUARD_RE.match(scan_text, token_end)
        if or_match:
            # new-defects R1 (round 3): `_or_operand_end` -- not a bare
            # "first separator" lookup -- so a lone pipe/`|&` (which binds
            # tighter than `||` and stays part of the SAME operand) never
            # ends it early (see that function's docstring).
            guard_end = _or_operand_end(scan_text, or_match.end())
            # A-R1/B-R1 (round 2 reopen review, generation 3): `{`/`(` are
            # themselves `_SEPARATOR_CHARS` members, so when the fallback
            # operand OPENS with a brace-group or subshell (`cd X || {
            # ...; }` / `cd X || (...)`), the lookup above lands `guard_end`
            # on that OPENING bracket -- the group's own body then sits at
            # or past `guard_end` and is wrongly treated as already outside
            # the guard, inheriting a `cd` that just failed to reach it. A
            # real shell only runs the group when the `cd` failed, i.e. in
            # whatever directory was in effect BEFORE it. Extend `guard_end`
            # past the bracket's own matching closer so the entire operand
            # -- not just its first character -- stays excluded from this
            # `cd`'s reach.
            if guard_end < len(scan_text) and scan_text[guard_end] in "({":
                guard_end = _skip_balanced_group(scan_text, guard_end)
            operand_windows.append((or_match.end(), guard_end))
        enclosing = paren_depths[token_start]
        break_pos = None
        if paren_depths[token_end] == enclosing:
            if next_lower is None:
                next_lower = _next_lower_paren_depth(paren_depths)
            break_pos = next_lower[token_end]
        else:
            for j in range(token_end, len(paren_depths)):
                if paren_depths[j] < enclosing:
                    break_pos = j
                    break
        starts.append(cd_match.start())
        infos.append((resolved, guard_end, break_pos, ceiling))
    return starts, infos


def _effective_checkout(text, scan_text, match_start, match_end, payload_cwd, cd_reach_starts, cd_reach_infos):
    invocation = scan_text[match_start:match_end]
    if _GIT_DIR_LOCATE_RE.search(invocation):
        return None
    # G2 (spec-level review, reopen generation 2): find whatever `cd`
    # already reached this invocation FIRST, before looking at `-C` --
    # `base_cwd` is the invocation's own effective working directory
    # absent any `-C` override.
    base_cwd = _base_cwd_before(match_start, cd_reach_starts, cd_reach_infos, payload_cwd)
    # A-R3/B-R3 (round 2 reopen review): git(1) folds MULTIPLE `-C`
    # options left-to-right -- each subsequent non-absolute `-C <path>`
    # resolves relative to the PRECEDING `-C <path>`, never straight
    # against the shell's cwd. Reading only the LAST occurrence
    # (`c_locates[-1]`) silently dropped an earlier absolute `-C`
    # entirely, so `-C /shared/checkout -C .` resolved the relative `.`
    # against `base_cwd` (the payload/cd cwd) instead of
    # `/shared/checkout`, the real preceding `-C`.
    for c_locate in _DASH_C_LOCATE_RE.finditer(invocation):
        value, _ = _read_token(text, match_start + c_locate.end())
        if value is None:
            return None
        if value.startswith("/"):
            base_cwd = value
        elif base_cwd is not None:
            base_cwd = _resolve_against_cwd(value, base_cwd)
        else:
            # A relative `-C` with no known base to resolve against
            # (the running base itself is unresolvable, e.g. a `cd $VAR`
            # earlier) -- stays uncertain, keep protection rather than
            # silently falling back to the payload cwd.
            return None
    return base_cwd


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
#
# G4 (spec-level review, reopen generation 2): a bare `\b(?:do|done)\b`
# counts EVERY occurrence of those letters, including a QUOTED argument
# (e.g. `echo 'done'`) -- masking only blanks a single-quoted span's own
# quote delimiters (G2, review_b round 1: ordinary argument content stays
# visible so other rules can still see it), so a quoted "done" reads
# exactly like the bare reserved word once the quotes are gone. Only an
# UNQUOTED `do`/`done` in actual command position -- the nearest
# non-whitespace character before it is a real separator, or it is the
# very first thing in the text -- is ever a genuine loop keyword; real
# bash never recognizes either as a reserved word anywhere else (an
# argument, mid-word, right after another word with no separator).
_LOOP_TOKEN_RE = re.compile(r"\b(?:do|done)\b")


def _is_command_position(text, idx):
    """True when `text[idx]` sits where a new command/reserved word may
    legitimately start: the very beginning of `text`, or the nearest
    non-whitespace character before it is a real separator
    (`_SEPARATOR_CHARS`) -- never an ordinary word/argument character."""
    j = idx - 1
    while j >= 0 and text[j] in " \t":
        j -= 1
    return j < 0 or text[j] in _SEPARATOR_CHARS


def _polling_loop_extent(matched_text):
    """Classify `matched_text`'s `do`/`done` nesting relative to its own
    span. Returns "closed" (a genuine, fully-bounded loop -- possibly
    containing fully-nested loops of its own), "unclosed" (every token
    consumed but depth is still positive -- a nested loop's `done` closed
    before the true outer terminator, which lies further out in scan_text
    than this candidate reached; the caller should extend and re-check),
    or "crossed" (depth returns to 0 somewhere in the MIDDLE of the span --
    an earlier, unrelated loop already closed; the caller must reject this
    span outright, never extend it). A quoted or otherwise not-command-
    position `do`/`done` (G4) is never counted as a token at all -- it
    never opens or closes anything, real or fake."""
    tokens = [
        tok
        for tok in _LOOP_TOKEN_RE.finditer(matched_text)
        if _is_command_position(matched_text, tok.start())
    ]
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
    if tool_name == "NotebookEdit":
        # F21: NotebookEdit's actual schema is notebook_path + new_source,
        # not file_path/new_string/content/edits -- the generic TEXT_TOOLS
        # branch below never read either, so a real NotebookEdit payload
        # canonicalized to an empty string and no rule could ever inspect
        # it. Canonical first line is the path (so a \A-anchored path rule
        # still works the same way it does for Edit/Write), then the
        # source, mirroring the generic branch's file_path+body shape.
        parts = []
        if "notebook_path" in tool_input:
            parts.append(str(tool_input.get("notebook_path", "")))
        if "new_source" in tool_input:
            parts.append(str(tool_input.get("new_source", "")))
        return "\n".join(parts)
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


def _extend_end_past_quote(quote_spans, end):
    """R6 (round 3): if `end` falls STRICTLY inside one of the top-level
    quoted spans `executable_mask` collected, return that span's own end
    instead -- so a rule's match sliced mid-quote still reaches the
    value's real closing delimiter before redact() ever sees it. Returns
    `end` unchanged when it doesn't land inside any span (including
    exactly AT a span's boundary, which is already a complete slice)."""
    for q_start, q_end in quote_spans:
        if q_start < end < q_end:
            return q_end
    return end


def ledger_path():
    ledger_dir = os.environ.get("HEX_LEDGER_DIR") or os.path.join(
        os.path.expanduser("~"), ".hex", "ledger"
    )
    _ensure_private_dir(ledger_dir)  # F7: 0700 regardless of umask
    return os.path.join(ledger_dir, LEDGER_FILENAME)


def evaluate(payload):
    tool_name = payload.get("tool_name", "")
    tool_input = payload.get("tool_input") or {}
    cwd = payload.get("cwd", "")
    session_id = payload.get("session_id", "")

    text = canonical_text(tool_name, tool_input)
    # F2/F13: Bash command rules only ever see EXECUTABLE text — a real
    # command-position anchor (a separator, or the opener of a live
    # `$(...)`/backtick substitution) is masked to a space wherever it
    # sits inside quoted text first, so it can never anchor a rule from
    # there. ORDINARY argument content (plain letters/digits) inside a
    # quote is left visible in scan_text by design (see
    # `_mask_literal_span`'s doc comment) — only `_RESERVED_LEADIN`'s
    # bare-word alternatives (if/then/elif/else/while/until/do) can ever
    # anchor on that visible-but-quoted text, since no OTHER `_CMD_PREFIX`
    # alternative survives masking inside a quote. `quote_spans` (below)
    # exists to filter exactly that case out. Other tools' canonical text
    # (file paths/content) isn't Bash syntax, so it is used as-is.
    quote_spans = []
    scan_text = executable_mask(text, quote_spans) if tool_name == "Bash" else text
    if tool_name == "Bash":
        scan_text = _widen_quoted_global_opt_args(text, scan_text, quote_spans)
    sep_positions = [i for i, ch in enumerate(scan_text) if ch in _SEPARATOR_CHARS]
    paren_depths = _paren_depths(scan_text)
    cd_reach_starts, cd_reach_infos = _precompute_cd_reach_info(
        text, scan_text, paren_depths, cwd, sep_positions
    )
    rules = load_rules()

    def _match_starts_inside_a_single_quote(m):
        # F2 (round 2 review, major): `_RESERVED_LEADIN` recognizes a
        # reserved word anywhere in scan_text, with no lookbehind
        # available (Rust `regex` port target) to check whether it sits
        # in REAL command position. `printf '%s\n' 'then stash-it'`
        # never runs a real stash invocation at all -- the word `then`
        # the rule anchored on is a QUOTED MENTION, visible in scan_text
        # only because ordinary quoted argument content is never blanked
        # (see the comment on `quote_spans` above).
        #
        # Restricted to SINGLE-quoted spans only (`text[q_start] == "'"`,
        # recovered from the ORIGINAL text rather than threading a new
        # discriminator into every quote_spans.append() call site): a
        # first regression pass filtered ANY quote_spans containment and
        # broke `echo "$(real stash invocation)"` and its siblings -- a
        # `$(...)`/
        # backtick substitution genuinely STAYS LIVE inside DOUBLE quotes
        # (unlike single quotes, which suppress every kind of expansion),
        # so a substitution-anchored match legitimately starting inside a
        # double-quoted span must never be discarded. `_RESERVED_LEADIN`'s
        # bare-word alternatives are the only ones a single-quoted span
        # can ever spuriously anchor (nothing else survives masking
        # there), so restricting the filter to single quotes closes F2
        # without reopening G1/round-2's substitution-in-double-quotes
        # coverage.
        return any(
            q_start < len(text) and text[q_start] == "'" and q_start <= m.start() < q_end
            for q_start, q_end in quote_spans
        )

    fires = []
    for rule in rules:
        if not rule["tool_re"].search(tool_name):
            continue
        all_matches = [
            m
            for m in rule["match_re"].finditer(scan_text)
            if not _match_starts_inside_a_single_quote(m)
        ]
        if not all_matches:
            continue

        unless_cwd_re = rule["unless_cwd_re"]

        def _cwd_exempts(candidate, _unless_cwd_re=unless_cwd_re):
            # F4: judged per-invocation from the EFFECTIVE checkout (see
            # `_effective_checkout`), never the blanket hook payload cwd --
            # an unresolved target never exempts (keeps protection).
            if _unless_cwd_re is None:
                return False
            eff_cwd = _effective_checkout(
                text, scan_text, candidate.start(), candidate.end(), cwd, cd_reach_starts, cd_reach_infos
            )
            if eff_cwd is None:
                return False
            return bool(_unless_cwd_re.search(eff_cwd))

        unless_re = rule["unless_match_re"]
        scope = rule["unless_scope"]
        # G1 (spec-level review, reopen generation 2): this carries the
        # (start, end) span of the winning occurrence IN `scan_text`
        # coordinates, never the already-masked text itself -- masking is
        # length-preserving (see the executable-region-scanner comment
        # above `_mask_literal_span`), so the same offsets index the
        # ORIGINAL, unmasked `text` too. `raw_matched` below is always
        # sliced from `text`, not `scan_text`: `_mask_literal_span`/
        # `_mask_double_quoted` blank a quoted value's surrounding quote
        # CHARACTERS (by design, so ordinary quoted argument content stays
        # visible to command rules -- see G2, review_b round 1), so by the
        # time `scan_text` exists, `password="alpha bravo charlie"` has
        # already lost its quotes. `redact()`'s quoted-value alternative
        # needs to see an actual quote character to take the "can contain
        # spaces" branch; without it, it dropped to the bare `\S+`
        # fallback and only the first word of a multi-word secret got
        # redacted -- the rest persisted in the ledger's `match` field in
        # the clear.
        match_override_span = None
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
                        # G4: a quoted/mid-word occurrence is never a real
                        # token (see `_is_command_position`) -- keep
                        # searching past it instead of extending to it.
                        while next_done is not None and not _is_command_position(
                            scan_text, next_done.start()
                        ):
                            next_done = _LOOP_TOKEN_RE.search(scan_text, next_done.end())
                        if next_done is None:
                            break
                        end = next_done.end()
                        extent = _polling_loop_extent(scan_text[candidate.start():end])
                    if extent != "closed":
                        search_pos = candidate.start() + 1
                        continue
                    m = candidate
                    match_override_span = (candidate.start(), end)
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
        # `match_override_span` carries that extended span when set.
        # G1: sliced from `text` (the unmasked original), never
        # `scan_text` -- see the comment above `match_override_span`.
        raw_start, raw_end = match_override_span if match_override_span is not None else m.span()
        # R6 (round 3): a rule's own match span can end INSIDE a quoted
        # secret value (e.g. the `+refspec` alternative matching the `+`
        # in the MIDDLE of `password="alpha bravo +charlie"`) -- slicing
        # there drops the value's own closing quote, so redact()'s
        # quoted-value alternative can't find it and falls back to the
        # bare-token alternative, leaking the rest of the value. Extend
        # the slice to the enclosing quote's real end first.
        raw_end = _extend_end_past_quote(quote_spans, raw_end)
        raw_matched = text[raw_start:raw_end]
        # F7: redact BEFORE truncating -- truncating first could slice a
        # secret in half and leave the visible fragment unredacted.
        matched = redact(raw_matched)[:MATCH_TRUNCATE]
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
        # F7: redact the full canonical text BEFORE truncating to a preview
        # (same reasoning as the `matched` redact-then-slice above).
        preview = redact(text)[:300]
        lines = []
        for fire in fires:
            entry = {
                "ts": ts,
                # G3 (review_b round 4): payload `session_id` is the same
                # attacker-controlled metadata as `cwd` below -- redact it
                # too (the RAW session_id is still used for the matching
                # logic elsewhere; only the persisted copy is scrubbed).
                "session_id": redact(session_id),
                "rule_id": fire["id"],
                "tool": tool_name,
                "decision": fire["decision"],
                "match": fire["match"],
                # First 300 chars of the canonical text (Bash: the command). The bare regex
                # match (e.g. just the subcommand that tripped a rule) cannot explain a
                # prompt after the fact, nor let step 3 cluster fires by context
                # (2026-09-06 04:06Z operator question).
                "preview": preview,
                # G3 (review_b round 3): payload `cwd` is attacker-controlled
                # metadata like every other persisted field -- redact it too
                # (matching logic above still uses the RAW `cwd`, only the
                # persisted copy is scrubbed).
                "cwd": redact(cwd),
            }
            lines.append(json.dumps(entry, sort_keys=True))
        with _open_private_append(ledger_path()) as lf:  # F7: 0600 regardless of umask
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
