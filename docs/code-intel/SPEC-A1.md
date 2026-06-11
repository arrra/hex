# scipd/cq — Code Intelligence Phase A1 — Specification

**Date:** 2026-06-11
**Branch:** `feature/code-intel-a1`
**Design source:** personal-instance `projects/system-improvement/research/2026-06-11-code-intel-optimal-design.md` (blind design competition, SCIPHQ winner + grafts)
**Phase:** A1 = index-only. No live rust-analyzer pool (A2), no daemon process (deferred — see Deviations).

---

## 1. What we are building

A **stateless, index-backed code-intelligence query tool** for autonomous agent fleets:

- An **indexer** that runs `rust-analyzer scip` over a registered Rust workspace, ingests the
  SCIP protobuf into SQLite, and atomically publishes immutable **generations** under
  `~/.codeintel/<workspace-id>/<generation>/`.
- A **CLI, `cq`**, that answers semantic queries (`def`, `refs`, `callers`, `symbols`, `search`)
  by reading the SQLite index directly — no daemon, no shared mutable state, safe for any
  number of concurrent consumers.
- A **git-blob-OID freshness layer**: every response is checked per-file against the querying
  worktree's actual blob OIDs; stale files are flagged loudly, never silently served.
- **Ephemeral-worktree native:** a fresh worktree resolves to its parent workspace via
  `git rev-parse --git-common-dir` and queries the existing index instantly. Worktree cold
  start = milliseconds. No per-worktree index state exists, so teardown is free.

### Consumers (in order of arrival)
1. Today: agents shell out to `cq` (JSON output). No MCP shim in A1.
2. Phase B: the hex Rust orchestrator links `scipd_core` (the lib inside this crate) directly.

## 2. What we are NOT building (A1 exclusions)

- NO live rust-analyzer pool, NO rename verb, NO diagnostics (`cq check`) — those are A2.
- NO UDS daemon. **Deviation from the design report (recorded):** the report's own §3.2 states
  "the daemon is an optimization, not a correctness requirement; `cq --no-daemon` can open the
  SQLite directly." A1 ships the direct-read path only. The lib/bin split keeps the daemon
  addable in A2 without rework.
