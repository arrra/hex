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
    ///
    /// G1: `submodule add` gets the same `core.hooksPath=<empty dir>`
    /// override — it stages the new gitlink/`.gitmodules` into this repo's
    /// index and, like `commit`, honors a locally-configured
    /// `post-index-change` hook while doing so. The subcommand is found by
    /// skipping any caller-supplied leading `-c key=value` pairs (e.g. the
    /// F11 submodule fixture's `-c protocol.file.allow=always`).
    fn run_git(dir: &std::path::Path, args: &[&str]) {
        let mut i = 0;
        while i + 1 < args.len() && args[i] == "-c" {
            i += 2;
        }
        let subcommand = args.get(i).copied();

        let mut full_args: Vec<String> = Vec::new();
        if matches!(subcommand, Some("commit") | Some("submodule")) {
            full_args.push("-c".to_string());
            full_args.push(format!("core.hooksPath={}", empty_hooks_dir().display()));
        }
        if subcommand == Some("commit") {
            full_args.push("-c".to_string());
            full_args.push("commit.gpgsign=false".to_string());
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
    /// A dep-free `Cargo.lock` matching this cargo's own lockfile format
    /// (verified by hand with `cargo generate-lockfile --offline` against a
    /// throwaway dep-free crate). Committing this into every fixture means
    /// the check's step-2 `cargo metadata --locked --offline` branch — which
    /// only runs when a `Cargo.lock` is tracked — actually executes under
    /// test, instead of being silently skipped (the gap review flagged: F2).
    const FIXTURE_LOCKFILE: &str = "# This file is automatically @generated by Cargo.\n# It is not intended for manual editing.\nversion = 4\n\n[[package]]\nname = \"fixture-harness\"\nversion = \"0.1.0\"\n";

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
    // ---- fixtures + tests for task Tygqwp59m (arrra/hex PR #7 round 2:
    // replace the source scanner with a real build) ----
    //
    // Every FAIL case below pins the check's actual purpose: a `mod`/
    // `include!` target (or manifest target) present on disk but absent
    // from git must fail *by compilation* against the exported committed
    // tree, never by re-implementing a scan of what cargo would do.

    /// Manifest for the fixtures below — package name MUST be
    /// `hex-harness`, matching the literal `-p hex-harness` the production
    /// check hardcodes (see `run_check_with_timeout`), so these fixtures
    /// exercise the exact command the real check runs.
    const HEX_HARNESS_MANIFEST: &str =
        "[package]\nname = \"hex-harness\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";

    /// A dep-free `Cargo.lock` matching this cargo's own lockfile format —
    /// same rationale as `FIXTURE_LOCKFILE` above, renamed to match
    /// `HEX_HARNESS_MANIFEST`'s package name so `--locked` resolves
    /// cleanly.
    const HEX_HARNESS_LOCKFILE: &str = "# This file is automatically @generated by Cargo.\n# It is not intended for manual editing.\nversion = 4\n\n[[package]]\nname = \"hex-harness\"\nversion = \"0.1.0\"\n";

    /// Builds a fixture repo whose root is the repo root (matching a real
    /// hex instance's `$HEX_DIR`) with `committed` files (plus the
    /// manifest/lockfile above) committed to `HEAD`, then writes
    /// `uncommitted` files to disk afterward without ever `git add`ing
    /// them — the exact "present on disk, absent from git" shape every
    /// FAIL case in this task's contract pins. Every path is repo-root
    /// relative (e.g. `.hex/harness/src/lib.rs`).
    fn init_hex_harness_repo(
        committed: &[(&str, &str)],
        uncommitted: &[(&str, &str)],
    ) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        run_git(tmp.path(), &["init", "-q"]);
        let mut all_committed: Vec<(&str, &str)> = vec![
            (".hex/harness/Cargo.toml", HEX_HARNESS_MANIFEST),
            (".hex/harness/Cargo.lock", HEX_HARNESS_LOCKFILE),
        ];
        all_committed.extend(committed.iter().copied());
        for (rel, content) in &all_committed {
            let path = tmp.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, content).unwrap();
        }
        run_git(tmp.path(), &["add", "-A"]);
        run_git(tmp.path(), &["commit", "-q", "-m", "fixture"]);
        for (rel, content) in uncommitted {
            let path = tmp.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, content).unwrap();
        }
        tmp
    }

    /// Runs the real doctor check against fixture repo root `tmp`, with a
    /// generous timeout — used by every PASS/FAIL test below where the
    /// wall-clock cap itself is not under test.
    fn run_harness_buildable(tmp: &std::path::Path) -> CheckResult {
        crate::doctor::checks::harness_buildable::run_check_with_timeout(
            tmp,
            std::time::Duration::from_secs(120),
        )
    }

    #[test]
    fn test_harness_buildable_passes_for_a_committed_buildable_crate() {
        // (a)
        let tmp = init_hex_harness_repo(
            &[(".hex/harness/src/lib.rs", "pub fn f() -> i32 { 1 }\n")],
            &[],
        );
        let result = run_harness_buildable(tmp.path());
        assert_eq!(
            result.status,
            Status::Pass,
            "a fully committed, buildable crate must PASS: {result:?}"
        );
    }

    #[test]
    fn test_harness_buildable_fails_naming_a_mod_declaration_missing_from_git() {
        // (b) — this check's whole purpose.
        let tmp = init_hex_harness_repo(
            &[(".hex/harness/src/lib.rs", "mod missing;\n")],
            &[(".hex/harness/src/missing.rs", "pub const X: i32 = 1;\n")],
        );
        let result = run_harness_buildable(tmp.path());
        assert_eq!(
            result.status,
            Status::Fail,
            "`mod missing;` whose file exists on disk but is not committed \
             must FAIL by compilation: {result:?}"
        );
        assert!(
            result.message.contains("missing"),
            "the diagnostic must name the missing module, got: {}",
            result.message
        );
    }

    #[test]
    fn test_harness_buildable_fails_naming_an_include_target_missing_from_git() {
        // (c)
        let tmp = init_hex_harness_repo(
            &[(
                ".hex/harness/src/lib.rs",
                "pub const DATA: &str = include_str!(\"data.txt\");\n",
            )],
            &[(".hex/harness/src/data.txt", "hello")],
        );
        let result = run_harness_buildable(tmp.path());
        assert_eq!(
            result.status,
            Status::Fail,
            "an include_str! target present on disk but absent from git \
             must FAIL by compilation: {result:?}"
        );
        assert!(
            result.message.contains("data.txt"),
            "the diagnostic must name the missing include target, got: {}",
            result.message
        );
    }

    #[test]
    fn test_harness_buildable_passes_when_missing_module_is_gated_by_an_inactive_cfg() {
        // (d)
        let tmp = init_hex_harness_repo(
            &[(
                ".hex/harness/src/lib.rs",
                "#[cfg(any())]\nmod missing;\n\npub fn f() -> i32 { 1 }\n",
            )],
            &[],
        );
        let result = run_harness_buildable(tmp.path());
        assert_eq!(
            result.status,
            Status::Pass,
            "a `mod` gated behind an always-false #[cfg(...)] is never \
             compiled by cargo, so a missing target there must not FAIL: \
             {result:?}"
        );
    }

    #[test]
    fn test_harness_buildable_fails_naming_a_missing_nested_module() {
        // (e) — G5 (review_b minor): the outer module must be declared
        // INLINE (`mod outer { mod inner; }` inside lib.rs itself), not as
        // its own file — this is the specific regression class ("inline
        // module context") the hand-written source scanner repeatedly got
        // wrong across earlier review rounds. rustc still resolves the
        // nested `mod inner;` to `src/outer/inner.rs` even though `outer`
        // has no file of its own (verified by hand: `cargo check` against
        // this exact shape reports `file not found for module \`inner\``
        // pointing at `src/outer/inner.rs`).
        let tmp = init_hex_harness_repo(
            &[(
                ".hex/harness/src/lib.rs",
                "pub mod outer {\n    pub mod inner;\n}\n",
            )],
            &[(".hex/harness/src/outer/inner.rs", "pub const X: i32 = 1;\n")],
        );
        let result = run_harness_buildable(tmp.path());
        assert_eq!(
            result.status,
            Status::Fail,
            "an inline `mod outer {{ mod inner; }}` whose nested module's \
             own file (src/outer/inner.rs) is on disk but not committed \
             must FAIL: {result:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_harness_buildable_passes_when_include_target_is_a_committed_symlink() {
        // (f)
        let tmp = tempfile::tempdir().unwrap();
        run_git(tmp.path(), &["init", "-q"]);
        let harness = tmp.path().join(".hex/harness");
        std::fs::create_dir_all(harness.join("src")).unwrap();
        std::fs::write(harness.join("Cargo.toml"), HEX_HARNESS_MANIFEST).unwrap();
        std::fs::write(harness.join("Cargo.lock"), HEX_HARNESS_LOCKFILE).unwrap();
        std::fs::write(
            harness.join("src/lib.rs"),
            "pub const DATA: &str = include_str!(\"data.txt\");\n",
        )
        .unwrap();
        std::fs::write(harness.join("src/real_data.txt"), "hello via symlink").unwrap();
        std::os::unix::fs::symlink("real_data.txt", harness.join("src/data.txt")).unwrap();
        run_git(tmp.path(), &["add", "-A"]);
        run_git(tmp.path(), &["commit", "-q", "-m", "fixture with symlink"]);

        let result = run_harness_buildable(tmp.path());
        assert_eq!(
            result.status,
            Status::Pass,
            "a committed symlink (git mode 120000) resolving to a \
             committed target must PASS: {result:?}"
        );
    }

    #[test]
    fn test_harness_buildable_fails_using_committed_manifest_bytes_even_under_a_smudge_filter() {
        // (g)
        let tmp = tempfile::tempdir().unwrap();
        run_git(tmp.path(), &["init", "-q"]);
        run_git(
            tmp.path(),
            &[
                "config",
                "filter.hex-buildable-manifest-fixup.smudge",
                "sed 's/missing_target/lib/'",
            ],
        );
        run_git(
            tmp.path(),
            &["config", "filter.hex-buildable-manifest-fixup.clean", "cat"],
        );
        run_git(
            tmp.path(),
            &[
                "config",
                "filter.hex-buildable-manifest-fixup.required",
                "true",
            ],
        );
        let harness = tmp.path().join(".hex/harness");
        std::fs::create_dir_all(harness.join("src")).unwrap();
        std::fs::write(
            harness.join(".gitattributes"),
            "Cargo.toml filter=hex-buildable-manifest-fixup\n",
        )
        .unwrap();
        // The COMMITTED manifest points `[lib]` at a target that is never
        // committed nor ever created on disk — genuinely broken. A smudge
        // filter configured on Cargo.toml would, on an ORDINARY checkout,
        // rewrite this to point at the real (present) `src/lib.rs` — the
        // check must never see that rewritten copy.
        std::fs::write(
            harness.join("Cargo.toml"),
            "[package]\nname = \"hex-harness\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [lib]\npath = \"src/missing_target.rs\"\n",
        )
        .unwrap();
        std::fs::write(harness.join("Cargo.lock"), HEX_HARNESS_LOCKFILE).unwrap();
        std::fs::write(harness.join("src/lib.rs"), "pub fn f() -> i32 { 1 }\n").unwrap();
        run_git(tmp.path(), &["add", "-A"]);
        run_git(
            tmp.path(),
            &[
                "commit",
                "-q",
                "-m",
                "fixture with smudge-filtered manifest",
            ],
        );

        let result = run_harness_buildable(tmp.path());
        assert_eq!(
            result.status,
            Status::Fail,
            "the check must build from the COMMITTED Cargo.toml (which \
             points `[lib]` at a target that does not exist), never from a \
             locally configured smudge filter's rewritten copy that would \
             have built: {result:?}"
        );
    }

    #[test]
    fn test_harness_buildable_never_triggers_checkout_or_index_hooks() {
        // (h)
        let tmp = init_hex_harness_repo(
            &[(".hex/harness/src/lib.rs", "pub fn f() -> i32 { 1 }\n")],
            &[],
        );
        let sentinel = tmp.path().join("HOOK_FIRED.marker");
        for hook_name in ["post-checkout", "post-index-change"] {
            let hook_path = tmp.path().join(".git/hooks").join(hook_name);
            std::fs::write(
                &hook_path,
                format!("#!/bin/sh\ntouch \"{}\"\n", sentinel.display()),
            )
            .unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perm = std::fs::metadata(&hook_path).unwrap().permissions();
                perm.set_mode(0o755);
                std::fs::set_permissions(&hook_path, perm).unwrap();
            }
        }

        let result = run_harness_buildable(tmp.path());
        assert_eq!(result.status, Status::Pass, "{result:?}");
        assert!(
            !sentinel.exists(),
            "the export mechanism (git ls-tree + cat-file) must never \
             invoke a post-checkout or post-index-change hook — sentinel \
             file exists: {}",
            sentinel.display()
        );
    }

    #[test]
    fn test_harness_buildable_warns_inconclusive_when_no_toolchain_on_path() {
        // (i)
        let tmp = init_hex_harness_repo(
            &[(".hex/harness/src/lib.rs", "pub fn f() -> i32 { 1 }\n")],
            &[],
        );
        let empty_bin = tempfile::tempdir().unwrap();
        let result =
            crate::doctor::checks::harness_buildable::run_check_with_timeout_and_path_override(
                tmp.path(),
                std::time::Duration::from_secs(30),
                &empty_bin.path().display().to_string(),
            );
        assert_eq!(
            result.status,
            Status::Warn,
            "no `cargo` reachable on PATH means this check cannot run at \
             all — that is inconclusive (WARN), never a build FAIL: \
             {result:?}"
        );
    }

    #[test]
    fn test_harness_buildable_removes_its_export_tempdir_after_pass_and_after_fail() {
        // (j) — reads the export path back via a thread-local test hook
        // (`last_export_path_for_tests`) rather than diffing the OS temp
        // directory's listing: `cargo test` runs tests in parallel, each on
        // its own thread, and another concurrently running test can create
        // (and not yet have removed) its own
        // `hex-doctor-harness-buildable-export-*` directory in the same
        // window, which a directory-listing diff cannot tell apart from a
        // genuine leak by this test's own two invocations.
        let pass_tmp = init_hex_harness_repo(
            &[(".hex/harness/src/lib.rs", "pub fn f() -> i32 { 1 }\n")],
            &[],
        );
        let pass_result = run_harness_buildable(pass_tmp.path());
        assert_eq!(pass_result.status, Status::Pass, "{pass_result:?}");
        let pass_export_path =
            crate::doctor::checks::harness_buildable::last_export_path_for_tests()
                .expect("a PASS outcome must have exported a temp dir");
        assert!(
            !pass_export_path.exists(),
            "the export temp dir must be removed after a PASS outcome, \
             still present: {}",
            pass_export_path.display()
        );

        let fail_tmp = init_hex_harness_repo(
            &[(".hex/harness/src/lib.rs", "mod missing;\n")],
            &[(".hex/harness/src/missing.rs", "pub const X: i32 = 1;\n")],
        );
        let fail_result = run_harness_buildable(fail_tmp.path());
        assert_eq!(fail_result.status, Status::Fail, "{fail_result:?}");
        let fail_export_path =
            crate::doctor::checks::harness_buildable::last_export_path_for_tests()
                .expect("a FAIL outcome must have exported a temp dir");
        assert!(
            !fail_export_path.exists(),
            "the export temp dir must be removed after a FAIL outcome, \
             still present: {}",
            fail_export_path.display()
        );
    }

    #[test]
    fn test_harness_buildable_warns_when_cargo_check_exceeds_the_wall_clock_cap() {
        // (k) — no wall-clock assertion on how long the fixture itself
        // would take to build; only that the cap actually bounds this
        // check's own runtime to well inside a generous ceiling.
        let tmp = init_hex_harness_repo(
            &[
                (".hex/harness/src/lib.rs", "pub fn f() -> i32 { 1 }\n"),
                (
                    ".hex/harness/build.rs",
                    "fn main() { std::thread::sleep(std::time::Duration::from_secs(30)); }\n",
                ),
            ],
            &[],
        );
        let start = std::time::Instant::now();
        let result = crate::doctor::checks::harness_buildable::run_check_with_timeout(
            tmp.path(),
            std::time::Duration::from_millis(300),
        );
        let elapsed = start.elapsed();
        assert_eq!(
            result.status,
            Status::Warn,
            "a cargo check that cannot finish inside the wall-clock cap \
             must be reported as inconclusive (WARN), never FAIL: \
             {result:?}"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(20),
            "the cap must actually bound the check's runtime — a 300ms cap \
             against a build.rs that sleeps 30s must return well inside a \
             generous ceiling, took {elapsed:?}"
        );
    }

    #[test]
    fn test_harness_buildable_ignores_local_replace_refs_and_still_fails() {
        // G1 (review_b): `git replace` transparently substitutes a
        // different object for the one a sha names, at the plumbing layer
        // `ls-tree`/`cat-file --batch` read from — a local replace ref
        // pointing HEAD's committed (broken) `lib.rs` blob at a different,
        // buildable stand-in must never change what this check certifies.
        // Verified by hand that `git cat-file --batch` honors replace refs
        // by default and returns the replacement's content, not the
        // original blob's, unless `GIT_NO_REPLACE_OBJECTS=1` is set.
        let tmp = init_hex_harness_repo(&[(".hex/harness/src/lib.rs", "mod missing;\n")], &[]);

        let committed_sha_output = std::process::Command::new("git")
            .args(["rev-parse", "HEAD:.hex/harness/src/lib.rs"])
            .current_dir(tmp.path())
            .output()
            .expect("git rev-parse spawns");
        assert!(committed_sha_output.status.success());
        let committed_sha = String::from_utf8_lossy(&committed_sha_output.stdout)
            .trim()
            .to_string();

        let stand_in_path = tmp.path().join("stand-in-not-committed.rs");
        std::fs::write(&stand_in_path, "pub fn f() -> i32 { 1 }\n").unwrap();
        let hash_output = std::process::Command::new("git")
            .args([
                "hash-object",
                "-w",
                stand_in_path.to_str().expect("utf8 path"),
            ])
            .current_dir(tmp.path())
            .output()
            .expect("git hash-object spawns");
        assert!(hash_output.status.success());
        let replacement_sha = String::from_utf8_lossy(&hash_output.stdout)
            .trim()
            .to_string();
        std::fs::remove_file(&stand_in_path).unwrap();

        run_git(tmp.path(), &["replace", &committed_sha, &replacement_sha]);

        let result = run_harness_buildable(tmp.path());
        assert_eq!(
            result.status,
            Status::Fail,
            "a local `git replace` ref substituting a buildable stand-in \
             for the committed (broken) blob must never make this check \
             PASS a tree that does not actually build from what git has \
             committed: {result:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_export_committed_head_refuses_symlink_that_escapes_export_root() {
        // G2 (review_b), export-level: a committed symlink (git mode
        // 120000) whose target walks OUT of the export root via `..`
        // segments must be refused outright rather than materialized —
        // this is the mechanism-level guard the end-to-end test below
        // exercises through the full check.
        let tmp = tempfile::tempdir().unwrap();
        run_git(tmp.path(), &["init", "-q"]);
        let harness = tmp.path().join(".hex/harness");
        std::fs::create_dir_all(harness.join("src")).unwrap();
        std::fs::write(harness.join("Cargo.toml"), HEX_HARNESS_MANIFEST).unwrap();
        std::fs::write(harness.join("Cargo.lock"), HEX_HARNESS_LOCKFILE).unwrap();
        // Enough `..` segments to walk out of any plausible OS temp-dir
        // nesting depth, regardless of how deep this machine's temp root
        // happens to be.
        let escape_target = "../".repeat(20) + "etc/hostname";
        std::os::unix::fs::symlink(&escape_target, harness.join("src/data.txt")).unwrap();
        run_git(tmp.path(), &["add", "-A"]);
        run_git(
            tmp.path(),
            &["commit", "-q", "-m", "fixture with escaping symlink"],
        );

        let result =
            crate::doctor::checks::harness_buildable::export_committed_head_for_tests(tmp.path());
        assert!(
            result.is_err(),
            "a committed symlink whose target walks out of the export \
             root must be refused, not silently materialized: {result:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_harness_buildable_never_falsely_passes_an_escaping_committed_symlink() {
        // G2 (review_b), end-to-end: without the export-level guard, an
        // escaping symlink could let `cargo check` silently read an
        // untracked file from elsewhere on this machine and falsely PASS a
        // commit that does not actually build from what git has. The
        // export must instead be treated as untrustworthy — WARN, never
        // PASS (and never FAIL, since there is no real compile error to
        // report; the export itself could not be trusted).
        let tmp = tempfile::tempdir().unwrap();
        run_git(tmp.path(), &["init", "-q"]);
        let harness = tmp.path().join(".hex/harness");
        std::fs::create_dir_all(harness.join("src")).unwrap();
        std::fs::write(harness.join("Cargo.toml"), HEX_HARNESS_MANIFEST).unwrap();
        std::fs::write(harness.join("Cargo.lock"), HEX_HARNESS_LOCKFILE).unwrap();
        std::fs::write(
            harness.join("src/lib.rs"),
            "pub const DATA: &str = include_str!(\"data.txt\");\n",
        )
        .unwrap();
        let escape_target = "../".repeat(20) + "etc/hostname";
        std::os::unix::fs::symlink(&escape_target, harness.join("src/data.txt")).unwrap();
        run_git(tmp.path(), &["add", "-A"]);
        run_git(
            tmp.path(),
            &[
                "commit",
                "-q",
                "-m",
                "fixture with escaping symlink include",
            ],
        );

        let result = run_harness_buildable(tmp.path());
        assert_ne!(
            result.status,
            Status::Pass,
            "an escaping committed symlink must never produce a false \
             PASS: {result:?}"
        );
        assert_eq!(
            result.status,
            Status::Warn,
            "an escaping committed symlink makes the export itself \
             untrustworthy — this must WARN (cannot certify), not FAIL: \
             {result:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_harness_buildable_warns_inconclusive_when_cargo_present_but_rustc_unreachable() {
        // G3 (review_b): cargo itself resolves, spawns, and only THEN fails
        // because it cannot find/execute `rustc` — a toolchain problem,
        // never a build failure, so it must WARN, not FAIL like a genuine
        // compile error would. Simulated by restricting PATH to a
        // directory containing ONLY a symlink to the real cargo binary
        // (located via the `CARGO` env var cargo itself sets for test
        // binaries it runs — verified by hand this is set and `RUSTC` is
        // NOT, so nothing here leaks a real toolchain path back in), so
        // cargo's own internal PATH lookup for `rustc` fails exactly like
        // it would on a machine with no toolchain installed at all.
        let tmp = init_hex_harness_repo(
            &[(".hex/harness/src/lib.rs", "pub fn f() -> i32 { 1 }\n")],
            &[],
        );
        let cargo_path = std::env::var("CARGO").expect("`cargo test` sets CARGO");
        let bin_dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(&cargo_path, bin_dir.path().join("cargo")).unwrap();

        let result =
            crate::doctor::checks::harness_buildable::run_check_with_timeout_and_path_override(
                tmp.path(),
                std::time::Duration::from_secs(60),
                &bin_dir.path().display().to_string(),
            );
        assert_eq!(
            result.status,
            Status::Warn,
            "cargo present but unable to find/execute rustc must WARN \
             (toolchain unreachable), never FAIL like a genuine compile \
             error: {result:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_harness_buildable_warns_inconclusive_when_rustc_override_path_does_not_exist() {
        // G3 (review_b, round 2): the original G3 fixture above only
        // exercised cargo's OWN PATH lookup failing to find a bare `rustc`.
        // cargo reports an EXPLICIT toolchain override (an `RUSTC` env var
        // pointing at a path that does not exist, exactly like a broken
        // rustup toolchain path) with the FULL PATH quoted after the
        // backtick — `` could not execute process `/no/such/rustc -vV` ``
        // — never the bare name `rustc` immediately following it. The old
        // classifier required "rustc" to immediately follow the opening
        // quote and so missed this, letting a toolchain problem fall
        // through to the generic FAIL branch.
        let tmp = init_hex_harness_repo(
            &[(".hex/harness/src/lib.rs", "pub fn f() -> i32 { 1 }\n")],
            &[],
        );
        let missing_rustc = tempfile::tempdir()
            .unwrap()
            .path()
            .join("no-such-rustc-binary");

        let result =
            crate::doctor::checks::harness_buildable::run_check_with_timeout_and_env_override(
                tmp.path(),
                std::time::Duration::from_secs(60),
                &[("RUSTC", missing_rustc.to_str().expect("utf8 path"))],
            );
        assert_eq!(
            result.status,
            Status::Warn,
            "an explicit RUSTC override pointing at a path that does not \
             exist must WARN (toolchain unreachable), never FAIL like a \
             genuine compile error: {result:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_harness_buildable_warns_inconclusive_when_rustc_override_runs_but_fails_its_probe() {
        // A-R2 (round-4 review): `is_toolchain_unavailable_failure` only
        // matched cargo's ENOENT wrapper (`could not execute process
        // ...`, never executed). A toolchain binary that EXISTS and RUNS,
        // but exits non-zero on cargo's own `-vV` probe — the actual
        // shape of a broken rustup proxy naming a toolchain that isn't
        // installed, or any other broken `RUSTC` override — never reaches
        // that ENOENT wrapper; cargo instead reports its own `process
        // didn't exit successfully: \`<rustc> -vV\` (exit status: N)`
        // wrapper. Simulated with `RUSTC=/usr/bin/false`: a real,
        // executable binary that always exits 1 with no output.
        let tmp = init_hex_harness_repo(
            &[(".hex/harness/src/lib.rs", "pub fn f() -> i32 { 1 }\n")],
            &[],
        );

        let result =
            crate::doctor::checks::harness_buildable::run_check_with_timeout_and_env_override(
                tmp.path(),
                std::time::Duration::from_secs(60),
                &[("RUSTC", "/usr/bin/false")],
            );
        assert_eq!(
            result.status,
            Status::Warn,
            "an RUSTC override that exists and runs but fails cargo's own \
             `-vV` toolchain probe must WARN (toolchain unreachable), \
             never FAIL like a genuine compile error: {result:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_export_committed_head_refuses_a_symlink_that_escapes_only_via_a_chained_hop() {
        // G2 (review_b, round 2), export-level: a purely LEXICAL `..`/`.`
        // walk assumes every intermediate path component contributes
        // exactly one real directory level — wrong when that component is
        // itself a committed symlink targeting `.` (its own parent, zero
        // real levels). `link_identity` -> `.` and `link_escape` ->
        // `link_identity/../<sentinel file name>` each look, purely
        // lexically, like they stay inside the export root (push then pop
        // cancel out), but the REAL filesystem resolves `link_identity` to
        // the repo root itself, so the single `..` actually walks one level
        // ABOVE the export root. Verified by hand this exact two-symlink
        // shape was accepted (not refused) before this fix.
        let outside = tempfile::NamedTempFile::new().unwrap();
        let outside_name = outside
            .path()
            .file_name()
            .expect("named temp file has a name")
            .to_owned();

        let tmp = tempfile::tempdir().unwrap();
        run_git(tmp.path(), &["init", "-q"]);
        let harness = tmp.path().join(".hex/harness");
        std::fs::create_dir_all(harness.join("src")).unwrap();
        std::fs::write(harness.join("Cargo.toml"), HEX_HARNESS_MANIFEST).unwrap();
        std::fs::write(harness.join("Cargo.lock"), HEX_HARNESS_LOCKFILE).unwrap();
        std::os::unix::fs::symlink(".", tmp.path().join("link_identity")).unwrap();
        let escape_target = PathBuf::from("link_identity/..").join(&outside_name);
        std::os::unix::fs::symlink(&escape_target, tmp.path().join("link_escape")).unwrap();
        run_git(tmp.path(), &["add", "-A"]);
        run_git(
            tmp.path(),
            &[
                "commit",
                "-q",
                "-m",
                "fixture with chained escaping symlink",
            ],
        );

        let result =
            crate::doctor::checks::harness_buildable::export_committed_head_for_tests(tmp.path());
        assert!(
            result.is_err(),
            "a committed symlink that escapes the export root only once a \
             chained committed symlink is REALLY resolved (never via a \
             purely lexical `..`/`.` walk) must still be refused: {result:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_export_committed_head_refuses_a_symlink_that_escapes_via_a_case_folded_chained_hop() {
        // A-R1 (round 3 review), export-level: `resolve_realpath_within_export`
        // substitutes a chained hop only when a path component matches a
        // committed symlink's path by EXACT byte equality (the HashMap key).
        // On a case-insensitive filesystem (macOS APFS by default — this
        // machine included) the REAL filesystem resolves a case-VARIANT
        // reference to the very same symlink the exact-byte lookup
        // requires; the lookup misses, the component is treated as an
        // ordinary directory push, the following `..` cancels it
        // lexically, and the escaping symlink is materialized. Same
        // two-symlink shape as the round-2 chained-hop fixture above,
        // except the committed symlink's name and the reference to it
        // differ only by case.
        let outside = tempfile::NamedTempFile::new().unwrap();
        let outside_name = outside
            .path()
            .file_name()
            .expect("named temp file has a name")
            .to_owned();

        let tmp = tempfile::tempdir().unwrap();
        if !filesystem_is_case_insensitive(tmp.path()) {
            // B-R2 (round-4 review): this fixture's escape exists ONLY on
            // a filesystem that folds `Link_Identity` and `link_identity`
            // onto the same directory entry — on a case-sensitive one
            // (most Linux CI runners) `link_identity` simply does not
            // exist, the lexical walk correctly leaves the reference
            // alone, and no escape can occur at all.
            eprintln!(
                "skipping test_export_committed_head_refuses_a_symlink_that_escapes_via_a_case_folded_chained_hop: \
                 this fixture's case-folded escape only exists on a case-insensitive filesystem"
            );
            return;
        }
        run_git(tmp.path(), &["init", "-q"]);
        let harness = tmp.path().join(".hex/harness");
        std::fs::create_dir_all(harness.join("src")).unwrap();
        std::fs::write(harness.join("Cargo.toml"), HEX_HARNESS_MANIFEST).unwrap();
        std::fs::write(harness.join("Cargo.lock"), HEX_HARNESS_LOCKFILE).unwrap();
        std::os::unix::fs::symlink(".", tmp.path().join("Link_Identity")).unwrap();
        let escape_target = PathBuf::from("link_identity/..").join(&outside_name);
        std::os::unix::fs::symlink(&escape_target, tmp.path().join("link_escape")).unwrap();
        run_git(tmp.path(), &["add", "-A"]);
        run_git(
            tmp.path(),
            &[
                "commit",
                "-q",
                "-m",
                "fixture with case-folded chained escaping symlink",
            ],
        );

        let result =
            crate::doctor::checks::harness_buildable::export_committed_head_for_tests(tmp.path());
        assert!(
            result.is_err(),
            "a committed symlink that escapes the export root only once a \
             case-variant reference to a chained committed symlink is \
             REALLY resolved by the filesystem (never via an exact-byte \
             lookup) must still be refused: {result:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_harness_buildable_never_falsely_passes_a_case_folded_escaping_symlink_chain() {
        // A-R1 (round 3 review), end-to-end: the same case-fold bypass as
        // the export-level test above, but proven through the full check —
        // a committed module reachable only via the case-folded escaping
        // chain must never let `cargo check` silently read real, uncommitted
        // content from elsewhere on this machine and report PASS.
        let tmp = tempfile::tempdir().unwrap();
        if !filesystem_is_case_insensitive(tmp.path()) {
            // B-R2 (round-4 review): see the export-level test above —
            // this fixture's escape exists only on a folding filesystem.
            eprintln!(
                "skipping test_harness_buildable_never_falsely_passes_a_case_folded_escaping_symlink_chain: \
                 this fixture's case-folded escape only exists on a case-insensitive filesystem"
            );
            return;
        }
        run_git(tmp.path(), &["init", "-q"]);
        let harness = tmp.path().join(".hex/harness");
        std::fs::create_dir_all(harness.join("src")).unwrap();
        std::fs::write(harness.join("Cargo.toml"), HEX_HARNESS_MANIFEST).unwrap();
        std::fs::write(harness.join("Cargo.lock"), HEX_HARNESS_LOCKFILE).unwrap();
        std::fs::write(harness.join("src/lib.rs"), "mod data;\npub use data::f;\n").unwrap();

        let outside = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(outside.path(), "pub fn f() -> i32 { 1 }\n").unwrap();
        let outside_name = outside
            .path()
            .file_name()
            .expect("named temp file has a name")
            .to_owned();
        std::os::unix::fs::symlink(".", tmp.path().join("Link_Identity")).unwrap();
        let escape_target = PathBuf::from("../../../link_identity/..").join(&outside_name);
        std::os::unix::fs::symlink(&escape_target, harness.join("src/data.rs")).unwrap();
        run_git(tmp.path(), &["add", "-A"]);
        run_git(
            tmp.path(),
            &[
                "commit",
                "-q",
                "-m",
                "fixture: mod reachable only via case-folded escaping chain",
            ],
        );

        let result = run_harness_buildable(tmp.path());
        assert_ne!(
            result.status,
            Status::Pass,
            "`data.rs` exists nowhere in the committed tree — only via a \
             case-folded chained symlink escape reading a real file \
             elsewhere on this machine — so this must never PASS: {result:?}"
        );
        assert_eq!(
            result.status,
            Status::Warn,
            "a case-folded escaping symlink chain makes the export itself \
             untrustworthy — this must WARN (cannot certify), not FAIL: \
             {result:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_export_committed_head_refuses_a_symlink_that_escapes_via_an_uppercase_variant_hop() {
        // B-R1 (round 3 review): the identical bug class as A-R1 above,
        // reported independently against the exact probe shape from that
        // finding's evidence — a committed `link_identity -> .` referenced
        // via its UPPERCASE spelling (`LINK_IDENTITY`) from the escaping
        // symlink's target. Already closed by the same fix as A-R1 (the
        // exact-byte chain lookup does not distinguish which side of the
        // case mismatch is upper/lower); pinned here directly so this
        // finding's own probe shape has a dedicated regression.
        let outside = tempfile::NamedTempFile::new().unwrap();
        let outside_name = outside
            .path()
            .file_name()
            .expect("named temp file has a name")
            .to_owned();

        let tmp = tempfile::tempdir().unwrap();
        if !filesystem_is_case_insensitive(tmp.path()) {
            // B-R2 (round-4 review): see the export-level case-fold test
            // above — this fixture's escape exists only on a folding
            // filesystem.
            eprintln!(
                "skipping test_export_committed_head_refuses_a_symlink_that_escapes_via_an_uppercase_variant_hop: \
                 this fixture's case-folded escape only exists on a case-insensitive filesystem"
            );
            return;
        }
        run_git(tmp.path(), &["init", "-q"]);
        let harness = tmp.path().join(".hex/harness");
        std::fs::create_dir_all(harness.join("src")).unwrap();
        std::fs::write(harness.join("Cargo.toml"), HEX_HARNESS_MANIFEST).unwrap();
        std::fs::write(harness.join("Cargo.lock"), HEX_HARNESS_LOCKFILE).unwrap();
        std::os::unix::fs::symlink(".", tmp.path().join("link_identity")).unwrap();
        let escape_target = PathBuf::from("LINK_IDENTITY/..").join(&outside_name);
        std::os::unix::fs::symlink(&escape_target, tmp.path().join("link_escape")).unwrap();
        run_git(tmp.path(), &["add", "-A"]);
        run_git(
            tmp.path(),
            &[
                "commit",
                "-q",
                "-m",
                "fixture with uppercase-variant chained escaping symlink",
            ],
        );

        let result =
            crate::doctor::checks::harness_buildable::export_committed_head_for_tests(tmp.path());
        assert!(
            result.is_err(),
            "an escaping symlink referenced only via an UPPERCASE variant \
             of a committed lowercase symlink's name must still be \
             refused: {result:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_export_committed_head_refuses_writing_a_committed_file_through_a_name_equivalent_symlink(
    ) {
        // A-R2 (round 3 review): entries are written in `git ls-tree`
        // order (bytewise), and the export writes a regular file with
        // `std::fs::write`, which FOLLOWS an existing symlink at `dest`
        // (O_TRUNC through the link) rather than replacing it. On a
        // case-insensitive filesystem, two committed names that are FS-
        // equivalent (`M` and `m`) sort in git's bytewise order such that
        // the symlink `M -> Cargo.toml` is materialized first, and the
        // regular file `m`'s bytes then get written straight through it
        // into `Cargo.toml` — a DIFFERENT committed path, already
        // materialized. `git update-index --add --cacheinfo` builds this
        // fixture directly in the index/object database, so the
        // case-insensitive working tree this test runs on never has to
        // hold `M` and `m` at once.
        let tmp = tempfile::tempdir().unwrap();
        run_git(tmp.path(), &["init", "-q"]);
        std::fs::create_dir_all(tmp.path().join(".hex/harness/src")).unwrap();

        let hash_object = |content: &[u8]| -> String {
            let scratch = tempfile::NamedTempFile::new().unwrap();
            std::fs::write(scratch.path(), content).unwrap();
            let output = std::process::Command::new("git")
                .args(["hash-object", "-w", scratch.path().to_str().unwrap()])
                .current_dir(tmp.path())
                .output()
                .expect("git hash-object spawns");
            assert!(output.status.success());
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        };

        // The manifest actually reviewed/committed at `Cargo.toml` points
        // `[lib]` at a source file that does not exist anywhere in the
        // repo — built as committed, `cargo check` must FAIL. `m`'s
        // manifest has no explicit `[lib]` section, so if it ever gets
        // written straight into `Cargo.toml`'s location it falls back to
        // the implicit `src/lib.rs` target below, which DOES exist and IS
        // valid — the fixture only demonstrates a genuine false PASS if
        // that fallback would actually succeed.
        let broken_manifest = format!("{HEX_HARNESS_MANIFEST}[lib]\npath = \"src/nope.rs\"\n");
        let broken_manifest_sha = hash_object(broken_manifest.as_bytes());
        let good_manifest_sha = hash_object(HEX_HARNESS_MANIFEST.as_bytes());
        let lockfile_sha = hash_object(HEX_HARNESS_LOCKFILE.as_bytes());
        let symlink_sha = hash_object(b"Cargo.toml");
        let lib_rs_sha = hash_object(b"pub fn f() -> i32 { 1 }\n");

        run_git(
            tmp.path(),
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("100644,{lib_rs_sha},.hex/harness/src/lib.rs"),
            ],
        );
        run_git(
            tmp.path(),
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("100644,{lockfile_sha},.hex/harness/Cargo.lock"),
            ],
        );
        run_git(
            tmp.path(),
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("100644,{broken_manifest_sha},.hex/harness/Cargo.toml"),
            ],
        );
        run_git(
            tmp.path(),
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("120000,{symlink_sha},.hex/harness/M"),
            ],
        );
        run_git(
            tmp.path(),
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("100644,{good_manifest_sha},.hex/harness/m"),
            ],
        );
        run_git(
            tmp.path(),
            &[
                "commit",
                "-q",
                "-m",
                "fixture: case-equivalent M/m collide via a symlink",
            ],
        );

        let result = run_harness_buildable(tmp.path());
        assert_ne!(
            result.status,
            Status::Pass,
            "the committed manifest's `[lib]` target does not exist \
             anywhere in the repo — it must never report PASS just \
             because a case-equivalent committed symlink name let the \
             export write straight through it into a different, valid \
             manifest sharing the same filesystem path: {result:?}"
        );
    }

    /// `true` iff `dir` sits on a case-insensitive (folding) filesystem —
    /// the only kind on which a reference that differs from a committed
    /// symlink's name only by case actually resolves, on the REAL
    /// filesystem, to that same symlink. Several fixtures below (and
    /// above) pin an escape that exists ONLY on such a filesystem; used to
    /// skip them for real, rather than assert an impossible-here escape,
    /// on a case-SENSITIVE one (most Linux CI runners) where the
    /// fixture's differently-cased reference is simply a distinct,
    /// nonexistent path and no escape can occur at all (B-R2, round-4
    /// review).
    #[cfg(unix)]
    fn filesystem_is_case_insensitive(dir: &std::path::Path) -> bool {
        let probe = dir.join("hex-doctor-case-probe-a");
        std::fs::write(&probe, b"x").expect("write case-insensitivity probe file");
        dir.join("HEX-DOCTOR-CASE-PROBE-A")
            .symlink_metadata()
            .is_ok()
    }

    #[cfg(unix)]
    #[test]
    fn test_harness_buildable_never_falsely_passes_a_case_folded_directory_symlink_write_through() {
        // A-R1 (round-4 review): the leaf-only collision guard
        // (`symlink_metadata(&dest)` on an entry's OWN final path
        // component) does nothing for a case-folded DIRECTORY symlink
        // sitting in an INTERMEDIATE component — `create_dir_all(parent)`/
        // `fs::write` still FOLLOW an already-materialized symlink there,
        // silently redirecting a deeper committed file's bytes to
        // wherever that symlink points. Built with `git update-index
        // --cacheinfo` (like the M/m fixture above) so the
        // case-insensitive working tree this test runs on never has to
        // hold `T` and `t` at once. No `src/lib.rs` is committed anywhere
        // in this tree — only `t/src/lib.rs`, reachable as `src/lib.rs`
        // ONLY via the case-folded directory symlink `T -> .` this
        // filesystem folds onto `t` — so a correct export/build can never
        // PASS; this pins that a write-through never smuggles it there.
        let tmp = tempfile::tempdir().unwrap();
        run_git(tmp.path(), &["init", "-q"]);
        std::fs::create_dir_all(tmp.path().join(".hex/harness")).unwrap();

        let hash_object = |content: &[u8]| -> String {
            let scratch = tempfile::NamedTempFile::new().unwrap();
            std::fs::write(scratch.path(), content).unwrap();
            let output = std::process::Command::new("git")
                .args(["hash-object", "-w", scratch.path().to_str().unwrap()])
                .current_dir(tmp.path())
                .output()
                .expect("git hash-object spawns");
            assert!(output.status.success());
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        };

        let manifest_sha = hash_object(HEX_HARNESS_MANIFEST.as_bytes());
        let lockfile_sha = hash_object(HEX_HARNESS_LOCKFILE.as_bytes());
        let identity_sha = hash_object(b".");
        let lib_rs_sha = hash_object(b"pub fn f() -> i32 { 1 }\n");

        run_git(
            tmp.path(),
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("100644,{manifest_sha},.hex/harness/Cargo.toml"),
            ],
        );
        run_git(
            tmp.path(),
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("100644,{lockfile_sha},.hex/harness/Cargo.lock"),
            ],
        );
        run_git(
            tmp.path(),
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("120000,{identity_sha},.hex/harness/T"),
            ],
        );
        run_git(
            tmp.path(),
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("100644,{lib_rs_sha},.hex/harness/t/src/lib.rs"),
            ],
        );
        run_git(
            tmp.path(),
            &[
                "commit",
                "-q",
                "-m",
                "fixture: case-folded directory symlink T over real dir t/src",
            ],
        );

        let result = run_harness_buildable(tmp.path());
        assert_ne!(
            result.status,
            Status::Pass,
            "no `src/lib.rs` exists anywhere in the committed tree — only \
             `t/src/lib.rs`, reachable as `src/lib.rs` ONLY via a \
             case-folded directory symlink `T -> .` this filesystem folds \
             onto `t` — so this must never PASS: {result:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_export_committed_head_never_writes_regular_file_bytes_outside_the_export_root() {
        // B-R1 (round-4 review): the realpath containment guard used to
        // run only ONCE, batched after the WHOLE materialization loop
        // finished — by the time it could catch an escaping symlink, any
        // regular file whose write FOLLOWED that symlink (because its
        // parent directory case-folds onto the symlink's name) had
        // already landed OUTSIDE the export root. Exact probe shape from
        // the finding's evidence: `Link_Identity -> .` (exact case) and
        // `Src -> link_identity/../../../<sibling>` (referencing
        // `Link_Identity` ONLY via a case-folded lowercase spelling,
        // which the lexical exact-byte check misses entirely and so
        // wrongly treats as an ordinary in-bounds path) sort, in `git
        // ls-tree` order, strictly before `.hex/harness/src/lib.rs` —
        // whose own parent directory (`src`) case-folds onto `Src` — so
        // the write follows the escaping symlink straight into `sibling`.
        // Built with `git update-index --cacheinfo` so `Src` and `src`
        // never have to coexist on the actual fixture-repo working tree.
        let sibling = tempfile::tempdir().unwrap();
        let sibling_name = sibling
            .path()
            .file_name()
            .expect("temp dir has a name")
            .to_str()
            .expect("utf8 temp dir name")
            .to_string();
        let leak_marker: &[u8] = b"SHOULD_NEVER_LEAK_OUTSIDE_THE_EXPORT_ROOT";

        let tmp = tempfile::tempdir().unwrap();
        if !filesystem_is_case_insensitive(tmp.path()) {
            eprintln!(
                "skipping test_export_committed_head_never_writes_regular_file_bytes_outside_the_export_root: \
                 this fixture's case-folded escape only exists on a case-insensitive filesystem"
            );
            return;
        }
        run_git(tmp.path(), &["init", "-q"]);
        std::fs::create_dir_all(tmp.path().join(".hex/harness")).unwrap();

        let hash_object = |content: &[u8]| -> String {
            let scratch = tempfile::NamedTempFile::new().unwrap();
            std::fs::write(scratch.path(), content).unwrap();
            let output = std::process::Command::new("git")
                .args(["hash-object", "-w", scratch.path().to_str().unwrap()])
                .current_dir(tmp.path())
                .output()
                .expect("git hash-object spawns");
            assert!(output.status.success());
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        };

        let manifest_sha = hash_object(HEX_HARNESS_MANIFEST.as_bytes());
        let lockfile_sha = hash_object(HEX_HARNESS_LOCKFILE.as_bytes());
        let identity_sha = hash_object(b".");
        let escape_target = format!("link_identity/../../../{sibling_name}");
        let escape_sha = hash_object(escape_target.as_bytes());
        let lib_rs_sha = hash_object(leak_marker);

        run_git(
            tmp.path(),
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("100644,{manifest_sha},.hex/harness/Cargo.toml"),
            ],
        );
        run_git(
            tmp.path(),
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("100644,{lockfile_sha},.hex/harness/Cargo.lock"),
            ],
        );
        run_git(
            tmp.path(),
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("120000,{identity_sha},.hex/harness/Link_Identity"),
            ],
        );
        run_git(
            tmp.path(),
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("120000,{escape_sha},.hex/harness/Src"),
            ],
        );
        run_git(
            tmp.path(),
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("100644,{lib_rs_sha},.hex/harness/src/lib.rs"),
            ],
        );
        run_git(
            tmp.path(),
            &[
                "commit",
                "-q",
                "-m",
                "fixture: B-R1 regular-file write-through via case-folded chained escape",
            ],
        );

        let _ =
            crate::doctor::checks::harness_buildable::export_committed_head_for_tests(tmp.path());

        let leaked = std::fs::read(sibling.path().join("lib.rs"));
        assert!(
            leaked.is_err(),
            "committed `src/lib.rs` bytes must never be written into a \
             directory OUTSIDE the export root via a case-folded chained \
             escaping symlink, regardless of whether the export call \
             itself ultimately returns Ok or Err: found {leaked:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_export_committed_head_never_creates_directories_or_symlinks_outside_the_export_root() {
        // Workflow ledger (final round): pass 2's `create_dir_all(parent)`
        // FOLLOWS an already-materialized, case-folded symlink at an
        // intermediate path component — not just a regular file's
        // `fs::write` (B-R1, the sibling test above) — so `mkdir`/`symlink`
        // themselves could land a real directory and a real symlink
        // OUTSIDE the export root before `verify_symlinks_resolve_within_export`'s
        // batched check ever ran. Exact probe shape from the finding's
        // evidence: `Link_Identity -> .` (exact case) and
        // `A -> link_identity/../../../<sibling>` (referencing
        // `Link_Identity` only via a case-folded lowercase spelling the
        // lexical exact-byte check misses) sort, in `git ls-tree` order,
        // before `a/b/link -> ../../Cargo.toml` — whose own parent
        // directory `a/b` case-folds onto the escaping symlink `A` once
        // pass 2 has already materialized it. Built with `git update-index
        // --cacheinfo` so `A`/`a` never have to coexist on the actual
        // fixture-repo working tree.
        let sibling = tempfile::tempdir().unwrap();
        let sibling_name = sibling
            .path()
            .file_name()
            .expect("temp dir has a name")
            .to_str()
            .expect("utf8 temp dir name")
            .to_string();

        let tmp = tempfile::tempdir().unwrap();
        if !filesystem_is_case_insensitive(tmp.path()) {
            eprintln!(
                "skipping test_export_committed_head_never_creates_directories_or_symlinks_outside_the_export_root: \
                 this fixture's case-folded escape only exists on a case-insensitive filesystem"
            );
            return;
        }
        run_git(tmp.path(), &["init", "-q"]);
        std::fs::create_dir_all(tmp.path().join(".hex/harness")).unwrap();

        let hash_object = |content: &[u8]| -> String {
            let scratch = tempfile::NamedTempFile::new().unwrap();
            std::fs::write(scratch.path(), content).unwrap();
            let output = std::process::Command::new("git")
                .args(["hash-object", "-w", scratch.path().to_str().unwrap()])
                .current_dir(tmp.path())
                .output()
                .expect("git hash-object spawns");
            assert!(output.status.success());
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        };

        let manifest_sha = hash_object(HEX_HARNESS_MANIFEST.as_bytes());
        let lockfile_sha = hash_object(HEX_HARNESS_LOCKFILE.as_bytes());
        let identity_sha = hash_object(b".");
        let escape_target = format!("link_identity/../../../{sibling_name}");
        let escape_sha = hash_object(escape_target.as_bytes());
        let nested_link_sha = hash_object(b"../../Cargo.toml");
        let lib_rs_sha = hash_object(b"pub fn f() -> i32 { 1 }\n");

        run_git(
            tmp.path(),
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("100644,{manifest_sha},.hex/harness/Cargo.toml"),
            ],
        );
        run_git(
            tmp.path(),
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("100644,{lockfile_sha},.hex/harness/Cargo.lock"),
            ],
        );
        run_git(
            tmp.path(),
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("120000,{identity_sha},.hex/harness/Link_Identity"),
            ],
        );
        run_git(
            tmp.path(),
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("120000,{escape_sha},.hex/harness/A"),
            ],
        );
        run_git(
            tmp.path(),
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("120000,{nested_link_sha},.hex/harness/a/b/link"),
            ],
        );
        run_git(
            tmp.path(),
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("100644,{lib_rs_sha},.hex/harness/src/lib.rs"),
            ],
        );
        run_git(
            tmp.path(),
            &[
                "commit",
                "-q",
                "-m",
                "fixture: mkdir/symlink escape via case-folded chained identity hop",
            ],
        );

        let _ =
            crate::doctor::checks::harness_buildable::export_committed_head_for_tests(tmp.path());

        let escaped_dir = sibling.path().join("b");
        let escaped_link = escaped_dir.join("link");
        assert!(
            !escaped_dir.exists() && std::fs::symlink_metadata(&escaped_link).is_err(),
            "a case-folded escaping symlink chain must never let \
             `create_dir_all`/`symlink` materialize a directory or link \
             OUTSIDE the export root, regardless of whether the export \
             call itself ultimately returns Ok or Err: dir exists={}, \
             link exists={}",
            escaped_dir.exists(),
            std::fs::symlink_metadata(&escaped_link).is_ok()
        );
    }

    #[test]
    fn test_harness_buildable_still_fails_when_a_failing_build_script_mentions_offline_wording() {
        // G4 (review_b, round 2): the original G4 fixture above only
        // exercised a genuine `rustc` compile error (which always carries
        // `error[...]`/`-->` markers). A failing `build.rs` is JUST as much
        // a real build failure, but cargo reports it as `error: failed to
        // run custom build command for ...` — never `error[...]`/`-->` —
        // so the old `looks_like_a_real_compiler_diagnostic` gate missed
        // it, letting a build script's own panic message (crafted here to
        // contain the offline-mode reminder wording) fall through to the
        // offline-dependency-failure classifier and misreport an
        // inconclusive WARN instead of the genuine FAIL it is.
        let tmp = init_hex_harness_repo(
            &[
                (".hex/harness/src/lib.rs", "pub fn f() -> i32 { 1 }\n"),
                (
                    ".hex/harness/build.rs",
                    "fn main() { panic!(\"pretend this mentions offline mode (--offline) in the diagnostic text\"); }\n",
                ),
            ],
            &[],
        );
        let result = run_harness_buildable(tmp.path());
        assert_eq!(
            result.status,
            Status::Fail,
            "a build script that genuinely fails must FAIL even when its \
             own panic message happens to contain the offline-mode \
             reminder wording — the build-script-failure marker must gate \
             the offline-dependency-failure classifier the same way a real \
             compiler diagnostic does: {result:?}"
        );
    }

    #[test]
    fn test_harness_buildable_still_fails_when_a_real_compile_error_mentions_offline_wording() {
        // G4 (review_b): the offline-dependency-failure classifier must
        // never fire just because the word "offline" appears SOMEWHERE in
        // stderr. A genuine compile error (a missing module) whose own
        // diagnostic text happens to quote wording that overlaps cargo's
        // offline-mode reminder must still FAIL, naming the real error —
        // verified by hand that this exact fixture's stderr contains both
        // `error[E0583]`/`-->` (real compiler diagnostic markers) AND the
        // literal phrase "offline mode (--offline)".
        let tmp = init_hex_harness_repo(
            &[(
                ".hex/harness/src/lib.rs",
                "mod missing;\ncompile_error!(\"pretend this mentions offline mode (--offline) in the diagnostic text\");\n",
            )],
            &[],
        );
        let result = run_harness_buildable(tmp.path());
        assert_eq!(
            result.status,
            Status::Fail,
            "a real compile error must FAIL even when its own diagnostic \
             text happens to contain the offline-mode reminder wording — a \
             substring match against the whole stderr blob must never \
             misclassify this as an inconclusive WARN: {result:?}"
        );
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
    // ---- tests for task Tygqwp59m (arrra/hex PR #7 round 2: replace the
    // source scanner with a real build) ----
    //
    // Operator premise correction (2026-09-11, chief-of-staff): `git archive`
    // runs the SAME convert-to-working-tree path as `git checkout` / `git
    // worktree add` — a locally configured smudge filter defeats it exactly
    // like it defeats the worktree-based scanner today. The export mechanism
    // MUST instead materialize HEAD from raw git objects — `git ls-tree -r`
    // (or `-z` + `git cat-file --batch`) followed by `git cat-file blob
    // <sha>` per entry — which never invokes any filter driver, hook, or
    // credential helper. These four tests pin that contract directly against
    // `export_committed_head_for_tests`, a test-only entry point into the new
    // export mechanism this task adds to `harness_buildable.rs`. The
    // mechanism does not exist yet, so this module does not compile — that
    // is the RED state; the next phase implements `export_committed_head`
    // (production) and its `_for_tests` wrapper.

    /// A dep-free fixture identical in shape to `init_repo_with_lib_rs_and_files`
    /// but returned alongside its harness dir path.
    fn init_repo_for_export_tests() -> (tempfile::TempDir, std::path::PathBuf) {
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
        let harness_path = harness.clone();
        (tmp, harness_path)
    }

    #[test]
    fn test_export_committed_head_uses_committed_bytes_and_ignores_smudge_side_effects() {
        // (a) A smudge filter rewrites the committed content of `src/lib.rs`
        // AND performs a side effect (writes an extra file) as part of the
        // same filter invocation — exactly the shape of the operator's probe
        // (`filter.evil.smudge = sed COMMITTED->SMUDGED plus touch of a
        // side-effect file`). The export must equal the COMMITTED blob byte
        // for byte, and the side-effect file must never appear anywhere
        // under the export directory, because the export never runs any
        // filter driver at all.
        let (tmp, harness) = init_repo_for_export_tests();
        run_git(
            tmp.path(),
            &[
                "config",
                "filter.hex-doctor-export-probe.smudge",
                "sed 's/COMMITTED/SMUDGED/'; touch .hex/harness/src/SIDE_EFFECT.marker",
            ],
        );
        run_git(
            tmp.path(),
            &["config", "filter.hex-doctor-export-probe.clean", "cat"],
        );
        run_git(
            tmp.path(),
            &["config", "filter.hex-doctor-export-probe.required", "true"],
        );
        std::fs::write(
            harness.join(".gitattributes"),
            "src/lib.rs filter=hex-doctor-export-probe\n",
        )
        .unwrap();
        std::fs::write(
            harness.join("src/lib.rs"),
            "pub const MARK: &str = \"COMMITTED\";\n",
        )
        .unwrap();
        run_git(tmp.path(), &["add", "-A"]);
        run_git(tmp.path(), &["commit", "-q", "-m", "add filtered lib.rs"]);

        let export =
            crate::doctor::checks::harness_buildable::export_committed_head_for_tests(tmp.path())
                .expect("export of a clean, buildable HEAD must succeed");

        let exported_lib_rs = export.path().join(".hex/harness/src/lib.rs");
        let bytes = std::fs::read_to_string(&exported_lib_rs)
            .expect("exported lib.rs must exist and be readable");
        assert_eq!(
            bytes, "pub const MARK: &str = \"COMMITTED\";\n",
            "the export must equal the COMMITTED git blob, never a locally \
             configured smudge filter's rewritten content — got {bytes:?}"
        );

        let side_effect = export.path().join(".hex/harness/src/SIDE_EFFECT.marker");
        assert!(
            !side_effect.exists(),
            "the export must never run any filter driver (smudge/clean) at \
             all — a smudge filter's side-effect file must not appear \
             anywhere under the export directory, found {}",
            side_effect.display()
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_export_committed_head_preserves_committed_symlinks() {
        // (b) A committed symlink (git mode 120000) must be exported AS a
        // symlink, with its target equal to the committed blob content
        // (git stores a symlink's target path as the blob bytes) — never
        // dereferenced or rewritten.
        let (tmp, harness) = init_repo_for_export_tests();
        std::fs::write(
            harness.join("src/lib.rs"),
            "pub const DATA: &str = include_str!(\"data.txt\");\n",
        )
        .unwrap();
        std::fs::write(harness.join("src/real_data.txt"), "hello via symlink").unwrap();
        std::os::unix::fs::symlink("real_data.txt", harness.join("src/data.txt")).unwrap();
        run_git(tmp.path(), &["add", "-A"]);
        run_git(tmp.path(), &["commit", "-q", "-m", "add committed symlink"]);

        let export =
            crate::doctor::checks::harness_buildable::export_committed_head_for_tests(tmp.path())
                .expect("export of a clean, buildable HEAD must succeed");

        let exported_link = export.path().join(".hex/harness/src/data.txt");
        let meta =
            std::fs::symlink_metadata(&exported_link).expect("exported symlink entry must exist");
        assert!(
            meta.file_type().is_symlink(),
            "a committed symlink (git mode 120000) must be exported AS a \
             symlink, not dereferenced into a regular file, got {:?}",
            meta.file_type()
        );
        let target = std::fs::read_link(&exported_link).unwrap();
        assert_eq!(
            target,
            std::path::PathBuf::from("real_data.txt"),
            "the exported symlink's target must equal the committed blob's \
             content exactly, got {}",
            target.display()
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_parse_ls_tree_entry_preserves_invalid_utf8_path_bytes_a_r22() {
        // A-R-22 (round-2 review, F22): `export_committed_head` used to
        // decode the WHOLE `git ls-tree` line with `String::from_utf8_lossy`
        // before splitting out the path, silently replacing any invalid
        // UTF-8 byte in a committed path with U+FFFD's own (valid) 3-byte
        // encoding. A committed path containing an invalid byte could then
        // get exported under a DIFFERENT name than git actually recorded —
        // one that happens to match whatever a source file's
        // `include_str!`/`mod` reference literally names — letting
        // `cargo check` succeed against a path this export fabricated
        // rather than the path the commit actually contains: a false PASS.
        //
        // This can't be regression-tested by committing such a file to a
        // real fixture repo on this machine: macOS APFS refuses to create
        // a filename containing an invalid UTF-8 byte at all (`EILSEQ`,
        // confirmed live on this checkout — an environmental limit, not
        // a code path this fix can exercise end-to-end here). The parsing
        // step is pure and filesystem-independent, so it is tested
        // directly instead, with a synthetic `git ls-tree -z` line built
        // by hand.
        use std::os::unix::ffi::OsStrExt;

        let raw = b"100644 blob 0123456789abcdef0123456789abcdef01234567\tdata\xffname.txt";
        let entry = crate::doctor::checks::harness_buildable::parse_ls_tree_entry(raw)
            .expect("a well-formed ls-tree line must parse even with an invalid-UTF-8 path");

        assert_eq!(
            entry.path.as_bytes(),
            b"data\xffname.txt",
            "the parsed path must carry git's raw committed bytes \
             (including the invalid byte 0xFF) exactly — never a \
             UTF-8-lossy substitution of them"
        );
        assert_ne!(
            entry.path.as_bytes(),
            "data\u{fffd}name.txt".as_bytes(),
            "the parsed path must NOT be silently renamed to the \
             U+FFFD-substituted spelling — that renaming is exactly what \
             let a source reference to a path that was never actually \
             committed resolve anyway"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_raw_path_from_bytes_preserves_invalid_utf8_symlink_target_bytes_a_r22() {
        // A-R-22 (F22), the symlink-target half: a committed symlink's
        // target is blob content (git stores it as the raw bytes of the
        // symlink's destination path), exactly as arbitrary-byte as a
        // path. `export_committed_head`'s pass-2 symlink loop used
        // `String::from_utf8_lossy(content)` on it before this fix, which
        // would rewrite an invalid-byte target the same lossy way a
        // path could be rewritten. Same environmental note as the path
        // test above applies to end-to-end symlink-target repro on APFS;
        // `raw_path_from_bytes` (shared by both the path- and
        // target-decoding call sites) is tested directly instead.
        use std::os::unix::ffi::OsStrExt;

        let raw_target: &[u8] = b"../real\xffdata.txt";
        let target = crate::doctor::checks::harness_buildable::raw_path_from_bytes(raw_target)
            .expect("an invalid-UTF-8 byte must still preserve exactly on unix (never Err)");

        assert_eq!(
            target.as_bytes(),
            raw_target,
            "a symlink target's raw committed bytes (including the \
             invalid byte 0xFF) must be preserved exactly — never a \
             UTF-8-lossy substitution of them"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_export_committed_head_refuses_an_absolute_include_str_target() {
        // A-R-4 (round-2 review, F4): a committed `include_str!` naming an
        // ABSOLUTE path is resolved by `rustc` directly against the real
        // filesystem, bypassing this export entirely. If a file happens to
        // exist at that path on THIS machine, `cargo check` would succeed
        // against content the commit never actually contains — a false
        // PASS. This must be refused before compilation ever runs, exactly
        // like an escaping committed symlink is refused.
        let (tmp, harness) = init_repo_for_export_tests();
        // A real, host-local file that exists independently of anything
        // this commit tracks — the adversarial "coincidentally exists on
        // this machine" file the finding describes.
        let external = tmp.path().join("outside-the-repo.txt");
        std::fs::write(&external, "not part of any commit").unwrap();
        std::fs::write(
            harness.join("src/lib.rs"),
            format!(
                "pub const DATA: &str = include_str!(\"{}\");\n",
                external.display()
            ),
        )
        .unwrap();
        run_git(tmp.path(), &["add", "-A"]);
        run_git(
            tmp.path(),
            &["commit", "-q", "-m", "add absolute include_str! target"],
        );

        let result =
            crate::doctor::checks::harness_buildable::export_committed_head_for_tests(tmp.path());
        let err = result.expect_err(
            "an absolute include_str! target must refuse the export, never \
             silently succeed by reading the real filesystem",
        );
        assert!(
            err.contains("include_str!") && err.contains("outside this export's root"),
            "error must name the escaping include! call and explain why, got: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_export_committed_head_refuses_a_cargo_path_dependency_escaping_the_export() {
        // A-R-4 (round-2 review, F4): a committed `Cargo.toml` path
        // dependency that climbs, via `..`, above the export root is
        // resolved by `cargo` directly against the real filesystem — same
        // hazard, same remedy, for manifests instead of source files.
        let (tmp, harness) = init_repo_for_export_tests();
        std::fs::write(
            harness.join("Cargo.toml"),
            "[package]\nname = \"fixture-harness\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [dependencies]\nescaping-dep = { path = \"../../../../outside-the-repo\" }\n",
        )
        .unwrap();
        run_git(tmp.path(), &["add", "-A"]);
        run_git(
            tmp.path(),
            &["commit", "-q", "-m", "add escaping path dependency"],
        );

        let result =
            crate::doctor::checks::harness_buildable::export_committed_head_for_tests(tmp.path());
        let err = result.expect_err(
            "a Cargo path dependency resolving outside the export root must \
             refuse the export, never silently succeed by reading the real \
             filesystem",
        );
        assert!(
            err.contains("escaping-dep") && err.contains("outside this export's root"),
            "error must name the escaping dependency and explain why, got: {err}"
        );
    }

    #[test]
    fn test_export_committed_head_refuses_a_target_specific_path_dependency_escaping_the_export() {
        // A-R-4 round-4 review: the manifest guard originally checked only
        // [dependencies]/[dev-dependencies]/[build-dependencies]/
        // [workspace.dependencies] — Cargo also supports a path dependency
        // under a per-target `[target.'cfg(...)'.dependencies]` table,
        // which bypassed the guard entirely.
        let (tmp, harness) = init_repo_for_export_tests();
        std::fs::write(
            harness.join("Cargo.toml"),
            "[package]\nname = \"fixture-harness\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [target.'cfg(unix)'.dependencies]\n\
             escaping-dep = { path = \"../../../../outside-the-repo\" }\n",
        )
        .unwrap();
        run_git(tmp.path(), &["add", "-A"]);
        run_git(
            tmp.path(),
            &[
                "commit",
                "-q",
                "-m",
                "add escaping target-specific path dependency",
            ],
        );

        let result =
            crate::doctor::checks::harness_buildable::export_committed_head_for_tests(tmp.path());
        let err = result.expect_err(
            "a target-specific Cargo path dependency resolving outside the \
             export root must refuse the export",
        );
        assert!(
            err.contains("escaping-dep") && err.contains("outside this export's root"),
            "error must name the escaping dependency and explain why, got: {err}"
        );
    }

    #[test]
    fn test_export_committed_head_ignores_include_str_mentioned_only_in_a_comment_a_r_4() {
        // A-R-4 round-4 review, F3 reintroduced: the byte-search guard
        // added for F4 originally scanned raw source text with no
        // comment/string awareness, so a comment merely MENTIONING
        // `include_str!(...)` (e.g. as documentation) was treated as a
        // real call and aborted the export before `cargo` ever ran —
        // reintroducing the original F3 finding (a scanner
        // over-interpreting non-code text) one level down, inside code
        // this same review round added. A comment naming an absolute path
        // must not abort the export.
        let (tmp, harness) = init_repo_for_export_tests();
        std::fs::write(
            harness.join("src/lib.rs"),
            "// Example: include_str!(\"/tmp/example.txt\")\n\
             pub const OK: &str = \"fine\";\n",
        )
        .unwrap();
        run_git(tmp.path(), &["add", "-A"]);
        run_git(
            tmp.path(),
            &[
                "commit",
                "-q",
                "-m",
                "add doc comment mentioning include_str!",
            ],
        );

        crate::doctor::checks::harness_buildable::export_committed_head_for_tests(tmp.path())
            .expect(
                "a comment merely mentioning include_str!(...) must not \
                 abort the export — only a REAL call in code counts",
            );
    }

    #[test]
    fn test_export_committed_head_refuses_a_raw_string_include_str_target() {
        // A-R-4 round-4 review: the first draft of the source guard only
        // recognized an ordinary `"..."` literal, explicitly documenting
        // raw strings (`r"..."`, `r#"..."#`, …) as an out-of-scope bypass
        // — the review asked for that closed rather than merely
        // documented. `extract_string_literal_argument` now recognizes
        // both forms.
        let (tmp, harness) = init_repo_for_export_tests();
        std::fs::write(
            harness.join("src/lib.rs"),
            "pub const DATA: &str = include_str!(r\"/tmp/example.txt\");\n",
        )
        .unwrap();
        run_git(tmp.path(), &["add", "-A"]);
        run_git(
            tmp.path(),
            &[
                "commit",
                "-q",
                "-m",
                "add raw-string absolute include_str! target",
            ],
        );

        let result =
            crate::doctor::checks::harness_buildable::export_committed_head_for_tests(tmp.path());
        let err = result.expect_err(
            "an absolute include_str! target spelled as a raw string \
             literal must refuse the export exactly like an ordinary \
             string literal does",
        );
        assert!(
            err.contains("include_str!") && err.contains("outside this export's root"),
            "error must name the escaping include! call and explain why, got: {err}"
        );
    }

    #[test]
    fn test_export_committed_head_ignores_include_str_inside_a_raw_byte_string_fixture_a_r_4() {
        // A-R-4 round-5 review, F3: the first masking fix's
        // `is_raw_string_start` only recognized a BARE `r`-prefixed raw
        // string — it rejected the `r` in `br#"..."#` outright, because
        // the preceding byte (`b`) is alphanumeric. The masking pass then
        // fell through to treating the whole raw BYTE string as ordinary
        // code, leaving its fixture text (which can itself spell out
        // `include_str!("/tmp/example.txt")` as literal bytes) fully
        // exposed to the macro-name search. `string_literal_start` now
        // recognizes all four prefix forms (none, `b`, `r`, `br`).
        let (tmp, harness) = init_repo_for_export_tests();
        std::fs::write(
            harness.join("src/lib.rs"),
            "pub const EXAMPLE: &[u8] =\n    \
             br#\"prefix \" include_str!(\"/tmp/example.txt\") \"#;\n",
        )
        .unwrap();
        run_git(tmp.path(), &["add", "-A"]);
        run_git(
            tmp.path(),
            &[
                "commit",
                "-q",
                "-m",
                "add raw byte-string fixture mentioning include_str!",
            ],
        );

        crate::doctor::checks::harness_buildable::export_committed_head_for_tests(tmp.path())
            .expect(
                "text inside a raw BYTE string literal (`br#\"...\"#`) must \
                 not be treated as a real include_str! call — only an \
                 ACTUAL call in code counts",
            );
    }

    #[test]
    fn test_export_committed_head_refuses_an_include_str_target_with_a_trailing_comma() {
        // A-R-4 round-5 review, F4: `include_str!`/`include_bytes!`/
        // `include!` accept an optional trailing comma after their sole
        // argument (ordinary Rust macro-call syntax) — the closing-paren
        // check added for F3 required `)` immediately (after only
        // whitespace), so `include_str!("...",)` silently skipped the
        // absolute literal instead of refusing the export.
        let (tmp, harness) = init_repo_for_export_tests();
        std::fs::write(
            harness.join("src/lib.rs"),
            "pub const DATA: &str = include_str!(\"/tmp/example.txt\",);\n",
        )
        .unwrap();
        run_git(tmp.path(), &["add", "-A"]);
        run_git(
            tmp.path(),
            &[
                "commit",
                "-q",
                "-m",
                "add absolute include_str! target with a trailing comma",
            ],
        );

        let result =
            crate::doctor::checks::harness_buildable::export_committed_head_for_tests(tmp.path());
        let err = result.expect_err(
            "a trailing comma after the sole include_str! argument must not \
             hide an otherwise-recognized escaping literal from this guard",
        );
        assert!(
            err.contains("include_str!") && err.contains("outside this export's root"),
            "error must name the escaping include! call and explain why, got: {err}"
        );
    }

    #[test]
    fn test_export_committed_head_still_refuses_a_real_include_after_a_c_string_literal() {
        // A-R-4 round-6 review: `string_literal_start` didn't recognize
        // `c"..."` (a C string literal, Rust 2021+) at all — its opening
        // `"` is preceded by the alphanumeric `c`, so the
        // identifier-boundary check rejected it, leaving the masking pass
        // to treat `c` as ordinary code. The literal's CLOSING `"`
        // (preceded by `.`, not alphanumeric) was then mistaken for the
        // OPENING of a brand-new string, and the search for its closing
        // `"` swallowed everything up to and including the real
        // `include_str!(` call that followed — masking the genuine call's
        // own NAME and hiding it from the search entirely (worse than
        // merely skipping a recognized-but-out-of-scope call: this call
        // was never found at all).
        let (tmp, harness) = init_repo_for_export_tests();
        std::fs::write(
            harness.join("src/lib.rs"),
            "pub const C: &core::ffi::CStr = c\"hello.\";\n\
             pub const DATA: &str = include_str!(\"/tmp/example.txt\");\n",
        )
        .unwrap();
        run_git(tmp.path(), &["add", "-A"]);
        run_git(
            tmp.path(),
            &[
                "commit",
                "-q",
                "-m",
                "add C-string literal preceding a real absolute include_str!",
            ],
        );

        let result =
            crate::doctor::checks::harness_buildable::export_committed_head_for_tests(tmp.path());
        let err = result.expect_err(
            "a genuine include_str! call following a C-string literal \
             elsewhere in the file must still be found and refused",
        );
        assert!(
            err.contains("include_str!") && err.contains("outside this export's root"),
            "error must name the escaping include! call and explain why, got: {err}"
        );
    }

    #[test]
    fn test_export_committed_head_refuses_an_absolute_include_str_target_built_via_concat() {
        // A-R-4 round-6 review, F4: `include_str!(concat!(...))` with an
        // all-literal-argument `concat!` call was an explicitly accepted
        // gap — under this review's rules, a deferred MAJOR still counts
        // as must-fix. `try_resolve_concat_literal` closes the bounded
        // case where every `concat!` argument is itself a plain literal.
        let (tmp, harness) = init_repo_for_export_tests();
        std::fs::write(
            harness.join("src/lib.rs"),
            "pub const DATA: &str = include_str!(concat!(\"/tmp/\", \"example.txt\"));\n",
        )
        .unwrap();
        run_git(tmp.path(), &["add", "-A"]);
        run_git(
            tmp.path(),
            &[
                "commit",
                "-q",
                "-m",
                "add absolute include_str! target built via an all-literal concat!",
            ],
        );

        let result =
            crate::doctor::checks::harness_buildable::export_committed_head_for_tests(tmp.path());
        let err = result.expect_err(
            "an absolute include_str! target built from an all-literal \
             concat!(...) call must be resolved and refused, not silently \
             allowed through as an unrecognized expression",
        );
        assert!(
            err.contains("include_str!") && err.contains("outside this export's root"),
            "error must name the escaping include! call and explain why, got: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_export_committed_head_refuses_an_include_target_escaping_through_a_committed_symlink() {
        // A-R-4 round-4 review: a purely lexical `..`/`.` walk of an
        // include! target, with NO awareness of committed symlinks,
        // misses an escape that chains through one. A committed
        // `link -> .` at the repo root, referenced as
        // `../../../link/../outside.txt` from `.hex/harness/src/lib.rs`,
        // resolves `link` on the real filesystem BEFORE applying the `..`
        // that follows it — landing one level above the repo root,
        // outside the export entirely. The guard must use the SAME
        // symlink-chain-aware resolver (`resolve_realpath_within_export`)
        // the materialized-symlink checks already use, not a simpler
        // lexical-only walk.
        let (tmp, harness) = init_repo_for_export_tests();
        std::os::unix::fs::symlink(".", tmp.path().join("link")).unwrap();
        std::fs::write(
            harness.join("src/lib.rs"),
            "pub const DATA: &str = include_str!(\"../../../link/../outside.txt\");\n",
        )
        .unwrap();
        run_git(tmp.path(), &["add", "-A"]);
        run_git(
            tmp.path(),
            &[
                "commit",
                "-q",
                "-m",
                "add symlink-mediated escaping include",
            ],
        );

        let result =
            crate::doctor::checks::harness_buildable::export_committed_head_for_tests(tmp.path());
        let err = result.expect_err(
            "an include! target that escapes the export root by chaining \
             through a committed symlink must be refused, not silently \
             allowed through by a symlink-blind lexical check",
        );
        assert!(
            err.contains("include_str!") && err.contains("outside this export's root"),
            "error must name the escaping include! call and explain why, got: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_export_committed_head_never_runs_filters_so_fabricated_modules_still_fail() {
        // (c) `src/lib.rs` declares `mod missing_dep;` whose file is
        // genuinely untracked. A locally configured filter would, on an
        // ORDINARY checkout, fabricate `missing_dep.rs` as a side effect
        // (mirroring the round-2 "smudge filter fabricates a symlink alias"
        // finding). The export must never invoke that filter at all, so the
        // fabricated file must never appear in the export, and an offline
        // `cargo check` against the export must fail.
        let (tmp, harness) = init_repo_for_export_tests();
        std::fs::write(harness.join("src/lib.rs"), "mod missing_dep;\n").unwrap();
        std::fs::write(harness.join("src/real.rs"), "pub const REAL: i32 = 1;\n").unwrap();
        run_git(
            tmp.path(),
            &[
                "config",
                "filter.hex-doctor-export-fabricate.smudge",
                "ln -sfn real.rs .hex/harness/src/missing_dep.rs; cat",
            ],
        );
        run_git(
            tmp.path(),
            &["config", "filter.hex-doctor-export-fabricate.clean", "cat"],
        );
        run_git(
            tmp.path(),
            &[
                "config",
                "filter.hex-doctor-export-fabricate.required",
                "true",
            ],
        );
        std::fs::write(harness.join("zzz_trigger.txt"), "trigger").unwrap();
        std::fs::write(
            harness.join(".gitattributes"),
            "zzz_trigger.txt filter=hex-doctor-export-fabricate\n",
        )
        .unwrap();
        run_git(tmp.path(), &["add", "-A"]);
        run_git(
            tmp.path(),
            &[
                "commit",
                "-q",
                "-m",
                "mod missing_dep with fabrication trap",
            ],
        );

        let export =
            crate::doctor::checks::harness_buildable::export_committed_head_for_tests(tmp.path())
                .expect("export itself must succeed even though the crate won't build");

        let fabricated = export.path().join(".hex/harness/src/missing_dep.rs");
        assert!(
            !fabricated.exists(),
            "the export must never invoke any filter driver — a \
             checkout-time fabrication trap must not appear anywhere in \
             the exported tree, found {}",
            fabricated.display()
        );

        let target_dir = tempfile::tempdir().unwrap();
        let output = std::process::Command::new("cargo")
            .args(["check", "-p", "fixture-harness", "--offline", "--locked"])
            .current_dir(export.path().join(".hex/harness"))
            .env("CARGO_TARGET_DIR", target_dir.path())
            .env("CARGO_NET_OFFLINE", "true")
            .output()
            .expect("cargo check spawns");
        assert!(
            !output.status.success(),
            "`mod missing_dep;` with no committed missing_dep.rs must fail \
             an offline `cargo check` against the exported tree even though \
             a checkout-time filter would have fabricated it, stdout: {} \
             stderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn test_export_committed_head_then_offline_cargo_check_passes_for_a_buildable_tree() {
        // (d) The passing case: a committed, buildable tree exported via
        // `export_committed_head_for_tests` passes `cargo check -p
        // fixture-harness --offline --locked` run directly in the export
        // directory.
        let tmp = init_repo_committed_include();
        let export =
            crate::doctor::checks::harness_buildable::export_committed_head_for_tests(tmp.path())
                .expect("export of a clean, buildable HEAD must succeed");

        let target_dir = tempfile::tempdir().unwrap();
        let output = std::process::Command::new("cargo")
            .args(["check", "-p", "fixture-harness", "--offline", "--locked"])
            .current_dir(export.path().join(".hex/harness"))
            .env("CARGO_TARGET_DIR", target_dir.path())
            .env("CARGO_NET_OFFLINE", "true")
            .output()
            .expect("cargo check spawns");
        assert!(
            output.status.success(),
            "a committed, buildable tree exported via git ls-tree + \
             cat-file must pass `cargo check -p fixture-harness --offline \
             --locked`, stdout: {} stderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // ---- verifier gaps re-pinned after the build-based redesign (workflow
    // ledger, adjudication-and-gates task): F1, F19, F18 all pinned real
    // behavior under the OLD scanner design; the redesign deleted their
    // fixtures along with the scanner code they exercised, but the
    // underlying contract items still apply to the new `cargo check`-based
    // mechanism and had no replacement test. ----

    /// F19: a `Cargo.toml` fully committed but with NO `Cargo.lock` at all
    /// (never generated, never committed) must FAIL — never an
    /// inconclusive WARN — naming cargo's own lockfile-specific
    /// diagnostic. A bare `Status::Fail` alone would also hold if the
    /// export broke or `.hex/harness` went missing for an unrelated
    /// reason, so this pins cargo's exact wording (verified by hand
    /// against a real `cargo check -p hex-harness --offline --locked`
    /// with no lockfile present: "error: cannot create the lock file
    /// ... because --locked was passed to prevent this").
    #[test]
    fn test_harness_buildable_fails_naming_cargo_own_diagnostic_when_lockfile_is_missing() {
        let tmp = tempfile::tempdir().unwrap();
        run_git(tmp.path(), &["init", "-q"]);
        let harness = tmp.path().join(".hex/harness");
        std::fs::create_dir_all(harness.join("src")).unwrap();
        std::fs::write(harness.join("Cargo.toml"), HEX_HARNESS_MANIFEST).unwrap();
        std::fs::write(harness.join("src/lib.rs"), "pub fn f() -> i32 { 1 }\n").unwrap();
        // Deliberately never written or committed: .hex/harness/Cargo.lock
        run_git(tmp.path(), &["add", "-A"]);
        run_git(
            tmp.path(),
            &["commit", "-q", "-m", "fixture with no Cargo.lock at all"],
        );

        let result = run_harness_buildable(tmp.path());
        assert_eq!(
            result.status,
            Status::Fail,
            "a committed tree with no Cargo.lock at all must FAIL, not \
             WARN — this is a genuine repository-input problem, not an \
             inconclusive one: {result:?}"
        );
        assert!(
            result.message.contains("cannot create the lock file")
                && result
                    .message
                    .contains("--locked was passed to prevent this"),
            "must surface cargo's own lockfile-specific diagnostic \
             verbatim, not a generic message that merely happens to \
             mention Cargo.lock, got: {:?}",
            result
        );
    }

    /// F1: a registry dependency the lockfile resolves to, but that is not
    /// present in the LOCAL cargo registry cache (`CARGO_HOME` pointed at
    /// an empty, never-warmed directory) must WARN — never FAIL, and
    /// never recommend `git add`, since nothing here is missing from git.
    /// `once_cell 1.19.0`'s checksum is a real, previously-verified value
    /// (see history at commit 6bf6577) — its exact bytes are irrelevant to
    /// this test since `--offline` fails before ever downloading (and so
    /// verifying) the crate.
    #[test]
    fn test_harness_buildable_warns_when_dependency_cache_is_unavailable() {
        let tmp = tempfile::tempdir().unwrap();
        run_git(tmp.path(), &["init", "-q"]);
        let harness = tmp.path().join(".hex/harness");
        std::fs::create_dir_all(harness.join("src")).unwrap();
        std::fs::write(
            harness.join("Cargo.toml"),
            "[package]\nname = \"hex-harness\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [dependencies]\nonce_cell = \"1.19.0\"\n",
        )
        .unwrap();
        std::fs::write(
            harness.join("Cargo.lock"),
            "# This file is automatically @generated by Cargo.\n\
             # It is not intended for manual editing.\n\
             version = 4\n\n\
             [[package]]\n\
             name = \"hex-harness\"\n\
             version = \"0.1.0\"\n\
             dependencies = [\n \"once_cell\",\n]\n\n\
             [[package]]\n\
             name = \"once_cell\"\n\
             version = \"1.19.0\"\n\
             source = \"registry+https://github.com/rust-lang/crates.io-index\"\n\
             checksum = \"3fe4419f0b09f6c7c3b40f6be0e6a2e08d70b03cd8f3c4a19e4f9db99e03f88\"\n",
        )
        .unwrap();
        std::fs::write(harness.join("src/lib.rs"), "pub fn f() {}\n").unwrap();
        run_git(tmp.path(), &["add", "-A"]);
        run_git(
            tmp.path(),
            &["commit", "-q", "-m", "fixture with a real registry dep"],
        );

        let empty_cargo_home = tempfile::tempdir().unwrap();
        let result =
            crate::doctor::checks::harness_buildable::run_check_with_timeout_and_env_override(
                tmp.path(),
                std::time::Duration::from_secs(60),
                &[(
                    "CARGO_HOME",
                    empty_cargo_home.path().to_str().expect("utf8 path"),
                )],
            );
        assert_eq!(
            result.status,
            Status::Warn,
            "a registry dependency missing only from the local cache (not \
             from git) must be inconclusive (WARN), never FAIL, got {:?}",
            result
        );
        let msg = result.message.to_lowercase();
        assert!(
            !msg.contains("git add"),
            "must not recommend `git add` for a dependency-cache problem, \
             got: {}",
            result.message
        );
    }

    /// F18: a machine with `commit.gpgsign` and `core.hooksPath`
    /// configured locally must not be able to break fixture setup —
    /// `run_git`'s commit step must override both, command-local, without
    /// ever mutating process-global env (tests run concurrently).
    #[test]
    fn test_fixture_commit_survives_inherited_signing_and_hooks_config() {
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
        // Must succeed despite the hostile local config above — would
        // panic inside `run_git` if the commit step ever stopped
        // overriding both settings command-local.
        run_git(tmp.path(), &["commit", "-q", "-m", "fixture commit"]);
    }

    /// A-R-1 (arrra/hex PR #7, workflow wf_c16ed20d-1bb final round): F11's
    /// original fix made this check permanently non-functional on the real
    /// `~/hex` repository it exists to protect. `export_committed_head`
    /// walked the WHOLE committed tree and hard-aborted with WARN the
    /// moment ANY gitlink appeared anywhere in it — but a gitlink wholly
    /// unrelated to `.hex/harness`'s own build (exactly the shape of
    /// `~/hex`'s own accidental `.hex/.upgrade-cache` nested-repo entry,
    /// confirmed live and reproducible) must not prevent the crate from
    /// being verified at all. A gitlink this build never reads is no
    /// different from any other file this build never reads: skip
    /// materializing it (nothing this export can put there is real content
    /// anyway — a gitlink names a commit in another repository, not a blob
    /// this repo's own object database holds) and let `cargo check` decide
    /// whether its absence matters. An unreferenced gitlink therefore PASSes;
    /// a REFERENCED one still surfaces as a FAIL naming the missing path
    /// (see the sibling test below) — by compilation, not a blanket WARN
    /// that can never distinguish the two cases.
    #[test]
    fn test_harness_buildable_passes_with_an_unreferenced_gitlink_elsewhere_in_the_tree() {
        let tmp = tempfile::tempdir().unwrap();
        run_git(tmp.path(), &["init", "-q"]);
        let harness = tmp.path().join(".hex/harness");
        std::fs::create_dir_all(harness.join("src")).unwrap();
        std::fs::write(harness.join("Cargo.toml"), HEX_HARNESS_MANIFEST).unwrap();
        std::fs::write(harness.join("Cargo.lock"), HEX_HARNESS_LOCKFILE).unwrap();
        std::fs::write(harness.join("src/lib.rs"), "pub fn f() -> i32 { 1 }\n").unwrap();
        run_git(tmp.path(), &["add", "-A"]);
        // A gitlink OUTSIDE .hex/harness entirely — e.g. an accidentally
        // nested repo elsewhere in the checkout, like ~/hex's own
        // `.hex/.upgrade-cache` — that the crate under test never reads.
        run_git(
            tmp.path(),
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                "160000,1111111111111111111111111111111111111111,unrelated-nested-repo",
            ],
        );
        run_git(
            tmp.path(),
            &[
                "commit",
                "-q",
                "-m",
                "fixture with an unrelated, unreferenced gitlink",
            ],
        );

        let result = run_harness_buildable(tmp.path());
        assert_eq!(
            result.status,
            Status::Pass,
            "A-R-1: a gitlink this build never reads must not block \
             verifying the build that the check actually exists to \
             protect: {result:?}"
        );
    }

    /// A-R-1 sibling: a gitlink the build DOES reference (via
    /// `include_str!`) must still surface as a FAIL naming the missing
    /// path, by compilation — proving A-R-1's fix did not also silently
    /// swallow the case F11 originally existed to catch.
    #[test]
    fn test_harness_buildable_fails_naming_a_referenced_uninitialized_gitlink_path() {
        let tmp = tempfile::tempdir().unwrap();
        run_git(tmp.path(), &["init", "-q"]);
        let harness = tmp.path().join(".hex/harness");
        std::fs::create_dir_all(harness.join("src")).unwrap();
        std::fs::write(harness.join("Cargo.toml"), HEX_HARNESS_MANIFEST).unwrap();
        std::fs::write(harness.join("Cargo.lock"), HEX_HARNESS_LOCKFILE).unwrap();
        std::fs::write(
            harness.join("src/lib.rs"),
            "pub const DATA: &str = include_str!(\"vendor/data.txt\");\n",
        )
        .unwrap();
        run_git(tmp.path(), &["add", "-A"]);
        // The gitlink sits exactly where the crate's own include_str! target
        // would be — its content is genuinely required by the build.
        run_git(
            tmp.path(),
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                "160000,1111111111111111111111111111111111111111,.hex/harness/src/vendor/data.txt",
            ],
        );
        run_git(
            tmp.path(),
            &[
                "commit",
                "-q",
                "-m",
                "fixture whose include_str! target is an uninitialized gitlink",
            ],
        );

        let result = run_harness_buildable(tmp.path());
        assert_eq!(
            result.status,
            Status::Fail,
            "A-R-1: a gitlink the build actually reads must still FAIL by \
             compilation, naming the missing path — the check must not lose \
             this real detection because it stopped treating every gitlink \
             as an automatic WARN: {result:?}"
        );
        let msg = result.message.to_lowercase();
        assert!(
            msg.contains("data.txt"),
            "the FAIL must name the missing include_str! target, got: {}",
            result.message
        );
    }

    /// F14 (re-added after the scanner deletion, adapted for the new
    /// design): the redesign replaced `git worktree add` + checkout
    /// filters with `git ls-tree`/`git cat-file --batch` plumbing, which
    /// runs no filters or hooks at all — the OLD F14 fixture (a hanging
    /// smudge filter) no longer applies. The same wall-clock discipline
    /// still must: a wedged `git` binary (lock contention, a hung
    /// credential helper some environments configure globally) must never
    /// hang this health check any more than a wedged `cargo` can.
    ///
    /// B-F1 (round-2 review): the original version of this test simulated
    /// a wedged `git` by mutating the process-wide `PATH` env var with
    /// `std::env::set_var`, guarded by a mutex that only serialized it
    /// against copies of ITSELF — a genuine data race under `cargo
    /// test`'s default multi-threaded runner, since virtually every OTHER
    /// test in this module spawns `git` with no per-command PATH override
    /// and so inherits whatever `PATH` happens to be live in the process
    /// at spawn time, not a snapshot taken at test start. This version
    /// threads the override through `export_committed_head_for_tests_with_timeout_and_env_override`
    /// as a per-`Command` `cmd.env()` call instead — exactly the pattern
    /// `run_check_with_timeout_and_path_override` already uses for the
    /// `cargo check` child (see the toolchain-unreachable tests above) —
    /// so this test never touches process-global state and needs no mutex
    /// at all.
    #[test]
    fn test_export_committed_head_bounds_git_ls_tree_to_the_same_wall_clock_cap_as_cargo_check() {
        let real_git = String::from_utf8(
            std::process::Command::new("sh")
                .args(["-c", "command -v git"])
                .output()
                .expect("resolve real git")
                .stdout,
        )
        .expect("utf8 git path")
        .trim()
        .to_string();
        assert!(!real_git.is_empty(), "must resolve a real `git` on PATH");

        let bin_dir = tempfile::tempdir().unwrap();
        std::fs::write(
            bin_dir.path().join("git"),
            format!(
                "#!/bin/sh\n\
                 if [ \"$1\" = \"ls-tree\" ]; then\n  sleep 5\nfi\n\
                 exec \"{real_git}\" \"$@\"\n"
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                bin_dir.path().join("git"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }

        let tmp = init_hex_harness_repo(
            &[(".hex/harness/src/lib.rs", "pub fn f() -> i32 { 1 }\n")],
            &[],
        );

        // Read-only: takes a snapshot of the current PATH to prepend the
        // wrapper dir to, but never writes it back — no process-global
        // mutation anywhere in this test.
        let prev_path = std::env::var("PATH").unwrap_or_default();
        let overridden_path = format!("{}:{prev_path}", bin_dir.path().display());
        let start = std::time::Instant::now();
        let result =
            crate::doctor::checks::harness_buildable::export_committed_head_for_tests_with_timeout_and_env_override(
                tmp.path(),
                std::time::Duration::from_millis(300),
                &[("PATH", &overridden_path)],
            );
        let elapsed = start.elapsed();

        assert!(
            elapsed < std::time::Duration::from_secs(3),
            "F14: `git ls-tree` must be bounded by the same wall-clock cap \
             as `cargo check` instead of blocking for however long a \
             wedged git binary takes — a 300ms cap against a `git` that \
             sleeps 5s on `ls-tree` must return well inside a generous \
             ceiling, took {elapsed:?}"
        );
        assert!(
            result.is_err(),
            "a `git ls-tree` that cannot finish inside the wall-clock cap \
             must be reported as an export failure (surfaced as WARN by \
             the caller), not silently treated as success: {result:?}"
        );
    }

    /// A-R-F14-1 (round-2 major, adjudication file): the fix above (and the
    /// `cat-file --batch` call right after it in `export_committed_head`)
    /// each independently received the FULL `timeout` value instead of a
    /// single shared, decreasing budget — three commands that are each
    /// individually fast enough to finish under the cap can still sum to
    /// several times the advertised wall-clock cap before anything is
    /// ever killed. Reproduced exactly the way the finding's own probe
    /// did: a `git` wrapper that sleeps on BOTH the `ls-tree` and
    /// `cat-file` arms, each individually well under the injected cap.
    /// Under a per-command (re-armed) budget, `ls-tree` finishes inside
    /// its own fresh window and `cat-file` finishes inside a SECOND fresh
    /// window unaware of the first's elapsed time, so the export as a
    /// whole silently succeeds in roughly double the single cap it was
    /// given. Under a correctly shared budget, `cat-file` only inherits
    /// whatever the cap has left over after `ls-tree` consumed most of
    /// it, and must time out instead.
    #[test]
    fn test_export_committed_head_shares_one_wall_clock_budget_across_ls_tree_and_cat_file() {
        let real_git = String::from_utf8(
            std::process::Command::new("sh")
                .args(["-c", "command -v git"])
                .output()
                .expect("resolve real git")
                .stdout,
        )
        .expect("utf8 git path")
        .trim()
        .to_string();
        assert!(!real_git.is_empty(), "must resolve a real `git` on PATH");

        let bin_dir = tempfile::tempdir().unwrap();
        std::fs::write(
            bin_dir.path().join("git"),
            format!(
                "#!/bin/sh\n\
                 if [ \"$1\" = \"ls-tree\" ] || [ \"$1\" = \"cat-file\" ]; then\n  sleep 3\nfi\n\
                 exec \"{real_git}\" \"$@\"\n"
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                bin_dir.path().join("git"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }

        let tmp = init_hex_harness_repo(
            &[(".hex/harness/src/lib.rs", "pub fn f() -> i32 { 1 }\n")],
            &[],
        );

        let prev_path = std::env::var("PATH").unwrap_or_default();
        let overridden_path = format!("{}:{prev_path}", bin_dir.path().display());
        let start = std::time::Instant::now();
        let result =
            crate::doctor::checks::harness_buildable::export_committed_head_for_tests_with_timeout_and_env_override(
                tmp.path(),
                std::time::Duration::from_secs(4),
                &[("PATH", &overridden_path)],
            );
        let elapsed = start.elapsed();

        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "the 4s cap given to this export must bound `ls-tree` AND \
             `cat-file` TOGETHER, not re-arm a fresh 4s window for each — \
             a git wrapper that sleeps 3s on EACH of the two commands \
             finishes both within their own independent 4s windows \
             (total ~6s) if the budget is not shared; a shared budget \
             leaves `cat-file` only ~1s after `ls-tree` spends ~3s of the \
             4s cap, so it must time out well before the second command \
             could ever finish. took {elapsed:?}"
        );
        assert!(
            result.is_err(),
            "`cat-file --batch` must be starved of whatever budget \
             `ls-tree` already spent and time out, not silently succeed \
             on a second, independently-capped window: {result:?}"
        );
    }
}
