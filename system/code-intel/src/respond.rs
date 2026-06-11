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

use crate::envelope::{Envelope, Escalated, QueryResult};
use crate::error::CqError;
use crate::freshness;
use crate::live::client::{ClientError, LiveClient};
use crate::proto::{QueryVerb, Reply};
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
    let a1 = run_a1(home, workspace, verb, strict, false)?;
    Ok((a1.envelope, a1.exit_code))
}

/// How the routing layer may use the live daemon (SPEC-A2 §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveMode {
    /// Default: A1 index answer first (unchanged fast path); escalate to
    /// live only when the target or a result file is stale AND the daemon
    /// can answer. Warming/down → index answer + `escalated`.
    Auto,
    /// `--live`: force the live path; `LIVE_UNAVAILABLE` (exit 7) when it
    /// cannot answer.
    Forced,
    /// `--no-live`: pure A1 behavior; never touches the socket.
    Disabled,
}

/// A1 answer plus the extras the routing layer needs (computed while the
/// index connection is open; invisible to pure-A1 callers).
struct A1Answer {
    envelope: Envelope,
    exit_code: i32,
    /// 1-based live query position: the Pos selector itself, or the
    /// symbol's indexed definition site for Name selectors (live queries
    /// are positional, SPEC-A2 §3).
    live_target: Option<(String, u32, u32)>,
    /// The verb's *target* file is stale even when no result file is —
    /// e.g. `def` from a call site in an edited file resolving into a
    /// fresh file (SPEC-A2 §5: "target file or ≥1 result file is stale").
    target_stale: bool,
}

/// Route `verb` per SPEC-A2 §5: A1 index path FIRST, live escalation only
/// when warranted and possible.
///
/// - `Auto`, all fresh → the A1 answer, byte-identical, socket untouched.
/// - `Auto`, stale + live answers → live results, `source:"live"`, exit 0.
/// - `Auto`, stale + live warming/unreachable → the A1 answer (A1 exit
///   rules stand: stale → 2) plus a structured `escalated` notice; under
///   `strict` the A1 refusal (`STALE_RESULTS`, exit 2) stands instead —
///   strict never serves stale index data.
/// - `Forced` → live or `LIVE_UNAVAILABLE` (exit 7).
/// - `Disabled`, or a verb without a live path (symbols/search) → pure A1.
pub fn run_routed(
    home: &Path,
    workspace: &Workspace,
    verb: &Verb,
    strict: bool,
    mode: LiveMode,
) -> Result<(Envelope, i32)> {
    let live_capable = matches!(verb, Verb::Def(_) | Verb::Refs(_) | Verb::Callers(_));
    if mode == LiveMode::Forced && !live_capable {
        return Err(CqError::LiveUnavailable {
            reason: "this verb has no live path (live supports def/refs/callers)".into(),
        }
        .into());
    }
    if mode == LiveMode::Disabled || !live_capable {
        return run(home, workspace, verb, strict);
    }

    let started = Instant::now();
    // A1 first, with strict deferred: when live answers, the results are
    // current disk truth and there is nothing stale to refuse.
    let a1 = run_a1(home, workspace, verb, false, true)?;
    let needs_live = !a1.envelope.stale_files.is_empty() || a1.target_stale;
    match mode {
        LiveMode::Auto if !needs_live => Ok((a1.envelope, a1.exit_code)),
        LiveMode::Auto => match try_live(home, workspace, verb, a1.live_target.as_ref()) {
            Ok(results) => Ok(live_envelope(a1.envelope, results, started)),
            Err(failure) => {
                if strict {
                    return Err(CqError::StaleResults.into());
                }
                let mut envelope = a1.envelope;
                envelope.escalated = Some(failure.into_escalated());
                envelope.latency_ms = started.elapsed().as_millis() as u64;
                Ok((envelope, a1.exit_code))
            }
        },
        LiveMode::Forced => match try_live(home, workspace, verb, a1.live_target.as_ref()) {
            Ok(results) => Ok(live_envelope(a1.envelope, results, started)),
            Err(failure) => {
                Err(CqError::LiveUnavailable { reason: failure.describe() }.into())
            }
        },
        LiveMode::Disabled => unreachable!("Disabled handled above"),
    }
}

