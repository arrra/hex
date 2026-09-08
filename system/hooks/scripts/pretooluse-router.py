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


def _window_bounds(text, start, end):
    """The substring boundaries of the "same command" as the occurrence at
    text[start:end]: from just after the nearest separator at or before
    `start` to just before the nearest separator at or after `end` (or the
    string edges). Comparing with `<=`/`>=` (not strict `<`/`>`) matters: a
    rule's `match` regex often captures its own leading separator as part of
    the anchor (e.g. a `;`-prefixed subcommand), so the separator can sit
    exactly at `start` and must still bound the window rather than being
    skipped over."""
    seps = [i for i, ch in enumerate(text) if ch in _SEPARATOR_CHARS]
    before = [p for p in seps if p <= start]
    window_start = (before[-1] + 1) if before else 0
    after = [p for p in seps if p >= end]
    window_end = after[0] if after else len(text)
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
        compiled.append(
            {
                "id": rule["id"],
                "tool_re": re.compile(rule["tool"], re.MULTILINE),
                "match_re": re.compile(rule["match"], re.MULTILINE),
                "unless_cwd_re": re.compile(unless_cwd, re.MULTILINE) if unless_cwd else None,
                "unless_match_re": re.compile(unless_match, re.MULTILINE) if unless_match else None,
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
    rules = load_rules()

    fires = []
    for rule in rules:
        if not rule["tool_re"].search(tool_name):
            continue
        if rule["unless_cwd_re"] is not None and rule["unless_cwd_re"].search(cwd):
            continue
        all_matches = list(rule["match_re"].finditer(text))
        if not all_matches:
            continue
        unless_re = rule["unless_match_re"]
        if unless_re is None:
            m = all_matches[0]
        elif len(all_matches) == 1:
            # Single occurrence: unchanged whole-text check (some rules,
            # e.g. pipe-tail-masks-exit and hex-events-flat-policy, rely on
            # a safety marker appearing ANYWHERE in the text, not just next
            # to the match).
            if unless_re.search(text):
                continue
            m = all_matches[0]
        else:
            # Multiple occurrences in one canonical text (e.g. a chained
            # Bash command with both a safe and a dangerous invocation):
            # judge each occurrence by its own "same command" window so one
            # safe occurrence can't blanket-suppress a dangerous sibling.
            m = None
            for candidate in all_matches:
                start, end = _window_bounds(text, candidate.start(), candidate.end())
                if not unless_re.search(text[start:end]):
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
