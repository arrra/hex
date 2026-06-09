# hex-ops

Operational guide for hex's runtime glue: session management, dashboards,
LaunchAgents, and **telemetry**.

---

## LaunchAgents (launchd)

hex's supervised long-running services run as **per-user gui LaunchAgents** in
`~/Library/LaunchAgents/`, bootstrapped into the **`gui/<uid>`** domain, with
**`SessionCreate=true`** and **no `UserName`**. Examples: `com.hex.harness` (the core
harness); `com.mrap.boi-daemon` (the personal BOI daemon — same pattern). The code already
implements this: `hex harness start|stop|status` targets `gui/$(id -u)/com.hex.harness` and
`upgrade.rs` kickstarts the same target after a binary swap.

### Why gui/ + SessionCreate (rationale)

The harness runs per-task reasoning *inside* `claude`, and BOI workers spawn `claude`;
Claude Code auth lives in the macOS **login keychain**. `SessionCreate=true` bridges the
launchd job into the user's Aqua login (security) session so keychain lookups succeed. The
alternatives cannot reach the login keychain:

| Option | Login keychain | Notes |
|---|---|---|
| **gui/ LaunchAgent + SessionCreate** (chosen) | yes — via the Aqua session | must be bootstrapped from a real GUI login session |
| user/ LaunchAgent (no SessionCreate) | no — no Aqua session | `SessionCreate` + `user/` also fails to bootstrap (EIO) |
| system LaunchDaemon (`UserName=mrap`) | no — runs outside any login session | starts at boot but can't read the login keychain |

FileVault forces a GUI login at every boot on this box, so there is effectively always a
login session — the gui LaunchAgent's only downside ("dies on logout") is moot.

**macOS 26 caveat:** the SecurityAgent session is NOT inherited by child processes — spawn
`claude` as a DIRECT program, never `bash -> claude`, or it loses keychain access.

### Operational gotchas (learned 2026-06-05)

- **Bootstrap only from a real GUI login session.** `launchctl bootstrap` returns
  `Input/output error` (errno 5) when run from a *detached* session — inside **tmux**,
  under the **Happy daemon**, or over plain **SSH** — because those carry their own audit
  session (`asid`), not the Aqua login session. The sandboxed agent shell cannot bootstrap
  either. Run it from **Terminal.app at the Mac console or via Screen Sharing**.
- **Reload = `bootout` THEN `bootstrap`.** `bootstrap` alone fails on an already-loaded
  service. After editing a plist:
  ```
  launchctl bootout   gui/$(id -u)/com.hex.harness
  launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.hex.harness.plist
  ```
- **Diagnose which session you're in:** `launchctl print pid/$$ | grep -E 'asid|coalition'`.
  If the coalition is `com.mrap.tmux-boot` (or the `asid` is not your Aqua login session),
  `launchctl bootstrap` will EIO from there — switch to a GUI terminal.
- **Status / health:** `launchctl print gui/$(id -u)/com.hex.harness | grep -E 'state =|pid ='`.

---

## Telemetry

hex telemetry is a **native, local SQLite event store** owned by the Rust
harness. Every iii worker job is auto-traced via the worker host
(`iii_worker::run_command`), and any other code path or shell script can emit
into the same store via `hex telemetry record`. There is no Prometheus,
Grafana, or OTLP collector — a single-user local system gets a single-user
local store.

### Store

- **Path:** `$HEX_DIR/.hex/telemetry/events.db` (HEX_DIR falls back to `.`).
- **Engine:** SQLite (rusqlite, bundled) with `PRAGMA journal_mode=WAL`.
- **Schema:**

```sql
CREATE TABLE events (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  ts          TEXT    NOT NULL,   -- RFC3339 UTC, event start time
  source      TEXT    NOT NULL,   -- worker_name, or arbitrary source
  event       TEXT    NOT NULL,   -- function/job id, e.g. hex::memory::index
  status      TEXT    NOT NULL,   -- 'ok' | 'error' | 'spawn_error'
  duration_ms INTEGER,            -- nullable
  exit_code   INTEGER,            -- nullable
  detail      TEXT                -- stdout/stderr tail or free-form/JSON meta
);
CREATE INDEX idx_events_ts    ON events(ts);
CREATE INDEX idx_events_event ON events(event);
```

### Auto-tracing of iii jobs

Every iii job flows through one chokepoint — `iii_worker::run_command` — and
that function writes a telemetry row on every outcome:

- `status = ok` on a successful exit.
- `status = error` on a non-zero exit (records the `exit_code`).
- `status = spawn_error` if the process failed to launch.

Each row carries the worker name as `source`, the job id as `event`, the
measured `duration_ms`, and a stdout/stderr tail in `detail`. Zero per-worker
opt-in is required — wiring a new iii worker automatically gets telemetry.

Telemetry writes from inside the worker are **loud-but-not-fatal**: a write
failure logs `telemetry: failed to record ...` to stderr but never fails the
observed job. Telemetry is observational; it must not break the thing it
observes.

### `hex telemetry` commands

