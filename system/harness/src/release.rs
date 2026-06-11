//! Release engine — repo profiles, gate battery, semver, release notes, the
//! GitFlow cut ceremony, and the git-guard pre-push backend for the typed
//! GitFlow release subsystem (oss-releaser spec, scope items 1 + 3–5).
//!
//! ## Profiles
//!
//! Release behavior is profile-driven. The `hex-foundation` profile is
//! BUILT-IN (code, [`builtin_foundation`]). Additional repos load from
//! `$HEX_DIR/.hex/config/releases.toml` (see
//! `system/templates/releases.toml.example`). A repo that matches no profile
//! is refused with an error listing every known profile — the engine never
//! guesses.
//!
//! ## releases.toml schema
//!
//! ```toml
//! [[profiles]]
//! name = "my-repo"               # display name (required, unique)
//! # Match rule — at least one required. A repo matches if EITHER rule hits:
//! match_remote = "acme/my-repo"  # substring of `git remote get-url origin`
//! match_dir    = "my-repo"       # exact toplevel directory name
//! # Gates run in order via `sh -c` from the repo root; all must pass.
//! gates = [
//!   { name = "check", command = "just check" },
//! ]
//! # Version files bumped by the ceremony. kind: "plain" (whole file is the
//! # version string, default) | "cargo-toml" (rewrites the `version = "…"`
//! # line, preserving the rest of the file).
//! version_files = [
//!   { path = "Cargo.toml", kind = "cargo-toml" },
//! ]
//! build_command  = "cargo build --release"  # optional, run after the bump
//! tag_prefix     = "v"                      # default "v"
//! gh_release     = false                    # default false
//! main_branch    = "main"                   # default "main"
//! develop_branch = "develop"                # default "develop"
//! ```
//!
//! Missing file ⇒ built-in profile only. Malformed file or an invalid
//! profile (no match rule, unknown version-file kind) ⇒ LOUD error (S6 —
//! no quiet failures).
//!
//! ## Gate battery
//!
//! Gates are data ([`GateSpec`]) evaluated to a three-state [`GateResult`]:
//! `Pass`, `Fail(reason)`, or `Skipped(reason)`. `Skipped` never blocks but
//! is always reported loudly. Exit 127 from a child process (and a spawn
//! `NotFound`) means "tool not found" and is reported as such — never
//! conflated with a test failure. The built-in hex-foundation battery ports
//! the legacy `release.sh` gates: clean-tree, docker-e2e (with the pinned
//! doctor carve-out), sanitize (in-process [`crate::sanitize::scan`]),
//! codex-parity, autonomy. The legacy version gate and ahead-of-remote gate
//! are NOT ported — the ceremony's version computation and push steps
//! subsume them.
//!
//! Telemetry note: the battery itself records nothing — the ceremony layer
//! owns `crate::telemetry::record_loud` (one event per gate outcome), so
//! unit tests and dry runs don't write to the events store implicitly.
//!
//! ## The cut ceremony
//!
//! [`cut`] (`hex release cut`) is the single release verb: exclusive lock →
//! preconditions → gate battery (`--dry-run` stops here) → version → the
//! `release/X.Y.Z` (or `hotfix/X.Y.Z`) branch → version bump with
//! build-failure revert → notes → `--no-ff` merge to main + tag → `--no-ff`
//! back-merge to develop → race guard → hardened pushes (every push carries
//! `HEX_RELEASE_PIPELINE=1`) → optional GitHub release → cleanup → summary.
//! Fully non-interactive; exit 0 only on full success.
//!
//! ## git-guard
//!
//! [`git_guard_pre_push`] backs the `.githooks/pre-push` shim: it reads the
//! standard pre-push stdin and blocks any update of `refs/heads/main` unless
//! `HEX_RELEASE_PIPELINE=1` is in the env — the seam that makes `hex release
//! cut` the only push path to main while develop, feature/*, release/*,
//! hotfix/*, and tags pass through.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::str::FromStr;

use anyhow::{bail, Context, Result};
use regex::Regex;
use serde::Deserialize;

// ---------------------------------------------------------------------------
// Gate results — three-state, mandatory.
// ---------------------------------------------------------------------------

/// Outcome of one gate. `Skipped` is non-blocking but always loud.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateResult {
    Pass,
    Fail(String),
    Skipped(String),
}

impl fmt::Display for GateResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GateResult::Pass => write!(f, "PASS"),
            GateResult::Fail(reason) => write!(f, "FAIL — {reason}"),
            GateResult::Skipped(reason) => write!(f, "SKIPPED — {reason}"),
        }
    }
}

/// One named gate plus its outcome, as returned by [`run_battery`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateOutcome {
    pub name: String,
    pub result: GateResult,
}

/// True iff any gate in the battery failed. `Skipped` never blocks.
pub fn battery_blocked(outcomes: &[GateOutcome]) -> bool {
    outcomes
        .iter()
        .any(|o| matches!(o.result, GateResult::Fail(_)))
}

/// One line per gate — the summary the ceremony prints before exiting.
pub fn format_battery_summary(outcomes: &[GateOutcome]) -> String {
    outcomes
        .iter()
        .map(|o| format!("{}: {}", o.name, o.result))
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------------------
// Gates as data.
// ---------------------------------------------------------------------------

/// What a gate does. The typed kinds are the built-in foundation battery;
/// config-loaded profiles express gates as [`GateKind::Command`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateKind {
    /// `git status --porcelain` must be empty.
    CleanTree,
    /// Both Docker suites: build+run `tests/Dockerfile.env`, then
    /// `tests/Dockerfile` with the doctor carve-out. Honors `--skip-e2e`.
    DockerE2e,
    /// In-process [`crate::sanitize::scan`]; passes iff no violations.
    Sanitize,
    /// `bash tests/codex-parity/run-all.sh` if the dir exists. Honors
    /// `--skip-parity`; `--skip-e2e` implies `--skip-parity`.
    CodexParity,
    /// `python3 tests/autonomy/run_autonomy_suite.py --mode structural` if
    /// the dir exists; passes iff exit 0 AND output contains "0 failed".
    Autonomy,
    /// Arbitrary command run via `sh -c` from the repo root; passes iff
    /// exit 0. The seam for config profiles and for test-injected gates.
    Command(String),
}

/// One named gate in a profile's battery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateSpec {
    pub name: String,
    pub kind: GateKind,
}

/// Skip flags threaded from `hex release cut` into the battery.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SkipFlags {
    pub skip_e2e: bool,
    pub skip_parity: bool,
}

// ---------------------------------------------------------------------------
// Version files.
// ---------------------------------------------------------------------------

/// How a version file stores its version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionFileKind {
    /// Whole file content is the version string (e.g. `system/version.txt`).
    Plain,
    /// First `version = "…"` line is rewritten in place; everything else
    /// (comments, other keys) is preserved — the port of the legacy sed.
    CargoToml,
}

/// One repo-relative file the ceremony bumps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionFile {
    pub path: String,
    pub kind: VersionFileKind,
}

impl VersionFile {
    /// Read the current version from this file under `repo_root`.
    pub fn read_version(&self, repo_root: &Path) -> Result<String> {
        let full = repo_root.join(&self.path);
        let body = std::fs::read_to_string(&full)
            .with_context(|| format!("reading version file {}", full.display()))?;
        match self.kind {
            VersionFileKind::Plain => Ok(body.trim().to_string()),
            VersionFileKind::CargoToml => {
                let line = body
                    .lines()
                    .find(|l| l.starts_with("version"))
                    .with_context(|| {
                        format!("no `version = \"…\"` line in {}", full.display())
                    })?;
                line.split('"').nth(1).map(str::to_string).with_context(|| {
                    format!("unquotable version line in {}: {line}", full.display())
                })
            }
        }
    }

