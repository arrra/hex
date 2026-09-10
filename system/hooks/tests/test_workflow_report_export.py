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

    def test_hyphenated_prose_containing_a_credential_prefix_substring_survives(self):
        # R1 redo regression: the F1 fix's unanchored "sk-[A-Za-z0-9_-]{20,}"
        # alternative also matched mid-word inside ordinary hyphenated prose
        # — "task-review-changes-and-summarize" contains "sk-review..." right
        # after "ta", and "risk-assessment-and-mitigation-plan" contains
        # "sk-assessment..." right after "ri". Neither is a credential.
        #
        # R2 redo regression: the interim "requires a digit" heuristic in
        # _redact_one() still mangled "desk-lamp-and-chair-inventory-2024" —
        # its 20+ char tail after "sk-" (from "de"+"sk-lamp-...") contains the
        # digits in "2024", so the digit check treated it as a credential.
        # The lookbehind fix blocks on "sk-" being glued to a lowercase
        # letter, regardless of digits elsewhere in the match.
        warnings: list[str] = []
        for text in [
            "task-review-changes-and-summarize",
            "risk-assessment-and-mitigation-plan",
            "desk-lamp-and-chair-inventory-2024",
        ]:
            out = self.m.redact(text, warnings, "wf_1")
            self.assertEqual(out, text, f"{text!r} was mangled by redaction")

    def test_all_letter_sk_live_key_without_digits_is_redacted(self):
        # R2 redo regression: the interim "requires a digit" heuristic left a
        # coverage hole — a ledger-shaped all-letter key never gets redacted
        # because it has no digit anywhere. The lookbehind fix has no digit
        # requirement, so this still redacts.
        m = load_script()
        secret = "sk-live-abcdefghijklmnopqrstuvwxyz"
        rec = {
            "runId": "wf_allletter1",
            "workflowName": "wf",
            "status": "completed",
            "result": secret,
        }
        report = m.build_report(rec, "/tmp/fake/path.json", [])
        self.assertNotIn(secret, report, "all-letter sk-live- key survived into the report")


class HyphenatedWorkflowNamesSurviveRedaction(unittest.TestCase):
    """F2 regression, end-to-end: an ordinary hyphenated workflow name that
    happens to contain a credential-prefix substring must reach the report
    filename and body intact, not get mangled by the anchored SECRET_RE."""

    def test_ordinary_hyphenated_workflow_name_not_redacted_in_report(self):
        with tempfile.TemporaryDirectory() as td:
            hex_dir = os.path.join(td, "hex")
            projects = os.path.join(td, "claude-projects")
            os.makedirs(hex_dir)
            name = "task-review-changes-and-summarize"
            rec = {
                "runId": "wf_plain1",
                "workflowName": name,
                "status": "completed",
                "timestamp": "2026-09-09T12:00:00Z",
                "result": {"summary": "ok"},
            }
            _write_record(projects, rec)
            rc, out, err = _run_export(hex_dir, projects)
            self.assertEqual(rc, 0, err)
            reports = list(Path(hex_dir, "projects").rglob("*.md"))
            self.assertEqual(len(reports), 1, reports)
            self.assertIn(name, reports[0].name, "ordinary hyphenated name mangled in filename")
            content = reports[0].read_text()
            self.assertNotIn("[REDACTED]", content, "ordinary hyphenated workflow name got redacted")


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
                "result": {"repo": "/tmp/acme-repo/src/main.py"},
            }
            _write_record(projects, rec)
            rc, out, err = _run_export(hex_dir, projects, dry_run=True)
            self.assertNotIn(secret, out, "secret leaked on stdout")
            self.assertNotIn(secret, err, "secret leaked on stderr")
            all_names = "\n".join(str(p) for p in Path(hex_dir).rglob("*"))
            self.assertNotIn(secret, all_names, "secret leaked into a destination path")

    def test_invalid_record_fallback_id_redacts_credential_shaped_filename(self):
        # review_b G1: the invalid-record warning path builds `fallback_id`
        # straight from the on-disk filename (which the harness names after
        # runId) without ever passing it through redact().
        with tempfile.TemporaryDirectory() as td:
            hex_dir = os.path.join(td, "hex")
            projects = os.path.join(td, "claude-projects")
            os.makedirs(hex_dir)
            secret = "sk-ant-" + "B" * 30
            wf_dir = os.path.join(projects, "-Users-x-hex", "sess1", "workflows")
            os.makedirs(wf_dir)
            Path(wf_dir, f"wf_{secret}.json").write_text("null")
            rc, out, err = _run_export(hex_dir, projects)
            self.assertEqual(rc, 1, err)
            self.assertIn("invalid record", err)
            self.assertNotIn(secret, err, "credential-shaped filename leaked into the invalid-record warning")

    def test_missing_explicit_root_warning_redacts_credential_shaped_path(self):
        # review_b G1: the missing-root warning interpolates the raw
        # --claude-projects value without redact().
        with tempfile.TemporaryDirectory() as td:
            hex_dir = os.path.join(td, "hex")
            os.makedirs(hex_dir)
            secret = "sk-ant-" + "C" * 30
            missing_root = os.path.join(td, f"{secret}-claude-projects")
            rc, out, err = _run_export(hex_dir, missing_root)
            self.assertEqual(rc, 1, err)
            self.assertIn("does not exist", err)
            self.assertNotIn(secret, err, "credential-shaped root path leaked into the missing-root warning")


