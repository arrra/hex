#!/usr/bin/env python3
# SSE Bus canonical server — PORTED to Rust.
#
# This file serves as the consolidation reference for the server.py cluster audit.
# The SSE bus functionality was ported to:
#   hex sse bridge  (from bridge.py, commit 9b3073b)
#
# All other server.py files in system/scripts/ were audited on 2026-05-16 and
# kept as KEEP-DIVERGENT because each serves a distinct domain:
#   artifacts/server.py       — static file serving (port 8897)
#   boi-web/server.py         — BOI SSE status with TLS (port 8891)
#   comments-service/server.py— comments REST API (port 8901)
#   pulse-dashboard/server.py — pulse dashboard v1 (port 8896, alternate)
#   pulse/server.py           — pulse health dashboard v2 (port 8896, primary)
#
# See /tmp/hygiene-server-py.md for full decision log.
raise SystemExit(
    "SSE bus server has been ported to Rust. Run: hex sse bridge"
)
