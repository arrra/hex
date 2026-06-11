//! cq — stateless code-intelligence CLI (SPEC-A1 §5).
//!
//! Every query verb goes through `scipd_core::respond::run`: response
//! envelope as a single JSON object on stdout, structured errors as JSON on
//! stderr, exit codes exactly per the spec §5 table (0 ok / 2 stale /
//! 3 NO_INDEX / 4 UNREGISTERED|UNSUPPORTED / 5 NOT_FOUND / 6 EMIT_FAILED).

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use scipd_core::doctor;
use scipd_core::error::CqError;
use scipd_core::indexer::{self, IndexOutcome};
use scipd_core::query::Selector;
use scipd_core::respond::{self, Verb};
use scipd_core::workspace::{codeintel_home, register_workspace, Registry, Workspace};

/// Positional-target heuristic, documented in `--help` (plan Task 9): an
/// argument whose two trailing `:`-separated segments are both positive
/// integers is parsed as a FILE:LINE:COL position (1-based); anything else —
/// including names with path separators or `::` like `ops::double` — is a
/// bare symbol name.
const TARGET_HELP: &str = "FILE:LINE:COL (1-based) or a bare symbol name. \
Parsed as a position when the two trailing ':'-separated segments are both \
positive integers (e.g. src/lib.rs:4:43); anything else (e.g. double, \
Vec::new) is a symbol name";

const WORKSPACE_HELP: &str = "Workspace path override (default: resolve from the current directory)";

#[derive(Parser)]
#[command(
    name = "cq",
    about = "Index-backed code intelligence queries (JSON output)",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Definition site(s) for a position or symbol name
    Def {
        #[arg(help = TARGET_HELP)]
        target: String,
        /// Refuse (exit 2) instead of annotating when results touch stale files
        #[arg(long)]
        strict: bool,
        #[arg(long, help = WORKSPACE_HELP)]
        workspace: Option<PathBuf>,
    },
    /// All reference sites (definitions flagged)
    Refs {
        #[arg(help = TARGET_HELP)]
        target: String,
        #[arg(long)]
        strict: bool,
        #[arg(long, help = WORKSPACE_HELP)]
        workspace: Option<PathBuf>,
    },
    /// Enclosing symbols of call/reference sites
    Callers {
        #[arg(help = TARGET_HELP)]
        target: String,
        #[arg(long)]
        strict: bool,
        #[arg(long, help = WORKSPACE_HELP)]
        workspace: Option<PathBuf>,
    },
    /// Symbol outline of one file
    Symbols {
        /// File path relative to the workspace root
        file: String,
        #[arg(long)]
        strict: bool,
        #[arg(long, help = WORKSPACE_HELP)]
        workspace: Option<PathBuf>,
    },
    /// FTS5 prefix/fuzzy search over symbol display names
    Search {
        query: String,
        #[arg(long)]
        strict: bool,
        #[arg(long, help = WORKSPACE_HELP)]
        workspace: Option<PathBuf>,
    },
    /// Emit + ingest + atomically publish a new index generation
    Index {
        #[arg(long, help = WORKSPACE_HELP)]
        workspace: Option<PathBuf>,
    },
    /// Register a workspace in ~/.codeintel/registry.toml
    Register {
        /// Path to the workspace root
        path: PathBuf,
    },
    /// Per-workspace index health; exit nonzero on red
    Doctor {
        /// JSON output (always on; flag accepted for spec compatibility)
        #[arg(long)]
        json: bool,
    },
}

/// FILE:LINE:COL vs bare-symbol heuristic (see [`TARGET_HELP`]). The 1-based
/// position is handed to the query layer, which owns the 1→0 conversion.
fn parse_target(target: &str) -> Selector {
    let mut tail = target.rsplitn(3, ':');
    if let (Some(col), Some(line), Some(path)) = (tail.next(), tail.next(), tail.next()) {
        if let (Ok(line), Ok(col)) = (line.parse::<u32>(), col.parse::<u32>()) {
            if line >= 1 && col >= 1 && !path.is_empty() {
                return Selector::Pos { path: path.to_string(), line, col };
            }
        }
    }
    Selector::Name(target.to_string())
}

