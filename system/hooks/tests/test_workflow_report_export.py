"""Tests for system/scripts/workflow-report-export.py (spec-review finding G2).

Loaded via importlib like test_router.py — the script has no package. Covers
the optional mapping file (most-hits wins, ties -> first rule), the file-path
fallback (repository root, never a file's immediate parent), and idempotence
(a second run writes nothing).
"""
from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import os
import re
import shutil
import sys
import tempfile
import unittest
import unittest.mock
import uuid
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[3]
SCRIPT = REPO_ROOT / "system" / "scripts" / "workflow-report-export.py"


def load_script():
    spec = importlib.util.spec_from_file_location("workflow_report_export", SCRIPT)
    mod = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(mod)
    return mod


def _run_export(hex_dir, claude_projects, dry_run=False):
    """Run main() the way the CLI would (including its own crash guard)."""
    m = load_script()
    argv = ["workflow-report-export.py", "--hex-dir", hex_dir, "--claude-projects", claude_projects]
    if dry_run:
        argv.append("--dry-run")
    buf_out, buf_err = io.StringIO(), io.StringIO()
    with contextlib.redirect_stdout(buf_out), contextlib.redirect_stderr(buf_err), unittest.mock.patch.object(
        sys, "argv", argv
    ):
        try:
            rc = m.main()
        except Exception as e:  # mirrors the script's own __main__ guard
            print(f"workflow-report-export: FATAL {type(e).__name__}: {e}", file=sys.stderr)
            rc = 1
    return rc, buf_out.getvalue(), buf_err.getvalue()


def _write_record(claude_projects, rec, project_key="-Users-x-hex", session="sess1"):
    # The exporter only globs "wf_*.json" — the on-disk filename is independent
    # of rec["runId"] (which a test may deliberately make unsafe), so always
    # start it with "wf_" regardless of what runId contains.
    wf_dir = os.path.join(claude_projects, project_key, session, "workflows")
    os.makedirs(wf_dir, exist_ok=True)
    safe_name = re.sub(r"[^A-Za-z0-9_.-]", "_", str(rec.get("runId") or "test"))
    Path(wf_dir, f"wf_{safe_name}.json").write_text(json.dumps(rec))


class RepoRootFallback(unittest.TestCase):
    def setUp(self):
        self.m = load_script()

    def test_file_path_resolves_to_repo_basename_not_immediate_parent(self):
        # G2 repro: the old code returned "src".
        self.assertEqual(self.m.repo_dir_basename("/tmp/acme-repo/src/main.py"), "acme-repo")
        # A monorepo package path without an on-disk .git is NOT resolvable by
        # heuristics alone (packages/<name> looks like a repo) — that case is
        # covered by the on-disk marker test below, which is the real signal.
        self.assertEqual(self.m.repo_dir_basename("/tmp/acme-repo/src/lib/util.py"), "acme-repo")
        self.assertEqual(self.m.repo_dir_basename("/tmp/acme-repo"), "acme-repo")

    def test_on_disk_git_marker_wins(self):
        with tempfile.TemporaryDirectory() as td:
            root = os.path.join(td, "myrepo")
            os.makedirs(os.path.join(root, ".git"))
            os.makedirs(os.path.join(root, "tools", "gen"))
            self.assertEqual(self.m.repo_dir_basename(os.path.join(root, "tools", "gen", "x.py")), "myrepo")
            # a worktree-style `.git` FILE is a marker too
            wt = os.path.join(td, "wt-repo")
            os.makedirs(os.path.join(wt, "src"))
            Path(os.path.join(wt, ".git")).write_text("gitdir: /elsewhere\n")
            self.assertEqual(self.m.repo_dir_basename(os.path.join(wt, "src", "lib.rs")), "wt-repo")

    def test_clone_layout_host_owner_repo(self):
        self.assertEqual(
            self.m.repo_dir_basename("/tmp/hex-home/github.com/acme/widgets/src/index.ts"), "widgets"
        )

    def test_infer_project_falls_back_to_result_path(self):
        rec = {"result": {"branch": "feat/x", "repo": "/tmp/acme-repo/src/main.py"}}
        self.assertEqual(self.m.infer_project(rec, []), "acme-repo")
        self.assertIsNone(self.m.infer_project({"result": "no paths here"}, []))