class ResultTruncationOrdering(unittest.TestCase):
    """F3 — the full rendered result must be redacted before RESULT_CAP truncation."""

    def test_credential_spanning_the_cap_boundary_is_fully_redacted(self):
        m = load_script()
        secret_tail = "S3CR3TVALUEAAAAAAAAAAAAAAAAAAA"  # >=20 chars after "sk-"
        secret = "sk-" + secret_tail
        # R2 redo (F2): SECRET_RE now requires "sk-" not be glued to another
        # letter/digit (so "task-"/"risk-"/"desk-" prose survives). The filler
        # must therefore end on a non-alnum separator rather than a bare "z",
        # or the boundary token would fail to match for the same reason a
        # real prose word would — unrelated to what this test is checking
        # (redact-before-truncate ordering at the cap boundary).
        filler = "z" * (m.RESULT_CAP - 11) + "_"
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
                "result": "/tmp/acme-repo/src/main.py",
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
                "result": {"repo": "/tmp/acme-repo/src/main.py"},
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

    def test_multi_space_name_is_not_truncated_to_first_segment(self):
        # review_b G1: a name with more than one space ("Jane Doe Smith")
        # still slipped past the old one-space-ahead check in
        # _looks_truncated_by_space, so the truncated "/Users/Jane" prefix
        # was accepted as a complete path.
        m = load_script()
        rec = {"result": "/Users/Jane Doe Smith/acme-repo/src/main.py"}
        self.assertNotEqual(m.infer_project(rec, []), "Jane")

    def test_structured_repo_field_wins_over_unrelated_path_in_free_text(self):
        # review_b G1 (round 2): a `result` dict can carry a path-shaped
        # string in an unrelated field (e.g. "summary") that happens to sit
        # earlier in the JSON dump than the actual structured "repo" field.
        # Scanning the whole blob as free text picks whichever path comes
        # first in the dump, not the one the record actually identifies as
        # the repo. The structured field must be inspected first.
        m = load_script()
        rec = {
            "result": {
                "summary": "see /tmp/wrong-repo/src/main.py for context",
                "repo": "/tmp/right-repo/src/main.py",
            }
        }
        self.assertEqual(m.infer_project(rec, []), "right-repo")


