/// Port of system/scripts/health/budget-period-reset.py
///
/// Auto-resets agent budget periods with a tiered safety gate:
///   ratio ≤ 1.0       → auto-reset (within budget)
///   1.0 < ratio ≤ 2.0 → auto-reset with audit log entry (minor_overage)
///   2.0 < ratio ≤ 5.0 → blocked, WARN alert
///   ratio > 5.0        → blocked, CRITICAL alert
use chrono::Utc;
use std::path::{Path, PathBuf};

const DEFAULT_PERIOD_DAYS: u64 = 7;
const SOURCE: &str = "budget-period-reset";

pub struct BudgetResetConfig {
    pub hex_dir: PathBuf,
    pub dry_run: bool,
}

enum AgentResult {
    Skip(String),
    Reset {
        overage_tag: &'static str,
        age_h: f64,
        spent: f64,
        budget: f64,
        ratio: f64,
    },
    Blocked {
        severity: &'static str,
        ratio: f64,
        spent: f64,
        budget: f64,
    },
    Error(String),
}

pub fn run(config: &BudgetResetConfig) -> i32 {
    if config.dry_run {
        let projects = config.hex_dir.join("projects");
        println!("[budget-period-reset] DRY RUN — no writes will occur");
        println!("[budget-period-reset] Projects dir: {}", projects.display());
        println!("[budget-period-reset] Default period: {}d", DEFAULT_PERIOD_DAYS);
        let count = std::fs::read_dir(&projects)
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|e| e.path().join("state.json").exists())
                    .count()
            })
            .unwrap_or(0);
        println!("[budget-period-reset] Would inspect {} agents with state.json", count);
        return 0;
    }

    let projects_dir = config.hex_dir.join("projects");
    if !projects_dir.exists() {
        eprintln!(
            "[budget-period-reset] ERROR: {} not found",
            projects_dir.display()
        );
        return 1;
    }

    let audit_dir = config.hex_dir.join(".hex/audit");
    let hex_alert = config.hex_dir.join(".hex/scripts/hex-alert.sh");

    let mut entries: Vec<_> = match std::fs::read_dir(&projects_dir) {
        Ok(e) => e.flatten().collect(),
        Err(e) => {
            eprintln!("[budget-period-reset] ERROR: cannot read projects dir: {e}");
            return 1;
        }
    };
    entries.sort_by_key(|e| e.file_name());

    let mut results: Vec<String> = Vec::new();
    let mut reset_count: u32 = 0;
    let mut blocked_count: u32 = 0;
    let mut skip_count: u32 = 0;
    let mut error_count: u32 = 0;

    for entry in entries {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        if name.starts_with('_') {
            continue;
        }

        let result = process_agent(&path, &audit_dir, &hex_alert);
        let summary = match &result {
            AgentResult::Skip(msg) => {
                skip_count += 1;
                format!("{name}: {msg}")
            }
            AgentResult::Reset {
                overage_tag,
                age_h,
                spent,
                budget,
                ratio,
            } => {
                reset_count += 1;
                let tag = if *overage_tag != "within_budget" {
                    format!(" [{overage_tag}]")
                } else {
                    String::new()
                };
                format!(
                    "{name}: RESET{tag} age={age_h:.1}h spent=${spent:.2}/${budget:.2} ({ratio:.1}x)"
                )
            }
            AgentResult::Blocked {
                severity,
                ratio,
                spent,
                budget,
            } => {
                blocked_count += 1;
                format!("{name}: BLOCKED {severity} {ratio:.1}x (${spent:.2}/${budget:.2})")
            }
            AgentResult::Error(msg) => {
                error_count += 1;
                format!("{name}: ERROR {msg}")
            }
        };
        results.push(summary);
    }

    println!(
        "\n[budget-period-reset] Summary: {} reset | {} blocked | {} skip | {} error",
        reset_count, blocked_count, skip_count, error_count
    );
    for r in &results {
        if !r.ends_with("skip (no state.json)") && !r.contains("skip (period age") {
            println!("  {r}");
        }
    }

    if error_count > 0 {
        1
    } else {
        0
    }
}