class MappingFile(unittest.TestCase):
    def setUp(self):
        self.m = load_script()

    def test_most_hits_wins_and_ties_go_to_first_rule(self):
        rules = [("acme", "acme-project"), ("widgets", "widgets-project")]
        # "widgets" appears twice, "acme" once -> widgets wins
        rec = {"result": "widgets widgets acme", "workflowName": "", "script": ""}
        self.assertEqual(self.m.infer_project(rec, rules), "widgets-project")
        # tie (one each) -> the earlier rule
        rec = {"result": "acme widgets", "workflowName": "", "script": ""}
        self.assertEqual(self.m.infer_project(rec, rules), "acme-project")
        # no rule matches -> path fallback still applies
        rec = {"result": "/tmp/acme-repo/src/main.py", "workflowName": "", "script": ""}
        self.assertEqual(self.m.infer_project(rec, [("zzz", "never")]), "acme-repo")

    def test_load_project_map_reads_ordered_rules(self):
        with tempfile.TemporaryDirectory() as hex_dir:
            cfg = Path(hex_dir) / ".hex" / "config"
            cfg.mkdir(parents=True)
            (cfg / "workflow-projects.toml").write_text(
                '[[map]]\nmatch = "acme"\nproject = "acme-project"\n\n[[map]]\nmatch = "widgets"\nproject = "widgets-project"\n'
            )
            self.assertEqual(
                self.m.load_project_map(hex_dir),
                [("acme", "acme-project"), ("widgets", "widgets-project")],
            )
            self.assertEqual(self.m.load_project_map(os.path.join(hex_dir, "nope")), [])


class Idempotence(unittest.TestCase):
    def test_second_run_writes_nothing(self):
        m = load_script()
        with tempfile.TemporaryDirectory() as td:
            hex_dir = os.path.join(td, "hex")
            projects = os.path.join(td, "claude-projects")
            wf_dir = os.path.join(projects, "-Users-x-hex", "sess1", "workflows")
            os.makedirs(wf_dir)
            os.makedirs(hex_dir)
            rec = {
                "runId": "wf_abc123",
                "workflowName": "review-changes",
                "status": "completed",
                "timestamp": "2026-09-09T12:00:00Z",
                "durationMs": 1234,
                "result": {"repo": "/tmp/acme-repo/src/main.py", "summary": "ok"},
            }
            Path(wf_dir, "wf_abc123.json").write_text(json.dumps(rec))

            def run():
                buf = io.StringIO()
                argv = ["workflow-report-export.py", "--hex-dir", hex_dir, "--claude-projects", projects]
                with contextlib.redirect_stdout(buf), unittest.mock.patch.object(sys, "argv", argv):
                    rc = m.main()
                return rc, buf.getvalue()

            rc1, out1 = run()
            self.assertEqual(rc1, 0, out1)
            self.assertIn("wrote=1", out1)
            reports = list(Path(hex_dir, "projects", "acme-repo", "workflow-reports").glob("*.md"))
            self.assertEqual(len(reports), 1, reports)
            self.assertIn("2026-09-09-review-changes-wf_abc123.md", reports[0].name)

            rc2, out2 = run()
            self.assertEqual(rc2, 0, out2)
            self.assertIn("wrote=0", out2)
            self.assertIn("skipped_existing=1", out2)
            self.assertEqual(
                len(list(Path(hex_dir, "projects").rglob("*.md"))), 1, "second run must not add files"
            )


