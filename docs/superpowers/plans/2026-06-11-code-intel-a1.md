# scipd/cq Code Intelligence A1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `cq` — a stateless SCIP-index-backed code-intelligence CLI (def/refs/callers/symbols/search + freshness + doctor) as a new cargo workspace member in hex-foundation, per `docs/code-intel/SPEC-A1.md`.

**Architecture:** Single new crate `system/code-intel` (package `scipd`, lib `scipd_core`, bin `cq`). Indexer shells out to `rust-analyzer scip`, ingests protobuf into per-generation SQLite under `~/.codeintel/<workspace-id>/`, publishes atomically. Queries are direct SQLite reads + git-blob-OID freshness checks. No daemon, no shared mutable state.

**Tech Stack:** Rust 2021, clap 4, rusqlite (bundled-full, same version as harness), `scip` crate (protobuf bindings), sha2, fs2 (flock), serde/serde_json, anyhow, chrono.

**Read first:** `docs/code-intel/SPEC-A1.md` (THE contract — schema DDL §4, CLI surface §5, error taxonomy §5, freshness §6, success criteria §8). When this plan and the spec disagree, the spec wins.

**Worker protocol (worktree-per-worker, mandatory):**
1. From `~/github.com/mrap/hex-foundation`: `git worktree add /tmp/ci-a1-t<N> -b code-intel-a1/t<N> feature/code-intel-a1`
2. Work ONLY in `/tmp/ci-a1-t<N>`. Commit there (small commits, conventional messages).
3. Verify: `cargo test -p scipd` green, `cargo clippy -p scipd -- -D warnings` clean.
4. Merge back: `cd ~/github.com/mrap/hex-foundation && git fetch . code-intel-a1/t<N> && git checkout feature/code-intel-a1 2>/dev/null; git -C /tmp/ci-a1-t<N> rebase feature/code-intel-a1 || true` — actually: the ORCHESTRATOR merges. Workers just commit on their branch and report the branch name. Do NOT touch `feature/code-intel-a1` or `main` directly.
5. Leave the worktree in place; the orchestrator removes it after merge.

---

### Task 1: Crate scaffold, error taxonomy, response envelope

**Files:**
- Modify: `Cargo.toml` (workspace root — add member)
- Create: `system/code-intel/Cargo.toml`
- Create: `system/code-intel/src/lib.rs`
- Create: `system/code-intel/src/error.rs`
- Create: `system/code-intel/src/envelope.rs`
- Create: `system/code-intel/src/bin/cq.rs`

- [ ] **Step 1: Workspace membership + crate manifest**

Root `Cargo.toml`: `members = ["system/harness", "system/code-intel"]`.

`system/code-intel/Cargo.toml`:
```toml
[package]
name = "scipd"
version = "0.1.0"
edition = "2021"

[lib]
name = "scipd_core"
path = "src/lib.rs"

[[bin]]
name = "cq"
path = "src/bin/cq.rs"

[dependencies]
anyhow = "1"
clap = { version = "4", features = ["derive"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
rusqlite = { version = "0.31", features = ["bundled-full"] }
sha2 = "0.10"
fs2 = "0.4"
chrono = { version = "0.4", features = ["serde"] }
scip = "0.5"          # verify latest on crates.io; pin exact. Fallback: protobuf + vendored scip.proto
toml = "0.8"

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: Failing tests for error taxonomy → exit codes** (`src/error.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exit_codes_match_spec() {
        assert_eq!(CqError::StaleResults.exit_code(), 2);
        assert_eq!(CqError::NoIndex { workspace_id: "x".into() }.exit_code(), 3);
        assert_eq!(CqError::UnregisteredWorkspace { cwd: "/tmp".into() }.exit_code(), 4);
        assert_eq!(CqError::UnsupportedWorkspace { reason: "no Cargo.toml".into() }.exit_code(), 4);
        assert_eq!(CqError::NotFound { query: "nope".into() }.exit_code(), 5);
        assert_eq!(CqError::EmitFailed { stderr_tail: "boom".into() }.exit_code(), 6);
    }
    #[test]
    fn error_serializes_with_code_message_hint() {
        let e = CqError::NoIndex { workspace_id: "ab12".into() };
        let j: serde_json::Value = serde_json::from_str(&e.to_json()).unwrap();
        assert_eq!(j["error"]["code"], "NO_INDEX");
        assert!(j["error"]["message"].as_str().unwrap().contains("ab12"));
        assert!(j["error"]["hint"].as_str().unwrap().contains("cq index"));
    }
}
```

- [ ] **Step 3: Implement `CqError`** — enum with variants above, `exit_code() -> i32`, `to_json() -> String` (shape `{"error":{"code","message","hint"}}`), `code_str() -> &'static str`. Every variant has a non-empty hint.