class RepoRootInference(unittest.TestCase):
    """F8 / F15 — only strip a confirmed trailing file component (parent
    segment is a recognized source/non-repo directory); nested unknown
    directories must resolve through recognized source-dir boundaries, never
    to the leaf directory or an unrelated ancestor."""

    def test_dotted_directory_is_not_stripped_as_a_filename(self):
        m = load_script()
        # A bare dotted directory right under a generic ancestor ("tmp" is
        # not a recognized boundary) is ambiguous — it must never be treated
        # as a stray file and popped down to its parent, e.g. never "tmp".
        self.assertIsNone(m.repo_dir_basename("/tmp/service.api"))

    def test_nested_non_whitelisted_directory_resolves_to_repo_not_leaf(self):
        m = load_script()
        self.assertEqual(m.repo_dir_basename("/tmp/acme-repo/src/auth/main.py"), "acme-repo")

    def test_dotted_leaf_under_arbitrary_directory_never_becomes_that_directory(self):
        # R1 redo counter-example: repo_dir_basename("/home/x/service.api")
        # returned "x" and ("/Users/sagar/work/service.api") returned "work"
        # — the old depth heuristic (len(work) > 3) let a dotted leaf pop
        # down to whatever ancestor happened to be there. Neither "x" nor
        # "work" is a recognized source/non-repo directory, so the trailing
        # segment must be left alone; with no boundary found and the tail
        # still file-shaped, this must resolve to None (_unmapped).
        m = load_script()
        self.assertIsNone(m.repo_dir_basename("/home/x/service.api"))
        self.assertIsNone(m.repo_dir_basename("/Users/sagar/work/service.api"))

    def test_unbounded_nested_directory_is_unmapped_never_the_leaf(self):
        # R1 redo counter-example: repo_dir_basename("/tmp/acme-repo/auth/main.py")
        # returned "auth" because no NON_REPO_DIRS boundary exists in the
        # path. Per the contract ("never auth"), an unresolved nested path
        # without a recognized boundary must go _unmapped (None), not guess
        # the immediate leaf directory.
        m = load_script()
        self.assertIsNone(m.repo_dir_basename("/tmp/acme-repo/auth/main.py"))

    def test_container_boundary_before_repo_name_does_not_win_over_the_real_boundary(self):
        # review_b G3: the old leftmost-boundary scan stopped at the FIRST
        # NON_REPO_DIRS segment it saw. In a container layout like
        # "/workspace/src/acme-repo/src/main.py" that first hit is the outer
        # "src" (a container prefix before the repo name), so it resolved to
        # "workspace" instead of the real repo root "acme-repo" anchored by
        # the second "src", the one immediately before the file.
        m = load_script()
        self.assertEqual(m.repo_dir_basename("/workspace/src/acme-repo/src/main.py"), "acme-repo")


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


class RunIdCollisionSafety(unittest.TestCase):
    """review_b G2 — redact() turns any credential-shaped runId into the same
    literal "[REDACTED]" string, so two distinct credential-shaped runIds
    collided on one filename and the second run was silently skipped as
    "already exported". Each run must still land its own report."""

    def test_distinct_credential_shaped_run_ids_do_not_collide(self):
        with tempfile.TemporaryDirectory() as td:
            hex_dir = os.path.join(td, "hex")
            projects = os.path.join(td, "claude-projects")
            os.makedirs(hex_dir)
            secret_a = "sk-ant-" + "A" * 25
            secret_b = "sk-ant-" + "B" * 25
            for i, secret in enumerate([secret_a, secret_b]):
                rec = {
                    "runId": f"wf_{secret}",
                    "workflowName": "wf",
                    "status": "completed",
                    "timestamp": "2026-09-09T12:00:00Z",
                    "result": {"repo": "/tmp/acme-repo/src/main.py"},
                }
                _write_record(projects, rec, session=f"sess{i}")

            rc, out, err = _run_export(hex_dir, projects)
            self.assertEqual(rc, 0, err)
            self.assertIn("wrote=2", out, out)
            reports = list(Path(hex_dir, "projects").rglob("*.md"))
            self.assertEqual(len(reports), 2, reports)
            self.assertEqual(
                len({p.name for p in reports}), 2, "two distinct runIds collided on one filename"
            )
            for name in (p.name for p in reports):
                self.assertNotIn(secret_a, name)
                self.assertNotIn(secret_b, name)