- NO Python/TypeScript emitters (smoke test #5 deferred).
- NO MCP shim (agents use their shell tool; revisit only if prompt-side friction shows up).

## 3. Architecture (A1 cut)

```
agent / orchestrator
   │  shells out
   ▼
cq (bin)  ──────────────┐
   │ uses               │ `cq index` (manual / launchd / git hook)
   ▼                    ▼
scipd_core (lib)     indexer module (in scipd_core)
   │ read-only SQLite     │ rust-analyzer scip → ingest → atomic swap
   ▼                      ▼
~/.codeintel/<workspace-id>/<generation>/{index.sqlite, manifest.json}
~/.codeintel/<workspace-id>/CURRENT          (file containing current generation name; atomic rename)
~/.codeintel/registry.toml                   (registered workspaces)
```

- **Crate:** new cargo workspace member `system/code-intel/` — package `scipd`, lib name
  `scipd_core`, one bin `cq`. Single crate (no premature multi-crate split).
- **Supervise the binary:** `rust-analyzer` is invoked as a subprocess (never link `ra_ap_*`).
- **Workspace identity:** `workspace-id = first 12 hex chars of sha256(realpath of primary
  checkout root)`. The primary root is derived from `git rev-parse --git-common-dir` (strip
  trailing `/.git`, follow `worktrees/<name>` back to the parent repo).
- **Generations are immutable:** ingest writes to `<gen>.tmp/`, fsyncs, renames to `<gen>/`,
  then atomically rewrites `CURRENT`. Keep the 2 most recent generations; prune older.

## 4. SQLite schema (generation-scoped, read-only after publish)

```sql
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
-- keys: schema_version=1, workspace_root, commit_sha, emitter (rust-analyzer version string),
--       created_at (RFC3339), emit_exit_code, emit_duration_secs, file_count, symbol_count

CREATE TABLE files (
  id INTEGER PRIMARY KEY,
  path TEXT NOT NULL UNIQUE,          -- relative to workspace root
  blob_oid TEXT NOT NULL,             -- git blob OID at index time
  language TEXT NOT NULL
);

CREATE TABLE symbols (
  id INTEGER PRIMARY KEY,
  scip_symbol TEXT NOT NULL UNIQUE,   -- full SCIP symbol string
  display_name TEXT NOT NULL,
  kind INTEGER NOT NULL DEFAULT 0,    -- scip SymbolInformation.kind
  documentation TEXT                  -- first doc string, may be NULL
);

CREATE TABLE occurrences (
  file_id INTEGER NOT NULL REFERENCES files(id),
  symbol_id INTEGER NOT NULL REFERENCES symbols(id),
  start_line INTEGER NOT NULL,        -- 0-based, SCIP convention
  start_col INTEGER NOT NULL,
  end_line INTEGER NOT NULL,
  end_col INTEGER NOT NULL,
  roles INTEGER NOT NULL,             -- SCIP SymbolRole bitfield (1 = definition)
  enclosing_symbol_id INTEGER REFERENCES symbols(id)  -- derived; NULL if underivable
);
CREATE INDEX idx_occ_symbol ON occurrences(symbol_id, roles);
CREATE INDEX idx_occ_file_pos ON occurrences(file_id, start_line);

CREATE VIRTUAL TABLE symbols_fts USING fts5(display_name, content='symbols', content_rowid='id');
```

`enclosing_symbol_id` derivation at ingest: for each reference occurrence, the enclosing symbol
is the definition occurrence in the same file whose range most tightly contains the reference
(smallest containing `[start,end)` span among definition occurrences), **excluding module-like
definitions** (SCIP kinds Module/Namespace/Package) so `use`-import lines inside inline modules
don't masquerade as callers. **NEVER use reference-side SCIP `enclosing_range`** — smoke test #2
(2026-06-11, `research/smoke-tests/2026-06-11-scip-callers-quality.md`) proved it contains the
*referenced definition's* range, not the call site's enclosing scope. Containment-only.
Smoke test verdict: callers() ships from the index cleanly (0% false negatives on macro-heavy
ground truth incl. `assert!`/`anyhow!`/`tokio::spawn` cases); no `quality: "best-effort"` flag
needed in the default path.

## 5. CLI surface (all verbs emit the envelope; `--json` is the default and only output mode)

```
cq def      <FILE:LINE:COL | symbol-name>     definition site(s)
cq refs     <FILE:LINE:COL | symbol-name>     all reference sites (definitions flagged)
cq callers  <symbol-name | FILE:LINE:COL>     enclosing symbols of call/reference sites
cq symbols  <FILE>                            symbol outline of one file
cq search   <QUERY>                           FTS5 prefix/fuzzy search over display names
cq index    [--workspace PATH]                emit + ingest + swap (debounced: one in flight)
cq register <PATH>                            add workspace to ~/.codeintel/registry.toml
cq doctor   [--json]                          health: per-workspace index age, commit lag,
                                              last emit status, generation list; exit ≠0 on red
```

- Positional `FILE:LINE:COL` is 1-based on the CLI (converted to SCIP 0-based internally).
- All query verbs run from inside any worktree of a registered workspace; `cq` resolves the
  workspace automatically. `--workspace PATH` overrides.

### Response envelope (every query reply, stdout, single JSON object)

```json
{
  "source": "index",
  "workspace_id": "ab12cd34ef56",
  "indexed_commit": "9b70565a…",
  "index_age_secs": 4210,
  "stale_files": ["system/harness/src/main.rs"],
  "latency_ms": 14,
  "results": [ { "path": "…", "line": 120, "col": 8, "symbol": "…", "display_name": "…",
                 "kind": "function", "role": "definition", "snippet": "fn consolidate(…" } ]
}
```

`snippet` = the source line, read from the **worktree** when the file is fresh, from nothing
(field omitted + file listed in `stale_files`) when stale.

### Error taxonomy & exit codes (loud failures — Standing Order S6)

| Condition | stderr JSON `error.code` | exit |
|---|---|---|
| OK, all result files fresh | — | 0 |
| OK, but ≥1 result file stale (or `--strict` refused) | `STALE_RESULTS` annotation / refusal | 2 |
| No index / `CURRENT` missing / SQLite unopenable | `NO_INDEX` | 3 |
| CWD not in a registered workspace | `UNREGISTERED_WORKSPACE` | 4 |
| Workspace registered but not a Rust workspace | `UNSUPPORTED_WORKSPACE` | 4 |
| Symbol/position resolves to nothing | `NOT_FOUND` | 5 |
| Emit subprocess failed (cq index) | `EMIT_FAILED` + captured stderr tail | 6 |

Never exit 0 with empty results due to an internal failure. Every error is a structured JSON
object on stderr with `code`, `message`, `hint`.

## 6. Freshness (the worktree answer)

1. On every query, run `git ls-files -s -- <paths in results>` (or full tree when cheap) in the
   querying worktree → per-file blob OIDs (one subprocess, no content hashing).
2. Compare against `files.blob_oid`. Mismatch or untracked-modification → file goes to
   `stale_files`. `--strict` → exit 2 with no results.
3. Files not in the index at all (new files) → listed under `stale_files` too; queries that
   target them directly → `UNINDEXED_FILE` flavor of `NOT_FOUND` with hint "run cq index".

## 7. Indexer & scheduling

- `cq index`: (a) flock on `~/.codeintel/<id>/index.lock` — second invocation exits 0
  immediately with `{"skipped":"emit-in-flight"}` on stdout (visible, not silent); (b) run
  `rust-analyzer scip .` in the **primary checkout** (never a worktree) writing `index.scip`
  to a temp dir; (c) ingest → `<gen>.tmp/index.sqlite`; (d) write `manifest.json` (same data
  as `meta` table, for humans); (e) atomic publish; (f) prune to 2 generations.
- **launchd:** template plist `system/templates/launchd/com.hex.codeintel-indexer.plist`
  runs `cq index` nightly at 02:30 per registered workspace. Installation is documented, not
  auto-installed by this branch (hex-upgrade wiring is a follow-up).
- Git post-commit/post-merge hooks: OPTIONAL follow-up, not in A1 scope (cadence decision
  gated on smoke test #1 emit-time results).

## 8. Success criteria (acceptance — ALL must pass; each maps to a verification step)

| # | Criterion | Verification |
|---|---|---|
| S1 | Workspace builds clean: `cargo build --release` and `cargo test -p scipd` green; `cargo clippy -p scipd -- -D warnings` clean | CI commands in plan Task N |
| S2 | **Golden queries:** on a committed fixture crate (`system/code-intel/tests/fixtures/golden-crate/`), after `cq index`, all of `def`/`refs`/`callers`/`symbols`/`search` return exact expected results for ≥10 golden symbols incl. a trait method, a generic fn, and a macro-wrapped call site | `cargo test -p scipd --test golden` |
| S3 | **Real-workspace E2E:** `cq index` on hex-foundation itself completes; `cq def`/`refs` on 5 known symbols (e.g. `consolidate`, `gatekeeper` fns) return correct file:line | scripted E2E `tests/e2e/code-intel-e2e.sh` (gated, not in unit CI) |
| S4 | **Ephemeral worktree:** from a fresh `git worktree add`, first query answers correctly in <2s wall with NO new generation created, then worktree removed with no residue in `~/.codeintel` | E2E script step |
| S5 | **Freshness:** edit one file in the worktree → any query whose results touch it lists it in `stale_files` and exits 2 under `--strict`; freshness overhead <150ms p95 | E2E script step + timing assert |
| S6 | **Loud failures:** unregistered dir → exit 4 + `UNREGISTERED_WORKSPACE`; registered-but-no-index → exit 3 + `NO_INDEX`; nonsense symbol → exit 5. Zero code paths return empty-success on error (code audit + tests) | unit tests per error |
| S7 | **Latency:** p95 over the golden query set <500ms warm on the hex-foundation index | E2E timing harness |
| S8 | **Concurrent safety:** 8 parallel `cq` query processes during an in-flight `cq index` publish all succeed against a consistent generation (old or new, never mixed) | stress test in E2E |
| S9 | `cq doctor` exits nonzero and says why when: no registry, no index, last emit failed, index older than 7 days | unit tests |
| S10 | Docs: `docs/code-intel.md` (operator guide: register, index, launchd, doctor) and AGENTS.md gains a `cq` tool-use section | review |

**callers() gate (smoke test #2):** if the measured false-negative rate for index-derived
callers exceeds 5% on macro-heavy ground truth, `cq callers` ships with a mandatory
`"quality": "best-effort"` field in its envelope and S2's callers golden expectations are
relaxed to the verified-reachable subset. The verb is never silently wrong-by-omission without
that flag.

## 9. Dependencies (new, to vet per Standing Order #7)

| Crate | Purpose | Note |
|---|---|---|
| `scip` (Sourcegraph) | SCIP protobuf bindings | pinned exact version; if unsuitable, fall back to `protobuf` + vendored scip.proto |
| `rusqlite` | already a workspace dep (bundled-full) | reuse same version as harness |
| `sha2`, `fs2`, `serde`, `serde_json`, `clap`, `anyhow`, `chrono` | already in workspace | reuse |

No network access at query time. Indexing uses only local toolchain (`rust-analyzer` from PATH,
which `cq doctor` verifies).

## 10. Risks / open items carried into the plan

- Smoke test #1 (emit time/RAM) and #2 (callers quality) are running; results land in
  the personal instance's `projects/system-improvement/research/smoke-tests/`. Plan tasks consuming their
  outputs are marked. Neither blocks the build of indexer/query plumbing.
- `rust-analyzer scip` output fidelity on our macro-heavy code: golden fixture includes macro
  cases so regressions are caught at test time, not in production.
- This is hex-foundation core tooling (Standing Order S1): all work on this branch, merged to
  foundation `main` only when complete end-to-end; personal instances consume via `/hex-upgrade`.

## 11. Acceptance record (Task 12 audit)

Audited 2026-06-11 on `feature/code-intel-a1` (Task 12 worktree). Gates run: `cargo build
--release` (workspace) green; `cargo test -p scipd` — 85 passed, 0 failed; `cargo clippy -p
scipd --all-targets -- -D warnings` clean; `cargo test -p hex-harness` green (no harness
regression); `bash tests/e2e/code-intel-e2e.sh` — all 6 sections PASS. Silent-failure audit
(S6 grep for `unwrap_or_default` / `.ok()` / `let _ =` / `unwrap_or(` / bare `if let Ok(`):
18 hits, every one justified (test helpers, commented clock-skew floors, display-only
fallbacks, conservative parse fallbacks); zero empty-success-on-error paths.

| # | Criterion | Evidence | Status |
|---|---|---|---|
| S1 | Workspace builds clean; tests + clippy green | `cargo build --release` finished clean; `cargo test -p scipd`: 85 passed (71 lib + 14 integration), 0 failed; `cargo clippy -p scipd --all-targets -- -D warnings`: clean | PASS |
| S2 | Golden queries on fixture crate, ≥10 symbols incl. trait method, generic fn, macro case | `tests/golden.rs`: `golden_defs_refs_and_callers`, `golden_file_outlines`, `golden_search_hits`, `callers_gate_file_is_well_formed_and_resolved`; `tests/fixtures/golden-expectations.json` covers 13 symbols incl. `Area::area` (trait method + both impls), `generic_max`, `macro_caller` (macro-body limitation pinned as ABSENT per callers gate; `tests/fixtures/callers-gate.json`) | PASS |
| S3 | Real-workspace E2E: index hex-foundation, 5 known symbols return correct file:line | `tests/e2e/code-intel-e2e.sh` sections 1–2: register + index (emit 69.9s), 5 grep-verified def/refs queries (`should_throttle`, `lower_to_background`, et al.) all PASS | PASS |
| S4 | Ephemeral worktree: first query <2s, no new generation, no residue | E2E section 3: cold start 64ms, same `workspace_id` as parent, generation count unchanged, `CODEINTEL_HOME` clean after teardown | PASS |
| S5 | Freshness: edited file → `stale_files` + `--strict` exit 2; overhead <150ms p95 | E2E section 4: non-strict exit 2 + file in `stale_files`, `--strict` exit 2 with `STALE_RESULTS`, freshness p95 25.8ms; unit: `src/freshness.rs` `unstaged_edit_is_stale`, `staged_edit_is_stale`, `commit_after_indexing_is_stale`, `file_removed_from_git_is_stale` | PASS |
| S6 | Loud failures: exit 4/3/5 per taxonomy; no empty-success on error | `src/error.rs` `exit_codes_match_spec`, `code_strings_match_spec_table`, `every_variant_has_nonempty_hint`; `tests/cli.rs` `unregistered_cwd_exit_4`, `registered_no_index_exit_3`, `nonsense_symbol_exit_5`, `stale_strict_exit_2`; `src/indexer.rs` `emit_failure_is_emit_failed_with_stderr_tail`, `missing_analyzer_binary_is_loud_hinted_emit_failure`; Task 12 silent-failure code audit (18 hits, all justified) | PASS |
| S7 | Latency p95 <500ms warm on hex-foundation index | E2E section 5: p95 172.0ms over 20 mixed queries | PASS |
| S8 | 8 parallel readers during in-flight publish see consistent generations | E2E section 6: 1648 responses across 8 readers during reindex, all exit 0/2, every `indexed_commit` ∈ {old, new}, post-publish queries see new HEAD | PASS |
| S9 | `cq doctor` red (nonzero + reason) on: no registry, no index, last emit failed, index >7 days | `tests/cli.rs` `doctor_red_when_no_index_and_green_after` (no-index, >7-days via meta UPDATE, `emit_exit_code=7`, commit-lag fields), `doctor_verifies_rust_analyzer_on_path`; `src/doctor.rs` `empty_registry_is_red`, `unreadable_db_is_an_error_not_a_skip` | PASS |
| S10 | Docs: operator guide + AGENTS.md `cq` section | `docs/code-intel.md` (register/index/doctor walkthrough, launchd install for `system/templates/launchd/com.hex.codeintel-indexer.plist`, error-code table); `AGENTS.md` "## Code intelligence (cq)" section | PASS |