- [ ] **Step 4: Failing tests for envelope** (`src/envelope.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn envelope_serializes_per_spec() {
        let env = Envelope {
            source: "index".into(), workspace_id: "ab12cd34ef56".into(),
            indexed_commit: "deadbeef".into(), index_age_secs: 10,
            stale_files: vec!["src/a.rs".into()], latency_ms: 3,
            quality: None,
            results: vec![QueryResult { path: "src/a.rs".into(), line: 12, col: 4,
                symbol: "scip …".into(), display_name: "foo".into(), kind: "function".into(),
                role: "definition".into(), snippet: Some("fn foo() {}".into()) }],
        };
        let j: serde_json::Value = serde_json::from_str(&serde_json::to_string(&env).unwrap()).unwrap();
        assert_eq!(j["source"], "index");
        assert_eq!(j["results"][0]["line"], 12);
        assert!(j.get("quality").is_none() || j["quality"].is_null());
    }
}
```

- [ ] **Step 5: Implement `Envelope` + `QueryResult`** (serde, `skip_serializing_if = "Option::is_none"` for `quality`/`snippet`). Lines/cols in the envelope are 1-based (spec §5).

- [ ] **Step 6: Minimal `cq` bin** — clap skeleton with all subcommands from spec §5 declared, each returning `anyhow::bail!("unimplemented")` for now; `main` maps `CqError` to its exit code and prints `to_json()` to stderr.

- [ ] **Step 7: Verify + commit** — `cargo test -p scipd && cargo clippy -p scipd -- -D warnings`; commit `feat(code-intel): scaffold scipd crate with error taxonomy and envelope`.

---

### Task 2: Workspace identity, registry, worktree resolution

**Files:**
- Create: `system/code-intel/src/workspace.rs`
- Modify: `system/code-intel/src/lib.rs`, `src/bin/cq.rs` (wire `cq register`)

- [ ] **Step 1: Failing tests** (use `tempfile` + real `git init`; helper `fn mkrepo() -> TempDir` runs `git init -b main`, one commit)

```rust
#[test]
fn workspace_id_is_stable_sha_prefix() {
    let repo = mkrepo();
    let a = Workspace::resolve(repo.path()).unwrap();
    assert_eq!(a.id.len(), 12);
    assert_eq!(a.id, Workspace::resolve(repo.path()).unwrap().id);
}
#[test]
fn worktree_resolves_to_parent_workspace() {
    let repo = mkrepo();
    let wt = repo.path().parent().unwrap().join("wt1");
    run_git(repo.path(), &["worktree", "add", wt.to_str().unwrap()]);
    let a = Workspace::resolve(repo.path()).unwrap();
    let b = Workspace::resolve(&wt).unwrap();
    assert_eq!(a.id, b.id);                       // same workspace identity
    assert_eq!(b.query_root, wt.canonicalize().unwrap()); // but queries run against the worktree
}
#[test]
fn non_git_dir_is_unregistered_error() {
    let d = tempfile::tempdir().unwrap();
    assert!(matches!(Workspace::resolve(d.path()), Err(CqError::UnregisteredWorkspace { .. })));
}
#[test]
fn registry_roundtrip_and_membership() {
    let home = tempfile::tempdir().unwrap();
    let repo = mkrepo();
    let mut reg = Registry::load(home.path()).unwrap();      // empty ok
    reg.register(repo.path()).unwrap();
    reg.save(home.path()).unwrap();
    let reg2 = Registry::load(home.path()).unwrap();
    assert!(reg2.contains(&Workspace::resolve(repo.path()).unwrap().id));
}
```

- [ ] **Step 2: Implement**
  - `Workspace::resolve(dir)`: run `git -C dir rev-parse --git-common-dir --show-toplevel`; primary root = parent of the common dir (canonicalized); `id` = first 12 hex of sha256(primary_root); `query_root` = the toplevel of `dir`'s own worktree. Non-git → `UnregisteredWorkspace`.
  - `Registry`: TOML at `<codeintel_home>/registry.toml`, `[[workspace]] id/root/registered_at`. `codeintel_home()` = `$CODEINTEL_HOME` or `~/.codeintel` (env override is what makes tests hermetic).
  - `cq register <PATH>`: resolve, require `Cargo.toml` at primary root (else `UnsupportedWorkspace`), add to registry, print JSON `{registered: id, root}`.
- [ ] **Step 3: Verify + commit** `feat(code-intel): workspace identity, registry, worktree resolution`.

---

### Task 3: Generation store (atomic publish, prune, lock)

**Files:**
- Create: `system/code-intel/src/store.rs`

- [ ] **Step 1: Failing tests**

```rust
#[test]
fn publish_is_atomic_and_current_points_at_it() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::new(home.path(), "ab12cd34ef56");
    let gen = store.begin_generation().unwrap();          // creates <ts>-<rand>.tmp/
    std::fs::write(gen.dir().join("index.sqlite"), b"x").unwrap();
    std::fs::write(gen.dir().join("manifest.json"), b"{}").unwrap();
    let name = store.publish(gen).unwrap();               // rename .tmp -> final, rewrite CURRENT atomically
    assert_eq!(store.current().unwrap().unwrap(), name);
    assert!(store.current_dir().unwrap().join("index.sqlite").exists());
}
#[test]
fn prune_keeps_two_most_recent() { /* publish 3 generations, assert oldest dir gone, CURRENT intact */ }
#[test]
fn no_current_returns_none_not_panic() {
    let home = tempfile::tempdir().unwrap();
    assert!(Store::new(home.path(), "zz").current().unwrap().is_none());
}
#[test]
fn lock_is_exclusive() {
    // store.try_lock() returns guard; second try_lock() in same process returns None (fs2 try_lock_exclusive)
}
```

- [ ] **Step 2: Implement `Store`** — generation name `YYYYMMDDTHHMMSSZ-<6 random hex>`; `publish` = `fs::rename(tmp, final)` then write `CURRENT.tmp` + `fs::rename(CURRENT.tmp, CURRENT)`; `prune()` keeps 2 newest non-tmp dirs, never the one named in CURRENT; `try_lock()` = `fs2::try_lock_exclusive` on `index.lock`.
- [ ] **Step 3: Verify + commit** `feat(code-intel): generation store with atomic publish and prune`.

---

### Task 4: Golden fixture crate

**Files:**
- Create: `system/code-intel/tests/fixtures/golden-crate/` (a complete tiny cargo package, committed)

- [ ] **Step 1: Author the fixture** — a self-contained crate exercising every golden case from spec S2:

`tests/fixtures/golden-crate/Cargo.toml`: `[package] name = "golden" version = "0.1.0" edition = "2021"` (zero deps — emit must be fast).

`src/lib.rs` (this exact content; goldens reference its line numbers):
```rust
pub mod shapes;
pub mod ops;

pub fn top_level_fn(x: i32) -> i32 { ops::double(x) }
```
`src/shapes.rs`:
```rust
pub trait Area { fn area(&self) -> f64; }
pub struct Circle { pub r: f64 }
impl Area for Circle { fn area(&self) -> f64 { std::f64::consts::PI * self.r * self.r } }
pub struct Sq { pub s: f64 }
impl Area for Sq { fn area(&self) -> f64 { self.s * self.s } }
pub fn total_area(items: &[&dyn Area]) -> f64 { items.iter().map(|i| i.area()).sum() }
```
`src/ops.rs`:
```rust
pub fn double(x: i32) -> i32 { x * 2 }
pub fn generic_max<T: PartialOrd>(a: T, b: T) -> T { if a > b { a } else { b } }
macro_rules! call_double { ($x:expr) => { crate::ops::double($x) }; }
pub fn macro_caller() -> i32 { call_double!(21) }      // call site inside macro — the hard case
pub fn fmt_user(name: &str) -> String { format!("user:{}", double(name.len() as i32)) }
```

- [ ] **Step 2: Golden expectations file** `tests/fixtures/golden-expectations.json`: for ≥10 symbols (`double`, `generic_max`, `Area`, `Area::area` trait method + both impls, `total_area`, `macro_caller`, `top_level_fn`, `Circle`, `fmt_user`), record expected def path:line, expected refs count, expected callers sets (e.g. callers of `double` ⊇ {`top_level_fn`, `fmt_user`}; whether `macro_caller` appears is recorded as `"macro_case": "expected_but_gated"` — see spec §8 callers gate).
- [ ] **Step 3: Commit** `test(code-intel): golden fixture crate and expectations`.

---

### Task 5: SCIP ingest

**Files:**
- Create: `system/code-intel/src/ingest.rs`
- Create: `system/code-intel/src/schema.rs` (DDL from spec §4, verbatim)

- [ ] **Step 1: Failing test** (requires `rust-analyzer` on PATH; mark `#[test] fn ingest_golden_crate()` — it copies `golden-crate` to a tempdir, `git init && git add -A && git commit`, runs `rust-analyzer scip .` there, then `ingest(scip_path, conn, repo_root)`):

```rust
#[test]
fn ingest_golden_crate_populates_schema() {
    let (dir, scip) = emit_golden();                 // helper: returns repo dir + index.scip path
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    schema::create(&conn).unwrap();
    let stats = ingest::ingest(&scip, &conn, dir.path()).unwrap();
    assert!(stats.symbols >= 10, "got {}", stats.symbols);
    let defs: i64 = conn.query_row(
        "SELECT count(*) FROM occurrences o JOIN symbols s ON s.id=o.symbol_id \
         WHERE s.display_name='double' AND o.roles & 1 != 0", [], |r| r.get(0)).unwrap();
    assert_eq!(defs, 1);
    // blob OIDs recorded for every file
    let no_oid: i64 = conn.query_row("SELECT count(*) FROM files WHERE blob_oid=''", [], |r| r.get(0)).unwrap();
    assert_eq!(no_oid, 0);
    // enclosing symbol derived for the double() call inside top_level_fn
    let encl: String = conn.query_row(
        "SELECT es.display_name FROM occurrences o \
         JOIN symbols s ON s.id=o.symbol_id JOIN symbols es ON es.id=o.enclosing_symbol_id \
         JOIN files f ON f.id=o.file_id \
         WHERE s.display_name='double' AND o.roles & 1 = 0 AND f.path='src/lib.rs'",
        [], |r| r.get(0)).unwrap();
    assert_eq!(encl, "top_level_fn");
}
```

- [ ] **Step 2: Implement**
  - `schema::create(conn)` — DDL exactly per spec §4 + `INSERT` triggers keeping `symbols_fts` in sync (or manual FTS insert during ingest; pick manual — simpler, index is write-once).
  - `ingest::ingest(scip_path, conn, repo_root) -> Stats`:
    1. Parse with the `scip` crate (`scip::types::Index::parse_from_bytes` via `protobuf::Message`).
    2. One pass: upsert symbols (from `SymbolInformation` and any occurrence symbols), insert files with `blob_oid` from one `git -C repo_root ls-files -s` call parsed into a map, insert occurrences with SCIP ranges verbatim (0-based).
    3. Second pass per file: derive `enclosing_symbol_id` — prefer `enclosing_range` from SCIP when populated; else smallest definition-occurrence span in the same file strictly containing the reference range. Definitions get NULL.
    4. Wrap in one transaction; `PRAGMA journal_mode=OFF, synchronous=OFF` during build (write-once artifact).
  - If the `scip` crate's API differs from the above sketch, adapt — the test is the contract, not the sketch.
- [ ] **Step 3: Verify + commit** `feat(code-intel): SCIP protobuf ingest into generation SQLite`.

---

### Task 6: Query engine

**Files:**
- Create: `system/code-intel/src/query.rs`

- [ ] **Step 1: Failing tests** — reuse `emit_golden()`+ingest into a temp DB once per test module (`once_cell` or build in each test; fine). Cover, per spec §5:

```rust
#[test] fn def_by_name() { /* q.def(Sel::Name("double")) → exactly src/ops.rs line of `pub fn double` */ }
#[test] fn def_by_position() { /* position of the `double` callsite in lib.rs (1-based in API) → same def */ }
#[test] fn refs_includes_def_flagged() { /* refs("double") ⊇ def + lib.rs + fmt_user sites; def has role "definition" */ }
#[test] fn callers_of_double() { /* set ⊇ {"top_level_fn","fmt_user"}; record whether "macro_caller" present (don't assert yet — gate) */ }
#[test] fn symbols_outline_for_file() { /* symbols("src/shapes.rs") contains Area, Circle, Sq, total_area with kinds */ }
#[test] fn search_prefix() { /* search("gener") finds generic_max */ }
#[test] fn unknown_symbol_is_not_found() { /* NotFound error, not empty success */ }
#[test] fn trait_method_def_via_impl_position() { /* position on `i.area()` call in total_area → trait method + both impls listed */ }
```

- [ ] **Step 2: Implement `query.rs`** — `Selector::{Name(String), Pos{path,line,col}}` (CLI 1-based → internal 0-based here, one place only); `def/refs/callers/symbols/search` returning `Vec<RawResult>` (0-based, no snippets — envelope layer owns presentation). Position resolution: smallest occurrence range containing the point. `callers`: `SELECT DISTINCT es.* FROM occurrences o JOIN symbols es ON es.id=o.enclosing_symbol_id WHERE o.symbol_id=? AND o.roles & 1 = 0`.
- [ ] **Step 3: Verify + commit** `feat(code-intel): query engine (def/refs/callers/symbols/search)`.

---

### Task 7: Freshness + envelope assembly

**Files:**
- Create: `system/code-intel/src/freshness.rs`
- Create: `system/code-intel/src/respond.rs`

- [ ] **Step 1: Failing tests**

```rust
#[test]
fn fresh_worktree_has_no_stale_files() { /* clone golden repo, index it, query via respond::run — stale_files empty, snippets present */ }
#[test]
fn edited_file_is_flagged_stale_and_strict_refuses() {
    // append a comment to src/ops.rs in the worktree (tracked-file modification)
    // respond::run(.., strict=false) → envelope.stale_files == ["src/ops.rs"], no snippet for its results, would-be exit 2
    // respond::run(.., strict=true) → Err(CqError::StaleResults)
}
#[test]
fn untracked_new_file_query_is_unindexed_not_found() { /* def by position inside a brand-new file → NotFound with UNINDEXED hint */ }
```

- [ ] **Step 2: Implement**
  - `freshness::check(query_root, files_in_results, conn) -> Vec<String>`: one `git -C query_root ls-files -s --` over result paths → mtime-cheap blob OIDs; ALSO run `git -C query_root diff --name-only --` over those paths to catch dirty-but-tracked content (ls-files -s shows the *staged* OID; unstaged edits need the diff check). Union of mismatched + dirty = stale.
  - `respond::run(workspace, verb, selector, strict) -> Result<(Envelope, i32), CqError>`: open CURRENT generation read-only (`?mode=ro`), execute query, map 0-based→1-based, read snippets from `query_root` for fresh files only, fill envelope (`index_age_secs` from meta `created_at`, `indexed_commit` from meta), compute exit code (0 or 2).
- [ ] **Step 3: Verify + commit** `feat(code-intel): blob-OID freshness and response assembly`.

---

### Task 8: `cq index` (emit → ingest → publish)

**Files:**
- Create: `system/code-intel/src/indexer.rs`
- Modify: `src/bin/cq.rs`

- [ ] **Step 1: Failing test**

```rust
#[test]
fn index_end_to_end_on_golden_crate() {
    let home = tempfile::tempdir().unwrap();       // CODEINTEL_HOME
    let repo = golden_repo();                       // git-initialized golden crate
    register(home.path(), repo.path());
    let report = indexer::run(home.path(), repo.path()).unwrap();
    assert_eq!(report.emit_exit_code, 0);
    let store = Store::new(home.path(), &report.workspace_id);
    assert!(store.current_dir().unwrap().join("index.sqlite").exists());
    // meta table populated
    // second concurrent run: hold the lock, assert run() returns Skipped variant (visible, not silent)
}
#[test]
fn emit_failure_is_emit_failed_with_stderr_tail() { /* point rust-analyzer at a dir with no Cargo.toml via a fake $PATH shim script that exits 7 printing "boom" → CqError::EmitFailed contains "boom" */ }
```

- [ ] **Step 2: Implement `indexer::run`** — resolve workspace (must be PRIMARY root: if invoked from a worktree, index the primary root); take store lock (held → return `IndexOutcome::SkippedInFlight`, caller prints `{"skipped":"emit-in-flight"}`); run `rust-analyzer scip .` with cwd=primary root, stdout/stderr captured to files in the tmp generation dir; on nonzero → `EmitFailed` (keep the .tmp dir for post-mortem, but never publish); ingest; write `manifest.json` (serde of meta); record `emit_duration_secs`; publish; prune.
- [ ] **Step 3: Verify + commit** `feat(code-intel): cq index orchestration with atomic publish`.

---

### Task 9: CLI wiring + doctor

**Files:**
- Modify: `system/code-intel/src/bin/cq.rs`
- Create: `system/code-intel/src/doctor.rs`
- Test: `system/code-intel/tests/cli.rs` (spawn the built binary via `env!("CARGO_BIN_EXE_cq")`)

- [ ] **Step 1: Failing CLI tests** (each asserts BOTH stdout/stderr JSON shape AND exit code; spec §5 table):

```rust
#[test] fn def_happy_path_exit_0() { /* full pipeline on golden repo via CARGO_BIN_EXE_cq, CODEINTEL_HOME=tempdir */ }
#[test] fn unregistered_cwd_exit_4() { /* run in plain tempdir → stderr error.code == UNREGISTERED_WORKSPACE */ }
#[test] fn registered_no_index_exit_3() { }
#[test] fn nonsense_symbol_exit_5() { }
#[test] fn stale_strict_exit_2() { }
#[test] fn doctor_red_when_no_index_and_green_after() {
    // doctor exit !=0 + reasons[] before cq index; exit 0 with per-workspace report after
    // ALSO red when: meta.created_at older than 7 days (inject by UPDATE meta), last emit_exit_code != 0
}
#[test] fn doctor_verifies_rust_analyzer_on_path() { }
```

- [ ] **Step 2: Implement** — wire every verb to `respond::run` / `indexer::run` / `doctor::run`; positional arg parsing `FILE:LINE:COL` vs bare name (contains `:` + numeric suffix heuristic; document in `--help`); `doctor::run` returns `{workspaces:[{id, root, index_age_secs, indexed_commit, commit_lag (git rev-list --count indexed..HEAD on primary), last_emit_exit, generations, red_reasons[]}], rust_analyzer: {found, version}}`, exit 1 if any `red_reasons` non-empty.
- [ ] **Step 3: Verify + commit** `feat(code-intel): full cq CLI with doctor`.

---

### Task 10: Golden acceptance test (S2) + callers gate consumption

**Files:**
- Create: `system/code-intel/tests/golden.rs`

- [ ] **Step 1: Write `tests/golden.rs`** — loads `golden-expectations.json`, runs the full pipeline (register→index→each verb) against the fixture, asserts every expectation. The macro-case caller (`macro_caller`) is asserted per the gate file: read `tests/fixtures/callers-gate.json` (`{"macro_callers_from_index": true|false}`) — **created in this task** from the smoke-test #2 verdict (orchestrator supplies it; if the smoke-test result file `~/hex/projects/system-improvement/research/smoke-tests/2026-06-11-scip-callers-quality.md` is not yet available, set `false` (conservative) and the envelope `quality: "best-effort"` field MUST be emitted by `cq callers` — add that to `respond::run`).
- [ ] **Step 2: Verify + commit** `test(code-intel): golden acceptance suite (spec S2)`.

---

### Task 11: E2E script, launchd template, docs

**Files:**
- Create: `tests/e2e/code-intel-e2e.sh` (follows existing patterns in `tests/` — look at neighbors for harness conventions)
- Create: `system/templates/launchd/com.hex.codeintel-indexer.plist`
- Create: `docs/code-intel.md`
- Modify: `AGENTS.md` (new "## Code intelligence (cq)" section, ~20 lines: verbs, envelope, stale_files meaning, exit codes)

- [ ] **Step 1: E2E script** proving spec S3-S8 against hex-foundation ITSELF (not the fixture). Sections, each `set -euo pipefail`, loud echo per assertion:
  1. `cq register` + `cq index` on the repo (S3; print emit duration).
  2. 5 known-symbol queries with expected file asserts (`consolidate`, `Gatekeeper`-area fns — pick at write time from `system/harness/src/`, grep-verify expectations inline).
  3. Fresh-worktree test: `git worktree add /tmp/cq-e2e-wt`, time first `cq def` (<2s), assert generation count unchanged, remove worktree, assert `~/.codeintel` (CODEINTEL_HOME) has no new entries (S4).
  4. Stale test: edit a file in the worktree, assert `stale_files` + `--strict` exit 2; time freshness overhead (<150ms p95 over 20 runs) (S5).
  5. Latency: 20 mixed queries, compute p95 < 500ms (S7).
  6. Concurrency: launch `cq index` in background, immediately fan out 8 parallel query loops; all must exit 0/2, none 3+; after publish, all see a consistent `indexed_commit` (S8).
- [ ] **Step 2: launchd plist template** — Label `com.hex.codeintel-indexer`, ProgramArguments `[cq, index, --workspace, __WORKSPACE__]`, StartCalendarInterval 02:30, StandardError/OutPath under `~/.codeintel/logs/`, `__WORKSPACE__` placeholder documented in docs/code-intel.md (manual install in A1).
- [ ] **Step 3: `docs/code-intel.md`** — operator guide: concepts (generations, freshness, worktrees), register/index/doctor walkthrough, launchd install steps, error-code table (copy from spec §5), troubleshooting (`cq doctor` first).
- [ ] **Step 4: Run the E2E script for real. Paste its full output into the task report.** Fix what fails.
- [ ] **Step 5: Commit** `feat(code-intel): E2E acceptance, launchd template, operator docs`.

---

### Task 12: Final audit + branch readiness

- [ ] **Step 1: Silent-failure audit** — grep the crate for `unwrap_or_default`, `ok()`, `let _ =`, empty `catch`-style fallbacks; every one must be justified or fixed (Standing Order S6). List each in the report.
- [ ] **Step 2: Full gates** — `cargo build --release && cargo test -p scipd && cargo clippy -p scipd -- -D warnings && cargo test -p hex-harness` (prove no harness regression).
- [ ] **Step 3: Spec coverage check** — walk spec §8 S1-S10 and cite the test/script proving each. Any gap → fix before reporting.
- [ ] **Step 4: Commit any fixes**; report branch ready for merge to `feature/code-intel-a1` → review → `main`.

---

## Self-Review (done at plan time)

- **Spec coverage:** S1→T12; S2→T4/T10; S3/S4/S5/S7/S8→T11; S6→T1/T9; S9→T9; S10→T11. callers gate→T10.
- **Type consistency:** `CqError` (T1) used in T2/T7/T8/T9; `Envelope`/`QueryResult` (T1) filled by T7; `Store` (T3) used by T8; `Workspace`/`Registry` (T2) used by T7/T8/T9; `Selector` defined T6, used T9.
- **Known judgment calls left to workers:** exact `scip` crate API shape (T5 notes the test is the contract); choice of harness E2E symbols (T11 instructs grep-verification at write time).

## Dependency order

T1 → T2, T3, T4 (parallel) → T5 (needs T3? no — needs T4 fixture only; T5 ∥ T2/T3) → T6 (needs T5) → T7 (needs T2+T5+T6) → T8 (needs T2+T3+T5) → T9 (needs T7+T8) → T10 (needs T9) → T11 (needs T9) → T12.
Parallel-safe waves: [T1] → [T2, T3, T4] → [T5] → [T6, T8-partial? no — keep serial: T6] → [T7, T8] → [T9] → [T10, T11] → [T12].
