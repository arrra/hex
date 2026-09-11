"""RED-first tests for the PreToolUse router (deliverables 1, 2, 4).

Pins the contract for:
  - system/hooks/router-rules.json  (13 seed rules)
  - system/hooks/scripts/pretooluse-router.py

Neither file exists yet at authoring time — every test below is expected to
FAIL against the current worktree. The write_code phase must create both so
that this suite goes green without loosening any assertion here.

Contract summary (see spec_contract for the authoritative text):
  - stdin: PreToolUse JSON {session_id, transcript_path, cwd, permission_mode,
    hook_event_name, tool_name, tool_input}.
  - stdout (only on non-abstain): one JSON object
    {"hookSpecificOutput": {"hookEventName": "PreToolUse",
      "permissionDecision": "deny"|"ask" (absent for "prior"),
      "permissionDecisionReason": "..." (deny/ask),
      "additionalContext": "..." (prior only)}}.
  - Empty stdout + exit 0 = abstain.
  - Combining rule: any deny -> deny; else any ask -> ask; else first prior
    (in router-rules.json array order) -> additionalContext, never
    concatenated.
  - Every rule that MATCHES (fires) appends one ledger line to
    ${HEX_LEDGER_DIR}/router-fires.jsonl, regardless of whether it "won" the
    combined decision. Abstain (no rule matches) writes nothing.
  - Ledger line has exactly these keys: ts, session_id, rule_id, tool,
    decision, match, cwd.
  - Malformed/empty stdin, or any internal error: exit 0, empty stdout,
    exactly one stderr line "[router] error: ...". Never blocks a tool.
"""

import ast
import importlib.util
import json
import os
import re
import shutil
import stat
import statistics
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[3]
ROUTER_SCRIPT = REPO_ROOT / "system" / "hooks" / "scripts" / "pretooluse-router.py"
LEDGER_FILENAME = "router-fires.jsonl"

DEFAULT_CWD = "/tmp/hex-home/hex"


def make_payload(tool_name, tool_input, cwd=DEFAULT_CWD, session_id="sess-1"):
    return {
        "session_id": session_id,
        "transcript_path": "/tmp/transcript.jsonl",
        "cwd": cwd,
        "permission_mode": "default",
        "hook_event_name": "PreToolUse",
        "tool_name": tool_name,
        "tool_input": tool_input,
    }



# Production invocation (required-hooks.json / router-probe.sh): isolated mode,
# no site-packages, so cold-start cost is paid once and consistently in every
# measurement below (operator amendment, 2026-09-06).
PYTHON_ISOLATED_FLAGS = ["-I", "-S"]


def run_router(stdin_text, ledger_dir, extra_env=None, timeout=5):
    env = dict(os.environ)
    env["HEX_LEDGER_DIR"] = str(ledger_dir)
    if extra_env:
        env.update(extra_env)
    proc = subprocess.run(
        [sys.executable, *PYTHON_ISOLATED_FLAGS, str(ROUTER_SCRIPT)],
        input=stdin_text,
        capture_output=True,
        text=True,
        env=env,
        timeout=timeout,
    )
    return proc


def run_router_payload(payload, ledger_dir, **kw):
    return run_router(json.dumps(payload), ledger_dir, **kw)


def read_ledger(ledger_dir):
    path = Path(ledger_dir) / LEDGER_FILENAME
    if not path.exists():
        return []
    lines = [l for l in path.read_text().splitlines() if l.strip()]
    return [json.loads(l) for l in lines]


# Full ledger-row schema (matches the `entry` dict `evaluate()` writes, not
# just the two fields a precedence test cares about winning) -- used by
# `assert_complete_ledger` below so a "complete ledger" assertion actually
# checks completeness (row count AND full row shape), not just a
# rule_id -> decision dict comprehension that silently collapses a
# duplicate row and can't see whether the other fields were stripped
# (G4, review_b round 3).
LEDGER_ROW_KEYS = {
    "ts", "session_id", "rule_id", "tool", "decision", "match", "preview", "cwd",
}


def assert_complete_ledger(testcase, lines, expected_rule_decisions):
    """G4: assert the ledger has EXACTLY the expected rows -- right count
    (catches an appended duplicate), right rule_id->decision mapping, and
    every row carries the full schema (catches rows reduced to a subset of
    fields)."""
    testcase.assertEqual(
        len(lines), len(expected_rule_decisions),
        f"expected exactly {len(expected_rule_decisions)} ledger row(s), "
        f"got {len(lines)}: {lines!r}",
    )
    for line in lines:
        testcase.assertEqual(set(line.keys()), LEDGER_ROW_KEYS)
    testcase.assertEqual(
        {l["rule_id"]: l["decision"] for l in lines}, expected_rule_decisions
    )


# Seed rules in the order the spec lists them (deliverable 4). The router's
# "first prior wins" behavior is pinned against this order.
SEED_RULE_ORDER = [
    "gh-pr-merge-ci-green",
    "vitest-spawnsync",
    "git-stash-shared-checkout",
    "git-push-force",
    "git-push-force-with-lease",
    "git-destructive-ask",
    "boi-dispatch-spec-priors",
    "gh-fast-polling",
    "pipe-tail-masks-exit",
    "hex-events-flat-policy",
    "hex-memory-index-full",
    "builtin-scheduler-tools",
    "builtin-websearch",
    "backticks-in-unquoted-heredoc",
]

# Each entry: (rule_id, decision, tool_name, positive tool_input, near-miss
# tool_input, optional cwd override for the positive fixture).
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
            "file_path": "/tmp/hex-home/.hex-events/policies/my-policy.yaml",
            "content": "name: my-policy\ntrigger:\n  event: foo\naction:\n  type: shell\n  command: echo hi\n",
        },
        near_miss={
            "file_path": "/tmp/hex-home/.hex-events/policies/my-policy.yaml",
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
        near_miss=None,  # near-miss uses a different tool_name; see below
        near_miss_tool_name="CronList",
        near_miss_input={"filter": "*"},
    ),
    dict(
        id="builtin-websearch",
        decision="ask",
        tool_name="WebSearch",
        positive={"query": "test"},
        near_miss=None,
        near_miss_tool_name="WebFetch",
        near_miss_input={"url": "https://example.com"},
    ),
    dict(
        id="backticks-in-unquoted-heredoc",
        decision="prior",
        tool_name="Bash",
        positive={"command": "python3 - <<PYEOF\nprint('run `boi start` now')\nPYEOF"},
        near_miss={"command": "python3 - <<'PYEOF'\nprint('run `boi start` now')\nPYEOF"},
    ),
]

assert {f["id"] for f in RULE_FIXTURES} == set(SEED_RULE_ORDER), "fixture/order drift"
assert len(RULE_FIXTURES) == 14, "expected exactly 14 seed rules"


class RouterTestCase(unittest.TestCase):
    def setUp(self):
        self.assertTrue(
            ROUTER_SCRIPT.exists(),
            f"pretooluse-router.py must exist at {ROUTER_SCRIPT} (deliverable 2)",
        )


class TestSeedRulePositiveFixtures(RouterTestCase):
    def test_each_rule_fires_with_expected_decision(self):
        for fx in RULE_FIXTURES:
            with self.subTest(rule=fx["id"]):
                with tempfile.TemporaryDirectory() as ledger_dir:
                    payload = make_payload(fx["tool_name"], fx["positive"])
                    proc = run_router_payload(payload, ledger_dir)

                    self.assertEqual(
                        proc.returncode, 0,
                        f"{fx['id']}: router must exit 0; stderr={proc.stderr!r}",
                    )
                    self.assertEqual(
                        proc.stderr.strip(), "",
                        f"{fx['id']}: abstain/fire must produce no stderr, got {proc.stderr!r}",
                    )
                    stdout = proc.stdout.strip()
                    self.assertTrue(
                        stdout,
                        f"{fx['id']}: expected non-empty stdout for a firing rule",
                    )
                    out = json.loads(stdout)
                    hso = out["hookSpecificOutput"]
                    self.assertEqual(hso["hookEventName"], "PreToolUse")

                    if fx["decision"] == "deny":
                        self.assertEqual(hso.get("permissionDecision"), "deny")
                        self.assertTrue(hso.get("permissionDecisionReason"))
                    elif fx["decision"] == "ask":
                        self.assertEqual(hso.get("permissionDecision"), "ask")
                        self.assertTrue(hso.get("permissionDecisionReason"))
                    else:  # prior
                        self.assertNotIn(hso.get("permissionDecision"), ("deny", "ask"))
                        self.assertTrue(hso.get("additionalContext"))

                    lines = read_ledger(ledger_dir)
                    self.assertEqual(
                        len(lines), 1,
                        f"{fx['id']}: expected exactly one ledger line, got {lines}",
                    )
                    entry = lines[0]
                    self.assertEqual(
                        set(entry.keys()),
                        {"ts", "session_id", "rule_id", "tool", "decision", "match", "preview", "cwd"},
                    )
                    self.assertEqual(entry["rule_id"], fx["id"])
                    self.assertEqual(entry["tool"], fx["tool_name"])
                    self.assertEqual(entry["decision"], fx["decision"])
                    self.assertEqual(entry["cwd"], DEFAULT_CWD)
                    self.assertEqual(entry["session_id"], "sess-1")
                    self.assertIsInstance(entry["match"], str)
                    self.assertTrue(0 < len(entry["match"]) <= 200)
                    self.assertIsInstance(entry["ts"], str)
                    self.assertTrue(entry["ts"])


class TestSeedRuleNearMisses(RouterTestCase):
    def test_each_rule_abstains_on_near_miss(self):
        for fx in RULE_FIXTURES:
            with self.subTest(rule=fx["id"]):
                tool_name = fx.get("near_miss_tool_name", fx["tool_name"])
                tool_input = fx.get("near_miss_input", fx["near_miss"])
                with tempfile.TemporaryDirectory() as ledger_dir:
                    payload = make_payload(tool_name, tool_input)
                    proc = run_router_payload(payload, ledger_dir)

                    self.assertEqual(proc.returncode, 0, f"{fx['id']}: near-miss must exit 0")
                    self.assertEqual(
                        proc.stdout.strip(), "",
                        f"{fx['id']}: near-miss fixture must abstain (empty stdout), "
                        f"got {proc.stdout!r}",
                    )
                    self.assertEqual(
                        proc.stderr.strip(), "",
                        f"{fx['id']}: abstain/fire must produce no stderr, got {proc.stderr!r}",
                    )
                    lines = read_ledger(ledger_dir)
                    self.assertEqual(
                        lines, [],
                        f"{fx['id']}: abstain must write nothing to the ledger",
                    )

    def test_git_stash_unless_cwd_skips_inside_worktrees(self):
        """git-stash-shared-checkout's unless_cwd exempts /worktrees/ checkouts
        even though the command itself matches — a distinct abstain path from
        the plain near-miss (git stash list) covered above."""
        with tempfile.TemporaryDirectory() as ledger_dir:
            payload = make_payload(
                "Bash",
                {"command": "git stash"},
                cwd="/tmp/hex-home/.boi/v2/worktrees/Scnfz8k1f/T7w2t3bzf",
            )
            proc = run_router_payload(payload, ledger_dir)
            self.assertEqual(proc.returncode, 0)
            self.assertEqual(proc.stdout.strip(), "")
            self.assertEqual(
                proc.stderr.strip(), "",
                f"abstain/fire must produce no stderr, got {proc.stderr!r}",
            )
            self.assertEqual(read_ledger(ledger_dir), [])


class TestGhFastPollingScope(RouterTestCase):
    """Regression (2026-09-06 03:10Z, live false positive in another session): a bounded
    `for` loop over a fixed PR list with a short sleep is a one-shot batch, not polling.
    Only open-ended loops (`while` / `until`) are the rate-limit risk the rule exists for."""

    def test_bounded_for_loop_over_pr_list_abstains(self):
        cmd = ('for n in 293 294 295 296 297; do t=$(gh pr view $n -R owner/repo --json title --jq .title); '
               'python3 hex_emit.py github.pr.opened "$t"; sleep 2; done')
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
            self.assertEqual(proc.returncode, 0)
            self.assertEqual(proc.stdout.strip(), "", f"bounded for-loop must abstain, got {proc.stdout!r}")
            self.assertEqual(read_ledger(ledger_dir), [])

    def test_until_loop_polling_still_asks(self):
        cmd = 'until gh run view 123 --json status --jq .status | grep -q completed; do sleep 10; done'
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
            out = json.loads(proc.stdout)
            self.assertEqual(out["hookSpecificOutput"]["permissionDecision"], "ask")
            self.assertEqual(read_ledger(ledger_dir)[0]["rule_id"], "gh-fast-polling")


class TestCommandPositionAnchoring(RouterTestCase):
    """Live false positive 2026-09-06 03:20Z: a heredoc whose Python source mentioned the stash
    command was denied. Bash rules match a command only at command position (start / after a
    separator), never a mention inside quotes, messages, heredocs, echo, or grep arguments."""

    MENTIONS = (
        'echo "please do not run git stash here"',
        'git commit -m "router: no git stash in shared checkouts"',
        "grep -rn 'git stash' docs/",
        "python3 - <<'EOF'\nrule = r'git stash'\nprint(rule)\nEOF",
        'echo "git push --force is bad"',
        "cat notes.txt | grep 'gh pr merge'",
    )

    def test_mentions_inside_text_abstain(self):
        for cmd in self.MENTIONS:
            with self.subTest(cmd=cmd), tempfile.TemporaryDirectory() as ledger_dir:
                proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
                self.assertEqual(proc.stdout.strip(), "", f"mention must abstain: {cmd!r}")
                self.assertEqual(read_ledger(ledger_dir), [])

    def test_real_invocations_still_fire(self):
        cases = {
            "git stash": "deny",
            "cd /tmp/x && git stash -u": "deny",
            "git add -A; git stash push -m wip": "deny",
            "time git push origin feat -f": "ask",
            "git push --force-with-lease origin feat": "ask",
            "gh pr merge 12 --squash": "prior",
        }
        for cmd, expected in cases.items():
            with self.subTest(cmd=cmd), tempfile.TemporaryDirectory() as ledger_dir:
                proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
                out = json.loads(proc.stdout)["hookSpecificOutput"]
                got = out.get("permissionDecision") or ("prior" if out.get("additionalContext") else None)
                self.assertEqual(got, expected, cmd)


class TestGitStashRecoveryOpsAllowed(RouterTestCase):
    def test_stash_pop_apply_drop_abstain(self):
        for cmd in ("git stash pop", "git stash apply stash@{0}", "git stash drop", "git -C /tmp/r stash pop"):
            with self.subTest(cmd=cmd), tempfile.TemporaryDirectory() as ledger_dir:
                proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
                self.assertEqual(proc.stdout.strip(), "", f"{cmd!r} must abstain")


