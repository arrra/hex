# hex-ops

Operational guide for hex's runtime glue: session management, dashboards,
LaunchAgents, and **telemetry**.

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
