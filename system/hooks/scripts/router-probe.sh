#!/usr/bin/env bash
# router-probe.sh — acceptance probe for the PreToolUse router (deliverable 5).
#
# Feeds one positive fixture and one near-miss fixture per rule id in
# router-rules.json through the REAL router implementation (a subprocess per
# fixture, never sourced/mocked), using a single temp ledger dir for the
# whole run. Prints one line per case:
#   <rule_id> expected=<decision|abstain> got=<decision|...> PASS|FAIL
# and one final ledger-count line. Exits 1 if any case FAILs or if the final
# ledger line count does not equal the number of positive fixtures (every
# near-miss must abstain and write nothing). Exit status comes from this
# script's own tally — never from a pipeline's last stage.
#
# ROUTER_IMPL selects which implementation is driven:
#   ROUTER_IMPL=python (default) — the reference pretooluse-router.py script.
#   ROUTER_IMPL=rust             — the `hex hook router` Rust port. Requires
#     HEX_ROUTER_BIN to point at a built `hex` binary (falls back to
#     ${CARGO_TARGET_DIR:-target}/release/hex under the repo root if unset).
# Same fixtures, same PASS/FAIL line format, same exit-code contract in both
# modes.
set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROUTER="$SCRIPT_DIR/pretooluse-router.py"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
ROUTER_IMPL="${ROUTER_IMPL:-python}"
HEX_ROUTER_BIN="${HEX_ROUTER_BIN:-${CARGO_TARGET_DIR:-$REPO_ROOT/target}/release/hex}"
LEDGER_DIR="$(mktemp -d)"
trap 'rm -rf "$LEDGER_DIR"' EXIT

python3 - "$ROUTER" "$LEDGER_DIR" "$ROUTER_IMPL" "$HEX_ROUTER_BIN" "$REPO_ROOT" <<'PYEOF'
import json
import os
import subprocess
import sys
from pathlib import Path

router = sys.argv[1]
ledger_dir = Path(sys.argv[2])
router_impl = sys.argv[3]
hex_router_bin = sys.argv[4]
repo_root = sys.argv[5]

if router_impl not in ("python", "rust"):
    print(
        f"[router-probe] unknown ROUTER_IMPL={router_impl!r}; expected 'python' or 'rust'",
        file=sys.stderr,
    )
    sys.exit(2)

if router_impl == "rust":
    if not hex_router_bin or not os.path.isfile(hex_router_bin) or not os.access(
        hex_router_bin, os.X_OK
    ):
        print(
            f"[router-probe] HEX_ROUTER_BIN={hex_router_bin!r} does not exist or is not "
            "executable — build it with `cargo build --release --locked` "
            "(from .hex/harness) first",
            file=sys.stderr,
        )
        sys.exit(2)

HOME_DIR = os.environ.get("HOME", "/tmp")
DEFAULT_CWD = os.path.join(HOME_DIR, "hex")


def make_payload(tool_name, tool_input, cwd=DEFAULT_CWD, session_id="probe-1"):
    return {
        "session_id": session_id,
        "transcript_path": "/tmp/transcript.jsonl",
        "cwd": cwd,
        "permission_mode": "default",
        "hook_event_name": "PreToolUse",
        "tool_name": tool_name,
        "tool_input": tool_input,
    }


# One positive + one near-miss fixture per seed rule id (deliverable 4).
# Mirrors .hex/hooks/tests/test_router.py::RULE_FIXTURES; kept in sync
# manually since this probe must run standalone against the real scripts,
# not import test machinery.
RULE_FIXTURES = [
    dict(
        id="gh-pr-merge-ci-green",
        decision="prior",
        tool_name="Bash",
        positive={"command": "gh pr merge 123 --squash"},
        near_miss={"command": "gh pr checks 123 --watch"},
    ),
    dict(
        id="vitest-spawnsync",
        decision="prior",
        tool_name="Edit",
        positive={
            "file_path": "src/components/foo.test.ts",
            "old_string": "x",
            "new_string": "const r = spawnSync('ls', []);",
        },
        near_miss={
            "file_path": "src/components/foo.ts",
            "old_string": "x",
            "new_string": "const r = spawnSync('ls', []);",
        },
    ),
    dict(
        id="git-stash-shared-checkout",
        decision="deny",
        tool_name="Bash",
        positive={"command": "git stash"},
        near_miss={"command": "git stash list"},
    ),
    dict(
        id="git-push-force",
        decision="ask",
        tool_name="Bash",
        positive={"command": "git push origin main --force"},
        near_miss={"command": "git push origin main"},
    ),
    dict(
        id="git-push-force-with-lease",
        decision="ask",
        tool_name="Bash",
        positive={"command": "git push origin main --force-with-lease"},
        near_miss={"command": "git push origin main"},
    ),
    dict(
        id="git-destructive-ask",
        decision="ask",
        tool_name="Bash",
        positive={"command": "git reset --hard HEAD~1"},
        near_miss={"command": "git reset --soft HEAD~1"},
    ),
    dict(
        id="boi-dispatch-spec-priors",
        decision="prior",
        tool_name="Bash",
        positive={"command": "boi dispatch spec.toml"},
        near_miss={"command": "boi dashboard"},
    ),
    dict(
        id="gh-fast-polling",
        decision="ask",
        tool_name="Bash",
        positive={"command": "while true; do gh pr checks 123; sleep 5; done"},
        near_miss={"command": "while true; do gh pr checks 123; sleep 90; done"},
    ),
    dict(
        id="pipe-tail-masks-exit",
        decision="prior",
        tool_name="Bash",
        positive={"command": "pytest -q | tail -20"},
        near_miss={"command": "pytest -q"},
    ),
    dict(
        id="hex-events-flat-policy",
        decision="prior",
        tool_name="Write",
        positive={
            "file_path": os.path.join(HOME_DIR, ".hex-events/policies/my-policy.yaml"),
            "content": "name: my-policy\ntrigger:\n  event: foo\naction:\n  type: shell\n  command: echo hi\n",
        },
        near_miss={
            "file_path": os.path.join(HOME_DIR, ".hex-events/policies/my-policy.yaml"),
            "content": "name: my-policy\nrules:\n  - name: r1\n    trigger:\n      event: foo\n",
        },
    ),
    dict(
        id="hex-memory-index-full",
        decision="ask",
        tool_name="Bash",
        positive={"command": "hex memory index --full"},
        near_miss={"command": "hex memory index"},
    ),
    dict(
        id="builtin-scheduler-tools",
        decision="ask",
        tool_name="CronCreate",
        positive={"schedule": "* * * * *", "command": "echo hi"},
        near_miss_tool_name="CronList",
        near_miss_input={"filter": "*"},
    ),
    dict(
        id="backticks-in-unquoted-heredoc",
        decision="prior",
        tool_name="Bash",
        positive={"command": "python3 - <<PYEOF\nprint('run `boi start` now')\nPYEOF"},
        near_miss={"command": "python3 - <<'PYEOF'\nprint('run `boi start` now')\nPYEOF"},
    ),
    dict(
        id="builtin-websearch",
        decision="ask",
        tool_name="WebSearch",
        positive={"query": "test"},
        near_miss_tool_name="WebFetch",
        near_miss_input={"url": "https://example.com"},
    ),
]