/// Why a live attempt produced no results. Every variant is surfaced —
/// as `escalated` (auto) or `LIVE_UNAVAILABLE` (forced) — never swallowed.
enum LiveFailure {
    /// Socket missing/refused/timed out, or the connection died.
    Unavailable { reason: String },
    /// Daemon reachable; the instance is still priming (SPEC-A2 §3).
    Warming { elapsed_secs: u64, workspace: Option<String> },
    /// Daemon reachable; it answered with a structured error.
    LiveError { code: String, message: String },
    /// No live position could be derived for the query target.
    NoTarget,
}

impl LiveFailure {
    fn into_escalated(self) -> Escalated {
        match self {
            LiveFailure::Unavailable { reason } => Escalated {
                reason: "daemon-unavailable".into(),
                elapsed_secs: None,
                workspace: None,
                detail: Some(reason),
            },
            LiveFailure::Warming { elapsed_secs, workspace } => Escalated {
                reason: "warming".into(),
                elapsed_secs: Some(elapsed_secs),
                workspace,
                detail: None,
            },
            LiveFailure::LiveError { code, message } => Escalated {
                reason: "live-error".into(),
                elapsed_secs: None,
                workspace: None,
                detail: Some(format!("{code}: {message}")),
            },
            LiveFailure::NoTarget => Escalated {
                reason: "live-error".into(),
                elapsed_secs: None,
                workspace: None,
                detail: Some(
                    "no live position could be derived for the query target".into(),
                ),
            },
        }
    }

    fn describe(&self) -> String {
        match self {
            LiveFailure::Unavailable { reason } => format!("daemon unreachable: {reason}"),
            LiveFailure::Warming { elapsed_secs, .. } => {
                format!("live instance still warming ({elapsed_secs}s elapsed)")
            }
            LiveFailure::LiveError { code, message } => format!("{code}: {message}"),
            LiveFailure::NoTarget => {
                "no live position could be derived for the query target".into()
            }
        }
    }
}

/// One live query attempt against the daemon for `workspace.query_root`.
fn try_live(
    home: &Path,
    workspace: &Workspace,
    verb: &Verb,
    target: Option<&(String, u32, u32)>,
) -> std::result::Result<Vec<QueryResult>, LiveFailure> {
    let Some((path, line, col)) = target else {
        return Err(LiveFailure::NoTarget);
    };
    let query_verb = match verb {
        Verb::Def(_) => QueryVerb::Def,
        Verb::Refs(_) => QueryVerb::Refs,
        Verb::Callers(_) => QueryVerb::Callers,
        other => unreachable!("non-live verb {other:?} routed to try_live"),
    };
    let map_client_err = |e: ClientError| LiveFailure::Unavailable { reason: e.to_string() };
    let mut client = LiveClient::connect(home).map_err(map_client_err)?;
    let reply = client
        .query(query_verb, &workspace.query_root, path, *line, *col)
        .map_err(map_client_err)?;
    interpret_query_reply(reply)
}

/// SPEC-A2 §3 reply → results or a classified failure.
fn interpret_query_reply(reply: Reply) -> std::result::Result<Vec<QueryResult>, LiveFailure> {
    if reply.ok {
        return reply.results.ok_or_else(|| LiveFailure::LiveError {
            code: "BAD_REPLY".into(),
            message: "ok query reply carried no results section".into(),
        });
    }
    if let Some(warming) = reply.warming {
        return Err(LiveFailure::Warming {
            elapsed_secs: warming.elapsed_secs,
            workspace: warming.workspace,
        });
    }
    if let Some(error) = reply.error {
        return Err(LiveFailure::LiveError { code: error.code, message: error.message });
    }
    Err(LiveFailure::LiveError {
        code: "BAD_REPLY".into(),
        message: "not-ok reply with neither warming nor error section".into(),
    })
}