class RedactionCoverage(unittest.TestCase):
    """F1 — current credential shapes must not survive redact()."""

    def setUp(self):
        self.m = load_script()

    def test_sk_prefixed_variants_with_dashes_and_mixed_alphabet(self):
        warnings: list[str] = []
        for token in [
            "sk-ant-api03-" + "AbCdEfGh12345678901234567890",
            "sk-proj-" + "AbCdEfGh12345678901234567890",
            "sk-live-" + "AbCdEfGh12345678901234567890",
            "sk-test-" + "AbCdEfGh12345678901234567890",
        ]:
            out = self.m.redact(f"token={token}", warnings, "wf_1")
            self.assertNotIn(token, out, f"{token!r} survived redaction")

    def test_github_pat_and_ghx_prefixes(self):
        warnings: list[str] = []
        for token in [
            "github_pat_" + "A" * 22 + "_" + "B" * 59,
            "ghp_" + "A" * 36,
            "gho_" + "A" * 36,
            "ghu_" + "A" * 36,
            "ghs_" + "A" * 36,
            "ghr_" + "A" * 36,
        ]:
            out = self.m.redact(token, warnings, "wf_1")
            self.assertNotIn(token, out, f"{token!r} survived redaction")

    def test_slack_aws_bearer_pit_and_keyvalue_pairs(self):
        warnings: list[str] = []
        cases = [
            "xoxa-1-2-" + "a" * 24,
            "xoxs-1-2-" + "a" * 24,
            "AKIA" + "B" * 16,
            "key=" + "c" * 24,
            "token=" + "d" * 24,
            "password=" + "e" * 24,
            "secret=" + "f" * 24,
        ]
        for token in cases:
            out = self.m.redact(token, warnings, "wf_1")
            self.assertNotIn(token, out, f"{token!r} survived redaction")

    def test_pem_private_key_block(self):
        warnings: list[str] = []
        pem = (
            "-----BEGIN RSA PRIVATE KEY-----\n"
            "MIIEpAIBAAKCAQEAsomefakekeydatahere1234567890\n"
            "-----END RSA PRIVATE KEY-----"
        )
        out = self.m.redact(pem, warnings, "wf_1")
        self.assertNotIn("MIIEpAIBAAKCAQEA", out)


class DestinationAndDiagnosticRedaction(unittest.TestCase):
    """F2 — credential-shaped runId/workflow name must not leak into
    destination filenames, --dry-run stdout, or stderr warnings."""

    def test_credential_shaped_identifiers_not_in_paths_or_diagnostics(self):
        with tempfile.TemporaryDirectory() as td:
            hex_dir = os.path.join(td, "hex")
            projects = os.path.join(td, "claude-projects")
            os.makedirs(hex_dir)
            secret = "sk-ant-api03-" + "A" * 30
            rec = {
                "runId": f"wf_{secret}",
                "workflowName": f"deploy-{secret}",
                "status": "completed",
                "timestamp": "2026-09-09T12:00:00Z",
                "result": {"repo": "/tmp/acme-repo/main.py"},
            }
            _write_record(projects, rec)
            rc, out, err = _run_export(hex_dir, projects, dry_run=True)
            self.assertNotIn(secret, out, "secret leaked on stdout")
            self.assertNotIn(secret, err, "secret leaked on stderr")
            all_names = "\n".join(str(p) for p in Path(hex_dir).rglob("*"))
            self.assertNotIn(secret, all_names, "secret leaked into a destination path")


class ResultTruncationOrdering(unittest.TestCase):
    """F3 — the full rendered result must be redacted before RESULT_CAP truncation."""

    def test_credential_spanning_the_cap_boundary_is_fully_redacted(self):
        m = load_script()
        secret_tail = "S3CR3TVALUEAAAAAAAAAAAAAAAAAAA"  # >=20 chars after "sk-"
        secret = "sk-" + secret_tail
        filler = "z" * (m.RESULT_CAP - 10)
        rec = {
            "runId": "wf_cap1",
            "workflowName": "wf",
            "status": "completed",
            "result": filler + secret,
        }
        warnings: list[str] = []
        report = m.build_report(rec, "/tmp/fake/path.json", warnings)
        self.assertNotIn("S3CR3T", report, "credential fragment survived cap truncation")


