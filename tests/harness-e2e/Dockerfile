# tests/harness-e2e/Dockerfile
#
# Containerized E2E test image for the `hex harness` runtime.
# Proves the at-most-once lifecycle (graceful drain + outbox replay) against a
# LIVE iii engine — which is now BAKED INTO the hex binary (mrap/hex-iii fork).
#
# VENDORLESS: there is no separate `iii` engine binary. `hex harness serve`
# starts the engine in-process, so the image needs ONLY `hex`. (This replaces
# the old vendored-iii model where the host pre-staged ~/.local/bin/iii.)
#
# Build from the repo root so the COPY paths resolve:
#   docker build -f tests/harness-e2e/Dockerfile -t hex-harness-e2e .
#   docker run --rm hex-harness-e2e

# ── Stage 1: Build the hex binary (engine compiled in via the git dep) ───────
FROM rust:latest AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config git \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY system/harness/ ./
# Sibling path dep: scipd = { path = "../code-intel" } resolves to /code-intel
# when the harness manifest is at /build. Copy it or the build can't resolve it.
COPY system/code-intel/ /code-intel/
# Debug build: the engine is fetched as a git dep (mrap/hex-iii) at build time.
# Debug (not release) keeps the in-container compile fast; the release profile
# is verified separately on the host. The binary contains the full engine.
# BuildKit cache mounts persist the cargo registry/git + target dir across
# rebuilds so only changed crates recompile; the binary is copied OUT of the
# cached target into a real layer (/hex-bin) for the runtime stage to grab.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/build/target \
    cargo build 2>&1 && cp /build/target/debug/hex /hex-bin

# ── Stage 2: Runtime — ONLY the hex binary, nothing vendored ─────────────────
FROM debian:trixie-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    bash coreutils procps \
    && rm -rf /var/lib/apt/lists/*

ENV HOME=/home/testuser
RUN useradd -m -s /bin/bash testuser

# The hex binary IS the engine + harness — no vendored iii.
COPY --from=builder /hex-bin /usr/local/bin/hex

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
# Register the test-only hex-e2e worker (gated in workers::registry()).
ENV HEX_HARNESS_E2E=1
# The baked-in engine's default config tries to spawn the external iii-worker-ops
# daemon if it's on PATH; it isn't here, but disable explicitly for determinism.
ENV IIIWORKER_DISABLE_BUILTIN_DAEMONS=1
ENV PATH="/usr/local/bin:$PATH"
WORKDIR $HOME/hex

ENTRYPOINT ["bash", "/usr/local/bin/test-harness-lifecycle.sh"]
