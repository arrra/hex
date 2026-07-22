use crate::doctor::check::{CheckResult, Status};
use serde_json::json;

/// Print a human-readable doctor report.
/// Format is byte-compatible with doctor.sh for launchd watchdog parsing.
pub fn print_text(results: &[(String, CheckResult)], quiet: bool) {
    for (name, result) in results {
        let tag = match result.status {
            Status::Pass => "PASS",
            Status::Warn => "WARN",
            Status::Fail => "ERROR",
            Status::Fixed => "FIXED",
            Status::Info => "INFO",
            Status::Skip => "SKIP",
        };
        if quiet && result.status.counts_as_pass() {
            continue;
        }
        if result.status == Status::Info {
            println!("  → {}", result.message);
        } else {
            println!("  [{}] {}: {}", tag, name, result.message);
        }
        if let Some(details) = &result.details {
            for line in details.lines() {
                println!("        {}", line);
            }
        }
    }

    let pass_count = results
        .iter()
        .filter(|(_, r)| r.status.counts_as_pass())
        .count();
    let warn_count = results
        .iter()
        .filter(|(_, r)| r.status.is_warning())
        .count();
    let error_count = results.iter().filter(|(_, r)| r.status.is_error()).count();
    let fixed_count = results
        .iter()
        .filter(|(_, r)| r.status == Status::Fixed)
        .count();

    println!();
    println!(
        "  Summary: {} passed, {} warnings, {} errors, {} fixed",
        pass_count, warn_count, error_count, fixed_count
    );
}

/// Print JSON output compatible with doctor.sh --json schema.
pub fn print_json(results: &[(String, CheckResult)]) {
    let pass_count = results
        .iter()
        .filter(|(_, r)| r.status.counts_as_pass())
        .count();
    let warn_count = results
        .iter()
        .filter(|(_, r)| r.status.is_warning())
        .count();
    let error_count = results.iter().filter(|(_, r)| r.status.is_error()).count();
    let fixed_count = results
        .iter()
        .filter(|(_, r)| r.status == Status::Fixed)
        .count();

    let overall = if error_count > 0 {
        "error"
    } else if warn_count > 0 {
        "warning"
    } else {
        "pass"
    };

    let checks: Vec<_> = results
        .iter()
        .enumerate()
        .map(|(i, (name, r))| {
            let status_str = match r.status {
                Status::Pass => "pass",
                Status::Warn => "warning",
                Status::Fail => "error",
                Status::Fixed => "fixed",
                Status::Info => "info",
                Status::Skip => "skip",
            };
            let mut obj = json!({
                "id": i + 1,
                "name": name,
                "status": status_str,
                "message": r.message,
                "elapsed_ms": r.elapsed_ms,
            });
            if let Some(details) = &r.details {
                obj["details"] = json!(details);
            }
            obj
        })
        .collect();

    let output = json!({
        "status": overall,
        "checks": checks,
        "summary": {
            "pass": pass_count,
            "warn": warn_count,
            "error": error_count,
            "fixed": fixed_count,
        }
    });
    println!("{}", serde_json::to_string_pretty(&output).unwrap());
}

/// Compute overall exit code from results (0=all pass, 1=errors, 2=warnings only).
pub fn exit_code(results: &[(String, CheckResult)]) -> i32 {
    if results.iter().any(|(_, r)| r.status.is_error()) {
        1
    } else if results.iter().any(|(_, r)| r.status.is_warning()) {
        2
    } else {
        0
    }
}
