#!/usr/bin/env bash
# test_env_resolution.sh — E2E tests for env.sh, path resolution, and native hex commands.
#
# Validates the current (post-rustification) installed shape:
#   1. env.sh exists after install and is executable
#   2. HEX_DIR / AGENT_DIR are set after sourcing env.sh
#   3. PATH composition is delegated to `hex env path` correctly
#   4. hex binary is installed and functional (replaces hex-agent-spawn.sh)
#   5. CLAUDE.md template references binaries, not paths
#   6. hex doctor native command works (replaces verify-agent-infra.sh + doctor.sh)
#   7. Old shell/Python scripts are absent (replaced by native hex commands)
#   8. Version is reconciled — Cargo.toml is the canonical source
#
# Usage:
#   bash test_env_resolution.sh                   # Run against local checkout
#   docker build -f tests/Dockerfile.env -t hex-env-test . && docker run hex-env-test

set -uo pipefail

PASS=0
FAIL=0
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

red()   { printf '\033[31mFAIL: %s\033[0m\n' "$*"; }
green() { printf '\033[32mPASS: %s\033[0m\n' "$*"; }
bold()  { printf '\033[1m%s\033[0m\n' "$*"; }

assert_pass() { PASS=$((PASS + 1)); green "$1"; }
assert_fail() { FAIL=$((FAIL + 1)); red "$1"; }

# Locate the hex binary: prefer the installed copy, fall back to system PATH.
find_hex() {
  local install_dir="$1"
  if [ -x "$install_dir/.hex/bin/hex" ]; then
    echo "$install_dir/.hex/bin/hex"
  elif type -P hex &>/dev/null; then
    type -P hex
  else
    echo ""
  fi
}

# ── Setup: run install to a temp dir ────────────────────────────────────────
INSTALL_BASE=$(mktemp -d /tmp/hex-env-test-XXXXXX)
INSTALL_DIR="$INSTALL_BASE/hex"
trap 'rm -rf "$INSTALL_BASE"' EXIT

bold "══ hex Environment Resolution Tests ══"
echo "Install dir: $INSTALL_DIR"
echo ""

# ── Test 0: Install succeeds ────────────────────────────────────────────────
bold "── Install ──"
if bash "$REPO_DIR/install.sh" "$INSTALL_DIR" 2>&1 | tail -5; then
  assert_pass "install.sh completed without errors"
else
  assert_fail "install.sh failed"
fi
echo ""

HEX_BIN="$(find_hex "$INSTALL_DIR")"

# ── Test 1: env.sh exists and is executable ─────────────────────────────────
bold "── env.sh ──"
if [ -f "$INSTALL_DIR/.hex/scripts/env.sh" ]; then
  assert_pass "env.sh exists at .hex/scripts/env.sh"
else
  assert_fail "env.sh missing from .hex/scripts/env.sh"
fi

if [ -x "$INSTALL_DIR/.hex/scripts/env.sh" ]; then
  assert_pass "env.sh is executable"
else
  assert_fail "env.sh is not executable"
fi

# ── Test 2: Sourcing env.sh sets HEX_DIR and AGENT_DIR ─────────────────────
bold "── HEX_DIR / AGENT_DIR ──"
if [ -n "$HEX_BIN" ]; then
  # Full source: env.sh bootstrap + hex env path delegation
  ENV_OUT=$(HEX_DIR="$INSTALL_DIR" PATH="$(dirname "$HEX_BIN"):$PATH" \
    bash -c "source '$INSTALL_DIR/.hex/scripts/env.sh' && echo HEX_DIR=\$HEX_DIR AGENT_DIR=\$AGENT_DIR" 2>&1)

  if echo "$ENV_OUT" | grep -q "HEX_DIR=$INSTALL_DIR"; then
    assert_pass "HEX_DIR set correctly after sourcing env.sh"
  else
    assert_fail "HEX_DIR not set correctly: $ENV_OUT"
  fi

  if echo "$ENV_OUT" | grep -q "AGENT_DIR=$INSTALL_DIR"; then
    assert_pass "AGENT_DIR set correctly (mirrors HEX_DIR)"
  else
    assert_fail "AGENT_DIR not set: $ENV_OUT"
  fi

  # Auto-detection (no env vars pre-set): env.sh detects HEX_DIR from BASH_SOURCE
  AUTO_OUT=$(PATH="$(dirname "$HEX_BIN"):$PATH" \
    bash -c "unset HEX_DIR; unset AGENT_DIR; source '$INSTALL_DIR/.hex/scripts/env.sh' && echo HEX_DIR=\$HEX_DIR" 2>&1)
  if echo "$AUTO_OUT" | grep -q "HEX_DIR="; then
    if echo "$AUTO_OUT" | grep -q "HEX_DIR=$"; then
      assert_fail "HEX_DIR auto-detection produced empty value"
    else
      assert_pass "HEX_DIR auto-detected from script location"
    fi
  else
    assert_fail "HEX_DIR auto-detection failed: $AUTO_OUT"
  fi
