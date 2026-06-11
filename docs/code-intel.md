# Code Intelligence (`cq`) — Operator Guide

`cq` is a stateless, index-backed code-intelligence CLI for Rust workspaces
(crate `system/code-intel`, package `scipd`). It answers `def` / `refs` /
`callers` / `symbols` / `search` queries from a SQLite index built by
`rust-analyzer scip`, with per-file git-blob freshness checks. No daemon, no
shared mutable state — safe for any number of concurrent agents.

Contract: [docs/code-intel/SPEC-A1.md](code-intel/SPEC-A1.md). This page is the
how-to.

## Concepts

- **Workspace.** A registered Rust repo, identified by
  `workspace-id = first 12 hex chars of sha256(realpath of the primary
  checkout root)`. All state lives under `~/.codeintel/<workspace-id>/`
  (override the root with `$CODEINTEL_HOME`).
- **Generations.** Each `cq index` run publishes an immutable snapshot
  directory `<timestamp>-<rand>/{index.sqlite, manifest.json}`. Publish is
  atomic (tmp-dir rename, then atomic rewrite of the `CURRENT` pointer file);
  the 2 most recent generations are kept, older ones pruned. Readers opening
  the old generation mid-publish keep a consistent view — never a mixed one.
- **Freshness.** Every query compares the blob OIDs recorded at index time
  against the querying worktree's actual git state (`git ls-files -s` +
  `git diff --name-only`). Files that drifted appear in `stale_files`, their
  snippets are withheld, and the query exits 2. `--strict` refuses outright
  (exit 2, `STALE_RESULTS` on stderr). Stale is loud, never silent.
- **Worktrees.** Any `git worktree` of a registered workspace resolves to its
  parent workspace automatically (`git rev-parse --git-common-dir`) and
  queries the existing index instantly — no per-worktree state, cold start in
  milliseconds, teardown leaves zero residue. `cq index` always indexes the
  primary checkout, never a worktree.

## Walkthrough

```bash
cargo build --release -p scipd        # binary at target/release/cq
cq register ~/github.com/mrap/hex-foundation
# {"registered":"ab12cd34ef56","root":"/Users/you/github.com/mrap/hex-foundation"}

cq index --workspace ~/github.com/mrap/hex-foundation
# Runs rust-analyzer scip over the primary checkout (~40s, ~3GB RSS on this
# repo), ingests into SQLite, publishes a generation. Concurrent invocation
# prints {"skipped":"emit-in-flight"} and exits 0 — visible, never doubled.

cd ~/github.com/mrap/hex-foundation   # or any worktree of it
cq def parse_proposal                 # by symbol name
cq def system/harness/src/main.rs:120:8   # or FILE:LINE:COL (1-based)
cq refs should_throttle
cq callers sha256_hex
cq symbols system/harness/src/ledger.rs
cq search gatekeep

cq doctor                             # health: index age, commit lag,
                                      # rust-analyzer presence; exit !=0 on red
```

Every query prints one JSON envelope on stdout:

```json
{
  "source": "index",
  "workspace_id": "ab12cd34ef56",
  "indexed_commit": "9b70565a…",
  "index_age_secs": 4210,
  "stale_files": [],
  "latency_ms": 14,
  "results": [ { "path": "…", "line": 120, "col": 8, "symbol": "…",
                 "display_name": "…", "kind": "function",
                 "role": "definition", "snippet": "fn consolidate(…" } ]
}
```

Lines/cols are 1-based. `snippet` is read from *your worktree* and only for
fresh files; stale files get no snippet and are listed in `stale_files`.

## Error codes (spec §5 — every error is structured JSON on stderr)

| Condition | stderr JSON `error.code` | exit |
|---|---|---|
| OK, all result files fresh | — | 0 |
| OK, but ≥1 result file stale (or `--strict` refused) | `STALE_RESULTS` annotation / refusal | 2 |
| No index / `CURRENT` missing / SQLite unopenable | `NO_INDEX` | 3 |
| CWD not in a registered workspace | `UNREGISTERED_WORKSPACE` | 4 |
| Workspace registered but not a Rust workspace | `UNSUPPORTED_WORKSPACE` | 4 |
| Symbol/position resolves to nothing | `NOT_FOUND` | 5 |
| Emit subprocess failed (`cq index`) | `EMIT_FAILED` + captured stderr tail | 6 |

