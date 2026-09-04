//! Query engine over an ingested generation SQLite (plan Task 6, spec §5).
//!
//! All verbs return [`RawResult`]s in SCIP 0-based coordinates with NO
//! snippets — presentation (1-based conversion, snippet reads, envelope
//! assembly) belongs to the respond layer (Task 7).
//!
//! Coordinate contract: the CLI is 1-based (`FILE:LINE:COL`), SCIP and this
//! database are 0-based. The 1→0 conversion happens HERE, in
//! [`Selector::Pos`] resolution, and nowhere else.
//!
//! Empty-vs-error policy (Standing Order S6 — never empty-success on a
//! resolution failure):
//! - Selector resolving to nothing (unknown name, unknown file, position on
//!   no occurrence) → [`CqError::NotFound`].
//! - `def`/`refs` resolving a symbol that has no matching occurrences in the
//!   index (e.g. an external symbol with no local definition) → `NotFound`.
//! - `search` with zero matches → `NotFound` ("resolves to nothing", spec §5).
//! - `callers` of a resolved-but-never-called symbol → `Ok(vec![])`: "no
//!   callers" is a true answer, not a failure.
//! - `symbols` on an indexed file with no non-local definitions →
//!   `Ok(vec![])` for the same reason.

use anyhow::{bail, Context, Result};
use rusqlite::Connection;

use crate::error::CqError;

/// SCIP `SymbolRole.Definition` bit.
const ROLE_DEFINITION: i64 = 1;

/// What the user pointed at: a bare symbol name, or a file position.
///
/// `Pos` carries CLI coordinates: **1-based** line and column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selector {
    Name(String),
    Pos { path: String, line: u32, col: u32 },
}

impl std::fmt::Display for Selector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Selector::Name(n) => write!(f, "{n}"),
            Selector::Pos { path, line, col } => write!(f, "{path}:{line}:{col}"),
        }
    }
}

/// One occurrence-shaped result row, 0-based, presentation-free.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawResult {
    pub path: String,
    pub start_line: i64,
    pub start_col: i64,
    pub end_line: i64,
    pub end_col: i64,
    pub scip_symbol: String,
    pub display_name: String,
    /// Raw SCIP `SymbolInformation.kind` value (respond layer maps to text).
    pub kind: i64,
    /// Raw SCIP `SymbolRole` bitfield of the occurrence.
    pub roles: i64,
}

impl RawResult {
    pub fn is_definition(&self) -> bool {
        self.roles & ROLE_DEFINITION != 0
    }
}

const RESULT_COLUMNS: &str = "f.path, o.start_line, o.start_col, o.end_line, o.end_col, \
     s.scip_symbol, s.display_name, s.kind, o.roles";

