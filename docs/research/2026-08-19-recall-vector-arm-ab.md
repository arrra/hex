# Recall vector (KNN) arm — arm-OFF vs arm-ON A/B

**Date:** 2026-08-19
**Spec:** Sdnap37he · **Task:** Tgsw5by6q (eval-ab)
**Author:** BOI execute worker (mike@mrap.me)
**Base:** develop @ c1905be · **Binary:** freshly built release @
`/Users/mrap/.boi/v2/cargo-target/release/hex` (built 2026-08-19 15:53, after the
task-2 commit 753871d)

## Outcome (decisive)

**DEFAULT STAYS OFF.** Three of the four SO-11 adoption gates fail. Two of them
(paraphrase improvement, regression count) fail on **load-independent scoring**
grounds, so the semantic KNN arm does not earn adoption regardless of host
conditions. The flag + loud/bounded BM25-only fallback (task 2) remain merged;
the compiled default `VectorArm::enabled` is already `false` and is **not**
flipped. This is a valid, complete negative result (spec SO 11: "A negative
result is a valid, complete outcome; forcing the arm on is not.").

| Gate | Criterion | Measured | Verdict |
|---|---|---|---|
| **G1** overall | ON facts-section score strictly > OFF | ON **68/77** vs OFF **63/77** (+5) | **PASS** |
| **G2** paraphrase | paraphrase slice improves by ≥ 3 cases | `holdout-paraphrase` 20/27 → **21/27** (Δ **+1** net; 3 new passes − 2 regressions) | **FAIL** |
| **G3** regressions | regressions vs arm-off ≤ 1 case | **2** (`a-boi-dashboard-defer-rollup`, `c-1`, both paraphrase) — **the same two in both ON runs** | **FAIL** |
| **G4** latency | arm-on p50 ≤ 200 ms and p95 ≤ 500 ms | in-proc **p50 873 ms / p95 1803 ms** (wall 912/1836 ms), load ~17–23 | **FAIL** (load-bound; see §5) |

**ALL pass → flip ON. ANY fail → stays OFF.** Result: G2, G3, G4 fail → **stays
OFF.**

---

## 1. Method

Everything ran against a **copy** of the frozen snapshot; the read-only source
`/Users/mrap/.hex-evalsnap` was never mutated (verified: `memory.db` mtime
`Aug 18 21:43` unchanged before and after).

- **Snapshot copy:** `/Users/mrap/.hex-evalsnap` → `/tmp/va-ab-snap`
  (`memory.db` + `-wal` + `-shm`, `recall-log.jsonl`, `CLAUDE.md` so
  `get_hex_dir` accepts it as `HEX_DIR`). `.fastembed_cache` symlinked to
  `~/hex/.fastembed_cache` — **no network fetch**, reuses the resident nomic
  model artifact.
- **HEX_DIR layout verified first** (STOP-condition pre-check): one
  `hex memory recall` against the copy injected the expected `mrap.me` facts
  before any A/B leg ran.
- **Case suite:** the 77-case golden set at
  `/Users/mrap/hex/.hex/eval/recall-cases.toml` (read-only), passed via
  `hex memory eval --cases … --json`. Slice sizes: `holdout-direct` 33,
  `holdout-paraphrase` 27, `fact-relevance` 9, `attribute` 5, `control` 2,
  `entity` 1.
- **Scoring metric:** `facts` hit = the expected substring reaches an injected
  `### Facts` line (the gate metric `hex memory eval` reports; `anywhere` is
  non-gating).
- **Arm toggle:** `$HEX_DIR/.hex/config/recall.toml`. OFF leg = **config
  absent** (compiled default → `query_vector` returns `None` → byte-identical
  BM25-only). ON leg = `[vector] enabled=true`, `socket_path` = the running
  endpoint, `timeout_ms = 2000` (generous, so scoring reflects the arm's true
  contribution rather than load-induced fallbacks — see §3 on WARN counting).
- **Resident embedder:** `hex memory embed-serve` (the shipped option-(b) path
  from `2026-08-19-recall-vector-arm.md`) held one resident `Embedder` and
  served query vectors over the unix socket; the recall CLI never cold-loaded
  the model.

### Maintain runs ONCE, before both legs (attribution guard)

`hex memory maintain --backfill-facts` also performs an FTS5 `optimize` on
`chunks`+`facts_fts` and a `facts_fts` integrity-check/rebuild (read
`maintain.rs`). Running it **once before both legs** — not between them — keeps
that FTS state identical for OFF and ON, so the measured OFF↔ON delta is
attributable to the **vector arm alone**. The OFF leg cannot be affected by the
populated `facts_vec` (arm disabled → `knn_ids = []`), so a pre-populated fact
vector table is safe for the OFF baseline. (Also folded 103 `transcript_files`
rows — irrelevant to recall ranking.)

---

## 2. Fact-vector backfill (task-2 dependency)

Snapshot fact-vector coverage started at **0 / 1863** (per the research memo:
`facts_vec` created but never written). The A/B requires it populated or the KNN
arm contributes nothing.

- Ran `hex memory maintain --backfill-facts` against the copy. It embeds the
  canonical `subject predicate object` triple document-side (batch-8,
  `search_document:` prefix), inserting into `facts_vec` incrementally.
- **The first run was SIGKILLed (exit 137) at 632/1863** — the OBS-019 ONNX
  activation-tensor memory pressure the research memo flagged as a task-2 risk,
  triggered here by concurrent-worker load spiking to ~15–27 on this shared
  host. Because the backfill commits incrementally and re-selects only facts
  **missing** a vector (idempotent), a resilient resume loop drove it to
  completion.
- **Final coverage: 1863 / 1863 (100 %)** (`SELECT count(*) FROM
  facts_vec_rowids`). The STOP-condition "coverage so low the A/B cannot be
  meaningful even after backfill" did **not** trigger.

---

## 3. Scoring results (arm-off vs arm-on)

Arm-OFF reproduced the spec's stated baseline exactly — `holdout-paraphrase`
**20/27**, matching "the misses skew paraphrase (20/27)" — which validates the
harness.

| Slice | OFF hits | ON hits | Δ |
|---|---|---|---|
| holdout-direct (33) | 27 | **30** | **+3** |
| holdout-paraphrase (27) | 20 | **21** | **+1** |
| attribute (5) | 4 | **5** | +1 |
| fact-relevance (9) | 9 | 9 | 0 |
| control (2) | 2 | 2 | 0 |
| entity (1) | 1 | 1 | 0 |
| **TOTAL (77)** | **63** | **68** | **+5** |

### Per-case flips (run 1, 0 WARN — every embed a real query vector)

- **New passes (7):** `a-agent-completion-value` (par), `a-zwerk-sse-endpoint`
  (direct), `b-hex-project-correction-rule` (par), `c-10` (par), `c-12`
  (direct), `c-13` (direct), `hex-focus` (attribute).
- **Regressions (2):** `a-boi-dashboard-defer-rollup` (par), `c-1` (par) —
  facts BM25 alone injected, but adding the KNN arm to the RRF fusion reshuffled
  the fused ranking enough to push them out of the rendered top-N.

The arm's gains skew **holdout-direct (+3)**, not the paraphrase slice it was
motivated by. Within `holdout-paraphrase` it is +3 new passes − 2 regressions =
**+1 net** — below the ≥3 bar (G2) — and the 2 paraphrase regressions alone
break G3.

### Determinism / WARN accounting

Two ON scoring runs: **run 1 = 68/77 (0 WARN)**, **run 2 = 67/77 (1 WARN)**. The
±1 is **not** ranking non-determinism (KNN over fixed vectors on a fixed DB is
deterministic) — it is one embed round-trip exceeding even the 2000 ms scoring
timeout under load ~23 and degrading loudly to BM25-only for that single query
(the exact loud/bounded fallback task 2 ships). Run 1 (0 WARN) is the arm's
clean ceiling; the gate verdicts are reported against it and are unchanged by
the variance (68 and 67 both clear G1's 63; G2/G3 fail either way).

**G3 is stable across both ON runs** (checked directly with `va_gates.py`
against each run's per-case JSON). The regression set is the *identical* two
cases — `a-boi-dashboard-defer-rollup`, `c-1` — in run 1 and run 2. The only
run-1→run-2 per-case difference is `c-10`, a paraphrase *new-pass* that the
run-2 WARN fallback reverted to BM25 (dropping ON 68→67, paraphrase 21→20). A
fallback reverts a query to its OFF result, so it can only cost a **gain**,
never manufacture a **regression** — which is exactly what happened, and why
the 2-regression G3 FAIL is load-independent, not a one-run artifact on a
±1-variance leg.

---

## 4. Latency (per-query percentiles)

Each query run as a **fresh `hex memory recall` process** — the true
per-message `UserPromptSubmit` hook shape — timed wall-clock (Python
`perf_counter` around the subprocess) with the in-process `latency_ms` read back
from `recall-log.jsonl`. 77 queries per leg. **Measured under load ~17–23**
(this shared BOI host; see §5).

| Metric | OFF p50 | OFF p95 | ON p50 | ON p95 | ON−OFF (marginal) |
|---|---|---|---|---|---|
| in-process `latency_ms` | 83 ms | 395 ms | **873 ms** | **1803 ms** | +790 / +1408 ms |
| wall-clock | 122 ms | 429 ms | 912 ms | 1836 ms | +790 / +1407 ms |

WARN fallbacks in the latency sweeps: OFF 0, ON 0 (all ON embeds succeeded).
In-process is the gate metric (apples-to-apples with the spec's cited ~18 ms
arm-off baseline; it already includes the embed round-trip). Wall-clock adds
constant process-spawn/dyld overhead present in both legs.

---

## 5. Reading the latency gate (load caveat — but not decision-relevant)

The research memo explicitly flagged that the p50 ≤ 200 / p95 ≤ 500 ms gate
"must be re-certified on a quiet host (load < ~2)." This host was **not** quiet:
load ~17–23 from concurrent BOI workers.

What the data supports, stated precisely:

- **G4 fails as measured** (in-proc p50 873 / p95 1803 ms), and this is a
  **load-inflated upper bound, not a quiet-host certification.**
- The clearest evidence it is inflated: **arm-OFF in-process p50 = 83 ms
  against the spec's ~18 ms quiet baseline — ~4.6× inflated by contention with
  the vector arm not even involved.** So even the BM25-only floor is well off
  its quiet figure here.
- The arm's marginal cost is **~790 ms p50 on this host**. It is **not**
  decomposable into "load-factor × quiet-cost" from this data: the 4.6× the OFF
  leg shows is a *SQLite-FTS* inflation, and ONNX inference under 12-core
  oversubscription with `ORT_NUM_THREADS=1` need not scale the same way — this
  A/B took no measurement isolating the two, so applying the OFF leg's 4.6× to
  the arm's forward pass is unjustified. The memo's expectation that a single
  nomic-v1.5 query-side forward pass costs "tens of ms" on a quiet host is **the
  memo's projection, unverified by this A/B.**

**None of this changes the decision:** G2 (paraphrase +1 < 3) and G3 (2
regressions > 1, stable across both runs) fail on **load-independent** scoring,
so the arm does not clear adoption even if a quiet-host latency re-certification
passed. Latency here is corroborating, not decisive.

If the paraphrase quality were ever brought over the bar in a future iteration,
G4 would need a genuine quiet-host (load < ~2) re-run — the reproduction bench
in `2026-08-19-recall-vector-arm.md §6` is the instrument.

---

## 6. Decision & compiled default

- **all_pass = false → DEFAULT STAYS OFF.** No flip.
- `system/harness/src/memory/recall_config.rs` → `impl Default for VectorArm`
  ships `enabled: false` (verified). The compiled default **matches** the gate
  outcome (ON only if all gates pass; here it is OFF).
- The flag, the resident `embed-serve` endpoint, the unix-socket client, the
  fact backfill, and the loud/bounded BM25-only fallback all remain merged
  (tasks 1–2) — ready behind the flag for a future iteration that improves the
  paraphrase quality and re-certifies latency on a quiet host.
- **In-scope drift check** (STOP condition):
  `git diff --stat c1905be..HEAD -- system/harness/src/memory/` shows only the
  five files tasks 1–2 authored (`assemble.rs`, `embed_client.rs`, `mod.rs`,
  `recall.rs`, `recall_config.rs`) — no unexpected changes.

### Why the arm underperformed (for the next iteration)

The KNN arm helped **direct** queries (+3) more than the **paraphrase** queries
(+1 net) it was built for, and it *regressed* 2 paraphrase cases via RRF
reshuffle. Candidate causes to investigate before a re-run: the
`KNN_MAX_DISTANCE = 1.15` (L2) relevance floor may cut exactly the
semantically-distant, zero-keyword-overlap paraphrase matches; RRF fusion with
equal arm weighting dilutes the KNN signal against three BM25 arms; and fact
vectors embed only the short `subject predicate object` triple, which may not
carry enough semantic surface for paraphrase matching. None of these is a
config toggle on this base — they are arm-design changes, out of scope for this
A/B, which correctly reports the shipped arm's measured negative result.

---

## Appendix — reproduction

```
BIN=/Users/mrap/.boi/v2/cargo-target/release/hex        # freshly built release
SNAP=/Users/mrap/.hex-evalsnap                          # READ-ONLY source
DEST=/tmp/va-ab-snap                                     # working copy
CASES=/Users/mrap/hex/.hex/eval/recall-cases.toml       # READ-ONLY suite
SOCK=$DEST/.hex/run/embed.sock

# 1. copy snapshot (memory.db family + CLAUDE.md), symlink cache, verify one recall
# 2. backfill facts_vec ONCE (idempotent; resume on OOM SIGKILL) -> 1863/1863
HEX_DIR=$DEST $BIN memory maintain --backfill-facts
# 3. resident embedder
HEX_DIR=$DEST $BIN memory embed-serve --socket $SOCK &
# 4. OFF: no recall.toml   -> HEX_DIR=$DEST $BIN memory eval --cases $CASES --json
# 5. ON:  [vector] enabled=true, socket_path=$SOCK, timeout_ms=2000
#         -> HEX_DIR=$DEST $BIN memory eval --cases $CASES --json
# 6. latency: 77x fresh `hex memory recall <query>`, timed, per leg
```

Load stamp on every measurement in §3–§4. Source snapshot never mutated;
`embed-serve` killed at the end.

---

## 7. Provenance & independent corroboration (finalization pass, 2026-08-21)

**Provenance.** The §3–§5 numbers were produced on 2026-08-19 by a *sibling*
execution of this task (working copy `/tmp/va-ab-snap`), not by the concurrent
execution that filed a `blocked` verdict at 2026-08-19 20:11Z. That blocked
worker used a *different* working copy (`/tmp/va-ab`) and declined to trust its
own run because the two executions' `/tmp` paths overlapped (cross-`pkill` /
mid-transaction-kill risk) and host load confounded the latency gate. The
sibling's A/B doc was preserved into the worktree by the "task blocked" wip
commit `61fe919b`. This finalization pass adopts those numbers **only after**
verifying them against the surviving artifacts and the merged code, because the
decision is the *conservative* one (default stays OFF) and every load-independent
gate is independently reproducible.

**Independent corroboration performed this pass (all load-independent):**

- **Fact-vector coverage (§2) re-confirmed:** direct `sqlite3` query on the
  surviving working copy `/tmp/va-ab-snap/.hex/memory.db` →
  `SELECT count(*) FROM facts, facts_vec_rowids` = **1863 / 1863**. The doc's
  100 % backfill claim is real, not asserted.
- **DB integrity:** `PRAGMA quick_check` on that copy = **ok** — no corruption
  from any mid-transaction kill, closing the blocked worker's contamination
  concern for the copy the numbers came from.
- **Arithmetic closure (§3):** 7 new passes (3 paraphrase / 3 direct / 1
  attribute) − 2 regressions reproduces every slice delta; 63 + 7 − 2 = 68;
  slice hits sum to 68; slice sizes sum to 77; and all four gate verdicts derive
  correctly from their criteria. Recomputed programmatically — closes cleanly.
- **No post-build drift:** `git diff --stat 753871db..HEAD -- system/harness/`
  is **empty**. The binary that produced these numbers was built from
  `753871d`; the merged harness code is byte-identical to it, so the A/B
  describes HEAD, not a stale tree.
- **No committed config overrides the compiled default:** no `recall.toml` or
  template is tracked, and the only `[vector] enabled=true` strings in the repo
  are this doc's own method description. The compiled default `VectorArm::enabled
  = false` (`recall_config.rs`) is authoritative.
- **Source snapshot never mutated:** `/Users/mrap/.hex-evalsnap/.hex/memory.db`
  mtime = `2026-08-18 21:43:45`, unchanged.

**Why no re-run.** A fresh A/B has **zero decision value**: G4 (latency) can only
be certified on a quiet host (load < ~2), which was unavailable (load ~15 this
pass), and under the SO-11 rule *ANY* gate failure → default OFF. Even a clean
re-run that flipped G2/G3 to PASS leaves G4 failing → still OFF. Re-running also
carries the OBS-019 ONNX-backfill SIGKILL risk that already blocked one
execution. The corroboration above establishes the decision-relevant gates
(G1/G2/G3, all load-independent and deterministic over the fixed DB) without it.

**Finalization verdict:** the deliverable stands. Default stays **OFF**; the
compiled `VectorArm::enabled = false` matches the gate outcome.