`cq` never exits 0 with empty results due to an internal failure; unexpected
internal errors exit 1 with `error.code = "INTERNAL"`.

## Scheduling with launchd (nightly reindex, 02:30)

Template: `system/templates/launchd/com.hex.codeintel-indexer.plist`. One copy
per registered workspace; manual install in A1.

```bash
mkdir -p ~/.codeintel/logs
sed -e "s|__CQ_BIN__|$HOME/github.com/mrap/hex-foundation/target/release/cq|" \
    -e "s|__WORKSPACE__|$HOME/github.com/mrap/hex-foundation|" \
    -e "s|__HOME__|$HOME|" \
    system/templates/launchd/com.hex.codeintel-indexer.plist \
    > ~/Library/LaunchAgents/com.hex.codeintel-indexer.plist
launchctl bootstrap "gui/$(id -u)" ~/Library/LaunchAgents/com.hex.codeintel-indexer.plist
launchctl kickstart "gui/$(id -u)/com.hex.codeintel-indexer"   # optional: run now
tail -f ~/.codeintel/logs/com.hex.codeintel-indexer.log
```

For additional workspaces, copy the plist with a unique filename and Label
suffix (`com.hex.codeintel-indexer.<name>`). To remove:
`launchctl bootout "gui/$(id -u)/com.hex.codeintel-indexer"` and delete the
plist.

## Known limitation: calls inside `macro_rules!` bodies

Call sites that live **inside a `macro_rules!` body** emit no SCIP occurrences,
so the calling function is invisible to `cq callers` (pinned by the golden
suite: `macro_caller` is asserted ABSENT from `callers(double)`; gate record in
`system/code-intel/tests/fixtures/callers-gate.json`). Calls passed **as macro
arguments** (`assert!(foo())`, `format!("{}", bar())`, `anyhow!`,
`tokio::spawn(async { … })`) keep their spans and ARE captured — measured 0%
false negatives on macro-heavy ground truth, which is why `callers` ships with
no `quality` flag. If you suspect a macro-bodied caller, fall back to `grep`
for that one edge.

## Troubleshooting

Run `cq doctor` first — it reports per-workspace index age, commit lag, last
emit status, generation list, and whether `rust-analyzer` is on PATH, and exits
nonzero with explicit `red_reasons` when anything is wrong.

| Symptom | Cause / fix |
|---|---|
| `UNREGISTERED_WORKSPACE` (exit 4) | CWD isn't inside a registered repo. `cq register <path>` (or pass `--workspace`). |
| `NO_INDEX` (exit 3) | Registered but never indexed, or store damaged. Run `cq index --workspace <path>`. |
| Exit 2 + `stale_files` | Your worktree drifted from the indexed commit (edits or different checkout). Results are still correct *positions for the indexed commit*; reindex to clear. |
| `EMIT_FAILED` (exit 6) | `rust-analyzer scip` crashed; stderr tail is in the error JSON and the failed `<gen>.tmp/` dir is kept for post-mortem. Check rust-analyzer version (`cq doctor`). |
| `{"skipped":"emit-in-flight"}` | Another `cq index` holds the flock. Wait for it; nothing was lost. |
| Doctor red: index older than 7 days | The launchd job isn't running — check `launchctl print gui/$(id -u)/com.hex.codeintel-indexer` and the log under `~/.codeintel/logs/`. |
| Slow first query after reboot | Cold page cache on `index.sqlite`; subsequent queries are warm (<500ms p95 budget). |

## E2E acceptance

`tests/e2e/code-intel-e2e.sh` proves spec S3–S8 against hex-foundation itself
(hermetic: clones the repo to /tmp and uses a throwaway `CODEINTEL_HOME`).
Gated — run manually, not part of unit CI:

```bash
bash tests/e2e/code-intel-e2e.sh
```