class TestUnlessMatchScopedPerOccurrence(RouterTestCase):
    """Review finding G1: a global `unless_match` (searching the WHOLE
    canonical text) lets a single safe sub-invocation blanket-suppress a
    dangerous sibling chained in the same command string. A single-command
    canonical text must keep the original whole-text behavior unchanged
    (some rules, e.g. pipe-tail-masks-exit, intentionally rely on a safety
    marker appearing anywhere in the text) — only a text with MORE THAN ONE
    occurrence of the rule's `match` pattern gets the narrower per-occurrence
    window check.
    """

    def test_safe_subcommand_does_not_shield_a_chained_dangerous_one(self):
        cmd = "git stash pop; git stash"
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
            hso = json.loads(proc.stdout)["hookSpecificOutput"]
            self.assertEqual(hso["permissionDecision"], "deny", cmd)
            self.assertEqual(read_ledger(ledger_dir)[-1]["rule_id"], "git-stash-shared-checkout")

    def test_force_with_lease_does_not_shield_a_chained_bare_force(self):
        cmd = "git push origin main --force-with-lease; git push origin main --force"
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
            hso = json.loads(proc.stdout)["hookSpecificOutput"]
            self.assertEqual(hso["permissionDecision"], "ask", cmd)
            rule_ids = {entry["rule_id"] for entry in read_ledger(ledger_dir)}
            self.assertIn("git-push-force", rule_ids)

    def test_single_occurrence_pipefail_check_is_unaffected(self):
        """Regression guard: the multi-occurrence path must never fire for a
        text where the rule's `match` pattern has only ONE occurrence — this
        is exactly the existing pipe-tail-masks-exit contract
        (TestPipeTailAbstainsWhenStatusCaptured) and must stay untouched."""
        for cmd in (
            "set -o pipefail; cargo test --locked | tail -20",
            "pytest -q | tail -5; echo exit=${PIPESTATUS[0]}",
        ):
            with self.subTest(cmd=cmd), tempfile.TemporaryDirectory() as ledger_dir:
                proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
                self.assertEqual(proc.stdout.strip(), "", cmd)
                self.assertEqual(read_ledger(ledger_dir), [])


class TestCanonicalJsonMatchesPythonEnsureAscii(RouterTestCase):
    """Review finding G2 (Rust side): the Rust port's canonical-text branch
    for tool_input compact-JSON must escape non-ASCII exactly like Python's
    `json.dumps(..., sort_keys=True)` default (`ensure_ascii=True`). This
    test pins the PYTHON reference's own byte output (ground truth) so a
    future change to canonical_text on either side can be diffed against it
    directly."""

    def test_non_ascii_tool_input_escapes_to_uXXXX(self):
        spec = importlib.util.spec_from_file_location(
            "pretooluse_router_under_test_ascii", ROUTER_SCRIPT
        )
        assert spec and spec.loader
        router_mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(router_mod)

        text = router_mod.canonical_text(
            "CronCreate", {"schedule": "* * * * *", "command": "echo héllo"}
        )
        self.assertEqual(
            text,
            '{"command":"echo h\\u00e9llo","schedule":"* * * * *"}',
        )
        self.assertTrue(text.isascii(), "canonical text for non-Bash/text tools must be ASCII-only")


class TestFlatPolicyRuleOnlyOnFullWrites(RouterTestCase):
    def test_edit_of_policy_yaml_abstains(self):
        payload = make_payload("Edit", {"file_path": "/tmp/hex-home/x/.hex-events/policies/foo.yaml",
                                        "old_string": "timeout: 60", "new_string": "timeout: 120"})
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(payload, ledger_dir)
            self.assertEqual(proc.stdout.strip(), "")


class TestPipeTailAbstainsWhenStatusCaptured(RouterTestCase):
    """The prior is about a MASKED exit status. A command that already uses pipefail or reads
    PIPESTATUS has not masked anything - the prior would be noise."""

    def test_pipefail_or_pipestatus_abstains(self):
        for cmd in (
            "set -o pipefail; cargo test --locked | tail -20",
            "pytest -q | tail -5; echo exit=${PIPESTATUS[0]}",
        ):
            with self.subTest(cmd=cmd), tempfile.TemporaryDirectory() as ledger_dir:
                proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
                self.assertEqual(proc.stdout.strip(), "", cmd)
                self.assertEqual(read_ledger(ledger_dir), [])

    def test_plain_pipe_to_tail_still_fires(self):
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(make_payload("Bash", {"command": "pnpm test | tail -20"}), ledger_dir)
            self.assertIn("additionalContext", json.loads(proc.stdout)["hookSpecificOutput"])
            self.assertEqual(read_ledger(ledger_dir)[0]["rule_id"], "pipe-tail-masks-exit")


class TestPipeFilterVariants(RouterTestCase):
    """2026-09-06 04:10Z: two commits landed on a RED suite because `unittest ... | grep -E
    '^(Ran|OK|FAILED)'` returned grep's status and colored output hid the FAILED line.
    grep/head mask exit status exactly like tail."""

    def test_grep_and_head_fire_like_tail(self):
        for cmd in ("python3 -m unittest discover -s tests | grep -E '^(Ran|OK|FAILED)'",
                    "cargo test --locked | head -40"):
            with self.subTest(cmd=cmd), tempfile.TemporaryDirectory() as ledger_dir:
                proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
                self.assertIn("additionalContext", json.loads(proc.stdout)["hookSpecificOutput"], cmd)
                self.assertEqual(read_ledger(ledger_dir)[0]["rule_id"], "pipe-tail-masks-exit")

    def test_redirect_attached_ampersand_still_fires(self):
        """F1 (round-2 redo): `2>&1` before the pipe is a stderr-to-stdout
        redirect, not a `&` separator that should end the "same command"
        scan — the F20 fix must not regress this to abstain."""
        cmd = "cargo test 2>&1 | tail -20"
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
            self.assertIn(
                "additionalContext", json.loads(proc.stdout)["hookSpecificOutput"],
                f"2>&1 before the pipe must not be treated as a command separator: {proc.stdout!r}",
            )
            self.assertEqual(read_ledger(ledger_dir)[0]["rule_id"], "pipe-tail-masks-exit")


class TestCombinedOutcomeSemantics(RouterTestCase):
    def test_deny_beats_prior_when_both_match(self):
        """A single command matches gh-pr-merge-ci-green (prior) AND
        git-stash-shared-checkout (deny). The combined stdout decision must be deny,
        exactly once — but both rules still each get a ledger line, since
        every match is a FIRE regardless of who wins the combined verdict.
        (Was git-push-force until 2026-09-06, when that rule became `ask`.)"""
        with tempfile.TemporaryDirectory() as ledger_dir:
            payload = make_payload(
                "Bash",
                {"command": "gh pr merge 42 --squash; git stash"},
            )
            proc = run_router_payload(payload, ledger_dir)

            self.assertEqual(proc.returncode, 0)
            stdout = proc.stdout.strip()
            self.assertTrue(stdout)
            # Exactly one JSON document on stdout.
            out = json.loads(stdout)
            hso = out["hookSpecificOutput"]
            self.assertEqual(hso.get("permissionDecision"), "deny")
            # F17: pin the EXACT winning message, not just the decision type --
            # returning the wrong rule's text (or a concatenation) would still
            # satisfy a bare `== "deny"` check.
            self.assertEqual(
                hso.get("permissionDecisionReason"),
                "Standing Order 7: no stash save/push in a shared checkout - it "
                "sweeps sibling agents' uncommitted work (2026-08-19/20, 23 files "
                "lost). Work in a git worktree. Recovery ops (pop/apply/drop/list/"
                "show) are allowed.",
            )
            self.assertNotIn("additionalContext", hso)

            lines = read_ledger(ledger_dir)
            rule_ids = sorted(l["rule_id"] for l in lines)
            self.assertEqual(rule_ids, sorted(["gh-pr-merge-ci-green", "git-stash-shared-checkout"]))
            decisions = {l["rule_id"]: l["decision"] for l in lines}
            self.assertEqual(decisions["gh-pr-merge-ci-green"], "prior")
            self.assertEqual(decisions["git-stash-shared-checkout"], "deny")

    def test_only_one_additional_context_when_multiple_priors_match(self):
        """Two prior-only rules match the same command (pipe-tail-masks-exit
        and gh-pr-merge-ci-green). Only ONE additionalContext may ever reach
        stdout — never concatenated — and per the documented seed order
        (deliverable 4), gh-pr-merge-ci-green is listed first, so it wins."""
        with tempfile.TemporaryDirectory() as ledger_dir:
            payload = make_payload(
                "Bash",
                {"command": "pytest -q | tail -20 && gh pr merge 7 --squash"},
            )
            proc = run_router_payload(payload, ledger_dir)

            self.assertEqual(proc.returncode, 0)
            stdout = proc.stdout.strip()
            self.assertTrue(stdout)
            out = json.loads(stdout)
            hso = out["hookSpecificOutput"]
            self.assertNotIn(hso.get("permissionDecision"), ("deny", "ask"))
            self.assertIn("additionalContext", hso)
            # F17: pin the EXACT winning message text (first prior in file
            # order), not just that some string is present.
            self.assertEqual(
                hso["additionalContext"],
                "Never merge while checks are pending; run `gh pr checks <n> "
                "--watch` first.",
            )

            lines = read_ledger(ledger_dir)
            rule_ids = sorted(l["rule_id"] for l in lines)
            self.assertEqual(
                rule_ids, sorted(["gh-pr-merge-ci-green", "pipe-tail-masks-exit"])
            )
            # Exactly one line is the "winner" carried into additionalContext;
            # the contract requires the FIRST prior in file order to win.
            self.assertEqual(
                {l["rule_id"]: l["decision"] for l in lines},
                {"gh-pr-merge-ci-green": "prior", "pipe-tail-masks-exit": "prior"},
            )

    def test_ask_beats_prior_exact_message_and_complete_ledger(self):
        """F17: the missing ask-vs-prior precedence pair. git-push-force
        (ask, fires on the leading `+` refspec) and gh-pr-merge-ci-green
        (prior) both match; ask must win with its exact message, and both
        rules must still each get a ledger line."""
        with tempfile.TemporaryDirectory() as ledger_dir:
            payload = make_payload(
                "Bash",
                {"command": "git push origin +HEAD:main; gh pr merge 42 --squash"},
            )
            proc = run_router_payload(payload, ledger_dir)

            self.assertEqual(proc.returncode, 0)
            stdout = proc.stdout.strip()
            self.assertTrue(stdout)
            out = json.loads(stdout)
            hso = out["hookSpecificOutput"]
            self.assertEqual(hso.get("permissionDecision"), "ask")
            self.assertEqual(
                hso.get("permissionDecisionReason"),
                "Force-push is ASK-FIRST (operator policy). NEVER on a "
                "stacked-PR branch (false-MERGED class) - use gh stack sync / "
                "gh stack push. Personal single branch after a rebase: "
                "confirm. (F5: a leading `+` on a refspec forces the update "
                "the same as --force.)",
            )
            self.assertNotIn("additionalContext", hso)

            lines = read_ledger(ledger_dir)
            assert_complete_ledger(
                self, lines,
                {"gh-pr-merge-ci-green": "prior", "git-push-force": "ask"},
            )

    def test_deny_beats_ask_exact_message_and_complete_ledger(self):
        """F17: the missing deny-vs-ask precedence pair. git-push-force (ask)
        and git-stash-shared-checkout (deny) both match; deny must win with
        its exact message, and both rules must still each get a ledger
        line."""
        with tempfile.TemporaryDirectory() as ledger_dir:
            payload = make_payload(
                "Bash",
                {"command": "git push --force origin main; git stash"},
            )
            proc = run_router_payload(payload, ledger_dir)

            self.assertEqual(proc.returncode, 0)
            stdout = proc.stdout.strip()
            self.assertTrue(stdout)
            out = json.loads(stdout)
            hso = out["hookSpecificOutput"]
            self.assertEqual(hso.get("permissionDecision"), "deny")
            self.assertEqual(
                hso.get("permissionDecisionReason"),
                "Standing Order 7: no stash save/push in a shared checkout - it "
                "sweeps sibling agents' uncommitted work (2026-08-19/20, 23 files "
                "lost). Work in a git worktree. Recovery ops (pop/apply/drop/list/"
                "show) are allowed.",
            )
            self.assertNotIn("additionalContext", hso)

            lines = read_ledger(ledger_dir)
            assert_complete_ledger(
                self, lines,
                {"git-push-force": "ask", "git-stash-shared-checkout": "deny"},
            )


class TestMalformedInput(RouterTestCase):
    def _assert_fails_open(self, stdin_text):
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router(stdin_text, ledger_dir)
            self.assertEqual(proc.returncode, 0, "must never block a tool on our own bug")
            self.assertEqual(proc.stdout.strip(), "")
            stderr_lines = [l for l in proc.stderr.splitlines() if l.strip()]
            self.assertEqual(
                len(stderr_lines), 1,
                f"expected exactly one stderr line, got {stderr_lines!r}",
            )
            self.assertTrue(stderr_lines[0].startswith("[router] error:"))
            self.assertEqual(read_ledger(ledger_dir), [])

    def test_malformed_json_stdin(self):
        self._assert_fails_open("{not valid json")

    def test_empty_stdin(self):
        self._assert_fails_open("")


class TestScriptSafety(RouterTestCase):
    """Operator decision #2 (2026-09-06 02:40Z): timing budgets (absolute AND
    relative) are load-sensitive on this shared worker and were dropped
    entirely. In their place: a STRUCTURAL guarantee that the router is
    mechanically incapable of being slow in the ways that matter (no
    subprocess/network/git/gh calls, only cheap stdlib imports) — that is
    the real content behind "fast, no LLM, no network, no git".
    """

    ALLOWED_IMPORTS = {"sys", "os", "re", "json", "hashlib", "time", "datetime", "pathlib"}
    FORBIDDEN_TOKENS = ("subprocess", "socket", "urllib", "http", "requests", "git ", "gh ")

    def _assert_safe_script(self, path):
        self.assertTrue(path.exists(), f"{path} must exist")
        text = path.read_text()
        tree = ast.parse(text, filename=str(path))
        imported = set()
        for node in ast.walk(tree):
            if isinstance(node, ast.Import):
                for alias in node.names:
                    imported.add(alias.name.split(".")[0])
            elif isinstance(node, ast.ImportFrom):
                if node.module:
                    imported.add(node.module.split(".")[0])
        disallowed = imported - self.ALLOWED_IMPORTS
        self.assertEqual(
            disallowed, set(),
            f"{path.name} imports disallowed modules: {sorted(disallowed)} "
            f"(allowlist: {sorted(self.ALLOWED_IMPORTS)})",
        )
        for token in self.FORBIDDEN_TOKENS:
            self.assertNotIn(
                token, text,
                f"{path.name} contains forbidden token {token!r} — hooks must "
                "stay pure-stdlib with no subprocess/network/git/gh calls",
            )

    def test_router_script_is_pure_stdlib_no_subprocess_no_network(self):
        self._assert_safe_script(ROUTER_SCRIPT)


