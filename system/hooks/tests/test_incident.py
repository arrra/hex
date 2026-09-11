"""RED-first tests for the PostToolUseFailure incident-logging hook.

Pins the CONTRACT (deliverable 3 of the closed-loop harness plan §2):
  system/hooks/scripts/posttoolusefailure-incident.py reads the PostToolUseFailure
  stdin payload once and appends exactly one line to
  ${HEX_LEDGER_DIR:-$HOME/.hex/ledger}/incidents.jsonl with the fields:
    ts, session_id, tool, args_hash, args_preview, error, cwd, tool_use_id,
    duration_ms, is_interrupt
  - args_hash: first 12 hex chars of sha256(json.dumps(tool_input, sort_keys=True))
    -> identical for two payloads whose tool_input differs only in key order.
  - error truncated to 2000 chars, args_preview truncated to 200 chars.
  - malformed/empty stdin -> exit 0, no ledger line, exactly one stderr line.
  - ledger dir is created if missing.
  - the hook never writes to stdout and never exits non-zero.

Run: python3 -m unittest discover -s system/hooks/tests -p "test_incident.py" -v
"""
import hashlib
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[3]
SCRIPT = REPO_ROOT / "system" / "hooks" / "scripts" / "posttoolusefailure-incident.py"

REQUIRED_FIELDS = {
    "ts",
    "session_id",
    "tool",
    "args_hash",
    "args_preview",
    "error",
    "cwd",
    "tool_use_id",
    "duration_ms",
    "is_interrupt",
}

HASH_RE = re.compile(r"^[0-9a-f]{12}$")

# The exact fixture from the docs (ground truth, PostToolUseFailure example).
DOCS_ERROR = "Exit code 1\nError: Cannot find module 'express'"


def _fixture(tool_input=None, **overrides):
    payload = {
        "session_id": "sess-abc123",
        "transcript_path": "/tmp/transcript.jsonl",
        "cwd": "/tmp/hex-home/project",
        "permission_mode": "default",
        "hook_event_name": "PostToolUseFailure",
        "tool_name": "Bash",
        "tool_input": tool_input if tool_input is not None else {"command": "npm test"},
        "tool_use_id": "toolu_01abc",
        "error": DOCS_ERROR,
        "is_interrupt": False,
        "duration_ms": 4187,
    }
    payload.update(overrides)
    return payload


class IncidentHookTestCase(unittest.TestCase):
    def setUp(self):
        self.ledger_dir = tempfile.mkdtemp(prefix="hex-ledger-")
        self.addCleanup(shutil.rmtree, self.ledger_dir, ignore_errors=True)

    def _incidents_path(self, ledger_dir=None):
        return Path(ledger_dir or self.ledger_dir) / "incidents.jsonl"

    def _run(self, payload, ledger_dir=None, raw_stdin=None):
        env = dict(os.environ)
        env["HEX_LEDGER_DIR"] = ledger_dir or self.ledger_dir
        stdin_bytes = raw_stdin if raw_stdin is not None else json.dumps(payload).encode()
        return subprocess.run(
            # F15: production (required-hooks.json) invokes this hook as
            # `python3 -I -S ...` -- isolated mode, no site-packages/
            # PYTHONPATH. Match that startup here so the suite exercises
            # the same environment the real hook runs under.
            [sys.executable, "-I", "-S", str(SCRIPT)],
            input=stdin_bytes,
            capture_output=True,
            env=env,
            timeout=10,
        )

    def _read_lines(self, ledger_dir=None):
        path = self._incidents_path(ledger_dir)
        if not path.exists():
            return []
        text = path.read_text()
        return [l for l in text.splitlines() if l.strip()]


