use clap::{CommandFactory, Parser, Subcommand};
use std::io;
use std::path::{Path, PathBuf};

mod consolidate;
mod doctor;
mod integration;
mod integration_cmd;
mod checkpoint;
mod shutdown;
mod startup;
mod integration_check_all;
use hex::memory;
mod path_map;
mod session_reflect;
mod env;
mod hook;
mod upgrade;
mod learnings;
// Personal modules (mrap-only overlay). Resolved via build.rs → OUT_DIR/personal_mods.rs.
#[cfg(feature = "personal")]
include!(concat!(env!("OUT_DIR"), "/personal_mods.rs"));
#[derive(Parser)]
#[command(name = "hex", about = "Hex harness", version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Integration bundle lifecycle management
    #[command(display_order = 9)]
    Integration {
        #[command(subcommand)]
        command: IntegrationCommands,
    },
    /// Behavioral and indexed memory operations
    #[command(display_order = 2)]
    Memory {
        #[command(subcommand)]
        command: MemoryCommands,
    },
    /// System health checks
    #[command(display_order = 5)]
    Doctor {
        #[command(subcommand)]
        command: DoctorCommands,
    },
    /// Session lifecycle commands
    #[command(display_order = 3)]
    Session {
        #[command(subcommand)]
        command: SessionCommands,
    },
    /// Environment setup utilities (Phase 5: port of env.sh non-shell logic)
    #[command(display_order = 24)]
    Env {
        #[command(subcommand)]
        command: env::EnvCommands,
    },
    /// Upgrade hex installation (port of system/scripts/upgrade.sh)
    #[command(display_order = 14)]
    Upgrade {
        /// Extra arguments forwarded to upgrade.sh
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Deterministic LLM-free release pipeline (wraps system/scripts/release.sh)
    #[cfg(feature = "personal")]
    #[command(display_order = 18)]
    Release {
        /// Explicit release version (e.g. 1.2.3); if omitted uses Cargo.toml version
        #[arg(long)]
        version: Option<String>,
        /// Skip Docker E2E and Codex parity gates (emergency bypass)
        #[arg(long)]
        skip_e2e: bool,
        /// Run gates only — no push, no tag, no GitHub release
        #[arg(long)]
        dry_run: bool,
    },
    /// Claude Code hook runners (port of .hex/hooks/scripts/*.sh)
    #[command(display_order = 13)]
    Hook {
        #[command(subcommand)]
        command: hook::HookCommands,
    },
    /// Print version
    #[command(display_order = 15)]
    Version,
    /// Generate shell completions
    #[command(display_order = 12)]
    Completions {
        shell: clap_complete::Shell,
    },
}

#[derive(Subcommand)]
enum ConsolidateCommands {
    /// Layers 1+2 only — deterministic, no LLM, no network. Safe to run nightly.
    Quick,
    /// Layers 1+2+3 — adds the LLM-assisted operating-model audit.
    Full,
}

