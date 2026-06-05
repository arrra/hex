use clap::{CommandFactory, Parser, Subcommand};
use std::io;
use std::path::PathBuf;

mod consolidate;
mod throttle;
mod doctor;
mod integration;
mod integration_cmd;
mod checkpoint;
mod shutdown;
mod startup;
mod integration_check_all;
// telemetry lives in the lib (used by the in-process worker runtime too); the
// bin shares that one copy rather than compiling a second (mirrors hex::memory).
use hex::memory;
use hex::telemetry;
mod path_map;
mod session_reflect;
mod env;
mod hook;
mod upgrade;
mod learnings;
// ops lives in the lib (the in-process worker runtime calls it too); the bin
// shares that one copy rather than compiling a second (mirrors hex::memory).
use hex::ops;
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
    /// Upgrade hex installation (native git pull + cargo build + codesign + atomic swap)
    #[command(display_order = 14)]
    Upgrade {
        /// Extra arguments forwarded to the upgrade flow (e.g. --local <path>)
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
    /// Hex harness lifecycle (single-process drain-aware host for typed Rust workers)
    #[command(display_order = 14)]
    Harness {
        #[command(subcommand)]
        command: HarnessCommands,
    },
    /// Emit hex events into the trigger substrate
    #[command(display_order = 14)]
    Triggers {
        #[command(subcommand)]
        command: TriggersCommands,
    },
    /// Telemetry store: query and emit events from the native SQLite log
    #[command(display_order = 6)]
    Telemetry {
        #[command(subcommand)]
        command: TelemetryCommands,
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
enum HarnessCommands {
    /// Install (idempotent) and load the com.hex.harness launchd service.
    Start,
    /// Bootout the com.hex.harness launchd service.
    Stop,
    /// List registered workers and report engine health.
    Status,
    /// Run the harness lifecycle loop (invoked by launchd; hidden from --help).
    #[command(hide = true)]
    Serve,
}

#[derive(Subcommand)]
enum TriggersCommands {
    /// Emit a hex event into the trigger substrate
    Emit {
        /// Event name (e.g. boi.spec.complete)
        event: String,
        /// JSON event payload (default `{}`)
        #[arg(long)]
        data: Option<String>,
        /// Producer attribution (defaults to $HEX_PRODUCER or "cli")
        #[arg(long)]
        producer: Option<String>,
    },
}

#[derive(Subcommand)]
enum TelemetryCommands {
    /// Show recent events (newest first)
    Recent {
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Show non-ok events since the given window (e.g. 24h, 7d)
    Failures {
        #[arg(long, default_value = "24h")]
        since: String,
        #[arg(long)]
        json: bool,
    },
    /// Aggregated per-event status (last run + ok/error counts)
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Append a single event from the CLI (or shell scripts)
    Record {
        #[arg(long)]
        source: String,
        #[arg(long)]
        event: String,
        #[arg(long)]
        status: String,
        #[arg(long = "duration-ms")]
        duration_ms: Option<i64>,
        #[arg(long = "exit-code")]
        exit_code: Option<i64>,
        #[arg(long)]
        detail: Option<String>,
    },
    /// Delete events older than keep-days
    Prune {
        #[arg(long = "keep-days", default_value_t = 30)]
        keep_days: i64,
    },
}

#[derive(Subcommand)]
enum ConsolidateCommands {
    /// Deterministic layers only (structural + memory DB + learnings promotion). No LLM, no network. Safe to run nightly.
    Quick {
        /// Run at full (normal) OS scheduling priority instead of background-throttled.
        #[arg(long)]
        max: bool,
    },
    /// All deterministic layers + the LLM-assisted operating-model audit.
    Full {
        /// Run at full (normal) OS scheduling priority instead of background-throttled.
        #[arg(long)]
        max: bool,
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
}

#[derive(Subcommand)]
enum MemoryCommands {
    /// Unified consolidation (structural + memory + learnings promotion + operating-model audit)
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
        /// Run at full (normal) OS scheduling priority instead of background-throttled
        #[arg(long)]
        max: bool,
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
    /// Retrieve workspace memory relevant to a query (FTS5 contextual recall).
    /// Internal: invoked by the memory-injection hook / BOI consumers, not humans.
    #[command(hide = true)]
    Recall {
        query: String,
        /// Apply the private filter (for BOI worker consumers)
        #[arg(long)]
        agent: bool,
    },
    /// Distill facts from a file into the memory facts layer.
    /// Internal: pipeline plumbing, not a human-facing command.
    #[command(hide = true)]
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
    /// Post-session reflection: update reflection-log.md and persist eval_records to memory.db.
    /// Internal: invoked by the Stop hook + checkpoint; the AI reflection is the /hex-reflect skill.
    #[command(hide = true)]
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
        Commands::Harness { command } => match command {
            HarnessCommands::Start => std::process::exit(harness_start()),
            HarnessCommands::Stop => std::process::exit(harness_stop()),
            HarnessCommands::Status => std::process::exit(harness_status()),
            HarnessCommands::Serve => {
                std::process::exit(hex::worker::runtime::serve(hex::workers::registry()))
            }
        },
        Commands::Triggers { command } => match command {
            TriggersCommands::Emit { event, data, producer } => {
                let parsed: serde_json::Value = match data.as_deref() {
                    None | Some("") => serde_json::Value::Object(Default::default()),
                    Some(s) => match serde_json::from_str(s) {
                        Ok(v) => v,
                        Err(e) => {
                            eprintln!("hex triggers emit: --data is not valid JSON: {e}");
                            std::process::exit(2);
                        }
                    },
                };
                match ops::emit(&event, parsed, producer.as_deref()) {
                    Ok(()) => std::process::exit(0),
                    Err(e) => {
                        eprintln!("{e}");
                        std::process::exit(1);
                    }
                }
            }
        },
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
                MemoryCommands::Index { full, stats, max } => {
                    // --stats is a cheap read; only throttle the heavy index path.
                    if !*stats {
                        throttle::apply("memory index", *max);
                    }
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
                MemoryCommands::Consolidate { command } => {
                    let (mode, max) = match command {
                        ConsolidateCommands::Quick { max } => (consolidate::Mode::Quick, *max),
                        ConsolidateCommands::Full { max } => (consolidate::Mode::Full, *max),
                    };
                    consolidate::run(mode, max, &hex_dir)
                }
            };
            std::process::exit(exit_code);
        }
        Commands::Doctor { command } => {
            let hex_dir = get_hex_dir();
            match command {
                DoctorCommands::StaleDeps { threshold, json } => {
                    let code = doctor::stale_deps(&hex_dir, threshold, json);
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
        Commands::Telemetry { command } => {
            std::process::exit(run_telemetry(command));
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

/// Resolve the current numeric UID (shells out to `id -u` — avoids a libc dep).
fn current_uid() -> String {
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout)
                    .ok()
                    .map(|s| s.trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "501".to_string())
}

/// Resolve the path to the installed `com.hex.harness` launchd plist under
/// `~/Library/LaunchAgents/`.
fn harness_plist_path() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(
        PathBuf::from(home)
            .join("Library")
            .join("LaunchAgents")
            .join("com.hex.harness.plist"),
    )
}

/// Render the harness.plist template by substituting placeholders.
fn render_harness_plist(hex_dir: &std::path::Path) -> Result<String, String> {
    let template_path = hex_dir
        .join("system")
        .join("templates")
        .join("launchd")
        .join("harness.plist");
    let template = std::fs::read_to_string(&template_path).map_err(|e| {
        format!(
            "hex harness: failed to read template {}: {e}",
            template_path.display()
        )
    })?;
    let hex_bin = hex_dir.join(".hex").join("bin").join("hex");
    let log_path = hex_dir
        .join(".hex")
        .join("logs")
        .join("com.hex.harness.log");
    let path_env = std::env::var("PATH")
        .unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".to_string());
    Ok(template
        .replace("HEXBIN_PLACEHOLDER", &hex_bin.to_string_lossy())
        .replace("HEXDIR_PLACEHOLDER", &hex_dir.to_string_lossy())
        .replace("LOG_PLACEHOLDER", &log_path.to_string_lossy())
        .replace("PATH_PLACEHOLDER", &path_env))
}

/// `hex harness start` — install (idempotent) and load the launchd service.
fn harness_start() -> i32 {
    let hex_dir = get_hex_dir();
    let plist_path = match harness_plist_path() {
        Some(p) => p,
        None => {
            eprintln!("hex harness start: $HOME is not set");
            return 1;
        }
    };
    let rendered = match render_harness_plist(&hex_dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    if let Some(parent) = plist_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!(
                "hex harness start: failed to create {}: {e}",
                parent.display()
            );
            return 1;
        }
    }
    // Idempotent: only rewrite the plist if its contents differ.
    let current = std::fs::read_to_string(&plist_path).ok();
    if current.as_deref() != Some(rendered.as_str()) {
        if let Err(e) = std::fs::write(&plist_path, &rendered) {
            eprintln!(
                "hex harness start: failed to write {}: {e}",
                plist_path.display()
            );
            return 1;
        }
        eprintln!("hex harness start: wrote {}", plist_path.display());
    } else {
        eprintln!(
            "hex harness start: {} already current",
            plist_path.display()
        );
    }
    // Ensure log dir exists (launchd will not create it).
    if let Some(parent) = hex_dir.join(".hex").join("logs").parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::create_dir_all(hex_dir.join(".hex").join("logs"));

    let uid = current_uid();
    let domain = format!("gui/{uid}");
    // `bootstrap` is idempotent only if the service is not already loaded;
    // ignore its exit code and follow with `kickstart -k` to (re)start.
    let _ = std::process::Command::new("launchctl")
        .args(["bootstrap", &domain, &plist_path.to_string_lossy()])
        .status();
    let status = std::process::Command::new("launchctl")
        .args([
            "kickstart",
            "-k",
            &format!("{domain}/com.hex.harness"),
        ])
        .status();
    match status {
        Ok(s) if s.success() => {
            eprintln!("hex harness start: com.hex.harness loaded");
            0
        }
        Ok(s) => {
            eprintln!(
                "hex harness start: launchctl kickstart exited {}",
                s.code().unwrap_or(-1)
            );
            1
        }
        Err(e) => {
            eprintln!("hex harness start: failed to spawn launchctl: {e}");
            1
        }
    }
}

/// `hex harness stop` — bootout the launchd service.
fn harness_stop() -> i32 {
    let plist_path = match harness_plist_path() {
        Some(p) => p,
        None => {
            eprintln!("hex harness stop: $HOME is not set");
            return 1;
        }
    };
    let uid = current_uid();
    let domain = format!("gui/{uid}");
    let status = std::process::Command::new("launchctl")
        .args(["bootout", &domain, &plist_path.to_string_lossy()])
        .status();
    match status {
        Ok(s) if s.success() => {
            eprintln!("hex harness stop: com.hex.harness booted out");
            0
        }
        Ok(s) => {
            eprintln!(
                "hex harness stop: launchctl bootout exited {}",
                s.code().unwrap_or(-1)
            );
            // Already-stopped is not a hard failure.
            0
        }
        Err(e) => {
            eprintln!("hex harness stop: failed to spawn launchctl: {e}");
            1
        }
    }
}

/// `hex harness status` — print registered workers + engine health.
fn harness_status() -> i32 {
    let workers = hex::workers::registry();
    println!("Registered workers ({}):", workers.len());
    for w in &workers {
        println!("  - {} ({} handler(s))", w.name, w.handlers.len());
    }
    let ctx = doctor::check::Context {
        hex_dir: get_hex_dir(),
        home: PathBuf::from(std::env::var("HOME").unwrap_or_default()),
        fix: false,
    };
    use doctor::check::DoctorCheck;
    let result = doctor::checks::iii_engine_health::IiiEngineHealth.run(&ctx);
    println!("iii engine: {:?} — {}", result.status, result.message);
    match result.status {
        doctor::check::Status::Pass | doctor::check::Status::Skip => 0,
        _ => 1,
    }
}

fn parse_since(s: &str) -> Result<chrono::Duration, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty --since value".into());
    }
    let (num_str, unit) = match s.chars().last().unwrap() {
        'h' | 'd' => (&s[..s.len() - 1], s.chars().last().unwrap()),
        _ => (s, 'h'),
    };
    let n: i64 = num_str
        .parse()
        .map_err(|e| format!("invalid --since number `{num_str}`: {e}"))?;
    Ok(match unit {
        'd' => chrono::Duration::days(n),
        _ => chrono::Duration::hours(n),
    })
}

fn print_event_table(rows: &[telemetry::EventRow]) {
    println!(
        "{:<25} {:<16} {:<32} {:<8} {:>8}",
        "TS", "SOURCE", "EVENT", "STATUS", "DUR_MS"
    );
    for r in rows {
        let dur = r
            .duration_ms
            .map(|d| d.to_string())
            .unwrap_or_else(|| "-".into());
        println!(
            "{:<25} {:<16} {:<32} {:<8} {:>8}",
            r.ts, r.source, r.event, r.status, dur
        );
    }
}

fn print_event_json(rows: &[telemetry::EventRow]) {
    let items: Vec<_> = rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "id": r.id,
                "ts": r.ts,
                "source": r.source,
                "event": r.event,
                "status": r.status,
                "duration_ms": r.duration_ms,
                "exit_code": r.exit_code,
                "detail": r.detail,
            })
        })
        .collect();
    println!("{}", serde_json::to_string_pretty(&items).unwrap());
}