    /// Write `new_version` into this file under `repo_root`.
    pub fn write_version(&self, repo_root: &Path, new_version: &str) -> Result<()> {
        let full = repo_root.join(&self.path);
        match self.kind {
            VersionFileKind::Plain => {
                std::fs::write(&full, format!("{new_version}\n"))
                    .with_context(|| format!("writing version file {}", full.display()))
            }
            VersionFileKind::CargoToml => {
                let body = std::fs::read_to_string(&full)
                    .with_context(|| format!("reading version file {}", full.display()))?;
                let mut replaced = false;
                let mut out: Vec<String> = Vec::with_capacity(body.lines().count());
                for line in body.lines() {
                    if !replaced && line.starts_with("version") {
                        out.push(format!("version = \"{new_version}\""));
                        replaced = true;
                    } else {
                        out.push(line.to_string());
                    }
                }
                if !replaced {
                    bail!("no `version = \"…\"` line to bump in {}", full.display());
                }
                std::fs::write(&full, out.join("\n") + "\n")
                    .with_context(|| format!("writing version file {}", full.display()))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Profiles.
// ---------------------------------------------------------------------------

/// Everything the release engine needs to know about one repo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseProfile {
    /// Display name, e.g. `hex-foundation`.
    pub name: String,
    /// Match rule: substring of `git remote get-url origin`.
    pub match_remote: Option<String>,
    /// Match rule: exact toplevel directory name.
    pub match_dir: Option<String>,
    /// Gate battery, run in order.
    pub gates: Vec<GateSpec>,
    /// Files bumped to the new version by the ceremony.
    pub version_files: Vec<VersionFile>,
    /// Optional build command (via `sh -c`) run after the bump so lockfiles
    /// update; a failing build reverts the version files and aborts.
    pub build_command: Option<String>,
    /// Tag prefix, normally `"v"`.
    pub tag_prefix: String,
    /// Create a GitHub release via `gh` after pushing.
    pub gh_release: bool,
    /// The GitFlow mainline (tagged release history).
    pub main_branch: String,
    /// The GitFlow integration branch.
    pub develop_branch: String,
}

impl ReleaseProfile {
    /// True iff this profile matches the given remote URL / toplevel dir.
    fn matches(&self, remote_url: Option<&str>, dir_name: &str) -> bool {
        if let (Some(sub), Some(url)) = (self.match_remote.as_deref(), remote_url) {
            if url.contains(sub) {
                return true;
            }
        }
        self.match_dir.as_deref() == Some(dir_name)
    }
}

/// The built-in hex-foundation profile. Matches both the canonical checkout
/// dir and any worktree of it (worktree dirs differ, the remote does not).
pub fn builtin_foundation() -> ReleaseProfile {
    ReleaseProfile {
        name: "hex-foundation".to_string(),
        match_remote: Some("hex-foundation".to_string()),
        match_dir: Some("hex-foundation".to_string()),
        gates: vec![
            GateSpec { name: "clean-tree".to_string(), kind: GateKind::CleanTree },
            GateSpec { name: "docker-e2e".to_string(), kind: GateKind::DockerE2e },
            GateSpec { name: "sanitize".to_string(), kind: GateKind::Sanitize },
            GateSpec { name: "codex-parity".to_string(), kind: GateKind::CodexParity },
            GateSpec { name: "autonomy".to_string(), kind: GateKind::Autonomy },
        ],
        version_files: vec![
            VersionFile {
                path: "system/harness/Cargo.toml".to_string(),
                kind: VersionFileKind::CargoToml,
            },
            VersionFile {
                path: "system/version.txt".to_string(),
                kind: VersionFileKind::Plain,
            },
        ],
        build_command: Some("cargo build --release -p hex-harness".to_string()),
        tag_prefix: "v".to_string(),
        gh_release: true,
        main_branch: "main".to_string(),
        develop_branch: "develop".to_string(),
    }
}

// -- releases.toml loading ---------------------------------------------------

#[derive(Debug, Deserialize, Default)]
struct ReleasesTomlFile {
    #[serde(default)]
    profiles: Vec<ProfileToml>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileToml {
    name: String,
    #[serde(default)]
    match_remote: Option<String>,
    #[serde(default)]
    match_dir: Option<String>,
    #[serde(default)]
    gates: Vec<GateToml>,
    #[serde(default)]
    version_files: Vec<VersionFileToml>,
    #[serde(default)]
    build_command: Option<String>,
    #[serde(default)]
    tag_prefix: Option<String>,
    #[serde(default)]
    gh_release: Option<bool>,
    #[serde(default)]
    main_branch: Option<String>,
    #[serde(default)]
    develop_branch: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GateToml {
    name: String,
    command: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VersionFileToml {
    path: String,
    #[serde(default)]
    kind: Option<String>,
}

impl ProfileToml {
    fn into_profile(self) -> Result<ReleaseProfile> {
        if self.match_remote.is_none() && self.match_dir.is_none() {
            bail!(
                "profile `{}` in releases.toml needs at least one match rule \
                 (`match_remote` or `match_dir`)",
                self.name
            );
        }
        let version_files = self
            .version_files
            .into_iter()
            .map(|vf| {
                let kind = match vf.kind.as_deref() {
                    None | Some("plain") => VersionFileKind::Plain,
                    Some("cargo-toml") => VersionFileKind::CargoToml,
                    Some(other) => bail!(
                        "profile `{}`: unknown version-file kind `{other}` for `{}` \
                         — valid kinds: plain, cargo-toml",
                        self.name,
                        vf.path
                    ),
                };
                Ok(VersionFile { path: vf.path, kind })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(ReleaseProfile {
            name: self.name,
            match_remote: self.match_remote,
            match_dir: self.match_dir,
            gates: self
                .gates
                .into_iter()
                .map(|g| GateSpec { name: g.name, kind: GateKind::Command(g.command) })
                .collect(),
            version_files,
            build_command: self.build_command,
            tag_prefix: self.tag_prefix.unwrap_or_else(|| "v".to_string()),
            gh_release: self.gh_release.unwrap_or(false),
            main_branch: self.main_branch.unwrap_or_else(|| "main".to_string()),
            develop_branch: self.develop_branch.unwrap_or_else(|| "develop".to_string()),
        })
    }
}

/// `$HEX_DIR/.hex/config/releases.toml` (same resolution as llm.toml).
fn config_path() -> PathBuf {
    let hex_dir = std::env::var("HEX_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("hex")
        });
    hex_dir.join(".hex/config/releases.toml")
}

/// Load profiles from an explicit releases.toml path. Missing file ⇒ empty
/// (built-ins only); malformed file or invalid profile ⇒ loud error.
fn load_profiles_file(path: &Path) -> Result<Vec<ReleaseProfile>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let body = std::fs::read_to_string(path)
        .with_context(|| format!("reading releases.toml at {}", path.display()))?;
    let parsed: ReleasesTomlFile = toml::from_str(&body)
        .with_context(|| format!("parsing releases.toml at {}", path.display()))?;
    parsed
        .profiles
        .into_iter()
        .map(|p| p.into_profile())
        .collect::<Result<Vec<_>>>()
        .with_context(|| format!("loading releases.toml at {}", path.display()))
}

/// All known profiles: built-in hex-foundation first, then releases.toml in
/// file order. First match wins in [`resolve_profile`].
pub fn known_profiles() -> Result<Vec<ReleaseProfile>> {
    let mut profiles = vec![builtin_foundation()];
    profiles.extend(load_profiles_file(&config_path())?);
    Ok(profiles)
}

fn match_profile<'a>(
    profiles: &'a [ReleaseProfile],
    remote_url: Option<&str>,
    dir_name: &str,
) -> Option<&'a ReleaseProfile> {
    profiles.iter().find(|p| p.matches(remote_url, dir_name))
}

/// The refusal message for a repo no profile matches — names what was
/// inspected and lists every known profile so the fix is obvious.
fn refusal_message(
    profiles: &[ReleaseProfile],
    remote_url: Option<&str>,
    dir_name: &str,
) -> String {
    let names: Vec<&str> = profiles.iter().map(|p| p.name.as_str()).collect();
    format!(
        "no release profile matches this repo (dir `{dir_name}`, remote {}) — \
         known profiles: {}. Add a [[profiles]] entry to \
         $HEX_DIR/.hex/config/releases.toml (see \
         system/templates/releases.toml.example).",
        remote_url
            .map(|u| format!("`{u}`"))
            .unwrap_or_else(|| "<none>".to_string()),
        names.join(", "),
    )
}

/// Resolve the release profile for the repo at `repo_root` (its git
/// toplevel). Unknown repos are refused with a clear error — never guessed.
pub fn resolve_profile(repo_root: &Path) -> Result<ReleaseProfile> {
    let profiles = known_profiles()?;
    let remote_url = git_stdout(repo_root, &["remote", "get-url", "origin"])
        .ok()
        .map(|s| s.trim().to_string());
    let dir_name = repo_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();
    match_profile(&profiles, remote_url.as_deref(), &dir_name)
        .cloned()
        .ok_or_else(|| {
            anyhow::anyhow!(refusal_message(&profiles, remote_url.as_deref(), &dir_name))
        })
}

// ---------------------------------------------------------------------------
// Child processes — exit 127 / spawn-NotFound = "tool not found", always
// reported as such, never conflated with a test failure.
// ---------------------------------------------------------------------------

struct RunResult {
    code: i32,
    stdout: String,
    stderr: String,
}

impl RunResult {
    /// stdout followed by stderr — the port of the bash `2>&1` captures.
    fn combined(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }
}

/// Run a child to completion, capturing output. `display` names the tool in
/// failure messages (for `sh -c` gates: the configured command, not `sh`).
/// `Err` is a complete, gate-ready failure description.
fn run_checked(display: &str, cmd: &mut Command) -> Result<RunResult, String> {
    cmd.stdin(Stdio::null());
    let out = match cmd.output() {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!("`{display}`: tool not found"));
        }
        Err(e) => return Err(format!("failed to run `{display}`: {e}")),
        Ok(out) => out,
    };
    match out.status.code() {
        None => Err(format!("`{display}`: terminated by signal")),
        Some(127) => Err(format!("`{display}`: tool not found (exit 127)")),
        Some(code) => Ok(RunResult {
            code,
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }),
    }
}

/// Run `git <args>` in `repo_root`, returning stdout; non-zero exit is an
/// error carrying git's stderr.
fn git_stdout(repo_root: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo_root)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("running git {}", args.join(" ")))?;
    if !out.status.success() {
        bail!(
            "git {} failed (exit {}): {}",
            args.join(" "),
            out.status.code().map_or("?".to_string(), |c| c.to_string()),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

// ---------------------------------------------------------------------------
// Gate implementations.
// ---------------------------------------------------------------------------

/// Evaluate one gate against the repo at `repo_root`.
pub fn run_gate(gate: &GateSpec, repo_root: &Path, skip: SkipFlags) -> GateResult {
    match &gate.kind {
        GateKind::CleanTree => gate_clean_tree(repo_root),
        GateKind::DockerE2e => gate_docker_e2e(repo_root, skip),
        GateKind::Sanitize => gate_sanitize(repo_root),
        GateKind::CodexParity => gate_codex_parity(repo_root, skip),
        GateKind::Autonomy => gate_autonomy(repo_root),
        GateKind::Command(cmd) => gate_command(cmd, repo_root),
    }
}

/// Run a profile's full battery in order. Every gate runs (no
/// short-circuit, like the legacy pipeline) so the operator sees the whole
/// picture; the ceremony exits 1 if [`battery_blocked`].
pub fn run_battery(
    profile: &ReleaseProfile,
    repo_root: &Path,
    skip: SkipFlags,
) -> Vec<GateOutcome> {
    profile
        .gates
        .iter()
        .map(|gate| {
            bold(&format!("Gate: {}", gate.name));
            let result = run_gate(gate, repo_root, skip);
            match &result {
                GateResult::Pass => green(&format!("  {} ✓", gate.name)),
                GateResult::Fail(reason) => red(&format!("  GATE FAIL: {}: {reason}", gate.name)),
                GateResult::Skipped(reason) => red(&format!("  SKIPPED: {reason}")),
            }
            GateOutcome { name: gate.name.clone(), result }
        })
        .collect()
}

fn gate_clean_tree(repo_root: &Path) -> GateResult {
    let mut cmd = Command::new("git");
    cmd.args(["status", "--porcelain"]).current_dir(repo_root);
    let r = match run_checked("git", &mut cmd) {
        Ok(r) => r,
        Err(msg) => return GateResult::Fail(msg),
    };
    if r.code != 0 {
        return GateResult::Fail(format!(
            "git status failed (exit {}): {}",
            r.code,
            r.stderr.trim()
        ));
    }
    if r.stdout.trim().is_empty() {
        GateResult::Pass
    } else {
        GateResult::Fail("uncommitted changes — commit first".to_string())
    }
}

/// One Docker suite: build, then run, capturing combined output from that
/// single run (the legacy script re-ran the container to inspect output —
/// this port does not). The doctor carve-out applies ONLY to the regression
/// suite (`carveout = true`) and ONLY when `docker run` exits nonzero —
/// NEVER on a build failure.
fn docker_suite(
    repo_root: &Path,
    dockerfile: &str,
    tag: &str,
    label: &str,
    carveout: bool,
) -> GateResult {
    println!("  Running {label}...");
    let mut build = Command::new("docker");
    build
        .args(["build", "-f", dockerfile, "-t", tag, "."])
        .current_dir(repo_root);
    let b = match run_checked("docker", &mut build) {
        Ok(r) => r,
        Err(msg) => return GateResult::Fail(msg),
    };
    if b.code != 0 {
        return GateResult::Fail(format!(
            "docker E2E build failed (exit {}): {label}",
            b.code
        ));
    }
    let mut run = Command::new("docker");
    run.args(["run", "--rm", tag]).current_dir(repo_root);
    let r = match run_checked("docker", &mut run) {
        Ok(r) => r,
        Err(msg) => return GateResult::Fail(msg),
    };
    if r.code == 0 {
        return GateResult::Pass;
    }
    if carveout {
        doctor_carveout(&r.combined())
    } else {
        GateResult::Fail(format!("{label} failed (exit {})", r.code))
    }
}

/// The pinned doctor carve-out: a Doctor failure inside Docker is expected
/// (no runtime binary in the container). Pass iff the combined output has
/// <= 1 line containing `FAIL:` AND matches `FAIL.*Doctor` (case-sensitive).
fn doctor_carveout(combined: &str) -> GateResult {
    let fail_count = combined.lines().filter(|l| l.contains("FAIL:")).count();
    let doctor_re = Regex::new("FAIL.*Doctor").expect("static regex must compile");
    if fail_count <= 1 && doctor_re.is_match(combined) {
        println!("  regression suite: PASS (doctor skip expected in Docker)");
        GateResult::Pass
    } else {
        GateResult::Fail(format!("regression suite failed ({fail_count} failures)"))
    }
}

fn gate_docker_e2e(repo_root: &Path, skip: SkipFlags) -> GateResult {
    if skip.skip_e2e {
        return GateResult::Skipped("--skip-e2e — emergency bypass".to_string());
    }
    let env = docker_suite(
        repo_root,
        "tests/Dockerfile.env",
        "hex-env-test",
        "env resolution tests",
        false,
    );
    if env != GateResult::Pass {
        return env;
    }
    docker_suite(
        repo_root,
        "tests/Dockerfile",
        "hex-e2e-test",
        "regression suite",
        true,
    )
}

fn gate_sanitize(repo_root: &Path) -> GateResult {
    match crate::sanitize::scan(repo_root, false) {
        Err(e) => GateResult::Fail(format!("sanitize scan error: {e:#}")),
        Ok(v) if v.is_empty() => GateResult::Pass,
        Ok(v) => GateResult::Fail(format!(
            "{} personalization violation(s) — run `hex sanitize --verbose` for details",
            v.len()
        )),
    }
}

fn gate_codex_parity(repo_root: &Path, skip: SkipFlags) -> GateResult {
    if skip.skip_parity {
        return GateResult::Skipped("--skip-parity — emergency bypass".to_string());
    }
    if skip.skip_e2e {
        return GateResult::Skipped("--skip-e2e implies --skip-parity".to_string());
    }
    if !repo_root.join("tests/codex-parity").is_dir() {
        return GateResult::Skipped(
            "codex parity tests not found at tests/codex-parity — skipping".to_string(),
        );
    }
    println!("  Running codex parity suite (tests/codex-parity/)...");
    let mut cmd = Command::new("bash");
    cmd.arg("tests/codex-parity/run-all.sh")
        .current_dir(repo_root)
        // Port of `export OPENAI_API_KEY="${OPENAI_API_KEY:-}"` — the suite
        // decides whether to run live tests or SKIP based on this.
        .env(
            "OPENAI_API_KEY",
            std::env::var("OPENAI_API_KEY").unwrap_or_default(),
        );
    match run_checked("bash tests/codex-parity/run-all.sh", &mut cmd) {
        Err(msg) => GateResult::Fail(msg),
        Ok(r) if r.code == 0 => GateResult::Pass,
        Ok(r) => GateResult::Fail(format!("codex parity failure (exit {})", r.code)),
    }
}

fn gate_autonomy(repo_root: &Path) -> GateResult {
    if !repo_root.join("tests/autonomy").is_dir() {
        return GateResult::Skipped(
            "autonomy tests not found at tests/autonomy — skipping".to_string(),
        );
    }
    println!("  Running mechanism routing tests...");
    let mut cmd = Command::new("python3");
    cmd.args(["tests/autonomy/run_autonomy_suite.py", "--mode", "structural"])
        .current_dir(repo_root);
    let r = match run_checked("python3", &mut cmd) {
        Ok(r) => r,
        Err(msg) => return GateResult::Fail(msg),
    };
    // Pinned: exit code AND the captured "0 failed" marker — never judged
    // through a tail/grep pipe like the legacy script.
    if r.code == 0 && r.combined().contains("0 failed") {
        GateResult::Pass
    } else {
        GateResult::Fail(format!(
            "autonomy regression failed (exit {}) — mechanism routing errors detected",
            r.code
        ))
    }
}

fn gate_command(command: &str, repo_root: &Path) -> GateResult {
    let mut cmd = Command::new("sh");
    cmd.args(["-c", command]).current_dir(repo_root);
    match run_checked(command, &mut cmd) {
        Err(msg) => GateResult::Fail(msg),
        Ok(r) if r.code == 0 => GateResult::Pass,
        Ok(r) => GateResult::Fail(format!("`{command}` failed (exit {})", r.code)),
    }
}

// ---------------------------------------------------------------------------
// Semver — port of release.sh `semver_valid` / `semver_gt` plus the
// next-patch suggestion (the legacy awk one-liner), in Rust.
// ---------------------------------------------------------------------------

/// Parse strict `X.Y.Z` (digits only — no `v` prefix, no pre-release).
pub fn parse_semver(v: &str) -> Option<(u64, u64, u64)> {
    let mut parts = v.split('.');
    let (a, b, c) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let num = |s: &str| -> Option<u64> {
        // Reject empty and any non-digit (u64::from_str would accept "+1").
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        s.parse().ok()
    };
    Some((num(a)?, num(b)?, num(c)?))
}

/// Port of `semver_valid`: `^[0-9]+\.[0-9]+\.[0-9]+$`.
pub fn semver_valid(v: &str) -> bool {
    parse_semver(v).is_some()
}

/// Port of `semver_gt`: true iff `a` is strictly greater than `b`.
/// Unparseable input is never greater (callers validate first).
pub fn semver_gt(a: &str, b: &str) -> bool {
    match (parse_semver(a), parse_semver(b)) {
        (Some(pa), Some(pb)) => pa > pb,
        _ => false,
    }
}

/// The legacy next-patch suggestion (`awk -F. '{print $1"."$2"."$3+1}'`).
pub fn next_patch_suggestion(latest: &str) -> Option<String> {
    let (maj, min, pat) = parse_semver(latest)?;
    Some(format!("{maj}.{min}.{}", pat + 1))
}

/// Bump tier for `hex release cut --level`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BumpLevel {
    Patch,
    Minor,
    Major,
}

impl FromStr for BumpLevel {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "patch" => Ok(BumpLevel::Patch),
            "minor" => Ok(BumpLevel::Minor),
            "major" => Ok(BumpLevel::Major),
            other => bail!("invalid --level `{other}` — valid levels: patch, minor, major"),
        }
    }
}