fn row_to_result(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawResult> {
    Ok(RawResult {
        path: row.get(0)?,
        start_line: row.get(1)?,
        start_col: row.get(2)?,
        end_line: row.get(3)?,
        end_col: row.get(4)?,
        scip_symbol: row.get(5)?,
        display_name: row.get(6)?,
        kind: row.get(7)?,
        roles: row.get(8)?,
    })
}

/// Fetch occurrences of `symbol_ids`, optionally restricted to definitions,
/// in deterministic (path, line, col) order.
fn occurrences_of(
    conn: &Connection,
    symbol_ids: &[i64],
    definitions_only: bool,
) -> Result<Vec<RawResult>> {
    let placeholders = vec!["?"; symbol_ids.len()].join(",");
    let role_filter = if definitions_only {
        "AND o.roles & 1 != 0"
    } else {
        ""
    };
    let sql = format!(
        "SELECT {RESULT_COLUMNS}
         FROM occurrences o
         JOIN files f ON f.id = o.file_id
         JOIN symbols s ON s.id = o.symbol_id
         WHERE o.symbol_id IN ({placeholders}) {role_filter}
         ORDER BY f.path, o.start_line, o.start_col"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(symbol_ids), row_to_result)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Resolve a selector to the symbol id(s) it denotes.
///
/// - `Name` → every symbol with that exact `display_name`.
/// - `Pos` → the symbol(s) of the smallest occurrence range containing the
///   point (1-based CLI coordinates converted to 0-based here, only here).
///   Ties — multiple occurrences sharing the identical smallest range — all
///   resolve.
fn resolve(conn: &Connection, selector: &Selector) -> Result<Vec<i64>> {
    match selector {
        Selector::Name(name) => {
            let mut stmt =
                conn.prepare("SELECT id FROM symbols WHERE display_name = ?1 ORDER BY id")?;
            let ids = stmt
                .query_map([name], |r| r.get::<_, i64>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            if ids.is_empty() {
                return Err(CqError::NotFound {
                    query: name.clone(),
                }
                .into());
            }
            Ok(ids)
        }
        Selector::Pos { path, line, col } => {
            if *line == 0 || *col == 0 {
                bail!("positions are 1-based on the CLI; got {selector}");
            }
            // The one and only 1-based → 0-based conversion.
            let point = (i64::from(*line) - 1, i64::from(*col) - 1);

            let not_found = || CqError::NotFound {
                query: selector.to_string(),
            };
            let file_id: i64 = conn
                .query_row("SELECT id FROM files WHERE path = ?1", [path], |r| r.get(0))
                .optional_not_found()?
                .ok_or_else(not_found)?;

            // Smallest occurrence range containing the point. Occurrence
            // ranges nest, so the tightest container is the one with the
            // greatest start (tie: least end).
            let mut stmt = conn.prepare(
                "SELECT symbol_id, start_line, start_col, end_line, end_col
                 FROM occurrences WHERE file_id = ?1",
            )?;
            let occs = stmt
                .query_map([file_id], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        (
                            (r.get::<_, i64>(1)?, r.get::<_, i64>(2)?),
                            (r.get::<_, i64>(3)?, r.get::<_, i64>(4)?),
                        ),
                    ))
                })?
                .collect::<rusqlite::Result<Vec<(i64, ((i64, i64), (i64, i64)))>>>()?;

            // Half-open [start, end) containment, lexicographic on (line, col).
            let mut best: Option<((i64, i64), (i64, i64))> = None;
            for (_, (start, end)) in &occs {
                if !(*start <= point && point < *end) {
                    continue;
                }
                let tighter = match best {
                    None => true,
                    Some((bs, be)) => *start > bs || (*start == bs && *end < be),
                };
                if tighter {
                    best = Some((*start, *end));
                }
            }
            let (bs, be) = best.ok_or_else(not_found)?;
            let mut ids: Vec<i64> = occs
                .iter()
                .filter(|(_, (s, e))| (*s, *e) == (bs, be))
                .map(|(id, _)| *id)
                .collect();
            ids.sort_unstable();
            ids.dedup();
            Ok(ids)
        }
    }
}

/// `cq def` — definition site(s) of the selected symbol(s).
pub fn def(conn: &Connection, selector: &Selector) -> Result<Vec<RawResult>> {
    let ids = resolve(conn, selector)?;
    let results = occurrences_of(conn, &ids, true)?;
    if results.is_empty() {
        // Symbol exists but has no definition in this index (e.g. external).
        return Err(CqError::NotFound {
            query: format!("{selector} (no definition in index)"),
        }
        .into());
    }
    Ok(results)
}

/// `cq refs` — every occurrence of the selected symbol(s); definitions are
/// distinguishable via [`RawResult::is_definition`].
pub fn refs(conn: &Connection, selector: &Selector) -> Result<Vec<RawResult>> {
    let ids = resolve(conn, selector)?;
    let results = occurrences_of(conn, &ids, false)?;
    if results.is_empty() {
        return Err(CqError::NotFound {
            query: format!("{selector} (no occurrences in index)"),
        }
        .into());
    }
    Ok(results)
}

/// `cq callers` — the DISTINCT enclosing symbols of every non-definition
/// occurrence of the selected symbol(s), each located at its own definition
/// site. An empty vec means "resolved, but nothing calls it".
///
/// Known limitation (smoke test #2 + golden fixture emit, 2026-06-11): call
/// sites expanded from a `macro_rules!` *body* emit no occurrence, so such
/// callers are structurally invisible to the index.
pub fn callers(conn: &Connection, selector: &Selector) -> Result<Vec<RawResult>> {
    let ids = resolve(conn, selector)?;
    let placeholders = vec!["?"; ids.len()].join(",");
    let sql = format!(
        "SELECT DISTINCT o.enclosing_symbol_id
         FROM occurrences o
         WHERE o.symbol_id IN ({placeholders})
           AND o.roles & 1 = 0
           AND o.enclosing_symbol_id IS NOT NULL"
    );
    let mut stmt = conn.prepare(&sql)?;
    let caller_ids = stmt
        .query_map(rusqlite::params_from_iter(&ids), |r| r.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if caller_ids.is_empty() {
        return Ok(Vec::new());
    }
    // Enclosing symbols are derived from definition occurrences (ingest pass
    // 2), so each caller has a definition site to report.
    occurrences_of(conn, &caller_ids, true)
}

/// `cq symbols` — definition outline of one file (workspace-relative path),
/// in source order, excluding SCIP `local` symbols (parameters, locals).
pub fn symbols(conn: &Connection, path: &str) -> Result<Vec<RawResult>> {
    let file_id: i64 = conn
        .query_row("SELECT id FROM files WHERE path = ?1", [path], |r| r.get(0))
        .optional_not_found()?
        .ok_or_else(|| CqError::NotFound {
            query: format!("{path} (file not in index)"),
        })?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {RESULT_COLUMNS}
         FROM occurrences o
         JOIN files f ON f.id = o.file_id
         JOIN symbols s ON s.id = o.symbol_id
         WHERE o.file_id = ?1 AND o.roles & 1 != 0
           AND s.scip_symbol NOT LIKE 'local %'
         ORDER BY o.start_line, o.start_col"
    ))?;
    let rows = stmt
        .query_map([file_id], row_to_result)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// `cq search` — FTS5 prefix search over symbol display names, returning the
/// matching symbols' definition sites in match-rank order. Matches without a
/// local definition (externals) are skipped; zero net matches → `NotFound`.
pub fn search(conn: &Connection, query: &str) -> Result<Vec<RawResult>> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        bail!("empty search query");
    }
    // Quoted phrase + prefix star; quoting makes user input inert to FTS5
    // query syntax (`"` escaped by doubling).
    let match_expr = format!("\"{}\"*", trimmed.replace('"', "\"\""));
    let mut stmt = conn.prepare(
        "SELECT s.id FROM symbols_fts ft
         JOIN symbols s ON s.id = ft.rowid
         WHERE symbols_fts MATCH ?1
         ORDER BY rank, s.id",
    )?;
    let ids = stmt
        .query_map([&match_expr], |r| r.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .with_context(|| format!("FTS5 search for {match_expr:?}"))?;

    // Preserve rank order: fetch definition sites per id, in order.
    let mut results = Vec::new();
    for id in ids {
        results.extend(occurrences_of(conn, &[id], true)?);
    }
    if results.is_empty() {
        return Err(CqError::NotFound {
            query: query.to_string(),
        }
        .into());
    }
    Ok(results)
}