fn run_telemetry(command: TelemetryCommands) -> i32 {
    match command {
        TelemetryCommands::Recent { limit, json } => match telemetry::recent(limit) {
            Ok(rows) => {
                if json {
                    print_event_json(&rows);
                } else {
                    print_event_table(&rows);
                }
                0
            }
            Err(e) => {
                eprintln!("telemetry recent: {e}");
                1
            }
        },
        TelemetryCommands::Failures { since, json } => {
            let dur = match parse_since(&since) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("telemetry failures: {e}");
                    return 2;
                }
            };
            let cutoff = chrono::Utc::now() - dur;
            match telemetry::failures(cutoff) {
                Ok(rows) => {
                    if json {
                        print_event_json(&rows);
                    } else {
                        print_event_table(&rows);
                    }
                    0
                }
                Err(e) => {
                    eprintln!("telemetry failures: {e}");
                    1
                }
            }
        }
        TelemetryCommands::Status { json } => match telemetry::status() {
            Ok(rows) => {
                if json {
                    let items: Vec<_> = rows
                        .iter()
                        .map(|r| {
                            serde_json::json!({
                                "event": r.event,
                                "last_ts": r.last_ts,
                                "last_status": r.last_status,
                                "last_duration_ms": r.last_duration_ms,
                                "run_count": r.run_count,
                                "ok_count": r.ok_count,
                                "error_count": r.error_count,
                            })
                        })
                        .collect();
                    println!("{}", serde_json::to_string_pretty(&items).unwrap());
                } else {
                    println!(
                        "{:<32} {:<25} {:<8} {:>5} {:>5} {:>5} {:>8}",
                        "EVENT", "LAST_TS", "LAST", "RUNS", "OK", "ERR", "LAST_MS"
                    );
                    for r in &rows {
                        let dur = r
                            .last_duration_ms
                            .map(|d| d.to_string())
                            .unwrap_or_else(|| "-".into());
                        println!(
                            "{:<32} {:<25} {:<8} {:>5} {:>5} {:>5} {:>8}",
                            r.event,
                            r.last_ts,
                            r.last_status,
                            r.run_count,
                            r.ok_count,
                            r.error_count,
                            dur
                        );
                    }
                }
                0
            }
            Err(e) => {
                eprintln!("telemetry status: {e}");
                1
            }
        },
        TelemetryCommands::Record {
            source,
            event,
            status,
            duration_ms,
            exit_code,
            detail,
        } => {
            let ev = telemetry::TelemetryEvent {
                source,
                event,
                status,
                duration_ms,
                exit_code,
                detail,
            };
            match telemetry::record(&ev) {
                Ok(()) => {
                    println!("recorded");
                    0
                }
                Err(e) => {
                    eprintln!("telemetry record: failed: {e}");
                    1
                }
            }
        }
        TelemetryCommands::Prune { keep_days } => match telemetry::prune(keep_days) {
            Ok(n) => {
                println!("pruned {} events (kept last {}d)", n, keep_days);
                0
            }
            Err(e) => {
                eprintln!("telemetry prune: {e}");
                1
            }
        },
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