```bash
hex telemetry recent   [--limit N] [--json]      # newest events first
hex telemetry failures [--since 24h|7d] [--json] # only status != 'ok'
hex telemetry status   [--json]                  # per-event aggregates
hex telemetry record   --source S --event E --status ok|error|spawn_error \
                       [--duration-ms N] [--exit-code N] [--detail TEXT]
hex telemetry prune    [--keep-days 30]
```

- **recent / failures / status** print a compact aligned text table by default
  (`ts source event status dur`) or JSON with `--json`. `--since` accepts
  `Nh`/`Nd`; default `24h`.
- **record** is the manual emit seam: any shell script or external tool can
  push an event into the same store. Unlike the in-worker path, write failures
  surface as a non-zero exit.
- **prune** deletes rows older than `keep-days` (default 30) and prints how
  many it removed.

### Doctor check

`hex doctor` runs a `telemetry-health` check. If the store is missing it
skips. Otherwise it queries the last 24h: any non-`ok` rows produce a warn
with a count and the most recent failing event id ("run
`hex telemetry failures` to inspect"); a clean window passes.

### History

This replaces the old in-memory iii observability (ephemeral, 1000-span cap,
not queryable) and the previous `.hex/telemetry/events.db` that was removed
when `hex-events` was deleted on 2026-06-02. The store is now rebuilt
natively in the Rust harness.

---

## Lean Claude runs (`claude-runs.toml`)

**Policy (spec Sf5bj7y1d):** every headless `claude -p` invocation hex makes
runs as lean as possible — no plugins, no skills, no MCP servers, no hooks,
no CLAUDE.md — unless a per-run profile explicitly re-enables specific
functionality.

This is enforced by a central profile resolver
(`system/harness/src/claude_runs.rs`) and a tiny CLI surface:

```
hex claude-flags <profile>     # prints eval-safe shell flags for that profile
```

Built-in profiles (apply with or without a config file):

| profile           | used by                                                | re-enabled |
|-------------------|--------------------------------------------------------|-----------|
| `default`         | fallback                                               | —         |
| `harness_worker`  | `system/harness/src/worker/run.rs`                     | —         |
| `meeting_prep`    | `system/scripts/meeting-prep.sh`                       | `google-calendar` MCP |
| `eval`            | `tests/eval/run_eval.py`                               | —         |

Lean default = `--bare --strict-mcp-config --mcp-config '{}'
--disable-slash-commands`. `--bare` skips auto-discovery of hooks, LSP,
plugin sync, auto-memory, CLAUDE.md, and plugin/MCP/skill auto-discovery.
The explicit empty strict mcp config ensures no MCP server loads even on a
future Claude Code version where `--bare` covers less.

### Profile schema

Drop a `claude-runs.toml` at `$HEX_DIR/.hex/config/claude-runs.toml` to
override the built-ins. See `system/templates/claude-runs.toml.example` for
a fully commented reference. Minimum:

```toml
[defaults]
bare = true
# disable_slash_commands = true
# mcp_servers     = []     # names looked up in workspace .mcp.json
# plugin_dirs     = []
# setting_sources = []     # subset of ["user", "project", "local"]
# allowed_tools   = []
# extra_flags     = []     # appended verbatim

[runs.harness_worker]
# Lean — no overrides needed.

[runs.meeting_prep]
mcp_servers = ["google-calendar"]   # must resolve in workspace .mcp.json

[runs.eval]
```

### Re-enable knobs (flag emission)

| TOML field               | Emits                                              |
|--------------------------|----------------------------------------------------|
| `bare = true`            | `--bare`                                           |
| `mcp_servers = [..]`     | `--strict-mcp-config --mcp-config '<inline json>'` containing ONLY the named servers, looked up from `.mcp.json`. Empty/absent → `'{}'`. |
| `disable_slash_commands` | `--disable-slash-commands`                         |
| `plugin_dirs = [..]`     | repeated `--plugin-dir <dir>`                      |
| `setting_sources = [..]` | `--setting-sources a,b,c`                          |
| `allowed_tools = [..]`   | `--allowedTools "..."`                             |
| `extra_flags = [..]`     | appended verbatim                                  |

Unknown profile name, malformed TOML, or `mcp_servers` naming a server
absent from the workspace MCP config → **hard error** (Standing Order S6:
no quiet failures). `hex doctor` runs the `claude-runs-config` check which
absent-passes when no `claude-runs.toml` is present, and validates the file
when one is — including resolving every named MCP server.

### Using the flags

Shell call sites use `hex claude-flags` with eval-style substitution:

```bash
claude $(hex claude-flags meeting_prep) -p "$(cat prompt.txt)"
```

The Rust harness call sites build the arg vector via
`claude_runs::resolve(<profile>, Some(&hex_dir))?.to_cli_flags(&mcp)?`.

**Behavior change at install time:** a machine with no
`claude-runs.toml` will still work — the built-in profiles apply and runs
become lean. That IS the intended default; only opt in to re-enabling
specific functionality, per profile.