/// `query_row` returns `QueryReturnedNoRows` for a miss; we want `None` for
/// that and a hard error for anything else.
trait OptionalNotFound<T> {
    fn optional_not_found(self) -> Result<Option<T>>;
}

impl<T> OptionalNotFound<T> for rusqlite::Result<T> {
    fn optional_not_found(self) -> Result<Option<T>> {
        match self {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ingest, schema};
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::OnceLock;

    // ---- golden DB built once per test process (pattern from src/ingest.rs
    // tests: copy fixture → git init+commit → rust-analyzer scip → ingest) ----

    fn run(cwd: &Path, prog: &str, args: &[&str]) {
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

    /// Path to a SQLite file holding the ingested golden crate. Emitted and
    /// ingested exactly once; every test opens its own read connection.
    fn golden_db() -> &'static Path {
        static DB: OnceLock<PathBuf> = OnceLock::new();
        DB.get_or_init(|| {
            let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/golden-crate");
            let dir = tempfile::tempdir().unwrap();
            copy_dir(&fixture, dir.path());
            run(dir.path(), "git", &["init", "-q", "-b", "main"]);
            run(dir.path(), "git", &["add", "-A"]);
            run(
                dir.path(),
                "git",
                &[
                    "-c",
                    "user.email=cq@test",
                    "-c",
                    "user.name=cq-test",
                    "commit",
                    "-q",
                    "-m",
                    "golden",
                ],
            );
            run(
                dir.path(),
                "rust-analyzer",
                &["scip", ".", "--output", "index.scip"],
            );
            let db_path = dir.path().join("query-golden.sqlite");
            let conn = Connection::open(&db_path).unwrap();
            schema::create(&conn).unwrap();
            ingest::ingest(&dir.path().join("index.scip"), &conn, dir.path()).unwrap();
            // Keep the tempdir alive for the whole test process; the OS
            // reaps TMPDIR. `into_path` would also work but is deprecated
            // in newer tempfile; forgetting the guard is equivalent here.
            std::mem::forget(dir);
            db_path
        })
    }

    fn conn() -> Connection {
        Connection::open(golden_db()).unwrap()
    }

    fn name(s: &str) -> Selector {
        Selector::Name(s.to_string())
    }

    fn pos(path: &str, line: u32, col: u32) -> Selector {
        Selector::Pos {
            path: path.to_string(),
            line,
            col,
        }
    }

    fn expect_not_found(err: anyhow::Error) -> CqError {
        match err.downcast_ref::<CqError>() {
            Some(e @ CqError::NotFound { .. }) => e.clone(),
            other => panic!("expected CqError::NotFound, got {other:?}"),
        }
    }

    // ---- the eight planned tests ----

    #[test]
    fn def_by_name() {
        // `pub fn double` is src/ops.rs line 1 (1-based) = 0-based line 0,
        // name token at cols 7..13.
        let results = def(&conn(), &name("double")).unwrap();
        assert_eq!(results.len(), 1, "{results:?}");
        let d = &results[0];
        assert_eq!(d.path, "src/ops.rs");
        assert_eq!((d.start_line, d.start_col, d.end_col), (0, 7, 13));
        assert!(d.is_definition());
        assert_eq!(d.display_name, "double");
        assert_eq!(
            d.scip_symbol,
            "rust-analyzer cargo golden 0.1.0 ops/double()."
        );
    }

    #[test]
    fn def_by_position() {
        // The `double` call site in src/lib.rs:
        //   pub fn top_level_fn(x: i32) -> i32 { ops::double(x) }
        // is 0-based (3, 42..48) → CLI 1-based line 4, cols 43..=48.
        // Probe both the first and last column of the token.
        for col in [43, 48] {
            let results = def(&conn(), &pos("src/lib.rs", 4, col)).unwrap();
            assert_eq!(results.len(), 1, "col {col}: {results:?}");
            assert_eq!(results[0].path, "src/ops.rs");
            assert_eq!(results[0].start_line, 0);
            assert_eq!(results[0].start_col, 7);
            assert_eq!(results[0].display_name, "double");
        }
    }

    #[test]
    fn refs_includes_def_flagged() {
        let results = refs(&conn(), &name("double")).unwrap();
        // Exactly: the def (ops.rs:0), the top_level_fn call (lib.rs:3) and
        // the fmt_user call (ops.rs:4). The call_double! macro-body call
        // emits no occurrence (known limitation; see callers_of_double).
        let sites: Vec<(&str, i64, i64, bool)> = results
            .iter()
            .map(|r| {
                (
                    r.path.as_str(),
                    r.start_line,
                    r.start_col,
                    r.is_definition(),
                )
            })
            .collect();
        assert_eq!(
            sites,
            vec![
                ("src/lib.rs", 3, 42, false),
                ("src/ops.rs", 0, 7, true),
                ("src/ops.rs", 4, 59, false),
            ],
            "{results:?}"
        );
    }

    #[test]
    fn callers_of_double() {
        let results = callers(&conn(), &name("double")).unwrap();
        let callers: std::collections::BTreeSet<&str> =
            results.iter().map(|r| r.display_name.as_str()).collect();
        assert!(
            callers.contains("top_level_fn") && callers.contains("fmt_user"),
            "expected top_level_fn and fmt_user in {callers:?}"
        );
        // Each caller is reported at its definition site.
        for r in &results {
            assert!(r.is_definition(), "{r:?}");
        }
        // Record (don't assert — spec §8 callers gate): whether the
        // macro-body call site in macro_caller is visible. Fixture emit
        // 2026-06-11 with rust-analyzer 2026-05-31: NOT visible.
        eprintln!(
            "callers gate: macro_caller present = {}",
            callers.contains("macro_caller")
        );
    }

    #[test]
    fn symbols_outline_for_file() {
        let results = symbols(&conn(), "src/shapes.rs").unwrap();
        // (display_name, SCIP kind): Trait=53, Struct=49, TraitMethod=70,
        // Method=26, Function=17 — values observed from the golden emit.
        let outline: Vec<(&str, i64)> = results
            .iter()
            .map(|r| (r.display_name.as_str(), r.kind))
            .collect();
        for expected in [
            ("Area", 53),
            ("Circle", 49),
            ("Sq", 49),
            ("total_area", 17),
            ("area", 70), // trait method declaration
            ("area", 26), // impl methods (x2)
        ] {
            assert!(
                outline.contains(&expected),
                "{expected:?} not in {outline:?}"
            );
        }
        // Locals (self, items, i) are excluded from the outline.
        assert!(
            !outline
                .iter()
                .any(|(n, _)| *n == "self" || *n == "items" || *n == "i"),
            "locals leaked into outline: {outline:?}"
        );
        // Source order.
        let mut sorted = results.clone();
        sorted.sort_by_key(|r| (r.start_line, r.start_col));
        assert_eq!(results, sorted);
    }

    #[test]
    fn search_prefix() {
        let results = search(&conn(), "gener").unwrap();
        assert!(
            results.iter().any(|r| r.display_name == "generic_max"),
            "{results:?}"
        );
        // Search results are definition sites.
        for r in &results {
            assert!(r.is_definition(), "{r:?}");
        }
    }

    #[test]
    fn unknown_symbol_is_not_found() {
        let c = conn();
        // Unknown name → NotFound, never empty success.
        expect_not_found(def(&c, &name("no_such_symbol_anywhere")).unwrap_err());
        expect_not_found(refs(&c, &name("no_such_symbol_anywhere")).unwrap_err());
        expect_not_found(callers(&c, &name("no_such_symbol_anywhere")).unwrap_err());
        // Unknown file → NotFound.
        expect_not_found(def(&c, &pos("src/nope.rs", 1, 1)).unwrap_err());
        expect_not_found(symbols(&c, "src/nope.rs").unwrap_err());
        // Position on no occurrence: past the end of the file. (Blank lines
        // inside the file resolve to the crate/module def occurrence, which
        // rust-analyzer emits spanning the whole file.)
        expect_not_found(def(&c, &pos("src/lib.rs", 100, 1)).unwrap_err());
        // Search with no match → NotFound.
        expect_not_found(search(&c, "zzz_no_match").unwrap_err());
    }

    #[test]
    fn trait_method_def_via_impl_position() {
        // Position on the `area` token of `i.area()` inside total_area:
        // src/shapes.rs 0-based (5, 71..75) → CLI 1-based line 6, col 72.
        //
        // Inspected emit (rust-analyzer 2026-05-31, 2026-06-11): the call
        // occurrence references ONLY the trait method
        // `shapes/Area#area().`; the impl methods are distinct symbols that
        // are NOT linked at the call site (SCIP relationships are not part
        // of the A1 schema). So the position resolves to exactly the trait
        // method definition.
        let results = def(&conn(), &pos("src/shapes.rs", 6, 72)).unwrap();
        assert_eq!(results.len(), 1, "{results:?}");
        let d = &results[0];
        assert_eq!(
            d.scip_symbol,
            "rust-analyzer cargo golden 0.1.0 shapes/Area#area()."
        );
        assert_eq!(
            (d.path.as_str(), d.start_line, d.start_col),
            ("src/shapes.rs", 0, 20)
        );
        assert_eq!(d.kind, 70); // SCIP TraitMethod

        // The impls remain reachable by name: def("area") surfaces the trait
        // method and both impl methods.
        let by_name = def(&conn(), &name("area")).unwrap();
        let syms: std::collections::BTreeSet<&str> =
            by_name.iter().map(|r| r.scip_symbol.as_str()).collect();
        assert_eq!(
            syms,
            [
                "rust-analyzer cargo golden 0.1.0 shapes/Area#area().",
                "rust-analyzer cargo golden 0.1.0 shapes/impl#[Circle][Area]area().",
                "rust-analyzer cargo golden 0.1.0 shapes/impl#[Sq][Area]area().",
            ]
            .into_iter()
            .collect()
        );
    }

    // ---- supplementary edge coverage ----

    #[test]
    fn position_resolution_picks_tightest_range() {
        // The module def occurrence for `shapes` spans the whole file
        // (0:0..6:0); a point on the `Area` trait name (0-based (0,10..14) →
        // 1-based line 1, col 11) must resolve to Area, not the module.
        let results = def(&conn(), &pos("src/shapes.rs", 1, 11)).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].display_name, "Area");