class TestLatencySanityCeiling(RouterTestCase):
    """No absolute or relative millisecond budget is asserted (operator
    decision #2 — both proved load-sensitive on this shared worker). This
    only catches a genuine hang; medians are printed for a human to
    sanity-check, never asserted on.
    """

    def test_router_invocation_never_exceeds_hang_ceiling(self):
        payload = make_payload("Bash", {"command": "echo hello world"})
        stdin_text = json.dumps(payload)

        router_durations = []
        with tempfile.TemporaryDirectory() as ledger_dir:
            for _ in range(20):
                start = time.perf_counter()
                proc = run_router(stdin_text, ledger_dir, timeout=5)
                router_durations.append(time.perf_counter() - start)
                self.assertEqual(proc.returncode, 0)
                self.assertEqual(proc.stdout.strip(), "")
            self.assertEqual(read_ledger(ledger_dir), [])

        baseline_durations = []
        for _ in range(20):
            start = time.perf_counter()
            proc = subprocess.run(
                [sys.executable, *PYTHON_ISOLATED_FLAGS, "-c", "pass"],
                capture_output=True,
                text=True,
                timeout=5,
            )
            baseline_durations.append(time.perf_counter() - start)
            self.assertEqual(proc.returncode, 0)

        router_median_s = statistics.median(router_durations)
        baseline_median_s = statistics.median(baseline_durations)
        delta_s = router_median_s - baseline_median_s

        print(f"\n[latency] router median (python3 -I -S pretooluse-router.py): "
              f"{router_median_s * 1000:.2f} ms")
        print(f"[latency] interpreter cold-start baseline (python3 -I -S -c pass): "
              f"{baseline_median_s * 1000:.2f} ms")
        print(f"[latency] delta: {delta_s * 1000:.2f} ms")

        # F18: the tight per-invocation ceiling is load-sensitive on a
        # shared worker (scheduler delays, filesystem stalls) and produced
        # correctness-suite failures with no router regression. It moves
        # behind an opt-in ROUTER_BENCH=1 gate; the structural guarantee --
        # the 5s subprocess `timeout=` passed to every `run_router*` call
        # above -- stays mandatory and unconditional.
        if os.environ.get("ROUTER_BENCH") == "1":
            for d in router_durations:
                self.assertLess(
                    d, 1.0,
                    f"one router invocation took {d * 1000:.2f}ms — exceeds the "
                    "1000ms sanity ceiling meant only to catch a genuine hang "
                    "(never load noise)",
                )


class TestNoLookaroundInRules(RouterTestCase):
    """Rust's `regex` crate has no lookaround support (spec ground truth,
    2026-09-07 closed-loop harness plan). The Python router is the reference
    a future Rust port must mirror, so every rule's `match`/`unless_match`
    must be expressible without `(?!...)` / `(?=...)` / `(?<...)`. The four
    rules that currently rely on lookaround (git-stash-shared-checkout,
    git-push-force, pipe-tail-masks-exit, hex-events-flat-policy) must be
    rewritten to use the new `unless_match` field instead. This test is RED
    until that rewrite lands.
    """

    LOOKAROUND_RE = re.compile(r"\(\?[=!<]")

    def test_no_rule_field_uses_lookaround(self):
        rules_path = REPO_ROOT / "system" / "hooks" / "router-rules.json"
        rules = json.loads(rules_path.read_text())
        offenders = []
        for rule in rules:
            for field in ("match", "unless_match"):
                value = rule.get(field)
                if value and self.LOOKAROUND_RE.search(value):
                    offenders.append(f"{rule['id']}.{field}")
        self.assertEqual(
            offenders, [],
            f"rules still use lookaround (unsupported by Rust's `regex` crate): {offenders}",
        )


class TestUnlessMatchField(RouterTestCase):
    """Pins the contract for the new optional `unless_match` rule field
    (spec: regex on the SAME canonical text; when it matches, the rule does
    not fire). Exercises the router's `evaluate()`/`load_rules()` directly,
    with an isolated one-rule fixture file, so the mechanism itself is
    pinned independently of which of the four seed rules ends up using it.

    RED now: `load_rules()` does not read `unless_match` at all, so the
    rule below fires unconditionally on any text containing "foo" and both
    assertions fail.
    """

    def _load_router_module(self):
        spec = importlib.util.spec_from_file_location(
            "pretooluse_router_under_test", ROUTER_SCRIPT
        )
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module

    def test_unless_match_suppresses_fire_and_is_absent_from_ledger(self):
        router_mod = self._load_router_module()

        with tempfile.TemporaryDirectory() as tmp:
            rules_path = Path(tmp) / "router-rules.json"
            rules_path.write_text(json.dumps([
                {
                    "id": "test-unless-match-suppression",
                    "tool": "^Bash$",
                    "match": "foo",
                    "unless_match": "foo-safe",
                    "decision": "ask",
                    "message": "should never surface when unless_match matches",
                }
            ]))
            ledger_dir = Path(tmp) / "ledger"
            ledger_dir.mkdir()

            router_mod.RULES_PATH = str(rules_path)
            old_ledger_env = os.environ.get("HEX_LEDGER_DIR")
            os.environ["HEX_LEDGER_DIR"] = str(ledger_dir)
            try:
                payload = make_payload("Bash", {"command": "run foo-safe now"})
                winner = router_mod.evaluate(payload)
            finally:
                if old_ledger_env is None:
                    os.environ.pop("HEX_LEDGER_DIR", None)
                else:
                    os.environ["HEX_LEDGER_DIR"] = old_ledger_env

            self.assertIsNone(
                winner,
                "unless_match matching the canonical text must suppress the fire entirely",
            )
            self.assertEqual(
                read_ledger(ledger_dir), [],
                "a rule suppressed by unless_match must not be recorded anywhere in the ledger",
            )


class TestInvocationLocalStashExemption(RouterTestCase):
    """F1 (arrra/hex PR #5 round 1): `unless_match` for the stash rule must
    be decided from the actual stash subcommand of EACH `git stash`
    invocation, never from arbitrary safe-looking text elsewhere in the
    canonical text (e.g. an echoed string or a commit message argument)."""

    def test_misleading_safe_text_elsewhere_does_not_suppress_denial(self):
        cases = (
            "echo 'stash list'; git stash",
            "git stash push -m 'stash pop'",
        )
        for cmd in cases:
            with self.subTest(cmd=cmd), tempfile.TemporaryDirectory() as ledger_dir:
                proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
                self.assertTrue(proc.stdout.strip(), f"{cmd!r} must not abstain (F1)")
                hso = json.loads(proc.stdout)["hookSpecificOutput"]
                self.assertEqual(hso.get("permissionDecision"), "deny", cmd)


class TestShellPrefixNormalization(RouterTestCase):
    """F3: leading whitespace, `NAME=value` assignment prefixes, and
    wrappers such as `command` must be skipped before the subcommand is
    matched; `git`'s global options (e.g. `-C <path>`) must be skipped
    before the git subcommand is matched."""

    def test_normalized_prefixes_still_trip_git_rules(self):
        cases = {
            " git stash": "deny",
            "FOO=1 git stash": "deny",
            "command git stash": "deny",
            "git -C /shared push --force": "ask",
        }
        for cmd, expected in cases.items():
            with self.subTest(cmd=cmd), tempfile.TemporaryDirectory() as ledger_dir:
                proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
                self.assertTrue(proc.stdout.strip(), f"{cmd!r} must not abstain (F3)")
                hso = json.loads(proc.stdout)["hookSpecificOutput"]
                self.assertEqual(hso.get("permissionDecision"), expected, cmd)


class TestAssignmentPrefixReDoS(RouterTestCase):
    """F9: `(?:\\S+=\\S*\\s+)*` lets `\\S+` and `\\S*` both absorb `=`
    characters, so a token like `A=B=C` has more than one way to split
    across the pattern. When the text never supplies the literal command the
    rule expects, the regex engine explores every combination — exponential
    in the number of repeated tokens. Restricting the assignment name to
    `[A-Za-z_][A-Za-z0-9_]*` (F9's fix) removes the ambiguity."""

    def test_env_repeated_ambiguous_assignments_does_not_hang(self):
        # Empirically: this exact input makes the current (vulnerable) router
        # exceed a 5s timeout (measured ~5s+ wall clock before this fix).
        cmd = "env " + "A=B=C " * 24 + "true"
        with tempfile.TemporaryDirectory() as ledger_dir:
            try:
                proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir, timeout=5)
            except subprocess.TimeoutExpired:
                self.fail(
                    "router hung (>5s) on adversarial env-assignment input — "
                    "exponential backtracking in the assignment-prefix regex (F9)"
                )
            self.assertEqual(proc.returncode, 0)
            self.assertEqual(proc.stdout.strip(), "", "not a real git/gh invocation, must abstain")


class TestPipefailIsShellWideAcrossPipelines(RouterTestCase):
    """F10: the pipefail exemption is SHELL-wide for subsequent pipelines in
    the same command, unlike the stash rule's invocation-local exemption
    (F1/F3). Once `set -o pipefail` has been set, every later pipeline in
    the same shell is protected, not just the first one the per-occurrence
    window happens to cover."""

    def test_multiple_pipelines_after_set_pipefail_abstain(self):
        cmd = "set -o pipefail; pytest -q | tail -5; npm test | tail -5"
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
            self.assertEqual(
                proc.stdout.strip(), "",
                f"pipefail must exempt both subsequent pipelines (F10): {proc.stdout!r}",
            )
            self.assertEqual(read_ledger(ledger_dir), [])


class TestExecutableRegionScanner(RouterTestCase):
    """F2: single/double-quoted text and QUOTED heredoc bodies are
    non-executable and must not trip command rules; a real `$(...)`
    command substitution must still be treated as executable."""

    def test_quoted_text_and_quoted_heredoc_abstain(self):
        cases = (
            "printf '%s\\n' 'example; git stash'",
            "cat <<'EOF2'\ngit stash\nEOF2\n",
        )
        for cmd in cases:
            with self.subTest(cmd=cmd), tempfile.TemporaryDirectory() as ledger_dir:
                proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
                self.assertEqual(
                    proc.stdout.strip(), "",
                    f"quoted/heredoc text must abstain (F2): {cmd!r} -> {proc.stdout!r}",
                )
                self.assertEqual(read_ledger(ledger_dir), [])

    def test_real_command_substitution_still_denies(self):
        cmd = "x=$(git stash)"
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
            self.assertTrue(proc.stdout.strip(), f"{cmd!r} must still deny (F2)")
            hso = json.loads(proc.stdout)["hookSpecificOutput"]
            self.assertEqual(hso.get("permissionDecision"), "deny", cmd)

    def test_real_newline_inside_quoted_literal_stays_inert(self):
        """Review R4 regression from G2 (review_b round 1): G2 stopped
        blanking _SEPARATOR_CHARS content inside a quoted literal so the
        shell's actual argument text (e.g. a quoted leading `+` refspec)
        stayed visible to rules -- but `_mask_literal_span` also excluded
        the REAL newline inside that quoted span from blanking, so a
        multi-line quoted string's second line stayed at a fresh line-start
        and `_CMD_PREFIX`'s `\\n\\s*` alternative anchored it as if it were
        a brand new command. A multi-line `-m` message that merely
        mentions `git stash`, or a multi-line quoted `echo` argument that
        mentions `git push --force`, must both stay inert -- same as their
        single-line equivalents already do."""
        cases = (
            ("git commit -m 'fix\n\ngit stash was wrong'", DEFAULT_CWD),
            ("echo 'x\ngit push --force'", DEFAULT_CWD),
            ('git commit -m "fix\n\ngit stash was wrong"', DEFAULT_CWD),
            ('echo "x\ngit push --force"', DEFAULT_CWD),
        )
        for cmd, cwd in cases:
            with self.subTest(cmd=cmd), tempfile.TemporaryDirectory() as ledger_dir:
                proc = run_router_payload(make_payload("Bash", {"command": cmd}, cwd=cwd), ledger_dir)
                self.assertEqual(
                    proc.stdout.strip(), "",
                    f"multi-line quoted literal must abstain: {cmd!r} -> {proc.stdout!r}",
                )
                self.assertEqual(read_ledger(ledger_dir), [])


