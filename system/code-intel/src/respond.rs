//! Response assembly: query → freshness → envelope (plan Task 7, spec §5/§6).
//!
//! [`run`] is the lib API every query verb goes through (the `cq` bin wires
//! to it in Task 9):
//!
//! 1. Open the CURRENT generation's SQLite **read-only** (generations are
//!    immutable after publish; queries never take locks).
//! 2. Execute the verb via [`crate::query`] (0-based [`RawResult`]s).
//! 3. Run the freshness check ([`crate::freshness`]) over the result files.
//!    `strict` + any stale file → [`CqError::StaleResults`] (exit 2 refusal).
//! 4. Assemble the [`Envelope`]: 0-based → 1-based exactly once, snippets
//!    read from the **query root** (the caller's worktree) for fresh files
//!    only — stale files get no snippet and are listed in `stale_files`.
//! 5. Exit code: 0 all-fresh, 2 when any result file is stale.
//!
//! Error mapping (spec §5): missing/unopenable CURRENT generation →
//! [`CqError::NoIndex`]. A query that directly targets a file that exists in
//! the worktree but not in the index → `NOT_FOUND` with an `UNINDEXED_FILE`
//! message (spec §6 rule 3).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use rusqlite::{Connection, OpenFlags};

use crate::envelope::{Envelope, QueryResult};
use crate::error::CqError;
use crate::freshness;
use crate::query::{self, RawResult, Selector};
use crate::store::Store;
use crate::workspace::Workspace;

/// A query verb with its argument, as parsed by the CLI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verb {
    Def(Selector),
    Refs(Selector),
    Callers(Selector),
    /// Symbol outline of one workspace-relative file.
    Symbols(String),
    /// FTS5 prefix search over display names.
    Search(String),
}

impl Verb {
    /// The file the verb targets *directly*, when it does (used for the
    /// UNINDEXED_FILE flavor of NOT_FOUND, spec §6).
    fn target_path(&self) -> Option<&str> {
        match self {
            Verb::Def(Selector::Pos { path, .. })
            | Verb::Refs(Selector::Pos { path, .. })
            | Verb::Callers(Selector::Pos { path, .. })
            | Verb::Symbols(path) => Some(path),
            _ => None,
        }
    }
}

/// Execute `verb` against the CURRENT generation of `workspace`'s index
/// under `home` (the codeintel home), and assemble the response envelope.
///
/// Returns the envelope plus its exit code (0 fresh, 2 stale-annotated).
/// Domain failures are [`CqError`]s inside the `anyhow::Error` (downcast for
/// exit codes): `NoIndex`, `NotFound`, and — under `strict` with any stale
/// result file — `StaleResults`.
pub fn run(
    home: &Path,
    workspace: &Workspace,
    verb: &Verb,
    strict: bool,
) -> Result<(Envelope, i32)> {
    let started = Instant::now();
    let conn = open_current_index(home, workspace)?;

    let raw = match verb {
        Verb::Def(sel) => query::def(&conn, sel),
        Verb::Refs(sel) => query::refs(&conn, sel),
        Verb::Callers(sel) => query::callers(&conn, sel),
        Verb::Symbols(path) => query::symbols(&conn, path),
        Verb::Search(q) => query::search(&conn, q),
    }
    .map_err(|e| flavor_unindexed(e, verb, &conn, &workspace.query_root))?;

    let files: BTreeSet<String> = raw.iter().map(|r| r.path.clone()).collect();
    let files: Vec<String> = files.into_iter().collect();
    let stale_files = freshness::check(&workspace.query_root, &files, &conn)?;
    if strict && !stale_files.is_empty() {
        return Err(CqError::StaleResults.into());
    }

    let stale_set: HashSet<&str> = stale_files.iter().map(String::as_str).collect();
    let mut snippet_cache: HashMap<String, Vec<String>> = HashMap::new();
    let results = raw
        .iter()
        .map(|r| to_query_result(r, &workspace.query_root, &stale_set, &mut snippet_cache))
        .collect::<Result<Vec<_>>>()?;

    let indexed_commit = meta_value(&conn, "commit_sha")?;
    let index_age_secs = index_age_secs(&conn)?;
    let exit_code = if stale_files.is_empty() { 0 } else { 2 };

    let envelope = Envelope {
        source: "index".into(),
        workspace_id: workspace.id.clone(),
        indexed_commit,
        index_age_secs,
        stale_files,
        latency_ms: started.elapsed().as_millis() as u64,
        quality: None,
        results,
    };
    Ok((envelope, exit_code))
}

