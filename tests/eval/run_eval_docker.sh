#!/usr/bin/env bash
set -euo pipefail

# Run hex eval harness in Docker.
#
# Usage:
#   bash tests/eval/run_eval_docker.sh                    # dry-run (no API key needed)
#   bash tests/eval/run_eval_docker.sh --live              # live run (needs ANTHROPIC_API_KEY)
#   bash tests/eval/run_eval_docker.sh --live --model haiku # cheaper model
#   bash tests/eval/run_eval_docker.sh --live --case onboarding  # single case

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

MODE="${1:---dry-run}"

echo "=== hex eval (Docker) ==="
echo ""

# Build
echo "Building eval image..."
docker build -f "$SCRIPT_DIR/Dockerfile.eval" -t hex-eval "$REPO_DIR" 2>&1 | tail -3
echo ""

# Run
if [ "$MODE" = "--live" ]; then
    if [ -z "${ANTHROPIC_API_KEY:-}" ]; then
        echo "ERROR: ANTHROPIC_API_KEY not set."
        echo "  export ANTHROPIC_API_KEY=sk-ant-..."
        echo "  bash tests/eval/run_eval_docker.sh --live"
        exit 1
    fi
    echo "Running live eval..."
    docker run --rm \
        -e ANTHROPIC_API_KEY="$ANTHROPIC_API_KEY" \
        -e HEX_EVAL_SANDBOXED=1 \
        hex-eval "$@"
else
    echo "Running dry-run..."
    docker run --rm hex-eval --dry-run
fi