class RecordScopedFailureHandling(unittest.TestCase):
    """F6/F18 — a per-record validation or write failure must not abort the
    scan for unrelated records: the run continues, temp files are not left
    behind, and the final exit status/aggregate counters reflect the
    failure. Each case pairs one broken record with a later valid, writable
    one and asserts the valid record's report still lands."""

    def test_null_record_does_not_abort_a_later_valid_record(self):
        with tempfile.TemporaryDirectory() as td:
            hex_dir = os.path.join(td, "hex")
            projects = os.path.join(td, "claude-projects")
            os.makedirs(hex_dir)
            wf_dir = os.path.join(projects, "-Users-x-hex", "sess1", "workflows")
            os.makedirs(wf_dir)
            Path(wf_dir, "wf_a_null.json").write_text("null")  # sorts before the valid record
            valid = {
                "runId": "wf_z_valid1",
                "workflowName": "wf",
                "status": "completed",
                "timestamp": "2026-09-09T12:00:00Z",
                "result": {"repo": "/tmp/acme-repo/src/main.py"},
            }
            Path(wf_dir, "wf_z_valid1.json").write_text(json.dumps(valid))
            rc, out, err = _run_export(hex_dir, projects)
            self.assertEqual(rc, 1, "a malformed (null) record must fail the run")
            reports = list(Path(hex_dir, "projects", "acme-repo", "workflow-reports").glob("*.md"))
            self.assertEqual(len(reports), 1, "the later valid record must still be written")
            self.assertIn("wf_a_null", err, "the malformed record must be named on stderr, not swallowed")

    def test_numeric_summary_field_does_not_abort_a_later_valid_record(self):
        with tempfile.TemporaryDirectory() as td:
            hex_dir = os.path.join(td, "hex")
            projects = os.path.join(td, "claude-projects")
            os.makedirs(hex_dir)
            wf_dir = os.path.join(projects, "-Users-x-hex", "sess1", "workflows")
            os.makedirs(wf_dir)
            bad = {
                "runId": "wf_a_numsummary",
                "workflowName": "wf",
                "status": "completed",
                "timestamp": "2026-09-09T12:00:00Z",
                "summary": 12345,
                "result": {"repo": "/tmp/acme-repo/src/main.py"},
            }
            Path(wf_dir, "wf_a_numsummary.json").write_text(json.dumps(bad))
            valid = {
                "runId": "wf_z_valid2",
                "workflowName": "wf",
                "status": "completed",
                "timestamp": "2026-09-09T12:00:00Z",
                "result": {"repo": "/tmp/acme-repo/src/main.py"},
            }
            Path(wf_dir, "wf_z_valid2.json").write_text(json.dumps(valid))
            rc, out, err = _run_export(hex_dir, projects)
            self.assertEqual(rc, 1, "a record with a malformed field (numeric summary) must fail the run")
            reports = list(Path(hex_dir, "projects", "acme-repo", "workflow-reports").glob("*.md"))
            self.assertEqual(len(reports), 1, "the later valid record must still be written")
            self.assertIn("wf_a_numsummary", err, "the malformed record must be named on stderr")

    def test_nonlist_logs_field_is_a_validation_failure_not_a_garbled_report(self):
        with tempfile.TemporaryDirectory() as td:
            hex_dir = os.path.join(td, "hex")
            projects = os.path.join(td, "claude-projects")
            os.makedirs(hex_dir)
            wf_dir = os.path.join(projects, "-Users-x-hex", "sess1", "workflows")
            os.makedirs(wf_dir)
            bad = {
                "runId": "wf_a_strlogs",
                "workflowName": "wf",
                "status": "completed",
                "timestamp": "2026-09-09T12:00:00Z",
                "logs": "not-a-list",
                "result": {"repo": "/tmp/acme-repo/src/main.py"},
            }
            Path(wf_dir, "wf_a_strlogs.json").write_text(json.dumps(bad))
            rc, out, err = _run_export(hex_dir, projects)
            self.assertEqual(rc, 1, "a non-list logs field must be rejected, not silently rendered")
            reports = list(Path(hex_dir, "projects").rglob("*.md"))
            self.assertEqual(reports, [], "a record failing structural validation must not produce a report")
            self.assertIn("wf_a_strlogs", err, "the malformed record must be named on stderr")

    def test_unwritable_record_does_not_abort_the_scan(self):
        with tempfile.TemporaryDirectory() as td:
            hex_dir = os.path.join(td, "hex")
            projects = os.path.join(td, "claude-projects")
            os.makedirs(hex_dir)
            wf_dir = os.path.join(projects, "-Users-x-hex", "sess1", "workflows")
            os.makedirs(wf_dir)
            unwritable = {
                "runId": "wf_a_unwritable",
                "workflowName": "wf",
                "status": "completed",
                "timestamp": "2026-09-09T12:00:00Z",
                "result": {"repo": "/tmp/blockedproj/src/main.py"},
            }
            Path(wf_dir, "wf_a_unwritable.json").write_text(json.dumps(unwritable))
            valid = {
                "runId": "wf_z_valid3",
                "workflowName": "wf",
                "status": "completed",
                "timestamp": "2026-09-09T12:00:00Z",
                "result": {"repo": "/tmp/acme-repo/src/main.py"},
            }
            Path(wf_dir, "wf_z_valid3.json").write_text(json.dumps(valid))
            # A regular FILE named "blockedproj" where a project directory is
            # expected makes os.makedirs(out_dir) raise OSError for that one
            # record's write, without depending on filesystem permissions.
            proj_root = os.path.join(hex_dir, "projects")
            os.makedirs(proj_root)
            Path(proj_root, "blockedproj").write_text("occupying the project directory slot")
            rc, out, err = _run_export(hex_dir, projects)
            self.assertEqual(rc, 1, "an OSError writing one record must fail the run")
            reports = list(Path(hex_dir, "projects", "acme-repo", "workflow-reports").glob("*.md"))
            self.assertEqual(len(reports), 1, "the later writable record must still be written")
            leftover_tmp = [str(p) for p in Path(hex_dir).rglob("*") if ".tmp" in p.name]
            self.assertEqual(leftover_tmp, [], f"temp files left behind after a write failure: {leftover_tmp}")


    def test_numeric_runid_field_does_not_abort_a_later_valid_record(self):
        # review r1 F1: a non-string runId (e.g. the harness ever emits a raw
        # numeric id) reached redact() unchecked and raised TypeError, which
        # only the *outer* try/except in the test harness's own crash guard
        # caught -- aborting the scan before the later valid record was ever
        # reached. validate_record() must reject this per-record instead.
        with tempfile.TemporaryDirectory() as td:
            hex_dir = os.path.join(td, "hex")
            projects = os.path.join(td, "claude-projects")
            os.makedirs(hex_dir)
            wf_dir = os.path.join(projects, "-Users-x-hex", "sess1", "workflows")
            os.makedirs(wf_dir)
            bad = {
                "runId": 123,
                "workflowName": "wf",
                "status": "completed",
                "timestamp": "2026-09-09T12:00:00Z",
                "result": {"repo": "/tmp/acme-repo/src/main.py"},
            }
            Path(wf_dir, "wf_a_numericrunid.json").write_text(json.dumps(bad))
            valid = {
                "runId": "wf_z_valid4",
                "workflowName": "wf",
                "status": "completed",
                "timestamp": "2026-09-09T12:00:00Z",
                "result": {"repo": "/tmp/acme-repo/src/main.py"},
            }
            Path(wf_dir, "wf_z_valid4.json").write_text(json.dumps(valid))
            rc, out, err = _run_export(hex_dir, projects)
            self.assertEqual(rc, 1, "a record with a malformed field (numeric runId) must fail the run")
            reports = list(Path(hex_dir, "projects", "acme-repo", "workflow-reports").glob("*.md"))
            self.assertEqual(len(reports), 1, "the later valid record must still be written")
            self.assertIn("wf_a_numericrunid", err, "the malformed record must be named on stderr")

    def test_numeric_phases_field_does_not_abort_a_later_valid_record(self):
        # review r1 F1: a non-list `phases` reached build_report() unchecked
        # (`phases or []` treats a truthy int as-is) and raised TypeError
        # iterating it, escaping the write-only OSError guard entirely.
        with tempfile.TemporaryDirectory() as td:
            hex_dir = os.path.join(td, "hex")
            projects = os.path.join(td, "claude-projects")
            os.makedirs(hex_dir)
            wf_dir = os.path.join(projects, "-Users-x-hex", "sess1", "workflows")
            os.makedirs(wf_dir)
            bad = {
                "runId": "wf_a_numericphases",
                "workflowName": "wf",
                "status": "completed",
                "timestamp": "2026-09-09T12:00:00Z",
                "phases": 7,
                "result": {"repo": "/tmp/acme-repo/src/main.py"},
            }
            Path(wf_dir, "wf_a_numericphases.json").write_text(json.dumps(bad))
            valid = {
                "runId": "wf_z_valid5",
                "workflowName": "wf",
                "status": "completed",
                "timestamp": "2026-09-09T12:00:00Z",
                "result": {"repo": "/tmp/acme-repo/src/main.py"},
            }
            Path(wf_dir, "wf_z_valid5.json").write_text(json.dumps(valid))
            rc, out, err = _run_export(hex_dir, projects)
            self.assertEqual(rc, 1, "a record with a malformed field (numeric phases) must fail the run")
            reports = list(Path(hex_dir, "projects", "acme-repo", "workflow-reports").glob("*.md"))
            self.assertEqual(len(reports), 1, "the later valid record must still be written")
            self.assertIn("wf_a_numericphases", err, "the malformed record must be named on stderr")


