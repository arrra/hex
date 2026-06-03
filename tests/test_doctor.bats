#!/usr/bin/env bats
# Unit tests for the canonical Rust doctor's boi-health check (check_17 /
# `boi_health.rs`).
#
# History: these tests used to source a shell `check_17` in
# `system/scripts/doctor.sh`. That file was removed in the doctor cutover
# (commit 790b6d35 "feat(harness): doctor cutover — hex doctor replaces
# doctor.sh") — the Rust harness (`hex doctor run`) is now the only live
# doctor. These tests drive that binary directly.
#
# V2 cutover (boi-v2, "boi 3.0.0"): the check is binary-present →
# `--version` ok → control-socket `~/.boi/v2/daemon.sock` present → pass;
# warn otherwise. The old V1 VERSIONS-mismatch and `boi-wrapper` assertions
# are gone (boi-v2 creates neither).
#
# `ctx.home` is read from $HOME and the boi paths hang off it, so we point
# HOME at a throwaway dir to stage each state. HEX_DIR must be a real hex
# workspace (the doctor refuses to run otherwise), so we point it at the repo.

REPO_ROOT="$(cd "${BATS_TEST_DIRNAME}/.." && pwd)"

# Prefer the workspace-root release build; fall back to the harness-local one.
if [ -x "${REPO_ROOT}/target/release/hex" ]; then
  HEX_BIN="${REPO_ROOT}/target/release/hex"
elif [ -x "${REPO_ROOT}/system/harness/target/release/hex" ]; then
  HEX_BIN="${REPO_ROOT}/system/harness/target/release/hex"
else
  HEX_BIN=""
fi

# ── Helpers ──────────────────────────────────────────────────────────────────

# Write a fake boi-v2 binary that prints "boi 3.0.0" for --version.
# Usage: make_fake_boi <path> <version_exit> <version_str>
make_fake_boi() {
  local path="$1" version_exit="${2:-0}" version_str="${3:-boi 3.0.0}"
  mkdir -p "$(dirname "$path")"
  cat > "$path" << EOF
#!/bin/bash
case "\$1" in
  --version|-V) echo "$version_str"; exit $version_exit ;;
  *)            exit 0 ;;
esac
EOF
  chmod +x "$path"
}

# Bind a unix-domain socket at the given path (the boi-v2 daemon control socket).
make_socket() {
  local path="$1"
  mkdir -p "$(dirname "$path")"
  python3 -c "import socket; s=socket.socket(socket.AF_UNIX); s.bind('$path')"
}

# Run the boi-health check in isolation and capture its JSON status.
# Sets: $boi_status (pass|warning|error), $output (raw json)
run_boi_health() {
  # `hex doctor run` exits non-zero when any check warns/errors — that's the
  # signal under test here, not a harness failure. Capture output regardless.
  output=$(HEX_DIR="$REPO_ROOT" HOME="$FAKE_HOME" \
    "$HEX_BIN" doctor run --filter boi --json 2>&1) || true
  boi_status=$(python3 -c \
    "import sys,json; d=json.load(sys.stdin); print(d['checks'][0]['status'] if d.get('checks') else 'none')" \
    <<< "$output" 2>/dev/null || echo "parse-error")
}

# ── Setup / teardown ─────────────────────────────────────────────────────────

setup() {
  if [ -z "$HEX_BIN" ]; then
    skip "hex binary not built (run: cargo build --release)"
  fi
  FAKE_HOME=$(mktemp -d)
}

teardown() {
  rm -rf "$FAKE_HOME"
}

# ── Tests ─────────────────────────────────────────────────────────────────────

@test "boi-health: binary missing → warning (non-critical, not error)" {
  # No binary at ~/.boi/bin/boi
  run_boi_health
  [ "$boi_status" = "warning" ]
}

@test "boi-health: dangling symlink at ~/.boi/bin/boi → warning" {
  mkdir -p "$FAKE_HOME/.boi/bin"
  ln -s "/nonexistent/boi_gone_$$" "$FAKE_HOME/.boi/bin/boi"
  run_boi_health
  # is_file() follows the symlink → target absent → "boi not found" warning.
  [ "$boi_status" = "warning" ]
}

@test "boi-health: boi --version fails → warning" {
  make_fake_boi "$FAKE_HOME/.boi/bin/boi" 1 "boi 3.0.0"
  run_boi_health
  [ "$boi_status" = "warning" ]
}

@test "boi-health: binary + version ok but daemon socket missing → warning" {
  make_fake_boi "$FAKE_HOME/.boi/bin/boi" 0 "boi 3.0.0"
  # No ~/.boi/v2/daemon.sock
  run_boi_health
  [ "$boi_status" = "warning" ]
  echo "$output" | grep -q "daemon.sock"
}

@test "boi-health: binary + version + daemon socket → pass" {
  make_fake_boi "$FAKE_HOME/.boi/bin/boi" 0 "boi 3.0.0"
  make_socket "$FAKE_HOME/.boi/v2/daemon.sock"
  run_boi_health
  [ "$boi_status" = "pass" ]
  echo "$output" | grep -q "healthy"
}