class TestHeredocBoundToItsDelimiter(RouterTestCase):
    """F13: heredoc detection must stop at the ACTUAL terminator instead of
    scanning `[\\s\\S]*` into whatever trailing commands follow it."""

    def test_backtick_after_heredoc_terminator_abstains(self):
        cmd = "cat <<EOF\nhello\nEOF\nprintf '%s\\n' '`literal`'"
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
            self.assertEqual(
                proc.stdout.strip(), "",
                "the backtick text is after the heredoc terminator and is safely "
                f"single-quoted, not part of the heredoc body (F13): {proc.stdout!r}",
            )
            self.assertEqual(read_ledger(ledger_dir), [])

    def test_unquoted_backtick_after_real_terminator_abstains(self):
        """F8 (round-3 redo): a regression from F13's own fix (1d4ae36) — the
        `backticks-in-unquoted-heredoc` rule's lazy pre-backtick scan must
        not be able to cross a REAL terminator line via a bare `|\\Z`
        fallback. An UNQUOTED backtick that appears in real code AFTER the
        heredoc has already closed must abstain, exactly like F13's original
        quoted case above, not fire `prior`."""
        cases = (
            "cat <<EOF\nhello\nEOF\necho `date`\n",
            "cat <<EOF\nhello\nEOF\n\nls\nX=`pwd`\n",
        )
        for cmd in cases:
            with self.subTest(cmd=cmd), tempfile.TemporaryDirectory() as ledger_dir:
                proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
                self.assertEqual(
                    proc.stdout.strip(), "",
                    "the backtick is real code after the heredoc's actual terminator, "
                    f"not part of the (already-closed) heredoc body (F8): {proc.stdout!r}",
                )
                self.assertEqual(read_ledger(ledger_dir), [])

    def test_tab_indented_dash_terminator_still_fires(self):
        """F2 (round-2 redo): `<<-` allows the terminator line to be
        tab-indented — the canonical use of `<<-`. The rule must not
        require an EXACT (unindented) terminator line to match."""
        cmd = "cat <<-EOF\n\t`cmd`\n\tEOF\n"
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
            self.assertIn(
                "additionalContext", json.loads(proc.stdout)["hookSpecificOutput"],
                f"tab-indented <<- terminator must still be recognized: {proc.stdout!r}",
            )
            self.assertEqual(read_ledger(ledger_dir)[0]["rule_id"], "backticks-in-unquoted-heredoc")

    def test_missing_terminator_still_fires(self):
        """F2 (round-2 redo): a heredoc with no terminator at all runs to
        end of script — the body (and its backtick) must still be caught,
        not silently ignored because there is no closing delimiter line."""
        cmd = "cat <<EOF\n`cmd`\n"
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
            self.assertIn(
                "additionalContext", json.loads(proc.stdout)["hookSpecificOutput"],
                f"missing-terminator heredoc body must still be scanned: {proc.stdout!r}",
            )
            self.assertEqual(read_ledger(ledger_dir)[0]["rule_id"], "backticks-in-unquoted-heredoc")

    def test_sequential_heredocs_each_bounded_to_own_delimiter(self):
        """F13 (round-2 redo): two sequential single-quoted heredocs must
        each stay bounded to their own delimiter — neither body's `git
        stash` line is executable, so the command abstains."""
        cmd = "cat <<'A'\ngit stash\nA\ncat <<'B'\ngit stash\nB\n"
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
            self.assertEqual(
                proc.stdout.strip(), "",
                f"both heredoc bodies are quoted and non-executable: {proc.stdout!r}",
            )
            self.assertEqual(read_ledger(ledger_dir), [])

    def test_same_line_multiple_heredocs_each_bounded_to_own_delimiter(self):
        """F13 (round-2 redo): `<<'A' <<'B'` on one line attaches the first
        body to A and the second to B — both are quoted/non-executable."""
        cmd = "cat <<'A' <<'B'\ngit stash\nA\ngit stash\nB\n"
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
            self.assertEqual(
                proc.stdout.strip(), "",
                f"both heredoc bodies (same-line delimiters) are non-executable: {proc.stdout!r}",
            )
            self.assertEqual(read_ledger(ledger_dir), [])

    def test_real_command_after_heredoc_terminator_still_denies(self):
        """F13 (round-2 redo): a real command placed AFTER the heredoc's
        own terminator is not part of the (quoted, non-executable) body
        and must still be evaluated normally."""
        cmd = "cat <<'A'\nhello\nA\ngit stash\n"
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
            out = json.loads(proc.stdout)
            self.assertEqual(
                out["hookSpecificOutput"].get("permissionDecision"), "deny",
                f"the trailing `git stash` is a real command outside the heredoc body: {proc.stdout!r}",
            )


class TestNoQuadraticRescanOnLargeAllExemptInput(RouterTestCase):
    """F14: `_window_bounds` must not rebuild the full separator list for
    every candidate occurrence — a large all-exempt command must still
    finish inside the existing 5s hang ceiling shared with
    TestLatencySanityCeiling; no tighter wall-clock number is asserted
    here (that would reintroduce F18)."""

    def test_thousands_of_exempt_stash_list_invocations_stays_under_hang_ceiling(self):
        # Empirically: 5000 repeats already exceeds a 5s timeout against the
        # current (quadratic) implementation.
        cmd = "git stash list; " * 5000
        with tempfile.TemporaryDirectory() as ledger_dir:
            try:
                proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir, timeout=5)
            except subprocess.TimeoutExpired:
                self.fail(
                    "router exceeded the 5s hang ceiling on a large all-exempt "
                    "input — quadratic rescanning in _window_bounds (F14)"
                )
            self.assertEqual(proc.returncode, 0)
            self.assertEqual(proc.stdout.strip(), "")

    def test_thousands_of_exempt_stash_list_invocations_with_leading_cd_stays_under_hang_ceiling(self):
        """F14 (review round redo): `_window_bounds` was fixed, but
        `_effective_checkout` -- called once per candidate occurrence for
        any rule with `unless_cwd` (git-stash-shared-checkout) -- still
        rescans `scan_text[:match_start]` with `_CD_LOCATE_RE` from
        scratch on EVERY candidate, and once a `cd` is found,
        `_cd_reaches` slices `paren_depths[token_end:target_pos+1]` and
        calls `min()` over it, again per candidate. A single leading `cd`
        followed by thousands of exempt `git stash list` invocations (the
        cwd is a `/worktrees/` checkout, matching the real F4 scenario)
        makes both of those per-candidate scans quadratic in the number of
        invocations. Empirically: 6000 repeats already exceeds a 5s
        timeout against the current implementation. No tighter wall-clock
        number is asserted here (that would reintroduce F18)."""
        cmd = "cd /shared && " + "git stash list; " * 6000
        with tempfile.TemporaryDirectory() as ledger_dir:
            try:
                proc = run_router_payload(
                    make_payload("Bash", {"command": cmd}, cwd="/worktrees/test-repo"),
                    ledger_dir,
                    timeout=5,
                )
            except subprocess.TimeoutExpired:
                self.fail(
                    "router exceeded the 5s hang ceiling on a large all-exempt "
                    "input with a leading `cd` — quadratic rescanning in "
                    "_effective_checkout/_cd_reaches (F14)"
                )
            self.assertEqual(proc.returncode, 0)
            self.assertEqual(proc.stdout.strip(), "")

    def test_thousands_of_leading_cds_stays_under_hang_ceiling(self):
        """F14 (redo 2): the previous fix (19821fc) precomputes each `cd`'s
        own reach data once, but `_precompute_cd_reach_info` still walks
        forward from `token_end` to the end of `paren_depths` looking for
        where the enclosing depth first drops, for EVERY `cd` it finds. At
        depth 0 with no real parens anywhere, that walk never terminates
        early, so thousands of `cd`s (not just thousands of candidates
        after one `cd`) are themselves quadratic. Empirically: 12000
        repeats already exceeds a 5s timeout against the 19821fc
        implementation. No tighter wall-clock number is asserted here
        (that would reintroduce F18)."""
        cmd = "cd /shared; " * 12000 + "true"
        with tempfile.TemporaryDirectory() as ledger_dir:
            try:
                proc = run_router_payload(
                    make_payload("Bash", {"command": cmd}, cwd="/worktrees/test-repo"),
                    ledger_dir,
                    timeout=5,
                )
            except subprocess.TimeoutExpired:
                self.fail(
                    "router exceeded the 5s hang ceiling on thousands of leading "
                    "`cd`s — quadratic per-cd forward scan in "
                    "_precompute_cd_reach_info (F14 redo 2)"
                )
            self.assertEqual(proc.returncode, 0)
            self.assertEqual(proc.stdout.strip(), "")

    def test_thousands_of_cds_with_command_substitution_argument_stays_under_hang_ceiling(self):
        """Review_b G7 (round 5): `_read_token`'s unquoted-word scanner stops
        at the FIRST unescaped `)` it sees, so a `cd $(pwd)` argument's own
        token boundary lands one character INSIDE the substitution (at its
        closing paren) instead of past it. `_precompute_cd_reach_info` then
        sees `paren_depths[token_end] != enclosing` for every such `cd`
        (the ')' hasn't been processed yet at that index) and falls back to
        the linear `break_pos` walk for EVERY one of them -- and since the
        net depth after a balanced `$(...)` never actually drops below
        `enclosing` anywhere later in the text, that walk runs all the way
        to the end of `paren_depths` each time, making thousands of `cd
        $(...)` invocations quadratic again despite the F14 redo-2 fix.
        Empirically: 12000 repeats already exceeds a 5s timeout at HEAD (a
        single leading `cd $(pwd)` is not even required -- every repeat
        re-triggers the bug on its own). No tighter wall-clock number is
        asserted here (that would reintroduce F18)."""
        cmd = "cd $(pwd); " * 12000 + "true"
        with tempfile.TemporaryDirectory() as ledger_dir:
            try:
                proc = run_router_payload(
                    make_payload("Bash", {"command": cmd}, cwd="/worktrees/test-repo"),
                    ledger_dir,
                    timeout=5,
                )
            except subprocess.TimeoutExpired:
                self.fail(
                    "router exceeded the 5s hang ceiling on thousands of `cd "
                    "$(...)` invocations -- _read_token's unquoted-word scan "
                    "stops mid-substitution at the first ')', forcing the "
                    "_precompute_cd_reach_info linear fallback walk (G7)"
                )
            self.assertEqual(proc.returncode, 0)
            self.assertEqual(proc.stdout.strip(), "")

    def test_thousands_of_comment_obscured_parens_in_substitution_stays_under_hang_ceiling(self):
        """Review_b G8 (round 6): `_find_matching_paren` (the scanner G7
        made `_read_token` rely on for every `cd $(...)` occurrence) has no
        `#`-comment awareness of its own. A `(` that only ever appears
        inside a real shell comment -- entirely valid, executable shell,
        since a real shell comments to end-of-line the same way inside a
        `$(...)` body as at top level -- was counted as a real unmatched
        paren. That kept this scanner's LOCAL depth above zero for the rest
        of the substitution's own real close, forcing it to keep scanning
        character-by-character all the way to the end of the text looking
        for one more `)`. `_precompute_cd_reach_info` calls this once per
        `cd $(...)` occurrence, so that full-remaining-text scan repeated
        for every occurrence made thousands of them quadratic again despite
        the G7 fix. Empirically: 6000 repeats already exceeds a 5s timeout
        at HEAD before the G8 fix. No tighter wall-clock number is asserted
        here (that would reintroduce F18)."""
        cmd = "cd $(pwd # comment (unbalanced\n); " * 6000 + "true"
        with tempfile.TemporaryDirectory() as ledger_dir:
            try:
                proc = run_router_payload(
                    make_payload("Bash", {"command": cmd}, cwd="/worktrees/test-repo"),
                    ledger_dir,
                    timeout=5,
                )
            except subprocess.TimeoutExpired:
                self.fail(
                    "router exceeded the 5s hang ceiling on thousands of "
                    "comment-obscured-paren `cd $(...)` invocations -- "
                    "_find_matching_paren has no #-comment awareness (G8)"
                )
            self.assertEqual(proc.returncode, 0)
            self.assertEqual(proc.stdout.strip(), "")

    def test_thousands_of_parameter_length_expansions_in_substitution_stays_under_hang_ceiling(self):
        """Review round 9, F11: the comment predicate shared by
        `executable_mask` and `_find_matching_paren` treats a `#` preceded by
        `{` as a comment start. Bash's parameter-LENGTH expansion opens with
        a dollar sign, then an open brace, then a hash mark, then a name,
        then a close brace (giving the length of the named variable) -- and
        that hash mark sits directly after the open brace, so the predicate
        misreads it as a comment opener. Inside a `cd $(...)` argument this
        means `_find_matching_paren` thinks everything from that hash mark
        onward (including the substitution's own real closing paren) is
        commented out, so it never finds a local close and keeps scanning
        character-by-character to the end of the text -- and since
        `_read_token`/`_precompute_cd_reach_info` calls this once per such
        `cd`, thousands of them go quadratic again despite the F14/G7/G8
        fixes. No tighter wall-clock number is asserted here (F18)."""
        cmd = "cd $(echo ${#HOME}); " * 6000 + "true"
        with tempfile.TemporaryDirectory() as ledger_dir:
            try:
                proc = run_router_payload(
                    make_payload("Bash", {"command": cmd}, cwd="/worktrees/test-repo"),
                    ledger_dir,
                    timeout=5,
                )
            except subprocess.TimeoutExpired:
                self.fail(
                    "router exceeded the 5s hang ceiling on thousands of "
                    "parameter-length-expansion `cd $(...)` invocations -- "
                    "the shared comment predicate misreads a hash mark right "
                    "after an open brace as a comment start (F11)"
                )
            self.assertEqual(proc.returncode, 0)
            self.assertEqual(proc.stdout.strip(), "")


class TestPipeTailScopedToTestCommandPipeline(RouterTestCase):
    """F20: the masked-test-exit prior must only fire when the
    `| tail/grep/head` pipeline belongs to the test command's OWN pipeline,
    not an unrelated command chained after it with `;` or `&&`."""

    def test_unrelated_piped_command_after_test_command_abstains(self):
        cases = (
            "pytest -q; printf 'finished\\n' | tail -5",
            "pytest -q && printf 'finished\\n' | tail -5",
        )
        for cmd in cases:
            with self.subTest(cmd=cmd), tempfile.TemporaryDirectory() as ledger_dir:
                proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
                self.assertEqual(
                    proc.stdout.strip(), "",
                    f"the tail pipeline belongs to printf, not the test command (F20): {cmd!r} -> {proc.stdout!r}",
                )
                self.assertEqual(read_ledger(ledger_dir), [])


class TestCommandSubstitutionParenMatchingIsQuoteAware(RouterTestCase):
    """Review G1 (review_b round 1): `_find_matching_paren`'s flat depth
    counter treated ANY `)` character as closing a `$(...)` span, including
    one that only appears inside a quoted literal INSIDE the substitution.
    A real shell tracks nested quoting when it looks for the substitution's
    true closing paren, so a `)` inside `'...'`/`"..."` never ends it early."""

    def test_quoted_paren_inside_substitution_does_not_end_it_early(self):
        """The quoted `)` at the start of `$(echo ")")` is NOT the real
        closing paren; a real shell keeps the substitution open through the
        actual final `)`. Everything after that real close is still
        literal text inside the OUTER double quotes, so `git stash` here
        never executes and the command must abstain. The buggy flat
        counter mistook the quoted `)` for the real close, which spilled
        `git stash` out into unquoted (executable) territory -> false deny."""
        cmd = 'echo "$(echo ")") ; git stash"'
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
            self.assertEqual(
                proc.stdout.strip(), "",
                f"git stash here is literal text inside the still-open outer quotes (G1): {proc.stdout!r}",
            )
            self.assertEqual(read_ledger(ledger_dir), [])

    def test_quoted_paren_inside_substitution_does_not_hide_a_real_stash(self):
        """`$(echo ')' ; git stash)` is ONE substitution (a real shell
        tracks the quoted `)` as part of the single-quoted literal, not a
        closer) that genuinely runs `git stash` as its second command. The
        buggy flat counter closed the substitution early at the quoted `)`,
        which caused the real `git stash` text to be masked as ordinary
        double-quoted literal content -> stash bypass (abstain instead of
        deny)."""
        cmd = "echo \"$(echo ')' ; git stash)\""
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
            self.assertTrue(proc.stdout.strip(), f"{cmd!r} must still deny (G1): a real substitution runs git stash")
            hso = json.loads(proc.stdout)["hookSpecificOutput"]
            self.assertEqual(hso.get("permissionDecision"), "deny", cmd)