fn process_agent(project_dir: &Path, audit_dir: &Path, hex_alert: &Path) -> AgentResult {
    let agent_id = project_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();

    let state_path = project_dir.join("state.json");
    if !state_path.exists() {
        return AgentResult::Skip("skip (no state.json)".to_string());
    }

    let period_days = read_charter_period_days(project_dir);
    let period_seconds = (period_days * 86400) as f64;
    let now = Utc::now();
    let now_iso = now.to_rfc3339();

    let mut state: serde_json::Value = match std::fs::read_to_string(&state_path)
        .map_err(|e| e.to_string())
        .and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string()))
    {
        Ok(v) => v,
        Err(e) => return AgentResult::Error(e),
    };

    let start_str = match state
        .pointer("/cost/current_period/start")
        .and_then(|v| v.as_str())
    {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return AgentResult::Skip("skip (no period.start)".to_string()),
    };

    let start = match parse_iso(&start_str) {
        Some(t) => t,
        None => {
            return AgentResult::Skip(format!(
                "skip (unparseable period.start: {start_str:?})"
            ))
        }
    };

    let age_seconds = (now - start).num_seconds() as f64;
    if age_seconds < period_seconds {
        let age_h = age_seconds / 3600.0;
        return AgentResult::Skip(format!(
            "skip (period age {age_h:.1}h < {period_days}d)"
        ));
    }

    let budget_usd = state
        .pointer("/cost/current_period/budget_usd")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let spent_usd = state
        .pointer("/cost/current_period/spent_usd")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);

    if budget_usd == 0.0 {
        emit_alert(
            hex_alert,
            "WARN",
            &format!(
                "agent {agent_id}: budget_usd=0, cannot auto-reset — manual budget assignment required"
            ),
        );
        hex::audit::append(
            audit_dir,
            &agent_id,
            "budget-reset-skipped",
            &serde_json::json!({
                "reason": "zero_budget",
                "spent_usd": spent_usd,
                "period_age_h": age_seconds / 3600.0,
            }),
        );
        return AgentResult::Skip(format!("SKIP zero-budget (spent=${spent_usd:.2})"));
    }

    let ratio = spent_usd / budget_usd;

    if ratio > 5.0 {
        emit_alert(
            hex_alert,
            "CRITICAL",
            &format!(
                "agent {agent_id}: {ratio:.1}x runaway (${spent_usd:.2}/${budget_usd:.2}) — manual review required"
            ),
        );
        hex::audit::append(
            audit_dir,
            &agent_id,
            "budget-reset-blocked",
            &serde_json::json!({
                "reason": "5x_runaway",
                "spent_usd": spent_usd,
                "budget_usd": budget_usd,
                "ratio": ratio,
            }),
        );
        return AgentResult::Blocked {
            severity: "CRITICAL",
            ratio,
            spent: spent_usd,
            budget: budget_usd,
        };
    }

    if ratio > 2.0 {
        emit_alert(
            hex_alert,
            "WARN",
            &format!(
                "agent {agent_id}: {ratio:.1}x overage (${spent_usd:.2}/${budget_usd:.2}) — reset blocked, review spending"
            ),
        );
        hex::audit::append(
            audit_dir,
            &agent_id,
            "budget-reset-blocked",
            &serde_json::json!({
                "reason": "2x_5x_overage",
                "spent_usd": spent_usd,
                "budget_usd": budget_usd,
                "ratio": ratio,
            }),
        );
        return AgentResult::Blocked {
            severity: "WARN",
            ratio,
            spent: spent_usd,
            budget: budget_usd,
        };
    }

    let overage_tag: &'static str = if ratio > 1.0 {
        "minor_overage"
    } else {
        "within_budget"
    };

    let trail_detail = serde_json::json!({
        "reason": "auto_period_reset",
        "old_start": &start_str,
        "new_start": &now_iso,
        "spent_at_reset": spent_usd,
        "budget_usd": budget_usd,
        "ratio_at_reset": ratio,
        "period_days": period_days,
        "overage_tag": overage_tag,
    });

    // Mutate state: reset period
    if let Some(period) = state.pointer_mut("/cost/current_period") {
        period["start"] = serde_json::Value::String(now_iso.clone());
        period["spent_usd"] = serde_json::Value::from(0.0_f64);
    }

    // Append trail entry
    let trail_entry = serde_json::json!({
        "ts": &now_iso,
        "type": "budget_reset",
        "detail": trail_detail,
        "queue_item": null,
    });
    if let Some(arr) = state.get_mut("trail").and_then(|t| t.as_array_mut()) {
        arr.push(trail_entry);
    } else {
        state["trail"] = serde_json::json!([trail_entry]);
    }

    // Atomic write
    let tmp = state_path.with_extension("json.tmp");
    if let Err(e) = std::fs::write(&tmp, serde_json::to_string_pretty(&state).unwrap_or_default())
        .and_then(|_| std::fs::rename(&tmp, &state_path))
    {
        return AgentResult::Error(format!("state write failed: {e}"));
    }

    hex::audit::append(
        audit_dir,
        &agent_id,
        "budget-reset",
        &serde_json::json!({
            "reason": "auto-period-reset",
            "old_start": &start_str,
            "new_start": &now_iso,
            "spent_at_reset": spent_usd,
            "budget_usd": budget_usd,
            "ratio": ratio,
            "overage_tag": overage_tag,
            "period_days": period_days,
        }),
    );

    AgentResult::Reset {
        overage_tag,
        age_h: age_seconds / 3600.0,
        spent: spent_usd,
        budget: budget_usd,
        ratio,
    }
}