/// Compute the `--level` bump of `latest`.
pub fn bump_version(latest: &str, level: BumpLevel) -> Option<String> {
    let (maj, min, pat) = parse_semver(latest)?;
    Some(match level {
        BumpLevel::Patch => format!("{maj}.{min}.{}", pat + 1),
        BumpLevel::Minor => format!("{maj}.{}.0", min + 1),
        BumpLevel::Major => format!("{}.0.0", maj + 1),
    })
}

/// Highest semver among `tags` (prefix stripped; non-semver tags ignored).
/// Semver ordering, not lexicographic — `v0.10.0` beats `v0.9.0`. Returns
/// the bare version string.
pub fn highest_semver_tag(tags: &[String], prefix: &str) -> Option<String> {
    tags.iter()
        .filter_map(|t| {
            let bare = t.strip_prefix(prefix)?;
            parse_semver(bare).map(|p| (p, bare))
        })
        .max_by_key(|(p, _)| *p)
        .map(|(_, bare)| bare.to_string())
}

/// Highest semver among `tags` strictly below `below` — the "prev" anchor
/// for release notes. Returns the bare version string.
pub fn highest_semver_below(tags: &[String], prefix: &str, below: &str) -> Option<String> {
    let limit = parse_semver(below)?;
    tags.iter()
        .filter_map(|t| {
            let bare = t.strip_prefix(prefix)?;
            parse_semver(bare).map(|p| (p, bare))
        })
        .filter(|(p, _)| *p < limit)
        .max_by_key(|(p, _)| *p)
        .map(|(_, bare)| bare.to_string())
}

// ---------------------------------------------------------------------------
// Release notes — `git log <prev>..HEAD` grouped by conventional-commit
// prefix; prev = highest semver tag strictly below the new version, full
// history if none exists (first release).
// ---------------------------------------------------------------------------

/// Group order and headings for the notes. `other` collects everything that
/// doesn't parse as a conventional commit.
const NOTE_GROUPS: &[(&str, &str)] = &[
    ("feat", "Features"),
    ("fix", "Fixes"),
    ("docs", "Docs"),
    ("chore", "Chores"),
    ("refactor", "Refactoring"),
    ("test", "Tests"),
    ("other", "Other"),
];

/// Classify one commit subject into a conventional-commit group key.
/// Accepts an optional scope and breaking-change `!` (`feat(x)!: …`).
pub fn classify_subject(subject: &str) -> &'static str {
    let re = Regex::new(r"^(feat|fix|docs|chore|refactor|test)(\([^)]*\))?!?:")
        .expect("static regex must compile");
    match re.captures(subject) {
        Some(c) => match c.get(1).map(|m| m.as_str()) {
            Some("feat") => "feat",
            Some("fix") => "fix",
            Some("docs") => "docs",
            Some("chore") => "chore",
            Some("refactor") => "refactor",
            Some("test") => "test",
            _ => "other",
        },
        None => "other",
    }
}

/// Render the notes for `new_version` from raw commit subjects. `prev` is
/// the previous release version (bare), `None` for a first release.
pub fn format_release_notes(
    tag_prefix: &str,
    new_version: &str,
    prev: Option<&str>,
    subjects: &[String],
) -> String {
    let mut out = format!("## {tag_prefix}{new_version}\n\n");
    match prev {
        Some(p) => out.push_str(&format!("Changes since {tag_prefix}{p}.\n")),
        None => out.push_str("First release — notes cover the full history.\n"),
    }
    if subjects.is_empty() {
        out.push_str("\n_No commits in range._\n");
        return out;
    }
    for (key, heading) in NOTE_GROUPS {
        let in_group: Vec<&String> = subjects
            .iter()
            .filter(|s| classify_subject(s) == *key)
            .collect();
        if in_group.is_empty() {
            continue;
        }
        out.push_str(&format!("\n### {heading}\n"));
        for subject in in_group {
            out.push_str(&format!("- {subject}\n"));
        }
    }
    out
}

/// Generate release notes for `new_version` from the repo at `repo_root`:
/// `git log <prev>..HEAD` where prev = highest semver tag strictly below
/// `new_version`; full history if no such tag exists.
pub fn generate_release_notes(
    repo_root: &Path,
    tag_prefix: &str,
    new_version: &str,
) -> Result<String> {
    let tags: Vec<String> = git_stdout(repo_root, &["tag"])?
        .lines()
        .map(str::to_string)
        .collect();
    let prev = highest_semver_below(&tags, tag_prefix, new_version);
    let range = match &prev {
        Some(p) => format!("{tag_prefix}{p}..HEAD"),
        None => "HEAD".to_string(),
    };
    let subjects: Vec<String> = git_stdout(
        repo_root,
        &["log", "--no-merges", "--pretty=format:%s", &range],
    )?
    .lines()
    .map(str::to_string)
    .collect();
    Ok(format_release_notes(
        tag_prefix,
        new_version,
        prev.as_deref(),
        &subjects,
    ))
}

// ---------------------------------------------------------------------------
// Release lock — ceremony step (a). An exclusive O_EXCL lockfile
// (`hex-release.lock`) in the repo's shared git dir, so concurrent cuts —
// including from different worktrees of the same repo — serialize. Released
// on EVERY exit path (success, error, panic unwind) via Drop.
// ---------------------------------------------------------------------------

/// Env var that authorizes a push from the release pipeline; the pre-push
/// guard ([`git_guard_pre_push`]) blocks `main` pushes without it.
pub const RELEASE_PIPELINE_ENV: &str = "HEX_RELEASE_PIPELINE";

/// Held for the duration of a cut. Dropping releases the lock.
#[derive(Debug)]
pub struct ReleaseLock {
    path: PathBuf,
}

impl ReleaseLock {
    /// Take the exclusive release lock for the repo at `repo_root`. If the
    /// lock is held, errors with one clear message naming the in-flight
    /// release — never queues silently or runs concurrently.
    pub fn acquire(repo_root: &Path) -> Result<Self> {
        let path = lock_file_path(repo_root)?;
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut f) => {
                use std::io::Write;
                // Best-effort holder info — the file's existence is the lock.
                let _ = writeln!(
                    f,
                    "pid={} started={}",
                    std::process::id(),
                    chrono::Utc::now().to_rfc3339()
                );
                Ok(ReleaseLock { path })
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let holder = std::fs::read_to_string(&path)
                    .map(|s| s.trim().to_string())
                    .unwrap_or_else(|_| "<unreadable>".to_string());
                bail!(
                    "a release is already in flight ({holder}) — lock held at {}. \
                     If that release is dead, remove the lockfile and re-run.",
                    path.display()
                );
            }
            Err(e) => {
                Err(e).with_context(|| format!("creating release lock {}", path.display()))
            }
        }
    }
}

impl Drop for ReleaseLock {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_file(&self.path) {
            // Loud, never fatal: a stale lock blocks the NEXT release, so say so.
            eprintln!(
                "WARN: failed to remove release lock {} ({e}) — remove it by hand \
                 before the next release",
                self.path.display()
            );
        }
    }
}

/// The repo's shared git dir (`--git-common-dir`): worktrees of one repo
/// share it, so the lock serializes across them too.
fn git_common_dir(repo_root: &Path) -> Result<PathBuf> {
    let raw = git_stdout(repo_root, &["rev-parse", "--git-common-dir"])?;
    let raw = raw.trim();
    if Path::new(raw).is_absolute() {
        Ok(PathBuf::from(raw))
    } else {
        Ok(repo_root.join(raw))
    }
}

fn lock_file_path(repo_root: &Path) -> Result<PathBuf> {
    Ok(git_common_dir(repo_root)?.join("hex-release.lock"))
}

// ---------------------------------------------------------------------------
// Ceremony git helpers — every git operation is a typed std::process::Command
// call; no shell pipelines.
// ---------------------------------------------------------------------------

fn rev_parse(repo_root: &Path, refname: &str) -> Result<String> {
    Ok(git_stdout(repo_root, &["rev-parse", refname])?.trim().to_string())
}

fn short_sha(sha: &str) -> &str {
    &sha[..sha.len().min(8)]
}