class TestPipefailExemptionRespectsOrdering(RouterTestCase):
    """Review G2 (review_b round 1): the shell-wide pipefail exemption for
    `pipe-tail-masks-exit` checked `unless_match` anywhere in the whole
    text, ignoring order. `set -o pipefail` only protects pipelines that
    run AFTER it -- a pipefail enabled AFTER an already-unsafe pipeline
    must never retroactively suppress that pipeline's prior. (Reading
    PIPESTATUS is a different, position-agnostic idiom and must keep
    working regardless of order -- TestPipeTailAbstainsWhenStatusCaptured.)
    """

    def test_pipefail_set_after_the_risky_pipeline_does_not_suppress_it(self):
        cmd = "pytest -q | tail -5; set -o pipefail"
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
            self.assertIn(
                "additionalContext", json.loads(proc.stdout)["hookSpecificOutput"],
                f"pipefail enabled AFTER the pipe must not suppress its prior (G2): {proc.stdout!r}",
            )
            self.assertEqual(read_ledger(ledger_dir)[0]["rule_id"], "pipe-tail-masks-exit")

    def test_pipefail_set_before_still_abstains(self):
        """Regression guard: the existing forward case (F10) must not break."""
        cmd = "set -o pipefail; cargo test --locked | tail -20"
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
            self.assertEqual(proc.stdout.strip(), "", cmd)
            self.assertEqual(read_ledger(ledger_dir), [])


class TestInvocationExemptionAnchoredNotSearched(RouterTestCase):
    """Review G3 (review_b round 1): the invocation-local stash exemption
    searched the WHOLE "same command" window for `unless_match`, so an
    arbitrary UNQUOTED argument elsewhere in the invocation (e.g. an
    unquoted `-m` message) could contain safe-looking text and wrongly
    exempt a genuinely dangerous `git stash push`. The exemption must be
    checked CONTIGUOUSLY from the match's own subcommand position, never
    searched across unrelated trailing arguments."""

    def test_unquoted_trailing_argument_does_not_shield_a_dangerous_push(self):
        cmd = "git stash push -m stash pop"
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
            self.assertTrue(proc.stdout.strip(), f"{cmd!r} must not abstain (G3)")
            hso = json.loads(proc.stdout)["hookSpecificOutput"]
            self.assertEqual(hso.get("permissionDecision"), "deny", cmd)


class TestSubstitutionMaskingIsRecursive(RouterTestCase):
    """Review G1 (review_b round 2): `_mask_double_quoted` skips over a
    `$(...)`/backtick substitution's ENTIRE body via `_find_matching_paren`
    without recursively masking quoted literals NESTED inside it. A
    single-quoted STRING ARGUMENT inside the substitution that merely
    CONTAINS `; git stash` text -- never executed as a command -- was
    therefore left fully visible to the stash rule, firing a false deny.
    The fix must recursively scan executable regions inside `$(...)`/
    backtick spans (nested quoting stays masked, nested `$(...)`/backticks
    stay executable) instead of leaving the whole span untouched."""

    def test_quoted_literal_inside_substitution_does_not_false_deny(self):
        """`printf '%s' '; git stash'` never runs `git stash` -- the text is
        a single-quoted string argument to printf. The buggy flat skip over
        the substitution left that quoted literal fully visible to the
        stash rule, firing a false deny."""
        cmd = "echo \"$(printf '%s' '; git stash')\""
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
            self.assertEqual(
                proc.stdout.strip(), "",
                f"quoted literal text inside the substitution is never executed (G1): {proc.stdout!r}",
            )
            self.assertEqual(read_ledger(ledger_dir), [])

    def test_real_stash_inside_substitution_still_denies(self):
        """Regression guard: an ACTUAL `git stash` invocation inside a
        `$(...)` substitution (not inside a nested quoted literal) must
        keep denying once the masking is made recursive."""
        cmd = 'echo "$(git stash)"'
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
            self.assertTrue(proc.stdout.strip(), f"{cmd!r} must still deny (G1 regression guard)")
            hso = json.loads(proc.stdout)["hookSpecificOutput"]
            self.assertEqual(hso.get("permissionDecision"), "deny", cmd)


class TestFindMatchingParenIsCommentAware(RouterTestCase):
    """Review_b G8 (round 6): `_find_matching_paren` had no `#`-comment
    awareness, so a quote or paren character that only ever appears inside
    a real shell comment inside a `$(...)` body was treated as real syntax
    -- see `TestNoQuadraticRescanOnLargeAllExemptInput` for the quadratic
    half of this finding; this class pins the correctness half."""

    def test_quote_in_comment_inside_substitution_does_not_extend_cd_reach(self):
        """`(cd $(pwd # it's fine\\n) ); git stash` is valid shell: the `cd`
        runs inside a subshell that closes (the outer `)`) before `git
        stash` runs, so it can never affect the outer shell's cwd, which
        stays the payload's own `/worktrees/` cwd -- exempt, must abstain.
        The buggy scanner treated the comment's apostrophe as opening a
        real quoted span, silently pairing it with an unrelated apostrophe
        it doesn't have here at all (so it consumed to end-of-text
        instead), which threw off the reach data enough to make this `cd`
        look like it still reached the later `git stash` -- an incorrect
        deny on an exempt worktree cwd."""
        cmd = "(cd $(pwd # it's fine\n) ); git stash"
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(
                make_payload("Bash", {"command": cmd}, cwd="/worktrees/test-repo"), ledger_dir
            )
            self.assertEqual(
                proc.stdout.strip(), "",
                f"the cd is subshell-scoped and the cwd is a worktree -- must abstain (G8): {proc.stdout!r}",
            )
            self.assertEqual(read_ledger(ledger_dir), [])

    def test_real_shared_checkout_cd_still_denies_alongside_a_commented_substitution(self):
        """Regression guard: a genuine `cd` into a shared checkout earlier
        in the same command must still deny, even with an unrelated
        comment-bearing `$(...)` substitution elsewhere in the text."""
        cmd = "cd $(pwd # it's fine\n); cd /shared/checkout; git stash"
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(
                make_payload("Bash", {"command": cmd}, cwd="/worktrees/test-repo"), ledger_dir
            )
            self.assertTrue(proc.stdout.strip(), f"{cmd!r} must still deny (G8 regression guard)")
            hso = json.loads(proc.stdout)["hookSpecificOutput"]
            self.assertEqual(hso.get("permissionDecision"), "deny", cmd)


class TestCommentPredicateExcludesParameterLengthExpansion(RouterTestCase):
    """Review round 9, F11: the shared comment predicate (used by both
    `executable_mask` and `_find_matching_paren`) treats a `#` preceded by
    `{` as a comment start. Bash's parameter-LENGTH expansion (dollar sign,
    open brace, hash mark, name, close brace -- the length of the named
    variable) puts a hash mark directly after an open brace with no real
    comment involved, so the predicate wrongly blanks the rest of the line
    -- including a real `git stash` that follows on the same line -- as if
    it were commented out. A real shell comment still needs the hash mark
    preceded by whitespace/start-of-line/a real separator; `{` immediately
    before `#` is never that on its own (a bare `{` only reserves as the
    command-grouping keyword when followed by whitespace, and here it is
    immediately followed by `#`)."""

    def test_parameter_length_expansion_does_not_mask_a_later_stash(self):
        cmd = "echo ${#HOME}; git stash"
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(
                make_payload("Bash", {"command": cmd}, cwd="/shared/checkout"), ledger_dir
            )
            self.assertTrue(
                proc.stdout.strip(),
                f"{cmd!r} must deny (F11): the hash mark is a length "
                f"expansion, not a comment opener, so the trailing "
                f"`git stash` is real executable text",
            )
            hso = json.loads(proc.stdout)["hookSpecificOutput"]
            self.assertEqual(hso.get("permissionDecision"), "deny", cmd)

    def test_positional_parameter_count_expansion_does_not_mask_a_later_stash(self):
        cmd = "[ ${#@} -gt 0 ] && git stash"
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(
                make_payload("Bash", {"command": cmd}, cwd="/shared/checkout"), ledger_dir
            )
            self.assertTrue(proc.stdout.strip(), f"{cmd!r} must deny (F11)")
            hso = json.loads(proc.stdout)["hookSpecificOutput"]
            self.assertEqual(hso.get("permissionDecision"), "deny", cmd)

    def test_plain_variable_reference_control_still_denies(self):
        """Regression guard: the unaffected control case from the ledger."""
        cmd = "echo $HOME; git stash"
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(
                make_payload("Bash", {"command": cmd}, cwd="/shared/checkout"), ledger_dir
            )
            self.assertTrue(proc.stdout.strip(), f"{cmd!r} must deny")
            hso = json.loads(proc.stdout)["hookSpecificOutput"]
            self.assertEqual(hso.get("permissionDecision"), "deny", cmd)


class TestSubstitutionScannerStateIsConsistent(RouterTestCase):
    """G3 (spec-level review, reopen generation 2, contract T8706x0j0):
    `_mask_quotes_recursive` -- the scanner used for text found INSIDE a
    `$(...)`/backtick substitution that is itself nested inside a
    double-quoted span -- tracks single/double quotes and further nested
    substitutions, but has no `#`-comment awareness (unlike the top-level
    `executable_mask` loop and `_find_matching_paren`, both of which share
    `_is_comment_start`) and no heredoc awareness at all. That inconsistency
    cuts both ways: text that should stay inert (a comment, a QUOTED
    heredoc body) leaks through as visible/executable and trips a false
    deny, while a genuinely executable multi-line backtick substitution
    inside a heredoc body gets its second line masked away by
    `_consume_heredoc_body`'s per-line, non-continuing treatment of an
    still-open backtick span, hiding a real invocation."""

    def test_comment_inside_double_quoted_substitution_abstains(self):
        """A `#` inside `$(...)` nested in double quotes opens a real shell
        comment that runs to the end of the line -- the `; git stash` text
        after it is never executed. `_mask_quotes_recursive` has no
        `#`-comment case, so that text (including the real `;` separator
        character) stays fully visible, and the router mistakes it for a
        genuinely anchored `git stash` invocation."""
        cmd = 'x="$(true # ; git stash\n)"'
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
            self.assertEqual(
                proc.stdout.strip(), "",
                f"commented-out text inside a dq substitution must abstain (G3): {proc.stdout!r}",
            )
            self.assertEqual(read_ledger(ledger_dir), [])

    def test_quoted_heredoc_inside_double_quoted_substitution_abstains(self):
        """A heredoc with a QUOTED delimiter (`<<'H'`) makes its whole body
        inert, same as any other single-quoted literal -- but
        `_mask_quotes_recursive` has no heredoc case at all, so the body
        text `git stash` is left fully visible/executable when the heredoc
        sits inside a `$(...)` nested in double quotes."""
        cmd = "x=\"$(cat <<'H'\ngit stash\nH\n)\""
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
            self.assertEqual(
                proc.stdout.strip(), "",
                f"a quoted heredoc body inside a dq substitution must abstain (G3): {proc.stdout!r}",
            )
            self.assertEqual(read_ledger(ledger_dir), [])

    def test_multiline_backtick_substitution_in_heredoc_body_still_denies(self):
        """A real shell treats a backtick command substitution as spanning
        multiple lines -- the embedded real newline is just whitespace
        inside the substitution, so `` `git\\nstash` `` genuinely executes
        `git stash`. `_consume_heredoc_body` masks an unquoted heredoc body
        one LINE at a time via `_mask_span_preserving_substitutions`, which
        has no memory of an still-open backtick span carried over from the
        previous line -- so the second line's `stash` text (no visible
        opening backtick on ITS line) gets masked away as ordinary literal
        text, and the stash-specific deny rule never sees a complete `git
        stash` to match against."""
        cmd = "x=$(cat <<H\n`git\nstash`\nH\n)"
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
            self.assertTrue(
                proc.stdout.strip(),
                f"{cmd!r} must not abstain (G3): the backtick substitution "
                f"genuinely executes `git stash` across the line break",
            )
            hso = json.loads(proc.stdout)["hookSpecificOutput"]
            self.assertEqual(hso.get("permissionDecision"), "deny", cmd)

    def test_escaped_single_quote_prefix_before_stash_still_denies(self):
        """`\\'` outside any quoting is bash for a LITERAL apostrophe
        character, not the start of a single-quoted span -- the shell still
        runs the `;`-separated `git stash` that follows as a normal second
        command. The top-level `executable_mask` loop has no backslash-
        escape awareness before deciding a bare `'` opens a quoted span, so
        it treats this `'` as a real (unterminated) single-quoted literal
        and masks the real `;` separator that follows to a space --
        breaking `_CMD_PREFIX`'s anchor requirement for the genuine `git
        stash` invocation and hiding it."""
        cmd = "echo \\'; git stash"
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
            self.assertTrue(
                proc.stdout.strip(),
                f"{cmd!r} must not abstain (G3): the escaped quote is a "
                f"literal apostrophe, not a real quote opener",
            )
            hso = json.loads(proc.stdout)["hookSpecificOutput"]
            self.assertEqual(hso.get("permissionDecision"), "deny", cmd)


