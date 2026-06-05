# tests/core-e2e/Dockerfile
# Comprehensive core E2E test image. Covers all hex primitives:
# messaging, events, assets, SSE, CLI subcommands, telemetry.
#
# Adds Python 3, SQLite CLI, and curl on top of the base agent-harness pattern.

# ── Stage 1: Build ───────────────────────────────────────────────────────────
FROM rust:latest AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config git \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY system/harness/ ./
RUN cargo build --release 2>&1
# Stamp the Cargo.toml version for runtime consistency checks (source of truth)
RUN grep '^version' ./Cargo.toml | head -1 | cut -d'"' -f2 > /build/compiled-version.txt

# ── Stage 2: Runtime ─────────────────────────────────────────────────────────
FROM debian:trixie-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    bash \
    python3 \
    sqlite3 \
    curl \
    && rm -rf /var/lib/apt/lists/*

ENV HOME=/home/testuser
RUN useradd -m -s /bin/bash testuser
USER testuser

# Workspace skeleton required by get_hex_dir()
RUN mkdir -p \
    $HOME/hex/projects \
    $HOME/hex/.hex/bin \
    $HOME/hex/.hex/data \
    $HOME/hex/.hex/audit \
    $HOME/hex/.hex/telemetry \
    $HOME/hex/.hex/reference/core-agents \
    $HOME/hex/.hex/scripts \
    $HOME/hex/tests/core-e2e/suites

# CLAUDE.md is required by get_hex_dir() to validate the workspace
RUN touch $HOME/hex/CLAUDE.md

# Binary and backward-compat symlink
COPY --from=builder /build/target/release/hex $HOME/hex/.hex/bin/hex
# Compiled-version stamp for cli-version-consistency test
COPY --from=builder /build/compiled-version.txt $HOME/hex/.hex/hex-version.txt
RUN ln -s $HOME/hex/.hex/bin/hex $HOME/hex/.hex/bin/hex-agent

# Integration and doctor scripts (hex delegates to these via bash)
# Binary looks for these at $HEX_DIR/.hex/scripts/ (runtime install path)
COPY system/scripts/hex-integration $HOME/hex/.hex/scripts/hex-integration
COPY system/scripts/hex-doctor      $HOME/hex/.hex/scripts/hex-doctor

# Core E2E test suite (helpers.sh, run-all.sh, suites/*.sh written by later tasks)
COPY tests/core-e2e/ $HOME/hex/tests/core-e2e/

ENV HEX_DIR=$HOME/hex
ENV PATH="$HOME/hex/.hex/bin:$PATH"
WORKDIR $HOME/hex
ENTRYPOINT ["bash", "tests/core-e2e/run-all.sh"]
