//! SCIP protobuf ingest into a generation SQLite database (spec §4).
//!
//! Reads an `index.scip` emitted by `rust-analyzer scip`, and populates the
//! schema created by [`crate::schema::create`] inside one transaction.
//!
//! Key derivation rules (smoke test #2, 2026-06-11 — see SPEC-A1 §4):
//!
//! - `enclosing_symbol_id` is derived by **containment only**: for each
//!   reference occurrence, the enclosing symbol is the definition occurrence
//!   in the same file whose span most tightly (strictly) contains the
//!   reference range, EXCLUDING module-like definitions
//!   (Module/Namespace/Package) so `use`-import lines inside inline modules
//!   don't masquerade as callers.
//! - The span of a *definition* occurrence is its definition-side
//!   `enclosing_range` (100% coverage from rust-analyzer; full body extent),
//!   falling back to `range` when absent.
//! - Reference-side `Occurrence.enclosing_range` is NEVER read: rust-analyzer
//!   populates it with the *referenced definition's* range, not the call
//!   site's scope. Using it would make callers() silently, totally wrong.
//! - rust-analyzer emits duplicate symbol strings (bin+lib targets); symbol
//!   and file rows are upserted, and occurrence rows deduplicated, instead of
//!   crashing on unique violations.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{bail, Context, Result};
use protobuf::Message as _;
use rusqlite::{params, Connection};
use scip::types::descriptor::Suffix;
use scip::types::symbol_information::Kind;
use scip::types::{Index, SymbolInformation};

/// SCIP `SymbolRole.Definition` bit.
const ROLE_DEFINITION: i64 = 1;

/// Counters returned by [`ingest`]; also what the indexer records in `meta`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    pub files: u64,
    pub symbols: u64,
    pub occurrences: u64,
}

/// A position as (line, column), 0-based per SCIP. Ord is lexicographic,
/// which is exactly document order.
type Point = (i64, i64);

/// Half-open `[start, end)` span.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Span {
    start: Point,
    end: Point,
}

impl Span {
    /// Strict containment: `self` covers `other` and is not equal to it.
    fn strictly_contains(&self, other: &Span) -> bool {
        self.start <= other.start
            && other.end <= self.end
            && !(self.start == other.start && self.end == other.end)
    }
}

/// Normalize a SCIP range (`[sl, sc, el, ec]` or `[sl, sc, ec]`) to a Span.
fn normalize_range(range: &[i32]) -> Result<Span> {
    match *range {
        [sl, sc, el, ec] => Ok(Span {
            start: (sl as i64, sc as i64),
            end: (el as i64, ec as i64),
        }),
        [sl, sc, ec] => Ok(Span {
            start: (sl as i64, sc as i64),
            end: (sl as i64, ec as i64),
        }),
        _ => bail!("malformed SCIP range {range:?} (expected 3 or 4 elements)"),
    }
}

/// Best-effort display name for symbols that carry no `SymbolInformation`
/// (or an empty `display_name`): the last descriptor's name from the SCIP
/// symbol grammar, else the raw symbol string (e.g. `local 7`).
fn derive_display_name(symbol: &str) -> String {
    if scip::symbol::is_local_symbol(symbol) {
        return symbol.to_string();
    }
    match scip::symbol::parse_symbol(symbol) {
        Ok(parsed) => parsed
            .descriptors
            .last()
            .map(|d| d.name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| symbol.to_string()),
        Err(_) => symbol.to_string(),
    }
}

/// True for module-like definitions (excluded as enclosing candidates).
/// Belt and suspenders: the `SymbolInformation.kind` when known, plus the
/// symbol grammar (last descriptor suffix `Namespace`, i.e. ends with `/`).
fn is_module_like(kind: i32, symbol: &str) -> bool {
    if kind == Kind::Module as i32 || kind == Kind::Namespace as i32 || kind == Kind::Package as i32
    {
        return true;
    }
    if scip::symbol::is_local_symbol(symbol) {
        return false;
    }
    matches!(
        scip::symbol::parse_symbol(symbol).ok().and_then(|p| {
            p.descriptors
                .last()
                .and_then(|d| d.suffix.enum_value().ok())
        }),
        Some(Suffix::Namespace)
    )
}

