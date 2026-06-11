<!--
verified-against: f6d0cfb3 (2026-06-11)
source-paths: system/harness/src/memory/, system/harness/src/consolidate.rs, system/harness/src/hook/capture.rs, system/harness/src/modules/memory_maintenance.worker.rs, system/harness/src/modules/backup.worker.rs
-->
# Memory Pipeline Architecture

> Hex's memory is one SQLite database fed by cron jobs inside the harness daemon.
> Conversations and workspace files flow IN through capture → index → distill;
> context flows OUT through recency pointers, per-message recall, and search.
> Everything below was verified line-by-line against the code at the commit above
> (the post-2026-06-11 "holistic fix" state — see Lineage).

```
 WRITE SIDE                                          READ SIDE
 Stop hook ─► raw/transcripts/*.jsonl
 cron      ─► raw/transcripts/YYYY-MM-DD.md          SessionStart ─► hex memory recent
                    │                                              (recency pointers, no LLM)
                    ▼
 consolidate quick (q15m) ── distill: watermark ──►  UserPromptSubmit ─► recall
   slice → LLM extract → judge → facts                 (chunks: FTS+KNN fused; facts: FTS)
                    │
 index (q15m) ── chunk + embed workspace files ──►  hex memory search
                    │                                  (chunks AND facts, both arms fused)
                    ▼
              .hex/memory.db
   (chunks FTS5 · vec_chunks · facts · facts_vec)
```

## Storage

Primary store: **`$HEX_DIR/.hex/memory.db`** (SQLite, WAL, schema v4 — DDL in
`memory/schema.rs` + `memory/index.rs`).

| Table | What it holds |
|---|---|
| `files` | One row per indexed workspace file (path, mtime, content_hash, chunk_count) |
| `chunks` (FTS5) | Chunk text + heading/path metadata — the keyword arm |
| `chunk_meta` | Per-chunk source weight for BM25 scaling |
| `vec_chunks` (vec0) | 768-dim embedding per chunk, rowid == chunk rowid — the semantic arm |
| `facts` | Subject–predicate–object triples with importance, tombstone, access counters |
| `facts_fts` (FTS5) | Keyword arm over facts |
| `facts_vec` (vec0) | 768-dim embedding per fact (`fact_id` TEXT PK) — populated by maintain backfill |
| `fact_history` | ADD/UPDATE/DELETE/FLAG audit trail per fact |
| `transcript_files` | Distill watermarks: `path` (PK, **absolute**), `last_offset`, `consecutive_failures` |
| `metadata` | K/V incl. `last_consolidated` (any L2 completion) and `last_full_consolidated` (full-run completion) |
| `sessions`, `topics`, `fact_topics` | Session/topic rollups |

Adjacent stores: telemetry events in `.hex/telemetry/events.db` (append-only, separate
DB); daily snapshots in `.hex/backups/YYYY-MM-DD/` (7-day rotation); embedder model
cache in `.fastembed_cache/`; advisory flock file `.hex/memory-consolidate.lock`
(0-byte file persists by design — the kernel releases the lock on process exit; its
presence does NOT mean it is held).

## Jobs

All scheduled inside the harness daemon (`com.hex.harness` → in-process iii engine;
7-field cron, UTC). Defined in `modules/memory_maintenance.worker.rs` and
`modules/backup.worker.rs`.

| Job | Schedule | Invokes |
|---|---|---|
| index | `:00/:15/:30/:45` | `hex memory index` |
| consolidate quick | `:05/:20/:35/:50` (offset so it never collides with the nightly) | `hex memory consolidate quick` |
| parse-transcripts | `:00/:15/:30/:45` | `hex memory parse-transcripts` |
| consolidate full | 03:00Z daily | `hex memory consolidate full` |
| backup | 04:00Z daily | `hex backup` |
| maintain | Sun 04:30Z weekly | `hex memory maintain --vacuum --backfill-facts` |

Event-driven (Claude Code hooks): `Stop` → `hex hook capture`;
`UserPromptSubmit` → recall injection; `SessionStart` → `hex memory recent`.

## Write path

**1. Capture (`hook/capture.rs`).** On every session Stop, the hook payload's stdin
JSON `transcript_path` is the authoritative source — the live `.jsonl` is copied to
`raw/transcripts/`. Fallbacks (env fast-path, then newest-`.jsonl` scan) only fire
when stdin is unusable, and say so. Every failure path emits stderr + a
`hook::capture` telemetry event; the hook itself always exits 0 (a failed backup never
blocks the session).

**2. Parse (cron).** `parse-transcripts` renders the raw `.jsonl` into readable
`raw/transcripts/YYYY-MM-DD.md` dailies — these dailies are what distill consumes.

**3. Distill (`memory/distill/`, driven by consolidate).** The transcript backstop
(`memory/consolidate.rs::op_transcript_backstop`) registers `raw/transcripts/*.md` in
`transcript_files` (**absolute paths are canonical**) and, per file, processes **at
most one budget-capped slice per invocation** from the watermark forward:

- Slice → LLM **extract** (S/P/O candidates; transport per `.hex/config/llm.toml`,
  default claude-cli with keychain auth) → **judge** (ADD/UPDATE/NOOP/FLAG against
  existing facts) → insert into `facts` + `fact_history` → advance `last_offset`.
- Failure escalation: strikes 1–2 retry the same slice with a halved budget; strike 3
  **loudly skips** the slice (watermark advances past it — an explicit data-loss
  escape hatch), resets strikes. Every slice emits a `distill::slice` telemetry event
  with `path= offset= bytes= est_tokens= strikes=` so skipped spans are recoverable
  forensically (rewind = set `last_offset` back; the judge dedupes re-extraction).
