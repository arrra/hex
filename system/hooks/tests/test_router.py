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
            self.assertIsInstance(hso["additionalContext"], str)

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


if __name__ == "__main__":
    unittest.main()