fn read_charter_period_days(project_dir: &Path) -> u64 {
    let charter_path = project_dir.join("charter.yaml");
    if !charter_path.exists() {
        return DEFAULT_PERIOD_DAYS;
    }
    let content = match std::fs::read_to_string(&charter_path) {
        Ok(c) => c,
        Err(_) => return DEFAULT_PERIOD_DAYS,
    };
    // Minimal parse mirroring the Python manual parser (no serde_yaml needed)
    let mut in_budget = false;
    for line in content.lines() {
        let stripped = line.trim();
        if stripped == "budget:" || stripped.starts_with("budget:") {
            in_budget = true;
            continue;
        }
        if in_budget {
            if stripped.starts_with("period_days:") {
                if let Some(val) = stripped.splitn(2, ':').nth(1) {
                    if let Ok(n) = val.trim().parse::<u64>() {
                        return n;
                    }
                }
            }
            // Exit budget block when we hit a new top-level key (no leading whitespace)
            if !line.is_empty()
                && !line.starts_with(' ')
                && !line.starts_with('\t')
                && line.contains(':')
            {
                break;
            }
        }
    }
    DEFAULT_PERIOD_DAYS
}

fn emit_alert(hex_alert: &Path, severity: &str, message: &str) {
    if !hex_alert.exists() {
        eprintln!("[ALERT-FALLBACK] {severity} {SOURCE}: {message}");
        return;
    }
    let _ = std::process::Command::new(hex_alert)
        .arg(severity)
        .arg(SOURCE)
        .arg(message)
        .output();
}

