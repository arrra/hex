#!/usr/bin/env bash
# Logic-free shim: the PreToolUse dispatch-lint hook lives in the typed,
# tested harness (`hex hook lint-predispatch` — see src/hook/lint_predispatch.rs).
# This script only resolves the binary and execs it; stdin passes through and
# the subcommand's exit code is the hook's exit code. Zero python (2026-06-10).
set -u

bin="${HEX_DIR:-$HOME/hex}/.hex/bin/hex"
if [ ! -x "$bin" ]; then
  bin="$(command -v hex || true)"
fi
if [ -z "${bin:-}" ] || [ ! -x "$bin" ]; then
  printf 'lint-predispatch shim: hex binary not found; allowing dispatch (degraded)\n' >&2
  exit 0
fi

exec "$bin" hook lint-predispatch
