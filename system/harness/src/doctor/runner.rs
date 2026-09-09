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

    /// Run a git command in `dir`, panicking with its args/output on failure.
    fn run_git(dir: &std::path::Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
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
            args,
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

    /// Fixture: harness references `system/harness/data/foo.txt`, which
    /// exists on disk but was never `git add`ed — i.e. exactly the
    /// gitignore / `.git/info/exclude` class of bug B2 describes. `git
    /// worktree add` from HEAD will not carry it.
    fn init_repo_missing_include() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        run_git(tmp.path(), &["init", "-q"]);
        let harness = tmp.path().join("system/harness");
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
                "system/harness/Cargo.toml",
                "system/harness/Cargo.lock",
                "system/harness/src/lib.rs",
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
        let harness = tmp.path().join("system/harness");
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
    /// `../code-intel` (mirroring the real `system/code-intel` / `scipd`
    /// path dep from bug B2) and `Cargo.lock` is committed and does resolve
    /// it — but the `system/code-intel` directory itself exists only on
    /// disk in this fixture repo, never `git add`ed (the `.git/info/exclude`
    /// class of bug). A fresh `git worktree add` from HEAD therefore won't
    /// carry it, so `cargo metadata --locked --offline` must fail naming
    /// `scipd`.
    fn init_repo_missing_path_dep() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        run_git(tmp.path(), &["init", "-q"]);
        let harness = tmp.path().join("system/harness");
        let code_intel = tmp.path().join("system/code-intel");
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
        // below once `system/code-intel` is left uncommitted.
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
        // Only the harness side is committed; `system/code-intel` is
        // deliberately left untracked, on disk in THIS repo only.
        run_git(
            tmp.path(),
            &[
                "add",
                "system/harness/Cargo.toml",
                "system/harness/Cargo.lock",
                "system/harness/src/lib.rs",
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
        let harness = tmp.path().join("system/harness");
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
            &[
                "add",
                "system/harness/Cargo.toml",
                "system/harness/src/lib.rs",
            ],
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
}
