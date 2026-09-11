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


if __name__ == "__main__":
    unittest.main()