class TestStashExemptionEffectiveCheckout(RouterTestCase):
    """F4 (arrra/hex PR #5 round 1): `unless_cwd` on git-stash-shared-checkout
    must track the EFFECTIVE checkout of each invocation, not just the hook's
    payload cwd. Running from a /worktrees/ cwd must not exempt a stash that
    actually targets a different (shared) checkout via `-C` or a preceding
    `cd`; conversely a stash that genuinely resolves to a /worktrees/
    checkout must still abstain even when the hook's own cwd is not itself a
    worktree. A cwd substring alone must never exempt another target."""

    WORKTREE_CWD = "/tmp/hex-home/.boi/v2/worktrees/Scnfz8k1f/T7w2t3bzf"

    def test_dash_c_to_shared_checkout_denies_even_from_worktree_cwd(self):
        cmd = "git -C /shared/checkout stash"
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(
                make_payload("Bash", {"command": cmd}, cwd=self.WORKTREE_CWD), ledger_dir
            )
            self.assertTrue(proc.stdout.strip(), f"{cmd!r} must not abstain (F4)")
            hso = json.loads(proc.stdout)["hookSpecificOutput"]
            self.assertEqual(hso.get("permissionDecision"), "deny", cmd)

    def test_cd_to_shared_checkout_denies_even_from_worktree_cwd(self):
        cmd = "cd /shared/checkout && git stash"
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(
                make_payload("Bash", {"command": cmd}, cwd=self.WORKTREE_CWD), ledger_dir
            )
            self.assertTrue(proc.stdout.strip(), f"{cmd!r} must not abstain (F4)")
            hso = json.loads(proc.stdout)["hookSpecificOutput"]
            self.assertEqual(hso.get("permissionDecision"), "deny", cmd)

    def test_genuine_worktree_local_checkout_still_abstains(self):
        cmd = f"git -C {self.WORKTREE_CWD} stash"
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(
                make_payload("Bash", {"command": cmd}, cwd=DEFAULT_CWD), ledger_dir
            )
            self.assertEqual(
                proc.stdout.strip(), "",
                f"genuine worktree-local stash must abstain (F4): {proc.stdout!r}",
            )
            self.assertEqual(read_ledger(ledger_dir), [])

    def test_unresolvable_target_keeps_protection(self):
        cmd = "git -C $SHARED_CHECKOUT stash"
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(
                make_payload("Bash", {"command": cmd}, cwd=self.WORKTREE_CWD), ledger_dir
            )
            self.assertTrue(proc.stdout.strip(), f"{cmd!r} must not abstain (F4, unresolved target)")
            hso = json.loads(proc.stdout)["hookSpecificOutput"]
            self.assertEqual(hso.get("permissionDecision"), "deny", cmd)

    def test_semicolon_separated_cd_chain_to_shared_checkout_denies(self):
        """F1 (review round 1 redo): _CD_RE captured `(\\S+)` which swallows a
        trailing `;`, so `cd /worktrees/x; cd /shared/checkout && git stash`
        had its second `cd` invisible (no leading separator left to match)
        and the stale first `cd` exempted the stash. The capture must stop
        at `;`/`&`/`|`/`)` so every `cd` in the chain is seen."""
        cmd = "cd /worktrees/x; cd /shared/checkout && git stash"
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(
                make_payload("Bash", {"command": cmd}, cwd=self.WORKTREE_CWD), ledger_dir
            )
            self.assertTrue(proc.stdout.strip(), f"{cmd!r} must not abstain (F1)")
            hso = json.loads(proc.stdout)["hookSpecificOutput"]
            self.assertEqual(hso.get("permissionDecision"), "deny", cmd)

    def test_semicolon_separated_cd_chain_to_worktree_still_abstains(self):
        """Same capture fix as above, but the near miss: the chain's last
        `cd` genuinely resolves to a /worktrees/ checkout, so it must keep
        abstaining even though the fix changes what the regex captures."""
        cmd = f"cd /worktrees/x; cd {self.WORKTREE_CWD} && git stash"
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(
                make_payload("Bash", {"command": cmd}, cwd=DEFAULT_CWD), ledger_dir
            )
            self.assertEqual(
                proc.stdout.strip(), "",
                f"genuine worktree-local chained cd must abstain: {proc.stdout!r}",
            )
            self.assertEqual(read_ledger(ledger_dir), [])

    def test_quoted_cd_to_shared_checkout_denies_even_from_worktree_cwd(self):
        """F8 (review round 2 redo): `executable_mask` blanks quoted
        argument content to spaces BEFORE `_effective_checkout` ever sees
        it, so a quoted `cd` target found no resolvable path, fell back to
        the hook's own /worktrees/ payload cwd, and wrongly exempted. A
        single- or double-quoted literal must resolve to its real
        (masked-away) content and still deny.

        NOTE: `git -C "<quoted>" stash` is a SEPARATE, pre-existing gap, not
        part of this regression -- verified by running this exact command
        against commit ac89538 (the commit the review cites as "denied
        before"): it abstains there too, because `_GIT_GLOBAL_OPTS`'s
        `-C\\s+\\S+` can never match through a masked (spaced-out) quoted
        argument in the rule's own top-level `match` regex, independent of
        `_effective_checkout`. Fixing that needs a new mechanism (a
        quote-preserving splice for @GITOPTS@ matching) beyond this
        finding's scope -- see the adjudication."""
        cases = (
            "cd '/shared/checkout' && git stash",
            'cd "/shared/checkout" && git stash',
        )
        for cmd in cases:
            with self.subTest(cmd=cmd), tempfile.TemporaryDirectory() as ledger_dir:
                proc = run_router_payload(
                    make_payload("Bash", {"command": cmd}, cwd=self.WORKTREE_CWD), ledger_dir
                )
                self.assertTrue(proc.stdout.strip(), f"{cmd!r} must not abstain (F8)")
                hso = json.loads(proc.stdout)["hookSpecificOutput"]
                self.assertEqual(hso.get("permissionDecision"), "deny", cmd)

    def test_quoted_variable_cd_keeps_protection(self):
        """F8: a double-quoted variable still expands at runtime, so it
        can't be resolved to a literal path -- uncertain must keep
        protection (deny), the same as the unquoted `$VAR` case."""
        cases = ('cd "$DIR" && git stash',)
        for cmd in cases:
            with self.subTest(cmd=cmd), tempfile.TemporaryDirectory() as ledger_dir:
                proc = run_router_payload(
                    make_payload("Bash", {"command": cmd}, cwd=self.WORKTREE_CWD), ledger_dir
                )
                self.assertTrue(proc.stdout.strip(), f"{cmd!r} must not abstain (F8, unresolved target)")
                hso = json.loads(proc.stdout)["hookSpecificOutput"]
                self.assertEqual(hso.get("permissionDecision"), "deny", cmd)

    def test_quoted_worktree_local_near_miss_still_abstains(self):
        """F8 near miss: a quoted `cd` target that genuinely resolves to a
        /worktrees/ checkout must keep abstaining -- the fix must recover
        the real literal, not just always deny once quotes are involved."""
        cmd = "cd '/worktrees/x/sub' && git stash"
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(make_payload("Bash", {"command": cmd}, cwd=DEFAULT_CWD), ledger_dir)
            self.assertEqual(
                proc.stdout.strip(), "",
                f"genuine quoted worktree-local stash must abstain (F8): {proc.stdout!r}",
            )
            self.assertEqual(read_ledger(ledger_dir), [])

    def test_relative_cd_within_worktree_cwd_still_abstains(self):
        """F4 (review round 1 redo): a relative `cd sub` from a /worktrees/
        payload cwd is resolvable against that cwd (not uncertain) and stays
        inside the same worktree checkout, so it must abstain -- the prior
        fix over-denied this by treating the unresolved relative literal as
        a non-worktree target."""
        cmd = "cd sub && git stash"
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(
                make_payload("Bash", {"command": cmd}, cwd=self.WORKTREE_CWD), ledger_dir
            )
            self.assertEqual(
                proc.stdout.strip(), "",
                f"relative cd within a worktree checkout must abstain (F4): {proc.stdout!r}",
            )
            self.assertEqual(read_ledger(ledger_dir), [])

    def test_subshell_local_cd_does_not_leak_to_a_later_invocation(self):
        """G1 (review_b round 1): `_effective_checkout` picked up the LAST
        `cd` anywhere earlier in the text, even one scoped to a `(...)`
        subshell that already closed. A subshell-local `cd` must not
        exempt a stash that runs OUTSIDE that subshell, back in the real
        (shared) checkout."""
        cmd = "(cd /worktrees/x); git stash"
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
            self.assertTrue(proc.stdout.strip(), f"{cmd!r} must not abstain (G1)")
            hso = json.loads(proc.stdout)["hookSpecificOutput"]
            self.assertEqual(hso.get("permissionDecision"), "deny", cmd)

    def test_or_guarded_cd_does_not_leak_to_its_failure_branch(self):
        """G1: `cd X || match` only reaches `match` when the `cd` FAILED --
        meaning the directory never actually changed. The pre-`cd` cwd (the
        real, shared checkout) must still govern, not the attempted target."""
        cmd = "cd /worktrees/x || git stash"
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
            self.assertTrue(proc.stdout.strip(), f"{cmd!r} must not abstain (G1)")
            hso = json.loads(proc.stdout)["hookSpecificOutput"]
            self.assertEqual(hso.get("permissionDecision"), "deny", cmd)

    def test_genuine_worktree_local_subshell_still_abstains(self):
        """G1 near miss: when the `cd` AND the stash both run inside the
        SAME subshell, the subshell-local checkout genuinely governs the
        stash too -- must keep abstaining, not over-deny once subshell
        scoping is enforced."""
        cmd = "(cd /worktrees/x; git stash)"
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
            self.assertEqual(
                proc.stdout.strip(), "",
                f"cd and stash inside the same subshell must abstain (G1): {proc.stdout!r}",
            )
            self.assertEqual(read_ledger(ledger_dir), [])

    def test_double_quoted_newline_does_not_fake_a_cd(self):
        """G4 (review_b round 2): `_mask_double_quoted` excluded the REAL
        newline inside a double-quoted span from blanking (only
        `_mask_literal_span`, used for single quotes, was fixed for R4), so
        a double-quoted string's second line stayed at a fresh line-start
        and `_CMD_PREFIX`'s `\\n\\s*` alternative anchored `cd
        /worktrees/x` inside the quotes as a brand new command --
        `_effective_checkout` then picked up that FAKE `cd` and wrongly
        exempted a real `git stash` that runs from an actual shared
        checkout with no real `cd` at all."""
        cmd = 'echo "x\ncd /worktrees/x"; git stash'
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(
                make_payload("Bash", {"command": cmd}, cwd="/shared/checkout"), ledger_dir
            )
            self.assertTrue(proc.stdout.strip(), f"{cmd!r} must not abstain (G4)")
            hso = json.loads(proc.stdout)["hookSpecificOutput"]
            self.assertEqual(hso.get("permissionDecision"), "deny", cmd)

    def test_double_quoted_backslash_newline_does_not_fake_a_cd(self):
        """G4 (review_b round 2, re-opened): `_mask_double_quoted`'s
        backslash-escape branch special-cased `\\<newline>` (a real shell
        line continuation -- the shell deletes both characters and joins
        the two source lines into one logical line) by leaving BOTH the
        backslash and the newline unmasked, so the general `_SEPARATOR_CHARS`
        branch (which blanks a bare embedded newline, fixed for the first
        G4) never got a chance to run on this one. The unmasked real
        newline still puts a fresh line-start inside the quoted span, so
        `_CMD_PREFIX`'s `\\n\\s*` alternative anchors `cd /worktrees/x` as
        a brand new command and `_effective_checkout` wrongly treats it as
        a real `cd`, exempting a `git stash` that actually runs from a
        real shared checkout with no real `cd` at all. A harmless shell
        mock confirms the quoted `cd` line never actually executes -- the
        echo just prints `xcd /worktrees/x` as one line."""
        cmd = 'echo "x\\\ncd /worktrees/x"; git stash'
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(
                make_payload("Bash", {"command": cmd}, cwd="/shared/checkout"), ledger_dir
            )
            self.assertTrue(proc.stdout.strip(), f"{cmd!r} must not abstain (G4)")
            hso = json.loads(proc.stdout)["hookSpecificOutput"]
            self.assertEqual(hso.get("permissionDecision"), "deny", cmd)

    def test_dash_c_dot_after_cd_resolves_against_effective_checkout(self):
        """G2 (spec-level review, reopen generation 2): `_effective_checkout`
        resolves a `-C <path>` global option by calling
        `_resolve_against_cwd(value, payload_cwd)` -- always against the raw
        hook PAYLOAD cwd, never against whatever effective directory an
        earlier `cd` in the same command already established. From a
        /worktrees/ payload cwd, `cd /shared/checkout && git -C . stash`
        genuinely targets `/shared/checkout` (`.` resolves relative to the
        shell's CURRENT directory after the `cd`, not the hook's payload
        cwd) and must deny -- but the relative `.` gets joined against the
        payload cwd instead, resolving back to the exempt /worktrees/ path
        and wrongly abstaining."""
        cmd = "cd /shared/checkout && git -C . stash"
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(
                make_payload("Bash", {"command": cmd}, cwd="/worktrees/review"), ledger_dir
            )
            self.assertTrue(proc.stdout.strip(), f"{cmd!r} must not abstain (G2)")
            hso = json.loads(proc.stdout)["hookSpecificOutput"]
            self.assertEqual(hso.get("permissionDecision"), "deny", cmd)


class TestForceRefspecAsksFirst(RouterTestCase):
    """F5: a leading `+` on any push refspec forces the update, the same as
    `--force`/`-f`, but neither force-push rule matched it. Inspect refspec
    arguments for a leading `+` and ask, alongside non-forcing near misses."""

    def test_leading_plus_refspec_asks(self):
        cmd = "git push origin +HEAD:main"
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
            self.assertTrue(proc.stdout.strip(), f"{cmd!r} must not abstain (F5)")
            hso = json.loads(proc.stdout)["hookSpecificOutput"]
            self.assertEqual(hso.get("permissionDecision"), "ask", cmd)

    def test_non_forcing_refspec_near_misses_abstain(self):
        cases = (
            "git push origin HEAD:main",
            "git push origin main",
        )
        for cmd in cases:
            with self.subTest(cmd=cmd), tempfile.TemporaryDirectory() as ledger_dir:
                proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
                self.assertEqual(proc.stdout.strip(), "", f"{cmd!r} must abstain (F5 near miss)")
                self.assertEqual(read_ledger(ledger_dir), [])

    def test_quoted_leading_plus_refspec_still_asks(self):
        """G2 (review_b round 1): masking used to blank a quoted argument's
        content down to pure spaces, so a QUOTED leading `+` was invisible
        to the rule even though the shell passes that exact literal
        refspec to the push -- abstained when it should ask."""
        cmd = "git push origin '+HEAD:main'"
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
            self.assertTrue(proc.stdout.strip(), f"{cmd!r} must not abstain (G2)")
            hso = json.loads(proc.stdout)["hookSpecificOutput"]
            self.assertEqual(hso.get("permissionDecision"), "ask", cmd)

    def test_quoted_non_forcing_refspec_near_miss_abstains(self):
        """G2 near miss: a quoted but non-forcing refspec must keep
        abstaining -- unmasking the literal content must not make the
        destructive-arg scan over-eager."""
        cmd = "git push origin 'main'"
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
            self.assertEqual(proc.stdout.strip(), "", f"{cmd!r} must abstain (G2 near miss): {proc.stdout!r}")
            self.assertEqual(read_ledger(ledger_dir), [])