class TestGoldenPath(IncidentHookTestCase):
    def test_writes_exactly_one_line_with_exact_fields(self):
        proc = self._run(_fixture())
        self.assertEqual(proc.returncode, 0, msg=proc.stderr.decode(errors="replace"))
        self.assertEqual(proc.stdout, b"", msg="hook must never write to stdout")

        lines = self._read_lines()
        self.assertEqual(len(lines), 1, msg=f"expected exactly one ledger line, got {lines!r}")

        record = json.loads(lines[0])
        self.assertEqual(set(record.keys()), REQUIRED_FIELDS)

        self.assertEqual(record["session_id"], "sess-abc123")
        self.assertEqual(record["tool"], "Bash")
        self.assertEqual(record["cwd"], "/tmp/hex-home/project")
        self.assertEqual(record["tool_use_id"], "toolu_01abc")
        self.assertEqual(record["duration_ms"], 4187)
        self.assertEqual(record["is_interrupt"], False)
        self.assertEqual(record["error"], DOCS_ERROR)
        self.assertIsInstance(record["ts"], str)
        self.assertTrue(record["ts"], msg="ts must be non-empty")

        self.assertRegex(record["args_hash"], HASH_RE)
        expected_hash = hashlib.sha256(
            json.dumps({"command": "npm test"}, sort_keys=True).encode()
        ).hexdigest()[:12]
        self.assertEqual(record["args_hash"], expected_hash)

        self.assertLessEqual(len(record["args_preview"]), 200)
        self.assertIn("npm test", record["args_preview"])

    def test_never_exits_non_zero_even_on_success(self):
        proc = self._run(_fixture())
        self.assertEqual(proc.returncode, 0)


class TestArgsHash(IncidentHookTestCase):
    def test_hash_is_key_order_independent(self):
        tool_input_a = {"command": "npm test", "timeout": 5000}
        tool_input_b = {"timeout": 5000, "command": "npm test"}

        dir_a = tempfile.mkdtemp(prefix="hex-ledger-a-")
        dir_b = tempfile.mkdtemp(prefix="hex-ledger-b-")
        self.addCleanup(shutil.rmtree, dir_a, ignore_errors=True)
        self.addCleanup(shutil.rmtree, dir_b, ignore_errors=True)

        proc_a = self._run(_fixture(tool_input=tool_input_a), ledger_dir=dir_a)
        proc_b = self._run(_fixture(tool_input=tool_input_b), ledger_dir=dir_b)
        self.assertEqual(proc_a.returncode, 0, msg=proc_a.stderr.decode(errors="replace"))
        self.assertEqual(proc_b.returncode, 0, msg=proc_b.stderr.decode(errors="replace"))

        record_a = json.loads(self._read_lines(dir_a)[0])
        record_b = json.loads(self._read_lines(dir_b)[0])

        expected_hash = hashlib.sha256(
            json.dumps(tool_input_a, sort_keys=True).encode()
        ).hexdigest()[:12]

        self.assertEqual(record_a["args_hash"], expected_hash)
        self.assertEqual(record_b["args_hash"], expected_hash)
        self.assertEqual(record_a["args_hash"], record_b["args_hash"])


class TestTruncation(IncidentHookTestCase):
    def test_error_truncated_to_2000_chars(self):
        long_error = "E" * 3000
        proc = self._run(_fixture(error=long_error))
        self.assertEqual(proc.returncode, 0, msg=proc.stderr.decode(errors="replace"))

        record = json.loads(self._read_lines()[0])
        self.assertEqual(len(record["error"]), 2000)
        self.assertEqual(record["error"], long_error[:2000])

    def test_args_preview_truncated_to_200_chars(self):
        big_command = "echo " + ("x" * 500)
        proc = self._run(_fixture(tool_input={"command": big_command}))
        self.assertEqual(proc.returncode, 0, msg=proc.stderr.decode(errors="replace"))

        record = json.loads(self._read_lines()[0])
        self.assertLessEqual(len(record["args_preview"]), 200)