/// Open `<home>/<id>/<CURRENT>/index.sqlite` read-only. Any failure on this
/// path — no CURRENT, dangling CURRENT, unopenable SQLite — is `NO_INDEX`
/// (spec §5 table row 3).
fn open_current_index(home: &Path, workspace: &Workspace) -> Result<Connection> {
    let no_index = || CqError::NoIndex {
        workspace_id: workspace.id.clone(),
    };
    let store = Store::new(home, &workspace.id);
    let generation_dir = store.current_dir().map_err(|_| no_index())?;
    Connection::open_with_flags(
        generation_dir.join("index.sqlite"),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| no_index().into())
}

/// Upgrade a plain `NOT_FOUND` to the UNINDEXED_FILE flavor when the verb
/// directly targeted a file that exists in the worktree but not in the
/// index (spec §6: "queries that target them directly → UNINDEXED_FILE
/// flavor of NOT_FOUND with hint run cq index").
fn flavor_unindexed(
    err: anyhow::Error,
    verb: &Verb,
    conn: &Connection,
    query_root: &Path,
) -> anyhow::Error {
    if !matches!(err.downcast_ref::<CqError>(), Some(CqError::NotFound { .. })) {
        return err;
    }
    let Some(path) = verb.target_path() else { return err };
    let in_index = conn
        .query_row("SELECT 1 FROM files WHERE path = ?1", [path], |_| Ok(()))
        .is_ok();
    if !in_index && query_root.join(path).is_file() {
        return CqError::NotFound {
            query: format!("{path} exists in the worktree but is not in the index (UNINDEXED_FILE)"),
        }
        .into();
    }
    err
}

/// Convert one 0-based [`RawResult`] to a 1-based envelope [`QueryResult`],
/// reading the snippet from the query root only when the file is fresh.
fn to_query_result(
    raw: &RawResult,
    query_root: &Path,
    stale: &HashSet<&str>,
    cache: &mut HashMap<String, Vec<String>>,
) -> Result<QueryResult> {
    let snippet = if stale.contains(raw.path.as_str()) {
        None
    } else {
        Some(snippet_line(query_root, &raw.path, raw.start_line, cache)?)
    };
    Ok(QueryResult {
        path: raw.path.clone(),
        line: u32::try_from(raw.start_line + 1).context("line overflows u32")?,
        col: u32::try_from(raw.start_col + 1).context("col overflows u32")?,
        symbol: raw.scip_symbol.clone(),
        display_name: raw.display_name.clone(),
        kind: kind_str(raw.kind),
        role: if raw.is_definition() { "definition" } else { "reference" }.into(),
        snippet,
    })
}

/// Read line `line0` (0-based) of `path` under `root`, via a per-response
/// cache. The file passed the freshness check, so its content matches the
/// index — a missing line means the check and reality disagree: fail loudly.
fn snippet_line(
    root: &Path,
    path: &str,
    line0: i64,
    cache: &mut HashMap<String, Vec<String>>,
) -> Result<String> {
    if !cache.contains_key(path) {
        let full = root.join(path);
        let content = std::fs::read_to_string(&full)
            .with_context(|| format!("reading fresh file {} for snippet", full.display()))?;
        cache.insert(path.to_string(), content.lines().map(str::to_string).collect());
    }
    let idx = usize::try_from(line0).context("negative line in index")?;
    cache[path].get(idx).cloned().ok_or_else(|| {
        anyhow!("fresh file {path} has no line {} — freshness/index mismatch", line0 + 1)
    })
}

/// Human kind string from the raw SCIP `SymbolInformation.kind` value
/// (e.g. 17 → "function", 49 → "struct", 53 → "trait").
fn kind_str(kind: i64) -> String {
    use protobuf::Enum as _;
    i32::try_from(kind)
        .ok()
        .and_then(scip::types::symbol_information::Kind::from_i32)
        .filter(|k| *k != scip::types::symbol_information::Kind::UnspecifiedKind)
        .map(|k| format!("{k:?}").to_lowercase())
        .unwrap_or_else(|| "unknown".into())
}

/// Read one required key from the generation `meta` table (written by the
/// indexer at publish time, spec §4). Missing keys are loud errors.
fn meta_value(conn: &Connection, key: &str) -> Result<String> {
    conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
        .with_context(|| format!("meta key {key} missing from generation index"))
}

