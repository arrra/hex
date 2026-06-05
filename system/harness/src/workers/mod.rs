//! `hex::workers` — the registry of typed Rust workers hosted by the harness.
//!
//! Each submodule defines one worker via `pub fn worker() -> Worker`. The
//! `registry()` function returns them all; the harness runtime registers
//! each worker's `(TriggerSpec, Handler)` pairs with iii at startup.

pub mod backup;
pub mod e2e;
pub mod memory_maintenance;

use crate::worker::Worker;

/// Return all workers hosted by the hex harness.
///
/// The test-only `hex-e2e` worker is appended ONLY when `HEX_HARNESS_E2E=1`
/// (set by the harness-e2e container) — it never registers in a real deployment.
pub fn registry() -> Vec<Worker> {
    let mut workers = vec![memory_maintenance::worker(), backup::worker()];
    if std::env::var("HEX_HARNESS_E2E").as_deref() == Ok("1") {
        workers.push(e2e::worker());
    }
    workers
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::TriggerSpec;

    fn cron_exprs(w: &Worker) -> Vec<String> {
        w.handlers
            .iter()
            .filter_map(|(spec, _)| match spec {
                TriggerSpec::Cron { expression } => Some(expression.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn registry_includes_memory_maintenance_and_backup() {
        let reg = registry();
        let names: Vec<&str> = reg.iter().map(|w| w.name.as_str()).collect();
        assert!(names.contains(&"hex-memory-maintenance"));
        assert!(names.contains(&"hex-backup"));
    }

    #[test]
    fn memory_maintenance_cron_matches_yaml() {
        let reg = registry();
        let mm = reg
            .iter()
            .find(|w| w.name == "hex-memory-maintenance")
            .expect("hex-memory-maintenance present");
        let exprs = cron_exprs(mm);
        assert!(exprs.iter().any(|e| e == memory_maintenance::CRON_INDEX));
        assert!(exprs
            .iter()
            .any(|e| e == memory_maintenance::CRON_CONSOLIDATE_FULL));
    }

    #[test]
    fn backup_is_cron_worker() {
        let reg = registry();
        let bk = reg
            .iter()
            .find(|w| w.name == "hex-backup")
            .expect("hex-backup present");
        assert!(!cron_exprs(bk).is_empty());
    }
}