        // A point between tokens falls back to the enclosing (module) range.
        let results = def(&conn(), &pos("src/shapes.rs", 1, 16)).unwrap();
        assert_eq!(results.len(), 1, "{results:?}");
        assert_eq!(results[0].display_name, "shapes");
    }

    #[test]
    fn callers_of_uncalled_symbol_is_empty_ok() {
        // total_area is never called inside the fixture: a true empty answer.
        let results = callers(&conn(), &name("total_area")).unwrap();
        assert_eq!(results, vec![]);
    }

    #[test]
    fn zero_based_cli_position_is_rejected() {
        let err = def(&conn(), &pos("src/lib.rs", 0, 1)).unwrap_err();
        assert!(err.to_string().contains("1-based"), "{err}");
        let err = def(&conn(), &pos("src/lib.rs", 1, 0)).unwrap_err();
        assert!(err.to_string().contains("1-based"), "{err}");
    }

    #[test]
    fn search_input_is_inert_to_fts_syntax() {
        // FTS5 operators / quotes in user input must not be interpreted.
        expect_not_found(search(&conn(), "gener\" OR \"double").unwrap_err());
        assert!(search(&conn(), "  gener  ").is_ok(), "trimmed input works");
        assert!(search(&conn(), "   ").is_err());
    }
}