/// Resolve the workspace for a query verb and require it to be registered
/// (spec §5: CWD not in a registered workspace → UNREGISTERED_WORKSPACE).
fn resolve_registered(home: &Path, workspace: Option<PathBuf>) -> Result<Workspace, anyhow::Error> {
    let dir = match workspace {
        Some(path) => path,
        None => std::env::current_dir()?,
    };
    let ws = Workspace::resolve(&dir)?;
    let registry = Registry::load(home)?;
    if !registry.contains(&ws.id) {
        return Err(CqError::UnregisteredWorkspace { cwd: dir.display().to_string() }.into());
    }
    Ok(ws)
}

/// Run one query verb end-to-end: envelope JSON to stdout, exit code per the
/// spec table (0 fresh, 2 stale-annotated; errors propagate as `CqError`).
fn run_query(
    workspace: Option<PathBuf>,
    verb: Verb,
    strict: bool,
) -> Result<i32, anyhow::Error> {
    let home = codeintel_home()?;
    let ws = resolve_registered(&home, workspace)?;
    let (envelope, exit_code) = respond::run(&home, &ws, &verb, strict)?;
    println!("{}", serde_json::to_string(&envelope)?);
    Ok(exit_code)
}

fn run(cli: Cli) -> Result<i32, anyhow::Error> {
    match cli.command {
        Command::Def { target, strict, workspace } => {
            run_query(workspace, Verb::Def(parse_target(&target)), strict)
        }
        Command::Refs { target, strict, workspace } => {
            run_query(workspace, Verb::Refs(parse_target(&target)), strict)
        }
        Command::Callers { target, strict, workspace } => {
            run_query(workspace, Verb::Callers(parse_target(&target)), strict)
        }
        Command::Symbols { file, strict, workspace } => {
            run_query(workspace, Verb::Symbols(file), strict)
        }
        Command::Search { query, strict, workspace } => {
            run_query(workspace, Verb::Search(query), strict)
        }
        Command::Index { workspace } => {
            let home = codeintel_home()?;
            let dir = match workspace {
                Some(path) => path,
                None => std::env::current_dir()?,
            };
            match indexer::run(&home, &dir)? {
                // Visible skip, exit 0 (spec §7): an emit is already in flight.
                IndexOutcome::SkippedInFlight => {
                    println!("{}", serde_json::json!({ "skipped": "emit-in-flight" }));
                }
                IndexOutcome::Completed(report) => {
                    println!("{}", serde_json::to_string(&report)?);
                }
            }
            Ok(0)
        }
        Command::Register { path } => {
            let home = codeintel_home()?;
            let entry = register_workspace(&home, &path)?;
            println!(
                "{}",
                serde_json::json!({ "registered": entry.id, "root": entry.root })
            );
            Ok(0)
        }
        Command::Doctor { .. } => {
            let home = codeintel_home()?;
            // Human-readable summary goes to stderr (inside doctor::run);
            // the JSON report is the only thing on stdout.
            let (report, exit_code) = doctor::run(&home)?;
            println!("{}", serde_json::to_string(&report)?);
            Ok(exit_code)
        }
    }
}

fn main() {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => std::process::exit(code),
        Err(err) => {
            // CqError carries its own spec exit code and JSON shape.
            if let Some(cq) = err.downcast_ref::<CqError>() {
                eprintln!("{}", cq.to_json());
                std::process::exit(cq.exit_code());
            }
            // Anything else is an internal failure: loud, structured, exit 1.
            eprintln!(
                "{}",
                serde_json::json!({
                    "error": {
                        "code": "INTERNAL",
                        "message": format!("{err:#}"),
                        "hint": "this is a cq bug; see docs/code-intel/SPEC-A1.md",
                    }
                })
            );
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_targets_need_two_trailing_numeric_segments() {
        assert_eq!(
            parse_target("src/lib.rs:4:43"),
            Selector::Pos { path: "src/lib.rs".into(), line: 4, col: 43 }
        );
        // Deep paths keep every leading segment.
        assert_eq!(
            parse_target("a/b:c/d.rs:12:1"),
            Selector::Pos { path: "a/b:c/d.rs".into(), line: 12, col: 1 }
        );
    }

    #[test]
    fn non_positions_are_symbol_names() {
        for name in [
            "double",
            "Vec::new",
            "ops::double",       // trailing segments not numeric
            "src/lib.rs:4",      // only one numeric segment
            "src/lib.rs:4:0",    // col 0 is not a 1-based position
            "src/lib.rs:0:4",    // line 0 is not a 1-based position
            ":4:5",              // empty path
            "src/lib.rs:4:43x",  // non-numeric col
        ] {
            assert_eq!(parse_target(name), Selector::Name(name.into()), "{name}");
        }
    }
}
