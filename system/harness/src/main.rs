use clap::{CommandFactory, Parser, Subcommand};
use std::io;
use std::path::{Path, PathBuf};

use hex::{state, wake};

mod alert;
mod boi_pm;
mod synthesis;
mod boi_web;
mod capture;
mod router;
mod spec_tool;
mod charter_triggers;
mod doctor;
mod fleet;
mod health;
mod integration;
mod integration_cmd;
mod integration_apple_addressbook;
mod metrics;
mod checkpoint;
mod shutdown;
mod startup;
mod validate;
mod integration_tailscale;
mod integration_mcp_exa;
mod integration_mcp_excalidraw;
mod integration_mcp_plugin_ecc;
mod integration_x_twitter;
mod integration_publer;
mod integration_granola_mcp;
mod integration_check_all;
mod integration_telemetry;
mod kalshi;
mod mcp;
mod memory;
mod mirofish;
mod path_map;
mod pulse;
mod session_reflect;
mod today;
mod workspace;
mod env;
mod agent_evolution;
mod agent_spawn;
mod hook;
mod upgrade;
mod initiative;
mod learnings;
use hex::route;

#[derive(Parser)]
#[command(name = "hex", about = "Hex multi-agent harness", version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Agent fleet management
    Agent {
        #[command(subcommand)]
        command: AgentCommands,
    },
    /// HTTP/SSE server
    Server {
        #[command(subcommand)]
        command: ServerCommands,
    },
    /// Asset registry
    Asset {
        #[command(subcommand)]
        command: AssetCommands,
    },
    /// Unified messaging (comments, agent messages, notifications)
    Message {
        #[command(subcommand)]
        command: MessageCommands,
    },
    /// Event engine
    Events {
        #[command(subcommand)]
        command: EventsCommands,
    },
    /// SSE bus operations
    Sse {
        #[command(subcommand)]
        command: SseCommands,
    },
    /// Integration bundle lifecycle management
    Integration {
        #[command(subcommand)]
        command: IntegrationCommands,
    },
    /// Behavioral and indexed memory operations
    Memory {
        #[command(subcommand)]
        command: MemoryCommands,
    },
    /// Extension management
    Extension {
        #[command(subcommand)]
        command: ExtensionCommands,
    },
    /// User-outcome metrics (port of .hex/scripts/metrics/run-all.sh)
    Metrics {
        #[command(subcommand)]
        command: MetricsCommands,
    },
    /// Agent health checks (port of .hex/scripts/health/)
    Health {
        #[command(subcommand)]
        command: HealthCommands,
    },
    /// System health checks
    Doctor {
        #[command(subcommand)]
        command: DoctorCommands,
    },
    /// Translate paths between v1 and v2 hex layouts (port of .hex/scripts/path-mapping.sh)
    #[command(name = "path-map")]
    PathMap {
        #[command(subcommand)]
        command: PathMapCommands,
    },
    /// Mirofish VM and service health (port of .hex/scripts/mirofish-status.sh)
    Mirofish {
        #[command(subcommand)]
        command: MirofishCommands,
    },
    /// Kalshi prediction market integration
    Kalshi {
        #[command(subcommand)]
        command: KalshiCommands,
    },
    /// Pulse server lifecycle (port of .hex/scripts/pulse/start.sh)
    Pulse {
        #[command(subcommand)]
        command: PulseCommands,
    },
    /// Session lifecycle commands
    Session {
        #[command(subcommand)]
        command: SessionCommands,
    },
    /// Print today's date (port of .hex/scripts/today.sh)
    Today {
        /// Optional date format, e.g. +%a (passed to strftime; mirrors shell's $1)
        format: Option<String>,
    },
    /// MCP utilities
    Mcp {
        #[command(subcommand)]
        command: McpCommands,
    },
    /// Hex tmux workspace launcher (port of .hex/scripts/workspace.sh)
    Workspace {
        #[command(subcommand)]
        command: WorkspaceCommands,
    },
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
    /// Hex Fleet Manager service management (port of .hex/scripts/hex-fleet/install.sh)
    Fleet {
        #[command(subcommand)]
        command: FleetCommands,
    },
    /// BOI Process Manager service management (port of .hex/scripts/boi-pm/install.sh)
    #[command(name = "boi-pm")]
    BoiPm {
        #[command(subcommand)]
        command: BoiPmCommands,
    },
    /// BOI live status web view launcher (port of .hex/scripts/boi-web/serve.sh)
    #[command(name = "boi-web")]
    BoiWeb {
        #[command(subcommand)]
        command: BoiWebCommands,
    },
    /// Spec-tool server launcher (port of .hex/scripts/spec-tool/run.sh)
    #[command(name = "spec-tool")]
    SpecTool {
        #[command(subcommand)]
        command: SpecToolCommands,
    },
    /// Hex-router reverse proxy launcher (port of .hex/scripts/hex-router/serve.sh)
    Router {
        #[command(subcommand)]
        command: RouterCommands,
    },
    /// Zero-friction context capture (port of .hex/scripts/capture.sh)
    Capture {
        #[command(subcommand)]
        command: CaptureCommands,
    },
    /// iMessage alert sender (port of .hex/scripts/hex-alert.sh)
    Alert {
        #[command(subcommand)]
        command: AlertCommands,
    },
    /// Weekly and on-demand synthesis pipeline (port of system/scripts/weekly-synthesis-digest.sh, synthesis-trigger.sh)
    Synthesis {
        #[command(subcommand)]
        command: SynthesisCommands,
    },
    /// Environment setup utilities (Phase 5: port of env.sh non-shell logic)
    Env {
        #[command(subcommand)]
        command: env::EnvCommands,
    },
    /// Message routing: classify messages and route comments to agent charters
    Route {
        #[command(subcommand)]
        command: RouteCommands,
    },
    /// Validate BOI specs, hex extensions, and E2E test guards
    Validate {
        #[command(subcommand)]
        command: ValidateCommands,
    },
    /// Initiative CRUD (port of system/scripts/hex-initiative.py)
    Initiative {
        #[command(subcommand)]
        command: InitiativeCommands,
    },
    /// Learnings analysis and promotion (port of system/scripts/promote-learnings.py)
    Learnings {
        #[command(subcommand)]
        command: LearningsCommands,
    },
    /// Telemetry file rotation and management (port of rotate-telemetry.sh)
    Telemetry {
        #[command(subcommand)]
        command: TelemetryCommands,
    },
    /// Interactive workspace picker (port of hex-picker.sh)
    Picker,
    /// Upgrade hex installation (port of system/scripts/upgrade.sh)
    Upgrade {
        /// Extra arguments forwarded to upgrade.sh
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Claude Code hook runners (port of .hex/hooks/scripts/*.sh)
    Hook {
        #[command(subcommand)]
        command: hook::HookCommands,
    },
    /// Print version
    Version,
    /// Generate shell completions
    Completions {
        shell: clap_complete::Shell,
    },
}

#[derive(Subcommand)]
enum AgentCommands {
    /// Run an agent wake cycle (shift)
    Wake {
        agent_id: String,
        #[arg(long, default_value = "manual")]
        trigger: String,
        #[arg(long, default_value = "{}")]
        payload: String,
    },
    /// Show agent status
    Status { agent_id: Option<String> },
    /// Show fleet overview
    Fleet,
    /// Send async message to another agent
    Message {
        from: String,
        to: String,
        #[arg(long)]
        subject: String,
        #[arg(long)]
        body: String,
        #[arg(long)]
        initiative: Option<String>,
        #[arg(long)]
        response_requested: bool,
    },
    /// List agent IDs (one per line, machine-readable)
    List {
        #[arg(long)]
        core: bool,
    },
    /// Check core agents against reference set
    CheckCore,
    /// Restore missing core agents from reference (never overwrites existing)
    RestoreCore,
    /// Wake the boi-optimizer agent (port of .hex/scripts/boi-optimizer-wake.sh)
    #[command(name = "optimizer-wake")]
    OptimizerWake {
        #[arg(default_value = "timer.tick.6h")]
        trigger: String,
        #[arg(default_value = "{}")]
        payload: String,
    },
    /// Run daily agent performance analysis and evolution proposals (port of agent-evolution.sh)
    Evolution {
        #[arg(long)]
        dry_run: bool,
    },
    /// Spawn a new hex agent from a role-spec YAML file (port of hex-agent-spawn.sh)
    Spawn {
        /// Path to role-spec YAML file
        spec_file: std::path::PathBuf,
        /// Validate spec but don't write any files
        #[arg(long)]
        dry_run: bool,
    },
    /// Query audit trail
    Audit {
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        action: Option<String>,
        #[arg(long)]
        since: Option<String>,
    },
    /// Show cost data
    Cost {
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        period: Option<String>,
    },
    /// Reset stale agent budget periods (port of health/reset-periods.py)
    #[command(name = "reset-periods")]
    ResetPeriods {
        /// Report what would happen without writing any state
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum FleetCommands {
    /// Install and register the Hex Fleet Manager LaunchAgent (port of hex-fleet/install.sh)
    Install,
}

#[derive(Subcommand)]
enum BoiPmCommands {
    /// Install and register the BOI Process Manager LaunchAgent (port of boi-pm/install.sh)
    Install,
    /// Verify a BOI spec's completion claims (port of boi-completion-verify.sh)
    Verify {
        /// Spec ID to verify
        spec_id: String,
    },
    /// Archive a completed BOI spec as JSON (port of boi-completion-to-archive.sh)
    Archive {
        /// Spec ID to archive
        spec_id: String,
        /// Target repo path (optional)
        #[arg(long)]
        target_repo: Option<String>,
    },
    /// Auto-commit BOI spec output to target repo (port of auto-commit-boi-output.sh)
    #[command(name = "auto-commit")]
    AutoCommit {
        /// Spec ID
        spec_id: String,
        /// Target repo path (optional)
        #[arg(long)]
        target_repo: Option<String>,
        /// Manifest path (optional)
        #[arg(long)]
        manifest: Option<String>,
    },
}

#[derive(Subcommand)]
enum BoiWebCommands {
    /// Launch the BOI live status web server (port of boi-web/serve.sh)
    Serve,
}

#[derive(Subcommand)]
enum SpecToolCommands {
    /// Launch the spec-tool server.py (port of spec-tool/run.sh)
    Run,
    /// Verify concrete claims in a BOI spec against the codebase (port of verify-spec-claims.py)
    #[command(name = "verify-claims")]
    VerifyClaims {
        /// Spec file path to verify
        spec_file: String,
        /// Workspace directory to scan
        #[arg(long)]
        workspace: Option<String>,
        /// Verbose output
        #[arg(long)]
        verbose: bool,
    },
    /// Resolve a spec's owning agent (port of spec-owner-resolver.py)
    #[command(name = "resolve-owner")]
    ResolveOwner {
        /// Spec ID or spec YAML path
        spec: String,
    },
    /// Build a structured failure brief for a failed BOI spec (port of build-failure-brief.py)
    #[command(name = "failure-brief")]
    FailureBrief {
        /// Spec ID
        spec_id: String,
    },
    /// Verify that work traces to active initiatives (port of check-cohesion.py)
    #[command(name = "check-cohesion")]
    CheckCohesion {
        /// Check a specific spec file
        #[arg(long)]
        spec: Option<String>,
        /// Check all active specs
        #[arg(long)]
        all: bool,
        /// Show initiative coverage map
        #[arg(long)]
        map: bool,
    },
}

#[derive(Subcommand)]
enum RouterCommands {
    /// Launch the hex-router reverse proxy (port of hex-router/serve.sh)
    Serve,
    /// Route a comment to matching agents via charter classification (port of route-comment.py)
    #[command(name = "route-comment")]
    RouteComment {
        /// Comment ID
        comment_id: String,
        /// Asset identifier
        asset: String,
        /// Comment text
        text: String,
    },
}

#[derive(Subcommand)]
enum RouteCommands {
    /// Route a message against agent charters via LLM (port of route-message-llm.py)
    Message {
        /// Message text to classify
        message: Vec<String>,
        /// Confidence threshold (default 0.4)
        #[arg(long, default_value = "0.4")]
        threshold: f64,
        /// Return all agents regardless of threshold
        #[arg(long)]
        all: bool,
        /// LLM provider: openrouter (default) | ollama
        #[arg(long, default_value = "openrouter")]
        provider: String,
    },
    /// Route a comment to matching agents.
    Comment {
        /// Comment ID
        comment_id: String,
        /// Asset identifier
        asset: String,
        /// Comment text
        text: Vec<String>,
    },
    /// Detect routing context via heuristic fingerprint (port of context_router/)
    #[command(name = "detect-context")]
    DetectContext {
        /// Message text to classify
        message: Vec<String>,
    },
}

#[derive(Subcommand)]
enum ValidateCommands {
    /// Validate a BOI spec file for known anti-patterns (port of validate-boi-spec.py)
    #[command(name = "boi-spec")]
    BoiSpec {
        /// One or more spec files to validate
        files: Vec<String>,
    },
    /// Validate a hex extension manifest (port of extension-validate.py)
    Extension {
        /// Path to extension directory or extension.yaml
        path: String,
    },
    /// HTTP-level E2E guard: verify a deployed URL is reachable and healthy (port of e2e-guard/verify.py)
    E2e {
        /// Base URL to test
        url: String,
        /// API health endpoint path (e.g. /api/health)
        #[arg(long, default_value = "")]
        check_api: String,
        /// SSE event stream path (e.g. /events)
        #[arg(long, default_value = "")]
        check_sse: String,
        /// Request timeout in seconds
        #[arg(long, default_value = "30")]
        timeout: u64,
    },
}

#[derive(Subcommand)]
enum InitiativeCommands {
    /// List initiatives, optionally filtered by status
    List {
        /// Filter by status: open, closed, or all (default: all)
        #[arg(long, default_value = "all")]
        status: String,
    },
    /// Show details for a specific initiative
    Show {
        /// Initiative ID
        id: String,
    },
    /// Create a new initiative
    Create {
        /// Initiative name (used to derive the ID)
        name: String,
        /// Initial status (default: open)
        #[arg(long, default_value = "open")]
        status: String,
    },
    /// Update an initiative's status
    Update {
        /// Initiative ID
        id: String,
        /// New status value
        #[arg(long)]
        status: String,
    },
    /// Close an initiative (set status to closed)
    Close {
        /// Initiative ID
        id: String,
    },
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
enum SynthesisCommands {
    /// Summarize the week's input compounding pipeline output (port of weekly-synthesis-digest.sh)
    Weekly {
        /// Run regardless of day-of-week
        #[arg(long)]
        force: bool,
        /// Print to stdout, don't write file
        #[arg(long)]
        dry_run: bool,
    },
    /// Cluster related inputs and dispatch synthesis BOI specs (port of synthesis-trigger.sh)
    Trigger {
        /// Show what would be dispatched without dispatching
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum ServerCommands {
    /// Start the HTTP/SSE server
    Start {
        #[arg(long, default_value = "8880")]
        port: u16,
    },
    /// Check if the server is running
    Health,
}

#[derive(Subcommand)]
enum AssetCommands {
    /// Resolve asset by type:local_id
    Resolve { id: String },
    /// List assets
    List {
        #[arg(long)]
        r#type: Option<String>,
    },
    /// Search assets
    Search { query: String },
    /// Register an asset
    Register {
        #[arg(long)]
        r#type: String,
        #[arg(long)]
        id: String,
        #[arg(long)]
        title: String,
        #[arg(long)]
        path: Option<String>,
    },
    /// List asset types with counts
    Types,
    /// Auto-discover hex assets and register them (port of hex-asset-discover.py)
    Discover {
        /// Report without writing
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum MessageCommands {
    /// Send a message
    Send {
        from: String,
        to: Vec<String>,
        #[arg(long)]
        content: String,
        #[arg(long, default_value = "agent")]
        msg_type: String,
        #[arg(long)]
        anchor: Option<String>,
    },
    /// List messages
    List {
        #[arg(long)]
        msg_type: Option<String>,
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        anchor: Option<String>,
    },
    /// Update message status / action log
    Respond {
        id: String,
        status: String,
        action: Option<String>,
        #[arg(long)]
        assets: Vec<String>,
    },
}

#[derive(Subcommand)]
enum EventsCommands {
    /// Show event engine status
    Status,
    /// Emit an event
    Emit {
        event_type: String,
        payload: String,
        /// Source tag for the emitted event (e.g., "hex:checkpoint")
        #[arg(long, default_value = "cli")]
        source: String,
    },
    /// Show full action chain for an event
    Trace {
        event_id: i64,
    },
    /// List loaded policies
    Policies,
    /// Force policy reload
    Reload,
    /// Run the event daemon (long-running; processes events, fires actions, logs heartbeats)
    Daemon {
        /// Shadow mode: process events and log intended actions, but do NOT execute them
        #[arg(long, default_value_t = false)]
        shadow: bool,
    },
}

#[derive(Subcommand)]
enum SseCommands {
    /// Publish an SSE event
    Publish {
        topic: String,
        r#type: String,
        payload: String,
    },
    /// List registered SSE topics
    Topics,
    /// Bridge a hex-event to the SSE bus (port of .hex/scripts/sse-bus/bridge.py)
    Bridge {
        hex_event_name: String,
        payload: String,
    },
}

#[derive(Subcommand)]
enum AlertCommands {
    /// Send an iMessage alert (port of .hex/scripts/hex-alert.sh)
    Send {
        severity: String,
        agent_id: String,
        message: String,
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
    #[command(name = "mcp-exa")]
    McpExa,
    /// Run Excalidraw MCP health probe (port of integrations/mcp-excalidraw.sh)
    #[command(name = "mcp-excalidraw")]
    McpExcalidraw,
    /// Run ECC plugin health probe (port of integrations/mcp-plugin-ecc.sh)
    #[command(name = "mcp-plugin-ecc")]
    McpPluginEcc,
    /// Run X (Twitter) API bearer token probe (port of integrations/x-twitter.sh)
    #[command(name = "x-twitter")]
    XTwitter,
    /// Run Apple Contacts TCC access probe (port of integrations/apple-addressbook.sh)
    #[command(name = "apple-addressbook")]
    AppleAddressbook,
    /// Run Tailscale daemon and peer connectivity probe (port of integrations/tailscale.sh)
    #[command(name = "tailscale")]
    Tailscale,
    /// Run Publer API health probe (port of integrations/publer.sh)
    #[command(name = "publer")]
    Publer,
    /// Run Granola MCP health probe (port of integrations/granola-mcp.sh)
    #[command(name = "granola-mcp")]
    GranolaMcp,
    /// Run integration checks for a tier in parallel (port of hex-integration-check-all.sh)
    #[command(name = "check-all")]
    CheckAll {
        /// Tier to check: critical, standard, slow, or all (default: all)
        #[arg(long, default_value = "all")]
        tier: String,
    },
    /// Emit a hex.integration.* telemetry event (port of lib/integration/telemetry.py)
    #[command(name = "telemetry")]
    Telemetry {
        /// Event type, e.g. hex.integration.installed.ok
        event_type: String,
        /// JSON payload (default: {})
        #[arg(default_value = "{}")]
        payload: String,
        /// Event source tag
        #[arg(default_value = "hex-integration")]
        source: String,
    },
    /// Post weekly integrations summary to #integrations (port of integrations-digest.sh)
    Digest,
    /// Run one integration sub-check, update state, emit events (port of hex-integration-check.sh)
    #[command(name = "run-check")]
    RunCheck {
        /// Integration name to check
        name: String,
    },
    /// Keep the xmcp OAuth2 access token alive by rotating it in .env (port of x-oauth2-refresh.sh)
    #[command(name = "x-refresh")]
    XRefresh,
}

#[derive(Subcommand)]
enum MemoryCommands {
    /// Query behavioral memory for relevant corrections
    CheckBehavior { query: String },
    /// Store a behavioral correction
    Store {
        text: String,
        #[arg(long)]
        rule: Option<String>,
        #[arg(long)]
        session: Option<String>,
    },
    /// Bootstrap memory from feedback files
    Bootstrap,
    /// Show memory health stats
    Health,
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
}

#[derive(Subcommand)]
enum ExtensionCommands {
    /// List installed extensions
    List,
    /// Validate an extension manifest
    Validate { path: PathBuf },
    /// Show full manifest for an extension
    Info { name: String },
    /// Enable a disabled extension
    Enable { name: String },
    /// Disable an installed extension
    Disable { name: String },
}

#[derive(Subcommand)]
enum PathMapCommands {
    /// Translate a v1 path (dot-claude/…) to its v2 equivalent (system/…)
    #[command(name = "v1-to-v2")]
    V1ToV2 {
        /// Source-relative v1 path (e.g. dot-claude/scripts/foo.sh)
        path: String,
    },
    /// Translate a v2 path (system/…) to its v1 equivalent (dot-claude/…)
    #[command(name = "v2-to-v1")]
    V2ToV1 {
        /// Source-relative v2 path (e.g. system/scripts/foo.sh)
        path: String,
    },
    /// Detect whether a repo root uses the v1 or v2 layout
    #[command(name = "detect-layout")]
    DetectLayout {
        /// Path to the repo root directory
        root: String,
    },
}

#[derive(Subcommand)]
enum SessionCommands {
    /// Post-session reflection: update reflection-log.md and run session-delta.py (port of session-reflect.sh)
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
enum MirofishCommands {
    /// Check VM status and service health
    Status,
    /// Deploy latest code to Mirofish GCE VM (port of mirofish-deploy.sh)
    Deploy,
}

#[derive(Subcommand)]
enum KalshiCommands {
    /// Generate RSA keypair for Kalshi API authentication (port of kalshi-keygen.sh)
    Keygen {
        /// Override the secrets directory (default: $HEX_DIR/.hex/secrets)
        #[arg(long)]
        secrets_dir: Option<std::path::PathBuf>,
    },
    /// Two-legged connectivity probe: public exchange/status + signed portfolio/balance (port of integrations/kalshi.sh)
    Probe {
        /// Override the secrets directory (default: $HEX_DIR/.hex/secrets)
        #[arg(long)]
        secrets_dir: Option<std::path::PathBuf>,
    },
}

#[derive(Subcommand)]
enum CaptureCommands {
    /// Capture text (port of .hex/scripts/capture.sh)
    Text {
        /// Text to capture; omit to read from stdin or $EDITOR
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Ingest hex-ui feedback messages into the feedback log (port of hex-ui-feedback-ingest.sh)
    Ingest,
    /// Dispatch triaged captures as BOI specs (port of capture-to-dispatch.sh)
    Dispatch {
        /// Show what would happen without dispatching
        #[arg(long)]
        dry_run: bool,
        /// Max specs to dispatch per run
        #[arg(long, default_value = "3")]
        max: u32,
        /// Path to a specific triage report
        #[arg(long)]
        triage: Option<String>,
    },
}

#[derive(Subcommand)]
enum PulseCommands {
    /// Load API key from secrets and start the pulse server.py (port of pulse/start.sh)
    Start {
        /// Extra arguments forwarded to server.py
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },
}

#[derive(Subcommand)]
enum McpCommands {
    /// Rewrite MCP OAuth auth URL so redirect_uri routes through hex-router (port of mcp-oauth-rewrite.sh)
    #[command(name = "oauth-rewrite")]
    OauthRewrite {
        /// The OAuth auth URL to rewrite
        auth_url: String,
    },
}

#[derive(Subcommand)]
enum WorkspaceCommands {
    /// Create or attach to the hex tmux workspace (port of workspace.sh)
    Launch,
}

#[derive(Subcommand)]
enum MetricsCommands {
    /// Run all user-outcome metric scripts and report PASS/FAIL (port of metrics/run-all.sh)
    #[command(name = "run-all")]
    RunAll,
    /// Rolling-24h system health scorer (port of hex-vitals.py)
    Vitals {
        /// JSON output
        #[arg(long)]
        json: bool,
    },
    /// Cost-effectiveness report: KR movement per dollar by agent (port of cost-effectiveness.py)
    #[command(name = "cost-effectiveness")]
    CostEffectiveness {
        /// Filter to a single agent ID
        #[arg(long)]
        agent: Option<String>,
        /// JSON output
        #[arg(long)]
        json: bool,
    },
    /// Input:Output telemetry ratio calculator (port of telemetry-ratio.py)
    #[command(name = "telemetry-ratio")]
    TelemetryRatio {
        /// Hours to look back (default 24)
        #[arg(long, default_value = "24")]
        hours: u32,
        /// Filter to a surface (e.g. pulse)
        #[arg(long)]
        surface: Option<String>,
        /// JSON output
        #[arg(long)]
        json: bool,
    },
    /// Delete telemetry files older than 7 days and cap dirs at 50MB (port of rotate-telemetry.sh)
    #[command(name = "rotate-telemetry")]
    RotateTelemetry,
}

#[derive(Subcommand)]
enum TelemetryCommands {
    /// Delete telemetry files older than 7 days and cap dirs at 50MB (port of rotate-telemetry.sh)
    Rotate,
}

#[derive(Subcommand)]
enum HealthCommands {
    /// Check agent memory system health (port of health/check-agent-memory.sh)
    #[command(name = "check-agent-memory")]
    CheckAgentMemory,
    /// Auto-reset agent budget periods with tiered safety gate (port of health/budget-period-reset.py)
    #[command(name = "budget-reset")]
    BudgetReset {
        /// Report what would happen without writing any state
        #[arg(long)]
        dry_run: bool,
    },
    /// Run health checks for a tier, emit integrations.health.* events (port of run-health-tier.sh)
    #[command(name = "run-tier")]
    RunTier {
        /// Tier to check: critical, important, or standard
        tier: String,
    },
    /// Verify sqlite-vec is loadable and memory.db has vectors (port of health/check-vector-search.sh)
    #[command(name = "check-vector-search")]
    CheckVectorSearch,
    /// Detect agent dormancy and ghost-waking via composite liveness score (port of health/check-fleet-pulse.sh)
    #[command(name = "check-fleet-pulse")]
    CheckFleetPulse {
        #[arg(long)]
        dry_run: bool,
    },
    /// Surface POLICY LOAD/VALIDATION ERROR entries from hex-events daemon log (port of health/check-hex-events-policy-load.sh)
    #[command(name = "check-policy-load")]
    CheckPolicyLoad,
    /// Detect stalled initiatives and auto-poke owners (port of health/check-stalled-initiatives.sh)
    #[command(name = "check-stalled-initiatives")]
    CheckStalledInitiatives {
        #[arg(long)]
        dry_run: bool,
    },
    /// Fleet-level agent performance scorecard with coalesced Slack digest (port of health/fleet-scorecard-aggregate.py)
    #[command(name = "fleet-scorecard")]
    FleetScorecard {
        /// Period: 7d, 14d, or 30d
        #[arg(long, default_value = "7d")]
        period: String,
        #[arg(long)]
        dry_run: bool,
        /// Write output to a file
        #[arg(long)]
        output: Option<String>,
    },
    /// Check daily reflection log freshness (port of health/check-reflection-liveness.sh)
    #[command(name = "check-reflection-liveness")]
    CheckReflectionLiveness,
    /// Verify failure-routing pipeline integrity (port of health/check-failure-routing-roundtrip.sh)
    #[command(name = "check-failure-routing")]
    CheckFailureRouting,
    /// Check that initiative watchdog has run recently (port of watchdog-heartbeat-check.sh)
    #[command(name = "check-watchdog-heartbeat")]
    CheckWatchdogHeartbeat,
    /// Run the initiative watchdog full check (port of watchdog-run-full.sh)
    #[command(name = "watchdog-run")]
    WatchdogRun,
    /// Compute mean time-to-detect for integration failures (port of health/compute-mttd.py)
    #[command(name = "compute-mttd")]
    ComputeMttd,
    /// Verify required secret files exist and are non-empty (port of health/check-secrets.sh)
    #[command(name = "check-secrets")]
    CheckSecrets,
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
    /// Deterministic dedup, stale reference pruning, memory reindex (port of consolidate.sh)
    Consolidate,
    /// Nightly system health audit via claude -p (port of system-introspection.sh)
    Introspect,
    /// Proactive tech research agent: generate queries, search, write briefs (port of tech-scout.sh)
    #[command(name = "tech-scout")]
    TechScout {
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        verbose: bool,
    },
    /// Map agent activity to OKRs, assess coverage, write report (port of goal-alignment.sh)
    #[command(name = "goal-alignment")]
    GoalAlignment {
        #[arg(long)]
        dry_run: bool,
    },
    /// Delete Claude project .jsonl files older than N days (port of cleanup-project-jsonl.sh)
    #[command(name = "cleanup-projects")]
    CleanupProjects {
        /// Retention period in days (default 30)
        #[arg(default_value = "30")]
        days: u32,
    },
    /// Validate charter → policy trigger contract for all fleet agents
    #[command(name = "charter-triggers")]
    CharterTriggers {
        /// Validation mode: pre-migration (default) or post-migration
        #[arg(long, default_value = "pre-migration")]
        mode: String,
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

fn yaml_get<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let prefix = format!("{}: ", key);
    text.lines()
        .find(|l| l.starts_with(&prefix))
        .map(|l| l[prefix.len()..].trim_matches('"').trim_matches('\'').trim())
}

/// Scan extension dirs and return (path, enabled) pairs.
fn scan_extension_dirs(hex_dir: &Path) -> Vec<(PathBuf, bool)> {
    let search_dirs = [
        hex_dir.join("extensions"),
        hex_dir.join(".hex/extensions"),
    ];
    let mut results = Vec::new();
    for base in &search_dirs {
        let entries = match std::fs::read_dir(base) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
            if name.is_empty() {
                continue;
            }
            // .disabled suffix marks disabled extensions
            let enabled = !name.ends_with(".disabled");
            // Only consider dirs that contain extension.yaml (or would after enabling)
            let manifest = if enabled {
                path.join("extension.yaml")
            } else {
                path.join("extension.yaml")
            };
            if manifest.exists() {
                results.push((path, enabled));
            }
        }
    }
    results
}

fn run_extension_command(command: ExtensionCommands) {
    let hex_dir = get_hex_dir();
    match command {
        ExtensionCommands::List => {
            let exts = scan_extension_dirs(&hex_dir);
            if exts.is_empty() {
                println!("No extensions found.");
                return;
            }
            println!("{:<24} {:<10} {:<10} {}", "NAME", "VERSION", "TYPE", "STATUS");
            println!("{}", "-".repeat(60));
            for (path, enabled) in &exts {
                let manifest_path = path.join("extension.yaml");
                let text = std::fs::read_to_string(&manifest_path).unwrap_or_default();
                let name = yaml_get(&text, "name").unwrap_or("?").to_string();
                let version = yaml_get(&text, "version").unwrap_or("?").to_string();
                let ext_type = yaml_get(&text, "type").unwrap_or("?").to_string();
                let status = if *enabled { "enabled" } else { "disabled" };
                println!("{:<24} {:<10} {:<10} {}", name, version, ext_type, status);
            }
            println!("\n{} extension(s)", exts.len());
        }

        ExtensionCommands::Validate { path } => {
            let script = hex_dir.join(".hex/scripts/extension-validate.py");
            let status = std::process::Command::new("python3")
                .arg(&script)
                .arg(&path)
                .env("HEX_DIR", &hex_dir)
                .status()
                .unwrap_or_else(|e| {
                    eprintln!("hex extension validate: failed to run validator: {e}");
                    std::process::exit(1);
                });
            std::process::exit(status.code().unwrap_or(1));
        }

        ExtensionCommands::Info { name } => {
            let exts = scan_extension_dirs(&hex_dir);
            let found = exts.iter().find(|(path, _)| {
                let dir_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                let bare = dir_name.trim_end_matches(".disabled");
                // Match by directory name or by manifest name field
                bare == name || {
                    let text = std::fs::read_to_string(path.join("extension.yaml")).unwrap_or_default();
                    yaml_get(&text, "name").unwrap_or("") == name
                }
            });
            match found {
                Some((path, enabled)) => {
                    let manifest_path = path.join("extension.yaml");
                    let text = std::fs::read_to_string(&manifest_path).unwrap_or_else(|e| {
                        eprintln!("Cannot read extension manifest: {e}");
                        std::process::exit(1);
                    });
                    let status = if *enabled { "enabled" } else { "disabled" };
                    println!("# Extension: {} ({})\n", name, status);
                    println!("{}", text);
                }
                None => {
                    eprintln!("Extension '{}' not found.", name);
                    std::process::exit(1);
                }
            }
        }

        ExtensionCommands::Disable { name } => {
            let exts = scan_extension_dirs(&hex_dir);
            let found = exts.iter().find(|(path, enabled)| {
                if !enabled {
                    return false;
                }
                let dir_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if dir_name == name {
                    return true;
                }
                let text = std::fs::read_to_string(path.join("extension.yaml")).unwrap_or_default();
                yaml_get(&text, "name").unwrap_or("") == name
            });
            match found {
                Some((path, _)) => {
                    let disabled_path = {
                        let parent = path.parent().unwrap_or(path);
                        let dir_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                        parent.join(format!("{}.disabled", dir_name))
                    };
                    std::fs::rename(path, &disabled_path).unwrap_or_else(|e| {
                        eprintln!("Cannot disable extension '{}': {e}", name);
                        std::process::exit(1);
                    });
                    println!("Extension '{}' disabled.", name);
                }
                None => {
                    eprintln!("Extension '{}' not found or already disabled.", name);
                    std::process::exit(1);
                }
            }
        }

        ExtensionCommands::Enable { name } => {
            // Find a .disabled directory matching the name
            let search_dirs = [
                hex_dir.join("extensions"),
                hex_dir.join(".hex/extensions"),
            ];
            let mut found_path: Option<PathBuf> = None;
            'outer: for base in &search_dirs {
                let entries = match std::fs::read_dir(base) {
                    Ok(e) => e,
                    Err(_) => continue,
                };
                for entry in entries.flatten() {
                    let path = entry.path();
                    if !path.is_dir() {
                        continue;
                    }
                    let dir_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
                    if !dir_name.ends_with(".disabled") {
                        continue;
                    }
                    let bare = dir_name.trim_end_matches(".disabled");
                    let manifest = path.join("extension.yaml");
                    let manifest_name = if manifest.exists() {
                        let text = std::fs::read_to_string(&manifest).unwrap_or_default();
                        yaml_get(&text, "name").unwrap_or("").to_string()
                    } else {
                        String::new()
                    };
                    if bare == name || manifest_name == name {
                        found_path = Some(path);
                        break 'outer;
                    }
                }
            }
            match found_path {
                Some(path) => {
                    let enabled_path = {
                        let parent = path.parent().unwrap_or(&path);
                        let dir_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                        parent.join(dir_name.trim_end_matches(".disabled"))
                    };
                    std::fs::rename(&path, &enabled_path).unwrap_or_else(|e| {
                        eprintln!("Cannot enable extension '{}': {e}", name);
                        std::process::exit(1);
                    });
                    println!("Extension '{}' enabled.", name);
                }
                None => {
                    eprintln!("No disabled extension '{}' found.", name);
                    std::process::exit(1);
                }
            }
        }
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

/// Discover all agents by scanning projects/*/charter.yaml.
/// Charter file IS the registration. No hardcoded lists.
fn discover_agents(hex_dir: &Path) -> Vec<String> {
    let projects_dir = hex_dir.join("projects");
    let mut agents: Vec<String> = Vec::new();
    let entries = match std::fs::read_dir(&projects_dir) {
        Ok(e) => e,
        Err(e) => {
            eprintln!(
                "ERROR: cannot read projects directory {}: {e}",
                projects_dir.display()
            );
            std::process::exit(1);
        }
    };
    for entry in entries {
        match entry {
            Ok(e) => {
                if e.path().join("charter.yaml").exists() {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if !is_safe_agent_id(&name) {
                        eprintln!(
                            "ERROR: agent directory '{}' contains unsafe characters — skipping",
                            name
                        );
                        continue;
                    }
                    agents.push(name);
                }
            }
            Err(e) => {
                eprintln!("WARN: cannot read entry in {}: {e}", projects_dir.display());
            }
        }
    }
    agents.sort();
    agents
}

fn is_safe_agent_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && !id.contains("..")
}

fn run_agent_command(command: AgentCommands) {
    match command {
        AgentCommands::Wake {
            agent_id,
            trigger,
            payload,
        } => {
            let hex_dir = get_hex_dir();
            match wake::run(wake::WakeConfig {
                hex_dir,
                agent_id: agent_id.clone(),
                trigger,
                payload,
            }) {
                Ok(code) => {
                    let home = std::env::var("HOME").unwrap_or_default();
                    let halt_path = format!("{}/.hex-{}-HALT-loop", home, agent_id);
                    if std::path::Path::new(&halt_path).exists() {
                        eprintln!(
                            "[{}] WARNING: loop.detected — HALT-loop file present, agent halted pending review",
                            agent_id
                        );
                    }
                    std::process::exit(code)
                }
                Err(e) => {
                    eprintln!("wake failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        AgentCommands::Status { agent_id } => {
            let hex_dir = get_hex_dir();
            if let Some(id) = agent_id {
                let state_path = hex_dir.join(format!("projects/{}/state.json", id));
                match state::load(&state_path) {
                    Ok(s) => {
                        println!("Agent: {}", s.agent_id);
                        println!("Wakes: {}", s.wake_count);
                        println!(
                            "Last wake: {}",
                            s.last_wake
                                .map(|t| t.to_rfc3339())
                                .unwrap_or("never".into())
                        );
                        println!("Active queue: {} items", s.queue.active.len());
                        println!("Blocked: {} items", s.queue.blocked.len());
                        println!("Scheduled: {} items", s.queue.scheduled.len());
                        println!("Inbox: {} messages", s.inbox.len());
                        println!("Trail: {} entries", s.trail.len());
                        println!("Cost (lifetime): ${:.4}", s.cost.lifetime_usd);
                        println!(
                            "Cost (period): ${:.4} / ${:.2}",
                            s.cost.current_period.spent_usd, s.cost.current_period.budget_usd
                        );
                    }
                    Err(e) => {
                        eprintln!("Cannot load state for '{}': {e}", id);
                        std::process::exit(1);
                    }
                }
            } else {
                eprintln!("Usage: hex agent status <agent-id>");
                std::process::exit(1);
            }
        }
        AgentCommands::Fleet => {
            let hex_dir = get_hex_dir();
            let agents = discover_agents(&hex_dir);

            if agents.is_empty() {
                eprintln!("ERROR: no agents found — no projects/*/charter.yaml files exist");
                std::process::exit(1);
            }

            let mut errors: Vec<String> = Vec::new();
            let mut charters: std::collections::HashMap<String, hex::types::Charter> =
                std::collections::HashMap::new();

            for id in &agents {
                let charter_path = hex_dir.join(format!("projects/{}/charter.yaml", id));
                match hex::charter::load(&charter_path) {
                    Ok(c) => {
                        if c.id != *id {
                            errors.push(format!(
                                "ERROR: agent '{}' charter.id is '{}' — must match directory name exactly",
                                id, c.id
                            ));
                        }
                        charters.insert(id.clone(), c);
                    }
                    Err(e) => {
                        errors.push(format!("ERROR: agent '{}' has invalid charter: {e}", id));
                    }
                }
            }

            if !errors.is_empty() {
                for err in &errors {
                    eprintln!("{}", err);
                }
                std::process::exit(1);
            }

            println!(
                "{:<20} {:>4} {:>6} {:>12} {:>8} {:>8} {:>10}",
                "AGENT", "CORE", "WAKES", "LAST WAKE", "ACTIVE", "BLOCKED", "COST/DAY"
            );
            println!("{}", "-".repeat(74));
            for id in &agents {
                let is_core = charters.get(id).map(|c| c.core).unwrap_or(false);
                let core_flag = if is_core { "  ●" } else { "" };
                let state_path = hex_dir.join(format!("projects/{}/state.json", id));
                if let Ok(s) = state::load(&state_path) {
                    let last = s
                        .last_wake
                        .map(|t| t.format("%H:%M:%S").to_string())
                        .unwrap_or("never".into());
                    println!(
                        "{:<20} {:>4} {:>6} {:>12} {:>8} {:>8} ${:>9.4}",
                        id,
                        core_flag,
                        s.wake_count,
                        last,
                        s.queue.active.len(),
                        s.queue.blocked.len(),
                        s.cost.current_period.spent_usd
                    );
                } else {
                    println!(
                        "{:<20} {:>4} {:>6} {:>12} {:>8} {:>8} {:>10}",
                        id, core_flag, 0, "never", 0, 0, "new"
                    );
                }
            }

            println!("\n{} agents", agents.len());

            let core_agents: Vec<&String> = agents
                .iter()
                .filter(|id| charters.get(*id).map(|c| c.core).unwrap_or(false))
                .collect();
            if !core_agents.is_empty() {
                let mut core_warnings: Vec<String> = Vec::new();
                for id in &core_agents {
                    let kill_switch = charters
                        .get(*id)
                        .map(|c| shellexpand::tilde(&c.kill_switch).to_string())
                        .unwrap_or_default();
                    if !kill_switch.is_empty() && Path::new(&kill_switch).exists() {
                        core_warnings.push(format!(
                            "WARN: core agent '{}' is HALTED — system self-healing may be degraded",
                            id
                        ));
                    }
                }
                if !core_warnings.is_empty() {
                    eprintln!();
                    for w in &core_warnings {
                        eprintln!("{}", w);
                    }
                }
            }
        }
        AgentCommands::List { core } => {
            let hex_dir = get_hex_dir();
            let agents = discover_agents(&hex_dir);
            for id in &agents {
                if core {
                    let charter_path = hex_dir.join(format!("projects/{}/charter.yaml", id));
                    if let Ok(c) = hex::charter::load(&charter_path) {
                        if !c.core {
                            continue;
                        }
                    } else {
                        continue;
                    }
                }
                println!("{}", id);
            }
        }
        AgentCommands::CheckCore => {
            let hex_dir = get_hex_dir();
            let ref_dir = hex_dir.join(".hex/reference/core-agents");
            if !ref_dir.exists() {
                eprintln!("ERROR: no reference core agents at {}", ref_dir.display());
                std::process::exit(1);
            }
            let mut missing: Vec<String> = Vec::new();
            let mut broken: Vec<String> = Vec::new();
            let mut ok: Vec<String> = Vec::new();
            let entries = match std::fs::read_dir(&ref_dir) {
                Ok(e) => e,
                Err(e) => {
                    eprintln!(
                        "ERROR: cannot read reference directory {}: {e}",
                        ref_dir.display()
                    );
                    std::process::exit(1);
                }
            };
            {
                for entry in entries {
                    let entry = match entry {
                        Ok(e) => e,
                        Err(e) => {
                            eprintln!("WARN: cannot read reference entry: {e}");
                            continue;
                        }
                    };
                    let fname = entry.file_name().to_string_lossy().to_string();
                    if !fname.ends_with(".yaml") {
                        continue;
                    }
                    let agent_id = fname.trim_end_matches(".yaml").to_string();
                    let charter_path = hex_dir.join(format!("projects/{}/charter.yaml", agent_id));
                    if !charter_path.exists() {
                        missing.push(agent_id);
                    } else {
                        match hex::charter::load(&charter_path) {
                            Ok(c) => {
                                if !c.core {
                                    broken.push(format!(
                                        "{} (exists but core: false — should be core: true)",
                                        agent_id
                                    ));
                                } else if c.id != agent_id {
                                    broken.push(format!(
                                        "{} (charter.id '{}' doesn't match directory)",
                                        agent_id, c.id
                                    ));
                                } else {
                                    ok.push(agent_id);
                                }
                            }
                            Err(e) => {
                                broken.push(format!("{} (invalid charter: {})", agent_id, e));
                            }
                        }
                    }
                }
            }
            let total = ok.len() + missing.len() + broken.len();
            println!("Core agents: {}/{} healthy", ok.len(), total);
            for id in &ok {
                println!("  ✓ {}", id);
            }
            if !missing.is_empty() {
                println!();
                for id in &missing {
                    println!("  MISSING: {} — not found in projects/", id);
                }
            }
            if !broken.is_empty() {
                println!();
                for desc in &broken {
                    println!("  BROKEN: {}", desc);
                }
            }
            if !missing.is_empty() || !broken.is_empty() {
                println!();
                println!("Run 'hex agent restore-core' to fix missing core agents.");
                std::process::exit(1);
            }
        }
        AgentCommands::RestoreCore => {
            let hex_dir = get_hex_dir();
            let ref_dir = hex_dir.join(".hex/reference/core-agents");
            if !ref_dir.exists() {
                eprintln!("ERROR: no reference core agents at {}", ref_dir.display());
                std::process::exit(1);
            }
            let mut restored = 0;
            let mut skipped = 0;
            let mut failed = 0;
            let entries = match std::fs::read_dir(&ref_dir) {
                Ok(e) => e,
                Err(e) => {
                    eprintln!(
                        "ERROR: cannot read reference directory {}: {e}",
                        ref_dir.display()
                    );
                    std::process::exit(1);
                }
            };
            for entry in entries {
                let entry = match entry {
                    Ok(e) => e,
                    Err(e) => {
                        eprintln!("  ERROR: cannot read reference entry: {e}");
                        failed += 1;
                        continue;
                    }
                };
                let fname = entry.file_name().to_string_lossy().to_string();
                if !fname.ends_with(".yaml") {
                    continue;
                }
                let agent_id = fname.trim_end_matches(".yaml").to_string();
                let target_dir = hex_dir.join(format!("projects/{}", agent_id));
                let target_charter = target_dir.join("charter.yaml");
                if target_charter.exists() {
                    println!(
                        "  SKIP: {} — charter already exists (not overwriting)",
                        agent_id
                    );
                    skipped += 1;
                    continue;
                }
                if let Err(e) = std::fs::create_dir_all(&target_dir) {
                    eprintln!("  ERROR: cannot create {}: {e}", target_dir.display());
                    failed += 1;
                    continue;
                }
                match std::fs::copy(entry.path(), &target_charter) {
                    Ok(_) => {
                        println!("  RESTORED: {} — charter created from reference", agent_id);
                        restored += 1;
                    }
                    Err(e) => {
                        eprintln!("  ERROR: cannot copy charter for {}: {e}", agent_id);
                        failed += 1;
                    }
                }
            }
            println!();
            if restored > 0 {
                println!(
                    "Restored {} core agent(s). Run 'hex agent fleet' to verify.",
                    restored
                );
            } else if skipped > 0 {
                println!("All core agents already present ({} checked).", skipped);
            } else {
                println!("No reference charters found.");
            }
            if failed > 0 {
                eprintln!("ERROR: {} operation(s) failed during restore", failed);
                std::process::exit(1);
            }
        }
        AgentCommands::Message {
            from,
            to,
            subject,
            body,
            initiative,
            response_requested,
        } => {
            let hex_dir = get_hex_dir();
            let bus = hex::sse::SseBus::new();
            let telemetry = std::sync::Arc::new(hex::telemetry::Telemetry::new(&hex_dir));
            let handler = hex::messaging::MessagingHandler::new(&hex_dir, bus, telemetry);
            let content = format!("[{}] {}", subject, body);
            handler.cli_send(&from, vec![to.clone()], &content, "agent", initiative.as_deref());
            if response_requested {
                let audit_dir = hex_dir.join(".hex/audit");
                wake::auto_wake_target(&hex_dir, &to, &from, &audit_dir);
                println!("Auto-waking {} for live response", to);
            }
        }
        AgentCommands::Audit { agent, .. } => {
            eprintln!("audit: {:?} (not yet implemented)", agent);
            std::process::exit(1);
        }
        AgentCommands::Cost { agent, .. } => {
            eprintln!("cost: {:?} (not yet implemented)", agent);
            std::process::exit(1);
        }
        AgentCommands::OptimizerWake { trigger, payload } => {
            let hex_dir = get_hex_dir();
            let hex_bin = hex_dir.join(".hex/bin/hex");
            let bin = if hex_bin.exists() { hex_bin } else { std::env::current_exe().unwrap_or_default() };
            let status = std::process::Command::new(&bin)
                .args(["agent", "wake", "boi-optimizer", "--trigger"])
                .arg(&trigger)
                .arg("--payload")
                .arg(&payload)
                .status()
                .unwrap_or_else(|e| {
                    eprintln!("ERROR: failed to wake boi-optimizer: {e}");
                    std::process::exit(1);
                });
            std::process::exit(status.code().unwrap_or(0));
        }
        AgentCommands::Evolution { dry_run } => {
            let rc = agent_evolution::run(dry_run);
            std::process::exit(rc);
        }
        AgentCommands::Spawn { spec_file, dry_run } => {
            let rc = agent_spawn::run_spawn(&spec_file, dry_run);
            std::process::exit(rc);
        }
        AgentCommands::ResetPeriods { dry_run } => {
            let home = std::env::var("HOME").unwrap_or_default();
            let projects_dir = std::path::PathBuf::from(&home).join("mrap-hex/projects");
            if !projects_dir.is_dir() {
                println!("[reset-periods] PROJECTS_DIR not found: {}", projects_dir.display());
                return;
            }
            let now = chrono::Utc::now();
            let stale_days = 7i64;
            let mut reset_count = 0u32;
            let mut checked_count = 0u32;
            let mut entries: Vec<std::path::PathBuf> = std::fs::read_dir(&projects_dir)
                .unwrap_or_else(|_| { eprintln!("cannot read projects dir"); std::process::exit(1); })
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_dir() && !p.file_name().unwrap_or_default().to_string_lossy().starts_with('_'))
                .collect();
            entries.sort();
            for agent_dir in entries {
                let agent_id = agent_dir.file_name().unwrap_or_default().to_string_lossy().to_string();
                let state_path = agent_dir.join("state.json");
                if !state_path.is_file() { continue; }
                let state_text = match std::fs::read_to_string(&state_path) {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                let mut state: serde_json::Value = match serde_json::from_str(&state_text) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let start_str = state.pointer("/cost/current_period/start")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let start_str = match start_str {
                    Some(s) => s,
                    None => continue,
                };
                checked_count += 1;
                let start = match chrono::DateTime::parse_from_rfc3339(&start_str)
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .or_else(|_| chrono::DateTime::parse_from_rfc3339(&start_str.replace("Z", "+00:00"))
                        .map(|dt| dt.with_timezone(&chrono::Utc)))
                {
                    Ok(dt) => dt,
                    Err(_) => continue,
                };
                let age_days = (now - start).num_days();
                if age_days > stale_days {
                    println!("  RESET  {}: period was {} (stale {}d)", agent_id, &start_str[..10], age_days);
                    if !dry_run {
                        let new_start = now.to_rfc3339().replace("+00:00", "Z");
                        if let Some(period) = state.pointer_mut("/cost/current_period") {
                            period["start"] = serde_json::Value::String(new_start);
                            period["spent_usd"] = serde_json::Value::from(0.0f64);
                        }
                        let tmp_path = state_path.with_extension("json.tmp");
                        if let Ok(s) = serde_json::to_string_pretty(&state) {
                            let _ = std::fs::write(&tmp_path, s);
                            let _ = std::fs::rename(&tmp_path, &state_path);
                        }
                    }
                    reset_count += 1;
                } else {
                    println!("  OK     {}: period started {}", agent_id, &start_str[..10]);
                }
            }
            println!("\n[reset-periods] checked={checked_count} reset={reset_count}{}",
                if dry_run { " (dry-run)" } else { "" });
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let binary_name = Path::new(&args[0])
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let effective_args = if binary_name == "hex-agent" {
        let mut new_args = vec![args[0].clone(), "agent".to_string()];
        new_args.extend(args[1..].to_vec());
        new_args
    } else {
        args
    };
    let cli = Cli::parse_from(effective_args);

    match cli.command {
        Commands::Agent { command } => run_agent_command(command),
        Commands::Extension { command } => run_extension_command(command),
        Commands::Server { command } => match command {
            ServerCommands::Start { port } => {
                let hex_dir = get_hex_dir();
                let bus = hex::sse::SseBus::new();
                let topics_dir = hex_dir.join("system/sse/topics");
                bus.load_manifests(&topics_dir);
                let telemetry = std::sync::Arc::new(hex::telemetry::Telemetry::new(&hex_dir));
                let events = hex::events::EventEngine::new(
                    &hex_dir,
                    std::sync::Arc::clone(&telemetry),
                    std::sync::Arc::clone(&bus),
                ).unwrap_or_else(|e| {
                    eprintln!("hex server: events engine init failed: {e}");
                    std::process::exit(1);
                });
                let messaging = hex::messaging::MessagingHandler::new(
                    &hex_dir,
                    std::sync::Arc::clone(&bus),
                    std::sync::Arc::clone(&telemetry),
                );
                let assets = hex::assets::AssetsHandler::new(
                    &hex_dir,
                    std::sync::Arc::clone(&bus),
                    std::sync::Arc::clone(&telemetry),
                );
                let ext_db = hex::extensions::ExtensionDb::open(&hex_dir)
                    .unwrap_or_else(|e| {
                        eprintln!("hex server: extension db init failed: {e}");
                        std::process::exit(1);
                    });
                ext_db.scan_and_migrate(&hex_dir);
                let server = hex::server::HexServer::new(port, hex_dir, bus, telemetry, events, messaging, assets, ext_db);
                server.start();
            }
            ServerCommands::Health => {
                let port = 8880u16;
                if hex::server::HexServer::check_health(port) {
                    println!("hex server is running on port {}", port);
                } else {
                    eprintln!("hex server is not running on port {}", port);
                    std::process::exit(1);
                }
            }
        },
        Commands::Asset { command } => {
            let hex_dir = get_hex_dir();
            let bus = hex::sse::SseBus::new();
            let telemetry = std::sync::Arc::new(hex::telemetry::Telemetry::new(&hex_dir));
            let handler = hex::assets::AssetsHandler::new(&hex_dir, bus, telemetry);
            match command {
                AssetCommands::Resolve { id } => handler.cli_resolve(&id),
                AssetCommands::List { r#type } => handler.cli_list(r#type.as_deref()),
                AssetCommands::Search { query } => handler.cli_search(&query),
                AssetCommands::Register { r#type, id, title, path } => {
                    handler.cli_register(&r#type, &id, &title, path.as_deref())
                }
                AssetCommands::Types => handler.cli_types(),
                AssetCommands::Discover { dry_run } => {
                    let script = hex_dir.join("system/scripts/hex-asset-discover.py");
                    let args: &[&str] = if dry_run { &["--dry-run"] } else { &[] };
                    std::process::exit(exec_script(&script, args));
                }
            }
        }
        Commands::Message { command } => {
            let hex_dir = get_hex_dir();
            let bus = hex::sse::SseBus::new();
            let telemetry = std::sync::Arc::new(hex::telemetry::Telemetry::new(&hex_dir));
            let handler = hex::messaging::MessagingHandler::new(&hex_dir, bus, telemetry);
            match command {
                MessageCommands::Send { from, to, content, msg_type, anchor } => {
                    handler.cli_send(&from, to, &content, &msg_type, anchor.as_deref());
                }
                MessageCommands::List { msg_type, status, anchor } => {
                    handler.cli_list(msg_type.as_deref(), status.as_deref(), anchor.as_deref());
                }
                MessageCommands::Respond { id, status, action, assets } => {
                    handler.cli_respond(&id, &status, action.as_deref(), assets);
                }
            }
        }
        Commands::Events { command } => {
            let hex_dir = get_hex_dir();
            let bus = hex::sse::SseBus::new();
            let telemetry = std::sync::Arc::new(hex::telemetry::Telemetry::new(&hex_dir));
            let engine = hex::events::EventEngine::new(&hex_dir, telemetry, bus)
                .unwrap_or_else(|e| {
                    eprintln!("events engine init failed: {e}");
                    std::process::exit(1);
                });
            match command {
                EventsCommands::Status => engine.cli_status(),
                EventsCommands::Emit { event_type, payload, source } => engine.cli_emit(&event_type, &payload, &source),
                EventsCommands::Trace { event_id } => engine.cli_trace(event_id),
                EventsCommands::Policies => engine.cli_policies(),
                EventsCommands::Reload => engine.cli_reload(),
                EventsCommands::Daemon { shadow } => {
                    hex::events::EventEngine::cli_daemon(engine, shadow);
                }
            }
        }
        Commands::Sse { command } => match command {
            SseCommands::Publish { topic, r#type, payload } => {
                eprintln!(
                    "hex sse publish {} {} {} (not yet implemented)",
                    topic, r#type, payload
                );
                std::process::exit(1);
            }
            SseCommands::Topics => {
                eprintln!("hex sse topics (not yet implemented)");
                std::process::exit(1);
            }
            SseCommands::Bridge { hex_event_name, payload } => {
                let hex_dir = get_hex_dir();
                hex::sse::bridge(&hex_dir, &hex_event_name, &payload);
            }
        },
        Commands::Integration { command } => {
            if let IntegrationCommands::Template = command {
                integration::template();
                return;
            }
            if let IntegrationCommands::McpExa = command {
                std::process::exit(integration_mcp_exa::run_probe());
            }
            if let IntegrationCommands::McpExcalidraw = command {
                std::process::exit(integration_mcp_excalidraw::run_probe());
            }
            if let IntegrationCommands::McpPluginEcc = command {
                std::process::exit(integration_mcp_plugin_ecc::run_probe());
            }
            if let IntegrationCommands::XTwitter = command {
                std::process::exit(integration_x_twitter::run_probe());
            }
            if let IntegrationCommands::AppleAddressbook = command {
                std::process::exit(integration_apple_addressbook::run_probe());
            }
            if let IntegrationCommands::Tailscale = command {
                std::process::exit(integration_tailscale::run_probe());
            }
            if let IntegrationCommands::Publer = command {
                std::process::exit(integration_publer::run_probe());
            }
            if let IntegrationCommands::GranolaMcp = command {
                std::process::exit(integration_granola_mcp::run_probe());
            }
            if let IntegrationCommands::CheckAll { ref tier } = command {
                let hex_dir = get_hex_dir();
                let code = integration_check_all::run(&hex_dir, tier);
                std::process::exit(code);
            }
            if let IntegrationCommands::Telemetry { ref event_type, ref payload, ref source } = command {
                let hex_dir = get_hex_dir();
                let code = integration_telemetry::emit_event(&hex_dir, event_type, payload, source);
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
            if let IntegrationCommands::XRefresh = command {
                let hex_dir = get_hex_dir();
                let script = hex_dir.join("system/scripts/x-oauth2-refresh.sh");
                std::process::exit(exec_script(&script, &[]));
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
                IntegrationCommands::McpExa => unreachable!(),
                IntegrationCommands::McpExcalidraw => unreachable!(),
                IntegrationCommands::McpPluginEcc => unreachable!(),
                IntegrationCommands::XTwitter => unreachable!(),
                IntegrationCommands::AppleAddressbook => unreachable!(),
                IntegrationCommands::Tailscale => unreachable!(),
                IntegrationCommands::Publer => unreachable!(),
                IntegrationCommands::GranolaMcp => unreachable!(),
                IntegrationCommands::CheckAll { .. } => unreachable!(),
                IntegrationCommands::Telemetry { .. } => unreachable!(),
                IntegrationCommands::Digest => unreachable!(),
                IntegrationCommands::RunCheck { .. } => unreachable!(),
                IntegrationCommands::XRefresh => unreachable!(),
            };
            let start = std::time::Instant::now();
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
            let duration_ms = start.elapsed().as_millis() as u64;
            let telemetry = std::sync::Arc::new(hex::telemetry::Telemetry::new(&hex_dir));
            let integration_name = name_arg.as_deref().unwrap_or("(all)");
            telemetry.emit(&format!("hex.integration.{}", subcmd), &serde_json::json!({
                "integration": integration_name,
                "exit_code": exit_code,
                "duration_ms": duration_ms,
            }));
            std::process::exit(exit_code);
        }
        Commands::Memory { command } => {
            let hex_dir = get_hex_dir();
            let telemetry = std::sync::Arc::new(hex::telemetry::Telemetry::new(&hex_dir));
            let subcmd_name = match &command {
                MemoryCommands::CheckBehavior { .. } => "check-behavior",
                MemoryCommands::Store { .. } => "store",
                MemoryCommands::Bootstrap => "bootstrap",
                MemoryCommands::Health => "health",
                MemoryCommands::Search { .. } => "search",
                MemoryCommands::Index { stats, .. } => {
                    if *stats { "index-stats" } else { "index" }
                }
                MemoryCommands::ParseTranscripts { .. } => "parse-transcripts",
            };
            let start = std::time::Instant::now();
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
                _ => {
                    let hex_memory = hex_dir.join(".hex/scripts/bin/hex-memory");
                    let mut cmd = std::process::Command::new("bash");
                    cmd.arg(&hex_memory);
                    match &command {
                        MemoryCommands::CheckBehavior { query } => {
                            cmd.arg("check-behavior").arg(query);
                        }
                        MemoryCommands::Store { text, rule, session } => {
                            cmd.arg("store").arg(text);
                            if let Some(r) = rule {
                                cmd.arg("--rule").arg(r);
                            }
                            if let Some(s) = session {
                                cmd.arg("--session").arg(s);
                            }
                        }
                        MemoryCommands::Bootstrap => {
                            cmd.arg("bootstrap");
                        }
                        MemoryCommands::Health => {
                            cmd.arg("health");
                        }
                        _ => unreachable!(),
                    }
                    cmd.env("HEX_DIR", &hex_dir);
                    cmd.status().map(|s| s.code().unwrap_or(1)).unwrap_or(1)
                }
            };
            let duration_ms = start.elapsed().as_millis() as u64;
            telemetry.emit(
                &format!("hex.memory.{}", subcmd_name),
                &serde_json::json!({
                    "exit_code": exit_code,
                    "duration_ms": duration_ms,
                }),
            );
            std::process::exit(exit_code);
        }
        Commands::Metrics { command } => match command {
            MetricsCommands::RunAll => {
                let hex_dir = get_hex_dir();
                metrics::run_all(&hex_dir);
            }
            MetricsCommands::Vitals { json } => {
                let hex_dir = get_hex_dir();
                let script = hex_dir.join("system/scripts/hex-vitals.py");
                let mut args: Vec<&str> = vec![];
                let json_flag;
                if json { json_flag = "--json"; args.push(json_flag); }
                std::process::exit(exec_script(&script, &args));
            }
            MetricsCommands::CostEffectiveness { agent, json } => {
                let hex_dir = get_hex_dir();
                let script = hex_dir.join("system/scripts/cost-effectiveness.py");
                let mut args: Vec<String> = vec![];
                if let Some(ref a) = agent { args.push("--agent".into()); args.push(a.clone()); }
                if json { args.push("--json".into()); }
                let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                std::process::exit(exec_script(&script, &arg_refs));
            }
            MetricsCommands::TelemetryRatio { hours, surface, json } => {
                let hex_dir = get_hex_dir();
                let script = hex_dir.join("system/scripts/telemetry-ratio.py");
                let hours_s = hours.to_string();
                let mut args: Vec<&str> = vec!["--hours", &hours_s];
                let surface_flag;
                if let Some(ref s) = surface { surface_flag = s.clone(); args.push("--surface"); args.push(&surface_flag); }
                if json { args.push("--json"); }
                std::process::exit(exec_script(&script, &args));
            }
            MetricsCommands::RotateTelemetry => {
                let home = std::env::var("HOME").unwrap_or_default();
                let script = std::path::PathBuf::from(&home).join("mrap-hex/.hex/scripts/rotate-telemetry.sh");
                std::process::exit(exec_script(&script, &[]));
            }
        },
        Commands::Health { command } => match command {
            HealthCommands::CheckAgentMemory => {
                health::check_agent_memory();
            }
            HealthCommands::BudgetReset { dry_run } => {
                let hex_dir = get_hex_dir();
                let code = health::budget_reset::run(&health::budget_reset::BudgetResetConfig {
                    hex_dir,
                    dry_run,
                });
                std::process::exit(code);
            }
            HealthCommands::RunTier { tier } => {
                let hex_dir = get_hex_dir();
                let code = health::run_tier(&hex_dir, &tier);
                std::process::exit(code);
            }
            HealthCommands::CheckVectorSearch => {
                let hex_dir = get_hex_dir();
                let script = hex_dir.join(".hex/scripts/health/check-vector-search.sh");
                std::process::exit(exec_script(&script, &[]));
            }
            HealthCommands::CheckFleetPulse { dry_run } => {
                let hex_dir = get_hex_dir();
                let script = hex_dir.join(".hex/scripts/health/check-fleet-pulse.sh");
                let args: &[&str] = if dry_run { &["--dry-run"] } else { &[] };
                std::process::exit(exec_script(&script, args));
            }
            HealthCommands::CheckPolicyLoad => {
                let hex_dir = get_hex_dir();
                let script = hex_dir.join(".hex/scripts/health/check-hex-events-policy-load.sh");
                std::process::exit(exec_script(&script, &[]));
            }
            HealthCommands::CheckStalledInitiatives { dry_run } => {
                let hex_dir = get_hex_dir();
                let script = hex_dir.join(".hex/scripts/health/check-stalled-initiatives.sh");
                let args: &[&str] = if dry_run { &["--dry-run"] } else { &[] };
                std::process::exit(exec_script(&script, args));
            }
            HealthCommands::FleetScorecard { period, dry_run, output } => {
                let hex_dir = get_hex_dir();
                let script = hex_dir.join(".hex/scripts/health/fleet-scorecard-aggregate.py");
                let mut args: Vec<String> = vec!["--period".into(), period];
                if dry_run { args.push("--dry-run".into()); }
                if let Some(o) = output { args.push("--output".into()); args.push(o); }
                let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                std::process::exit(exec_script(&script, &arg_refs));
            }
            HealthCommands::CheckReflectionLiveness => {
                let home = std::env::var("HOME").unwrap_or_default();
                let script = std::path::PathBuf::from(&home).join("mrap-hex/.hex/scripts/health/check-reflection-liveness.sh");
                std::process::exit(exec_script(&script, &[]));
            }
            HealthCommands::CheckFailureRouting => {
                let home = std::env::var("HOME").unwrap_or_default();
                let script = std::path::PathBuf::from(&home).join("mrap-hex/.hex/scripts/health/check-failure-routing-roundtrip.sh");
                std::process::exit(exec_script(&script, &[]));
            }
            HealthCommands::CheckWatchdogHeartbeat => {
                let hex_dir = get_hex_dir();
                let script = hex_dir.join("system/scripts/watchdog-heartbeat-check.sh");
                std::process::exit(exec_script(&script, &[]));
            }
            HealthCommands::WatchdogRun => {
                let hex_dir = get_hex_dir();
                let script = hex_dir.join("system/scripts/watchdog-run-full.sh");
                std::process::exit(exec_script(&script, &[]));
            }
            HealthCommands::ComputeMttd => {
                let code = health::compute_mttd();
                std::process::exit(code);
            }
            HealthCommands::CheckSecrets => {
                let code = health::check_secrets();
                std::process::exit(code);
            }
        },
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
                DoctorCommands::Consolidate => {
                    let script = hex_dir.join("system/scripts/consolidate.sh");
                    std::process::exit(exec_script(&script, &[]));
                }
                DoctorCommands::Introspect => {
                    let script = hex_dir.join(".hex/scripts/system-introspection.legacy.sh");
                    std::process::exit(exec_script(&script, &[]));
                }
                DoctorCommands::TechScout { dry_run, verbose } => {
                    let script = hex_dir.join(".hex/scripts/tech-scout.legacy.sh");
                    let mut args: Vec<&str> = vec![];
                    if dry_run { args.push("--dry-run"); }
                    if verbose { args.push("--verbose"); }
                    std::process::exit(exec_script(&script, &args));
                }
                DoctorCommands::GoalAlignment { dry_run } => {
                    let script = hex_dir.join(".hex/scripts/goal-alignment.legacy.sh");
                    let args: &[&str] = if dry_run { &["--dry-run"] } else { &[] };
                    std::process::exit(exec_script(&script, args));
                }
                DoctorCommands::CleanupProjects { days } => {
                    let script = hex_dir.join(".hex/scripts/cleanup-project-jsonl.legacy.sh");
                    let days_s = days.to_string();
                    std::process::exit(exec_script(&script, &[&days_s]));
                }
                DoctorCommands::CharterTriggers { mode } => {
                    std::process::exit(charter_triggers::run(&hex_dir, &mode));
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
                    let telemetry = std::sync::Arc::new(hex::telemetry::Telemetry::new(&hex_dir));
                    let start = std::time::Instant::now();
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
                    let duration_ms = start.elapsed().as_millis() as u64;
                    telemetry.emit("hex.doctor.run", &serde_json::json!({
                        "fix": fix,
                        "quiet": quiet,
                        "json": json,
                        "filter": filter,
                        "exit_code": exit_code,
                        "duration_ms": duration_ms,
                    }));
                    if exit_code != 0 {
                        telemetry.emit("hex.doctor.failed", &serde_json::json!({
                            "exit_code": exit_code,
                            "duration_ms": duration_ms,
                        }));
                    }
                    std::process::exit(exit_code);
                }
                DoctorCommands::List => {
                    doctor::Runner::all_checks().list();
                }
            }
        }
        Commands::Mirofish { command } => match command {
            MirofishCommands::Status => mirofish::run_status(),
            MirofishCommands::Deploy => mirofish::run_deploy(),
        },
        Commands::Kalshi { command } => match command {
            KalshiCommands::Keygen { secrets_dir } => {
                let dir = match secrets_dir {
                    Some(d) => d,
                    None => {
                        let hex_dir = get_hex_dir();
                        kalshi::secrets_dir_from_hex(&hex_dir)
                    }
                };
                kalshi::run_keygen(&dir);
            }
            KalshiCommands::Probe { secrets_dir } => {
                let hex_dir = get_hex_dir();
                let dir = secrets_dir.unwrap_or_else(|| kalshi::secrets_dir_from_hex(&hex_dir));
                let sign_script = hex_dir.join(".hex/scripts/integrations/lib/kalshi_sign.py");
                std::process::exit(kalshi::run_probe(&dir, &sign_script));
            }
        },
        Commands::Pulse { command } => match command {
            PulseCommands::Start { args } => {
                let hex_dir = get_hex_dir();
                pulse::run_start(&hex_dir, &args);
            }
        },
        Commands::Session { command } => match command {
            SessionCommands::Reflect { session_id, quiet } => {
                session_reflect::run(session_id.as_deref(), quiet);
            }
        },
        Commands::PathMap { command } => match command {
            PathMapCommands::V1ToV2 { path } => path_map::run_v1_to_v2(&path),
            PathMapCommands::V2ToV1 { path } => path_map::run_v2_to_v1(&path),
            PathMapCommands::DetectLayout { root } => path_map::run_detect_layout(&root),
        },
        Commands::Today { format } => {
            today::run(format.as_deref());
        }
        Commands::Mcp { command } => match command {
            McpCommands::OauthRewrite { auth_url } => {
                std::process::exit(mcp::oauth_rewrite(&auth_url));
            }
        },
        Commands::Workspace { command } => match command {
            WorkspaceCommands::Launch => {
                let hex_dir = get_hex_dir();
                workspace::run_launch(&hex_dir);
            }
        },
        Commands::Startup { quick, step, status } => {
            let hex_dir = get_hex_dir();
            let code = startup::run(
                &hex_dir,
                startup::StartupArgs { quick, step, status },
            );
            std::process::exit(code);
        }
        Commands::Checkpoint { focus } => {
            let hex_dir = get_hex_dir();
            let code = checkpoint::run(&hex_dir, checkpoint::CheckpointArgs { focus });
            std::process::exit(code);
        }
        Commands::Shutdown { session_id } => {
            let hex_dir = get_hex_dir();
            let code = shutdown::run(&hex_dir, shutdown::ShutdownArgs { session_id });
            std::process::exit(code);
        }
        Commands::Fleet { command } => match command {
            FleetCommands::Install => {
                let hex_dir = get_hex_dir();
                fleet::run_install(&hex_dir);
            }
        },
        Commands::BoiPm { command } => match command {
            BoiPmCommands::Install => {
                let hex_dir = get_hex_dir();
                boi_pm::run_install(&hex_dir);
            }
            BoiPmCommands::Verify { spec_id } => {
                let home = std::env::var("HOME").unwrap_or_default();
                let script = std::path::PathBuf::from(&home).join("mrap-hex/.hex/scripts/boi-completion-verify.sh");
                std::process::exit(exec_script(&script, &[&spec_id]));
            }
            BoiPmCommands::Archive { spec_id, target_repo } => {
                let home = std::env::var("HOME").unwrap_or_default();
                let script = std::path::PathBuf::from(&home).join(".boi/scripts/boi-completion-to-archive.sh");
                let mut args: Vec<String> = vec![spec_id];
                if let Some(r) = target_repo { args.push(r); }
                let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                std::process::exit(exec_script(&script, &arg_refs));
            }
            BoiPmCommands::AutoCommit { spec_id, target_repo, manifest } => {
                let home = std::env::var("HOME").unwrap_or_default();
                let script = std::path::PathBuf::from(&home).join(".hex-events/scripts/auto-commit-boi-output.sh");
                let mut args: Vec<String> = vec![spec_id];
                if let Some(r) = target_repo { args.push(r); } else { args.push(String::new()); }
                if let Some(m) = manifest { args.push(m); }
                let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                std::process::exit(exec_script(&script, &arg_refs));
            }
        },
        Commands::BoiWeb { command } => match command {
            BoiWebCommands::Serve => {
                let hex_dir = get_hex_dir();
                boi_web::run_serve(&hex_dir);
            }
        },
        Commands::SpecTool { command } => match command {
            SpecToolCommands::Run => {
                let hex_dir = get_hex_dir();
                spec_tool::run_run(&hex_dir);
            }
            SpecToolCommands::VerifyClaims { spec_file, workspace, verbose } => {
                let hex_dir = get_hex_dir();
                let script = hex_dir.join("system/scripts/verify-spec-claims.py");
                let mut args: Vec<String> = vec![spec_file];
                if let Some(w) = workspace { args.push("--workspace".into()); args.push(w); }
                if verbose { args.push("--verbose".into()); }
                let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                std::process::exit(exec_script(&script, &arg_refs));
            }
            SpecToolCommands::ResolveOwner { spec } => {
                let hex_dir = get_hex_dir();
                let script = hex_dir.join("system/scripts/spec-owner-resolver.py");
                std::process::exit(exec_script(&script, &[&spec]));
            }
            SpecToolCommands::FailureBrief { spec_id } => {
                let hex_dir = get_hex_dir();
                let script = hex_dir.join("system/scripts/build-failure-brief.py");
                std::process::exit(exec_script(&script, &[&spec_id]));
            }
            SpecToolCommands::CheckCohesion { spec, all, map } => {
                let hex_dir = get_hex_dir();
                let script = hex_dir.join("system/scripts/check-cohesion.py");
                let mut args: Vec<&str> = vec![];
                let spec_s;
                if let Some(ref s) = spec { spec_s = s.clone(); args.push("--spec"); args.push(&spec_s); }
                if all { args.push("--all"); }
                if map { args.push("--map"); }
                std::process::exit(exec_script(&script, &args));
            }
        },
        Commands::Router { command } => match command {
            RouterCommands::Serve => {
                let hex_dir = get_hex_dir();
                router::run_serve(&hex_dir);
            }
            RouterCommands::RouteComment { comment_id, asset, text } => {
                let hex_dir = get_hex_dir();
                let script = hex_dir.join("system/scripts/route-comment.py");
                std::process::exit(exec_script(&script, &[&comment_id, &asset, &text]));
            }
        },
        Commands::Capture { command } => {
            let hex_dir = get_hex_dir();
            match command {
                CaptureCommands::Text { args } => {
                    capture::run_capture(&hex_dir, &args);
                }
                CaptureCommands::Ingest => {
                    capture::run_ingest(&hex_dir);
                }
                CaptureCommands::Dispatch { dry_run, max, triage } => {
                    capture::run_dispatch(&hex_dir, dry_run, max, triage);
                }
            }
        }
        Commands::Alert { command } => match command {
            AlertCommands::Send { severity, agent_id, message } => {
                let hex_dir = get_hex_dir();
                alert::run_send(&hex_dir, &severity, &agent_id, &message);
            }
        },
        Commands::Synthesis { command } => {
            let hex_dir = get_hex_dir();
            match command {
                SynthesisCommands::Weekly { force, dry_run } => {
                    let script = hex_dir.join("system/scripts/weekly-synthesis-digest.sh");
                    let mut args: Vec<&str> = vec![];
                    if force { args.push("--force"); }
                    if dry_run { args.push("--dry-run"); }
                    std::process::exit(exec_script(&script, &args));
                }
                SynthesisCommands::Trigger { dry_run } => {
                    let script = hex_dir.join("system/scripts/synthesis-trigger.sh");
                    let args: &[&str] = if dry_run { &["--dry-run"] } else { &[] };
                    std::process::exit(exec_script(&script, args));
                }
            }
        }
        Commands::Route { command } => {
            let hex_dir = get_hex_dir();
            match command {
                RouteCommands::Message { message, threshold, all, provider } => {
                    let text = message.join(" ");
                    std::process::exit(route::run_message(&hex_dir, &text, threshold, all, &provider));
                }
                RouteCommands::Comment { comment_id, asset, text } => {
                    let text_str = text.join(" ");
                    std::process::exit(route::run_comment(&hex_dir, &comment_id, &asset, &text_str));
                }
                RouteCommands::DetectContext { message } => {
                    let text = message.join(" ");
                    std::process::exit(route::run_detect_context(&hex_dir, &text));
                }
            }
        }
        Commands::Env { command } => env::run_env_command(command),
        Commands::Validate { command } => match command {
            ValidateCommands::BoiSpec { files } => {
                std::process::exit(validate::run_boi_spec(&files));
            }
            ValidateCommands::Extension { path } => {
                std::process::exit(validate::run_extension(&path));
            }
            ValidateCommands::E2e { url, check_api, check_sse, timeout } => {
                std::process::exit(validate::run_e2e(&url, &check_api, &check_sse, timeout));
            }
        },
        Commands::Initiative { command } => {
            let hex_dir = get_hex_dir();
            match command {
                InitiativeCommands::List { status } => initiative::run_list(&hex_dir, Some(&status)),
                InitiativeCommands::Show { id } => initiative::run_show(&hex_dir, &id),
                InitiativeCommands::Create { name, status } => initiative::run_create(&hex_dir, &name, &status),
                InitiativeCommands::Update { id, status } => initiative::run_update(&hex_dir, &id, &status),
                InitiativeCommands::Close { id } => initiative::run_close(&hex_dir, &id),
            }
        }
        Commands::Learnings { command } => {
            let hex_dir = get_hex_dir();
            match command {
                LearningsCommands::Promote { dry_run } => learnings::run_promote(&hex_dir, dry_run),
            }
        }
        Commands::Telemetry { command } => match command {
            TelemetryCommands::Rotate => {
                let home = std::env::var("HOME").unwrap_or_default();
                let dirs = [
                    std::path::PathBuf::from(&home).join("mrap-hex/.hex/audit"),
                    std::path::PathBuf::from(&home).join("mrap-hex/.hex/logs"),
                ];
                let ttl_days: u64 = 7;
                let cap_bytes: u64 = 50 * 1024 * 1024;
                let mut rotated = 0u64;
                let mut freed_bytes = 0u64;
                let mut cap_truncated = 0u64;
                let now = std::time::SystemTime::now();
                let ttl_secs = ttl_days * 86400;
                for dir in &dirs {
                    if !dir.is_dir() { continue; }
                    let entries: Vec<_> = std::fs::read_dir(dir)
                        .unwrap_or_else(|_| { eprintln!("cannot read {}", dir.display()); std::process::exit(1); })
                        .filter_map(|e| e.ok())
                        .filter(|e| {
                            let name = e.file_name();
                            let n = name.to_string_lossy();
                            e.path().is_file() && (n.ends_with(".jsonl") || n.ends_with(".log"))
                        })
                        .collect();
                    let mut remaining: Vec<_> = entries.iter().filter_map(|e| {
                        let meta = e.path().metadata().ok()?;
                        let modified = meta.modified().ok()?;
                        let age = now.duration_since(modified).ok()?.as_secs();
                        if age > ttl_secs {
                            let sz = meta.len();
                            let _ = std::fs::remove_file(e.path());
                            freed_bytes += sz;
                            rotated += 1;
                            None
                        } else {
                            Some((e.path(), meta.len(), modified))
                        }
                    }).collect();
                    let total: u64 = remaining.iter().map(|(_, sz, _)| sz).sum();
                    if total > cap_bytes {
                        remaining.sort_by_key(|(_, _, m)| *m);
                        let mut running = total;
                        for (path, sz, _) in &remaining {
                            if running <= cap_bytes { break; }
                            let _ = std::fs::remove_file(path);
                            freed_bytes += sz;
                            running -= sz;
                            cap_truncated += 1;
                        }
                    }
                }
                println!("{{\"rotated\":{rotated},\"freed_bytes\":{freed_bytes},\"cap_truncated\":{cap_truncated}}}");
            }
        },
        Commands::Picker => {
            let home = std::env::var("HOME").unwrap_or_default();
            let ctx_json = std::path::PathBuf::from(&home).join("mrap-hex/.hex/contexts.json");
            let contexts_text = if ctx_json.is_file() {
                std::fs::read_to_string(&ctx_json).unwrap_or_else(|_| "{}".to_string())
            } else {
                "{}".to_string()
            };
            let data: serde_json::Value = serde_json::from_str(&contexts_text)
                .unwrap_or_else(|_| serde_json::json!({}));
            let active = data.get("active").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let contexts = data.get("contexts").and_then(|v| v.as_object()).cloned().unwrap_or_default();
            let mut names: Vec<String> = contexts.keys().cloned().collect();
            names.sort();
            if !active.is_empty() && !names.contains(&active) {
                names.insert(0, active.clone());
            } else if let Some(pos) = names.iter().position(|n| n == &active) {
                names.remove(pos);
                names.insert(0, active.clone());
            }
            let lines: Vec<String> = names.iter().map(|name| {
                let marker = if name == &active { "▶" } else { " " };
                format!("{} {}", marker, name)
            }).collect();
            let input = lines.join("\n");
            let mut fzf = std::process::Command::new("fzf");
            fzf.arg("--header=Workspaces  |  Enter=switch")
               .arg("--prompt=  ")
               .arg("--pointer=▶")
               .arg("--height=40%")
               .stdin(std::process::Stdio::piped())
               .stdout(std::process::Stdio::piped());
            let mut child = match fzf.spawn() {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("hex picker: fzf not found or failed to start: {e}");
                    eprintln!("Install fzf: brew install fzf");
                    std::process::exit(1);
                }
            };
            if let Some(stdin) = child.stdin.take() {
                use std::io::Write;
                let mut stdin = stdin;
                let _ = stdin.write_all(input.as_bytes());
            }
            let output = match child.wait_with_output() {
                Ok(o) => o,
                Err(e) => {
                    eprintln!("hex picker: fzf wait failed: {e}");
                    std::process::exit(1);
                }
            };
            if !output.status.success() { std::process::exit(0); }
            let selected = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let context_name = selected.trim_start_matches(['▶', ' ']).trim().to_string();
            if context_name.is_empty() { std::process::exit(0); }
            let hex_dir = get_hex_dir();
            let switch = hex_dir.join(".hex/scripts/hex-context-switch.sh");
            let code = if switch.is_file() {
                std::process::Command::new("bash")
                    .arg(&switch)
                    .arg(&context_name)
                    .status()
                    .map(|s| s.code().unwrap_or(0))
                    .unwrap_or(0)
            } else {
                println!("Selected: {context_name}");
                0
            };
            std::process::exit(code);
        }
        Commands::Upgrade { args } => {
            std::process::exit(upgrade::run(&args));
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