class TestFailOpen(IncidentHookTestCase):
    def test_malformed_stdin_exits_zero_writes_no_line_one_stderr_line(self):
        proc = self._run(None, raw_stdin=b"not json {{{ at all")
        self.assertEqual(proc.returncode, 0)
        self.assertEqual(proc.stdout, b"")
        self.assertEqual(self._read_lines(), [])

        stderr_lines = [l for l in proc.stderr.decode(errors="replace").splitlines() if l.strip()]
        self.assertEqual(len(stderr_lines), 1, msg=f"expected exactly one stderr line, got {stderr_lines!r}")

    def test_empty_stdin_exits_zero_writes_no_line_one_stderr_line(self):
        proc = self._run(None, raw_stdin=b"")
        self.assertEqual(proc.returncode, 0)
        self.assertEqual(proc.stdout, b"")
        self.assertEqual(self._read_lines(), [])

        stderr_lines = [l for l in proc.stderr.decode(errors="replace").splitlines() if l.strip()]
        self.assertEqual(len(stderr_lines), 1, msg=f"expected exactly one stderr line, got {stderr_lines!r}")


class TestLedgerDirCreation(IncidentHookTestCase):
    def test_ledger_dir_created_if_missing(self):
        parent = tempfile.mkdtemp(prefix="hex-ledger-parent-")
        self.addCleanup(shutil.rmtree, parent, ignore_errors=True)
        missing_dir = str(Path(parent) / "nested" / "ledger")
        self.assertFalse(Path(missing_dir).exists())

        proc = self._run(_fixture(), ledger_dir=missing_dir)
        self.assertEqual(proc.returncode, 0, msg=proc.stderr.decode(errors="replace"))
        self.assertTrue(Path(missing_dir).is_dir())
        self.assertEqual(len(self._read_lines(missing_dir)), 1)