assert len(RULE_FIXTURES) == 14, "expected exactly 14 seed rule fixtures"


def run(payload):
    env = dict(os.environ)
    env["HEX_LEDGER_DIR"] = str(ledger_dir)
    if router_impl == "rust":
        env["HEX_DIR"] = repo_root
        cmd = [hex_router_bin, "hook", "router"]
    else:
        cmd = [sys.executable, router]
    return subprocess.run(
        cmd,
        input=json.dumps(payload),
        capture_output=True,
        text=True,
        env=env,
        timeout=5,
    )


def classify(proc):
    """Return the winning decision string, or 'abstain'/'error:<detail>'."""
    if proc.returncode != 0:
        return f"error:exit={proc.returncode}"
    out = proc.stdout.strip()
    if not out:
        return "abstain"
    try:
        doc = json.loads(out)
        hso = doc["hookSpecificOutput"]
    except Exception as exc:
        return f"error:parse:{exc}"
    pd = hso.get("permissionDecision")
    if pd in ("deny", "ask"):
        return pd
    if hso.get("additionalContext"):
        return "prior"
    return "error:unknown-shape"


failures = 0
ledger_path = ledger_dir / "router-fires.jsonl"


def read_ledger():
    if not ledger_path.exists():
        return []
    return [l for l in ledger_path.read_text().splitlines() if l.strip()]


for fx in RULE_FIXTURES:
    # Positive fixture: must fire with the rule's declared decision AND must
    # append exactly one ledger line stamped with THIS rule's own id — not
    # merely produce the right decision (which another rule could also
    # produce) and not merely bump a global counter another fire could pad.
    before = read_ledger()
    proc = run(make_payload(fx["tool_name"], fx["positive"]))
    got = classify(proc)
    decision_ok = got == fx["decision"]

    after = read_ledger()
    new_lines = after[len(before):]
    ledger_ok = False
    if len(new_lines) == 1:
        try:
            rec = json.loads(new_lines[0])
            ledger_ok = (
                rec.get("rule_id") == fx["id"] and rec.get("decision") == fx["decision"]
            )
        except Exception:
            ledger_ok = False

    ok = decision_ok and ledger_ok
    detail = "" if ok else f" (decision_ok={decision_ok} ledger_ok={ledger_ok} new_lines={len(new_lines)})"
    print(f"{fx['id']} expected={fx['decision']} got={got} {'PASS' if ok else 'FAIL'}{detail}")
    if not ok:
        failures += 1

    # Near-miss fixture: must abstain (empty stdout, exit 0) and write
    # nothing to the ledger.
    before = read_ledger()
    nm_tool = fx.get("near_miss_tool_name", fx["tool_name"])
    nm_input = fx.get("near_miss_input", fx.get("near_miss"))
    proc = run(make_payload(nm_tool, nm_input))
    got = classify(proc)
    after = read_ledger()
    ok = got == "abstain" and len(after) == len(before)
    print(f"{fx['id']}-near-miss expected=abstain got={got} {'PASS' if ok else 'FAIL'}")
    if not ok:
        failures += 1

lines = read_ledger()
expected_count = len(RULE_FIXTURES)
count_ok = len(lines) == expected_count
print(
    f"ledger-count expected={expected_count} got={len(lines)} "
    f"{'PASS' if count_ok else 'FAIL'}"
)
if not count_ok:
    failures += 1

sys.exit(1 if failures else 0)
PYEOF
status=$?
exit "$status"