#[derive(Subcommand)]
enum LearningsCommands {
    /// Scan learnings.md for recurring patterns and write promotion candidates to evolution/suggestions.md
    Promote {
        /// Print candidates without writing any files
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum IntegrationCommands {
    /// Install an integration bundle
    Install { name: String },
    /// Uninstall an integration bundle
    Uninstall { name: String },
    /// Update an integration bundle
    Update { name: String },
    /// List installed integrations
    List,
    /// Validate an integration bundle
    Validate { name: String },
    /// Show integration status
    Status { name: Option<String> },
    /// Probe an integration's connectivity
    Probe { name: String },
    /// Rotate an integration's credentials
    Rotate { name: String },
    /// Print integration health-check template to stdout (port of integrations/_template.sh)
    Template,
    /// Run Exa MCP health probe (port of integrations/mcp-exa.sh)
    #[cfg(feature = "personal")]
    #[command(name = "mcp-exa")]
    McpExa,
    /// Run Excalidraw MCP health probe (port of integrations/mcp-excalidraw.sh)
    #[cfg(feature = "personal")]
    #[command(name = "mcp-excalidraw")]
    McpExcalidraw,
    /// Run ECC plugin health probe (port of integrations/mcp-plugin-ecc.sh)
    #[cfg(feature = "personal")]
    #[command(name = "mcp-plugin-ecc")]
    McpPluginEcc,
    /// Run X (Twitter) API bearer token probe (port of integrations/x-twitter.sh)
    #[cfg(feature = "personal")]
    #[command(name = "x-twitter")]
    XTwitter,
    /// Run Apple Contacts TCC access probe (port of integrations/apple-addressbook.sh)
    #[cfg(feature = "personal")]
    #[command(name = "apple-addressbook")]
    AppleAddressbook,
    /// Run Tailscale daemon and peer connectivity probe (port of integrations/tailscale.sh)
    #[cfg(feature = "personal")]
    #[command(name = "tailscale")]
    Tailscale,
    /// Run Publer API health probe (port of integrations/publer.sh)
    #[cfg(feature = "personal")]
    #[command(name = "publer")]
    Publer,
    /// Run Granola MCP health probe (port of integrations/granola-mcp.sh)
    #[cfg(feature = "personal")]
    #[command(name = "granola-mcp")]
    GranolaMcp,
    /// Run integration checks for a tier in parallel (port of hex-integration-check-all.sh)
    #[command(name = "check-all")]
    CheckAll {
        /// Tier to check: critical, standard, slow, or all (default: all)
        #[arg(long, default_value = "all")]
        tier: String,
    },
    /// Post weekly integrations summary to #integrations (port of integrations-digest.sh)
    Digest,
    /// Run one integration sub-check, update state, emit events (port of hex-integration-check.sh)
    #[command(name = "run-check")]
    RunCheck {
        /// Integration name to check
        name: String,
    },
}

#[derive(Subcommand)]
enum MemoryCommands {
    /// Show memory database health/stats (alias for `stats`)
    Health,
    /// Learnings analysis and promotion (port of system/scripts/promote-learnings.py)
    Learnings {
        #[command(subcommand)]
        command: LearningsCommands,
    },
    /// Unified consolidation (structural + memory + operating-model audit)
    Consolidate {
        #[command(subcommand)]
        command: ConsolidateCommands,
    },
    /// Search indexed memory files
    Search {
        query: String,
        /// Number of results (default 10)
        #[arg(long, default_value = "10")]
        top: usize,
        /// Filter results to paths matching this pattern
        #[arg(long)]
        file: Option<String>,
        /// Compact single-line output per result
        #[arg(long)]
        compact: bool,
        /// Show N lines of context around matching terms
        #[arg(long)]
        context: Option<usize>,
        /// Exclude sensitive paths (me/, people/, raw/)
        #[arg(long)]
        private: bool,
    },
    /// Index memory files
    Index {
        #[arg(long)]
        full: bool,
        #[arg(long)]
        stats: bool,
    },
    /// Parse Claude JSONL transcripts to markdown
    #[command(name = "parse-transcripts")]
    ParseTranscripts {
        #[arg(long)]
        file: Option<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        force: bool,
    },
    /// Retrieve workspace memory relevant to a query (FTS5 contextual recall)
    Recall {
        query: String,
        /// Apply the private filter (for BOI worker consumers)
        #[arg(long)]
        agent: bool,
    },
    /// Run the memory smoke-eval + consumption-rate check (nightly)
    Eval {
        /// Print only the 7-day consumption rate (decimal) and exit 0.
        #[arg(long = "rate-only")]
        rate_only: bool,
    },
    /// Check LLM provider reachability (exits 0 ok, 2 deferred, 3 upstream)
    #[command(name = "llm-check")]
    LlmCheck,
    /// Distill facts from a file into the memory facts layer
    Distill {
        /// Path to the file to distill
        path: PathBuf,
    },
    /// Show memory database statistics (facts, files, predicates, schema version)
    Stats {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Verify sqlite-vec is loadable and memory.db has indexed vectors
    #[command(name = "check-vector-search")]
    CheckVectorSearch,
    /// Check daily reflection log freshness (evolution/reflection-log.md)
    #[command(name = "check-reflection-liveness")]
    CheckReflectionLiveness,
}

#[derive(Subcommand)]
enum SessionCommands {
    /// Run the hex session startup checklist (port of .hex/scripts/startup.sh)
    Startup {
        /// Skip slow steps (integration pulls, evolution engine, priority scoring)
        #[arg(long)]
        quick: bool,
        /// Run a single named step and exit (see --status for names)
        #[arg(long)]
        step: Option<String>,
        /// List available steps and exit
        #[arg(long)]
        status: bool,
    },
    /// Checkpoint the current session (port of /hex-checkpoint slash command)
    Checkpoint {
        /// What to work on next (used in compact suggestion and handoff)
        focus: Option<String>,
    },
    /// Close the current session (port of /hex-shutdown slash command)
    Shutdown {
        /// Session ID to deregister (from startup output); omit to get manual instructions
        session_id: Option<String>,
    },
    /// Post-session reflection: update reflection-log.md and persist eval_records to memory.db
    Reflect {
        /// Session identifier to record in the reflection log
        #[arg(long)]
        session_id: Option<String>,
        /// Suppress informational output
        #[arg(long)]
        quiet: bool,
    },
}

#[derive(Subcommand)]
enum DoctorCommands {
    /// Run all registered DoctorCheck impls (Rust framework)
    Run {
        #[arg(long)]
        fix: bool,
        #[arg(long)]
        smoke: bool,
        #[arg(long)]
        quiet: bool,
        #[arg(long)]
        json: bool,
        /// Only run checks whose name contains this pattern
        #[arg(long)]
        filter: Option<String>,
    },
    /// List all registered checks
    List,
    /// Check Codex CLI + config health (port of doctor-checks/codex.sh)
    #[command(name = "check-codex")]
    CheckCodex,
    /// Gaming detector for BOI initiative loop specs (port of quality-check.py)
    #[command(name = "quality-check")]
    QualityCheck {
        /// Check a specific spec by ID
        #[arg(long)]
        spec: Option<String>,
        /// Sweep all open specs
        #[arg(long)]
        sweep: bool,
        /// Check a specific KR path
        #[arg(long)]
        kr: Option<String>,
    },
    /// Nightly system health audit via claude -p (port of system-introspection.sh)
    Introspect,
    /// Delete Claude project .jsonl files older than N days (port of cleanup-project-jsonl.sh)
    #[command(name = "cleanup-projects")]
    CleanupProjects {
        /// Retention period in days (default 30)
        #[arg(default_value = "30")]
        days: u32,
    },
    /// Scan for stale dependency-blocked items (port of stale_deps.py)
    #[command(name = "stale-deps")]
    StaleDeps {
        /// Days threshold before an item is considered stale
        #[arg(long, default_value = "2")]
        threshold: u32,
        /// Output JSON
        #[arg(long)]
        json: bool,
    },
    /// Detect three-strike failure patterns in the BOI queue (port of detect-failure-pattern.py)
    #[command(name = "detect-failure-pattern")]
    DetectFailurePattern {
        /// Lookback window in seconds
        #[arg(long, default_value = "86400")]
        window: u64,
        /// Optional spec ID to scope the pattern check
        spec_id: Option<String>,
    },
}

/// Native implementation of the former health/check-vector-search.sh.
/// Opens memory.db via open_db (which loads sqlite-vec), then counts vec_chunks rows.
/// Exit 0 if loadable AND count > 0; exit 1 with stderr message otherwise.
fn check_vector_search(hex_dir: &std::path::Path) -> i32 {
    let db_path = memory::db_path(hex_dir);
    if !db_path.exists() {
        eprintln!(
            "check-vector-search: FAIL — memory.db not found at {}",
            db_path.display()
        );
        return 1;
    }
    let conn = match memory::open_db(&db_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("check-vector-search: FAIL — cannot open memory.db: {e}");
            return 1;
        }
    };
    // open_db already called register_sqlite_vec(); if vec0 isn't available the
    // COUNT query will fail with "no such module: vec0" or "no such table".
    match conn.query_row("SELECT COUNT(*) FROM vec_chunks", [], |r| r.get::<_, i64>(0)) {
        Ok(0) => {
            eprintln!("check-vector-search: FAIL — vec_chunks is empty (no vectors indexed). Run `hex memory index --full`.");
            1
        }
        Ok(n) => {
            println!("check-vector-search: ok — {n} vectors in vec_chunks");
            0
        }
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("no such module") {
                eprintln!("check-vector-search: FAIL — sqlite-vec extension not loadable: {msg}");
            } else if msg.contains("no such table") {
                eprintln!("check-vector-search: FAIL — vec_chunks table missing. Run `hex memory index --full`.");
            } else {
                eprintln!("check-vector-search: FAIL — sqlite error: {msg}");
            }
            1
        }
    }
}

/// Native implementation of the former health/check-reflection-liveness.sh.
/// Checks that evolution/reflection-log.md was modified within the last 48 hours.
/// Exit 0 if fresh; exit 1 with a clear stderr message if stale or missing.
/// (48h threshold: a session that ran yesterday should have a reflection entry.)
fn check_reflection_liveness(hex_dir: &std::path::Path) -> i32 {
    let log_path = hex_dir.join("evolution/reflection-log.md");
    if !log_path.exists() {
        eprintln!(
            "check-reflection-liveness: FAIL — reflection-log.md not found at {}. Run `hex session reflect` after a session.",
            log_path.display()
        );
        return 1;
    }
    match std::fs::metadata(&log_path).and_then(|m| m.modified()) {
        Ok(modified) => {
            let elapsed = modified.elapsed().unwrap_or(std::time::Duration::MAX);
            let threshold = std::time::Duration::from_secs(48 * 3600);
            if elapsed > threshold {
                let hours = elapsed.as_secs() / 3600;
                eprintln!(
                    "check-reflection-liveness: FAIL — reflection-log.md last updated {hours}h ago (threshold: 48h). Run `hex session reflect`."
                );
                1
            } else {
                let hours = elapsed.as_secs() / 3600;
                println!("check-reflection-liveness: ok — reflection-log.md updated {hours}h ago");
                0
            }
        }
        Err(e) => {
            eprintln!("check-reflection-liveness: FAIL — cannot read mtime of reflection-log.md: {e}");
            1
        }
    }
}

/// Parse a single top-level `key: value` from raw YAML text (no nesting).
/// Run a shell or Python script, streaming stdout/stderr, return exit code.
fn exec_script(script: &Path, args: &[&str]) -> i32 {
    if !script.exists() {
        eprintln!("ERROR: script not found: {}", script.display());
        return 1;
    }
    let ext = script.extension().and_then(|e| e.to_str()).unwrap_or("");
    let mut cmd = if ext == "py" {
        let mut c = std::process::Command::new("python3");
        c.arg(script);
        c
    } else {
        let mut c = std::process::Command::new("bash");
        c.arg(script);
        c
    };
    for a in args { cmd.arg(a); }
    cmd.stdout(std::process::Stdio::inherit());
    cmd.stderr(std::process::Stdio::inherit());
    match cmd.status() {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => { eprintln!("ERROR: failed to exec {}: {e}", script.display()); 1 }
    }
}

fn get_hex_dir() -> PathBuf {
    if let Ok(v) = std::env::var("HEX_DIR") {
        let p = PathBuf::from(&v);
        if !p.join("CLAUDE.md").exists() {
            eprintln!(
                "ERROR: HEX_DIR={} does not contain CLAUDE.md — not a valid hex workspace",
                v
            );
            std::process::exit(1);
        }
        return p;
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| {
        eprintln!("ERROR: neither HEX_DIR nor HOME is set");
        std::process::exit(1);
    });
    let p = PathBuf::from(&home).join("hex");
    if !p.join("CLAUDE.md").exists() {
        eprintln!(
            "ERROR: default hex dir {} does not contain CLAUDE.md — set HEX_DIR explicitly",
            p.display()
        );
        std::process::exit(1);
    }
    p
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Integration { command } => {
            if let IntegrationCommands::Template = command {
                integration::template();
                return;
            }
            #[cfg(feature = "personal")]
            if let IntegrationCommands::McpExa = command {
                std::process::exit(integration_mcp_exa::run_probe());
            }
            #[cfg(feature = "personal")]
            if let IntegrationCommands::McpExcalidraw = command {
                std::process::exit(integration_mcp_excalidraw::run_probe());
            }
            #[cfg(feature = "personal")]
            if let IntegrationCommands::McpPluginEcc = command {
                std::process::exit(integration_mcp_plugin_ecc::run_probe());
            }
            #[cfg(feature = "personal")]
            if let IntegrationCommands::XTwitter = command {
                std::process::exit(integration_x_twitter::run_probe());
            }
            #[cfg(feature = "personal")]
            if let IntegrationCommands::AppleAddressbook = command {
                std::process::exit(integration_apple_addressbook::run_probe());
            }
            #[cfg(feature = "personal")]
            if let IntegrationCommands::Tailscale = command {
                std::process::exit(integration_tailscale::run_probe());
            }
            #[cfg(feature = "personal")]
            if let IntegrationCommands::Publer = command {
                std::process::exit(integration_publer::run_probe());
            }
            #[cfg(feature = "personal")]
            if let IntegrationCommands::GranolaMcp = command {
                std::process::exit(integration_granola_mcp::run_probe());
            }
            if let IntegrationCommands::CheckAll { ref tier } = command {
                let hex_dir = get_hex_dir();
                let code = integration_check_all::run(&hex_dir, tier);
                std::process::exit(code);
            }
            if let IntegrationCommands::Digest = command {
                let hex_dir = get_hex_dir();
                let script = hex_dir.join("system/scripts/integrations-digest.sh");
                std::process::exit(exec_script(&script, &[]));
            }
            if let IntegrationCommands::RunCheck { ref name } = command {
                let hex_dir = get_hex_dir();
                let script = hex_dir.join("system/scripts/hex-integration-check.sh");
                std::process::exit(exec_script(&script, &[name]));
            }
            // Native Rust ports of Python integration commands
            if let IntegrationCommands::List = command {
                let hex_dir = get_hex_dir();
                std::process::exit(integration_cmd::list(&hex_dir, false));
            }
            if let IntegrationCommands::Status { ref name } = command {
                let hex_dir = get_hex_dir();
                std::process::exit(integration_cmd::status(&hex_dir, name.as_deref(), false));
            }
            if let IntegrationCommands::Probe { ref name } = command {
                let hex_dir = get_hex_dir();
                std::process::exit(integration_cmd::probe(&hex_dir, name, false, false));
            }
            if let IntegrationCommands::Rotate { ref name } = command {
                let hex_dir = get_hex_dir();
                std::process::exit(integration_cmd::rotate(&hex_dir, name, false, false));
            }
            if let IntegrationCommands::Validate { ref name } = command {
                let hex_dir = get_hex_dir();
                std::process::exit(integration_cmd::validate(&hex_dir, name, false, false));
            }
            if let IntegrationCommands::Update { ref name } = command {
                let hex_dir = get_hex_dir();
                std::process::exit(integration_cmd::update(&hex_dir, name, false, false, false, false));
            }
            let hex_dir = get_hex_dir();
            let script = hex_dir.join(".hex/scripts/hex-integration");
            let (subcmd, name_arg): (&str, Option<String>) = match &command {
                IntegrationCommands::Install { name } => ("install", Some(name.clone())),
                IntegrationCommands::Uninstall { name } => ("uninstall", Some(name.clone())),
                IntegrationCommands::Update { .. } => unreachable!(),
                IntegrationCommands::List => unreachable!(),
                IntegrationCommands::Validate { .. } => unreachable!(),
                IntegrationCommands::Status { .. } => unreachable!(),
                IntegrationCommands::Probe { .. } => unreachable!(),
                IntegrationCommands::Rotate { .. } => unreachable!(),
                IntegrationCommands::Template => unreachable!(),
                #[cfg(feature = "personal")]
                IntegrationCommands::McpExa => unreachable!(),
                #[cfg(feature = "personal")]
                IntegrationCommands::McpExcalidraw => unreachable!(),
                #[cfg(feature = "personal")]
                IntegrationCommands::McpPluginEcc => unreachable!(),
                #[cfg(feature = "personal")]
                IntegrationCommands::XTwitter => unreachable!(),
                #[cfg(feature = "personal")]
                IntegrationCommands::AppleAddressbook => unreachable!(),
                #[cfg(feature = "personal")]
                IntegrationCommands::Tailscale => unreachable!(),
                #[cfg(feature = "personal")]
                IntegrationCommands::Publer => unreachable!(),
                #[cfg(feature = "personal")]
                IntegrationCommands::GranolaMcp => unreachable!(),
                IntegrationCommands::CheckAll { .. } => unreachable!(),
                IntegrationCommands::Digest => unreachable!(),
                IntegrationCommands::RunCheck { .. } => unreachable!(),
            };
            let mut cmd = std::process::Command::new("bash");
            cmd.arg(&script).arg(subcmd);
            if let Some(n) = &name_arg {
                cmd.arg(n);
            }
            cmd.env("HEX_DIR", &hex_dir);
            let status = cmd.status().unwrap_or_else(|e| {
                eprintln!("hex integration: failed to run script: {e}");
                std::process::exit(1);
            });
            let exit_code = status.code().unwrap_or(1);
            std::process::exit(exit_code);
        }
        Commands::Memory { command } => {
            let hex_dir = get_hex_dir();
            let exit_code = match &command {
                MemoryCommands::Search { query, top, file, compact, context, private } => {
                    let args = memory::search::SearchArgs {
                        query: query.clone(),
                        top: *top,
                        file: file.clone(),
                        compact: *compact,
                        context: *context,
                        private: *private,
                    };
                    memory::search::run(&hex_dir, &args)
                }
                MemoryCommands::Index { full, stats } => {
                    memory::index::run(&hex_dir, *full, *stats)
                }
                MemoryCommands::ParseTranscripts { file, dry_run, force } => {
                    let args = memory::parse_transcripts::ParseArgs {
                        file: file.clone(),
                        dry_run: *dry_run,
                        force: *force,
                    };
                    memory::parse_transcripts::run(&hex_dir, &args)
                }
                MemoryCommands::Recall { query, agent } => {
                    memory::recall::run(&hex_dir, query, *agent)
                }
                MemoryCommands::Eval { rate_only } => {
                    if *rate_only { memory::eval::run_rate_only(&hex_dir) }
                    else { memory::eval::run(&hex_dir) }
                }
                MemoryCommands::LlmCheck => {
                    match memory::provider::health_check() {
                        Ok(_) => {
                            println!("provider OK");
                            0
                        }
                        Err(memory::provider::ProviderError::Deferred(msg)) => {
                            eprintln!("provider DEFERRED: {}", msg);
                            2
                        }
                        Err(memory::provider::ProviderError::Upstream(msg)) => {
                            eprintln!("provider UPSTREAM error: {}", msg);
                            3
                        }
                    }
                }
                MemoryCommands::Distill { path } => {
                    let db_path = memory::db_path(&hex_dir);
                    match memory::open_db(&db_path) {
                        Ok(mut conn) => {
                            let path_str = path.to_string_lossy().to_string();
                            match memory::distill::run_on_file(&mut conn, &path_str, 500) {
                                Ok(report) => {
                                    println!(
                                        "distill: adds={} updates={} noops={} flags={}",
                                        report.adds, report.updates, report.noops, report.flags
                                    );
                                    0
                                }
                                Err(e) => {
                                    eprintln!("distill error: {}", e);
                                    1
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("open_db error: {}", e);
                            1
                        }
                    }
                }
                MemoryCommands::Stats { json } => {
                    memory::stats::run(&hex_dir, *json)
                }
                MemoryCommands::CheckVectorSearch => {
                    check_vector_search(&hex_dir)
                }
                MemoryCommands::CheckReflectionLiveness => {
                    check_reflection_liveness(&hex_dir)
                }
                MemoryCommands::Health => {
                    // `health` is a thin alias for the native memory DB stats path.
                    memory::stats::run(&hex_dir, false)
                }
                MemoryCommands::Learnings { command } => match command {
                    LearningsCommands::Promote { dry_run } => {
                        learnings::run_promote(&hex_dir, *dry_run);
                        0
                    }
                },
                MemoryCommands::Consolidate { command } => {
                    let mode = match command {
                        ConsolidateCommands::Quick => consolidate::Mode::Quick,
                        ConsolidateCommands::Full => consolidate::Mode::Full,
                    };
                    consolidate::run(mode, &hex_dir)
                }
            };
            std::process::exit(exit_code);
        }
        Commands::Doctor { command } => {
            let hex_dir = get_hex_dir();
            match command {
                DoctorCommands::CheckCodex => {
                    doctor::check_codex(&hex_dir);
                }
                DoctorCommands::QualityCheck { spec, sweep, kr } => {
                    let code = doctor::quality_check(&hex_dir, spec.as_deref(), sweep, kr.as_deref());
                    std::process::exit(code);
                }
                DoctorCommands::Introspect => {
                    std::process::exit(doctor::introspect::run(&hex_dir));
                }
                DoctorCommands::CleanupProjects { days } => {
                    std::process::exit(doctor::cleanup_projects::run(&hex_dir, days as u64));
                }
                DoctorCommands::StaleDeps { threshold, json } => {
                    let code = doctor::stale_deps(&hex_dir, threshold, json);
                    std::process::exit(code);
                }
                DoctorCommands::DetectFailurePattern { window, spec_id } => {
                    let code = doctor::detect_failure_pattern(window, spec_id.as_deref());
                    std::process::exit(code);
                }
                DoctorCommands::Run { fix, smoke: _, quiet, json, filter } => {
                    let ctx = doctor::Context::new(hex_dir.clone(), fix);
                    let runner = match &filter {
                        Some(pat) => doctor::Runner::filtered(pat),
                        None => doctor::Runner::all_checks(),
                    };
                    let results = runner.run(&ctx);
                    if json {
                        doctor::reporter::print_json(&results);
                    } else {
                        doctor::reporter::print_text(&results, quiet);
                    }
                    let exit_code = doctor::reporter::exit_code(&results);
                    std::process::exit(exit_code);
                }
                DoctorCommands::List => {
                    doctor::Runner::all_checks().list();
                }
            }
        }
        Commands::Session { command } => match command {
            SessionCommands::Startup { quick, step, status } => {
                let hex_dir = get_hex_dir();
                let code = startup::run(
                    &hex_dir,
                    startup::StartupArgs { quick, step, status },
                );
                std::process::exit(code);
            }
            SessionCommands::Checkpoint { focus } => {
                let hex_dir = get_hex_dir();
                let code = checkpoint::run(&hex_dir, checkpoint::CheckpointArgs { focus });
                std::process::exit(code);
            }
            SessionCommands::Shutdown { session_id } => {
                let hex_dir = get_hex_dir();
                let code = shutdown::run(&hex_dir, shutdown::ShutdownArgs { session_id });
                std::process::exit(code);
            }
            SessionCommands::Reflect { session_id, quiet } => {
                session_reflect::run(session_id.as_deref(), quiet);
            }
        },
        Commands::Env { command } => env::run_env_command(command),
        Commands::Upgrade { args } => {
            std::process::exit(upgrade::run(&args));
        }
        #[cfg(feature = "personal")]
        Commands::Release { version, skip_e2e, dry_run } => {
            let hex_dir = get_hex_dir();
            let code = release::run(&hex_dir, release::ReleaseArgs { version, skip_e2e, dry_run });
            std::process::exit(code);
        }
        Commands::Hook { command } => hook::run(command),
        Commands::Version => {
            println!("hex {} ({})", env!("CARGO_PKG_VERSION"), env!("HEX_GIT_SHA"));
        }
        Commands::Completions { shell } => {
            clap_complete::generate(shell, &mut Cli::command(), "hex", &mut io::stdout());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use clap_complete::Shell;

    #[test]
    fn completions_zsh_nonempty_and_contains_hex() {
        let mut buf = Vec::new();
        clap_complete::generate(Shell::Zsh, &mut Cli::command(), "hex", &mut buf);
        assert!(!buf.is_empty(), "zsh completions must not be empty");
        let output = String::from_utf8(buf).unwrap();
        assert!(
            output.contains("_hex"),
            "zsh completions must contain '_hex', got: {}",
            &output[..200.min(output.len())]
        );
    }
}