class TestF7RedactionAndLedgerPrivacy(IncidentHookTestCase):
    """F7: this hook copies raw `error` and `args_preview` text straight
    into the ledger, and creates the ledger dir/file with whatever the
    process umask leaves (0755/0644 under a common 022 umask). A shared
    redaction policy must scrub every persisted text field, and the
    ledger dir/file must be private (0700/0600) regardless of umask."""

    SECRET = "sk-ant-api03-REDACTME1234567890ABCDEFGHIJK"

    def test_secrets_in_error_and_args_are_redacted(self):
        proc = self._run(
            _fixture(
                tool_input={"command": f"curl -H 'Authorization: Bearer {self.SECRET}'"},
                error=(
                    "failed: password=hunter2secretvalue and "
                    "token=ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"
                ),
            )
        )
        self.assertEqual(proc.returncode, 0, msg=proc.stderr.decode(errors="replace"))
        record = json.loads(self._read_lines()[0])
        self.assertNotIn(self.SECRET, record["args_preview"])
        self.assertNotIn("hunter2secretvalue", record["error"])
        self.assertNotIn(
            "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789", record["error"]
        )

    def test_ledger_dir_and_file_are_private_under_permissive_umask(self):
        old_umask = os.umask(0o022)
        try:
            parent = tempfile.mkdtemp(prefix="hex-ledger-f7-")
            self.addCleanup(shutil.rmtree, parent, ignore_errors=True)
            fresh_dir = str(Path(parent) / "fresh-ledger-subdir")

            proc = self._run(_fixture(), ledger_dir=fresh_dir)
            self.assertEqual(proc.returncode, 0, msg=proc.stderr.decode(errors="replace"))

            incidents_path = Path(fresh_dir) / "incidents.jsonl"
            self.assertTrue(incidents_path.exists())
            dir_mode = stat.S_IMODE(Path(fresh_dir).stat().st_mode)
            file_mode = stat.S_IMODE(incidents_path.stat().st_mode)
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

    # --- review_b round 3 (G1/G2/G3): same redaction-policy gaps as the
    # router, duplicated verbatim into this hook. -------------------------

    def test_sk_proj_and_svcacct_style_keys_are_redacted(self):
        """G1: real OpenAI-shaped keys with internal hyphens/underscores
        are only partially matched by the generic `sk-` pattern (alnum-only
        body) and the live suffix survives into the ledger unchanged."""
        # No "Bearer "/"password="/etc wrapper -- isolates the `sk-`
        # pattern itself (a wrapping keyword's own greedy `\S+` would mask
        # this bug by accident).
        proj_key = "sk-proj-AbCdEfGh_IjKlMnOp-QrStUvWx1234567890"
        proc = self._run(
            _fixture(
                tool_input={"command": f"echo {proj_key}"},
                error=f"failed with key {proj_key}",
            )
        )
        self.assertEqual(proc.returncode, 0, msg=proc.stderr.decode(errors="replace"))
        record = json.loads(self._read_lines()[0])
        self.assertNotIn(proj_key, record["args_preview"])
        self.assertNotIn(proj_key, record["error"])

    def test_quoted_password_with_spaces_is_fully_redacted(self):
        """G2a: `password=...` matches `\\S+` for the value, so a quoted
        password containing spaces only redacts up to the first space."""
        proc = self._run(
            _fixture(error='failed: password="hunter two secret" and retry')
        )
        self.assertEqual(proc.returncode, 0, msg=proc.stderr.decode(errors="replace"))
        record = json.loads(self._read_lines()[0])
        self.assertNotIn("hunter two secret", record["error"])
        self.assertNotIn("two secret", record["error"])

    def test_quoted_password_in_args_preview_is_fully_redacted(self):
        """G2 (review round 3 redo): args_preview = redact(json.dumps(tool_input))
        JSON-escapes the quoted value (password=\\"hunter two secret\\"), so the
        bare-double-quote alternation never matches and `\\S+` eats only up to
        the first space, leaking the rest of the phrase into the ledger."""
        proc = self._run(
            _fixture(
                tool_input={
                    "command": 'curl -u admin password="hunter two secret" https://x'
                }
            )
        )
        self.assertEqual(proc.returncode, 0, msg=proc.stderr.decode(errors="replace"))
        record = json.loads(self._read_lines()[0])
        self.assertNotIn("two secret", record["args_preview"])

    def test_pem_block_survives_a_preceding_secret_assignment(self):
        """G2b: `secret=...` is matched (and truncated at the first token)
        BEFORE the PEM-block pattern runs, so a `secret=` prefix right
        before a PEM block eats the `-----BEGIN` marker and the PEM body
        leaks unredacted."""
        pem = (
            "-----BEGIN PRIVATE KEY-----\n"
            "MIIEvQIBADANBgkqhkiG9w0BAQEREDACTMEREDACTMEREDACTME\n"
            "-----END PRIVATE KEY-----"
        )
        proc = self._run(_fixture(error=f"secret={pem}"))
        self.assertEqual(proc.returncode, 0, msg=proc.stderr.decode(errors="replace"))
        record = json.loads(self._read_lines()[0])
        self.assertNotIn(
            "MIIEvQIBADANBgkqhkiG9w0BAQEREDACTMEREDACTMEREDACTME",
            record["error"],
        )

    def test_cwd_field_is_redacted(self):
        """G3: this hook copies the raw hook-payload `cwd` straight into
        the ledger record without passing it through `redact()` -- only
        `error`/`args_preview` are scrubbed. A secret embedded in `cwd`
        (payload metadata, fully attacker-controlled) survives."""
        secret = "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"
        proc = self._run(_fixture(cwd=f"/tmp/{secret}/repo"))
        self.assertEqual(proc.returncode, 0, msg=proc.stderr.decode(errors="replace"))
        record = json.loads(self._read_lines()[0])
        self.assertNotIn(secret, record["cwd"])

    def test_session_id_and_tool_use_id_are_redacted(self):
        """G3 (review_b round 4): this hook copies the raw hook-payload
        `session_id` and `tool_use_id` straight into the ledger record
        without passing them through `redact()` -- `cwd` was fixed but
        these are the same attacker-controlled payload metadata and were
        missed."""
        secret = "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"
        proc = self._run(
            _fixture(
                session_id=f"sess-{secret}",
                tool_use_id=f"toolu_{secret}",
            )
        )
        self.assertEqual(proc.returncode, 0, msg=proc.stderr.decode(errors="replace"))
        record = json.loads(self._read_lines()[0])
        self.assertNotIn(secret, record["session_id"])
        self.assertNotIn(secret, record["tool_use_id"])

    def test_escaped_quote_inside_password_does_not_leak_the_tail(self):
        """G2 (review_b round 4): the bare-double-quote alternative
        (`"[^"]*"`) has no escape awareness, so a raw text value with a
        backslash-escaped inner quote (`password="alpha \\"bravo\\"
        charlie"`) makes `[^"]*` stop at that embedded quote instead of
        the real closing one. Only the leading fragment gets redacted and
        the rest of the value (starting with "bravo") survives in the
        clear. This hits the `error` field, which is plain text (not
        JSON-escaped like `args_preview`)."""
        proc = self._run(
            _fixture(error='failed: password="alpha \\"bravo\\" charlie" and retry')
        )
        self.assertEqual(proc.returncode, 0, msg=proc.stderr.decode(errors="replace"))
        record = json.loads(self._read_lines()[0])
        self.assertNotIn("bravo", record["error"])
        self.assertNotIn("charlie", record["error"])

    def test_escaped_quote_followed_by_real_newline_does_not_leak_the_tail(self):
        """G2 continued (review_b round 5): the escaped-char alternative
        `\\.` in `(?:[^"\\]|\\.)*` requires `.`, which -- with no
        re.DOTALL -- never matches a real newline. A backslash immediately
        followed by a real newline is valid bash line-continuation inside a
        double-quoted string, but the regex can't step past it, falls out
        of the quoted branch, and the `\\S+` fallback leaks everything
        after the newline into the plain-text `error` field."""
        proc = self._run(
            _fixture(error='failed: password="alpha \\\nbravo charlie" and retry')
        )
        self.assertEqual(proc.returncode, 0, msg=proc.stderr.decode(errors="replace"))
        record = json.loads(self._read_lines()[0])
        self.assertNotIn("bravo", record["error"])
        self.assertNotIn("charlie", record["error"])