/// Rebuild the A1 envelope around a successful live answer: live results
/// speak for current disk state, so nothing is stale and the exit is 0
/// (index identity fields are kept for provenance).
fn live_envelope(
    mut envelope: Envelope,
    results: Vec<QueryResult>,
    started: Instant,
) -> (Envelope, i32) {
    envelope.source = "live".into();
    envelope.results = results;
    envelope.stale_files = Vec::new();
    envelope.escalated = None;
    envelope.latency_ms = started.elapsed().as_millis() as u64;
    (envelope, 0)
}

/// The A1 pipeline (index query → freshness → envelope), optionally
/// computing the routing extras while the connection is open.
fn run_a1(
    home: &Path,
    workspace: &Workspace,
    verb: &Verb,
    strict: bool,
    for_routing: bool,
) -> Result<A1Answer> {
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

    let target_stale = if for_routing {
        target_is_stale(workspace, verb, &conn, &files, &stale_files)?
    } else {
        false
    };
    let live_target = if for_routing { live_target(verb, &conn) } else { None };

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
        escalated: None,
        results,
    };
    Ok(A1Answer { envelope, exit_code, live_target, target_stale })
}

/// Freshness of the verb's *target* file (SPEC-A2 §5 routing input). Files
/// already covered by the result-set check reuse that verdict; otherwise
/// one extra single-file check runs. A target file absent from the index
/// (only possible in exotic layouts — a successful positional query implies
/// an indexed file) is reported not-stale, since the index cannot speak
/// about it either way.
fn target_is_stale(
    workspace: &Workspace,
    verb: &Verb,
    conn: &Connection,
    result_files: &[String],
    stale_files: &[String],
) -> Result<bool> {
    let Some(target) = verb.target_path() else { return Ok(false) };
    if result_files.iter().any(|f| f == target) {
        return Ok(stale_files.iter().any(|f| f == target));
    }
    let in_index = conn
        .query_row("SELECT 1 FROM files WHERE path = ?1", [target], |_| Ok(()))
        .is_ok();
    if !in_index {
        return Ok(false);
    }
    let stale = freshness::check(&workspace.query_root, &[target.to_string()], conn)?;
    Ok(!stale.is_empty())
}

