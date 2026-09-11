#!/usr/bin/env python3
"""PostToolUseFailure hook: append one incident record per tool failure.

Reads the PostToolUseFailure stdin JSON payload once and appends a single
line to ${HEX_LEDGER_DIR:-$HOME/.hex/ledger}/incidents.jsonl. Never writes to
stdout, never exits non-zero (fail-open: a bug here must never block a tool
or surface as a hook failure to Claude Code). Any error -> exactly one stderr
line, exit 0.
"""
import hashlib
import json
import os
import re
import sys
from datetime import datetime, timezone
from pathlib import Path

ERROR_MAX = 2000
PREVIEW_MAX = 200

# --- Shared secret-redaction policy (F7) ------------------------------------
#
# DUPLICATED VERBATIM in pretooluse-router.py. This hook runs as
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
    (
        re.compile(
            r"""(?i)\b(password|token|secret|api[_-]?key)\s*=\s*"""
            r"""(\\"(?:[^"\\]|\\[\s\S])*\\"|"(?:[^"\\]|\\[\s\S])*"|'[^']*'|\S+)"""
        ),
        r"\1=***REDACTED***",
    ),
]


def redact(text):
    """Scrub every known secret shape out of `text`. A no-op (returns the
    same string) when nothing matches -- ordinary error/args text is
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


def ledger_dir() -> Path:
    raw = os.environ.get("HEX_LEDGER_DIR")
    if raw:
        return Path(raw)
    return Path.home() / ".hex" / "ledger"


def build_record(payload: dict) -> dict:
    tool_input = payload.get("tool_input", {})
    canonical = json.dumps(tool_input, sort_keys=True)
    args_hash = hashlib.sha256(canonical.encode()).hexdigest()[:12]
    # F7: redact BEFORE truncating -- truncating first could slice a secret
    # in half and leave the visible fragment unredacted.
    error = redact(str(payload.get("error", "")))[:ERROR_MAX]
    args_preview = redact(canonical)[:PREVIEW_MAX]
    return {
        "ts": datetime.now(timezone.utc).isoformat(),
        # G3 (review_b round 4): `session_id` and `tool_use_id` are the
        # same attacker-controlled payload metadata as `cwd` below --
        # `redact()` is a no-op on an ordinary id/None, so this is safe
        # on the common case too.
        "session_id": redact(payload.get("session_id")),
        "tool": payload.get("tool_name"),
        "args_hash": args_hash,
        "args_preview": args_preview,
        "error": error,
        # G3 (review_b round 3): payload `cwd` is attacker-controlled
        # metadata like every other persisted field -- redact() is a no-op
        # on a plain path/None, so this is safe on the common case too.
        "cwd": redact(payload.get("cwd")),
        "tool_use_id": redact(payload.get("tool_use_id")),
        "duration_ms": payload.get("duration_ms"),
        "is_interrupt": payload.get("is_interrupt"),
    }


def main() -> int:
    try:
        raw = sys.stdin.buffer.read()
        if not raw.strip():
            raise ValueError("empty stdin")
        payload = json.loads(raw)
        if not isinstance(payload, dict):
            raise ValueError("stdin payload is not a JSON object")

        record = build_record(payload)

        out_dir = ledger_dir()
        _ensure_private_dir(out_dir)  # F7: 0700 regardless of umask
        with _open_private_append(out_dir / "incidents.jsonl") as f:  # F7: 0600
            f.write(json.dumps(record) + "\n")
        return 0
    except Exception as exc:  # fail-open: never block, never crash loudly to stdout
        sys.stderr.write(f"[incident] error: {exc}\n")
        return 0


if __name__ == "__main__":
    sys.exit(main())
