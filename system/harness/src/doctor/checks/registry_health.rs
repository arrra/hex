use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};
use std::fs;

/// Flags orphaned bin/<id> entries — an executable exists but functions/<id>.json is absent.
/// This indicates a partial/aborted capability_add that left the registry in an inconsistent state.
pub struct RegistryOrphanedBin;

impl DoctorCheck for RegistryOrphanedBin {
    fn name(&self) -> &str {
        "registry-orphaned-bin"
    }
    fn category(&self) -> Category {
        Category::Health
    }

    fn run(&self, ctx: &Context) -> CheckResult {
        let registry_dir = ctx.hex_dir.join(".hex/registry");
        let bin_dir = registry_dir.join("bin");
        let fn_dir = registry_dir.join("functions");

        if !bin_dir.is_dir() {
            return CheckResult::pass("no registry/bin directory (no capabilities registered)");
        }

        let entries = match fs::read_dir(&bin_dir) {
            Ok(e) => e,
            Err(e) => return CheckResult::fail(format!("cannot read registry/bin: {e}")),
        };

        let mut orphans: Vec<String> = Vec::new();
        for entry in entries.flatten() {
            let bin_name = entry.file_name().to_string_lossy().to_string();
            let json_path = fn_dir.join(format!("{bin_name}.json"));
            if !json_path.exists() {
                orphans.push(bin_name);
            }
        }

        if orphans.is_empty() {
            CheckResult::pass("no orphaned registry/bin entries")
        } else {
            CheckResult::warn(format!(
                "{} orphaned bin entry(ies) — bin exists but functions/<id>.json missing: {}",
                orphans.len(),
                orphans.join(", ")
            ))
        }
    }
}

/// Flags stale .hex/registry/policies/registry-*.yaml files whose corresponding
/// trigger capability no longer exists in .hex/registry/triggers/<id>.json.
pub struct RegistryStalePolicy;

impl DoctorCheck for RegistryStalePolicy {
    fn name(&self) -> &str {
        "registry-stale-policy"
    }
    fn category(&self) -> Category {
        Category::Health
    }

    fn run(&self, ctx: &Context) -> CheckResult {
        let registry_dir = ctx.hex_dir.join(".hex/registry");
        let policies_dir = registry_dir.join("policies");
        let tr_dir = registry_dir.join("triggers");

        if !policies_dir.is_dir() {
            return CheckResult::pass("no registry/policies directory (no trigger policies)");
        }

        let entries = match fs::read_dir(&policies_dir) {
            Ok(e) => e,
            Err(e) => return CheckResult::fail(format!("cannot read registry/policies: {e}")),
        };

        let mut stale: Vec<String> = Vec::new();
        for entry in entries.flatten() {
            let file_name = entry.file_name().to_string_lossy().to_string();
            // Only inspect registry-<id>.yaml files
            if !file_name.starts_with("registry-") || !file_name.ends_with(".yaml") {
                continue;
            }
            // Extract <id> from "registry-<id>.yaml"
            let id = &file_name["registry-".len()..file_name.len() - ".yaml".len()];
            let trigger_json = tr_dir.join(format!("{id}.json"));
            if !trigger_json.exists() {
                stale.push(file_name);
            }
        }

        if stale.is_empty() {
            CheckResult::pass("no stale registry policy files")
        } else {
            CheckResult::warn(format!(
                "{} stale registry policy file(s) — trigger capability removed but policy remains: {}",
                stale.len(),
                stale.join(", ")
            ))
        }
    }
}
