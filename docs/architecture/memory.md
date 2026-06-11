<!--
verified-against: 3c8f6b14 (2026-06-11)
source-paths: system/harness/src/memory/, system/harness/src/consolidate.rs, system/harness/src/hook/capture.rs, system/harness/src/hook/user_prompt_submit.rs, system/harness/src/modules/memory_maintenance.worker.rs, system/harness/src/modules/backup.worker.rs
-->
# Memory Pipeline Architecture

> Hex's memory is one SQLite database fed by cron jobs inside the harness daemon.
> Conversations and workspace files flow IN through capture → index → distill;
> context flows OUT through recency pointers, per-message recall (the context
> assembler), and `hex memory search`. Claims below were fact-checked against the
> code at the commit above by an adversarial review pass (2026-06-11); where behavior
> is instance-configurable, the **code default** is stated.

```
 WRITE SIDE                                          READ SIDE
 Stop hook ─► raw/transcripts/*.jsonl
 cron      ─► raw/transcripts/YYYY-MM-DD.md          SessionStart ─► hex memory recent
                    │                                              (recency pointers, no LLM)
                    ▼
 consolidate quick (q15m) ── distill: watermark ──►  UserPromptSubmit ─► recall
   slice → LLM extract → dedup/judge → facts           (ContextAssembler: M1–M4 moves)
                    │
 index (q15m) ── chunk + embed workspace files ──►  hex memory search
                    │                                  (FTS + KNN fused via RRF,
                    ▼                                   chunks AND facts)
              .hex/memory.db
   (chunks FTS5 · vec_chunks · facts · facts_vec)
```

## Storage

Primary store: **`$HEX_DIR/.hex/memory.db`** (SQLite, WAL, schema v4 — DDL in
`memory/schema.rs` + `memory/index.rs`; version tracked in a `schema_version` table).

| Table | What it holds |
|---|---|
| `files` | One row per indexed workspace file (path, mtime, content_hash, chunk_count) |
| `chunks` (FTS5) | Chunk text + heading/path metadata + `private` flag — the keyword arm |
| `chunk_meta` | Per-chunk source weight for BM25 scaling |
| `vec_chunks` (vec0) | 768-dim embedding per chunk, rowid == chunk rowid — the semantic arm |
| `facts` | Subject–predicate–object triples: importance, `private`, tombstone, access counters |
| `facts_fts` (FTS5) | Keyword arm over facts (used by `hex memory search`) |
| `facts_vec` (vec0) | 768-dim embedding per fact (`fact_id` TEXT PK) — populated by maintain backfill |
| `fact_history` | ADD/UPDATE/DELETE/FLAG audit trail per fact |
| `transcript_files` | Distill watermarks: `path` (PK, **absolute**), `last_offset`, `last_distilled_at` (drives catch-up re-scan), `consecutive_failures` |
| `messages` | Agent message/ask-answer queue (created by every `open_db` — rides the same backup/vacuum surface) |
| `metadata` | K/V incl. `last_consolidated` (any L2 completion) and `last_full_consolidated` (**error-free** full runs only — a full run that exits 1 does not stamp, so liveness checks catch partially-failed nights) |
| `sessions`, `topics`, `fact_topics` | Plan-2 DDL reserved for session/topic rollups — **currently unpopulated** (the topic-rollup op is a not-yet-implemented stub) |

Adjacent stores: telemetry events in `.hex/telemetry/events.db` (append-only, separate
DB); daily snapshots in `.hex/backups/YYYY-MM-DD/` (keep 7); embedder model cache in
`.fastembed_cache/`; per-prompt recall log `.hex/memory/recall-log.jsonl` (grows
unbounded — see Operations); two advisory flock files, `.hex/memory-consolidate.lock`
and `.hex/memory-index.lock` (0-byte files persist by design — the kernel releases the
lock on process exit; **presence does not mean held**).

**Privacy model:** path rules at index/extract time set the `private` flag on chunks
and facts (e.g. `me/decisions/`, `people/`); recall/assembly filters on it when
building agent-facing context. A new read surface MUST honor the `private` columns.

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
`raw/transcripts/`. When stdin is unusable it falls back to an env fast-path
(silently), then to a newest-`.jsonl` scan (announced on stderr — race-prone with
concurrent sessions). Every failure path emits stderr + a `hook::capture` telemetry
event; the hook itself always exits 0 (a failed backup never blocks the session).

**2. Parse (cron).** `parse-transcripts` renders the raw `.jsonl` into readable
`raw/transcripts/YYYY-MM-DD.md` dailies — these dailies are what distill consumes.

**3. Distill (`memory/distill/`, driven by consolidate).** The transcript backstop
(`memory/consolidate.rs::op_transcript_backstop`) registers `raw/transcripts/*.md` in
`transcript_files` (**absolute paths are canonical**) and, per file, processes **at
most one budget-capped slice per invocation** from the watermark forward:

