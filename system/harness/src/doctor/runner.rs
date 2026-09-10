use crate::doctor::check::{CheckResult, Context, DoctorCheck};
use crate::doctor::checks;

pub struct Runner {
    pub checks: Vec<Box<dyn DoctorCheck>>,
}

impl Runner {
    pub fn all_checks() -> Self {
        Self { checks: registry() }
    }

    pub fn filtered(pattern: &str) -> Self {
        let pattern = pattern.to_lowercase();
        Self {
            checks: registry()
                .into_iter()
                .filter(|c| c.name().to_lowercase().contains(&pattern))
                .collect(),
        }
    }

    pub fn run(&self, ctx: &Context) -> Vec<(String, CheckResult)> {
        self.checks
            .iter()
            .map(|c| {
                let start = std::time::Instant::now();
                let mut result = c.run(ctx);
                result.elapsed_ms = start.elapsed().as_millis() as u64;
                (c.name().to_string(), result)
            })
            .collect()
    }

    pub fn list(&self) {
        for check in &self.checks {
            println!("{:35} [{}]", check.name(), check.category());
        }
    }
}

fn registry() -> Vec<Box<dyn DoctorCheck>> {
    vec![
        // Health — structural checks
        Box::new(checks::hex_dir::HexDirSet),
        Box::new(checks::hex_structure::HexExists),
        Box::new(checks::hex_structure::HexSkillsExists),
        Box::new(checks::hex_structure::HexSkillsPopulated),
        Box::new(checks::git::GitInitialized),
        Box::new(checks::git::HooksPathConfigured),
        Box::new(checks::harness_buildable::HarnessBuildableFromGit),
        Box::new(checks::symlinks::AgentsSkillsSymlink),
        Box::new(checks::symlinks::NoBrokenSymlinks),
        Box::new(checks::memory_db::MemoryDbExists),
        Box::new(checks::distill_strikes::DistillStrikes),
        Box::new(checks::vector_search::VectorSearchHealthy),
        Box::new(checks::reflection_liveness::ReflectionLogFresh),
        Box::new(checks::nightly_full_liveness::NightlyFullLiveness),
        Box::new(checks::consolidation_audit_freshness::ConsolidationAuditFreshness),
        Box::new(checks::scripts_exec::ScriptsExecutable),
        Box::new(checks::boi_health::BoiHealth),
        Box::new(checks::iii_engine_health::IiiEngineHealth),
        Box::new(checks::telemetry_health::TelemetryHealth),
        Box::new(checks::python::PythonVersion),
        Box::new(checks::hex_binary::HexBinaryOnPath),
        // Config checks
        Box::new(checks::llm_provider::LlmProviderReachable),
        Box::new(checks::distill_readiness::DistillReadiness),
        Box::new(checks::env_sh::EnvSh),
        Box::new(checks::claude_md::ClaudeMdExists),
        Box::new(checks::charter_drift::CharterDrift),
        Box::new(checks::claude_runs_config::ClaudeRunsConfig),
        Box::new(checks::codex_config::CodexConfigExists),
        Box::new(checks::codex::CodexCliOnPath),
        Box::new(checks::codex::CodexVersionOk),
        Box::new(checks::codex::CodexApiKey),
        Box::new(checks::codex::CodexAgentsMdExists),
        Box::new(checks::codex::CodexAgentsMdComplete),
        Box::new(checks::me_md::MeMdContent),
        Box::new(checks::todo_md::TodoMdExists),
        Box::new(checks::llm_preference::LlmPreferenceExists),
        Box::new(checks::llm_preference::NoStaleLlmPreference),
        Box::new(checks::llm_config::LlmConfigCheck),
        Box::new(checks::llm_config::StaleLlmPreferenceCheck),
        Box::new(checks::settings_json::SettingsJsonValid),
        Box::new(checks::timezone::TimezoneValid),
        // Registry health checks
        Box::new(checks::registry_health::RegistryOrphanedBin),
        Box::new(checks::registry_health::RegistryStalePolicy),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doctor::check::{Category, Status};
    use std::path::PathBuf;

    struct AlwaysPass;
    impl DoctorCheck for AlwaysPass {
        fn name(&self) -> &str {
            "always-pass"
        }
        fn category(&self) -> Category {
            Category::Health
        }
        fn run(&self, _ctx: &Context) -> CheckResult {
            CheckResult::pass("ok")
        }
    }

    struct AlwaysWarn;
    impl DoctorCheck for AlwaysWarn {
        fn name(&self) -> &str {
            "always-warn"
        }
        fn category(&self) -> Category {
            Category::Config
        }
        fn run(&self, _ctx: &Context) -> CheckResult {
            CheckResult::warn("degraded")
        }
    }

    struct AlwaysFail;
    impl DoctorCheck for AlwaysFail {
        fn name(&self) -> &str {
            "always-fail"
        }
        fn category(&self) -> Category {
            Category::Health
        }
        fn run(&self, _ctx: &Context) -> CheckResult {
            CheckResult::fail("broken")
        }
    }

    fn test_ctx() -> Context {
        Context {
            hex_dir: PathBuf::from("/tmp/fake-hex"),
            home: PathBuf::from("/tmp"),
            fix: false,
        }
    }

    #[test]
    fn test_runner_trait_dispatch() {
        let runner = Runner {
            checks: vec![
                Box::new(AlwaysPass),
                Box::new(AlwaysWarn),
                Box::new(AlwaysFail),
            ],
        };
        let results = runner.run(&test_ctx());
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].1.status, Status::Pass);
        assert_eq!(results[1].1.status, Status::Warn);
        assert_eq!(results[2].1.status, Status::Fail);
    }

    #[test]
    fn test_filter_matching() {
        // A runner built from ad-hoc checks filtered by name substring
        let checks: Vec<Box<dyn DoctorCheck>> = vec![
            Box::new(AlwaysPass),
            Box::new(AlwaysWarn),
            Box::new(AlwaysFail),
        ];
        let pattern = "warn";
        let filtered: Vec<_> = checks
            .into_iter()
            .filter(|c| c.name().contains(pattern))
            .collect();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name(), "always-warn");
    }

    #[test]
    fn test_result_aggregation() {
        let runner = Runner {
            checks: vec![
                Box::new(AlwaysPass),
                Box::new(AlwaysWarn),
                Box::new(AlwaysFail),
            ],
        };
        let results = runner.run(&test_ctx());
        let error_count = results.iter().filter(|(_, r)| r.status.is_error()).count();
        let warn_count = results
            .iter()
            .filter(|(_, r)| r.status.is_warning())
            .count();
        let pass_count = results
            .iter()
            .filter(|(_, r)| r.status.counts_as_pass())
            .count();
        assert_eq!(error_count, 1);
        assert_eq!(warn_count, 1);
        assert_eq!(pass_count, 1);
    }

    #[test]
    fn test_elapsed_ms_populated() {
        let runner = Runner {
            checks: vec![Box::new(AlwaysPass)],
        };
        let results = runner.run(&test_ctx());
        // elapsed_ms should be set (may be 0 for instant check, that's fine)
        assert_eq!(results[0].0, "always-pass");
    }

    #[test]
    fn test_registry_has_checks() {
        let runner = Runner::all_checks();
        assert!(
            runner.checks.len() >= 10,
            "registry must have at least 10 checks"
        );
    }

    #[test]
    fn test_registry_includes_telemetry_health() {
        let runner = Runner::all_checks();
        assert!(
            runner.checks.iter().any(|c| c.name() == "telemetry-health"),
            "registry must include the telemetry-health doctor check"
        );
    }

    #[test]
    fn test_registry_includes_consolidation_audit_freshness() {
        let runner = Runner::all_checks();
        assert!(
            runner
                .checks
                .iter()
                .any(|c| c.name() == "consolidation-audit-freshness"),
            "registry must include the consolidation-audit-freshness doctor check"
        );
    }

    // ---- tests for task Tv7psnbjd (B2: harness-buildable-from-git) ----

    #[test]
    fn test_registry_includes_harness_buildable_from_git() {
        let runner = Runner::all_checks();
        assert!(
            runner
                .checks
                .iter()
                .any(|c| c.name() == "harness-buildable-from-git"),
            "registry must include the harness-buildable-from-git doctor check (task Tv7psnbjd)"
        );
    }

    /// An empty, never-populated directory used as `core.hooksPath` for
    /// fixture commits below (F18) — created once per test binary and
    /// shared read-only, so no per-call temp-dir churn and no risk of two
    /// concurrent tests racing on the same path.
    fn empty_hooks_dir() -> &'static std::path::Path {
        static DIR: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
        DIR.get_or_init(|| tempfile::tempdir().expect("create empty hooks dir"))
            .path()
    }

    /// Run a git command in `dir`, panicking with its args/output on
    /// failure.
    ///
    /// F18: a `commit` invocation gets `-c commit.gpgsign=false -c
    /// core.hooksPath=<empty dir>` prepended, command-local — a machine or
    /// fixture with `commit.gpgsign`/`core.hooksPath` configured locally
    /// must not be able to require a developer's signing credentials or
    /// run unrelated hooks just to create a test fixture. Command-local `-c`
    /// flags (not process-global env mutation) keep this safe under
    /// concurrent test execution.
    fn run_git(dir: &std::path::Path, args: &[&str]) {
        let mut full_args: Vec<String> = Vec::new();
        if args.first() == Some(&"commit") {
            full_args.push("-c".to_string());
            full_args.push("commit.gpgsign=false".to_string());
            full_args.push("-c".to_string());
            full_args.push(format!("core.hooksPath={}", empty_hooks_dir().display()));
        }
        full_args.extend(args.iter().map(|s| s.to_string()));

        let output = std::process::Command::new("git")
            .args(&full_args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "hex-test")
            .env("GIT_AUTHOR_EMAIL", "hex-test@example.com")
            .env("GIT_COMMITTER_NAME", "hex-test")
            .env("GIT_COMMITTER_EMAIL", "hex-test@example.com")
            .output()
            .expect("git command spawns");
        assert!(
            output.status.success(),
            "git {:?} failed in {}: {}",
            full_args,
            dir.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Count `git worktree list` entries for `dir` — used to prove the doctor
    /// check's temp worktree is cleaned up (never left behind, pass or fail).
    fn worktree_count(dir: &std::path::Path) -> usize {
        let output = std::process::Command::new("git")
            .args(["worktree", "list", "--porcelain"])
            .current_dir(dir)
            .output()
            .expect("git worktree list runs");
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|l| l.starts_with("worktree "))
            .count()
    }

    /// A dep-free `Cargo.lock` matching this cargo's own lockfile format
    /// (verified by hand with `cargo generate-lockfile --offline` against a
    /// throwaway dep-free crate). Committing this into every fixture means
    /// the check's step-2 `cargo metadata --locked --offline` branch — which
    /// only runs when a `Cargo.lock` is tracked — actually executes under
    /// test, instead of being silently skipped (the gap review flagged: F2).
    const FIXTURE_LOCKFILE: &str = "# This file is automatically @generated by Cargo.\n# It is not intended for manual editing.\nversion = 4\n\n[[package]]\nname = \"fixture-harness\"\nversion = \"0.1.0\"\n";

    /// Fixture: harness references `.hex/harness/data/foo.txt`, which
    /// exists on disk but was never `git add`ed — i.e. exactly the
    /// gitignore / `.git/info/exclude` class of bug B2 describes. `git
    /// worktree add` from HEAD will not carry it.
    fn init_repo_missing_include() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        run_git(tmp.path(), &["init", "-q"]);
        let harness = tmp.path().join(".hex/harness");
        std::fs::create_dir_all(harness.join("src")).unwrap();
        std::fs::write(
            harness.join("Cargo.toml"),
            "[package]\nname = \"fixture-harness\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(harness.join("Cargo.lock"), FIXTURE_LOCKFILE).unwrap();
        std::fs::write(
            harness.join("src/lib.rs"),
            "pub const DATA: &str = include_str!(\"../data/foo.txt\");\n",
        )
        .unwrap();
        run_git(
            tmp.path(),
            &[
                "add",
                ".hex/harness/Cargo.toml",
                ".hex/harness/Cargo.lock",
                ".hex/harness/src/lib.rs",
            ],
        );
        run_git(tmp.path(), &["commit", "-q", "-m", "init harness, no data"]);
        // Exists on disk, deliberately never committed.
        std::fs::create_dir_all(harness.join("data")).unwrap();
        std::fs::write(harness.join("data/foo.txt"), "hello").unwrap();
        tmp
    }

    /// Same fixture, but the include target is committed too.
    fn init_repo_committed_include() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        run_git(tmp.path(), &["init", "-q"]);
        let harness = tmp.path().join(".hex/harness");
        std::fs::create_dir_all(harness.join("src")).unwrap();
        std::fs::create_dir_all(harness.join("data")).unwrap();
        std::fs::write(
            harness.join("Cargo.toml"),
            "[package]\nname = \"fixture-harness\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(harness.join("Cargo.lock"), FIXTURE_LOCKFILE).unwrap();
        std::fs::write(
            harness.join("src/lib.rs"),
            "pub const DATA: &str = include_str!(\"../data/foo.txt\");\n",
        )
        .unwrap();
        std::fs::write(harness.join("data/foo.txt"), "hello").unwrap();
        run_git(tmp.path(), &["add", "-A"]);
        run_git(
            tmp.path(),
            &["commit", "-q", "-m", "init harness with data"],
        );
        tmp
    }

    /// Fixture: harness `Cargo.toml` declares a path dependency on
    /// `../code-intel` (mirroring the real `.hex/code-intel` / `scipd`
    /// path dep from bug B2) and `Cargo.lock` is committed and does resolve
    /// it — but the `.hex/code-intel` directory itself exists only on
    /// disk in this fixture repo, never `git add`ed (the `.git/info/exclude`
    /// class of bug). A fresh `git worktree add` from HEAD therefore won't
    /// carry it, so `cargo metadata --locked --offline` must fail naming
    /// `scipd`.
    fn init_repo_missing_path_dep() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        run_git(tmp.path(), &["init", "-q"]);
        let harness = tmp.path().join(".hex/harness");
        let code_intel = tmp.path().join(".hex/code-intel");
        std::fs::create_dir_all(harness.join("src")).unwrap();
        std::fs::create_dir_all(code_intel.join("src")).unwrap();
        std::fs::write(
            harness.join("Cargo.toml"),
            "[package]\nname = \"fixture-harness\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [dependencies]\nscipd = { path = \"../code-intel\" }\n",
        )
        .unwrap();
        std::fs::write(harness.join("src/lib.rs"), "pub fn f() {}\n").unwrap();
        std::fs::write(
            code_intel.join("Cargo.toml"),
            "[package]\nname = \"scipd\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(code_intel.join("src/lib.rs"), "pub fn g() {}\n").unwrap();
        // Real lockfile resolving the `scipd` path dep, generated by hand
        // with `cargo generate-lockfile --offline` against this exact
        // fixture layout — verified to reproduce the `--offline` failure
        // below once `.hex/code-intel` is left uncommitted.
        std::fs::write(
            harness.join("Cargo.lock"),
            "# This file is automatically @generated by Cargo.\n\
             # It is not intended for manual editing.\n\
             version = 4\n\n\
             [[package]]\n\
             name = \"fixture-harness\"\n\
             version = \"0.1.0\"\n\
             dependencies = [\n \"scipd\",\n]\n\n\
             [[package]]\n\
             name = \"scipd\"\n\
             version = \"0.1.0\"\n",
        )
        .unwrap();
        // Only the harness side is committed; `.hex/code-intel` is
        // deliberately left untracked, on disk in THIS repo only.
        run_git(
            tmp.path(),
            &[
                "add",
                ".hex/harness/Cargo.toml",
                ".hex/harness/Cargo.lock",
                ".hex/harness/src/lib.rs",
            ],
        );
        run_git(
            tmp.path(),
            &[
                "commit",
                "-q",
                "-m",
                "init harness with uncommitted path dep",
            ],
        );
        tmp
    }

    /// Fixture: harness has NO `Cargo.lock` at all (never committed, never
    /// generated) — a fresh checkout that genuinely cannot build reproducibly
    /// offline. Review finding G1: the check must not silently skip step 2
    /// when no lockfile is tracked (that let an unbuildable checkout falsely
    /// PASS); `cargo metadata --locked` must actually run and fail naming the
    /// missing/needs-update lock file.
    fn init_repo_missing_lockfile() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        run_git(tmp.path(), &["init", "-q"]);
        let harness = tmp.path().join(".hex/harness");
        std::fs::create_dir_all(harness.join("src")).unwrap();
        std::fs::write(
            harness.join("Cargo.toml"),
            "[package]\nname = \"fixture-harness\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(harness.join("src/lib.rs"), "pub fn f() {}\n").unwrap();
        // Deliberately no Cargo.lock anywhere — not written to disk, not
        // committed.
        run_git(
            tmp.path(),
            &["add", ".hex/harness/Cargo.toml", ".hex/harness/src/lib.rs"],
        );
        run_git(
            tmp.path(),
            &["commit", "-q", "-m", "init harness, no lockfile"],
        );
        tmp
    }

    #[test]
    fn test_harness_buildable_fails_when_lockfile_missing() {
        let tmp = init_repo_missing_lockfile();
        let ctx = Context {
            hex_dir: tmp.path().to_path_buf(),
            home: PathBuf::from("/tmp"),
            fix: false,
        };
        let results = Runner::filtered("harness-buildable-from-git").run(&ctx);
        let (_, result) = results
            .iter()
            .find(|(name, _)| name == "harness-buildable-from-git")
            .expect("expected a doctor check named `harness-buildable-from-git` to run");
        assert_eq!(
            result.status,
            Status::Fail,
            "a harness with no Cargo.lock at all must FAIL cargo metadata \
             (review finding G1: it must not be silently skipped), got {:?}",
            result
        );
        assert_eq!(
            worktree_count(tmp.path()),
            1,
            "the check's temp `git worktree` must be removed after a FAIL run"
        );
    }

    #[test]
    fn test_harness_buildable_fails_and_names_missing_include_target() {
        let tmp = init_repo_missing_include();
        let ctx = Context {
            hex_dir: tmp.path().to_path_buf(),
            home: PathBuf::from("/tmp"),
            fix: false,
        };
        let results = Runner::filtered("harness-buildable-from-git").run(&ctx);
        let (_, result) = results
            .iter()
            .find(|(name, _)| name == "harness-buildable-from-git")
            .expect(
                "expected a doctor check named `harness-buildable-from-git` to run — \
                 not yet implemented (task Tv7psnbjd, src/doctor/checks/harness_buildable.rs)",
            );
        assert_eq!(
            result.status,
            Status::Fail,
            "an include target absent from git must FAIL, got {:?}",
            result
        );
        let msg = format!(
            "{} {}",
            result.message,
            result.details.clone().unwrap_or_default()
        );
        assert!(
            msg.contains("data/foo.txt"),
            "failure must name the missing path, got: {msg}"
        );
        assert!(
            msg.contains("lib.rs"),
            "failure must name the referencing file, got: {msg}"
        );
        assert_eq!(
            worktree_count(tmp.path()),
            1,
            "the check's temp `git worktree` must be removed after a FAIL run"
        );
    }

    #[test]
    fn test_harness_buildable_passes_when_include_target_committed() {
        let tmp = init_repo_committed_include();
        let ctx = Context {
            hex_dir: tmp.path().to_path_buf(),
            home: PathBuf::from("/tmp"),
            fix: false,
        };
        let results = Runner::filtered("harness-buildable-from-git").run(&ctx);
        let (_, result) = results
            .iter()
            .find(|(name, _)| name == "harness-buildable-from-git")
            .expect(
                "expected a doctor check named `harness-buildable-from-git` to run — \
                 not yet implemented (task Tv7psnbjd, src/doctor/checks/harness_buildable.rs)",
            );
        assert_eq!(
            result.status,
            Status::Pass,
            "a committed include target must PASS, got {:?}",
            result
        );
        assert_eq!(
            worktree_count(tmp.path()),
            1,
            "the check's temp `git worktree` must be removed after a PASS run"
        );
    }

    #[test]
    fn test_harness_buildable_fails_and_names_missing_path_dependency() {
        let tmp = init_repo_missing_path_dep();
        let ctx = Context {
            hex_dir: tmp.path().to_path_buf(),
            home: PathBuf::from("/tmp"),
            fix: false,
        };
        let results = Runner::filtered("harness-buildable-from-git").run(&ctx);
        let (_, result) = results
            .iter()
            .find(|(name, _)| name == "harness-buildable-from-git")
            .expect(
                "expected a doctor check named `harness-buildable-from-git` to run — \
                 not yet implemented (task Tv7psnbjd, src/doctor/checks/harness_buildable.rs)",
            );
        assert_eq!(
            result.status,
            Status::Fail,
            "a path dependency present on disk but absent from git must FAIL \
             cargo metadata, got {:?}",
            result
        );
        assert!(
            result.message.contains("scipd"),
            "failure must name the missing path dependency (scipd), got: {}",
            result.message
        );
        assert_eq!(
            worktree_count(tmp.path()),
            1,
            "the check's temp `git worktree` must be removed after a FAIL run"
        );
    }

    // ---- tests for task Thcdrea2q (arrra/hex PR #7 review round 1:
    // F15/F16, F13, F20, F18, F14, F12) ----

    /// Registers `path` as a git worktree of `dir` at HEAD, then deletes its
    /// on-disk directory WITHOUT deregistering it — exactly the "missing
    /// worktree" administrative state that a repo-wide `git worktree prune`
    /// cleans up. Used to prove F15/F16: this check's cleanup must never
    /// touch worktree registrations it did not create itself.
    fn register_dangling_worktree(dir: &std::path::Path) {
        let wt_tmp = tempfile::tempdir().unwrap();
        let wt_path = wt_tmp.keep();
        run_git(
            dir,
            &[
                "worktree",
                "add",
                "--detach",
                wt_path.to_str().unwrap(),
                "HEAD",
            ],
        );
        std::fs::remove_dir_all(&wt_path).unwrap();
    }

    #[test]
    fn test_unrelated_dangling_worktree_survives_a_passing_check() {
        let tmp = init_repo_committed_include();
        register_dangling_worktree(tmp.path());
        assert_eq!(
            worktree_count(tmp.path()),
            2,
            "sanity: main worktree + the unrelated dangling registration"
        );

        let ctx = Context {
            hex_dir: tmp.path().to_path_buf(),
            home: PathBuf::from("/tmp"),
            fix: false,
        };
        let results = Runner::filtered("harness-buildable-from-git").run(&ctx);
        let (_, result) = results
            .iter()
            .find(|(name, _)| name == "harness-buildable-from-git")
            .unwrap();
        assert_eq!(
            result.status,
            Status::Pass,
            "sanity: fixture is fully buildable, got {:?}",
            result
        );
        assert_eq!(
            worktree_count(tmp.path()),
            2,
            "F15/F16: an unrelated dangling worktree registration must survive \
             a PASSING check — cleanup must remove only this invocation's own \
             worktree, never run a repo-wide `git worktree prune`"
        );
    }

    #[test]
    fn test_unrelated_dangling_worktree_survives_a_failing_check() {
        let tmp = init_repo_missing_lockfile();
        register_dangling_worktree(tmp.path());
        assert_eq!(
            worktree_count(tmp.path()),
            2,
            "sanity: main worktree + the unrelated dangling registration"
        );

        let ctx = Context {
            hex_dir: tmp.path().to_path_buf(),
            home: PathBuf::from("/tmp"),
            fix: false,
        };
        let results = Runner::filtered("harness-buildable-from-git").run(&ctx);
        let (_, result) = results
            .iter()
            .find(|(name, _)| name == "harness-buildable-from-git")
            .unwrap();
        assert_eq!(
            result.status,
            Status::Fail,
            "sanity: fixture has no lockfile and must fail cargo metadata, got {:?}",
            result
        );
        assert_eq!(
            worktree_count(tmp.path()),
            2,
            "F15/F16: an unrelated dangling worktree registration must survive \
             a FAILING check — cleanup must remove only this invocation's own \
             worktree, never run a repo-wide `git worktree prune`"
        );
    }

    #[test]
    fn test_diagnostic_worktree_does_not_run_checkout_hooks() {
        let tmp = init_repo_committed_include();
        // A `post-checkout` hook that leaves a sentinel if it ever runs.
        // `git worktree add` triggers this hook by default and honors the
        // repo's `core.hooksPath` — F13 requires the diagnostic to override
        // that to an empty directory for this command only, so no checkout
        // automation (state mutation, network access) ever runs during an
        // ordinary health check.
        let hooks_dir = tmp.path().join("hostile-hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        let sentinel = tmp.path().join("hook-ran.sentinel");
        std::fs::write(
            hooks_dir.join("post-checkout"),
            format!("#!/bin/sh\ntouch '{}'\n", sentinel.display()),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                hooks_dir.join("post-checkout"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        run_git(
            tmp.path(),
            &["config", "core.hooksPath", hooks_dir.to_str().unwrap()],
        );

        let ctx = Context {
            hex_dir: tmp.path().to_path_buf(),
            home: PathBuf::from("/tmp"),
            fix: false,
        };
        let _ = Runner::filtered("harness-buildable-from-git").run(&ctx);

        assert!(
            !sentinel.exists(),
            "F13: the diagnostic `git worktree add` must not run the \
             repository's post-checkout hook (core.hooksPath must be \
             overridden to an empty dir for that command only)"
        );
    }

    #[test]
    fn test_diagnostic_worktree_materializes_full_checkout_despite_caller_sparse() {
        let tmp = init_repo_committed_include();
        // Turn the CALLER's checkout sparse, excluding the required tracked
        // include target. `core.sparseCheckout` and `info/sparse-checkout`
        // live in the shared git dir, so a naive `git worktree add` inherits
        // them into the new worktree too. F20: the diagnostic must
        // materialize a full checkout regardless, scoped to that worktree.
        run_git(tmp.path(), &["config", "core.sparseCheckout", "true"]);
        std::fs::write(
            tmp.path().join(".git/info/sparse-checkout"),
            "/*\n!/.hex/harness/data/\n",
        )
        .unwrap();
        run_git(tmp.path(), &["read-tree", "-m", "-u", "HEAD"]);
        assert!(
            !tmp.path().join(".hex/harness/data/foo.txt").exists(),
            "sanity: sparse-checkout must have excluded the data dir from the \
             caller's own working copy"
        );

        let ctx = Context {
            hex_dir: tmp.path().to_path_buf(),
            home: PathBuf::from("/tmp"),
            fix: false,
        };
        let results = Runner::filtered("harness-buildable-from-git").run(&ctx);
        let (_, result) = results
            .iter()
            .find(|(name, _)| name == "harness-buildable-from-git")
            .unwrap();
        assert_eq!(
            result.status,
            Status::Pass,
            "F20: a tracked include target excluded only by the CALLER's \
             sparse-checkout patterns must still be present in the \
             diagnostic worktree, got {:?}",
            result
        );
    }

    #[test]
    fn test_fixture_commit_survives_inherited_signing_and_hooks_config() {
        // F18: a machine with `commit.gpgsign` and `core.hooksPath`
        // configured locally must not be able to break fixture setup —
        // `run_git`'s commit step must override both, command-local, without
        // ever mutating process-global env (tests run concurrently).
        let tmp = tempfile::tempdir().unwrap();
        run_git(tmp.path(), &["init", "-q"]);
        run_git(tmp.path(), &["config", "commit.gpgsign", "true"]);
        run_git(
            tmp.path(),
            &[
                "config",
                "gpg.program",
                "/nonexistent-hex-doctor-f18-fixture-gpg",
            ],
        );
        let hooks_dir = tmp.path().join("hostile-hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        std::fs::write(
            hooks_dir.join("pre-commit"),
            "#!/bin/sh\necho blocked by hostile pre-commit hook >&2\nexit 1\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                hooks_dir.join("pre-commit"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        run_git(
            tmp.path(),
            &["config", "core.hooksPath", hooks_dir.to_str().unwrap()],
        );

        std::fs::write(tmp.path().join("file.txt"), "x").unwrap();
        run_git(tmp.path(), &["add", "file.txt"]);
        // Must succeed despite the hostile local config above — today this
        // panics inside `run_git` because it inherits both settings
        // unmodified.
        run_git(tmp.path(), &["commit", "-q", "-m", "fixture commit"]);
    }

    #[test]
    fn test_harness_buildable_bounds_command_runtime_when_git_hangs() {
        // F14: every external command this check spawns must run under a
        // deadline. Review finding F14 names "Git checkout filters can
        // block" as a hang vector distinct from checkout HOOKS — a content
        // filter driver (`.gitattributes` `filter=`) is not neutralized by
        // F13's `core.hooksPath` override (verified: a hostile
        // `post-checkout` HOOK is correctly suppressed by that override and
        // can no longer simulate a hang here), so a hanging `smudge` filter
        // is the mechanism that actually exercises this deadline during
        // `git worktree add`'s checkout.
        let tmp = init_repo_committed_include();
        run_git(
            tmp.path(),
            &["config", "filter.hex-doctor-f14-hang.clean", "cat"],
        );
        run_git(
            tmp.path(),
            &[
                "config",
                "filter.hex-doctor-f14-hang.smudge",
                "sleep 30 && cat",
            ],
        );
        run_git(
            tmp.path(),
            &["config", "filter.hex-doctor-f14-hang.required", "true"],
        );
        std::fs::write(
            tmp.path().join(".hex/harness/data/.gitattributes"),
            "foo.txt filter=hex-doctor-f14-hang\n",
        )
        .unwrap();
        run_git(tmp.path(), &["add", ".hex/harness/data/.gitattributes"]);
        run_git(
            tmp.path(),
            &[
                "commit",
                "-q",
                "-m",
                "add hanging smudge filter for foo.txt",
            ],
        );

        let start = std::time::Instant::now();
        // `run_check_with_timeout` does not exist yet — this is the
        // testable entry point F14 asks for ("configurable via the check's
        // params"), a thin wrapper the production `run_check` should call
        // with a much larger default deadline.
        let result = crate::doctor::checks::harness_buildable::run_check_with_timeout(
            tmp.path(),
            std::time::Duration::from_millis(500),
        );
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(10),
            "F14: a hung external command must be killed at its deadline \
             instead of blocking for the hook's full 30s hang — took {:?}",
            elapsed
        );
        assert_eq!(
            result.status,
            Status::Fail,
            "a timed-out diagnostic command must be reported as a failure, \
             got {:?}",
            result
        );
        let msg = result.message.to_lowercase();
        assert!(
            msg.contains("timeout") || msg.contains("timed out"),
            "the failure must name the timeout as the cause, got: {}",
            result.message
        );
    }

    #[test]
    fn test_host_triple_runs_rustc_with_given_current_dir() {
        // F12: `host_triple` must resolve the toolchain in the SAME
        // directory context `cargo metadata` uses (the harness dir inside
        // the diagnostic worktree), not doctor's own ambient cwd. Passing a
        // directory that does not exist proves the argument is actually
        // honored (spawning `rustc` there must fail) rather than silently
        // falling back to the ambient cwd.
        let bogus_dir = std::path::Path::new("/nonexistent-hex-doctor-f12-fixture-dir");
        assert!(
            !bogus_dir.exists(),
            "test precondition: fixture path must not exist"
        );
        let result = crate::doctor::checks::harness_buildable::host_triple(bogus_dir);
        assert!(
            result.is_none(),
            "host_triple must run `rustc -vV` with current_dir set to the \
             given directory (F12) — got a result even though that directory \
             does not exist: {:?}",
            result
        );
    }

    // ---- tests for review round 1's follow-up findings (G1, G2 — from the
    // `review_b` pass on task Thcdrea2q) ----

    /// Recursively reinstates `0o755` and removes `path` — best-effort, used
    /// only to undo the fault injection this test module performs on itself
    /// (a deliberately unremovable directory, to force the check's own
    /// cleanup to fail) so it doesn't leak a permission-locked directory in
    /// the OS temp dir across test runs.
    #[cfg(unix)]
    fn force_remove_dir_all(path: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
        if path.is_dir() {
            if let Ok(entries) = std::fs::read_dir(path) {
                for entry in entries.flatten() {
                    force_remove_dir_all(&entry.path());
                }
            }
        }
        let _ = std::fs::remove_dir_all(path);
    }

    /// Monotonic source for `unique_fault_injection_token` — see there.
    #[cfg(unix)]
    static FAULT_INJECTION_TOKEN_COUNTER: std::sync::atomic::AtomicU64 =
        std::sync::atomic::AtomicU64::new(0);

    /// A token unique to ONE invocation of the self-locking cleanup fault
    /// injection (`add_self_locking_cleanup_trap` /
    /// `sweep_leaked_harness_buildable_tempdirs` below). Combines the
    /// process id with a monotonic counter so that two invocations —
    /// whether two tests in this binary running concurrently under
    /// `cargo test`'s default parallelism, or two separate `cargo test`
    /// processes on a shared host — never mint the same token (G3
    /// follow-up).
    #[cfg(unix)]
    fn unique_fault_injection_token() -> String {
        let n = FAULT_INJECTION_TOKEN_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        format!("hex-doctor-g1-selflock-{}-{}", std::process::id(), n)
    }

    /// Sweeps the OS temp dir for leaked `hex-doctor-harness-buildable-*`
    /// directories that carry THIS invocation's own fault-injection
    /// signature (`.hex/harness/aaa_locked` containing exactly `token`,
    /// written only by `add_self_locking_cleanup_trap` below) and
    /// force-removes them. Used before/after the G1 fault-injection tests
    /// below so a cleanup failure they deliberately cause doesn't leak a
    /// permission-locked directory into `/tmp` for the rest of the suite
    /// (or the next CI run) to trip over.
    ///
    /// G3: an earlier version matched on the check's own tempdir PREFIX
    /// alone and force-removed every match. Under `cargo test`'s default
    /// parallelism (or a real `hex doctor` run happening concurrently on
    /// the same host), that prefix is shared with every OTHER in-flight
    /// `harness-buildable-from-git` diagnostic worktree — the sweep could
    /// delete a live worktree belonging to an unrelated, still-running
    /// check.
    ///
    /// G3 follow-up: gating on the mere PRESENCE of
    /// `.hex/harness/aaa_locked` was still not enough — every invocation of
    /// the fault injection wrote the exact same fixed marker content, so
    /// two invocations of it produced indistinguishable directories and
    /// one invocation's sweep could delete another, still-running
    /// invocation's live worktree. Requiring the marker's CONTENT to match
    /// this invocation's own unique `token` scopes the sweep to exactly
    /// the directory THIS invocation created.
    #[cfg(unix)]
    fn sweep_leaked_harness_buildable_tempdirs(token: &str) {
        let base = std::env::temp_dir();
        let Ok(entries) = std::fs::read_dir(&base) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let marker = path.join(".hex/harness/aaa_locked/file.txt");
            let is_our_own_fault_injection = entry
                .file_name()
                .to_string_lossy()
                .starts_with("hex-doctor-harness-buildable-")
                && std::fs::read_to_string(&marker)
                    .map(|content| content == token)
                    .unwrap_or(false);
            if is_our_own_fault_injection {
                force_remove_dir_all(&path);
            }
        }
    }

    /// G3: an earlier version of `sweep_leaked_harness_buildable_tempdirs`
    /// matched on the check's own tempdir PREFIX alone and force-removed
    /// every match. Under `cargo test`'s default parallelism (or a real
    /// `hex doctor` run happening concurrently on the same host), that
    /// prefix is shared with every OTHER in-flight
    /// `harness-buildable-from-git` diagnostic worktree — the sweep could
    /// delete a live worktree belonging to an unrelated, still-running
    /// check. Plant a decoy directory with the exact prefix but WITHOUT
    /// this test module's own fault-injection signature
    /// (`.hex/harness/aaa_locked`, written only by
    /// `add_self_locking_cleanup_trap`) and assert the sweep leaves it
    /// alone.
    #[cfg(unix)]
    #[test]
    fn test_cleanup_sweep_never_touches_an_unrelated_matching_prefix_tempdir() {
        let decoy = tempfile::Builder::new()
            .prefix("hex-doctor-harness-buildable-")
            .tempdir()
            .expect("failed to create decoy tempdir");
        std::fs::create_dir_all(decoy.path().join("some-other-checks-live-worktree"))
            .expect("failed to populate decoy");

        sweep_leaked_harness_buildable_tempdirs(&unique_fault_injection_token());

        assert!(
            decoy.path().exists(),
            "G3: the sweep must not delete an unrelated matching-prefix \
             tempdir that lacks this test module's own fault-injection \
             signature — doing so would delete another concurrently \
             running check's live worktree"
        );
    }

    /// G3 follow-up (review_b, iteration 2): gating the sweep on the mere
    /// PRESENCE of `.hex/harness/aaa_locked` is not enough — every
    /// invocation of `add_self_locking_cleanup_trap` wrote the exact same
    /// fixed marker content, so two invocations of the fault injection
    /// (e.g. two of the tests below running concurrently under `cargo
    /// test`'s default parallelism, or two separate `cargo test` processes
    /// on a shared CI host) produced INDISTINGUISHABLE directories: one
    /// invocation's sweep could delete another, still-running invocation's
    /// live worktree. Plant a decoy that carries the signature PATH but a
    /// DIFFERENT invocation's token as its content (simulating a
    /// concurrently running invocation of the same fault injection) and
    /// assert this invocation's sweep — scoped to its own unique token —
    /// leaves it alone.
    #[cfg(unix)]
    #[test]
    fn test_cleanup_sweep_never_touches_a_concurrent_invocations_matching_signature_tempdir() {
        let decoy = tempfile::Builder::new()
            .prefix("hex-doctor-harness-buildable-")
            .tempdir()
            .expect("failed to create decoy tempdir");
        std::fs::create_dir_all(decoy.path().join(".hex/harness/aaa_locked"))
            .expect("failed to populate decoy");
        std::fs::write(
            decoy.path().join(".hex/harness/aaa_locked/file.txt"),
            "some-other-concurrent-invocations-token",
        )
        .expect("failed to write decoy's marker content");

        let my_token = unique_fault_injection_token();
        sweep_leaked_harness_buildable_tempdirs(&my_token);

        assert!(
            decoy.path().exists(),
            "G3: the sweep must not delete a directory carrying a \
             DIFFERENT invocation's fault-injection signature — doing so \
             could delete another concurrently running invocation's live \
             worktree"
        );
    }

    /// Deterministically forces the check's OWN diagnostic worktree cleanup
    /// to fail, without timing/race dependence or process-global side
    /// effects: a `smudge` filter on a small trigger file `chmod 000`s a
    /// SIBLING directory that sorts earlier in git's checkout order (so its
    /// own content is already fully written by the time the filter runs).
    /// After checkout, that directory can no longer be deleted by `git
    /// worktree remove --force` or by a plain recursive filesystem removal,
    /// so cleanup deterministically fails — reproducing a real leaked
    /// worktree (e.g. from a permissions/ACL quirk), rather than simulating
    /// one.
    #[cfg(unix)]
    fn add_self_locking_cleanup_trap(tmp: &std::path::Path, token: &str) {
        run_git(
            tmp,
            &[
                "config",
                "filter.hex-doctor-g1-selflock.smudge",
                "chmod 000 .hex/harness/aaa_locked; cat",
            ],
        );
        run_git(
            tmp,
            &["config", "filter.hex-doctor-g1-selflock.clean", "cat"],
        );
        run_git(
            tmp,
            &["config", "filter.hex-doctor-g1-selflock.required", "true"],
        );
        let harness = tmp.join(".hex/harness");
        std::fs::create_dir_all(harness.join("aaa_locked")).unwrap();
        std::fs::write(harness.join("aaa_locked/file.txt"), token).unwrap();
        std::fs::write(harness.join("zzz_trigger.txt"), "trigger").unwrap();
        std::fs::write(
            harness.join(".gitattributes"),
            "zzz_trigger.txt filter=hex-doctor-g1-selflock\n",
        )
        .unwrap();
        run_git(
            tmp,
            &[
                "add",
                ".hex/harness/aaa_locked/file.txt",
                ".hex/harness/zzz_trigger.txt",
                ".hex/harness/.gitattributes",
            ],
        );
        run_git(
            tmp,
            &["commit", "-q", "-m", "add self-locking cleanup trap"],
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_cleanup_failure_surfaces_on_an_intermediate_failure_path() {
        // G1: earlier code only called `WorktreeGuard::cleanup()` from the
        // single FINAL return (the `missing.is_empty()` branch at the end
        // of the old monolithic function) — every earlier `return
        // CheckResult::fail(...)` (e.g. the `cargo metadata` failure below)
        // skipped cleanup entirely and left it to `Drop`, which discards
        // any cleanup error silently. Combine a diagnostic that fails
        // BEFORE the old final-return site (no Cargo.lock) with a cleanup
        // that itself deterministically fails: the returned message must
        // name BOTH problems, proving cleanup now runs — and its failure is
        // surfaced — on this early path too, not just the final one.
        let token = unique_fault_injection_token();
        sweep_leaked_harness_buildable_tempdirs(&token);
        let tmp = init_repo_missing_lockfile();
        add_self_locking_cleanup_trap(tmp.path(), &token);

        let result = crate::doctor::checks::harness_buildable::run_check_with_timeout(
            tmp.path(),
            std::time::Duration::from_secs(30),
        );

        assert_eq!(
            result.status,
            Status::Fail,
            "sanity: no Cargo.lock must still fail cargo metadata, got {:?}",
            result
        );
        let msg = result.message.to_lowercase();
        assert!(
            msg.contains("cargo metadata") || msg.contains("lockfile") || msg.contains("lock"),
            "must still name the original cargo-metadata failure, got: {}",
            result.message
        );
        assert!(
            msg.contains("cleanup"),
            "G1: an early `return` before the old final cleanup call must still \
             surface a cleanup failure (not silently rely on Drop), got: {}",
            result.message
        );

        sweep_leaked_harness_buildable_tempdirs(&token);
    }

    #[cfg(unix)]
    #[test]
    fn test_cleanup_failure_downgrades_an_otherwise_passing_result_to_fail() {
        // G1: on the one path that DID call `cleanup()` explicitly (the
        // final, success return), a cleanup error was appended to the
        // message as a "(cleanup warning: ...)" suffix but `Status::Pass`
        // was preserved — a doctor check reporting PASS while it just
        // leaked a worktree it could not clean up is a silent failure (SO
        // S6: no quiet failures). A fully buildable fixture plus a
        // deterministic cleanup trap must now report FAIL, not PASS.
        let token = unique_fault_injection_token();
        sweep_leaked_harness_buildable_tempdirs(&token);
        let tmp = init_repo_committed_include();
        add_self_locking_cleanup_trap(tmp.path(), &token);

        let result = crate::doctor::checks::harness_buildable::run_check_with_timeout(
            tmp.path(),
            std::time::Duration::from_secs(30),
        );

        assert_eq!(
            result.status,
            Status::Fail,
            "G1: a cleanup failure must never leave the result as Status::Pass, \
             got {:?}",
            result
        );
        let msg = result.message.to_lowercase();
        assert!(
            msg.contains("cleanup"),
            "failure must name the cleanup problem, got: {}",
            result.message
        );

        sweep_leaked_harness_buildable_tempdirs(&token);

        // F6 (review R4, reopened): `sweep_leaked_harness_buildable_tempdirs`
        // recognizes its own leaked directory by reading
        // `.hex/harness/aaa_locked/file.txt` and comparing its content to
        // `token` — but `add_self_locking_cleanup_trap` chmod 000s that very
        // `aaa_locked` directory during checkout to force the deterministic
        // cleanup failure this test exercises. `read_to_string` on the
        // marker then fails with EACCES, and `.unwrap_or(false)` treats that
        // as "not mine", so the sweep call directly above silently leaves
        // this test's own permission-locked worktree behind in the OS temp
        // dir instead of removing it. Walk the temp dir directly, restoring
        // read/execute permission on any matching `aaa_locked` directory so
        // its marker can actually be inspected (a fixed sweep must do the
        // same internally to have any chance of finding its own directory),
        // and fail loudly if anything carrying this invocation's token
        // survived the sweep above. This must fail today and pass once the
        // sweep can see past its own chmod-000 marker.
        use std::os::unix::fs::PermissionsExt;
        for entry in std::fs::read_dir(std::env::temp_dir())
            .expect("failed to read OS temp dir")
            .flatten()
        {
            let leaked = entry.path();
            if !entry
                .file_name()
                .to_string_lossy()
                .starts_with("hex-doctor-harness-buildable-")
            {
                continue;
            }
            let locked_dir = leaked.join(".hex/harness/aaa_locked");
            if locked_dir.is_dir() {
                let _ =
                    std::fs::set_permissions(&locked_dir, std::fs::Permissions::from_mode(0o755));
            }
            let marker = locked_dir.join("file.txt");
            let is_ours = std::fs::read_to_string(&marker)
                .map(|content| content == token)
                .unwrap_or(false);
            if is_ours {
                force_remove_dir_all(&leaked);
                panic!(
                    "F6: sweep_leaked_harness_buildable_tempdirs left behind \
                     this invocation's own chmod-000 leaked worktree at {} — \
                     its marker was unreadable (EACCES) so the sweep's \
                     `.unwrap_or(false)` treated it as not-ours and skipped \
                     removing it",
                    leaked.display()
                );
            }
        }
    }

    #[test]
    fn test_run_with_timeout_bounds_pipe_drain_when_descendant_holds_stdout_open() {
        // G2: `try_wait` observing the direct child's exit does not mean its
        // stdout/stderr pipes are closed. Backgrounding a longer-lived
        // descendant that inherits the child's stdout fd and then letting
        // the direct child (`sh`) exit immediately reproduces exactly that:
        // an unbounded `.join()` on the drain thread would block for as
        // long as the descendant lives (verified by hand: this exact shell
        // snippet piped to `cat` takes as long as the backgrounded sleep,
        // not as long as `sh` itself). `run_with_timeout` must bound the
        // drain instead of hanging past its own configured deadline.
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c").arg("(sleep 30 >&1 &) ; exit 0");
        let start = std::time::Instant::now();
        let result = crate::doctor::checks::harness_buildable::run_with_timeout(
            &mut cmd,
            std::time::Duration::from_millis(500),
        );
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(10),
            "G2: a descendant holding the pipe open after the direct child \
             exits must not block the drain for its full lifetime — took {:?}",
            elapsed
        );
        assert!(
            result.is_err(),
            "a drain that times out waiting for the descendant's pipe to \
             close must be reported as an error, got: {:?}",
            result
        );
    }

    #[test]
    fn test_run_with_timeout_shares_one_drain_budget_across_stdout_and_stderr() {
        // G2 (follow-up from review_b): the first fix computed a single
        // `drain_budget` up front but then handed that SAME, un-shrunk
        // value to BOTH `recv_timeout` calls — a stdout pipe that closes
        // only after consuming most of the budget still let stderr wait for
        // a second FULL budget on top, so a staggered stdout/stderr closure
        // could push total drain time to roughly double the configured
        // timeout. Here the first backgrounded descendant releases stdout
        // only after 800ms (most of the 1000ms overall timeout, but still
        // "within budget"), while a second descendant holds stderr open for
        // 30s. A single shared deadline caps total elapsed near the
        // configured 1000ms; reusing the full budget per pipe pushes it
        // toward ~1800ms.
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c")
            .arg("(sleep 0.8 >&1 2>/dev/null &) ; (sleep 30 >/dev/null &) ; exit 0");
        let start = std::time::Instant::now();
        let result = crate::doctor::checks::harness_buildable::run_with_timeout(
            &mut cmd,
            std::time::Duration::from_millis(1000),
        );
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(1500),
            "G2: stdout and stderr must share a single drain deadline, not \
             each get the full configured timeout — a stdout pipe that \
             closes late must not let stderr's wait start a fresh clock. \
             Took {:?} (bug reuses the full budget per pipe, ~1.8s; fix \
             shares one budget, ~1s)",
            elapsed
        );
        assert!(
            result.is_err(),
            "stderr never closes in this fixture (30s sleep), so the \
             overall call must still report a timeout error, got: {:?}",
            result
        );
    }
}