class TestDestructiveArgumentFormsAskFirst(RouterTestCase):
    """F6: the destructive-git rule only recognized specific argument
    spellings/positions. Equivalent forms with long options, reordered
    options, or checkout-with-tree must ask just the same; non-destructive
    near misses must keep abstaining."""

    def test_equivalent_destructive_forms_ask(self):
        cases = (
            "git clean --force -d",
            "git reset HEAD~1 --hard",
            "git checkout HEAD -- tracked-file",
        )
        for cmd in cases:
            with self.subTest(cmd=cmd), tempfile.TemporaryDirectory() as ledger_dir:
                proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
                self.assertTrue(proc.stdout.strip(), f"{cmd!r} must not abstain (F6)")
                hso = json.loads(proc.stdout)["hookSpecificOutput"]
                self.assertEqual(hso.get("permissionDecision"), "ask", cmd)

    def test_non_destructive_near_misses_abstain(self):
        cases = (
            "git clean -n",
            "git reset --soft",
            "git checkout -b newbranch",
        )
        for cmd in cases:
            with self.subTest(cmd=cmd), tempfile.TemporaryDirectory() as ledger_dir:
                proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
                self.assertEqual(proc.stdout.strip(), "", f"{cmd!r} must abstain (F6 near miss)")
                self.assertEqual(read_ledger(ledger_dir), [])

    def test_option_scan_does_not_cross_a_command_separator(self):
        """F2 (review round 1 redo): `(?:\\S+\\s+)*` treats `;`/`&&`/`|` as
        ordinary whitespace-separated tokens, so a non-destructive command
        followed by an unrelated command containing a destructive-looking
        flag (e.g. `rm -rf`) was scanned as one option run and asked. The
        option-skip must stop at `;`, `&`, and `|`."""
        cases = (
            "git clean -n; rm -rf build",
            "git checkout -b feat && printf -- '%s' x",
            "git reset --soft HEAD~1 && ls --hard",
        )
        for cmd in cases:
            with self.subTest(cmd=cmd), tempfile.TemporaryDirectory() as ledger_dir:
                proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
                self.assertEqual(proc.stdout.strip(), "", f"{cmd!r} must abstain (F2 near miss)")
                self.assertEqual(read_ledger(ledger_dir), [])

    def test_quoted_destructive_flags_still_ask(self):
        """G2 (review_b round 1): masking used to blank a quoted flag's
        content down to pure spaces, erasing it from the option scan -- a
        quoted `--hard`/`--force` abstained even though the shell still
        passes that exact literal flag to the command."""
        cases = (
            "git reset HEAD~1 '--hard'",
            "git clean '--force' -d",
        )
        for cmd in cases:
            with self.subTest(cmd=cmd), tempfile.TemporaryDirectory() as ledger_dir:
                proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
                self.assertTrue(proc.stdout.strip(), f"{cmd!r} must not abstain (G2)")
                hso = json.loads(proc.stdout)["hookSpecificOutput"]
                self.assertEqual(hso.get("permissionDecision"), "ask", cmd)

    def test_quoted_literal_text_near_miss_abstains(self):
        """G2 near miss: a destructive-looking flag spelled out as ordinary
        quoted TEXT to an unrelated subcommand (not `reset`/`clean`/
        `checkout`/`branch -D`) must keep abstaining -- unmasking literal
        argument content must not make the destructive-arg scan cross into
        an unrelated command's own arguments."""
        cmd = "git commit -m 'reset --hard would be bad here'"
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
            self.assertEqual(proc.stdout.strip(), "", f"{cmd!r} must abstain (G2 near miss): {proc.stdout!r}")
            self.assertEqual(read_ledger(ledger_dir), [])


class TestMultilinePollingLoopScope(RouterTestCase):
    """F11: rules compile with MULTILINE, not DOTALL, so the polling rule's
    `.*` never crossed a newline -- a normal multiline `while`/`until` loop
    abstained while its one-line equivalent asked. Match must span the loop
    body's actual extent (bounded at its own `done`), keeping an out-of-loop
    `gh` call from being pulled into an unrelated loop's sleep."""

    def test_multiline_while_loop_asks(self):
        cmd = "while true; do\n  gh pr checks 123\n  sleep 5\ndone"
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
            self.assertTrue(proc.stdout.strip(), f"{cmd!r} must not abstain (F11)")
            hso = json.loads(proc.stdout)["hookSpecificOutput"]
            self.assertEqual(hso.get("permissionDecision"), "ask", cmd)

    def test_multiline_until_loop_asks(self):
        cmd = "until false; do\n  gh pr checks 123\n  sleep 5\ndone"
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
            self.assertTrue(proc.stdout.strip(), f"{cmd!r} must not abstain (F11)")
            hso = json.loads(proc.stdout)["hookSpecificOutput"]
            self.assertEqual(hso.get("permissionDecision"), "ask", cmd)

    def test_bounded_for_loop_over_pr_list_still_abstains(self):
        cmd = "for n in 293 294 295; do gh pr checks $n; sleep 2; done"
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
            self.assertEqual(proc.stdout.strip(), "", f"{cmd!r} must abstain (F11 near miss)")
            self.assertEqual(read_ledger(ledger_dir), [])

    def test_out_of_loop_gh_call_before_an_unrelated_loop_abstains(self):
        cmd = "gh pr checks 123\nwhile true; do\n  sleep 5\ndone"
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
            self.assertEqual(proc.stdout.strip(), "", f"{cmd!r} must abstain (F11 near miss)")
            self.assertEqual(read_ledger(ledger_dir), [])

    def test_polling_loop_after_an_earlier_unrelated_loop_still_asks(self):
        """F7 (review round 2 redo): `finditer` never revisits text inside
        an already-yielded span, even a REJECTED one. The first candidate
        greedily spanned from the earlier `while read` loop all the way
        through the later polling loop's own `done`, got rejected by
        `_polling_loop_bounded` (two `done`s), and finditer then resumed
        searching from that rejected span's END -- past the real polling
        loop entirely. A genuine polling loop placed after an unrelated
        loop must still ask."""
        cmd = "while read x; do echo $x; done < f\nwhile true; do gh pr checks 1; sleep 5; done"
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
            self.assertTrue(proc.stdout.strip(), f"{cmd!r} must not abstain (F7)")
            hso = json.loads(proc.stdout)["hookSpecificOutput"]
            self.assertEqual(hso.get("permissionDecision"), "ask", cmd)

    def test_gh_call_between_an_earlier_loops_done_and_a_later_loop_abstains(self):
        """F3 (review round 1 redo): the loop-body gap used a lazy
        `[\\s\\S]*?` that is not bounded to its OWN `done` -- it can skip
        past an earlier, unrelated loop's `done` and pick up a `gh`/`sleep`
        pair that sits between two loops, plus a `for` loop after it, and
        ask. The gap must not cross any `done` token."""
        cmd = (
            "while read x; do echo $x; done < f\n"
            "gh pr checks 123\n"
            "sleep 5\n"
            "for p in 1 2; do echo; done"
        )
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
            self.assertEqual(proc.stdout.strip(), "", f"{cmd!r} must abstain (F3 near miss)")
            self.assertEqual(read_ledger(ledger_dir), [])

    def test_outer_loop_containing_a_genuine_nested_loop_still_asks(self):
        """G3 (review_b round 1): `_polling_loop_bounded` rejected any span
        with more than one `done` token, which also rejects a genuine OUTER
        polling loop that merely CONTAINS a fully-closed nested bounded
        loop (two `done`s here, one per loop, both legitimate). Must still
        ask -- nesting is not the same thing as crossing into an unrelated
        sibling loop (F3/F7's actual concern, covered by the near miss
        above)."""
        cmd = "while true; do\n  for i in 1 2; do\n    echo $i\n  done\n  gh pr checks 123\n  sleep 5\ndone"
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
            self.assertTrue(proc.stdout.strip(), f"{cmd!r} must not abstain (G3)")
            hso = json.loads(proc.stdout)["hookSpecificOutput"]
            self.assertEqual(hso.get("permissionDecision"), "ask", cmd)

    def test_nested_bounded_loop_after_gh_and_sleep_still_asks(self):
        """G5 (review_b round 2): a nested bounded loop placed AFTER the
        `gh`+sleep pair (rather than before, G3's case) makes the lazy
        gh-fast-polling regex's candidate span stop at the NESTED loop's own
        `done` -- the first `done` that satisfies the CLI+sleep requirement
        -- instead of the outer loop's real terminator further out.
        `_polling_loop_bounded` correctly rejects that short span (depth
        never returns to 0), but the caller then discarded the match
        entirely instead of extending the candidate to the next `done` and
        re-checking. Must still ask -- nesting is legitimate regardless of
        whether it comes before or after the CLI+sleep pair."""
        cmd = "while true; do\n  gh pr checks 123\n  sleep 5\n  for i in 1 2; do\n    echo $i\n  done\ndone"
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
            self.assertTrue(proc.stdout.strip(), f"{cmd!r} must not abstain (G5)")
            hso = json.loads(proc.stdout)["hookSpecificOutput"]
            self.assertEqual(hso.get("permissionDecision"), "ask", cmd)

    def test_quoted_done_argument_does_not_terminate_the_loop_early(self):
        """G4 (spec-level review, reopen generation 2, contract Tnqez39tp):
        the loop-body bound only looks for the literal word `done`, with no
        awareness that a `done` occurrence can sit inside a QUOTED argument
        (e.g. `echo 'done'`) rather than in command position as the actual
        `do`/`done` loop-closing keyword. `while true; do echo 'done'; gh pr
        checks 123; sleep 5; done` has its real terminator at the very end,
        but the quoted `'done'` argument to `echo` gets counted as if it
        were that terminator, truncating the loop body before the `gh`+
        `sleep` pair is ever reached -- so the base gh-fast-polling rule,
        which DOES match this command's raw text, never gets a chance to
        ask."""
        cmd = "while true; do echo 'done'; gh pr checks 123; sleep 5; done"
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(make_payload("Bash", {"command": cmd}), ledger_dir)
            self.assertTrue(proc.stdout.strip(), f"{cmd!r} must not abstain (G4)")
            hso = json.loads(proc.stdout)["hookSpecificOutput"]
            self.assertEqual(hso.get("permissionDecision"), "ask", cmd)


