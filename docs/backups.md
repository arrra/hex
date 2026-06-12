<!--
verified-against: feature/backup-restic-offsite
source-paths: system/harness/src/backup.rs, system/harness/src/modules/backup.worker.rs, system/harness/src/modules/backup_offsite.worker.rs
-->
# Backups

hex backs up in two tiers, both driven by typed harness workers (no shell scripts, no
launchd plists). Logic lives in `system/harness/src/backup.rs`; the cron schedules live in
two one-file workers under `system/harness/src/modules/`.

| Tier | Worker | Cron | Command | What | Where |
| --- | --- | --- | --- | --- | --- |
| Local snapshot | `hex-backup` (`backup.worker.rs`) | 04:00 | `hex backup` | Consistent sqlite snapshots of `memory.db` / `telemetry/events.db` / `ledger/ledger.db` via the sqlite backup API (WAL-safe), 7-day rotation | `$HEX_DIR/.hex/backups/YYYY-MM-DD/` (on-machine) |
| Off-site | `hex-backup-offsite` (`backup_offsite.worker.rs`) | 04:30 | `hex backup offsite` | Encrypted, deduplicated `restic` backup of the **whole operating layer** to mounted Google Drive | configured `RESTIC_REPOSITORY` (off-machine) |

The local snapshot is on-machine only (it lives inside the workspace it backs up). The
off-site tier is what survives disk loss / theft / accidental `rm`.

## Off-site backup (`hex backup offsite`)

Ships the operating layer off-machine via [restic](https://restic.net): client-side
encryption, dedup, retention, and integrity check. Source set:

- `$HEX_DIR` — the whole workspace, including `.git` and `raw/transcripts/`
- `~/.boi/v2/boi.db` — BOI engine state (git never tracks it)
- `~/.claude/projects` — Claude Code session transcripts incl. `subagents/` (exist nowhere else)

Regenerable paths are excluded (typed const `EXCLUDES` in `backup.rs`): `.hex/.upgrade-cache`,
`.hex/.upgrade-backup-*`, `.hex/bin/.fastembed_cache`, the live `.hex/*.db` (captured instead
via the consistent `.hex/backups/<today>` snapshot the job takes first), `target`, `node_modules`.

Each run: `restic unlock` (clears a stale lock from the eventually-consistent gdrive mount) →
`restic backup … --tag hex-offsite` → `restic forget --keep-daily 7 --keep-weekly 4
--keep-monthly 6 --prune` → `restic check`.

### Configuration (one-time, machine-local)

The job is a **deliberate no-op until `RESTIC_REPOSITORY` is set** — it prints a "not
configured" line and exits 0, so it never false-alarms before setup. To enable:

```sh
# 1. Point at a restic repo on the locally-mounted Google Drive (no OAuth — it's a path).
GDRIVE="$HOME/Library/CloudStorage/GoogleDrive-<you>/My Drive"
export RESTIC_REPOSITORY="$GDRIVE/hex-restic"

# 2. Strong password in the macOS Keychain; restic reads it via RESTIC_PASSWORD_COMMAND.
security add-generic-password -s hex-restic -a "$USER" -w   # prompts for the password
export RESTIC_PASSWORD_COMMAND='security find-generic-password -s hex-restic -a "$USER" -w'

# 3. Initialize the repo (first time only).
restic init
```

Set `RESTIC_REPOSITORY` and `RESTIC_PASSWORD_COMMAND` in the harness env so the worker
inherits them. The first snapshot is several GB (the workspace `.git` alone is ~4 GB);
subsequent runs are cheap via dedup.

If the gdrive virtual mount proves slow or lock-flaky, switch the repo to restic's rclone
backend (Drive API direct, one-time OAuth): `export RESTIC_REPOSITORY="rclone:<remote>:hex-restic"`.

### Failure behavior (loud — SO-S6)

Any restic step failing routes through `alert::notify("backup-offsite", …)` (stderr +
telemetry row + macOS notification, deduped 6 h) and the command exits non-zero. restic
stderr is surfaced, never swallowed — the old gdrive worker died *silently* (it called a
`backup-to-gdrive.sh` that didn't exist); this one cannot.

## Restore (fresh machine)

A backup you can't restore is theater. To rebuild on a new machine:

```sh
brew install restic
export RESTIC_REPOSITORY="…/My Drive/hex-restic"
export RESTIC_PASSWORD_COMMAND='…'      # or paste the password when prompted

restic snapshots                        # confirm the repo + list snapshots
restic restore latest --target /tmp/hex-restore   # restore the operating layer
# then move $HEX_DIR, ~/.boi, ~/.claude/projects into place from /tmp/hex-restore
```

The round-trip (backup → snapshot → restore → byte-identical file) is covered by
`offsite_roundtrip_local_repo` in `backup.rs` (gated on restic being installed).
