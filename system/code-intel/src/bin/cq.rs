//! cq — stateless code-intelligence CLI (SPEC-A1 §5).
//!
//! Task 1 skeleton: all subcommands are declared but unimplemented. `main`
//! maps `CqError` to its spec exit code and prints the structured JSON error
//! to stderr; any other error exits 1 with the same JSON shape.

use clap::{Parser, Subcommand};
use scipd_core::error::CqError;

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
    /// Definition site(s) for a FILE:LINE:COL position (1-based) or symbol name
    Def {
        /// FILE:LINE:COL or symbol name
        target: String,
        /// Refuse (exit 2) instead of annotating when results touch stale files
        #[arg(long)]
        strict: bool,
    },
    /// All reference sites (definitions flagged)
    Refs {
        /// FILE:LINE:COL or symbol name
        target: String,
        #[arg(long)]
        strict: bool,
    },
    /// Enclosing symbols of call/reference sites
    Callers {
        /// Symbol name or FILE:LINE:COL
        target: String,
        #[arg(long)]
        strict: bool,
    },
    /// Symbol outline of one file
    Symbols {
        /// File path relative to the workspace root
        file: String,
        #[arg(long)]
        strict: bool,
    },
    /// FTS5 prefix/fuzzy search over symbol display names
    Search {
        query: String,
        #[arg(long)]
        strict: bool,
    },
    /// Emit + ingest + atomically publish a new index generation
    Index {
        /// Workspace path override (default: resolve from CWD)
        #[arg(long)]
        workspace: Option<String>,
    },
    /// Register a workspace in ~/.codeintel/registry.toml
    Register {
        /// Path to the workspace root
        path: String,
    },
    /// Per-workspace index health; exit nonzero on red
    Doctor {
        /// JSON output (always on; flag accepted for spec compatibility)
        #[arg(long)]
        json: bool,
    },
}

fn run(cli: Cli) -> Result<(), anyhow::Error> {
    match cli.command {
        Command::Def { .. } => anyhow::bail!("unimplemented: cq def (Task 9)"),
        Command::Refs { .. } => anyhow::bail!("unimplemented: cq refs (Task 9)"),
        Command::Callers { .. } => anyhow::bail!("unimplemented: cq callers (Task 9)"),
        Command::Symbols { .. } => anyhow::bail!("unimplemented: cq symbols (Task 9)"),
        Command::Search { .. } => anyhow::bail!("unimplemented: cq search (Task 9)"),
        Command::Index { .. } => anyhow::bail!("unimplemented: cq index (Task 8)"),
        Command::Register { .. } => anyhow::bail!("unimplemented: cq register (Task 2)"),
        Command::Doctor { .. } => anyhow::bail!("unimplemented: cq doctor (Task 9)"),
    }
}

fn main() {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => {}
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
                        "message": err.to_string(),
                        "hint": "this is a cq bug or an unimplemented verb; see docs/code-intel/SPEC-A1.md",
                    }
                })
            );
            std::process::exit(1);
        }
    }
}
