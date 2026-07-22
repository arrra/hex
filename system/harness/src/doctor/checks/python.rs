use crate::doctor::check::{Category, CheckResult, Context, DoctorCheck};
use std::path::Path;
use std::process::Command;

/// check_18: Python 3.10+ is available — probes versioned binaries in brew paths first.
pub struct PythonVersion;

impl DoctorCheck for PythonVersion {
    fn name(&self) -> &str {
        "python-version"
    }
    fn category(&self) -> Category {
        Category::Health
    }
    fn run(&self, _ctx: &Context) -> CheckResult {
        // Mirror doctor.sh: probe versioned binaries in priority order, prefer brew paths
        let search_dirs = ["/opt/homebrew/bin", "/usr/local/bin", ""];
        let candidates = [
            "python3.14",
            "python3.13",
            "python3.12",
            "python3.11",
            "python3.10",
            "python3",
        ];

        for candidate in &candidates {
            // Try in explicit search dirs first, then rely on PATH (empty dir sentinel)
            let bins: Vec<String> = if search_dirs[0] != "" {
                search_dirs
                    .iter()
                    .filter(|d| !d.is_empty())
                    .map(|d| format!("{}/{}", d, candidate))
                    .filter(|p| Path::new(p).is_file())
                    .chain(std::iter::once(candidate.to_string()))
                    .collect()
            } else {
                vec![candidate.to_string()]
            };

            for bin in &bins {
                let out = Command::new(bin).arg("--version").output();
                if let Ok(o) = out {
                    if o.status.success() {
                        let ver_out = String::from_utf8_lossy(if o.stdout.is_empty() {
                            &o.stderr
                        } else {
                            &o.stdout
                        })
                        .trim()
                        .to_string();
                        if let Some(ver_str) = ver_out.strip_prefix("Python ") {
                            let parts: Vec<u32> =
                                ver_str.split('.').filter_map(|p| p.parse().ok()).collect();
                            if parts.len() >= 2
                                && (parts[0] > 3 || (parts[0] == 3 && parts[1] >= 10))
                            {
                                return CheckResult::pass(format!("{} {} (≥3.10)", bin, ver_str));
                            }
                            // Too old — keep looking for a newer one
                        }
                    }
                }
                // This binary path exists but failed — stop trying it, move to next candidate
                if bin != candidate {
                    break;
                }
            }
        }

        CheckResult::fail("no Python 3.10+ found — checked /opt/homebrew/bin, /usr/local/bin, PATH")
    }
}