fn ref_exists(repo_root: &Path, refname: &str) -> bool {
    Command::new("git")
        .args(["rev-parse", "--verify", "--quiet", refname])
        .current_dir(repo_root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// True iff `ancestor` is an ancestor of `descendant` — exit 0/1 are the
/// yes/no answers, anything else is a real error.
fn is_ancestor(repo_root: &Path, ancestor: &str, descendant: &str) -> Result<bool> {
    let mut cmd = Command::new("git");
    cmd.args(["merge-base", "--is-ancestor", ancestor, descendant])
        .current_dir(repo_root);
    match run_checked("git merge-base --is-ancestor", &mut cmd) {
        Err(msg) => bail!("{msg}"),
        Ok(r) if r.code == 0 => Ok(true),
        Ok(r) if r.code == 1 => Ok(false),
        Ok(r) => bail!(
            "git merge-base --is-ancestor failed (exit {}): {}",
            r.code,
            r.stderr.trim()
        ),
    }
}

/// SHA of `refname` on origin, `None` if absent. A failing `ls-remote` is a
/// hard error — "cannot verify" must never read as "absent".
fn ls_remote_sha(repo_root: &Path, refname: &str) -> Result<Option<String>> {
    let out = git_stdout(repo_root, &["ls-remote", "origin", refname])
        .with_context(|| format!("checking origin for {refname}"))?;
    Ok(out.split_whitespace().next().map(str::to_string))
}

/// `git merge --no-ff` of `branch` into the current branch. `Err` carries
/// the failure output; the caller owns the abort semantics.
fn merge_no_ff(repo_root: &Path, branch: &str, message: &str) -> Result<(), String> {
    let mut cmd = Command::new("git");
    cmd.args(["merge", "--no-ff", "-m", message, branch])
        .current_dir(repo_root);
    match run_checked("git merge --no-ff", &mut cmd) {
        Err(msg) => Err(msg),
        Ok(r) if r.code == 0 => Ok(()),
        Ok(r) => Err(format!("exit {}: {}", r.code, r.combined().trim())),
    }
}

/// Abandon an in-progress merge, best-effort (there may be none).
fn abort_merge(repo_root: &Path) {
    let _ = Command::new("git")
        .args(["merge", "--abort"])
        .current_dir(repo_root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// One hardened push. EVERY push — branches AND the tag — carries
/// `HEX_RELEASE_PIPELINE=1` in the child env (omitting it on the tag would
/// ship a pipeline whose own tag push is blocked by its own pre-push guard).
/// Non-zero exit = hard abort.
fn push_ref(repo_root: &Path, refspec: &str) -> Result<()> {
    println!("  Pushing {refspec}...");
    let mut cmd = Command::new("git");
    cmd.args(["push", "origin", refspec])
        .current_dir(repo_root)
        .env(RELEASE_PIPELINE_ENV, "1");
    match run_checked(&format!("git push origin {refspec}"), &mut cmd) {
        Err(msg) => bail!("push of {refspec} failed: {msg}"),
        Ok(r) if r.code == 0 => Ok(()),
        Ok(r) => bail!(
            "PUSH REJECTED for {refspec} (exit {}): {} — release ABORTED. \
             Common causes: remote moved ahead, or a pre-push hook blocked it.",
            r.code,
            r.stderr.trim()
        ),
    }
}

/// Independent post-push verify: origin's branch SHA must equal the local
/// one (a 0 exit from `git push` is not proof on its own).
fn verify_pushed(repo_root: &Path, branch: &str, expected_sha: &str) -> Result<()> {
    match ls_remote_sha(repo_root, &format!("refs/heads/{branch}"))?.as_deref() {
        Some(sha) if sha == expected_sha => Ok(()),
        other => bail!(
            "SHA MISMATCH after push: origin/{branch} is {} but expected \
             {expected_sha} — release ABORTED",
            other.unwrap_or("<absent>")
        ),
    }
}

// ---------------------------------------------------------------------------
// Pure ceremony decisions — version computation, race guard, tag-push action.
// ---------------------------------------------------------------------------

/// Ceremony step (d): the next version from `--version` / `--level`.
/// `latest` is the highest existing semver tag (bare version). Refuses
/// anything not strictly greater than `latest`.
pub fn compute_next_version(
    explicit: Option<&str>,
    level: Option<BumpLevel>,
    latest: Option<&str>,
) -> Result<String> {
    let next = match explicit {
        Some(v) => {
            if !semver_valid(v) {
                bail!("--version `{v}` is not semver (expected X.Y.Z)");
            }
            v.to_string()
        }
        None => {
            let base = latest.ok_or_else(|| {
                anyhow::anyhow!(
                    "no semver tags to bump — pass an explicit --version X.Y.Z for a \
                     first release"
                )
            })?;
            let level = level.unwrap_or(BumpLevel::Patch);
            bump_version(base, level).ok_or_else(|| {
                anyhow::anyhow!("latest tag `{base}` is not semver — pass --version X.Y.Z")
            })?
        }
    };
    if let Some(latest) = latest {
        if !semver_gt(&next, latest) {
            let hint = next_patch_suggestion(latest)
                .map(|s| format!(" (next patch: {s})"))
                .unwrap_or_default();
            bail!("version {next} is not greater than the latest tag {latest}{hint}");
        }
    }
    Ok(next)
}

/// Ceremony step (j): the pinned branch must not have moved during the cut.
pub fn check_pinned_unmoved(branch: &str, pinned_sha: &str, current_sha: &str) -> Result<()> {
    if pinned_sha == current_sha {
        return Ok(());
    }
    bail!(
        "RACE: branch `{branch}` moved during the cut (pinned {}, now {}) — aborting \
         before any push. Nothing was pushed; re-run `hex release cut` to release the \
         new tip.",
        short_sha(pinned_sha),
        short_sha(current_sha)
    )
}

/// What to do about the tag on origin at push time (step k).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagPushAction {
    /// Tag absent on origin — push it.
    Push,
    /// Tag already on origin at the same commit — idempotent skip.
    SkipAlreadyOnOrigin,
    /// Tag on origin points elsewhere — refuse, never overwrite.
    RefuseDivergent { remote_sha: String },
}

pub fn tag_push_action(local_sha: &str, remote_sha: Option<&str>) -> TagPushAction {
    match remote_sha {
        None => TagPushAction::Push,
        Some(sha) if sha == local_sha => TagPushAction::SkipAlreadyOnOrigin,
        Some(sha) => TagPushAction::RefuseDivergent { remote_sha: sha.to_string() },
    }
}

/// Step (d) refusals: the tag must not already exist locally or on origin.
fn refuse_existing_tag(repo_root: &Path, tag: &str) -> Result<()> {
    if ref_exists(repo_root, &format!("refs/tags/{tag}")) {
        bail!("tag {tag} already exists locally — refusing to reuse it");
    }
    if let Some(sha) = ls_remote_sha(repo_root, &format!("refs/tags/{tag}"))? {
        bail!(
            "tag {tag} already exists on origin (at {}) — refusing to reuse it",
            short_sha(&sha)
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The GitFlow cut ceremony — `hex release cut` (spec steps a–m).
// ---------------------------------------------------------------------------

/// Flags for `hex release cut`. `version` wins over `level`; `level`
/// defaults to patch.
#[derive(Debug, Clone, Default)]
pub struct CutOptions {
    pub level: Option<BumpLevel>,
    pub version: Option<String>,
    pub hotfix: bool,
    pub dry_run: bool,
    pub skip: SkipFlags,
}

/// Cut a release from the repo containing the current directory. Resolves
/// the profile, then runs the full ceremony. `Err` = aborted (exit 1).
pub fn cut(opts: &CutOptions) -> Result<()> {
    let cwd = std::env::current_dir().context("resolving current dir")?;
    let repo_root = PathBuf::from(
        git_stdout(&cwd, &["rev-parse", "--show-toplevel"])
            .context("not inside a git repository")?
            .trim(),
    );
    let profile = resolve_profile(&repo_root)?;
    cut_with_profile(&repo_root, &profile, opts)
}

/// The ceremony with an injected profile — the seam unit and integration
/// tests use (toy profiles against temp repos). Owns the pipeline telemetry:
/// one `record_loud` per gate outcome (inside the run) plus one per
/// completion/abort here.
pub fn cut_with_profile(
    repo_root: &Path,
    profile: &ReleaseProfile,
    opts: &CutOptions,
) -> Result<()> {
    let result = cut_ceremony(repo_root, profile, opts);
    let (status, detail) = match &result {
        Ok(detail) => ("ok", detail.clone()),
        Err(e) => ("error", format!("{e:#}")),
    };
    crate::telemetry::record_loud(&crate::telemetry::TelemetryEvent {
        source: "oss-releaser".to_string(),
        event: "release::cut".to_string(),
        status: status.to_string(),
        duration_ms: None,
        exit_code: None,
        detail: Some(detail),
    });
    result.map(|_| ())
}

/// One telemetry event per gate outcome (spec step m).
fn record_gate_outcomes(outcomes: &[GateOutcome]) {
    for o in outcomes {
        let (status, detail) = match &o.result {
            GateResult::Pass => ("ok", None),
            GateResult::Fail(reason) => ("error", Some(reason.clone())),
            GateResult::Skipped(reason) => ("skipped", Some(reason.clone())),
        };
        crate::telemetry::record_loud(&crate::telemetry::TelemetryEvent {
            source: "oss-releaser".to_string(),
            event: format!("release::gate::{}", o.name),
            status: status.to_string(),
            duration_ms: None,
            exit_code: None,
            detail,
        });
    }
}

/// The full ceremony. Returns the completion detail for telemetry; any `Err`
/// is an abort (the lock is still released via Drop).
fn cut_ceremony(repo_root: &Path, profile: &ReleaseProfile, opts: &CutOptions) -> Result<String> {
    let mut phases: Vec<(&'static str, String)> = Vec::new();
    let main = profile.main_branch.as_str();
    let develop = profile.develop_branch.as_str();

    // (a) Exclusive lock — Drop releases it on every exit path.
    let _lock = ReleaseLock::acquire(repo_root)?;
    phases.push(("lock", "acquired".to_string()));

    // (b) Preconditions: clean tree, both GitFlow branches, pinned SHA,
    // main-ancestor-of-develop invariant.
    let dirty = git_stdout(repo_root, &["status", "--porcelain"])?;
    if !dirty.trim().is_empty() {
        bail!(
            "working tree is not clean — commit or stash first:\n{}",
            dirty.trim_end()
        );
    }
    if !ref_exists(repo_root, &format!("refs/heads/{main}")) {
        bail!("branch `{main}` does not exist — the GitFlow mainline is required");
    }
    if !ref_exists(repo_root, &format!("refs/heads/{develop}")) {
        bail!(
            "branch `{develop}` does not exist — bootstrap GitFlow first:\n  \
             git branch {develop} {main} && git push origin {develop}"
        );
    }
    let pinned_branch = if opts.hotfix { main } else { develop };
    let pinned_sha = rev_parse(repo_root, &format!("refs/heads/{pinned_branch}"))?;
    if !is_ancestor(
        repo_root,
        &format!("refs/heads/{main}"),
        &format!("refs/heads/{develop}"),
    )? {
        bail!(
            "`{main}` is not an ancestor of `{develop}` — the GitFlow invariant is \
             broken (did someone commit to {main} without a back-merge?). Merge {main} \
             into {develop} first, then re-run."
        );
    }
    phases.push((
        "preconditions",
        format!("ok — pinned {pinned_branch} @ {}", short_sha(&pinned_sha)),
    ));

    // Remember where the operator was, to restore on non-mutating exits
    // (dry run, blocked battery, version refusals).
    let original_ref = match git_stdout(repo_root, &["symbolic-ref", "--quiet", "--short", "HEAD"])
    {
        Ok(s) => s.trim().to_string(),
        Err(_) => rev_parse(repo_root, "HEAD")?, // detached
    };
    let restore = || {
        if let Err(e) = git_stdout(repo_root, &["checkout", "-q", &original_ref]) {
            eprintln!("WARN: could not restore original checkout `{original_ref}`: {e:#}");
        }
    };

    // (c) Battery against the pinned SHA (checked out detached, so a
    // mid-battery branch move cannot change what is being tested). ANY gate
    // Fail exits 1 — never maskable by a later "nothing to do" path.
    git_stdout(repo_root, &["checkout", "-q", &pinned_sha])
        .with_context(|| format!("checking out pinned {pinned_branch} @ {pinned_sha}"))?;
    bold(&format!(
        "═══ {} release cut — gate battery ({} gates) ═══",
        profile.name,
        profile.gates.len()
    ));
    let outcomes = run_battery(profile, repo_root, opts.skip);
    record_gate_outcomes(&outcomes);
    if battery_blocked(&outcomes) {
        restore();
        bail!(
            "gate battery BLOCKED the release:\n{}",
            format_battery_summary(&outcomes)
        );
    }
    let skipped = outcomes
        .iter()
        .filter(|o| matches!(o.result, GateResult::Skipped(_)))
        .count();
    phases.push((
        "battery",
        format!("{} gates green ({skipped} skipped)", outcomes.len()),
    ));
    if opts.dry_run {
        restore();
        print_phase_summary(&phases);
        bold("Dry run complete — battery green. Run without --dry-run to cut the release.");
        return Ok("dry-run: battery green".to_string());
    }

    // (d) Version: --version, or --level bump of the latest semver tag.
    let tags: Vec<String> = git_stdout(repo_root, &["tag"])?
        .lines()
        .map(str::to_string)
        .collect();
    let latest = highest_semver_tag(&tags, &profile.tag_prefix);
    let version =
        match compute_next_version(opts.version.as_deref(), opts.level, latest.as_deref()) {
            Ok(v) => v,
            Err(e) => {
                restore();
                return Err(e);
            }
        };
    let tag = format!("{}{version}", profile.tag_prefix);
    if let Err(e) = refuse_existing_tag(repo_root, &tag) {
        restore();
        return Err(e);
    }
    phases.push((
        "version",
        format!("{} → {version}", latest.as_deref().unwrap_or("<none>")),
    ));

    // (e) The release/hotfix branch, from the pinned SHA.
    let rel_branch = format!(
        "{}/{version}",
        if opts.hotfix { "hotfix" } else { "release" }
    );
    git_stdout(repo_root, &["checkout", "-q", "-b", &rel_branch, &pinned_sha])
        .with_context(|| format!("creating branch {rel_branch}"))?;
    phases.push(("branch", rel_branch.clone()));

    // (f) Bump version files; a failing build reverts them and aborts.
    if profile.version_files.is_empty() {
        println!("No version files configured — skipping bump commit.");
        phases.push(("bump", "skipped (no version files)".to_string()));
    } else {
        bump_and_commit(repo_root, profile, &version, &tag, &rel_branch, pinned_branch)?;
        phases.push(("bump", format!("bump: {tag}")));
    }

    // (g) Release notes — failures downgrade to loud WARN, never an abort.
    let notes = generate_release_notes(repo_root, &profile.tag_prefix, &version)
        .unwrap_or_else(|e| {
            red(&format!(
                "WARN: release notes generation failed ({e:#}) — continuing with \
                 minimal notes"
            ));
            format!("## {tag}\n\n_Notes generation failed; see `git log` for changes._\n")
        });
    let notes_path = git_common_dir(repo_root)?.join(format!("hex-release-notes-{tag}.md"));
    if let Err(e) = std::fs::write(&notes_path, &notes) {
        red(&format!(
            "WARN: could not write release notes to {} ({e}) — the gh release step \
             will WARN accordingly",
            notes_path.display()
        ));
    }
    phases.push(("notes", notes_path.display().to_string()));

    // (h) --no-ff merge to main; tag the merge commit.
    let main_before = rev_parse(repo_root, &format!("refs/heads/{main}"))?;
    if opts.hotfix {
        // A hotfix pins main: it must not have moved between pin and merge.
        if let Err(e) = check_pinned_unmoved(main, &pinned_sha, &main_before) {
            bail!(
                "{e:#}\nClean up the abandoned hotfix branch with: \
                 git checkout {main} && git branch -D {rel_branch}"
            );
        }
    }
    git_stdout(repo_root, &["checkout", "-q", main])?;
    if let Err(why) = merge_no_ff(repo_root, &rel_branch, &format!("release: {tag}")) {
        abort_merge(repo_root);
        bail!(
            "merge of {rel_branch} into {main} failed ({why}) — merge aborted, nothing \
             pushed. `{main}` moved during the cut or conflicts with the release; \
             inspect, then clean up with: git checkout {pinned_branch} && \
             git branch -D {rel_branch}"
        );
    }
    let main_sha = rev_parse(repo_root, "HEAD")?;
    git_stdout(repo_root, &["tag", &tag])
        .with_context(|| format!("tagging {tag} on the merge commit"))?;
    phases.push((
        "merge",
        format!("{rel_branch} → {main} @ {} (tag {tag})", short_sha(&main_sha)),
    ));

    // (i)+(j) Race guard, then --no-ff back-merge to develop. The guard runs
    // BEFORE the back-merge (the only moment develop's tip is still
    // comparable to the pin) and before any push, per step (j).
    if !opts.hotfix {
        let develop_now = rev_parse(repo_root, &format!("refs/heads/{develop}"))?;
        if let Err(e) = check_pinned_unmoved(develop, &pinned_sha, &develop_now) {
            bail!(
                "{e:#}\nLocal state to unwind first:\n  git tag -d {tag}\n  \
                 git branch -f {main} {main_before}\n  git checkout {develop} && \
                 git branch -D {rel_branch}"
            );
        }
    }
    git_stdout(repo_root, &["checkout", "-q", develop])?;
    if let Err(why) = merge_no_ff(repo_root, main, &format!("back-merge: {tag}")) {
        abort_merge(repo_root);
        red("BACK-MERGE CONFLICT — the release is NOT pushed.");
        bail!(
            "back-merge of {main} into {develop} failed ({why}).\n\
             State: local {main} has the release merge and tag {tag}; {develop} is \
             unchanged (merge aborted); {rel_branch} still exists. Nothing was pushed.\n\
             Recover (v1 = operator resolves):\n  \
             1. git checkout {develop} && git merge --no-ff {main}   # resolve, commit\n  \
             2. {RELEASE_PIPELINE_ENV}=1 git push origin {main} {develop} {tag}\n  \
             3. git branch -d {rel_branch}"
        );
    }
    let develop_sha = rev_parse(repo_root, "HEAD")?;
    phases.push((
        "back-merge",
        format!("{main} → {develop} @ {}", short_sha(&develop_sha)),
    ));

    // (j) Final mutual-consistency check before any push: the tag must sit
    // on the main tip and main must be an ancestor of develop.
    let tag_sha = rev_parse(repo_root, &format!("{tag}^{{commit}}"))?;
    if tag_sha != main_sha
        || !is_ancestor(
            repo_root,
            &format!("refs/heads/{main}"),
            &format!("refs/heads/{develop}"),
        )?
    {
        bail!(
            "inconsistent release state before push (tag {tag} @ {}, {main} @ {}) — \
             aborting; nothing was pushed",
            short_sha(&tag_sha),
            short_sha(&main_sha)
        );
    }
    phases.push((
        "race-guard",
        format!("ok — {main}, {develop}, and {tag} mutually consistent"),
    ));

    // (k) Hardened pushes — every push carries HEX_RELEASE_PIPELINE=1;
    // independent post-push ls-remote verify for both branches.
    bold("Pushing...");
    push_ref(repo_root, main)?;
    verify_pushed(repo_root, main, &main_sha)?;
    push_ref(repo_root, develop)?;
    verify_pushed(repo_root, develop, &develop_sha)?;
    let tag_ref = format!("refs/tags/{tag}");
    let tag_phase = match tag_push_action(&tag_sha, ls_remote_sha(repo_root, &tag_ref)?.as_deref())
    {
        TagPushAction::Push => {
            push_ref(repo_root, &tag_ref)?;
            // Independent verify for the tag too — same S6 doctrine.
            match ls_remote_sha(repo_root, &tag_ref)?.as_deref() {
                Some(sha) if sha == tag_sha => {}
                other => bail!(
                    "SHA MISMATCH after tag push: origin {tag} is {} but expected {tag_sha}",
                    other.unwrap_or("<absent>")
                ),
            }
            format!("{tag} pushed")
        }
        TagPushAction::SkipAlreadyOnOrigin => {
            green(&format!(
                "  Tag {tag} already on origin at the same commit ✓ (idempotent skip)"
            ));
            format!("{tag} already on origin")
        }
        TagPushAction::RefuseDivergent { remote_sha } => bail!(
            "tag {tag} on origin points to {}, not {} — refusing to overwrite a \
             divergent remote tag",
            short_sha(&remote_sha),
            short_sha(&tag_sha)
        ),
    };
    green(&format!("  Pushed {main} + {develop} — SHAs verified ✓"));
    phases.push(("push", format!("{main} + {develop} verified; {tag_phase}")));

    // (l) GitHub release — recoverable: every failure is a loud WARN naming
    // the backfill command; the pushes already succeeded.
    let gh_phase = if profile.gh_release {
        gh_release_step(repo_root, &tag, &notes_path)
    } else {
        "disabled by profile".to_string()
    };
    phases.push(("gh-release", gh_phase));

    // (m) Branch cleanup — best-effort, loud on failure.
    let mut cleanup = Vec::new();
    match git_stdout(repo_root, &["branch", "-d", &rel_branch]) {
        Ok(_) => cleanup.push(format!("{rel_branch} deleted locally")),
        Err(e) => {
            red(&format!("WARN: could not delete {rel_branch} locally: {e:#}"));
            cleanup.push(format!("{rel_branch} NOT deleted locally"));
        }
    }
    match ls_remote_sha(repo_root, &format!("refs/heads/{rel_branch}")) {
        Ok(None) => {} // never pushed — nothing to delete on origin
        Ok(Some(_)) => match push_ref(repo_root, &format!(":refs/heads/{rel_branch}")) {
            Ok(()) => cleanup.push(format!("{rel_branch} deleted on origin")),
            Err(e) => {
                red(&format!("WARN: could not delete {rel_branch} on origin: {e:#}"));
                cleanup.push(format!("{rel_branch} NOT deleted on origin"));
            }
        },
        Err(e) => red(&format!("WARN: could not check origin for {rel_branch}: {e:#}")),
    }
    phases.push(("cleanup", cleanup.join("; ")));

    // Final summary — every phase outcome.
    bold(&format!(
        "═══ Release complete: {tag} ({}) ═══",
        short_sha(&main_sha)
    ));
    print_phase_summary(&phases);
    Ok(format!("{tag} released"))
}

/// Step (f): write the new version into every profile version file, run the
/// profile build command (so lockfiles update), commit staging exactly the
/// version files + Cargo.lock. A failing build reverts the version files and
/// aborts.
fn bump_and_commit(
    repo_root: &Path,
    profile: &ReleaseProfile,
    version: &str,
    tag: &str,
    rel_branch: &str,
    pinned_branch: &str,
) -> Result<()> {
    let saved: Vec<(PathBuf, String)> = profile
        .version_files
        .iter()
        .map(|vf| {
            let full = repo_root.join(&vf.path);
            let body = std::fs::read_to_string(&full)
                .with_context(|| format!("reading version file {}", full.display()))?;
            Ok((full, body))
        })
        .collect::<Result<Vec<_>>>()?;
    for vf in &profile.version_files {
        vf.write_version(repo_root, version)?;
    }
    if let Some(build) = profile.build_command.as_deref() {
        bold(&format!("Bump build: {build}"));
        let mut cmd = Command::new("sh");
        cmd.args(["-c", build]).current_dir(repo_root);
        let failure = match run_checked(build, &mut cmd) {
            Err(msg) => Some(msg),
            Ok(r) if r.code != 0 => {
                eprintln!("{}", r.combined().trim_end());
                Some(format!("`{build}` failed (exit {})", r.code))
            }
            Ok(_) => None,
        };
        if let Some(why) = failure {
            for (path, body) in &saved {
                if let Err(e) = std::fs::write(path, body) {
                    eprintln!("WARN: reverting {} failed: {e}", path.display());
                }
            }
            bail!(
                "bump build failed: {why} — version files reverted. Clean up with: \
                 git checkout {pinned_branch} && git branch -D {rel_branch}"
            );
        }
    }
    let mut add = vec!["add".to_string(), "--".to_string()];
    add.extend(profile.version_files.iter().map(|vf| vf.path.clone()));
    if repo_root.join("Cargo.lock").exists() {
        add.push("Cargo.lock".to_string());
    }
    let add_refs: Vec<&str> = add.iter().map(String::as_str).collect();
    git_stdout(repo_root, &add_refs)?;
    git_stdout(repo_root, &["commit", "-q", "-m", &format!("bump: {tag}")])?;
    Ok(())
}

/// Step (l). Never errors: by this point the pushes have succeeded, so a gh
/// failure must not abort the release — it WARNs loudly with the exact
/// backfill command instead. Returns the phase-summary detail.
fn gh_release_step(repo_root: &Path, tag: &str, notes_path: &Path) -> String {
    let backfill = format!(
        "gh release create {tag} --title {tag} --notes-file {}",
        notes_path.display()
    );
    let mut view = Command::new("gh");
    view.args(["release", "view", tag]).current_dir(repo_root);
    match run_checked("gh release view", &mut view) {
        Err(msg) => {
            red(&format!(
                "WARN: GitHub release SKIPPED — {msg}. Backfill with: {backfill}"
            ));
            return format!("skipped ({msg})");
        }
        Ok(r) if r.code == 0 => {
            green(&format!(
                "  GitHub release {tag} already exists ✓ (idempotent skip)"
            ));
            return "already exists (idempotent skip)".to_string();
        }
        Ok(_) => {} // no existing release — create it (auth errors surface below)
    }
    let mut create = Command::new("gh");
    create
        .args(["release", "create", tag, "--title", tag, "--notes-file"])
        .arg(notes_path)
        .current_dir(repo_root);
    match run_checked("gh release create", &mut create) {
        Ok(r) if r.code == 0 => {
            green(&format!("  GitHub release {tag} created ✓"));
            "created".to_string()
        }
        Ok(r) => {
            let why = format!(
                "gh release create failed (exit {}): {}",
                r.code,
                r.stderr.trim()
            );
            red(&format!(
                "WARN: GitHub release SKIPPED — {why}. Backfill with: {backfill}"
            ));
            format!("skipped ({why})")
        }
        Err(msg) => {
            red(&format!(
                "WARN: GitHub release SKIPPED — {msg}. Backfill with: {backfill}"
            ));
            format!("skipped ({msg})")
        }
    }
}

fn print_phase_summary(phases: &[(&'static str, String)]) {
    bold("Phase summary:");
    for (name, detail) in phases {
        println!("  {name:<14} {detail}");
    }
}

// ---------------------------------------------------------------------------
// git-guard — the Rust backend for the pre-push hook shim (scope item 5).
// The hook is a footgun-guard for main, not a security boundary.
// ---------------------------------------------------------------------------

/// One parsed line of pre-push stdin:
/// `<local ref> SP <local sha> SP <remote ref> SP <remote sha>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushLine {
    pub local_ref: String,
    pub local_sha: String,
    pub remote_ref: String,
    pub remote_sha: String,
}

/// Parse one pre-push stdin line. `None` for anything that isn't exactly
/// four whitespace-separated fields.
pub fn parse_push_line(line: &str) -> Option<PushLine> {
    let mut fields = line.split_whitespace();
    let (local_ref, local_sha, remote_ref, remote_sha) = (
        fields.next()?,
        fields.next()?,
        fields.next()?,
        fields.next()?,
    );
    if fields.next().is_some() {
        return None;
    }
    Some(PushLine {
        local_ref: local_ref.to_string(),
        local_sha: local_sha.to_string(),
        remote_ref: remote_ref.to_string(),
        remote_sha: remote_sha.to_string(),
    })
}

/// Verdict for one pushed ref.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushDecision {
    Allow,
    Block(String),
}

/// The decision table: any update of `refs/heads/<main_branch>` (including a
/// delete) is blocked unless the pipeline env is set; develop, feature/*,
/// release/*, hotfix/*, tags — and any other non-main ref — pass through.
pub fn guard_decision(remote_ref: &str, pipeline_env_set: bool, main_branch: &str) -> PushDecision {
    if remote_ref == format!("refs/heads/{main_branch}") && !pipeline_env_set {
        return PushDecision::Block(format!(
            "BLOCKED: direct push to {main_branch} is not allowed — use `hex release cut`, \
             which runs the full gate battery and pushes with {RELEASE_PIPELINE_ENV}=1."
        ));
    }
    PushDecision::Allow
}

/// Evaluate a full pre-push stdin payload. The first blocked ref aborts the
/// push; a malformed line is a loud error, never a silent allow (S6).
pub fn guard_pre_push_input(input: &str, pipeline_env_set: bool, main_branch: &str) -> Result<()> {
    for line in input.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let parsed = parse_push_line(line).ok_or_else(|| {
            anyhow::anyhow!("git-guard: unrecognized pre-push input line: `{line}`")
        })?;
        if let PushDecision::Block(msg) =
            guard_decision(&parsed.remote_ref, pipeline_env_set, main_branch)
        {
            bail!("{msg}");
        }
    }
    Ok(())
}

/// CLI entry for `hex git-guard pre-push`: `input` is the hook's stdin;
/// the pipeline env is read from this process's environment. `Err` = block
/// (the hook exits 1).
pub fn git_guard_pre_push(input: &str) -> Result<()> {
    let env_set = std::env::var(RELEASE_PIPELINE_ENV)
        .map(|v| v == "1")
        .unwrap_or(false);
    guard_pre_push_input(input, env_set, "main")
}

// ---------------------------------------------------------------------------
// Reporting helpers — same streams as the legacy pipeline (red → stderr,
// green/bold → stdout, ANSI always on).
// ---------------------------------------------------------------------------

fn red(msg: &str) {
    eprintln!("\x1b[31m{msg}\x1b[0m");
}

fn green(msg: &str) {
    println!("\x1b[32m{msg}\x1b[0m");
}

fn bold(msg: &str) {
    println!("\x1b[1m{msg}\x1b[0m");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- semver: ports of tests/test_release_gates.sh tests 1–8 --------------

    #[test]
    fn semver_valid_accepts_valid_versions() {
        for v in ["0.0.1", "1.0.0", "10.20.30", "0.8.1"] {
            assert!(semver_valid(v), "semver_valid rejected valid '{v}'");
        }
    }

    #[test]
    fn semver_valid_rejects_invalid_versions() {
        for v in [
            "banana", "1.0", "1", "v1.0.0", "1.0.0-beta", "1.0.0.1", "", "+1.0.0", "1. 0.0",
        ] {
            assert!(!semver_valid(v), "semver_valid accepted invalid '{v}'");
        }
    }

    #[test]
    fn semver_gt_ordering() {
        for (a, b) in [
            ("1.0.1", "1.0.0"),
            ("2.0.0", "1.9.9"),
            ("0.8.1", "0.8.0"),
            ("1.0.0", "0.99.99"),
        ] {
            assert!(semver_gt(a, b), "semver_gt: {a} should be > {b}");
        }
        for (a, b) in [
            ("1.0.0", "1.0.0"),
            ("0.8.0", "0.8.0"),
            ("1.0.0", "2.0.0"),
            ("0.7.9", "0.8.0"),
        ] {
            assert!(!semver_gt(a, b), "semver_gt: {a} should NOT be > {b}");
        }
    }

    #[test]
    fn version_gate_blocks_unchanged_version() {
        // Legacy test 4: version == latest tag must block.
        assert!(!semver_gt("1.0.0", "1.0.0"));
    }

    #[test]
    fn version_gate_accepts_patch_bump() {
        // Legacy test 5: 1.0.0 → 1.0.1 passes.
        assert!(semver_gt("1.0.1", "1.0.0"));
    }

    #[test]
    fn version_gate_blocks_regression() {
        // Legacy test 6: 1.0.0 → 0.9.0 blocks.
        assert!(!semver_gt("0.9.0", "1.0.0"));
    }

    #[test]
    fn version_gate_accepts_major_bump() {
        // Legacy test 7: 1.0.0 → 2.0.0 passes.
        assert!(semver_gt("2.0.0", "1.0.0"));
    }

    #[test]
    fn next_patch_suggestion_matches_legacy_awk() {
        // Legacy test 8.
        assert_eq!(next_patch_suggestion("0.8.0").as_deref(), Some("0.8.1"));
        assert_eq!(next_patch_suggestion("1.2.9").as_deref(), Some("1.2.10"));
        assert_eq!(next_patch_suggestion("not-semver"), None);
    }

    #[test]
    fn bump_version_levels() {
        assert_eq!(bump_version("1.2.3", BumpLevel::Patch).as_deref(), Some("1.2.4"));
        assert_eq!(bump_version("1.2.3", BumpLevel::Minor).as_deref(), Some("1.3.0"));
        assert_eq!(bump_version("1.2.3", BumpLevel::Major).as_deref(), Some("2.0.0"));
        assert_eq!("patch".parse::<BumpLevel>().unwrap(), BumpLevel::Patch);
        assert!("banana".parse::<BumpLevel>().is_err());
    }

    #[test]
    fn highest_semver_tag_orders_by_semver_not_lexicographically() {
        let tags: Vec<String> = ["v0.9.0", "v0.10.0", "v0.2.0", "not-a-tag", "1.0"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(highest_semver_tag(&tags, "v").as_deref(), Some("0.10.0"));
        assert_eq!(highest_semver_tag(&[], "v"), None);
    }

    #[test]
    fn highest_semver_below_picks_notes_prev() {
        let tags: Vec<String> = ["v0.9.0", "v0.10.0", "v0.2.0"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            highest_semver_below(&tags, "v", "0.10.0").as_deref(),
            Some("0.9.0")
        );
        assert_eq!(
            highest_semver_below(&tags, "v", "1.0.0").as_deref(),
            Some("0.10.0")
        );
        assert_eq!(highest_semver_below(&tags, "v", "0.2.0"), None);
    }

    // -- release notes --------------------------------------------------------

    #[test]
    fn classify_subject_groups_conventional_prefixes() {
        assert_eq!(classify_subject("feat: add thing"), "feat");
        assert_eq!(classify_subject("feat(scope): add thing"), "feat");
        assert_eq!(classify_subject("fix!: breaking fix"), "fix");
        assert_eq!(classify_subject("docs: explain"), "docs");
        assert_eq!(classify_subject("chore: tidy"), "chore");
        assert_eq!(classify_subject("refactor(core): reshape"), "refactor");
        assert_eq!(classify_subject("test: cover"), "test");
        // "feature:" is not "feat:" — must not match.
        assert_eq!(classify_subject("feature: nope"), "other");
        assert_eq!(classify_subject("random subject line"), "other");
    }

    #[test]
    fn format_release_notes_groups_and_omits_empty_sections() {
        let subjects: Vec<String> = [
            "feat: one",
            "fix(api): two",
            "plain subject",
            "feat(ui)!: three",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let notes = format_release_notes("v", "1.2.3", Some("1.2.2"), &subjects);
        assert!(notes.starts_with("## v1.2.3"));
        assert!(notes.contains("Changes since v1.2.2."));
        assert!(notes.contains("### Features\n- feat: one\n- feat(ui)!: three\n"));
        assert!(notes.contains("### Fixes\n- fix(api): two\n"));
        assert!(notes.contains("### Other\n- plain subject\n"));
        // Empty groups omitted entirely.
        assert!(!notes.contains("### Docs"));
        assert!(!notes.contains("### Chores"));

        let first = format_release_notes("v", "0.1.0", None, &subjects);
        assert!(first.contains("First release"));
    }

    #[test]
    fn generate_release_notes_uses_prev_tag_then_full_history() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path();
        git(root, &["init", "-q"]);
        std::fs::write(root.join("a.txt"), "a").unwrap();
        git(root, &["add", "-A"]);
        commit(root, "feat: first thing");

        // No tags yet → full history.
        let notes = generate_release_notes(root, "v", "0.1.0").unwrap();
        assert!(notes.contains("First release"));
        assert!(notes.contains("- feat: first thing"));

        git(root, &["tag", "v0.1.0"]);
        std::fs::write(root.join("b.txt"), "b").unwrap();
        git(root, &["add", "-A"]);
        commit(root, "fix: second thing");

        // prev = v0.1.0 → only the post-tag commit appears.
        let notes = generate_release_notes(root, "v", "0.2.0").unwrap();
        assert!(notes.contains("Changes since v0.1.0."));
        assert!(notes.contains("- fix: second thing"));
        assert!(!notes.contains("- feat: first thing"));
    }

    // -- gate results ----------------------------------------------------------

    #[test]
    fn gate_result_formatting() {
        assert_eq!(GateResult::Pass.to_string(), "PASS");
        assert_eq!(
            GateResult::Fail("docker E2E build failed (exit 1): env resolution".into())
                .to_string(),
            "FAIL — docker E2E build failed (exit 1): env resolution"
        );
        assert_eq!(
            GateResult::Skipped("--skip-e2e — emergency bypass".into()).to_string(),
            "SKIPPED — --skip-e2e — emergency bypass"
        );

        let outcomes = vec![
            GateOutcome { name: "clean-tree".into(), result: GateResult::Pass },
            GateOutcome { name: "docker-e2e".into(), result: GateResult::Fail("boom".into()) },
        ];
        assert_eq!(
            format_battery_summary(&outcomes),
            "clean-tree: PASS\ndocker-e2e: FAIL — boom"
        );
        assert!(battery_blocked(&outcomes));
        assert!(!battery_blocked(&[GateOutcome {
            name: "x".into(),
            result: GateResult::Skipped("why".into()),
        }]));
    }

    #[test]
    fn doctor_carveout_pinned_semantics() {
        // Exactly one FAIL: line that is the Doctor check → pass.
        let one_doctor = "ok\nFAIL: Doctor check (no runtime binary)\nok\n";
        assert_eq!(doctor_carveout(one_doctor), GateResult::Pass);
        // Two FAIL: lines → fail, with the count reported.
        let two = "FAIL: Doctor check\nFAIL: memory index\n";
        assert_eq!(
            doctor_carveout(two),
            GateResult::Fail("regression suite failed (2 failures)".into())
        );
        // Nonzero exit with no FAIL lines at all → still a failure.
        let none = "container crashed before the suite started\n";
        assert_eq!(
            doctor_carveout(none),
            GateResult::Fail("regression suite failed (0 failures)".into())
        );
        // Case-sensitive: lowercase doctor does not qualify.
        let lower = "FAIL: doctor check\n";
        assert_eq!(
            doctor_carveout(lower),
            GateResult::Fail("regression suite failed (1 failures)".into())
        );
    }

    #[test]
    fn command_gate_pass_fail_and_tool_not_found() {
        let td = tempfile::tempdir().unwrap();
        assert_eq!(gate_command("true", td.path()), GateResult::Pass);
        match gate_command("exit 3", td.path()) {
            GateResult::Fail(msg) => assert!(msg.contains("exit 3"), "got: {msg}"),
            other => panic!("expected Fail, got {other:?}"),
        }
        // sh -c with a missing binary exits 127 → "tool not found", never a
        // plain test failure.
        match gate_command("definitely-not-a-real-tool-xyz", td.path()) {
            GateResult::Fail(msg) => {
                assert!(msg.contains("tool not found"), "got: {msg}");
                assert!(msg.contains("exit 127"), "got: {msg}");
            }
            other => panic!("expected Fail, got {other:?}"),
        }
    }

    #[test]
    fn clean_tree_gate_detects_dirt() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path();
        git(root, &["init", "-q"]);
        assert_eq!(gate_clean_tree(root), GateResult::Pass);
        std::fs::write(root.join("dirty.txt"), "x").unwrap();
        match gate_clean_tree(root) {
            GateResult::Fail(msg) => assert!(msg.contains("uncommitted"), "got: {msg}"),
            other => panic!("expected Fail, got {other:?}"),
        }
    }

    #[test]
    fn skip_flag_interplay() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path();
        let e2e = SkipFlags { skip_e2e: true, skip_parity: false };
        let parity = SkipFlags { skip_e2e: false, skip_parity: true };

        // --skip-e2e skips docker outright (no docker spawn).
        match run_gate(
            &GateSpec { name: "docker-e2e".into(), kind: GateKind::DockerE2e },
            root,
            e2e,
        ) {
            GateResult::Skipped(msg) => assert!(msg.contains("--skip-e2e"), "got: {msg}"),
            other => panic!("expected Skipped, got {other:?}"),
        }
        // --skip-e2e implies --skip-parity.
        match gate_codex_parity(root, e2e) {
            GateResult::Skipped(msg) => assert!(msg.contains("implies"), "got: {msg}"),
            other => panic!("expected Skipped, got {other:?}"),
        }
        // --skip-parity alone.
        match gate_codex_parity(root, parity) {
            GateResult::Skipped(msg) => assert!(msg.contains("--skip-parity"), "got: {msg}"),
            other => panic!("expected Skipped, got {other:?}"),
        }
        // Absent suites warn+skip (temp dir has no tests/).
        match gate_codex_parity(root, SkipFlags::default()) {
            GateResult::Skipped(msg) => assert!(msg.contains("not found"), "got: {msg}"),
            other => panic!("expected Skipped, got {other:?}"),
        }
        match gate_autonomy(root) {
            GateResult::Skipped(msg) => assert!(msg.contains("not found"), "got: {msg}"),
            other => panic!("expected Skipped, got {other:?}"),
        }
    }

    // -- profiles --------------------------------------------------------------

    fn toy_profile(name: &str, remote: Option<&str>, dir: Option<&str>) -> ReleaseProfile {
        ReleaseProfile {
            name: name.to_string(),
            match_remote: remote.map(str::to_string),
            match_dir: dir.map(str::to_string),
            gates: vec![GateSpec {
                name: "noop".to_string(),
                kind: GateKind::Command("true".to_string()),
            }],
            version_files: Vec::new(),
            build_command: None,
            tag_prefix: "v".to_string(),
            gh_release: false,
            main_branch: "main".to_string(),
            develop_branch: "develop".to_string(),
        }
    }

    #[test]
    fn profile_matches_by_remote_substring_or_dir_name() {
        let profiles = vec![
            toy_profile("widget", Some("acme/widget"), None),
            toy_profile("gadget", None, Some("gadget")),
        ];
        // Substring matches both https and ssh remote forms.
        for remote in [
            "https://github.com/acme/widget.git",
            "git@github.com:acme/widget.git",
        ] {
            assert_eq!(
                match_profile(&profiles, Some(remote), "elsewhere").map(|p| p.name.as_str()),
                Some("widget")
            );
        }
        // Dir-name match needs the exact toplevel name.
        assert_eq!(
            match_profile(&profiles, None, "gadget").map(|p| p.name.as_str()),
            Some("gadget")
        );
        assert_eq!(match_profile(&profiles, None, "gadget-fork"), None);
        // Built-in foundation matches its worktrees via the remote rule.
        let builtin = vec![builtin_foundation()];
        assert_eq!(
            match_profile(
                &builtin,
                Some("https://github.com/example/hex-foundation.git"),
                "hex-foundation-wt-xyz"
            )
            .map(|p| p.name.as_str()),
            Some("hex-foundation")
        );
    }

    #[test]
    fn unknown_repo_is_refused_listing_known_profiles() {
        let profiles = vec![builtin_foundation(), toy_profile("widget", Some("acme/widget"), None)];
        assert!(match_profile(&profiles, Some("https://example.com/x/y.git"), "y").is_none());
        let msg = refusal_message(&profiles, Some("https://example.com/x/y.git"), "y");
        assert!(msg.contains("no release profile matches"), "got: {msg}");
        assert!(msg.contains("hex-foundation"), "got: {msg}");
        assert!(msg.contains("widget"), "got: {msg}");
        assert!(msg.contains("releases.toml"), "got: {msg}");
        // No remote at all is still a clear message.
        let msg = refusal_message(&profiles, None, "y");
        assert!(msg.contains("<none>"), "got: {msg}");
    }

    #[test]
    fn builtin_foundation_battery_is_the_pinned_five() {
        let p = builtin_foundation();
        let names: Vec<&str> = p.gates.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(
            names,
            ["clean-tree", "docker-e2e", "sanitize", "codex-parity", "autonomy"]
        );
        assert_eq!(p.version_files.len(), 2);
        assert!(p.gh_release);
        assert_eq!(p.tag_prefix, "v");
        assert_eq!(p.main_branch, "main");
        assert_eq!(p.develop_branch, "develop");
    }

    #[test]
    fn releases_toml_loads_profiles_with_defaults() {
        let td = tempfile::tempdir().unwrap();
        let path = td.path().join("releases.toml");
        std::fs::write(
            &path,
            r#"
[[profiles]]
name = "boi"
match_dir = "boi"
gates = [
  { name = "just-check", command = "just check" },
  { name = "lint-scripts", command = "just lint-scripts" },
]
version_files = [{ path = "Cargo.toml", kind = "cargo-toml" }]
build_command = "cargo build --release"
"#,
        )
        .unwrap();
        let profiles = load_profiles_file(&path).unwrap();
        assert_eq!(profiles.len(), 1);
        let p = &profiles[0];
        assert_eq!(p.name, "boi");
        assert_eq!(p.gates.len(), 2);
        assert_eq!(
            p.gates[0].kind,
            GateKind::Command("just check".to_string())
        );
        assert_eq!(p.version_files[0].kind, VersionFileKind::CargoToml);
        // Defaults applied.
        assert_eq!(p.tag_prefix, "v");
        assert!(!p.gh_release);
        assert_eq!(p.main_branch, "main");
        assert_eq!(p.develop_branch, "develop");

        // Missing file ⇒ no extra profiles, no error.
        assert!(load_profiles_file(&td.path().join("absent.toml"))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn releases_toml_rejects_invalid_profiles_loudly() {
        let td = tempfile::tempdir().unwrap();

        // Malformed TOML is a hard error naming the file.
        let bad = td.path().join("bad.toml");
        std::fs::write(&bad, "this is = = not valid toml [[[").unwrap();
        let err = format!("{:#}", load_profiles_file(&bad).unwrap_err());
        assert!(err.contains("releases.toml") || err.contains("TOML"), "got: {err}");

        // A profile with no match rule is refused.
        let unmatched = td.path().join("unmatched.toml");
        std::fs::write(&unmatched, "[[profiles]]\nname = \"ghost\"\n").unwrap();
        let err = format!("{:#}", load_profiles_file(&unmatched).unwrap_err());
        assert!(err.contains("match rule"), "got: {err}");

        // An unknown version-file kind is refused with the valid kinds named.
        let badkind = td.path().join("badkind.toml");
        std::fs::write(
            &badkind,
            "[[profiles]]\nname = \"x\"\nmatch_dir = \"x\"\nversion_files = [{ path = \"V\", kind = \"exotic\" }]\n",
        )
        .unwrap();
        let err = format!("{:#}", load_profiles_file(&badkind).unwrap_err());
        assert!(err.contains("exotic"), "got: {err}");
        assert!(err.contains("plain, cargo-toml"), "got: {err}");
    }

    #[test]
    fn version_files_read_and_write_both_kinds() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(
            root.join("sub/Cargo.toml"),
            "[package]\n# version comment stays\nname = \"x\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(root.join("version.txt"), "0.1.0\n").unwrap();

        let cargo = VersionFile {
            path: "sub/Cargo.toml".to_string(),
            kind: VersionFileKind::CargoToml,
        };
        let plain = VersionFile {
            path: "version.txt".to_string(),
            kind: VersionFileKind::Plain,
        };
        assert_eq!(cargo.read_version(root).unwrap(), "0.1.0");
        assert_eq!(plain.read_version(root).unwrap(), "0.1.0");

        cargo.write_version(root, "0.2.0").unwrap();
        plain.write_version(root, "0.2.0").unwrap();
        assert_eq!(cargo.read_version(root).unwrap(), "0.2.0");
        assert_eq!(plain.read_version(root).unwrap(), "0.2.0");

        // The rest of the Cargo.toml survives the rewrite.
        let body = std::fs::read_to_string(root.join("sub/Cargo.toml")).unwrap();
        assert!(body.contains("# version comment stays"));
        assert!(body.contains("name = \"x\""));
        assert!(body.contains("edition = \"2021\""));
        assert_eq!(
            std::fs::read_to_string(root.join("version.txt")).unwrap(),
            "0.2.0\n"
        );

        // Missing version line is a loud error.
        std::fs::write(root.join("sub/Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        assert!(cargo.read_version(root).is_err());
        assert!(cargo.write_version(root, "9.9.9").is_err());
    }

    #[test]
    fn run_battery_runs_injected_toy_gates_in_order() {
        let td = tempfile::tempdir().unwrap();
        let mut profile = toy_profile("toy", None, Some("toy"));
        profile.gates = vec![
            GateSpec { name: "ok".into(), kind: GateKind::Command("true".into()) },
            GateSpec { name: "bad".into(), kind: GateKind::Command("exit 7".into()) },
        ];
        let outcomes = run_battery(&profile, td.path(), SkipFlags::default());
        assert_eq!(outcomes.len(), 2);
        assert_eq!(outcomes[0].name, "ok");
        assert_eq!(outcomes[0].result, GateResult::Pass);
        assert_eq!(outcomes[1].name, "bad");
        assert!(matches!(outcomes[1].result, GateResult::Fail(_)));
        assert!(battery_blocked(&outcomes));
    }

    // -- pure ceremony decisions ----------------------------------------------

    #[test]
    fn compute_next_version_rules() {
        // Explicit wins and must be semver.
        assert_eq!(
            compute_next_version(Some("1.2.3"), None, Some("1.2.2")).unwrap(),
            "1.2.3"
        );
        let err = format!("{:#}", compute_next_version(Some("banana"), None, None).unwrap_err());
        assert!(err.contains("not semver"), "got: {err}");
        // Explicit on a first release (no tags at all) is fine.
        assert_eq!(compute_next_version(Some("0.1.0"), None, None).unwrap(), "0.1.0");
        // Level bumps the latest tag; the default level is patch.
        assert_eq!(
            compute_next_version(None, Some(BumpLevel::Minor), Some("1.2.3")).unwrap(),
            "1.3.0"
        );
        assert_eq!(compute_next_version(None, None, Some("1.2.3")).unwrap(), "1.2.4");
        // Level with no tags refuses, pointing at --version.
        let err = format!(
            "{:#}",
            compute_next_version(None, Some(BumpLevel::Patch), None).unwrap_err()
        );
        assert!(err.contains("--version"), "got: {err}");
        // Not strictly greater refuses, with the next-patch hint.
        let err = format!(
            "{:#}",
            compute_next_version(Some("1.2.2"), None, Some("1.2.2")).unwrap_err()
        );
        assert!(err.contains("not greater"), "got: {err}");
        assert!(err.contains("1.2.3"), "got: {err}");
    }

    #[test]
    fn tag_push_action_decision_table() {
        assert_eq!(tag_push_action("abc", None), TagPushAction::Push);
        assert_eq!(
            tag_push_action("abc", Some("abc")),
            TagPushAction::SkipAlreadyOnOrigin
        );
        assert_eq!(
            tag_push_action("abc", Some("def")),
            TagPushAction::RefuseDivergent { remote_sha: "def".to_string() }
        );
    }

    #[test]
    fn race_guard_decision() {
        assert!(check_pinned_unmoved("develop", "abc", "abc").is_ok());
        let err = format!(
            "{:#}",
            check_pinned_unmoved("develop", "abc123def", "fed321cba").unwrap_err()
        );
        assert!(err.contains("`develop`"), "got: {err}");
        assert!(err.contains("moved during the cut"), "got: {err}");
        assert!(err.contains("Nothing was pushed"), "got: {err}");
    }

    // -- release lock -----------------------------------------------------------

    #[test]
    fn release_lock_is_exclusive_and_released_on_drop() {
        let td = tempfile::tempdir().unwrap();
        let repo = td.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        let lock_file = repo.join(".git/hex-release.lock");

        let lock = ReleaseLock::acquire(&repo).unwrap();
        assert!(lock_file.exists());
        // Held lock refuses a second acquire, naming the holder.
        let err = format!("{:#}", ReleaseLock::acquire(&repo).unwrap_err());
        assert!(err.contains("already in flight"), "got: {err}");
        assert!(err.contains("pid="), "got: {err}");
        // Dropping releases; the lock can be re-taken.
        drop(lock);
        assert!(!lock_file.exists());
        let again = ReleaseLock::acquire(&repo).unwrap();
        drop(again);
        assert!(!lock_file.exists());
    }

    // -- git-guard ----------------------------------------------------------------

    #[test]
    fn parse_push_line_handles_the_pre_push_wire_format() {
        let ones = "1111111111111111111111111111111111111111";
        let twos = "2222222222222222222222222222222222222222";
        let p = parse_push_line(&format!("refs/heads/dev {ones} refs/heads/main {twos}")).unwrap();
        assert_eq!(p.local_ref, "refs/heads/dev");
        assert_eq!(p.local_sha, ones);
        assert_eq!(p.remote_ref, "refs/heads/main");
        assert_eq!(p.remote_sha, twos);
        assert!(parse_push_line("too few fields").is_none());
        assert!(parse_push_line("a b c d e").is_none());
        assert!(parse_push_line("").is_none());
    }

    #[test]
    fn guard_decision_table() {
        // main is blocked without the pipeline env...
        match guard_decision("refs/heads/main", false, "main") {
            PushDecision::Block(msg) => {
                assert!(msg.contains("hex release cut"), "got: {msg}");
                assert!(msg.contains(RELEASE_PIPELINE_ENV), "got: {msg}");
            }
            PushDecision::Allow => panic!("main without env must be blocked"),
        }
        // ...and allowed with it.
        assert_eq!(guard_decision("refs/heads/main", true, "main"), PushDecision::Allow);
        // develop, feature/*, release/*, hotfix/*, and tags pass through env-less.
        for r in [
            "refs/heads/develop",
            "refs/heads/feature/x",
            "refs/heads/release/1.2.3",
            "refs/heads/hotfix/1.2.4",
            "refs/tags/v1.2.3",
        ] {
            assert_eq!(guard_decision(r, false, "main"), PushDecision::Allow, "{r}");
        }
        // The protected branch name is a parameter, not hardcoded twice.
        assert!(matches!(
            guard_decision("refs/heads/trunk", false, "trunk"),
            PushDecision::Block(_)
        ));
        assert_eq!(guard_decision("refs/heads/main", false, "trunk"), PushDecision::Allow);
    }

    #[test]
    fn guard_pre_push_input_blocks_main_anywhere_in_stdin() {
        let zeros = "0000000000000000000000000000000000000000";
        let sha = "1111111111111111111111111111111111111111";
        let ok_input = format!(
            "refs/heads/develop {sha} refs/heads/develop {zeros}\n\
             refs/tags/v1.0.0 {sha} refs/tags/v1.0.0 {zeros}\n"
        );
        assert!(guard_pre_push_input(&ok_input, false, "main").is_ok());
        let main_input = format!(
            "refs/heads/develop {sha} refs/heads/develop {zeros}\n\
             refs/heads/main {sha} refs/heads/main {zeros}\n"
        );
        assert!(guard_pre_push_input(&main_input, false, "main").is_err());
        assert!(guard_pre_push_input(&main_input, true, "main").is_ok());
        // A DELETE of main (local ref `(delete)`, zero sha) is still an
        // update of refs/heads/main — blocked.
        let delete_main = format!("(delete) {zeros} refs/heads/main {sha}\n");
        assert!(guard_pre_push_input(&delete_main, false, "main").is_err());
        // Malformed stdin is a loud error, never a silent allow (S6).
        assert!(guard_pre_push_input("garbage line", false, "main").is_err());
        // Empty stdin (nothing to push) allows.
        assert!(guard_pre_push_input("", false, "main").is_ok());
    }

    // -- ceremony fixtures --------------------------------------------------------

    /// A temp GitFlow repo (main + develop, one commit, version.txt @ 0.1.0)
    /// with a local bare origin both branches are pushed to.
    fn gitflow_fixture(td: &Path) -> PathBuf {
        let origin = td.join("origin.git");
        std::fs::create_dir(&origin).unwrap();
        git(&origin, &["init", "-q", "--bare"]);
        let repo = td.join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "test@example.com"]);
        git(&repo, &["config", "user.name", "Test"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        // Hermetic commits/pushes: neutralize any global hooks.
        let nohooks = td.join("nohooks");
        std::fs::create_dir_all(&nohooks).unwrap();
        git(&repo, &["config", "core.hooksPath", nohooks.to_str().unwrap()]);
        std::fs::write(repo.join("version.txt"), "0.1.0\n").unwrap();
        git(&repo, &["add", "-A"]);
        commit(&repo, "feat: initial");
        git(&repo, &["branch", "develop"]);
        git(&repo, &["remote", "add", "origin", origin.to_str().unwrap()]);
        git(&repo, &["push", "-q", "origin", "main", "develop"]);
        repo
    }

    /// Toy ceremony profile: one trivial gate, version.txt bump, no build
    /// command, no gh release.
    fn ceremony_profile() -> ReleaseProfile {
        let mut p = toy_profile("toy", None, Some("repo"));
        p.version_files = vec![VersionFile {
            path: "version.txt".to_string(),
            kind: VersionFileKind::Plain,
        }];
        p
    }

    fn cut_v(repo: &Path, profile: &ReleaseProfile, version: &str) -> Result<()> {
        cut_with_profile(
            repo,
            profile,
            &CutOptions { version: Some(version.to_string()), ..Default::default() },
        )
    }

    // -- ceremony ------------------------------------------------------------------

    #[test]
    fn cut_success_path_full_ceremony() {
        let (_hex, _guard) = crate::telemetry::test_support::isolate();
        let td = tempfile::tempdir().unwrap();
        let repo = gitflow_fixture(td.path());
        let origin = td.path().join("origin.git");
        cut_v(&repo, &ceremony_profile(), "0.2.0").unwrap();

        // Tag sits on the main merge commit, locally and on origin.
        let main_sha = git_stdout(&repo, &["rev-parse", "refs/heads/main"]).unwrap();
        assert_eq!(git_stdout(&repo, &["rev-parse", "v0.2.0^{commit}"]).unwrap(), main_sha);
        assert_eq!(git_stdout(&origin, &["rev-parse", "refs/heads/main"]).unwrap(), main_sha);
        assert_eq!(git_stdout(&origin, &["rev-parse", "v0.2.0^{commit}"]).unwrap(), main_sha);
        // develop pushed too, carrying the back-merged bump.
        assert_eq!(
            git_stdout(&origin, &["rev-parse", "refs/heads/develop"]).unwrap(),
            git_stdout(&repo, &["rev-parse", "refs/heads/develop"]).unwrap()
        );
        assert_eq!(git_stdout(&repo, &["show", "main:version.txt"]).unwrap(), "0.2.0\n");
        assert_eq!(git_stdout(&repo, &["show", "develop:version.txt"]).unwrap(), "0.2.0\n");
        // Release branch cleaned up; lock released; repo left on develop.
        assert!(!ref_exists(&repo, "refs/heads/release/0.2.0"));
        assert!(!repo.join(".git/hex-release.lock").exists());
        assert_eq!(
            git_stdout(&repo, &["symbolic-ref", "--short", "HEAD"]).unwrap().trim(),
            "develop"
        );
    }

    #[test]
    fn cut_hotfix_pins_main_and_uses_hotfix_branch() {
        let (_hex, _guard) = crate::telemetry::test_support::isolate();
        let td = tempfile::tempdir().unwrap();
        let repo = gitflow_fixture(td.path());
        // develop ahead of main — the normal GitFlow state during a hotfix.
        git(&repo, &["checkout", "-q", "develop"]);
        std::fs::write(repo.join("dev.txt"), "wip").unwrap();
        git(&repo, &["add", "-A"]);
        commit(&repo, "feat: in-flight work");
        git(&repo, &["push", "-q", "origin", "develop"]);
        git(&repo, &["checkout", "-q", "main"]);

        cut_with_profile(
            &repo,
            &ceremony_profile(),
            &CutOptions {
                version: Some("0.1.1".to_string()),
                hotfix: true,
                ..Default::default()
            },
        )
        .unwrap();

        let main_sha = git_stdout(&repo, &["rev-parse", "refs/heads/main"]).unwrap();
        assert_eq!(git_stdout(&repo, &["rev-parse", "v0.1.1^{commit}"]).unwrap(), main_sha);
        assert!(!ref_exists(&repo, "refs/heads/hotfix/0.1.1"));
        // The back-merge carried the bump to develop without losing its work.
        assert_eq!(git_stdout(&repo, &["show", "develop:version.txt"]).unwrap(), "0.1.1\n");
        assert_eq!(git_stdout(&repo, &["show", "develop:dev.txt"]).unwrap(), "wip");
    }

    #[test]
    fn cut_dry_run_stops_after_battery_and_restores() {
        let (_hex, _guard) = crate::telemetry::test_support::isolate();
        let td = tempfile::tempdir().unwrap();
        let repo = gitflow_fixture(td.path());
        let profile = ceremony_profile();
        git(&repo, &["checkout", "-q", "main"]);

        // Green battery: exit 0, nothing mutated, original checkout restored.
        cut_with_profile(&repo, &profile, &CutOptions { dry_run: true, ..Default::default() })
            .unwrap();
        assert!(git_stdout(&repo, &["tag"]).unwrap().trim().is_empty());
        assert_eq!(
            git_stdout(&repo, &["symbolic-ref", "--short", "HEAD"]).unwrap().trim(),
            "main"
        );

        // Blocked battery: still exit 1 even with --dry-run.
        let mut blocked = profile.clone();
        blocked.gates = vec![GateSpec {
            name: "boom".to_string(),
            kind: GateKind::Command("exit 1".to_string()),
        }];
        let err = format!(
            "{:#}",
            cut_with_profile(&repo, &blocked, &CutOptions { dry_run: true, ..Default::default() })
                .unwrap_err()
        );
        assert!(err.contains("BLOCKED"), "got: {err}");
        assert!(git_stdout(&repo, &["tag"]).unwrap().trim().is_empty());
        // The lock is released on the failure path too.
        assert!(!repo.join(".git/hex-release.lock").exists());
    }

    #[test]
    fn cut_refuses_dirty_tree_and_missing_develop() {
        let (_hex, _guard) = crate::telemetry::test_support::isolate();
        let td = tempfile::tempdir().unwrap();
        let repo = gitflow_fixture(td.path());
        let profile = ceremony_profile();

        std::fs::write(repo.join("stray.txt"), "x").unwrap();
        let err = format!("{:#}", cut_v(&repo, &profile, "0.2.0").unwrap_err());
        assert!(err.contains("not clean"), "got: {err}");
        std::fs::remove_file(repo.join("stray.txt")).unwrap();

        // Missing develop refuses with the exact bootstrap instruction.
        git(&repo, &["checkout", "-q", "main"]);
        git(&repo, &["branch", "-D", "develop"]);
        let err = format!("{:#}", cut_v(&repo, &profile, "0.2.0").unwrap_err());
        assert!(
            err.contains("git branch develop main && git push origin develop"),
            "got: {err}"
        );
    }

    #[test]
    fn cut_refuses_when_lock_held() {
        let (_hex, _guard) = crate::telemetry::test_support::isolate();
        let td = tempfile::tempdir().unwrap();
        let repo = gitflow_fixture(td.path());
        std::fs::write(repo.join(".git/hex-release.lock"), "pid=999 started=earlier").unwrap();
        let err = format!("{:#}", cut_v(&repo, &ceremony_profile(), "0.2.0").unwrap_err());
        assert!(err.contains("already in flight"), "got: {err}");
        assert!(err.contains("pid=999"), "got: {err}");
        // The holder's lock survives the refused attempt.
        assert!(repo.join(".git/hex-release.lock").exists());
    }

    #[test]
    fn cut_bump_build_failure_reverts_version_files() {
        let (_hex, _guard) = crate::telemetry::test_support::isolate();
        let td = tempfile::tempdir().unwrap();
        let repo = gitflow_fixture(td.path());
        let mut profile = ceremony_profile();
        profile.build_command = Some("exit 1".to_string());

        let err = format!("{:#}", cut_v(&repo, &profile, "0.2.0").unwrap_err());
        assert!(err.contains("version files reverted"), "got: {err}");
        // Reverted on disk, tree clean again, nothing tagged or pushed.
        assert_eq!(std::fs::read_to_string(repo.join("version.txt")).unwrap(), "0.1.0\n");
        assert!(git_stdout(&repo, &["status", "--porcelain"]).unwrap().trim().is_empty());
        assert!(git_stdout(&repo, &["tag"]).unwrap().trim().is_empty());
    }

    #[test]
    fn cut_refuses_existing_tags_local_and_origin() {
        let td = tempfile::tempdir().unwrap();
        let repo = gitflow_fixture(td.path());
        let origin = td.path().join("origin.git");
        assert!(refuse_existing_tag(&repo, "v0.2.0").is_ok());
        git(&repo, &["tag", "v9.9.9"]);
        let err = format!("{:#}", refuse_existing_tag(&repo, "v9.9.9").unwrap_err());
        assert!(err.contains("locally"), "got: {err}");
        // A tag that exists only on origin is refused too.
        git(&origin, &["tag", "v8.8.8", "main"]);
        let err = format!("{:#}", refuse_existing_tag(&repo, "v8.8.8").unwrap_err());
        assert!(err.contains("origin"), "got: {err}");
    }

    #[test]
    fn cut_aborts_on_develop_moved_mid_cut_before_any_push() {
        let (_hex, _guard) = crate::telemetry::test_support::isolate();
        let td = tempfile::tempdir().unwrap();
        let repo = gitflow_fixture(td.path());
        let origin = td.path().join("origin.git");
        // A second commit so the malicious gate has somewhere to move develop.
        git(&repo, &["checkout", "-q", "develop"]);
        std::fs::write(repo.join("b.txt"), "b").unwrap();
        git(&repo, &["add", "-A"]);
        commit(&repo, "feat: second");
        git(&repo, &["push", "-q", "origin", "develop"]);
        let origin_main = git_stdout(&origin, &["rev-parse", "refs/heads/main"]).unwrap();
        let origin_dev = git_stdout(&origin, &["rev-parse", "refs/heads/develop"]).unwrap();

        // The battery runs detached, so a gate CAN move the develop ref —
        // exactly the race the guard must catch before any push.
        let mut profile = ceremony_profile();
        profile.gates = vec![GateSpec {
            name: "mover".to_string(),
            kind: GateKind::Command("git update-ref refs/heads/develop HEAD~1".to_string()),
        }];
        let err = format!("{:#}", cut_v(&repo, &profile, "0.2.0").unwrap_err());
        assert!(err.contains("moved during the cut"), "got: {err}");
        // Nothing reached origin.
        assert_eq!(git_stdout(&origin, &["rev-parse", "refs/heads/main"]).unwrap(), origin_main);
        assert_eq!(
            git_stdout(&origin, &["rev-parse", "refs/heads/develop"]).unwrap(),
            origin_dev
        );
    }

    // -- test helpers ------------------------------------------------------------

    fn git(root: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("git must be runnable in tests");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn commit(root: &Path, subject: &str) {
        git(
            root,
            &[
                "-c",
                "user.email=test@example.com",
                "-c",
                "user.name=Test",
                "commit",
                "-q",
                "-m",
                subject,
            ],
        );
    }
}