class MissingSourceRootDiagnostics(unittest.TestCase):
    """F9 — an explicitly supplied --claude-projects root that does not exist
    must be diagnosed (WARN + non-zero exit), not read as a healthy empty
    scan; the default root missing must stay a quiet empty scan."""

    def test_explicit_missing_root_is_diagnosed_not_a_quiet_empty_scan(self):
        with tempfile.TemporaryDirectory() as td:
            hex_dir = os.path.join(td, "hex")
            os.makedirs(hex_dir)
            missing_root = os.path.join(td, "does-not-exist-claude-projects")
            rc, out, err = _run_export(hex_dir, missing_root)
            self.assertNotEqual(rc, 0, "a missing explicit --claude-projects root must fail, not exit 0")
            self.assertIn(missing_root, err, "the missing root path must be named on stderr")

    def test_default_root_missing_stays_a_quiet_empty_scan(self):
        with tempfile.TemporaryDirectory() as td:
            hex_dir = os.path.join(td, "hex")
            os.makedirs(hex_dir)
            fake_home = os.path.join(td, "fake-home")
            os.makedirs(fake_home)  # ~/.claude/projects under this does not exist
            m = load_script()
            argv = ["workflow-report-export.py", "--hex-dir", hex_dir]
            buf_out, buf_err = io.StringIO(), io.StringIO()
            with contextlib.redirect_stdout(buf_out), contextlib.redirect_stderr(
                buf_err
            ), unittest.mock.patch.object(sys, "argv", argv), unittest.mock.patch.dict(
                os.environ, {"HOME": fake_home}, clear=False
            ):
                rc = m.main()
            self.assertEqual(rc, 0, "a missing DEFAULT root must stay a quiet, successful empty scan")
            self.assertEqual(buf_err.getvalue(), "", "the default root missing must not warn")


