/// Port of .hex/scripts/metrics/run-all.sh
///
/// Runs all user-outcome metric scripts under $HEX_DIR/.hex/scripts/metrics/
/// and prints PASS/FAIL for each. Also snapshots telemetry-ratio data.
use std::path::{Path, PathBuf};
use std::process::Command;

const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

fn red(msg: &str) {
    println!("{}{}{}", RED, msg, RESET);
}

fn green(msg: &str) {
    println!("{}{}{}", GREEN, msg, RESET);
}

fn bold(msg: &str) {
    println!("{}{}{}", BOLD, msg, RESET);
}

fn run_metric(name: &str, script: &Path, overall: &mut i32) {
    if !script.exists() {
        red(&format!("  MISSING: {} — script not found at {}", name, script.display()));
        *overall = 1;
        return;
    }
    let result = Command::new("python3").arg(script).output();
    match result {
        Err(e) => {
            red(&format!("  FAIL (exec error): {} — {}", name, e));
            *overall = 1;
        }
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            let combined = if stderr.trim().is_empty() {
                stdout.trim().to_string()
            } else {
                format!("{} {}", stdout.trim(), stderr.trim())
            };
            let rc = out.status.code().unwrap_or(1);
            if rc == 0 {
                green(&format!("  PASS: {} — {}", name, combined));
            } else if rc == 2 {
                red(&format!("  FAIL (threshold breached): {} — {}", name, combined));
                *overall = 1;
            } else {
                red(&format!("  FAIL (script error rc={}): {} — {}", rc, name, combined));
                *overall = 1;
            }
        }
    }
}

fn snapshot_telemetry_ratio(scripts_dir: &Path) {
    let ratio_script = scripts_dir.join("telemetry-ratio.py");
    if !ratio_script.exists() {
        red(&format!(
            "  MISSING: telemetry-ratio snapshot — script not found at {}",
            ratio_script.display()
        ));
        return;
    }

    let result = Command::new("python3")
        .arg(&ratio_script)
        .arg("--json")
        .arg("--hours")
        .arg("24")
        .output();

    let json_out = match result {
        Err(_) => return,
        Ok(out) => {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if s.is_empty() {
                red("  WARN: telemetry-ratio produced no output — skipping snapshot");
                return;
            }
            s
        }
    };

    let snapshots_file = match std::env::var("HOME") {
        Ok(h) => PathBuf::from(h).join(".hex/audit/metric-snapshots.jsonl"),
        Err(_) => return,
    };

    // Write snapshot via inline Python (same logic as original shell script)
    let py = r#"
import json, sys, datetime, os

raw, snapshots_path = sys.argv[1], sys.argv[2]
try:
    data = json.loads(raw)
except json.JSONDecodeError as e:
    print(f"  WARN: could not parse telemetry-ratio JSON: {e}", file=sys.stderr)
    sys.exit(0)

ts = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
os.makedirs(os.path.dirname(snapshots_path), exist_ok=True)

lines = []
for surface in data.get("surfaces", []):
    entry = {
        "ts": ts,
        "source": "telemetry-ratio",
        "window_hours": data.get("window_hours", 24),
        "surface": surface["surface"],
        "inputs": surface["inputs"],
        "outputs": surface["outputs"],
        "ratio_pct": surface["ratio_pct"],
        "status": surface["status"],
    }
    if surface.get("unique_inputs") is not None:
        entry["unique_inputs"] = surface["unique_inputs"]
        entry["unique_outputs"] = surface["unique_outputs"]
        entry["ratio_pct"] = surface["ratio_pct"]
    lines.append(json.dumps(entry))

tmp = snapshots_path + ".tmp"
with open(tmp, "a") as f:
    f.write("\n".join(lines) + "\n")
os.replace(tmp, snapshots_path)

boi = next((s for s in data.get("surfaces", []) if s["surface"] == "boi"), None)
if boi and boi.get("unique_inputs") is not None:
    print(f"  telemetry-ratio snapshot written: boi {boi['ratio_pct']}% ({boi['unique_inputs']} unique inputs -> {boi['unique_outputs']} unique outputs) [{boi['status']}]")
else:
    print(f"  telemetry-ratio snapshot written ({len(lines)} surfaces)")
"#;

    let out = Command::new("python3")
        .arg("-c")
        .arg(py)
        .arg(&json_out)
        .arg(snapshots_file.to_str().unwrap_or(""))
        .output();

    if let Ok(out) = out {
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        if !stdout.trim().is_empty() {
            println!("{}", stdout.trim());
        }
        if !stderr.trim().is_empty() {
            eprintln!("{}", stderr.trim());
        }
    }
}

pub fn run_all(hex_dir: &Path) {
    let metrics_dir = hex_dir.join(".hex/scripts/metrics");
    let scripts_dir = hex_dir.join(".hex/scripts");
    let mut overall: i32 = 0;

    bold("══ User-Outcome Metrics ══");

    run_metric("frustration-signals",     &metrics_dir.join("frustration-signals.py"),     &mut overall);
    run_metric("feedback-recurrence",     &metrics_dir.join("feedback-recurrence.py"),     &mut overall);
    run_metric("loop-waste-detection",    &metrics_dir.join("loop-waste-detection.py"),    &mut overall);
    run_metric("done-claim-verification", &metrics_dir.join("done-claim-verification.py"), &mut overall);
    run_metric("context-continuity",      &metrics_dir.join("context-continuity.py"),      &mut overall);

    bold("");
    bold("══ Telemetry Ratio Snapshot ══");
    snapshot_telemetry_ratio(&scripts_dir);

    println!();
    if overall == 0 {
        green("Overall: PASS");
    } else {
        red("Overall: FAIL");
    }

    std::process::exit(overall);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_metric_missing_script_sets_overall() {
        let mut overall = 0;
        run_metric("test-metric", Path::new("/nonexistent/path/script.py"), &mut overall);
        assert_eq!(overall, 1, "missing script must set overall to 1");
    }
}
