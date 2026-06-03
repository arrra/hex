use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};
use std::process::Command;

/// check_17: BOI binary is present, reports a version, and daemon is responsive.
pub struct BoiHealth;

impl DoctorCheck for BoiHealth {
    fn name(&self) -> &str { "boi-health" }
    fn category(&self) -> Category { Category::Health }
    fn run(&self, ctx: &Context) -> CheckResult {
        let boi_bin = ctx.home.join(".boi/bin/boi");
        if !boi_bin.is_file() {
            return CheckResult::warn("~/.boi/bin/boi not found");
        }

        // Check version
        let version_out = Command::new(&boi_bin)
            .arg("--version")
            .output();

        let version = match version_out {
            Ok(o) if o.status.success() => {
                String::from_utf8_lossy(&o.stdout).trim().to_string()
            }
            _ => {
                return CheckResult::warn("boi binary found but --version failed");
            }
        };

        // boi-v2 control-socket daemon: the daemon binds ~/.boi/v2/daemon.sock.
        // (The old V1 ~/.boi/VERSIONS file + ~/.boi/bin/boi-wrapper checks were
        // dropped at the v3.0.0 cutover — boi-v2 creates neither.)
        // `boi --version` already prints "boi <semver>", so `version` carries
        // the "boi" prefix — don't prepend it again.
        let socket = ctx.home.join(".boi/v2/daemon.sock");
        if !socket.exists() {
            return CheckResult::warn(format!(
                "{} present but daemon socket ~/.boi/v2/daemon.sock missing (daemon not running?)",
                version
            ));
        }

        CheckResult::pass(format!("{} healthy", version))
    }
}