/// `index_age_secs` = now − meta `created_at` (RFC3339), floored at 0.
fn index_age_secs(conn: &Connection) -> Result<u64> {
    let created_at = meta_value(conn, "created_at")?;
    let created = chrono::DateTime::parse_from_rfc3339(&created_at)
        .with_context(|| format!("meta created_at {created_at:?} is not RFC3339"))?;
    let age = chrono::Utc::now().signed_duration_since(created).num_seconds();
    Ok(u64::try_from(age).unwrap_or(0)) // clock skew → age 0, never a crash
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ingest, schema};
    use std::process::Command;
    use tempfile::TempDir;

    // ---- fixture: golden crate, emitted + ingested + published into a
    // hermetic CODEINTEL_HOME (pattern from src/query.rs tests) ----

    fn run_cmd(cwd: &Path, prog: &str, args: &[&str]) -> String {
        let path = format!(
            "/opt/homebrew/bin:{}",
            std::env::var("PATH").unwrap_or_default()
        );
        let out = Command::new(prog)
            .args(args)
            .current_dir(cwd)
            .env("PATH", path)
            .output()
            .unwrap_or_else(|e| panic!("spawning {prog}: {e}"));
        assert!(
            out.status.success(),
            "{prog} {args:?} failed\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn copy_dir(src: &Path, dst: &Path) {
        for entry in std::fs::read_dir(src).unwrap() {
            let entry = entry.unwrap();
            let to = dst.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                std::fs::create_dir_all(&to).unwrap();
                copy_dir(&entry.path(), &to);
            } else {
                std::fs::copy(entry.path(), &to).unwrap();
            }
        }
    }

    struct Fixture {
        home: TempDir,
        repo: TempDir,
        head: String,
    }

    impl Fixture {
        fn workspace(&self) -> Workspace {
            Workspace::resolve(self.repo.path()).unwrap()
        }
    }

    /// Golden crate in a fresh git repo, emitted with rust-analyzer, ingested
    /// into a generation database, meta populated (as the Task 8 indexer
    /// will), and atomically published under a hermetic home.
    fn indexed_golden() -> Fixture {
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/golden-crate");
        let repo = tempfile::tempdir().unwrap();
        copy_dir(&fixture, repo.path());
        run_cmd(repo.path(), "git", &["init", "-q", "-b", "main"]);
        run_cmd(repo.path(), "git", &["config", "user.email", "cq@test"]);
        run_cmd(repo.path(), "git", &["config", "user.name", "cq-test"]);
        run_cmd(repo.path(), "git", &["add", "-A"]);
        run_cmd(repo.path(), "git", &["commit", "-q", "-m", "golden"]);
        let head = run_cmd(repo.path(), "git", &["rev-parse", "HEAD"]);
        run_cmd(
            repo.path(),
            "rust-analyzer",
            &["scip", ".", "--output", "index.scip"],
        );

        let home = tempfile::tempdir().unwrap();
        let ws = Workspace::resolve(repo.path()).unwrap();
        let store = Store::new(home.path(), &ws.id);
        let generation = store.begin_generation().unwrap();
        {
            let conn = Connection::open(generation.dir().join("index.sqlite")).unwrap();
            schema::create(&conn).unwrap();
            ingest::ingest(&repo.path().join("index.scip"), &conn, repo.path()).unwrap();
            conn.execute(
                "INSERT INTO meta (key, value) VALUES
                   ('schema_version', '1'),
                   ('commit_sha', ?1),
                   ('created_at', ?2)",
                rusqlite::params![head, chrono::Utc::now().to_rfc3339()],
            )
            .unwrap();
        }
        store.publish(generation).unwrap();
        Fixture { home, repo, head }
    }

    fn name(s: &str) -> Verb {
        Verb::Def(Selector::Name(s.to_string()))
    }

    fn pos(path: &str, line: u32, col: u32) -> Selector {
        Selector::Pos {
            path: path.to_string(),
            line,
            col,
        }
    }

    // ---- the three planned tests ----

    #[test]
    fn fresh_worktree_has_no_stale_files() {
        let fx = indexed_golden();
        let ws = fx.workspace();
        let (env, exit) = run(fx.home.path(), &ws, &name("double"), false).unwrap();

        assert_eq!(exit, 0);
        assert_eq!(env.stale_files, Vec::<String>::new());
        assert_eq!(env.source, "index");
        assert_eq!(env.workspace_id, ws.id);
        assert_eq!(env.indexed_commit, fx.head);
        assert!(env.index_age_secs < 3600, "{}", env.index_age_secs);

        // `pub fn double` is src/ops.rs 0-based (0, 7) → 1-based 1:8.
        assert_eq!(env.results.len(), 1);
        let r = &env.results[0];
        assert_eq!((r.path.as_str(), r.line, r.col), ("src/ops.rs", 1, 8));
        assert_eq!(r.role, "definition");
        assert_eq!(r.kind, "function");
        assert_eq!(
            r.snippet.as_deref(),
            Some("pub fn double(x: i32) -> i32 { x * 2 }")
        );
    }

    #[test]
    fn edited_file_is_flagged_stale_and_strict_refuses() {
        let fx = indexed_golden();
        let ws = fx.workspace();
        // Unstaged tracked-file modification in the worktree.
        let ops = fx.repo.path().join("src/ops.rs");
        let mut content = std::fs::read_to_string(&ops).unwrap();
        content.push_str("// edited after indexing\n");
        std::fs::write(&ops, content).unwrap();

        let verb = Verb::Refs(Selector::Name("double".to_string()));
        let (env, exit) = run(fx.home.path(), &ws, &verb, false).unwrap();
        assert_eq!(exit, 2, "stale results must exit 2");
        assert_eq!(env.stale_files, vec!["src/ops.rs".to_string()]);
        // refs("double") spans both files: ops.rs results lose their
        // snippets, the lib.rs result keeps its own.
        assert!(!env.results.is_empty());
        for r in &env.results {
            match r.path.as_str() {
                "src/ops.rs" => assert!(r.snippet.is_none(), "{r:?}"),
                "src/lib.rs" => assert!(r.snippet.is_some(), "{r:?}"),
                other => panic!("unexpected result file {other}"),
            }
        }

        let err = run(fx.home.path(), &ws, &verb, true).unwrap_err();
        assert!(
            matches!(err.downcast_ref::<CqError>(), Some(CqError::StaleResults)),
            "{err:?}"
        );
    }

    #[test]
    fn untracked_new_file_query_is_unindexed_not_found() {
        let fx = indexed_golden();
        let ws = fx.workspace();
        std::fs::write(
            fx.repo.path().join("src/brand_new.rs"),
            "pub fn brand_new() {}\n",
        )
        .unwrap();

        for verb in [
            Verb::Def(pos("src/brand_new.rs", 1, 8)),
            Verb::Symbols("src/brand_new.rs".to_string()),
        ] {
            let err = run(fx.home.path(), &ws, &verb, false).unwrap_err();
            let Some(CqError::NotFound { query }) = err.downcast_ref::<CqError>() else {
                panic!("expected NotFound for {verb:?}, got {err:?}");
            };
            assert!(query.contains("UNINDEXED_FILE"), "{query}");
            // The standard NotFound hint already points at `cq index`.
            let j: serde_json::Value = serde_json::from_str(
                &err.downcast_ref::<CqError>().unwrap().to_json(),
            )
            .unwrap();
            assert!(j["error"]["hint"].as_str().unwrap().contains("cq index"));
        }

        // A garden-variety miss stays a plain NOT_FOUND (no UNINDEXED flavor).
        let err = run(fx.home.path(), &ws, &name("no_such_symbol"), false).unwrap_err();
        let Some(CqError::NotFound { query }) = err.downcast_ref::<CqError>() else {
            panic!("expected NotFound, got {err:?}");
        };
        assert!(!query.contains("UNINDEXED_FILE"), "{query}");
    }

    // ---- supplementary coverage ----

    #[test]
    fn missing_current_generation_is_no_index() {
        // Registered-shaped workspace, but nothing ever published.
        let repo = tempfile::tempdir().unwrap();
        run_cmd(repo.path(), "git", &["init", "-q", "-b", "main"]);
        run_cmd(repo.path(), "git", &["config", "user.email", "t@t"]);
        run_cmd(repo.path(), "git", &["config", "user.name", "t"]);
        std::fs::write(repo.path().join("x"), "x").unwrap();
        run_cmd(repo.path(), "git", &["add", "-A"]);
        run_cmd(repo.path(), "git", &["commit", "-q", "-m", "init"]);
        let ws = Workspace::resolve(repo.path()).unwrap();
        let home = tempfile::tempdir().unwrap();

        let err = run(home.path(), &ws, &name("double"), false).unwrap_err();
        match err.downcast_ref::<CqError>() {
            Some(CqError::NoIndex { workspace_id }) => assert_eq!(workspace_id, &ws.id),
            other => panic!("expected NoIndex, got {other:?}"),
        }
    }

    #[test]
    fn worktree_query_reads_snippets_from_the_worktree() {
        let fx = indexed_golden();
        let wt_parent = tempfile::tempdir().unwrap();
        let wt = wt_parent.path().join("wt1");
        run_cmd(
            fx.repo.path(),
            "git",
            &["worktree", "add", "-q", wt.to_str().unwrap()],
        );
        let ws = Workspace::resolve(&wt).unwrap();
        assert_eq!(ws.id, fx.workspace().id, "worktree shares workspace id");

        // Fresh worktree at the indexed commit: everything fresh.
        let (env, exit) = run(fx.home.path(), &ws, &name("double"), false).unwrap();
        assert_eq!(exit, 0);
        assert_eq!(env.stale_files, Vec::<String>::new());

        // Edit inside the WORKTREE only → stale there, while the primary
        // checkout still answers fresh.
        let ops = wt.join("src/ops.rs");
        let mut content = std::fs::read_to_string(&ops).unwrap();
        content.push_str("// worktree edit\n");
        std::fs::write(&ops, content).unwrap();
        let (env, exit) = run(fx.home.path(), &ws, &name("double"), false).unwrap();
        assert_eq!(exit, 2);
        assert_eq!(env.stale_files, vec!["src/ops.rs".to_string()]);
        assert!(env.results[0].snippet.is_none());

        let (env, exit) =
            run(fx.home.path(), &fx.workspace(), &name("double"), false).unwrap();
        assert_eq!(exit, 0);
        assert_eq!(env.stale_files, Vec::<String>::new());
        assert!(env.results[0].snippet.is_some());

        run_cmd(
            fx.repo.path(),
            "git",
            &["worktree", "remove", "--force", wt.to_str().unwrap()],
        );
    }

    #[test]
    fn callers_with_no_callers_is_empty_fresh_success() {
        let fx = indexed_golden();
        let ws = fx.workspace();
        // total_area is never called in the fixture: a true empty answer.
        let verb = Verb::Callers(Selector::Name("total_area".to_string()));
        let (env, exit) = run(fx.home.path(), &ws, &verb, false).unwrap();
        assert_eq!(exit, 0);
        assert!(env.results.is_empty());
        assert!(env.stale_files.is_empty());
        assert!(env.quality.is_none());
    }

    #[test]
    fn search_and_symbols_assemble_envelopes() {
        let fx = indexed_golden();
        let ws = fx.workspace();

        let (env, exit) =
            run(fx.home.path(), &ws, &Verb::Search("gener".to_string()), false).unwrap();
        assert_eq!(exit, 0);
        assert!(env.results.iter().any(|r| r.display_name == "generic_max"));

        let (env, _) = run(
            fx.home.path(),
            &ws,
            &Verb::Symbols("src/shapes.rs".to_string()),
            false,
        )
        .unwrap();
        let kinds: std::collections::BTreeSet<(&str, &str)> = env
            .results
            .iter()
            .map(|r| (r.display_name.as_str(), r.kind.as_str()))
            .collect();
        assert!(kinds.contains(&("Area", "trait")), "{kinds:?}");
        assert!(kinds.contains(&("Circle", "struct")), "{kinds:?}");
        assert!(kinds.contains(&("total_area", "function")), "{kinds:?}");
        // Every fresh result carries its 1-based source line as snippet.
        for r in &env.results {
            assert!(r.snippet.is_some(), "{r:?}");
            assert!(r.line >= 1 && r.col >= 1, "{r:?}");
        }
    }

    #[test]
    fn kind_str_maps_known_scip_kinds() {
        assert_eq!(kind_str(17), "function");
        assert_eq!(kind_str(49), "struct");
        assert_eq!(kind_str(53), "trait");
        assert_eq!(kind_str(26), "method");
        assert_eq!(kind_str(70), "traitmethod");
        assert_eq!(kind_str(0), "unknown");
        assert_eq!(kind_str(-5), "unknown");
        assert_eq!(kind_str(999_999), "unknown");
    }
}