- Slice → LLM **extract** (S/P/O candidates; transport per `.hex/config/llm.toml` —
  the **code default is `http`** (OpenAI-compatible, OpenRouter via
  `OPENROUTER_API_KEY`); `claude-cli` with keychain auth is an opt-in override the
  operating instance uses — the prompt streams to `claude -p` on **stdin**, never
  argv, which is capped at 128 KB per string on Linux: mrap/hex#7) → **deterministic dedup** (exact S/P/O match → NOOP, plain
  SQL, no LLM) → LLM **judge** for the ambiguous remainder (ADD/UPDATE/FLAG) → insert
  into `facts` + `fact_history` → advance `last_offset`.
- Failure escalation: on strikes 1–2 the watermark stays put and the next tick retries
  from the same offset with the input budget **bisected per strike** (`base >> strikes`,
  floor 2,000 tokens) — so the retried slice is re-capped smaller each time. Strike 3
  **loudly skips** the current (smaller) slice — watermark advances past it (an
  explicit data-loss escape hatch), strikes reset. Every slice emits a
  `distill::slice` telemetry event with `path= offset= bytes= est_tokens= strikes=`
  so skipped spans are recoverable forensically (rewind = set `last_offset` back;
  dedup absorbs re-extraction, so re-running a span never duplicates facts).
- The whole backstop loop has a **10-minute wall-clock budget** per quick tick
  (`BACKSTOP_BUDGET`); leftovers wait for the next tick, so a deep backlog can never
  starve the nightly run of the lock.

**4. Index (`memory/index.rs`).** Scans workspace `*.md` and `*.txt` (skip-listed:
`.hex`, `.claude`, `.sessions`, `.git`, `node_modules`, `_archive`, `hex-archive`),
chunks changed files, embeds chunks (fastembed `NomicEmbedTextV15`, 768-dim, ~1.6s
cold model load per CLI invocation), and writes both arms. Re-indexing a file deletes
its old chunks AND their vectors (orphan-safe). If embedding fails, the chunk stays
FTS5-only and the error is printed; **every index run ends with a backfill pass** that
re-embeds up to 500 vectorless chunks, so embed failures self-heal instead of
persisting until the file changes.

## Consolidation layers

`hex memory consolidate {quick|full}` (`consolidate.rs`) is the umbrella job:

| Layer | Runs in | What it does |
|---|---|---|
| L1 structural | quick + full | Deterministic workspace lint (`doctor/consolidate.rs`): broken links, orphan projects, evolution files, **audit-artifact freshness** (48h window on `evolution/consolidation-audit-*.md` — an L1 check, *not* a registered `hex doctor` check). Writes `evolution/consolidation-latest.log`. |
| L2 memory DB | quick + full | Transcript backstop (above) first, then the standard op list — incl. catch-up distill (files untouched >1 day re-scanned via `last_distilled_at`) and **prune**: facts with `access_count = 0` older than 60 days are tombstoned. ⚠️ Nothing currently increments `access_count`, so 60-day expiry is effectively universal for non-exempt facts — factor this into any retention reasoning. Stamps `metadata.last_consolidated` on completion. |
| L2.5 learnings promotion | quick + full | Scans `me/learnings.md` + `raw/reflections/` for recurring clusters → writes promotion candidates to `evolution/suggestions.md`. State in `evolution/.pending-promotions.json` (deleting it causes duplicate re-suggestions). |
| L3 operating-model audit | **full only** | LLM audit of CLAUDE.md + learnings (provider profile `consolidate_audit`); writes `evolution/consolidation-audit-YYYY-MM-DD.md` + appends `consolidation-log-YYYY-MM-DD.md`. Findings-only — never edits sources. Failure exits 1 without undoing L1/L2. |

## Read path

| Surface | Mechanism |
|---|---|
| `hex memory recent` | Live filesystem recency scan — pointers only, no DB, no LLM, target <200ms |
| Recall (UserPromptSubmit hook) | **ContextAssembler** (`assemble.rs`), four parallel moves merged by confidence with a coverage floor and per-move quotas: M1 content match (FTS5 hits first, then KNN hits appended with dedup — **no RRF on this path**; M1 *attempts* an embedder load per message, falling back loudly to keyword-only), M2 entity-subject facts, M3 predicate-cue facts, M4 temporal facts (direct SQL over `facts`, importance/recency ordered — not FTS). Trivial/short prompts deliberately get **zero injection**; output hard-capped at 10k chars; per-move stats logged to `.hex/memory/recall-log.jsonl`. |
| `hex memory search` | Embeds the query once; chunks AND facts each get FTS+KNN arms fused with **RRF (k=60)**; facts print in a dedicated section |

The KNN arm applies a relevance floor: hits with L2 distance > `KNN_MAX_DISTANCE`
(1.15; override `HEX_KNN_MAX_DISTANCE`) are dropped, so garbage queries return empty
rather than confident noise. RRF means a #1 search result scores ~0.033 (= 2/(60+1)) —
**0.03 is the maximum, not a low score.**