class DestinationContainment(unittest.TestCase):
    """F4 — project/runId must be safe path components; the resolved
    destination must stay under $HEX_DIR/projects/, including through
    symlinks."""

    def setUp(self):
        self._cleanup_paths: list[str] = []

    def tearDown(self):
        for p in self._cleanup_paths:
            shutil.rmtree(p, ignore_errors=True)

    def test_absolute_project_from_mapping_does_not_escape_hex_dir(self):
        with tempfile.TemporaryDirectory() as td:
            hex_dir = os.path.join(td, "hex")
            projects = os.path.join(td, "claude-projects")
            os.makedirs(hex_dir)
            escape_root = os.path.join(tempfile.gettempdir(), f"hex-f4-escape-{uuid.uuid4().hex}")
            self._cleanup_paths.append(escape_root)
            cfg = Path(hex_dir, ".hex", "config")
            cfg.mkdir(parents=True)
            (cfg / "workflow-projects.toml").write_text(
                f'[[map]]\nmatch = "trigger"\nproject = "{escape_root}"\n'
            )
            rec = {
                "runId": "wf_esc1",
                "workflowName": "trigger-flow",
                "status": "completed",
                "timestamp": "2026-09-09T12:00:00Z",
                "result": "trigger",
            }
            _write_record(projects, rec)
            _run_export(hex_dir, projects)
            self.assertFalse(os.path.exists(escape_root), "absolute project escaped $HEX_DIR/projects")
            unmapped = list(Path(hex_dir, "projects", "_unmapped", "workflow-reports").glob("*.md"))
            self.assertEqual(len(unmapped), 1, "unsafe project must fall back to _unmapped")

    def test_traversal_project_does_not_escape_hex_dir(self):
        with tempfile.TemporaryDirectory() as td:
            hex_dir = os.path.join(td, "hex")
            projects = os.path.join(td, "claude-projects")
            os.makedirs(hex_dir)
            outside_marker = os.path.join(td, "outside-marker")
            cfg = Path(hex_dir, ".hex", "config")
            cfg.mkdir(parents=True)
            (cfg / "workflow-projects.toml").write_text(
                '[[map]]\nmatch = "trigger"\nproject = "../../outside-marker"\n'
            )
            rec = {
                "runId": "wf_esc2",
                "workflowName": "trigger-flow",
                "status": "completed",
                "timestamp": "2026-09-09T12:00:00Z",
                "result": "trigger",
            }
            _write_record(projects, rec)
            _run_export(hex_dir, projects)
            self.assertFalse(os.path.exists(outside_marker), "traversal project escaped $HEX_DIR")
            unmapped = list(Path(hex_dir, "projects", "_unmapped", "workflow-reports").glob("*.md"))
            self.assertEqual(len(unmapped), 1, "traversal project must fall back to _unmapped")

    def test_separator_in_runid_can_escape_workflow_reports_directory(self):
        with tempfile.TemporaryDirectory() as td:
            hex_dir = os.path.join(td, "hex")
            projects = os.path.join(td, "claude-projects")
            os.makedirs(hex_dir)
            project_dir = os.path.join(hex_dir, "projects", "acme-repo")
            out_dir = os.path.join(project_dir, "workflow-reports")
            # Ledger scenario: "can permit traversal when intermediate directories exist".
            os.makedirs(os.path.join(out_dir, "2026-09-09-wf-sneaky"))
            rec = {
                "runId": "sneaky/../../escape-marker",
                "workflowName": "wf",
                "status": "completed",
                "timestamp": "2026-09-09T12:00:00Z",
                "result": "/tmp/acme-repo/main.py",
            }
            _write_record(projects, rec)
            _run_export(hex_dir, projects)
            escaped_file = os.path.join(project_dir, "escape-marker.md")
            self.assertFalse(
                os.path.exists(escaped_file),
                "runId separator escaped workflow-reports/ into the project directory",
            )

    def test_symlinked_project_dir_pointing_outside_is_rejected(self):
        with tempfile.TemporaryDirectory() as td:
            hex_dir = os.path.join(td, "hex")
            projects = os.path.join(td, "claude-projects")
            outside = os.path.join(td, "outside")
            os.makedirs(hex_dir)
            os.makedirs(os.path.join(hex_dir, "projects"))
            os.makedirs(outside)
            os.symlink(outside, os.path.join(hex_dir, "projects", "linked"))
            cfg = Path(hex_dir, ".hex", "config")
            cfg.mkdir(parents=True)
            (cfg / "workflow-projects.toml").write_text('[[map]]\nmatch = "trigger"\nproject = "linked"\n')
            rec = {
                "runId": "wf_esc4",
                "workflowName": "trigger-flow",
                "status": "completed",
                "timestamp": "2026-09-09T12:00:00Z",
                "result": "trigger",
            }
            _write_record(projects, rec)
            _run_export(hex_dir, projects)
            self.assertEqual(
                list(Path(outside).rglob("*.md")), [], "report written through symlinked project escape"
            )


