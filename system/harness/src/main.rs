use clap::{CommandFactory, Parser, Subcommand};
use std::io;
use std::path::PathBuf;

mod consolidate;
mod throttle;
mod doctor;
mod integration;
mod integration_cmd;
mod integration_check_all;
// telemetry lives in the lib (used by the in-process worker runtime too); the
// bin shares that one copy rather than compiling a second (mirrors hex::memory).
use hex::memory;
use hex::telemetry;
mod path_map;
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
    /// Questions & replies: ask a structured question, reply to one by id.
    #[command(display_order = 4)]
    Messages {
        #[command(subcommand)]
        command: MessagesCommands,
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
enum MessagesCommands {
    /// Submit a message; prints the Result (and any question's options + ids).
    Submit { text: String },
    /// Reply to a question by id. Selection = `b` or `a,c` (option ids); plus optional --text.
    Reply {
        question_id: String,
        #[arg(default_value = "")]
        selection: String,
        #[arg(long)]
        text: Option<String>,
    },
}

#[derive(Subcommand)]
enum HarnessCommands {
    /// Install (idempotent) and load the com.hex.harness service via daemon-green.
    Start,
    /// Stop + unload the com.hex.harness service via daemon-green.
    Stop,
    /// Restart the com.hex.harness service (pick up a new binary) via daemon-green.
    Restart,
    /// List registered workers and report engine health.
    Status,
    /// Tail the last N lines of the harness service log via daemon-green.
    Logs {
        /// Number of trailing lines to print.
        #[arg(long, default_value_t = 200)]
        lines: usize,
    },
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
    #[command(name = "parse-transcripts", hide = true)]
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
    /// Print ~10 recency-ordered pointers into the live workspace (project dirs,
    /// recent decisions, todo "Now" items). No LLM, target <200ms.
    Recent,
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
            HarnessCommands::Restart => std::process::exit(harness_restart()),
            HarnessCommands::Status => std::process::exit(harness_status()),
            HarnessCommands::Logs { lines } => std::process::exit(harness_logs(lines)),
            HarnessCommands::Serve => {
                // Bootstrap secrets before the worker runtime starts (before any
                // thread is spawned). Reads $HEX_DIR/.hex/secrets/*.env and injects
                // into the process env — no secrets appear in the plist.
                if let Ok(hex_dir) = std::env::var("HEX_DIR") {
                    bootstrap_secrets_env(std::path::Path::new(&hex_dir));
                }
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
                MemoryCommands::Recent => {
                    memory::recent::run(&hex_dir)
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
        Commands::Messages { command } => {
            std::process::exit(run_messages(command));
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

/// The reverse-DNS label of the harness service. Single source of truth — all
/// daemon-green calls go through this constant so a rename only touches one
/// line.
const HARNESS_LABEL: &str = "com.hex.harness";

/// Build the platform-neutral `ServiceSpec` that daemon-green renders into the
/// per-user launchd plist (macOS) or systemd --user unit (Linux). Reproduces
/// the exact behavior of the old `render_harness_plist` template:
///   - program           = $HEX_DIR/.hex/bin/hex
///   - args              = ["harness", "serve"]
///   - working_dir       = $HEX_DIR
///   - env               = HEX_DIR, III_URL, PATH (homebrew prepended),
///                         GOOGLE_WORKSPACE_CLI_KEYRING_BACKEND=file
///   - keep_alive        = true (restart on crash)
///   - run_at_load       = true (start at login)
///   - log_path          = $HEX_DIR/.hex/logs/com.hex.harness.log
/// daemon-green guarantees the rendered plist omits the launchd login-session
/// detach key (verified 2026-06-05: when present, keychain reads fail rc=36;
/// when absent, rc=0). We deliberately do NOT — and CANNOT — set it here.
fn build_harness_spec(hex_dir: &std::path::Path) -> daemon_green::ServiceSpec {
    let hex_bin = hex_dir.join(".hex").join("bin").join("hex");
    let log_path = hex_dir
        .join(".hex")
        .join("logs")
        .join("com.hex.harness.log");
    // launchd hands the agent a minimal env — guarantee homebrew is on PATH so
    // the folded-in workers (gws, cargo, etc.) resolve.
    let base_path =
        std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_string());
    let path_env = if base_path.split(':').any(|p| p == "/opt/homebrew/bin") {
        base_path
    } else {
        format!("/opt/homebrew/bin:{base_path}")
    };
    let spec = daemon_green::ServiceSpec::new(HARNESS_LABEL, hex_bin)
        .args(["harness", "serve"])
        .env("HEX_DIR", hex_dir.to_string_lossy().into_owned())
        .env("III_URL", "ws://127.0.0.1:49134")
        .env("PATH", path_env)
        .env("GOOGLE_WORKSPACE_CLI_KEYRING_BACKEND", "file")
        .working_dir(hex_dir)
        .keep_alive(true)
        .run_at_load(true)
        .log_path(log_path);
    // Secrets are NOT baked into the plist. The harness reads
    // $HEX_DIR/.hex/secrets/*.env at serve startup via bootstrap_secrets_env()
    // (called in main() before the worker runtime starts). The plist carries
    // only non-secret config: HEX_DIR, PATH, III_URL, log path.
    spec
}

/// Load every `*.env` file from `$HEX_DIR/.hex/secrets/` into the process
/// environment. Called at `hex harness serve` startup, before any thread is
/// spawned. Follows symlinks (metadata()) so symlinked secrets files work.
///
/// # Safety
/// Must be called before any thread is spawned. std::env::set_var is unsound
/// in a multi-threaded context.
#[allow(unsafe_code)]
fn bootstrap_secrets_env(hex_dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let dir = hex_dir.join(".hex").join("secrets");
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("env") {
            continue;
        }
        // metadata() follows symlinks — symlinked secrets files are intentional.
        let mode = path
            .metadata()
            .map(|m| m.permissions().mode() & 0o077)
            .unwrap_or(0o077);
        if mode != 0 {
            eprintln!(
                "hex harness: skipping {} — unsafe permissions (run: chmod 600 {})",
                path.display(),
                path.display()
            );
            continue;
        }
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("hex harness: skipping {}: {e}", path.display());
                continue;
            }
        };
        for raw in content.lines() {
            let line = raw.trim().strip_prefix("export").unwrap_or(raw.trim()).trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                let k = k.trim();
                let v = v.trim().trim_matches('\'').trim_matches('"');
                if !k.is_empty() {
                    // SAFETY: called before worker runtime spawns threads.
                    unsafe { std::env::set_var(k, v) };
                    eprintln!("hex harness: loaded {k} from {}", path.display());
                }
            }
        }
    }
}