class TestF7RedactionAndLedgerPrivacy(RouterTestCase):
    """F7: the router persists raw `match`/`preview` text into the ledger,
    and creates the ledger dir/file with whatever the process umask leaves
    (0755/0644 under a common 022 umask) -- both a secret-leak and a
    world/group-readable-file class of bug. A shared redaction policy must
    scrub every persisted text field, and the ledger dir/file must be
    private (0700/0600) regardless of umask."""

    SECRET = "sk-ant-api03-REDACTME1234567890ABCDEFGHIJK"

    def test_secret_in_command_is_redacted_from_match_and_preview(self):
        payload = make_payload(
            "Bash",
            {
                "command": (
                    f"curl -H 'Authorization: Bearer {self.SECRET}' https://x; "
                    "git stash"
                )
            },
        )
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(payload, ledger_dir)
            self.assertEqual(proc.returncode, 0)
            lines = read_ledger(ledger_dir)
            self.assertTrue(lines)
            for entry in lines:
                self.assertNotIn(self.SECRET, entry.get("match", ""))
                self.assertNotIn(self.SECRET, entry.get("preview", ""))

    def test_ledger_dir_and_file_are_private_under_permissive_umask(self):
        old_umask = os.umask(0o022)
        try:
            with tempfile.TemporaryDirectory() as base:
                ledger_dir = Path(base) / "fresh-ledger-subdir"
                payload = make_payload("Bash", {"command": "git stash"})
                proc = run_router_payload(payload, ledger_dir)
                self.assertEqual(proc.returncode, 0)

                ledger_file = ledger_dir / LEDGER_FILENAME
                self.assertTrue(ledger_file.exists())
                dir_mode = stat.S_IMODE(ledger_dir.stat().st_mode)
                file_mode = stat.S_IMODE(ledger_file.stat().st_mode)
                self.assertEqual(
                    oct(dir_mode), oct(0o700),
                    f"ledger dir must be 0700, got {oct(dir_mode)}",
                )
                self.assertEqual(
                    oct(file_mode), oct(0o600),
                    f"ledger file must be 0600, got {oct(file_mode)}",
                )
        finally:
            os.umask(old_umask)

    # --- review_b round 3 (G1/G2/G3): the F7 redaction policy has three
    # concrete gaps, each demonstrated with a real-shaped secret. ---------

    def test_sk_proj_and_svcacct_style_keys_are_redacted(self):
        """G1: the generic `sk-` pattern only allows [A-Za-z0-9] in the key
        body, so real OpenAI-shaped keys with internal hyphens/underscores
        (`sk-proj-...`, `sk-svcacct-...`) are only partially matched (or not
        at all) and the live suffix survives into the ledger unchanged."""
        # No "Bearer "/"password="/etc wrapper -- isolates the `sk-`
        # pattern itself (a wrapping keyword's own greedy `\S+` would mask
        # this bug by accident).
        proj_key = "sk-proj-AbCdEfGh_IjKlMnOp-QrStUvWx1234567890"
        svcacct_key = "sk-svcacct-ZyXwVuTs_RqPoNmLk-JiHgFeDc0987654321"
        payload = make_payload(
            "Bash",
            {
                "command": (
                    f"echo {proj_key} {svcacct_key} && git stash"
                )
            },
        )
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(payload, ledger_dir)
            self.assertEqual(proc.returncode, 0)
            lines = read_ledger(ledger_dir)
            self.assertTrue(lines)
            for entry in lines:
                self.assertNotIn(proj_key, entry.get("match", ""))
                self.assertNotIn(proj_key, entry.get("preview", ""))
                self.assertNotIn(svcacct_key, entry.get("match", ""))
                self.assertNotIn(svcacct_key, entry.get("preview", ""))

    def test_quoted_password_with_spaces_is_fully_redacted(self):
        """G2a: `password=...` matches `\\S+` for the value, so a quoted
        password containing spaces (`password="hunter two secret"`) only
        redacts up to the first space and leaks the rest of the phrase."""
        payload = make_payload(
            "Bash",
            {"command": 'echo password="hunter two secret" && git stash'},
        )
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(payload, ledger_dir)
            self.assertEqual(proc.returncode, 0)
            lines = read_ledger(ledger_dir)
            self.assertTrue(lines)
            for entry in lines:
                self.assertNotIn("hunter two secret", entry.get("match", ""))
                self.assertNotIn("hunter two secret", entry.get("preview", ""))
                self.assertNotIn("two secret", entry.get("preview", ""))

    def test_pem_block_survives_a_preceding_secret_assignment(self):
        """G2b: `secret=...` is matched (and its value truncated at the
        first token) BEFORE the PEM-block pattern runs, so a `secret=` (or
        `token=`) prefix right before a PEM block eats the `-----BEGIN`
        marker and the PEM pattern can no longer recognize (and redact) the
        private-key body that follows."""
        pem = (
            "-----BEGIN PRIVATE KEY-----\n"
            "MIIEvQIBADANBgkqhkiG9w0BAQEREDACTMEREDACTMEREDACTME\n"
            "-----END PRIVATE KEY-----"
        )
        payload = make_payload(
            "Bash",
            {"command": f'echo "secret={pem}" && git stash'},
        )
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(payload, ledger_dir)
            self.assertEqual(proc.returncode, 0)
            lines = read_ledger(ledger_dir)
            self.assertTrue(lines)
            for entry in lines:
                self.assertNotIn(
                    "MIIEvQIBADANBgkqhkiG9w0BAQEREDACTMEREDACTMEREDACTME",
                    entry.get("preview", ""),
                )

    def test_cwd_field_in_ledger_entry_is_redacted(self):
        """G3: the router persists the raw hook-payload `cwd` straight into
        every ledger line without passing it through `redact()` -- only
        `match`/`preview` are scrubbed. A secret embedded in `cwd` (payload
        metadata, fully attacker-controlled) survives into the ledger."""
        secret = "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"
        payload = make_payload(
            "Bash", {"command": "git stash"}, cwd=f"/tmp/{secret}/repo"
        )
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(payload, ledger_dir)
            self.assertEqual(proc.returncode, 0)
            lines = read_ledger(ledger_dir)
            self.assertTrue(lines)
            for entry in lines:
                self.assertNotIn(secret, entry.get("cwd", ""))

    def test_session_id_field_in_ledger_entry_is_redacted(self):
        """G3 (review_b round 4): the router persists the raw hook-payload
        `session_id` straight into every ledger line without passing it
        through `redact()` -- `cwd` was fixed but `session_id` is the same
        attacker-controlled payload metadata and was missed."""
        secret = "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"
        payload = make_payload(
            "Bash", {"command": "git stash"}, session_id=f"sess-{secret}"
        )
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(payload, ledger_dir)
            self.assertEqual(proc.returncode, 0)
            lines = read_ledger(ledger_dir)
            self.assertTrue(lines)
            for entry in lines:
                self.assertNotIn(secret, entry.get("session_id", ""))

    def test_escaped_quote_inside_password_does_not_leak_the_tail(self):
        """G2 (review_b round 4): the bare-double-quote alternative
        (`"[^"]*"`) has no escape awareness, so a raw shell value with a
        backslash-escaped inner quote (`password="alpha \\"bravo\\"
        charlie"`, valid bash -- `\\"` inside double quotes is a literal
        quote) makes `[^"]*` stop at that embedded quote instead of the
        real closing one. Only the leading fragment gets redacted and the
        rest of the value (starting with "bravo") survives in the clear."""
        payload = make_payload(
            "Bash",
            {"command": r'echo password="alpha \"bravo\" charlie" && git stash'},
        )
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(payload, ledger_dir)
            self.assertEqual(proc.returncode, 0)
            lines = read_ledger(ledger_dir)
            self.assertTrue(lines)
            for entry in lines:
                self.assertNotIn("bravo", entry.get("match", ""))
                self.assertNotIn("bravo", entry.get("preview", ""))
                self.assertNotIn("charlie", entry.get("preview", ""))

    def test_escaped_quote_followed_by_real_newline_does_not_leak_the_tail(self):
        """G2 continued (review_b round 5): the escaped-char alternative
        `\\.` in `(?:[^"\\]|\\.)*` requires `.`, which -- with no
        re.DOTALL -- never matches a real newline. A trailing backslash
        immediately before a real newline is valid bash line-continuation
        inside a double-quoted string; confirmed against a real bash that
        `password="alpha <backslash-newline>bravo" charlie` folds to
        `alpha bravo charlie`. The regex alternation can't step past that
        backslash-newline, falls out of the quoted branch, and the
        `\\S+` fallback leaks everything after the newline in the
        clear."""
        payload = make_payload(
            "Bash",
            {"command": 'echo password="alpha \\\nbravo charlie" && git stash'},
        )
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(payload, ledger_dir)
            self.assertEqual(proc.returncode, 0)
            lines = read_ledger(ledger_dir)
            self.assertTrue(lines)
            for entry in lines:
                self.assertNotIn("bravo", entry.get("match", ""))
                self.assertNotIn("bravo", entry.get("preview", ""))
                self.assertNotIn("charlie", entry.get("preview", ""))

    def test_masking_does_not_strip_quotes_before_redaction_sees_the_value(self):
        """G1 (spec-level review, reopen generation 2): `evaluate()` builds
        the ledger's `match` field by extracting `raw_matched` from
        `scan_text` (the `executable_mask`-masked copy) and only THEN
        calling `redact()` on it -- but masking a double-quoted argument
        blanks the quote DELIMITERS themselves (see `_mask_literal_span`),
        leaving the literal content visible with no surrounding quote
        characters. `redact()`'s `password=`/`token=` pattern needs to see
        an actual quote character to take its quoted-value alternative
        (which can contain spaces); with the quotes already gone by the
        time `redact()` runs, it falls through to the bare `\\S+`
        alternative and only the first word is redacted -- the rest of a
        multi-word password (e.g. a `git push -o password="..."` refspec
        override) survives in the clear in the persisted `match` field."""
        payload = make_payload(
            "Bash",
            {
                "command": (
                    'git push -o password="alpha bravo charlie" '
                    "origin +HEAD:main"
                )
            },
        )
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(payload, ledger_dir)
            self.assertEqual(proc.returncode, 0)
            ledger_path = Path(ledger_dir) / LEDGER_FILENAME
            raw_line = ledger_path.read_text()
            for word in ("alpha", "bravo", "charlie"):
                self.assertNotIn(
                    word, raw_line,
                    f"secret word {word!r} leaked into the raw ledger line "
                    f"(G1): {raw_line!r}",
                )
            lines = read_ledger(ledger_dir)
            self.assertTrue(lines)
            for entry in lines:
                for word in ("alpha", "bravo", "charlie"):
                    self.assertNotIn(word, entry.get("match", ""))
                    self.assertNotIn(word, entry.get("preview", ""))


class TestF8ManifestQuoting(RouterTestCase):
    """F8: required-hooks.json's two Python hook commands interpolate
    $HEX_DIR unquoted (`python3 -I -S $HEX_DIR/.hex/hooks/scripts/...py`).
    The hook runner executes these as a real shell command line (bash -c),
    so a HEX_DIR containing a space is word-split: Python receives a
    truncated, nonexistent script path as argv[0] and exits non-zero."""

    MANIFEST = REPO_ROOT / "system" / "hooks" / "required-hooks.json"

    def _hook_commands(self):
        manifest = json.loads(self.MANIFEST.read_text())
        router_cmd = next(
            e["command"] for e in manifest["PreToolUse"]
            if "pretooluse-router.py" in e.get("command", "")
        )
        incident_cmd = manifest["PostToolUseFailure"][0]["command"]
        return router_cmd, incident_cmd

    def _run_from_spaced_hex_dir(self, command, stdin_payload):
        with tempfile.TemporaryDirectory() as base:
            spaced = Path(base) / "hex workspace with spaces"
            scripts_dir = spaced / ".hex" / "hooks" / "scripts"
            scripts_dir.mkdir(parents=True)
            shutil.copy(ROUTER_SCRIPT, scripts_dir / "pretooluse-router.py")
            shutil.copy(
                REPO_ROOT / "system" / "hooks" / "router-rules.json",
                spaced / ".hex" / "hooks" / "router-rules.json",
            )
            shutil.copy(
                REPO_ROOT / "system" / "hooks" / "scripts" / "posttoolusefailure-incident.py",
                scripts_dir / "posttoolusefailure-incident.py",
            )
            with tempfile.TemporaryDirectory() as ledger_dir:
                env = dict(os.environ)
                env["HEX_DIR"] = str(spaced)
                env["HEX_LEDGER_DIR"] = ledger_dir
                return subprocess.run(
                    ["bash", "-c", command],
                    input=json.dumps(stdin_payload),
                    capture_output=True,
                    text=True,
                    env=env,
                    timeout=10,
                )

    def test_router_hook_runs_from_a_path_containing_a_space(self):
        router_cmd, _ = self._hook_commands()
        proc = self._run_from_spaced_hex_dir(
            router_cmd, make_payload("Bash", {"command": "git stash"})
        )
        self.assertEqual(
            proc.returncode, 0,
            f"router hook command must run from a spaced HEX_DIR; "
            f"stderr={proc.stderr!r}",
        )
        self.assertIn("permissionDecision", proc.stdout)

    def test_incident_hook_runs_from_a_path_containing_a_space(self):
        _, incident_cmd = self._hook_commands()
        payload = {
            "session_id": "s1",
            "transcript_path": "/tmp/t",
            "cwd": "/repo",
            "permission_mode": "default",
            "hook_event_name": "PostToolUseFailure",
            "tool_name": "Bash",
            "tool_input": {"command": "npm test"},
            "tool_use_id": "t1",
            "error": "boom",
            "is_interrupt": False,
            "duration_ms": 1,
        }
        proc = self._run_from_spaced_hex_dir(incident_cmd, payload)
        self.assertEqual(
            proc.returncode, 0,
            f"incident hook command must run from a spaced HEX_DIR; "
            f"stderr={proc.stderr!r}",
        )


class TestF12PathRuleAnchoring(RouterTestCase):
    """F12: rules are compiled with re.MULTILINE, so `^[^\\n]*...` matches
    every line of the canonical text, not just the first (the actual file
    path). A pattern that only appears inside file CONTENT must abstain --
    only the real path, on the canonical first line, may fire."""

    def test_vitest_pattern_in_content_only_abstains(self):
        payload = make_payload(
            "Write",
            {"file_path": "notes.md", "content": "intro line\nfoo.test.ts\nspawnSync(cmd)"},
        )
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(payload, ledger_dir)
            self.assertEqual(
                proc.stdout.strip(), "",
                "a .test.ts/spawnSync pair inside file CONTENT (not the "
                "canonical file_path) must not fire vitest-spawnsync",
            )
            self.assertEqual(
                proc.stderr.strip(), "",
                f"abstain/fire must produce no stderr, got {proc.stderr!r}",
            )
            self.assertEqual(read_ledger(ledger_dir), [])

    def test_hex_events_policy_pathname_in_content_only_abstains(self):
        payload = make_payload(
            "Write",
            {
                "file_path": "notes.md",
                "content": "intro\n.hex-events/policies/foo.yaml mentioned here",
            },
        )
        with tempfile.TemporaryDirectory() as ledger_dir:
            proc = run_router_payload(payload, ledger_dir)
            self.assertEqual(
                proc.stdout.strip(), "",
                "a policy pathname mentioned inside file CONTENT (not the "
                "canonical file_path) must not fire hex-events-flat-policy",
            )
            self.assertEqual(
                proc.stderr.strip(), "",
                f"abstain/fire must produce no stderr, got {proc.stderr!r}",
            )
            self.assertEqual(read_ledger(ledger_dir), [])


class TestF18LatencyGateIsOptIn(RouterTestCase):
    """F18: the tight per-invocation wall-clock ceiling in
    TestLatencySanityCeiling currently runs unconditionally on every
    correctness pass (a bare `assertLess(d, 1.0, ...)` over 20 runs) and
    is load-sensitive on a shared worker. It must move behind an opt-in
    env gate (e.g. ROUTER_BENCH=1); the 5s subprocess hang timeout is a
    separate, structural guarantee and stays mandatory."""

    def test_tight_latency_assertion_is_env_gated_in_source(self):
        text = Path(__file__).read_text()
        start = text.index("class TestLatencySanityCeiling")
        end = text.index("\nclass ", start + 1)
        body = text[start:end]
        self.assertIn(
            "ROUTER_BENCH", body,
            "F18: the per-invocation latency ceiling must be gated behind "
            "an opt-in ROUTER_BENCH env var, not asserted unconditionally "
            "in every correctness run",
        )


class TestF21NotebookEditCanonicalization(RouterTestCase):
    """F21: NotebookEdit is listed in TEXT_TOOLS, but canonical_text()'s
    TEXT_TOOLS branch only ever reads file_path/new_string/content/edits.
    NotebookEdit's actual schema is notebook_path + new_source -- neither
    is read, so a real NotebookEdit payload canonicalizes to an empty
    string and no rule can ever inspect it."""

    def _load_router_module(self):
        spec = importlib.util.spec_from_file_location(
            "pretooluse_router_under_test_f21", ROUTER_SCRIPT
        )
        assert spec and spec.loader
        mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(mod)
        return mod

    def test_notebook_path_and_new_source_both_reach_canonical_text(self):
        mod = self._load_router_module()
        # A real PostToolUse/PreToolUse NotebookEdit payload shape.
        tool_input = {
            "notebook_path": "/repo/analysis.ipynb",
            "new_source": "result = spawnSync('ls', [])",
            "cell_type": "code",
            "edit_mode": "replace",
        }
        text = mod.canonical_text("NotebookEdit", tool_input)
        self.assertIn("/repo/analysis.ipynb", text)
        self.assertIn("spawnSync('ls', [])", text)

    def test_new_source_alone_reaches_canonical_text_when_path_absent(self):
        mod = self._load_router_module()
        text = mod.canonical_text("NotebookEdit", {"new_source": "spawnSync(cmd)"})
        self.assertIn("spawnSync(cmd)", text)


class TestF22ProbeMktempFailure(RouterTestCase):
    """F22: router-probe.sh only has `set -u`, never checking mktemp's own
    exit status (`LEDGER_DIR="$(mktemp -d)"`). If mktemp fails, LEDGER_DIR
    is empty (not unset, so `set -u` never trips); Python's `Path("")`
    resolves to the current directory, so a failed run can silently drop
    router-fires.jsonl into cwd while still reporting PASS/FAIL lines."""

    PROBE = REPO_ROOT / "system" / "hooks" / "scripts" / "router-probe.sh"

    def test_mktemp_failure_aborts_instead_of_writing_into_cwd(self):
        with tempfile.TemporaryDirectory() as fake_bin, tempfile.TemporaryDirectory() as cwd:
            mktemp_stub = Path(fake_bin) / "mktemp"
            mktemp_stub.write_text(
                "#!/bin/sh\necho 'mktemp: simulated failure' >&2\nexit 1\n"
            )
            mktemp_stub.chmod(0o755)

            env = dict(os.environ)
            env["PATH"] = f"{fake_bin}:{env.get('PATH', '')}"

            proc = subprocess.run(
                ["bash", str(self.PROBE)],
                capture_output=True,
                text=True,
                env=env,
                cwd=cwd,
                timeout=60,
            )

            self.assertNotEqual(
                proc.returncode, 0,
                "the probe must abort when mktemp -d fails, not report a "
                f"clean run; stdout={proc.stdout[-500:]!r}",
            )
            self.assertFalse(
                (Path(cwd) / LEDGER_FILENAME).exists(),
                "a failed mktemp must never leave a ledger file behind in cwd",
            )


if __name__ == "__main__":
    unittest.main()