/// 1-based live query position for the verb. Pos selectors pass through;
/// Name selectors resolve to the symbol's indexed definition site (the live
/// protocol is positional, SPEC-A2 §3). `None` when no definition is known
/// — surfaced later as a loud `NoTarget` failure, never silently skipped.
fn live_target(verb: &Verb, conn: &Connection) -> Option<(String, u32, u32)> {
    let selector = match verb {
        Verb::Def(sel) | Verb::Refs(sel) | Verb::Callers(sel) => sel,
        _ => return None,
    };
    if let Selector::Pos { path, line, col } = selector {
        return Some((path.clone(), *line, *col));
    }
    match query::def(conn, selector) {
        Ok(defs) => defs.first().and_then(|d| {
            let line = u32::try_from(d.start_line + 1).ok()?;
            let col = u32::try_from(d.start_col + 1).ok()?;
            Some((d.path.clone(), line, col))
        }),
        Err(_) => None,
    }
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

    // ---- SPEC-A2 §5 routing (fake daemon socket; real-daemon coverage in
    // tests/cli_live.rs) ----

    /// Newline-JSON fake daemon at `<home>/scipd.sock` answering each
    /// request via `respond`.
    fn fake_daemon(
        home: &Path,
        respond: impl Fn(&serde_json::Value) -> String + Send + 'static,
    ) {
        let listener =
            std::os::unix::net::UnixListener::bind(crate::daemon::socket_path(home)).unwrap();
        std::thread::spawn(move || {
            use std::io::{BufRead as _, BufReader, Write as _};
            while let Ok((stream, _)) = listener.accept() {
                let mut writer = stream.try_clone().unwrap();
                let reader = BufReader::new(stream);
                for line in reader.lines() {
                    let Ok(line) = line else { break };
                    let v: serde_json::Value = serde_json::from_str(&line).unwrap();
                    let mut reply = respond(&v);
                    reply.push('\n');
                    if writer.write_all(reply.as_bytes()).is_err() {
                        break;
                    }
                }
            }
        });
    }

    fn make_stale(fx: &Fixture) {
        let ops = fx.repo.path().join("src/ops.rs");
        let mut content = std::fs::read_to_string(&ops).unwrap();
        content.push_str("// edited after indexing\n");
        std::fs::write(&ops, content).unwrap();
    }

    #[test]
    fn routed_fresh_query_is_pure_index_and_never_touches_the_socket() {
        let fx = indexed_golden();
        let ws = fx.workspace();
        // A poisoned "socket": a plain file. A connect attempt would fail
        // loudly as daemon-unavailable — fresh queries must not even try,
        // so no escalated section may appear.
        std::fs::write(crate::daemon::socket_path(fx.home.path()), b"not a socket").unwrap();
        let (env, exit) =
            run_routed(fx.home.path(), &ws, &name("double"), false, LiveMode::Auto).unwrap();
        assert_eq!(exit, 0);
        assert_eq!(env.source, "index");
        assert!(env.escalated.is_none(), "{:?}", env.escalated);
    }

    #[test]
    fn routed_stale_with_daemon_down_serves_index_plus_escalated() {
        let fx = indexed_golden();
        let ws = fx.workspace();
        make_stale(&fx);
        let verb = Verb::Refs(Selector::Name("double".to_string()));
        let (env, exit) =
            run_routed(fx.home.path(), &ws, &verb, false, LiveMode::Auto).unwrap();
        assert_eq!(exit, 2, "A1 exit rules stand");
        assert_eq!(env.source, "index");
        assert_eq!(env.stale_files, vec!["src/ops.rs".to_string()]);
        let escalated = env.escalated.expect("escalated section");
        assert_eq!(escalated.reason, "daemon-unavailable");
        assert!(escalated.detail.is_some());
    }

    #[test]
    fn routed_stale_with_warming_daemon_serves_index_plus_warming() {
        let fx = indexed_golden();
        let ws = fx.workspace();
        make_stale(&fx);
        fake_daemon(fx.home.path(), |req| {
            assert_eq!(req["op"], "query");
            format!(
                r#"{{"id":{},"ok":false,"warming":{{"elapsed_secs":7,"workspace":"/w"}}}}"#,
                req["id"]
            )
        });
        let verb = Verb::Refs(Selector::Name("double".to_string()));
        let (env, exit) =
            run_routed(fx.home.path(), &ws, &verb, false, LiveMode::Auto).unwrap();
        assert_eq!(exit, 2);
        assert_eq!(env.source, "index");
        let escalated = env.escalated.expect("escalated section");
        assert_eq!(escalated.reason, "warming");
        assert_eq!(escalated.elapsed_secs, Some(7));
    }

    #[test]
    fn routed_stale_with_live_answer_returns_live_envelope() {
        let fx = indexed_golden();
        let ws = fx.workspace();
        make_stale(&fx);
        fake_daemon(fx.home.path(), |req| {
            // Name selector resolves to the indexed def position of double
            // (src/ops.rs 1:8) — assert the wire request carries it.
            assert_eq!(req["op"], "query");
            assert_eq!(req["verb"], "refs");
            assert_eq!(req["path"], "src/ops.rs");
            assert_eq!(req["line"], 1);
            assert_eq!(req["col"], 8);
            format!(
                r#"{{"id":{},"ok":true,"source":"live","results":[{{"path":"src/ops.rs","line":1,"col":8,"symbol":"","display_name":"double","kind":"function","role":"definition","snippet":"pub fn double(x: i32) -> i32 {{ x * 2 }}"}}]}}"#,
                req["id"]
            )
        });
        let verb = Verb::Refs(Selector::Name("double".to_string()));
        let (env, exit) =
            run_routed(fx.home.path(), &ws, &verb, false, LiveMode::Auto).unwrap();
        assert_eq!(exit, 0, "live answers are current: exit 0");
        assert_eq!(env.source, "live");
        assert_eq!(env.stale_files, Vec::<String>::new());
        assert!(env.escalated.is_none());
        assert_eq!(env.results.len(), 1);
        assert_eq!(env.results[0].display_name, "double");
        // Index identity fields are kept for provenance.
        assert_eq!(env.indexed_commit, fx.head);
    }

    #[test]
    fn routed_strict_stale_daemon_down_refuses_like_a1() {
        let fx = indexed_golden();
        let ws = fx.workspace();
        make_stale(&fx);
        let verb = Verb::Refs(Selector::Name("double".to_string()));
        let err =
            run_routed(fx.home.path(), &ws, &verb, true, LiveMode::Auto).unwrap_err();
        assert!(
            matches!(err.downcast_ref::<CqError>(), Some(CqError::StaleResults)),
            "{err:?}"
        );
    }

    #[test]
    fn routed_forced_with_daemon_down_is_live_unavailable() {
        let fx = indexed_golden();
        let ws = fx.workspace();
        let err = run_routed(fx.home.path(), &ws, &name("double"), false, LiveMode::Forced)
            .unwrap_err();
        match err.downcast_ref::<CqError>() {
            Some(CqError::LiveUnavailable { reason }) => {
                assert!(reason.contains("unreachable"), "{reason}");
            }
            other => panic!("expected LiveUnavailable, got {other:?}"),
        }
    }

    #[test]
    fn routed_forced_on_symbols_or_search_is_live_unavailable() {
        let fx = indexed_golden();
        let ws = fx.workspace();
        for verb in [
            Verb::Symbols("src/shapes.rs".to_string()),
            Verb::Search("gener".to_string()),
        ] {
            let err = run_routed(fx.home.path(), &ws, &verb, false, LiveMode::Forced)
                .unwrap_err();
            assert!(
                matches!(err.downcast_ref::<CqError>(), Some(CqError::LiveUnavailable { .. })),
                "{verb:?}: {err:?}"
            );
        }
    }

    #[test]
    fn routed_disabled_is_pure_a1_even_when_stale() {
        let fx = indexed_golden();
        let ws = fx.workspace();
        make_stale(&fx);
        // A live-answering fake daemon that must never be consulted.
        fake_daemon(fx.home.path(), |req| {
            panic!("--no-live touched the socket: {req}");
        });
        let verb = Verb::Refs(Selector::Name("double".to_string()));
        let (env, exit) =
            run_routed(fx.home.path(), &ws, &verb, false, LiveMode::Disabled).unwrap();
        assert_eq!(exit, 2);
        assert_eq!(env.source, "index");
        assert!(env.escalated.is_none());
    }

    #[test]
    fn routed_stale_target_with_fresh_results_escalates() {
        // `def` from a position in the EDITED file resolving into a fresh
        // file: stale_files is empty (the result file is fresh) but the
        // TARGET file is stale — SPEC-A2 §5 says this must escalate too.
        let fx = indexed_golden();
        let ws = fx.workspace();
        // Edit lib.rs (the query target); def target `double` lives in
        // ops.rs which stays fresh. Append, so positions stay valid.
        let lib = fx.repo.path().join("src/lib.rs");
        let mut content = std::fs::read_to_string(&lib).unwrap();
        let line = content
            .lines()
            .position(|l| l.contains("ops::double"))
            .unwrap() as u32
            + 1;
        let col = content
            .lines()
            .find(|l| l.contains("ops::double"))
            .unwrap()
            .find("double")
            .unwrap() as u32
            + 1;
        content.push_str("// edited after indexing\n");
        std::fs::write(&lib, content).unwrap();

        let verb = Verb::Def(pos("src/lib.rs", line, col));
        // Pure A1 sanity: result (ops.rs def) is fresh → exit 0, no stale.
        let (env, exit) = run(fx.home.path(), &ws, &verb, false).unwrap();
        assert_eq!((exit, env.stale_files.len()), (0, 0), "{env:?}");

        // Routed: the stale target forces the escalation attempt; with the
        // daemon down that surfaces as escalated.daemon-unavailable.
        let (env, exit) =
            run_routed(fx.home.path(), &ws, &verb, false, LiveMode::Auto).unwrap();
        assert_eq!(exit, 0, "A1 exit rules stand (no stale RESULT files)");
        let escalated = env.escalated.expect("stale target must escalate");
        assert_eq!(escalated.reason, "daemon-unavailable");
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