else
  assert_fail "hex binary not available — cannot test env.sh sourcing"
fi

# ── Test 3: PATH composition via `hex env path` ─────────────────────────────
bold "── PATH (hex env path) ──"
if [ -n "$HEX_BIN" ]; then
  PATH_OUT=$(HEX_DIR="$INSTALL_DIR" "$HEX_BIN" env path 2>&1)

  if [ -n "$PATH_OUT" ]; then
    assert_pass "hex env path produces non-empty PATH"
  else
    assert_fail "hex env path returned empty string"
  fi

  # hex bin dir is created by install.sh (mkdir -p) and included by hex env path
  if echo "$PATH_OUT" | grep -q "$INSTALL_DIR/.hex/bin"; then
    assert_pass "PATH includes \$INSTALL_DIR/.hex/bin"
  else
    assert_fail "PATH missing \$INSTALL_DIR/.hex/bin: $PATH_OUT"
  fi

  if echo "$PATH_OUT" | grep -q "/usr/local/bin"; then
    assert_pass "PATH includes /usr/local/bin"
  else
    assert_fail "PATH missing /usr/local/bin: $PATH_OUT"
  fi

  # Platform-specific: /opt/homebrew/bin only on macOS ARM
  if [ -d "/opt/homebrew/bin" ]; then
    if echo "$PATH_OUT" | grep -q "/opt/homebrew/bin"; then
      assert_pass "PATH includes /opt/homebrew/bin (macOS)"
    else
      assert_fail "PATH missing /opt/homebrew/bin (macOS)"
    fi
  else
    assert_pass "PATH skips /opt/homebrew/bin (not macOS, dir absent — correct)"
  fi
else
  assert_fail "hex binary not available — cannot test hex env path"
fi

# ── Test 4: hex binary installed and functional ──────────────────────────────
# Replaces the old hex-agent-spawn.sh checks: agent spawning is now native.
bold "── hex binary ──"
if [ -n "$HEX_BIN" ]; then
  assert_pass "hex binary found at $HEX_BIN"

  if "$HEX_BIN" version &>/dev/null; then
    HEX_VER=$("$HEX_BIN" version 2>/dev/null || echo "unknown")
    assert_pass "hex version works ($HEX_VER)"
  else
    assert_fail "hex version failed"
  fi

  if "$HEX_BIN" --help 2>&1 | grep -q "doctor"; then
    assert_pass "hex binary includes doctor subcommand"
  else
    assert_fail "hex binary missing doctor subcommand"
  fi

  if "$HEX_BIN" --help 2>&1 | grep -q "startup"; then
    assert_pass "hex binary includes startup subcommand"
  else
    assert_fail "hex binary missing startup subcommand"
  fi

  # Fleet/agent teardown: `hex agent` was removed. Assert it's correctly absent.
  if "$HEX_BIN" agent --help &>/dev/null; then
    assert_fail "hex agent subcommand still present (should be removed in fleet teardown)"
  else
    assert_pass "hex agent subcommand correctly absent (fleet teardown)"
  fi
else
  assert_fail "hex binary not found — check install or Docker multi-stage build"
fi

# ── Test 5: CLAUDE.md template references binaries not paths ────────────────
bold "── CLAUDE.md template ──"
CLAUDE_MD="$INSTALL_DIR/CLAUDE.md"
if [ -f "$CLAUDE_MD" ]; then
  if grep -q 'bash ~/.boi/boi' "$CLAUDE_MD"; then
    assert_fail "CLAUDE.md still references 'bash ~/.boi/boi' (should be 'boi')"
  else
    assert_pass "CLAUDE.md uses 'boi' binary references"
  fi

  if grep -q 'python3 ~/.hex-events/hex_events_cli.py' "$CLAUDE_MD"; then
    assert_fail "CLAUDE.md still references 'python3 ~/.hex-events/hex_events_cli.py'"
  else
    assert_pass "CLAUDE.md uses 'hex-events' binary references"
  fi

  if grep -q 'env\.sh' "$CLAUDE_MD"; then
    assert_pass "CLAUDE.md documents env.sh"
  else
    assert_fail "CLAUDE.md doesn't mention env.sh"
  fi
else
  assert_fail "CLAUDE.md not found in install"
fi