/// One `git ls-files -s -z` parse → repo-relative path → blob OID.
fn git_blob_oids(repo_root: &Path) -> Result<HashMap<String, String>> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["ls-files", "-s", "-z"])
        .output()
        .with_context(|| format!("spawning git ls-files in {}", repo_root.display()))?;
    if !out.status.success() {
        bail!(
            "git ls-files failed in {}: {}",
            repo_root.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let mut map = HashMap::new();
    for entry in out.stdout.split(|b| *b == 0).filter(|e| !e.is_empty()) {
        let entry = std::str::from_utf8(entry).context("non-UTF8 entry in git ls-files output")?;
        // Format: "<mode> <oid> <stage>\t<path>"
        let (meta, path) = entry
            .split_once('\t')
            .with_context(|| format!("unparseable git ls-files entry: {entry:?}"))?;
        let oid = meta
            .split(' ')
            .nth(1)
            .with_context(|| format!("unparseable git ls-files entry: {entry:?}"))?;
        map.insert(path.to_string(), oid.to_string());
    }
    Ok(map)
}

/// Upsert a symbol from `SymbolInformation` (graceful on duplicate symbol
/// strings from bin+lib targets: richer info wins, never a unique violation).
fn upsert_symbol_info(conn: &Connection, info: &SymbolInformation) -> Result<()> {
    let display_name = if info.display_name.is_empty() {
        derive_display_name(&info.symbol)
    } else {
        info.display_name.clone()
    };
    conn.execute(
        "INSERT INTO symbols (scip_symbol, display_name, kind, documentation)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(scip_symbol) DO UPDATE SET
           display_name = CASE WHEN excluded.display_name <> ''
                               THEN excluded.display_name ELSE symbols.display_name END,
           kind = CASE WHEN excluded.kind <> 0 THEN excluded.kind ELSE symbols.kind END,
           documentation = COALESCE(symbols.documentation, excluded.documentation)",
        params![
            info.symbol,
            display_name,
            info.kind.value(),
            info.documentation.first(),
        ],
    )
    .with_context(|| format!("upserting symbol {}", info.symbol))?;
    Ok(())
}

/// Get-or-insert a symbol row by SCIP symbol string, with an id cache.
fn ensure_symbol(
    conn: &Connection,
    cache: &mut HashMap<String, i64>,
    symbol: &str,
) -> Result<i64> {
    if let Some(id) = cache.get(symbol) {
        return Ok(*id);
    }
    conn.execute(
        "INSERT INTO symbols (scip_symbol, display_name) VALUES (?1, ?2)
         ON CONFLICT(scip_symbol) DO NOTHING",
        params![symbol, derive_display_name(symbol)],
    )?;
    let id: i64 = conn
        .query_row(
            "SELECT id FROM symbols WHERE scip_symbol = ?1",
            [symbol],
            |r| r.get(0),
        )
        .with_context(|| format!("symbol {symbol} missing after upsert"))?;
    cache.insert(symbol.to_string(), id);
    Ok(id)
}

/// Ingest `scip_path` into `conn` (schema already created), resolving blob
/// OIDs against the git repository at `repo_root`. One transaction;
/// durability PRAGMAs are off because a generation is a write-once artifact
/// that is re-emitted from scratch on any failure.
pub fn ingest(scip_path: &Path, conn: &Connection, repo_root: &Path) -> Result<Stats> {
    let bytes = std::fs::read(scip_path)
        .with_context(|| format!("reading SCIP index {}", scip_path.display()))?;
    let index = Index::parse_from_bytes(&bytes)
        .with_context(|| format!("parsing SCIP protobuf {}", scip_path.display()))?;
    let blob_oids = git_blob_oids(repo_root)?;

    // journal_mode returns the resulting mode as a row, so query it.
    // In-memory databases only support MEMORY/OFF; both are fine here.
    conn.query_row("PRAGMA journal_mode = OFF", [], |r| r.get::<_, String>(0))?;
    conn.pragma_update(None, "synchronous", "OFF")?;
    let tx = conn.unchecked_transaction()?;

    // Pass 1: symbol table. Document symbols first, then external symbols.
    let mut symbol_kinds: HashMap<String, i32> = HashMap::new();
    for info in index
        .documents
        .iter()
        .flat_map(|d| d.symbols.iter())
        .chain(index.external_symbols.iter())
    {
        upsert_symbol_info(&tx, info)?;
        let kind = info.kind.value();
        if kind != 0 {
            symbol_kinds.insert(info.symbol.clone(), kind);
        }
    }

    // Pass 2: files and occurrences, deriving enclosing_symbol_id per file.
    let mut symbol_ids: HashMap<String, i64> = HashMap::new();
    let mut file_ids: HashMap<String, i64> = HashMap::new();
    let mut seen_occurrences: HashSet<(i64, i64, i64, i64, i64, i64, i64)> = HashSet::new();
    let mut occurrence_count: u64 = 0;
    {
        let mut insert_occ = tx.prepare(
            "INSERT INTO occurrences
               (file_id, symbol_id, start_line, start_col, end_line, end_col, roles,
                enclosing_symbol_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        for doc in &index.documents {
            let file_id = match file_ids.get(&doc.relative_path) {
                Some(id) => *id,
                None => {
                    let blob_oid = blob_oids.get(&doc.relative_path).with_context(|| {
                        format!(
                            "file {} from SCIP index has no git blob OID in {} \
                             (untracked or uncommitted?)",
                            doc.relative_path,
                            repo_root.display()
                        )
                    })?;
                    tx.execute(
                        "INSERT INTO files (path, blob_oid, language) VALUES (?1, ?2, ?3)
                         ON CONFLICT(path) DO NOTHING",
                        params![doc.relative_path, blob_oid, doc.language],
                    )?;
                    let id: i64 = tx.query_row(
                        "SELECT id FROM files WHERE path = ?1",
                        [&doc.relative_path],
                        |r| r.get(0),
                    )?;
                    file_ids.insert(doc.relative_path.clone(), id);
                    id
                }
            };

            // Enclosing candidates: definition occurrences in this document,
            // spanned by their definition-side enclosing_range (full body
            // extent; falls back to the name-token range when absent),
            // excluding module-like definitions.
            let mut candidates: Vec<(Span, &str)> = Vec::new();
            for occ in &doc.occurrences {
                if occ.symbol.is_empty() || (occ.symbol_roles as i64) & ROLE_DEFINITION == 0 {
                    continue;
                }
                let kind = symbol_kinds.get(&occ.symbol).copied().unwrap_or(0);
                if is_module_like(kind, &occ.symbol) {
                    continue;
                }
                let span = if occ.enclosing_range.is_empty() {
                    normalize_range(&occ.range)?
                } else {
                    normalize_range(&occ.enclosing_range)?
                };
                candidates.push((span, occ.symbol.as_str()));
            }

            for occ in &doc.occurrences {
                if occ.symbol.is_empty() {
                    // Syntax-highlighting-only occurrence: no symbol to index.
                    continue;
                }
                let range = normalize_range(&occ.range).with_context(|| {
                    format!("occurrence of {} in {}", occ.symbol, doc.relative_path)
                })?;
                let roles = occ.symbol_roles as i64;
                let symbol_id = ensure_symbol(&tx, &mut symbol_ids, &occ.symbol)?;

                // Containment-ONLY derivation for references; definitions get
                // NULL. occ.enclosing_range is intentionally not consulted
                // here (reference-side values are the referenced definition's
                // range — smoke test #2).
                let enclosing_symbol_id = if roles & ROLE_DEFINITION == 0 {
                    let mut best: Option<(Span, &str)> = None;
                    for cand in &candidates {
                        if !cand.0.strictly_contains(&range) {
                            continue;
                        }
                        let tighter = match best {
                            None => true,
                            // Properly nested spans: the tightest container
                            // has the greatest start (tie: least end).
                            Some((b, _)) => {
                                cand.0.start > b.start
                                    || (cand.0.start == b.start && cand.0.end < b.end)
                            }
                        };
                        if tighter {
                            best = Some(*cand);
                        }
                    }
                    match best {
                        Some((_, sym)) => Some(ensure_symbol(&tx, &mut symbol_ids, sym)?),
                        None => None,
                    }
                } else {
                    None
                };

                let key = (
                    file_id,
                    symbol_id,
                    range.start.0,
                    range.start.1,
                    range.end.0,
                    range.end.1,
                    roles,
                );
                if !seen_occurrences.insert(key) {
                    continue; // duplicate document emission (bin+lib targets)
                }
                insert_occ.execute(params![
                    file_id,
                    symbol_id,
                    range.start.0,
                    range.start.1,
                    range.end.0,
                    range.end.1,
                    roles,
                    enclosing_symbol_id,
                ])?;
                occurrence_count += 1;
            }
        }
    }

    // FTS5 is external-content and the index is write-once: one manual sync.
    tx.execute(
        "INSERT INTO symbols_fts (rowid, display_name)
         SELECT id, display_name FROM symbols",
        [],
    )?;
    tx.commit()?;

    let symbols: i64 = conn.query_row("SELECT count(*) FROM symbols", [], |r| r.get(0))?;
    Ok(Stats {
        files: file_ids.len() as u64,
        symbols: symbols as u64,
        occurrences: occurrence_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema;
    use std::path::PathBuf;
    use std::process::Command;
    use tempfile::TempDir;

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

    fn git_repo_with(files: &[(&str, &str)]) -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, contents) in files {
            let p = dir.path().join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, contents).unwrap();
        }
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
                "fixture",
            ],
        );
        dir
    }

    /// Copy the golden fixture crate to a tempdir, git-init + commit it, and
    /// run `rust-analyzer scip .` there (requires rust-analyzer on PATH).
    fn emit_golden() -> (TempDir, PathBuf) {
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
        let scip = dir.path().join("index.scip");
        assert!(scip.exists(), "rust-analyzer scip produced no index.scip");
        (dir, scip)
    }

    #[test]
    fn ingest_golden_crate_populates_schema() {
        let (dir, scip) = emit_golden();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        schema::create(&conn).unwrap();
        let stats = ingest(&scip, &conn, dir.path()).unwrap();
        eprintln!("golden ingest stats: {stats:?}");
        assert!(stats.symbols >= 10, "got {}", stats.symbols);
        assert!(stats.files >= 3, "got {}", stats.files);
        assert!(stats.occurrences > 0);

        // Exactly one definition of `double`.
        let defs: i64 = conn
            .query_row(
                "SELECT count(*) FROM occurrences o JOIN symbols s ON s.id = o.symbol_id \
                 WHERE s.display_name = 'double' AND o.roles & 1 != 0",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(defs, 1);

        // Blob OIDs recorded for every file.
        let no_oid: i64 = conn
            .query_row(
                "SELECT count(*) FROM files WHERE blob_oid = ''",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(no_oid, 0);

        // Enclosing symbol derived for the double() call inside top_level_fn.
        let encl: String = conn
            .query_row(
                "SELECT es.display_name FROM occurrences o \
                 JOIN symbols s ON s.id = o.symbol_id \
                 JOIN symbols es ON es.id = o.enclosing_symbol_id \
                 JOIN files f ON f.id = o.file_id \
                 WHERE s.display_name = 'double' AND o.roles & 1 = 0 AND f.path = 'src/lib.rs'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(encl, "top_level_fn");

        // Definitions never get an enclosing symbol.
        let def_with_encl: i64 = conn
            .query_row(
                "SELECT count(*) FROM occurrences \
                 WHERE roles & 1 != 0 AND enclosing_symbol_id IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(def_with_encl, 0);

        // FTS5 index populated and queryable.
        let hit: String = conn
            .query_row(
                "SELECT display_name FROM symbols_fts WHERE symbols_fts MATCH 'gener*' LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hit, "generic_max");
    }

    #[test]
    fn ingest_handles_duplicate_symbols_and_documents() {
        // Simulate the rust-analyzer bin+lib emission: the same document and
        // symbol appear twice in one index. Must upsert, never crash.
        use scip::types::{Document, Occurrence};

        let repo = git_repo_with(&[("src/lib.rs", "pub fn f() {}\n")]);
        let sym = "rust-analyzer cargo dup 0.1.0 dup/f().";

        let mut info = SymbolInformation::new();
        info.symbol = sym.to_string();
        info.display_name = "f".to_string();
        info.kind = Kind::Function.into();

        let mut def = Occurrence::new();
        def.symbol = sym.to_string();
        def.range = vec![0, 7, 8];
        def.enclosing_range = vec![0, 0, 0, 13];
        def.symbol_roles = 1;

        let make_doc = || {
            let mut d = Document::new();
            d.relative_path = "src/lib.rs".to_string();
            d.language = "rust".to_string();
            d.symbols = vec![info.clone()];
            d.occurrences = vec![def.clone()];
            d
        };
        let mut index = Index::new();
        index.documents = vec![make_doc(), make_doc()];

        let scip_path = repo.path().join("index.scip");
        std::fs::write(&scip_path, index.write_to_bytes().unwrap()).unwrap();

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        schema::create(&conn).unwrap();
        let stats = ingest(&scip_path, &conn, repo.path()).unwrap();
        assert_eq!(stats.files, 1);
        assert_eq!(stats.occurrences, 1, "duplicate occurrence not deduped");
        let rows: i64 = conn
            .query_row(
                "SELECT count(*) FROM symbols WHERE scip_symbol = ?1",
                [sym],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rows, 1);
    }

    #[test]
    fn ingest_fails_loudly_on_untracked_file() {
        use scip::types::Document;

        let repo = git_repo_with(&[("src/lib.rs", "pub fn f() {}\n")]);
        let mut doc = Document::new();
        doc.relative_path = "src/not_in_git.rs".to_string();
        doc.language = "rust".to_string();
        let mut index = Index::new();
        index.documents = vec![doc];

        let scip_path = repo.path().join("index.scip");
        std::fs::write(&scip_path, index.write_to_bytes().unwrap()).unwrap();

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        schema::create(&conn).unwrap();
        let err = ingest(&scip_path, &conn, repo.path()).unwrap_err();
        assert!(err.to_string().contains("no git blob OID"), "{err}");
    }

    #[test]
    fn normalize_range_handles_three_and_four_elements() {
        let four = normalize_range(&[1, 2, 3, 4]).unwrap();
        assert_eq!(four.start, (1, 2));
        assert_eq!(four.end, (3, 4));
        let three = normalize_range(&[5, 1, 9]).unwrap();
        assert_eq!(three.start, (5, 1));
        assert_eq!(three.end, (5, 9));
        assert!(normalize_range(&[1, 2]).is_err());
        assert!(normalize_range(&[]).is_err());
    }

    #[test]
    fn strict_containment_excludes_equal_spans() {
        let outer = Span {
            start: (0, 0),
            end: (10, 0),
        };
        let inner = Span {
            start: (2, 4),
            end: (2, 10),
        };
        assert!(outer.strictly_contains(&inner));
        assert!(!inner.strictly_contains(&outer));
        assert!(!inner.strictly_contains(&inner));
    }

    #[test]
    fn derive_display_name_from_symbol_grammar() {
        assert_eq!(
            derive_display_name("rust-analyzer cargo golden 0.1.0 ops/double()."),
            "double"
        );
        assert_eq!(derive_display_name("local 7"), "local 7");
    }

    #[test]
    fn module_like_symbols_are_excluded_as_enclosing_candidates() {
        assert!(is_module_like(
            Kind::Module as i32,
            "rust-analyzer cargo golden 0.1.0 ops/"
        ));
        // Kind unknown but the symbol grammar says namespace (trailing `/`).
        assert!(is_module_like(0, "rust-analyzer cargo golden 0.1.0 ops/"));
        assert!(!is_module_like(
            Kind::Function as i32,
            "rust-analyzer cargo golden 0.1.0 ops/double()."
        ));
        assert!(!is_module_like(0, "local 7"));
    }
}