class InvalidMappingRulesRejected(unittest.TestCase):
    """F11 — mapping rules missing match/project, or with non-string values,
    must be rejected (rule index + field named) instead of silently dropped
    or coerced; invalid config must fail the run."""

    def test_rule_missing_project_field_is_rejected_not_silently_dropped(self):
        with tempfile.TemporaryDirectory() as hex_dir:
            cfg = Path(hex_dir) / ".hex" / "config"
            cfg.mkdir(parents=True)
            (cfg / "workflow-projects.toml").write_text(
                '[[map]]\nmatch = "acme"\nproject = "acme-project"\n\n'
                '[[map]]\nmatch = "widgets"\nproject_typo = "widgets-project"\n'
            )
            m = load_script()
            with self.assertRaises(Exception) as ctx:
                m.load_project_map(hex_dir)
            msg = str(ctx.exception)
            self.assertIn("project", msg, "the missing field name must be named in the error")
            self.assertRegex(msg, r"\b[12]\b", "the offending rule's index must be named in the error")

    def test_rule_with_non_string_match_is_rejected_not_silently_coerced(self):
        with tempfile.TemporaryDirectory() as hex_dir:
            cfg = Path(hex_dir) / ".hex" / "config"
            cfg.mkdir(parents=True)
            (cfg / "workflow-projects.toml").write_text('[[map]]\nmatch = 123\nproject = "num-project"\n')
            m = load_script()
            with self.assertRaises(Exception) as ctx:
                m.load_project_map(hex_dir)
            msg = str(ctx.exception)
            self.assertIn("match", msg, "the offending field name must be named in the error")

    def test_non_table_rule_is_rejected_naming_the_rule_index(self):
        # review r1 F2 (round-2 finding): `[[map]]` entries that aren't
        # tables (e.g. a bare string) reached entry.get("match") unchecked
        # and raised AttributeError naming neither rule index nor field.
        with tempfile.TemporaryDirectory() as hex_dir:
            cfg = Path(hex_dir) / ".hex" / "config"
            cfg.mkdir(parents=True)
            (cfg / "workflow-projects.toml").write_text('map = ["acme"]\n')
            m = load_script()
            with self.assertRaises(Exception) as ctx:
                m.load_project_map(hex_dir)
            msg = str(ctx.exception)
            self.assertNotIsInstance(ctx.exception, AttributeError, "must be a diagnosed error, not a raw crash")
            self.assertRegex(msg, r"\b1\b", "the offending rule's index must be named in the error")

    def test_invalid_mapping_config_fails_the_whole_run(self):
        with tempfile.TemporaryDirectory() as td:
            hex_dir = os.path.join(td, "hex")
            projects = os.path.join(td, "claude-projects")
            os.makedirs(hex_dir)
            cfg = Path(hex_dir) / ".hex" / "config"
            cfg.mkdir(parents=True)
            (cfg / "workflow-projects.toml").write_text('[[map]]\nmatch = "acme"\nproject_typo = "acme-project"\n')
            rec = {
                "runId": "wf_1",
                "workflowName": "wf",
                "status": "completed",
                "timestamp": "2026-09-09T12:00:00Z",
                "result": "acme",
            }
            _write_record(projects, rec)
            rc, out, err = _run_export(hex_dir, projects)
            self.assertNotEqual(rc, 0, "an invalid mapping file must fail the run, not silently drop the rule")


