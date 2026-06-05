# tests/harness-e2e/Dockerfile
#
# Containerized E2E test image for the `hex harness` runtime.
# Proves the at-most-once lifecycle (graceful drain + outbox replay)
# against a LIVE iii engine.
#
# ── Vendored iii binary ──────────────────────────────────────────────────────
# This image expects the iii engine binary at tests/harness-e2e/vendor/iii.
# The HOST test runner must copy ~/.local/bin/iii into that path before
# `docker build`. Example:
#
#   mkdir -p tests/harness-e2e/vendor
#   cp ~/.local/bin/iii tests/harness-e2e/vendor/iii
#   chmod +x tests/harness-e2e/vendor/iii
#   docker build -f tests/harness-e2e/Dockerfile -t hex-harness-e2e .
#
# The lifecycle script (test-harness-lifecycle.sh) prints a loud SKIP
# (and exits 0) when the vendored binary is absent — see S6 "no quiet
# failures" — so a missing vendor never silently passes as covered.

# ── Stage 1: Build the harness ───────────────────────────────────────────────
FROM rust:latest AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config git \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY system/harness/ ./
RUN cargo build --release 2>&1

# ── Stage 2: Runtime ─────────────────────────────────────────────────────────
FROM debian:trixie-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    bash coreutils procps curl jq \
    && rm -rf /var/lib/apt/lists/*

ENV HOME=/home/testuser
RUN useradd -m -s /bin/bash testuser

# Hex binary
COPY --from=builder /build/target/release/hex /usr/local/bin/hex

# Vendored iii engine (host pre-stages it under tests/harness-e2e/vendor/iii).
# If absent at build time, the file simply won't exist in the image;
# the lifecycle script detects this at runtime and emits a loud SKIP.
COPY tests/harness-e2e/vendor/ /opt/iii-vendor/

# Lifecycle test script
COPY tests/harness-e2e/test-harness-lifecycle.sh /usr/local/bin/test-harness-lifecycle.sh
RUN chmod +x /usr/local/bin/test-harness-lifecycle.sh

# Hex workspace skeleton
RUN mkdir -p \
        $HOME/hex/.hex/harness \
        $HOME/hex/.hex/bin \
    && touch $HOME/hex/CLAUDE.md \
    && chown -R testuser:testuser $HOME/hex

USER testuser
ENV HEX_DIR=$HOME/hex
ENV PATH="/opt/iii-vendor:/usr/local/bin:$PATH"
WORKDIR $HOME/hex

ENTRYPOINT ["bash", "/usr/local/bin/test-harness-lifecycle.sh"]