class ConcurrencySafety(unittest.TestCase):
    """F5 — concurrent exporters must not share a truncatable temp filename."""

    def test_stale_or_concurrent_tmp_file_is_not_clobbered(self):
        with tempfile.TemporaryDirectory() as td:
            hex_dir = os.path.join(td, "hex")
            projects = os.path.join(td, "claude-projects")
            os.makedirs(hex_dir)
            rec = {
                "runId": "wf_conc1",
                "workflowName": "review-changes",
                "status": "completed",
                "timestamp": "2026-09-09T12:00:00Z",
                "result": {"repo": "/tmp/acme-repo/main.py"},
            }
            _write_record(projects, rec)
            out_dir = os.path.join(hex_dir, "projects", "acme-repo", "workflow-reports")
            os.makedirs(out_dir)
            out = os.path.join(out_dir, "2026-09-09-review-changes-wf_conc1.md")
            fixed_tmp = out + ".tmp"
            sentinel = "SENTINEL-FROM-ANOTHER-CONCURRENT-EXPORTER"
            Path(fixed_tmp).write_text(sentinel)
            _run_export(hex_dir, projects)
            self.assertTrue(os.path.exists(fixed_tmp), "the other process's temp file vanished")
            self.assertEqual(
                Path(fixed_tmp).read_text(),
                sentinel,
                "exporter reused/overwrote a fixed-name temp file instead of a unique one",
            )


class PathExtractionEdgeCases(unittest.TestCase):
    """F7 / F16 — skip URLs; reject incomplete/space-broken path matches."""

    def test_url_is_not_treated_as_a_filesystem_path(self):
        m = load_script()
        rec = {"result": "See https://example.com/api/v1 for details"}
        self.assertIsNone(m.infer_project(rec, []))

    def test_space_broken_path_is_not_truncated_to_first_segment(self):
        m = load_script()
        rec = {"result": "/Users/Jane Doe/acme-repo/src/main.py"}
        self.assertNotEqual(m.infer_project(rec, []), "Jane")


class RepoRootInference(unittest.TestCase):
    """F8 / F15 — only strip a confirmed trailing file component; nested
    unknown directories must resolve through recognized source-dir
    boundaries, never to the leaf directory."""

    def test_dotted_directory_is_not_stripped_as_a_filename(self):
        m = load_script()
        self.assertEqual(m.repo_dir_basename("/tmp/service.api"), "service.api")

    def test_nested_non_whitelisted_directory_resolves_to_repo_not_leaf(self):
        m = load_script()
        self.assertEqual(m.repo_dir_basename("/tmp/acme-repo/src/auth/main.py"), "acme-repo")


class MappingSearchesFullScript(unittest.TestCase):
    """F17 — the mapping search must read the full script text, not an
    8,000-character prefix, with correct cross-chunk most-hits counting."""

    def test_match_occurring_only_beyond_the_old_cutoff(self):
        m = load_script()
        script = "x" * 8500 + " acme-widgets"
        rec = {"result": "", "workflowName": "", "script": script}
        self.assertEqual(m.infer_project(rec, [("acme-widgets", "acme-project")]), "acme-project")

    def test_later_occurrences_beyond_cutoff_change_the_winner(self):
        m = load_script()
        script = "acme " + "x" * 8000 + " widgets widgets widgets"
        rec = {"result": "", "workflowName": "", "script": script}
        rules = [("acme", "acme-project"), ("widgets", "widgets-project")]
        self.assertEqual(m.infer_project(rec, rules), "widgets-project")


if __name__ == "__main__":
    unittest.main()
