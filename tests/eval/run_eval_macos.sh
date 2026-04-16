#!/usr/bin/env bash
set -euo pipefail

# Run hex eval harness in a Tart macOS VM.
#
# Prerequisites:
#   brew install cirruslabs/cli/tart sshpass
#   tart clone ghcr.io/cirruslabs/macos-sequoia-base:latest hex-test-base
#
# Usage:
#   bash tests/eval/run_eval_macos.sh                      # dry-run
#   bash tests/eval/run_eval_macos.sh --live               # live (needs ANTHROPIC_API_KEY)

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

MODE="${1:---dry-run}"
VM_NAME="hex-eval-$(date +%s)"
BASE_IMAGE="hex-test-base"
SSH_USER="admin"
SSH_PASS="admin"
SSH_OPTS="-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o PreferredAuthentications=password -o PubkeyAuthentication=no"

cleanup() {
    echo ""
    echo "Cleaning up VM: $VM_NAME"
    tart stop "$VM_NAME" 2>/dev/null || true
    tart delete "$VM_NAME" 2>/dev/null || true
}
trap cleanup EXIT

echo "=== hex eval (macOS Tart) ==="
echo ""

# Validate
if ! command -v tart &>/dev/null; then
    echo "ERROR: tart not installed. Run: brew install cirruslabs/cli/tart"
    exit 1
fi
if ! tart list | grep -q "$BASE_IMAGE"; then
    echo "ERROR: Base image '$BASE_IMAGE' not found."
    echo "Run: tart clone ghcr.io/cirruslabs/macos-sequoia-base:latest $BASE_IMAGE"
    exit 1
fi

# Clone + start VM
echo "[1/5] Starting macOS VM..."
tart clone "$BASE_IMAGE" "$VM_NAME"
tart run --no-graphics "$VM_NAME" &

# Wait for IP
for i in $(seq 1 60); do
    VM_IP=$(tart ip "$VM_NAME" 2>/dev/null || true)
    [ -n "$VM_IP" ] && break
    sleep 2
done
[ -z "$VM_IP" ] && { echo "FAIL: VM didn't get IP"; exit 1; }

# Wait for SSH
for i in $(seq 1 30); do
    sshpass -p "$SSH_PASS" ssh $SSH_OPTS -o ConnectTimeout=2 "$SSH_USER@$VM_IP" "echo ready" 2>/dev/null | grep -q ready && break
    sleep 2
done

vm_run() {
    sshpass -p "$SSH_PASS" ssh $SSH_OPTS "$SSH_USER@$VM_IP" "$@"
}

echo "  VM IP: $VM_IP"
echo "  Python: $(vm_run 'python3 --version 2>&1')"

# Copy repo
echo "[2/5] Copying repo into VM..."
vm_run "mkdir -p /tmp/hex-setup"
tar -C "$REPO_DIR" -czf - --exclude='.git' --exclude='__pycache__' --exclude='.pytest_cache' . \
    | vm_run "tar -C /tmp/hex-setup -xzf -"
echo "  Done"

# Install deps
echo "[3/5] Installing dependencies..."
vm_run "pip3 install pyyaml 2>/dev/null || python3 -m pip install pyyaml 2>/dev/null || true" 2>/dev/null

# Install Node.js + Claude Code for live mode
if [ "$MODE" = "--live" ]; then
    if [ -z "${ANTHROPIC_API_KEY:-}" ]; then
        echo "ERROR: ANTHROPIC_API_KEY not set."
        exit 1
    fi
    echo "  Installing Node.js + Claude Code..."
    vm_run "brew install node 2>/dev/null || true" 2>/dev/null
    vm_run "npm install -g @anthropic-ai/claude-code 2>/dev/null || true" 2>/dev/null
    echo "  Claude: $(vm_run 'claude --version 2>/dev/null || echo NOT_AVAILABLE')"
fi

# Run eval
echo "[4/5] Running eval..."
if [ "$MODE" = "--live" ]; then
    vm_run "cd /tmp/hex-setup && ANTHROPIC_API_KEY='$ANTHROPIC_API_KEY' python3 tests/eval/run_eval.py --live --model sonnet"
else
    vm_run "cd /tmp/hex-setup && python3 tests/eval/run_eval.py --dry-run"
fi

echo "[5/5] Done"