fn parse_iso(s: &str) -> Option<chrono::DateTime<Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .or_else(|_| chrono::DateTime::parse_from_rfc3339(&s.replace('Z', "+00:00")))
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_hex_dir() -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("projects")).unwrap();
        // CLAUDE.md required by get_hex_dir() validation — not needed here since
        // budget_reset takes hex_dir directly
        dir
    }

    fn write_state(agent_dir: &Path, start_iso: &str, spent: f64, budget: f64) {
        let state = serde_json::json!({
            "agent_id": agent_dir.file_name().unwrap().to_str().unwrap(),
            "cost": {
                "lifetime_usd": spent,
                "last_wake_usd": 0.0,
                "current_period": {
                    "start": start_iso,
                    "spent_usd": spent,
                    "budget_usd": budget,
                }
            },
            "trail": [],
        });
        std::fs::write(agent_dir.join("state.json"), serde_json::to_string_pretty(&state).unwrap()).unwrap();
    }

    fn far_past() -> String {
        // 30 days ago — always expired
        let t = Utc::now() - chrono::Duration::days(30);
        t.to_rfc3339()
    }

    fn recent() -> String {
        // 1 hour ago — never expired for default 7d period
        let t = Utc::now() - chrono::Duration::hours(1);
        t.to_rfc3339()
    }

    #[test]
    fn zero_budget_guard_skips_agent() {
        let hex = make_hex_dir();
        let agent_dir = hex.path().join("projects/agent-a");
        std::fs::create_dir_all(&agent_dir).unwrap();
        write_state(&agent_dir, &far_past(), 5.0, 0.0);

        let config = BudgetResetConfig {
            hex_dir: hex.path().to_path_buf(),
            dry_run: false,
        };
        let code = run(&config);
        // zero-budget is not an error — exit 0
        assert_eq!(code, 0);

        // state.json must NOT have been modified (spent remains 5.0)
        let state: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(agent_dir.join("state.json")).unwrap())
                .unwrap();
        let spent = state.pointer("/cost/current_period/spent_usd").and_then(|v| v.as_f64()).unwrap();
        assert_eq!(spent, 5.0, "spent_usd must be unchanged for zero-budget agent");
    }

    #[test]
    fn ratio_within_budget_resets_period() {
        let hex = make_hex_dir();
        let agent_dir = hex.path().join("projects/agent-b");
        std::fs::create_dir_all(&agent_dir).unwrap();
        write_state(&agent_dir, &far_past(), 0.5, 10.0); // ratio 0.05

        let config = BudgetResetConfig { hex_dir: hex.path().to_path_buf(), dry_run: false };
        let code = run(&config);
        assert_eq!(code, 0);

        let state: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(agent_dir.join("state.json")).unwrap()).unwrap();
        let spent = state.pointer("/cost/current_period/spent_usd").and_then(|v| v.as_f64()).unwrap();
        assert_eq!(spent, 0.0, "spent_usd must be reset to 0");

        // Trail entry must exist
        let trail = state.get("trail").and_then(|t| t.as_array()).unwrap();
        assert!(!trail.is_empty(), "trail must have a budget_reset entry");
        assert_eq!(trail[0]["type"], "budget_reset");
    }

    #[test]
    fn ratio_minor_overage_resets_with_tag() {
        let hex = make_hex_dir();
        let agent_dir = hex.path().join("projects/agent-c");
        std::fs::create_dir_all(&agent_dir).unwrap();
        write_state(&agent_dir, &far_past(), 15.0, 10.0); // ratio 1.5

        let config = BudgetResetConfig { hex_dir: hex.path().to_path_buf(), dry_run: false };
        let code = run(&config);
        assert_eq!(code, 0);

        let state: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(agent_dir.join("state.json")).unwrap()).unwrap();
        let spent = state.pointer("/cost/current_period/spent_usd").and_then(|v| v.as_f64()).unwrap();
        assert_eq!(spent, 0.0, "minor_overage must still reset");

        let trail = state.get("trail").and_then(|t| t.as_array()).unwrap();
        assert_eq!(trail[0]["detail"]["overage_tag"], "minor_overage");
    }

    #[test]
    fn ratio_3x_blocked_no_reset() {
        let hex = make_hex_dir();
        let agent_dir = hex.path().join("projects/agent-d");
        std::fs::create_dir_all(&agent_dir).unwrap();
        write_state(&agent_dir, &far_past(), 30.0, 10.0); // ratio 3.0

        let config = BudgetResetConfig { hex_dir: hex.path().to_path_buf(), dry_run: false };
        let code = run(&config);
        assert_eq!(code, 0); // blocked is not an error exit

        let state: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(agent_dir.join("state.json")).unwrap()).unwrap();
        let spent = state.pointer("/cost/current_period/spent_usd").and_then(|v| v.as_f64()).unwrap();
        assert_eq!(spent, 30.0, "3x overage must NOT reset");
    }

    #[test]
    fn ratio_6x_critical_blocked() {
        let hex = make_hex_dir();
        let agent_dir = hex.path().join("projects/agent-e");
        std::fs::create_dir_all(&agent_dir).unwrap();
        write_state(&agent_dir, &far_past(), 60.0, 10.0); // ratio 6.0

        let config = BudgetResetConfig { hex_dir: hex.path().to_path_buf(), dry_run: false };
        let code = run(&config);
        assert_eq!(code, 0);

        let state: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(agent_dir.join("state.json")).unwrap()).unwrap();
        let spent = state.pointer("/cost/current_period/spent_usd").and_then(|v| v.as_f64()).unwrap();
        assert_eq!(spent, 60.0, "6x runaway must NOT reset");
    }

    #[test]
    fn unexpired_period_skipped() {
        let hex = make_hex_dir();
        let agent_dir = hex.path().join("projects/agent-f");
        std::fs::create_dir_all(&agent_dir).unwrap();
        write_state(&agent_dir, &recent(), 1.0, 10.0); // period age 1h < 7d

        let config = BudgetResetConfig { hex_dir: hex.path().to_path_buf(), dry_run: false };
        let code = run(&config);
        assert_eq!(code, 0);

        let state: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(agent_dir.join("state.json")).unwrap()).unwrap();
        let spent = state.pointer("/cost/current_period/spent_usd").and_then(|v| v.as_f64()).unwrap();
        assert_eq!(spent, 1.0, "unexpired period must not be reset");
    }

    #[test]
    fn charter_period_days_read() {
        let dir = tempfile::tempdir().unwrap();
        let charter = "id: test-agent\nbudget:\n  period_days: 14\n  usd_per_day: 1.0\n";
        std::fs::write(dir.path().join("charter.yaml"), charter).unwrap();
        let days = read_charter_period_days(dir.path());
        assert_eq!(days, 14, "period_days must be read from charter.yaml");
    }

    #[test]
    fn charter_missing_uses_default() {
        let dir = tempfile::tempdir().unwrap();
        let days = read_charter_period_days(dir.path());
        assert_eq!(days, DEFAULT_PERIOD_DAYS);
    }

    #[test]
    fn audit_log_written_on_reset() {
        let hex = make_hex_dir();
        let agent_dir = hex.path().join("projects/agent-g");
        std::fs::create_dir_all(&agent_dir).unwrap();
        write_state(&agent_dir, &far_past(), 0.5, 10.0);

        let config = BudgetResetConfig { hex_dir: hex.path().to_path_buf(), dry_run: false };
        run(&config);

        let audit_path = hex.path().join(".hex/audit/actions.jsonl");
        assert!(audit_path.exists(), "actions.jsonl must be created");
        let content = std::fs::read_to_string(&audit_path).unwrap();
        assert!(content.contains("budget-reset"), "actions.jsonl must contain budget-reset entry");
        assert!(content.contains("agent-g"), "actions.jsonl must name the agent");
    }
}