class TerminalStateAndFailureBranchCoverage(unittest.TestCase):
    """F14 — independent end-to-end coverage for excluding running records,
    including failed/killed records, the _unmapped + WARN path, and
    continuing after unparsable JSON with exit 1. Separate from the
    idempotence happy path in `Idempotence` above."""

    def test_running_record_is_excluded_no_report_written(self):
        with tempfile.TemporaryDirectory() as td:
            hex_dir = os.path.join(td, "hex")
            projects = os.path.join(td, "claude-projects")
            os.makedirs(hex_dir)
            rec = {
                "runId": "wf_running1",
                "workflowName": "wf",
                "status": "running",
                "timestamp": "2026-09-09T12:00:00Z",
                "result": {"repo": "/tmp/acme-repo/src/main.py"},
            }
            _write_record(projects, rec)
            rc, out, err = _run_export(hex_dir, projects)
            self.assertEqual(rc, 0, err)
            self.assertIn("nonterminal=1", out)
            self.assertEqual(
                list(Path(hex_dir, "projects").rglob("*.md")), [], "a running record must not be exported"
            )

    def test_failed_and_killed_records_are_included(self):
        with tempfile.TemporaryDirectory() as td:
            hex_dir = os.path.join(td, "hex")
            projects = os.path.join(td, "claude-projects")
            os.makedirs(hex_dir)
            for status, key in (("failed", "fail1"), ("killed", "kill1")):
                rec = {
                    "runId": f"wf_{key}",
                    "workflowName": "wf",
                    "status": status,
                    "timestamp": "2026-09-09T12:00:00Z",
                    "result": {"repo": "/tmp/acme-repo/src/main.py"},
                }
                _write_record(projects, rec, session=key)
            rc, out, err = _run_export(hex_dir, projects)
            self.assertEqual(rc, 0, err)
            self.assertIn("wrote=2", out)
            reports = {p.name for p in Path(hex_dir, "projects", "acme-repo", "workflow-reports").glob("*.md")}
            self.assertTrue(any("wf_fail1" in n for n in reports), reports)
            self.assertTrue(any("wf_kill1" in n for n in reports), reports)

    def test_unmappable_record_lands_in_unmapped_with_a_warning(self):
        with tempfile.TemporaryDirectory() as td:
            hex_dir = os.path.join(td, "hex")
            projects = os.path.join(td, "claude-projects")
            os.makedirs(hex_dir)
            rec = {
                "runId": "wf_nopath1",
                "workflowName": "wf",
                "status": "completed",
                "timestamp": "2026-09-09T12:00:00Z",
                "result": "no filesystem path anywhere in here",
            }
            _write_record(projects, rec)
            rc, out, err = _run_export(hex_dir, projects)
            self.assertEqual(rc, 0, err)
            self.assertIn("unmapped=1", out)
            self.assertIn("wf_nopath1", err)
            self.assertIn("_unmapped", err)
            reports = list(Path(hex_dir, "projects", "_unmapped", "workflow-reports").glob("*.md"))
            self.assertEqual(len(reports), 1, reports)

    def test_unparsable_json_is_reported_and_the_run_continues_with_exit_1(self):
        with tempfile.TemporaryDirectory() as td:
            hex_dir = os.path.join(td, "hex")
            projects = os.path.join(td, "claude-projects")
            os.makedirs(hex_dir)
            wf_dir = os.path.join(projects, "-Users-x-hex", "sess1", "workflows")
            os.makedirs(wf_dir)
            Path(wf_dir, "wf_a_badjson.json").write_text("{not valid json")
            valid = {
                "runId": "wf_z_valid4",
                "workflowName": "wf",
                "status": "completed",
                "timestamp": "2026-09-09T12:00:00Z",
                "result": {"repo": "/tmp/acme-repo/src/main.py"},
            }
            Path(wf_dir, "wf_z_valid4.json").write_text(json.dumps(valid))
            rc, out, err = _run_export(hex_dir, projects)
            self.assertEqual(rc, 1, "unparsable JSON must fail the overall run")
            self.assertIn("wf_a_badjson", err)
            self.assertIn("unreadable=1", out)
            reports = list(Path(hex_dir, "projects", "acme-repo", "workflow-reports").glob("*.md"))
            self.assertEqual(len(reports), 1, "the run must continue past the unparsable record")


if __name__ == "__main__":
    unittest.main()
