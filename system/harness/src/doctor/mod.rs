pub mod check;
pub mod checks;
pub mod cleanup_projects;
pub mod consolidate;
pub mod legacy;
pub mod reporter;
pub mod runner;

pub use check::{Category, CheckResult, Context, DoctorCheck, Status};
pub use legacy::{check_codex, detect_failure_pattern, quality_check, stale_deps};
pub use runner::Runner;