# ── Test 6: hex doctor native command works ──────────────────────────────────
# Replaces: verify-agent-infra.sh (now native hex infra) + doctor.sh (now hex doctor)
bold "── hex doctor ──"
if [ -n "$HEX_BIN" ]; then
  if "$HEX_BIN" doctor --help &>/dev/null; then
    assert_pass "hex doctor --help works"
  else
    assert_fail "hex doctor --help failed"
  fi

  if HEX_DIR="$INSTALL_DIR" "$HEX_BIN" doctor list &>/dev/null; then
    assert_pass "hex doctor list works"
  else
    assert_fail "hex doctor list failed"
  fi

  # Old doctor.sh should not be present (replaced by hex doctor)
  if [ -f "$INSTALL_DIR/.hex/scripts/doctor.sh" ]; then
    assert_fail "old doctor.sh still installed — should be replaced by hex doctor"
  else
    assert_pass "old doctor.sh correctly absent (replaced by hex doctor)"
  fi

  # Old verify-agent-infra.sh should not be present
  if [ -f "$INSTALL_DIR/.hex/scripts/verify-agent-infra.sh" ]; then
    assert_fail "verify-agent-infra.sh still installed — should be replaced by native hex"
  else
    assert_pass "verify-agent-infra.sh correctly absent (replaced by native hex)"
  fi
else
  assert_fail "hex binary not available — cannot test hex doctor"
fi

# ── Test 7: Legacy scripts absent; native hex commands present ───────────────
bold "── native hex replaces legacy scripts ──"
# These scripts were removed by rustification; only .legacy.sh variants should remain
for removed_script in hex-agent-spawn.sh verify-agent-infra.sh doctor.sh; do
  if [ -f "$INSTALL_DIR/.hex/scripts/$removed_script" ]; then
    assert_fail "$removed_script still present in install (rustified — use native hex)"
  else
    assert_pass "$removed_script correctly absent (replaced by native hex)"
  fi
done

# Metrics .py scripts replaced by hex metrics subcommand
METRICS_DIR="$INSTALL_DIR/.hex/scripts/metrics"
if [ -d "$METRICS_DIR" ]; then
  PY_COUNT=$(ls "$METRICS_DIR"/*.py 2>/dev/null | wc -l | tr -d ' ')
  if [ "$PY_COUNT" -gt 0 ]; then
    assert_fail "Legacy Python metrics scripts still present: $(ls "$METRICS_DIR"/*.py 2>/dev/null | tr '\n' ' ')"
  else
    assert_pass "No legacy Python metrics scripts in .hex/scripts/metrics/ (replaced by hex metrics)"
  fi
else
  assert_pass ".hex/scripts/metrics/ absent — metrics moved to native hex (correct)"
fi

if [ -n "$HEX_BIN" ]; then
  if "$HEX_BIN" metrics --help &>/dev/null; then
    assert_pass "hex metrics native command accessible (replaces metrics/*.py)"
  else
    assert_fail "hex metrics subcommand missing"
  fi
fi

# ── Test 8: Version reconciled — Cargo.toml is canonical source ─────────────
bold "── version ──"
CARGO_VERSION=$(grep -E '^version' "$REPO_DIR/system/harness/Cargo.toml" | head -1 | cut -d'"' -f2)
INSTALLED_VERSION=$(cat "$INSTALL_DIR/.hex/version.txt" 2>/dev/null || echo "MISSING")

if [ -n "$CARGO_VERSION" ]; then
  assert_pass "Cargo.toml version readable: $CARGO_VERSION"
else
  assert_fail "Could not read version from system/harness/Cargo.toml"
fi

if [ "$INSTALLED_VERSION" = "$CARGO_VERSION" ]; then
  assert_pass "Installed version.txt ($INSTALLED_VERSION) matches Cargo.toml canonical version"
else
  assert_fail "Version mismatch: installed='$INSTALLED_VERSION', Cargo.toml='$CARGO_VERSION'"
fi

# Catch the stale pre-rustification version
if [ "$INSTALLED_VERSION" = "0.12.0" ]; then
  assert_fail "version.txt is the stale 0.12.0 — T20D1 reconciliation did not apply"
else
  assert_pass "version.txt is not the stale 0.12.0 value"
fi

# ── Summary ─────────────────────────────────────────────────────────────────
echo ""
bold "══ Results ══"
echo "  Pass: $PASS"
echo "  Fail: $FAIL"
echo "  Total: $((PASS + FAIL))"

if [ $FAIL -gt 0 ]; then
  red "OVERALL: FAIL ($FAIL failures)"
  exit 1
else
  green "OVERALL: PASS"
  exit 0
fi