- The whole backstop loop has a **10-minute wall-clock budget** per quick tick
  (`BACKSTOP_BUDGET`); leftovers wait for the next tick, so a deep backlog can never
  starve the nightly run of the lock.

**4. Index (`memory/index.rs`).** Scans workspace markdown (skip-listed: `.hex`,
`.git`, `node_modules`, `_archive`, …), chunks changed files, embeds chunks
(fastembed `NomicEmbedTextV15`, 768-dim, ~1.6s cold model load per CLI invocation),
and writes both arms. Re-indexing a file deletes its old chunks AND their vectors
(orphan-safe). If embedding fails, the chunk stays FTS5-only and the error is printed;
**every index run ends with a backfill pass** that re-embeds up to 500 vectorless
chunks, so embed failures self-heal instead of persisting until the file changes.

## Read path

| Surface | Mechanism |
|---|---|
| `hex memory recent` | Live filesystem recency scan — pointers only, no DB write, no LLM, <200ms |
| Recall (UserPromptSubmit hook) | Chunks: FTS5 (BM25 × source_weight) + KNN over `vec_chunks`, fused with RRF (k=60). Facts: FTS-only **by design** (hot-path latency budget — no embedder load per message) |
| `hex memory search` | Embeds the query once; chunks AND facts both get FTS+KNN arms, each fused with RRF; facts output in a dedicated section |

The KNN arm applies a relevance floor: hits with L2 distance > `KNN_MAX_DISTANCE`
(1.15; override `HEX_KNN_MAX_DISTANCE`) are dropped, so garbage queries return empty
rather than confident noise. RRF means a #1 result scores ~0.033 (= 2/(60+1)) —
**0.03 is the maximum, not a low score.**

## Failure surfaces

The governing rule is S6 — no quiet failures. The specific semantics:

- **Findings ≠ failure.** `hex memory consolidate` exits 1 only on *operational*
  errors (DB unopenable, op/LLM hard failure, full-mode lock timeout). Layer-1 doctor
  *findings* (broken links, orphan projects) are reported in output/artifacts but never
  fail the run (`consolidate.rs::exit_code_for`). History: conflating these produced
  472 consecutive "error" crons that masked real breakage.
- **Lock discipline.** Quick: `try_lock`, on contention records telemetry status
  `skipped-lock` (exit 0 — overlap is normal). Full: **waits up to 45 min** polling
  every 15s; on timeout records `lock-timeout`, fires an alert, exits 1. The nightly
  can no longer be silently skipped.
- **Alerts (`alert.rs`).** stderr + telemetry row + macOS notification, deduped per
  key (6h window via stamp files in `.hex/run/alerts/`). Never fails the caller.
- **Doctor checks.** `consolidate-liveness` (`last_consolidated` > 48h → FAIL),
  `nightly-full-liveness` (`last_full_consolidated` > 26h → FAIL + alert),
  `distill-strikes` (any file with strikes → WARN), audit-artifact freshness (48h
  window on `evolution/consolidation-audit-*.md`), `memory-db` existence.
- **Child processes.** `claude -p` extract children run in their own process group
  (for timeout kill) and write pidfiles to `.hex/run/distill/`; harness startup runs a
  **reaper** (`reaper.rs`) that kills orphaned children (alive + ppid 1 + "claude" in
  argv — identity-checked to survive PID reuse) and records each kill. Harness drain
  timeouts record a `drain::timeout` telemetry event naming how many handlers died
  in-flight.
- **Telemetry detail** keeps head+tail of child stderr (600+400 chars) — error heads
  carry file paths; tails carry exit reasons.

## Operations

Health: `hex memory stats` (files/facts counts, DB size, last-consolidated stamps,
backfill pending bytes, **unembedded chunks, orphan vectors** — nonzero gaps are
visible by design), `hex doctor`, and
`sqlite3 "file:.hex/telemetry/events.db?mode=ro" "SELECT * FROM events WHERE source LIKE 'memory%' ORDER BY id DESC LIMIT 20"`.

Maintenance (`hex memory maintain`, weekly cron or on demand): orphan-vector sweep,
FTS5 `optimize` on both FTS tables, `transcript_files` hygiene (folds relative-path
duplicates into the canonical **absolute** rows, purges non-transcript paths),
`--backfill-facts` (embeds facts missing from `facts_vec`, prunes tombstoned ones),
`--vacuum` (rebuilds the file; reclaims dead vec slots + FTS segments + freelist).

Backups (`hex backup`, daily cron): online-safe snapshots via the sqlite backup API
(WAL-correct) of `memory.db`, `telemetry/events.db`, `ledger/ledger.db` into
`.hex/backups/YYYY-MM-DD/`, keep newest 7.

Recovery levers: watermark rewind (`UPDATE transcript_files SET last_offset = <n>,
consecutive_failures = 0 WHERE path = …` — judge dedupes re-extraction, cost ≈ cents
per MB); skipped-slice forensics via `distill::slice` events (`offset=` since
2026-06-11); `maintain --backfill-facts` / index's vector backfill for embedding gaps.

## Lineage

- `docs/superpowers/plans/2026-06-11-memory-pipeline-holistic-fix.md` — the 12-task
  plan that produced the current failure-surface semantics (from a 22-finding
  assessment; FIX-007..011 in the operating instance).
- Distill slice design: windowed spans, not naive chunking (instance decision
  2026-06-09); claude-cli keychain transport, no setup-token (2026-06-10).
- Earlier stack selection: sqlite-vec hybrid (FTS5 + vec0) with fastembed nomic
  embeddings; facts layer per "Plan 2" v2-memory design (2026-05-20→06-04).
