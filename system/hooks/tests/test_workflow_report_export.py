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
import sys
import tempfile
import unittest
import unittest.mock
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[3]
SCRIPT = REPO_ROOT / "system" / "scripts" / "workflow-report-export.py"


def load_script():
    spec = importlib.util.spec_from_file_location("workflow_report_export", SCRIPT)
    mod = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(mod)
    return mod


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
            self.m.repo_dir_basename("/Users/x/github.com/acme/widgets/src/index.ts"), "widgets"
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


if __name__ == "__main__":
    unittest.main()
