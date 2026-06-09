#!/usr/bin/env bats
# Red tests for T1wjfnvj9 — migration of headless claude call sites to
# claude_runs::resolve / `hex claude-flags <profile>` / claude_lean().
#
# These assert the call-site migration only; the claude_runs module and
# `hex claude-flags` subcommand are covered by sibling task tests.

setup() {
  REPO_ROOT="$(cd "$(dirname "$BATS_TEST_FILENAME")/.." && pwd)"
}

@test "worker/run.rs uses claude_runs::resolve(\"harness_worker\")" {
  run grep -E 'claude_runs::resolve\("harness_worker"\)|claude_runs::resolve\(.harness_worker.\)' \
      "$REPO_ROOT/system/harness/src/worker/run.rs"
  [ "$status" -eq 0 ]
}

@test "worker/run.rs references claude_runs module" {
  run grep -q 'claude_runs' "$REPO_ROOT/system/harness/src/worker/run.rs"
  [ "$status" -eq 0 ]
}

@test "meeting-prep.sh invokes \`hex claude-flags meeting_prep\`" {
  run grep -E 'hex[[:space:]]+claude-flags[[:space:]]+meeting_prep' \
      "$REPO_ROOT/system/scripts/meeting-prep.sh"
  [ "$status" -eq 0 ]
}

@test "env.sh defines a claude_lean shell function" {
  run grep -E '^[[:space:]]*claude_lean[[:space:]]*\(\)' \
      "$REPO_ROOT/system/scripts/env.sh"
  [ "$status" -eq 0 ]
}

@test "run_eval.py prepends \`hex claude-flags eval\` when hex is on PATH" {
  run grep -E 'claude-flags[[:space:]]+eval|claude-flags.*eval' \
      "$REPO_ROOT/tests/eval/run_eval.py"
  [ "$status" -eq 0 ]
}
