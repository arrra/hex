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
            [sys.executable, str(SCRIPT)],
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


if __name__ == "__main__":
    unittest.main()