class TestF15ProductionIsolationFlags(IncidentHookTestCase):
    """F15: this test suite's `_run` helper invokes the hook as plain
    `[sys.executable, str(SCRIPT)]`, without production's `-I -S` isolated
    startup (required-hooks.json / router unittest helper both use it).
    Proven behaviorally: plant a sitecustomize.py that only runs when site
    processing happens (no -S) and is only visible via PYTHONPATH without
    -I."""

    def test_subprocess_helper_isolates_site_and_pythonpath(self):
        sitedir = tempfile.mkdtemp(prefix="hex-f15-sitecustomize-")
        self.addCleanup(shutil.rmtree, sitedir, ignore_errors=True)
        marker = Path(self.ledger_dir) / "f15-marker"
        Path(sitedir, "sitecustomize.py").write_text(
            "import os\n"
            "m = os.environ.get('F15_MARKER')\n"
            "if m:\n    open(m, 'w').close()\n"
        )

        old_pythonpath = os.environ.get("PYTHONPATH")
        old_marker_env = os.environ.get("F15_MARKER")
        os.environ["PYTHONPATH"] = sitedir
        os.environ["F15_MARKER"] = str(marker)
        try:
            self._run(_fixture())
        finally:
            if old_pythonpath is None:
                os.environ.pop("PYTHONPATH", None)
            else:
                os.environ["PYTHONPATH"] = old_pythonpath
            if old_marker_env is None:
                os.environ.pop("F15_MARKER", None)
            else:
                os.environ["F15_MARKER"] = old_marker_env

        self.assertFalse(
            marker.exists(),
            "F15: the test's own subprocess helper must invoke the hook "
            "with python3 -I -S (production parity) -- sitecustomize.py "
            "ran, proving site processing/PYTHONPATH were not isolated",
        )


if __name__ == "__main__":
    unittest.main()
