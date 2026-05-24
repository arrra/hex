"""
hex-router — Reverse proxy routing incoming requests to hex sub-services.

Listens on 127.0.0.1:PORT (default 7000). Tailscale Serve fronts this on
:443 so named paths like /ui, /boi, /visions work.

Started by: hex router serve (system/harness/src/router.rs)

TODO: implement the router. This stub exits with an error to make the
missing-file guardrail test pass while the real implementation is pending.

See system/harness/src/router.rs and system/scripts/hex-router/serve.legacy.sh
"""
import os
import sys

port = os.environ.get("PORT", "7000")
print(f"ERROR: hex-router router.py is not yet implemented (would listen on port {port}).", file=sys.stderr)
print("See system/harness/src/router.rs and system/scripts/hex-router/serve.legacy.sh", file=sys.stderr)
sys.exit(1)