## Failure surfaces

The governing rule is S6 — no quiet failures. The specific semantics:

- **Findings ≠ failure.** `hex memory consolidate` exits 1 only on *operational*
  errors (DB unopenable, op/LLM hard failure, full-mode lock timeout). L1 *findings*
  (broken links, orphan projects) are reported in output/artifacts but never fail the
  run (`consolidate.rs::exit_code_for`). History: conflating these produced 472
  consecutive "error" crons that masked real breakage.
- **Lock discipline.** Quick: `try_lock`, on contention records telemetry status
  `skipped-lock` (exit 0 — overlap is normal). Full: **waits up to 45 min** polling
  every 15s; on timeout records `lock-timeout`, fires an alert, exits 1. The nightly
  can no longer be silently skipped.
- **Alerts (`alert.rs`).** stderr + telemetry row + macOS notification, deduped per
  key (6h window via stamp files in `.hex/run/alerts/`). Never fails the caller.
- **Doctor checks** (registered in `doctor/runner.rs`): `consolidate-liveness`
  (`last_consolidated` > 48h → FAIL), `nightly-full-liveness`
  (`last_full_consolidated` > 26h → FAIL + alert), `distill-strikes` (any file with
  strikes → WARN), `memory-db` existence.
- **Child processes.** `claude -p` extract children run in their own process group
  (for timeout kill) and write pidfiles to `.hex/run/distill/`; harness startup runs a
  **reaper** (`reaper.rs`) that kills orphaned children (alive + ppid 1 + "claude" in
  argv — identity-checked to survive PID reuse) and records each kill. Harness drain
  timeouts record a `drain::timeout` telemetry event naming how many handlers died
  in-flight.
- **Telemetry detail truncation:** harness worker `Ctx::run` children keep
  head+tail of stderr (600+400 chars — heads carry file paths, tails carry exit
  reasons); `claude -p` distill children keep the last 800 chars (tail-only).

## Operations

Health: `hex memory stats` (files/facts counts, DB size, the `last_consolidated`
stamp — `last_full_consolidated` is surfaced by doctor's nightly-full-liveness, not
stats — backfill pending bytes, **unembedded chunks, orphan vectors**; a `-1` means
the gap-query itself failed and is printed deliberately), `hex doctor`, and
`sqlite3 "file:.hex/telemetry/events.db?mode=ro" "SELECT * FROM events WHERE source LIKE 'memory%' ORDER BY id DESC LIMIT 20"`.

When "memory didn't inject": check (in order) the trivial-prompt gate (short prompts
get zero injection by design), the 10k-char cap, then
`.hex/memory/recall-log.jsonl` (per-prompt move stats; unbounded growth — rotation is
an open follow-up).

Maintenance (`hex memory maintain`, weekly cron or on demand): orphan-vector sweep,
FTS5 `optimize` on both FTS tables, `transcript_files` hygiene (folds non-canonical
transcript-shaped rows — relative paths and stale absolute prefixes from an old
hex_dir — into the canonical **absolute** row keeping the furthest watermark via
`MAX(last_offset)`; purges non-transcript paths; note: a deliberate watermark
*rewind* should therefore be followed by a distill catch-up before the next weekly
maintain, or the fold may restore the higher offset), `--backfill-facts` (embeds facts
missing from `facts_vec`, prunes tombstoned ones), `--vacuum` (rebuilds the file;
reclaims dead vec slots + FTS segments + freelist).

Backups (`hex backup`, daily cron): online-safe snapshots via the sqlite backup API
(WAL-correct) of `memory.db`, `telemetry/events.db`, `ledger/ledger.db` into
`.hex/backups/YYYY-MM-DD/`, keep newest 7.

Recovery levers: watermark rewind (`UPDATE transcript_files SET last_offset = <n>,
consecutive_failures = 0 WHERE path = …` — raw SQL is the right lever; the in-code
advance is deliberately monotonic), skipped-slice forensics via `distill::slice`
events (`offset=` since 2026-06-11), `maintain --backfill-facts` / index's vector
backfill for embedding gaps.

## Lineage

- `docs/superpowers/plans/2026-06-11-memory-pipeline-holistic-fix.md` — the 12-task
  plan that produced the current failure-surface semantics (from a 22-finding
  assessment; FIX-007..011 in the operating instance).
- Recall = parallel moves + confidence merge, "keep the heuristic simple, iterate"
  (context-assembly decision 2026-06-04); distill = windowed spans, not naive chunking
  (2026-06-09); claude-cli keychain transport as instance opt-in (2026-06-10).
- Stack: sqlite-vec hybrid (FTS5 + vec0) with fastembed nomic embeddings; facts layer
  per "Plan 2" v2-memory design (2026-05-20→06-04).