/// `hex harness start` — install (idempotent) and load the per-user service.
///
/// On macOS this is a gui-domain LaunchAgent (NOT a system daemon) because the
/// harness spawns `claude` for per-task reasoning, and Claude Code auth lives
/// in the LOGIN keychain — reachable only from a login session. On Linux it is
/// a `systemd --user` unit. daemon-green owns the launchctl plumbing
/// (bootstrap/kickstart, asuser fallback, wait-out-bootout retry).
fn harness_start() -> i32 {
    let hex_dir = get_hex_dir();
    // launchd / systemd won't create the log dir for us.
    let _ = std::fs::create_dir_all(hex_dir.join(".hex").join("logs"));
    let spec = build_harness_spec(&hex_dir);
    let mgr = daemon_green::native();
    if let Err(e) = mgr.install(&spec) {
        eprintln!("hex harness start: install failed: {e}");
        return 1;
    }
    match mgr.start(HARNESS_LABEL) {
        Ok(()) => {
            eprintln!("hex harness start: {HARNESS_LABEL} loaded");
            0
        }
        Err(e) => {
            eprintln!("hex harness start: start failed: {e}");
            1
        }
    }
}

/// `hex harness stop` — stop + unload the per-user service via daemon-green.
fn harness_stop() -> i32 {
    let mgr = daemon_green::native();
    match mgr.stop(HARNESS_LABEL) {
        Ok(()) => {
            eprintln!("hex harness stop: {HARNESS_LABEL} stopped");
            0
        }
        Err(e) => {
            eprintln!("hex harness stop: {e}");
            1
        }
    }
}

/// `hex harness restart` — restart the per-user service (e.g. to pick up a new
/// binary) via daemon-green.
fn harness_restart() -> i32 {
    let mgr = daemon_green::native();
    match mgr.restart(HARNESS_LABEL) {
        Ok(()) => {
            eprintln!("hex harness restart: {HARNESS_LABEL} restarted");
            0
        }
        Err(e) => {
            eprintln!("hex harness restart: {e}");
            1
        }
    }
}

/// `hex harness logs` — tail the last N lines of the service's combined log.
fn harness_logs(lines: usize) -> i32 {
    let mgr = daemon_green::native();
    match mgr.logs(HARNESS_LABEL, lines) {
        Ok(s) => {
            print!("{s}");
            if !s.ends_with('\n') {
                println!();
            }
            0
        }
        Err(e) => {
            eprintln!("hex harness logs: {e}");
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

fn run_messages(command: MessagesCommands) -> i32 {
    let hex_dir = get_hex_dir();
    let db = hex::memory::db_path(&hex_dir);
    let conn = match hex::memory::open_db(&db) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("hex messages: cannot open {}: {e}", db.display());
            return 1;
        }
    };
    let event = match &command {
        MessagesCommands::Submit { text } => hex::messages::build_submit_event(text),
        MessagesCommands::Reply {
            question_id,
            selection,
            text,
        } => {
            let ids: Vec<&str> = selection
                .split(',')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect();
            hex::messages::build_reply_event(question_id, &ids, text.clone())
        }
    };
    match hex::harness::submit(&conn, &event, hex::worker::run::run_worker) {
        Ok(r) => {
            if let Some(p) = &r.prompt {
                println!("hex asks (question {}): {}", p.id, p.text);
                for o in &p.options {
                    println!("  [{}] {} — {}", o.id, o.label, o.description);
                }
                println!("(reply: hex messages reply {} <id[,id]> [--text ...])", p.id);
            } else {
                println!("{}", r.output);
            }
            0
        }
        Err(e) => {
            eprintln!("hex messages: {e}");
            1
        }
    }
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
