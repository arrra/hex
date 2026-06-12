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

The target is **Backblaze B2** (decided 2026-06-12 — restic-native, instant-read, no minimum
storage duration, no OAuth; see the backend comparison in the design doc). restic is
backend-agnostic — the repo is just `RESTIC_REPOSITORY`, so switching targets later is a
config change, not a rebuild. The job is a **deliberate no-op until `RESTIC_REPOSITORY` is
set** — it prints a "not configured" line and exits 0, so it never false-alarms before setup.
To enable:

```sh
# 1. One-time, in the Backblaze console: create a private bucket (e.g. mrap-hex-restic) and
#    an Application Key scoped to it. Note the keyID and applicationKey.

# 2. Point restic at the B2 repo + give it the B2 creds (restic's native B2 env vars).
export RESTIC_REPOSITORY="b2:mrap-hex-restic:hex-restic"   # b2:<bucket>:<path-in-bucket>
export B2_ACCOUNT_ID="<keyID>"
export B2_ACCOUNT_KEY="<applicationKey>"

# 3. Repo encryption password in the macOS Keychain (separate from the B2 creds);
#    restic reads it via RESTIC_PASSWORD_COMMAND.
security add-generic-password -s hex-restic -a "$USER" -w   # prompts for the password
export RESTIC_PASSWORD_COMMAND='security find-generic-password -s hex-restic -a "$USER" -w'

# 4. Initialize the repo (first time only).
restic init
```

Set `RESTIC_REPOSITORY`, `B2_ACCOUNT_ID`, `B2_ACCOUNT_KEY`, and `RESTIC_PASSWORD_COMMAND` in
the harness env so the worker inherits them (keep the B2 creds out of source — Keychain or an
untracked env file). The first snapshot is several GB (the workspace `.git` alone is ~4 GB);
subsequent runs are cheap via dedup.

**Fallbacks (same code, just a different `RESTIC_REPOSITORY` — no rebuild):**
- *Stay in Google Cloud:* GCS Coldline bucket — `RESTIC_REPOSITORY="gs:<bucket>:hex-restic"`
  + `GOOGLE_APPLICATION_CREDENTIALS` (service-account key). Instant-read, 11-nines durability.
- *Consumer Google Drive (no object storage):* rclone backend (Drive API, one-time OAuth) —
  `rclone:gdrive:hex-restic`; or the locally-mounted Drive as a plain path (less reliable —
  virtual-mount consistency/locking).
- Never use S3 Glacier Flexible/Deep Archive — restic can't read archived packs without a thaw.

### Encryption & key custody

restic encrypts the **entire repo client-side (AES-256)** before upload — B2 only ever
stores ciphertext and never holds the key. That is the encryption that matters; B2's
bucket-level Server-Side Encryption (SSE-B2) would just re-encrypt already-encrypted blobs
with Backblaze-held keys — redundant, though free and harmless to enable.

**The `RESTIC_PASSWORD` is the single point of failure: lose it and every snapshot is
permanently unrecoverable** (no escrow, no reset). Keychain serves the running worker; keep a
**second copy in a password manager** (and ideally one offline). The B2 Application Key is
re-issuable; the restic password is not.

### Failure behavior (loud — SO-S6)

Any restic step failing routes through `alert::notify("backup-offsite", …)` (stderr +
telemetry row + macOS notification, deduped 6 h) and the command exits non-zero. restic
stderr is surfaced, never swallowed — the old gdrive worker died *silently* (it called a
`backup-to-gdrive.sh` that didn't exist); this one cannot.

## Restore (fresh machine)

A backup you can't restore is theater. To rebuild on a new machine:

```sh
brew install restic
export RESTIC_REPOSITORY="b2:mrap-hex-restic:hex-restic"
export B2_ACCOUNT_ID="<keyID>"; export B2_ACCOUNT_KEY="<appKey>"
export RESTIC_PASSWORD='<from your password manager>'   # the irreplaceable repo password

restic snapshots                        # confirm the repo + list snapshots
restic restore latest --target /tmp/hex-restore   # restore the operating layer
# then move $HEX_DIR, ~/.boi, ~/.claude/projects into place from /tmp/hex-restore
```

The round-trip (backup → snapshot → restore → byte-identical file) is covered by
`offsite_roundtrip_local_repo` in `backup.rs` (gated on restic being installed).
