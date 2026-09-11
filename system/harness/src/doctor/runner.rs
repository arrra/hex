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
            &["commit", "-q", "-m", "fixture with chained escaping symlink"],
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
}
