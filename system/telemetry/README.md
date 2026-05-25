# `system/telemetry/` — Source of Truth for Hex Telemetry Migrations

This directory is the canonical source for hex telemetry schema. Files here
are checked into hex-foundation and propagate to deployed hex instances via
`install.sh` and `hex upgrade`.

> Runtime / deployed location: `~/hex/.hex/telemetry/`. That directory is the
> live SQLite database, queue, and applied-migrations ledger — never edit
> migrations there by hand. Edit the SQL in this source directory, then run
> `install.sh` (or copy the file manually) and re-run `telemetry-init.sh`.

---

## Layout

| Path | Purpose |
|------|---------|
| `migrations/NNN_description.sql` | Numbered, idempotent DDL migrations |
| `README.md` | This file |

Migrations are applied in numeric filename order by
`~/hex/.hex/bin/telemetry-init.sh`, which records every applied file in
`~/hex/.hex/telemetry/.applied_migrations` to enforce idempotency.

---

## Current Migrations

| File | Adds |
|------|------|
| `001_initial.sql` (in deployed `~/hex/.hex/telemetry/migrations/` only) | Initial `events` table + indexes |
| `002_c3_views.sql` | C3 baseline-metric read VIEWs: `v_c3_quiet_failure_weekly`, `v_c3_orphan_scan_daily` |

> `001_initial.sql` predates this source-tree home and lives only in the
> deployed mirror. Future migrations land here first and are copied out by
> `install.sh`.

---

## Adding a New Migration

1. Pick the next zero-padded sequence number (e.g. `003_my_change.sql`).
2. Write **idempotent** SQL only — use `CREATE TABLE IF NOT EXISTS`,
   `CREATE VIEW IF NOT EXISTS`, `CREATE INDEX IF NOT EXISTS`. Never `DROP`.
3. Append-only schema evolution: add columns with `DEFAULT` values, never
   rename or remove columns (see "Schema Evolution Rules" in
   `~/hex/.hex/telemetry/README.md`).
4. Drop the file at `system/telemetry/migrations/NNN_description.sql`.
5. Confirm `install.sh` copies the file into
   `$TARGET_DIR/.hex/telemetry/migrations/` — this is handled by the bulk
   `cp -r "$SCRIPT_DIR/system" "$TARGET_DIR/.hex"` step. If you reorganise
   the install script, ensure the migrations path is still covered (the
   verify-gate for Task 0 of the C3 instrumentation spec greps install.sh
   for the literal string `system/telemetry/migrations`).
6. In a deployed instance, run `bash ~/hex/.hex/bin/telemetry-init.sh` to
   apply the new migration. It is safe to re-run — already-applied
   migrations are skipped via the `.applied_migrations` ledger.

---

## Why Source-of-Truth Lives Here

Before this directory existed, telemetry migrations lived only inside
deployed hex instances (`~/hex/.hex/telemetry/migrations/`). That meant:

- No version control of schema changes alongside the harness binary.
- No way to ship a migration to multiple instances via `hex upgrade`.
- Schema drift between instances was undetectable.

Landing migrations in `system/telemetry/migrations/` gives us:

- Git history for every schema change.
- One canonical copy in the foundation repo; instances pull via upgrade.
- A clear failure mode if `install.sh` ever stops copying this tree —
  Task 0 of the iii-hex instrumentation spec asserts the install path
  literally references `system/telemetry/migrations`.

---

## Related Documentation

- `~/hex/.hex/telemetry/README.md` — runtime / query reference, event taxonomy,
  schema evolution rules, emit/flush mechanics.
- `docs/superpowers/specs/2026-05-24-iii-hex-mirror-sink-contract.md` —
  the amended mirror-sink contract that the C3 instrumentation work delivers
  alongside `002_c3_views.sql`. Note: the mirror sink writes JSONL files to
  `~/.hex-events/mirror/`, **not** a SQL table here.
- `docs/c3-instrumentation.md` — operator-facing documentation for the four
  C3 baseline metrics (M1–M4) and the mirror sink.
