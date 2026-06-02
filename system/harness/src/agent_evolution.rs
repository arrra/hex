/// Real Rust port of the agent-evolution subcommand.
///
/// Reads charter KPIs, state.json trails, and cost ledger to compute
/// performance metrics for each agent, then writes a dated evolution
/// report and updates the fleet-lead board.md.
use chrono::{DateTime, Duration, Utc};
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead};
use std::path::Path;

// ── Types ─────────────────────────────────────────────────────────────────────

#[derive(Debug)]
struct AgentMetrics {
    kpi_count: usize,
    kpi_achievement: f64,
    trail_7d: usize,
    trail_total: usize,
    productive_count: usize,
    action_types: HashMap<String, usize>,
    f2a_ratio: f64,
    diversity: usize,
    trail_quality: f64,
    is_idle: bool,
    last_trail_ts: Option<DateTime<Utc>>,
    kpis: Vec<String>,
    wake_count: u64,
}

impl AgentMetrics {
    fn composite_score(&self) -> f64 {
        // Cost-derived score component removed 2026-06-01 (Mike's "strip $
        // everywhere" directive). Score now caps at 0.8 — kpi_achievement
        // and trail_quality remain the only signal.
        self.kpi_achievement * 0.5 + self.trail_quality * 0.3
    }
}

// ── Public entry point ────────────────────────────────────────────────────────

pub fn run(dry_run: bool) -> i32 {
    let hex_dir = crate::get_hex_dir();
    let projects_dir = hex_dir.join("projects");
    let ledger_path = hex_dir.join(".hex/tokens/ledger.jsonl");
    let evolution_dir = projects_dir.join("fleet-lead/evolution");
    let board_path = projects_dir.join("fleet-lead/board.md");
    let today = Utc::now().format("%Y-%m-%d").to_string();
    let report_path = evolution_dir.join(format!("{today}.md"));

    // Collect agents
    let agent_ids = collect_agent_ids(&projects_dir);
    if agent_ids.is_empty() {
        eprintln!("ERROR: no agent charters found in {}", projects_dir.display());
        return 1;
    }
    println!("Analyzing {} agents: {}", agent_ids.len(), agent_ids.join(", "));

    // Load ledger
    let ledger = load_ledger(&ledger_path);

    // Compute metrics
    let now = Utc::now();
    let seven_days_ago = now - Duration::days(7);
    let fortyeight_h_ago = now - Duration::hours(48);

    let metrics: HashMap<String, AgentMetrics> = agent_ids
        .iter()
        .map(|id| {
            let m = compute_metrics(id, &projects_dir, &ledger, seven_days_ago, fortyeight_h_ago);
            (id.clone(), m)
        })
        .collect();

    // Rank agents
    let mut scored: Vec<(&str, f64)> = metrics
        .iter()
        .map(|(id, m)| (id.as_str(), m.composite_score()))
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // Build report
    let report = build_report(&today, &scored, &metrics);

    // Update board
    update_board(&board_path, &today, &scored, &metrics, dry_run);

    // Write report
    if dry_run {
        println!("=== DRY RUN: would write to {} ===", report_path.display());
        println!("{report}");
    } else {
        if let Err(e) = fs::create_dir_all(&evolution_dir) {
            eprintln!("ERROR: cannot create {}: {e}", evolution_dir.display());
            return 1;
        }
        let tmp = report_path.with_extension("tmp");
        if let Err(e) = fs::write(&tmp, &report) {
            eprintln!("ERROR: cannot write report: {e}");
            return 1;
        }
        if let Err(e) = fs::rename(&tmp, &report_path) {
            eprintln!("ERROR: cannot rename report: {e}");
            return 1;
        }
        println!("Evolution report written: {}", report_path.display());
    }

    println!("Done. Report: {}", report_path.display());
    0
}

// ── Agent discovery ───────────────────────────────────────────────────────────

fn collect_agent_ids(projects_dir: &Path) -> Vec<String> {
    let mut ids = Vec::new();
    let Ok(entries) = fs::read_dir(projects_dir) else { return ids };
    for entry in entries.flatten() {
        let charter = entry.path().join("charter.yaml");
        if charter.is_file() {
            if let Some(name) = entry.file_name().to_str() {
                ids.push(name.to_string());
            }
        }
    }
    ids.sort();
    ids
}

// ── Ledger loading ────────────────────────────────────────────────────────────

/// Returns map of agent_id → vec of cost entries (last 7 days)
fn load_ledger(path: &Path) -> HashMap<String, Vec<Value>> {
    let mut map: HashMap<String, Vec<Value>> = HashMap::new();
    let Ok(file) = fs::File::open(path) else { return map };
    let seven_days_ago = Utc::now() - Duration::days(7);
    for line in io::BufReader::new(file).lines().flatten() {
        let line = line.trim().to_string();
        if line.is_empty() { continue; }
        let Ok(entry): Result<Value, _> = serde_json::from_str(&line) else { continue };
        let agent = entry.get("agent").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let ts_str = entry.get("ts").and_then(|v| v.as_str()).unwrap_or("");
        if let Ok(ts) = parse_iso(ts_str) {
            if ts >= seven_days_ago {
                map.entry(agent).or_default().push(entry);
            }
        }
    }
    map
}

// ── KPI extraction ────────────────────────────────────────────────────────────

fn load_yaml_kpis(charter_path: &Path) -> Vec<String> {
    let Ok(content) = fs::read_to_string(charter_path) else { return vec![] };
    let mut kpis = Vec::new();
    let mut in_kpis = false;
    let kpi_header = Regex::new(r"^kpis\s*:").unwrap();
    let kpi_item = Regex::new(r"^\s+-\s+").unwrap();
    for line in content.lines() {
        if kpi_header.is_match(line) {
            in_kpis = true;
            continue;
        }
        if in_kpis {
            if kpi_item.is_match(line) {
                let kpi = line.trim_start().trim_start_matches('-').trim().trim_matches('"').to_string();
                kpis.push(kpi);
            } else if !line.is_empty() && !line.starts_with(' ') {
                in_kpis = false;
            }
        }
    }
    kpis
}

// ── Metric computation ────────────────────────────────────────────────────────

fn compute_metrics(
    agent_id: &str,
    projects_dir: &Path,
    ledger: &HashMap<String, Vec<Value>>,
    seven_days_ago: DateTime<Utc>,
    fortyeight_h_ago: DateTime<Utc>,
) -> AgentMetrics {
    let charter_path = projects_dir.join(agent_id).join("charter.yaml");
    let state_path = projects_dir.join(agent_id).join("state.json");

    let kpis = load_yaml_kpis(&charter_path);
    let state: Value = fs::read_to_string(&state_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Object(Default::default()));

    let empty_trail = vec![];
    let trail = state.get("trail").and_then(|v| v.as_array()).unwrap_or(&empty_trail);
    let wake_count = state.get("wake_count").and_then(|v| v.as_u64()).unwrap_or(0);

    // Filter trail to last 7 days
    let recent_trail: Vec<&Value> = trail
        .iter()
        .filter(|e| {
            e.get("ts")
                .and_then(|v| v.as_str())
                .and_then(|s| parse_iso(s).ok())
                .map(|ts| ts >= seven_days_ago)
                .unwrap_or(false)
        })
        .collect();

    // Most recent trail timestamp across all trail (not just 7d)
    let last_trail_ts: Option<DateTime<Utc>> = trail
        .iter()
        .filter_map(|e| e.get("ts")?.as_str())
        .filter_map(|s| parse_iso(s).ok())
        .max();

    let is_idle = last_trail_ts.map(|ts| ts < fortyeight_h_ago).unwrap_or(true);

    // Action type counts from recent trail
    let mut action_types: HashMap<String, usize> = HashMap::new();
    for entry in &recent_trail {
        let t = entry.get("type").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();
        *action_types.entry(t).or_insert(0) += 1;
    }

    let productive_count: usize = ["find", "act", "dispatch", "verify"]
        .iter()
        .map(|t| action_types.get(*t).copied().unwrap_or(0))
        .sum();

    let finds = action_types.get("find").copied().unwrap_or(0);
    let acts = action_types.get("act").copied().unwrap_or(0)
        + action_types.get("dispatch").copied().unwrap_or(0);
    let f2a_ratio = if finds > 0 { acts as f64 / finds as f64 } else { 0.0 };

    let diversity = action_types.values().filter(|&&v| v > 0).count();

    let total_entries = recent_trail.len();
    let productive_ratio = if total_entries > 0 { productive_count as f64 / total_entries as f64 } else { 0.0 };
    let diversity_score = (diversity as f64 / 5.0).min(1.0);
    let trail_quality = (productive_ratio * 0.6 + diversity_score * 0.4 * 1000.0).round() / 1000.0;

    let kpi_count = kpis.len();
    let kpi_target = kpi_count * 5;
    let kpi_achievement = if kpi_count == 0 {
        0.5
    } else if total_entries >= kpi_target {
        1.0
    } else {
        (total_entries as f64 / kpi_target.max(1) as f64 * 1000.0).round() / 1000.0
    };

    AgentMetrics {
        kpi_count,
        kpi_achievement,
        trail_7d: recent_trail.len(),
        trail_total: trail.len(),
        productive_count,
        action_types,
        f2a_ratio: (f2a_ratio * 1000.0).round() / 1000.0,
        diversity,
        trail_quality,
        is_idle,
        last_trail_ts,
        kpis,
        wake_count,
    }
}

// ── Report generation ─────────────────────────────────────────────────────────

fn build_report(
    today: &str,
    scored: &[(&str, f64)],
    metrics: &HashMap<String, AgentMetrics>,
) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!("# Agent Evolution Report — {today}"));
    lines.push(String::new());
    lines.push("## Fleet Performance Scorecard".into());
    lines.push(String::new());
    lines.push("| Agent | KPI Achievement | Trail (7d) | Trail Quality | Cost/Action | Idle | Score |".into());
    lines.push("|-------|:--------------:|:----------:|:-------------:|:-----------:|:----:|:-----:|".into());

    let idle_agents: Vec<&str> = scored
        .iter()
        .filter(|(id, _)| metrics[*id].is_idle)
        .map(|(id, _)| *id)
        .collect();

    for (agent_id, score) in scored.iter() {
        let m = &metrics[*agent_id];
        let idle_marker = if m.is_idle { "YES" } else { "" };
        lines.push(format!(
            "| {agent_id} | {:.0}% | {} | {:.2} | {idle_marker} | {score:.3} |",
            m.kpi_achievement * 100.0,
            m.trail_7d,
            m.trail_quality,
        ));
    }
    lines.push(String::new());

    // Top performer
    if let Some((top_id, _)) = scored.first() {
        let m = &metrics[*top_id];
        lines.push(format!("## Top Performer: {top_id}"));
        lines.push(String::new());
        lines.push(format!("- KPI achievement: {:.0}%", m.kpi_achievement * 100.0));
        lines.push(format!("- Trail entries (7d): {}", m.trail_7d));
        lines.push(format!("- Trail quality: {:.2}", m.trail_quality));
        lines.push(format!("- Action diversity: {} types used", m.diversity));
        lines.push("- What's working: high activity, broad action coverage".into());
        lines.push(String::new());
    }

    // Idle agents
    if !idle_agents.is_empty() {
        lines.push("## Idle Agents (no trail in 48h+)".into());
        lines.push(String::new());
        for agent_id in &idle_agents {
            let m = &metrics[*agent_id];
            let last = m
                .last_trail_ts
                .map(|ts| ts.to_rfc3339())
                .unwrap_or_else(|| "never".into());
            lines.push(format!("- **{agent_id}**: last trail entry={last}, wake_count={}", m.wake_count));
        }
        lines.push(String::new());
        lines.push("**Recommendation:** Investigate queue seeding. Agents wake but produce no trail entries — likely cold-start / empty queue. Run `hex-agent status <agent-id>` and seed with a bootstrap queue item.".into());
        lines.push(String::new());
    }

    // Underperformer evolution proposal
    let under_candidates: Vec<&&str> = scored
        .iter()
        .rev()
        .take(3)
        .map(|(id, _)| id)
        .filter(|id| !idle_agents.contains(*id) && metrics[**id].trail_7d > 0)
        .collect();

    let under_agent = under_candidates.last().copied().or_else(|| scored.last().map(|(id, _)| id));

    if let Some(under_id) = under_agent {
        let top_id = scored.first().map(|(id, _)| *id);
        if Some(*under_id) != top_id {
            let m = &metrics[*under_id];
            lines.push(format!("## Evolution Proposal: {under_id}"));
            lines.push(String::new());
            lines.push("### What's Not Working (data-backed)".into());
            lines.push(String::new());

            let (proposed_change, proposed_field) = if m.trail_7d == 0 {
                lines.push(format!("- Zero trail entries in past 7 days (wake_count={})", m.wake_count));
                lines.push("- Agent wakes but produces no output — empty queue or cold-start failure".into());
                (
                    "Seed queue with initial responsibility task".to_string(),
                    "queue.active: add bootstrap item for primary responsibility".to_string(),
                )
            } else if m.f2a_ratio < 0.3 {
                lines.push(format!("- Low KPI achievement: {:.0}% (trail_7d={}, kpi_count={})",
                    m.kpi_achievement * 100.0, m.trail_7d, m.kpi_count));
                lines.push(format!("- Low finding-to-action ratio: {:.2} — findings not converting to actions", m.f2a_ratio));
                if m.diversity < 3 {
                    lines.push(format!("- Low action diversity: only {} action types used — narrow task execution", m.diversity));
                }
                (
                    "Add standing order: every find entry must be followed by an act or dispatch".to_string(),
                    "wake.responsibilities: add explicit action requirement to each responsibility description".to_string(),
                )
            } else {
                lines.push(format!("- Low KPI achievement: {:.0}% (trail_7d={}, kpi_count={})",
                    m.kpi_achievement * 100.0, m.trail_7d, m.kpi_count));
                if m.diversity < 3 {
                    lines.push(format!("- Low action diversity: only {} action types used — narrow task execution", m.diversity));
                }
                (
                    "Increase wake interval from 21600s to 14400s for primary responsibility".to_string(),
                    "wake.responsibilities[0].interval: 21600 → 14400".to_string(),
                )
            };

            lines.push(String::new());
            lines.push("### Hypothesis for Improvement".into());
            lines.push(String::new());
            if m.trail_7d == 0 {
                lines.push("Bootstrapping the agent queue with a first task will unblock all downstream work.".into());
            } else if m.f2a_ratio < 0.3 {
                lines.push("Adding a mandatory 'act on every finding' standing order will increase conversion rate.".into());
            } else {
                lines.push("Increasing wake frequency will surface more work items and improve KPI coverage.".into());
            }
            lines.push(String::new());
            lines.push("### Proposed Charter Change".into());
            lines.push(String::new());
            lines.push("```yaml".into());
            lines.push(format!("# {proposed_field}"));
            lines.push(format!("# Change: {proposed_change}"));
            lines.push("```".into());
            lines.push(String::new());
            lines.push("### Experiment".into());
            lines.push(String::new());
            let today_str = today;
            lines.push("```yaml".into());
            lines.push("evolution:".into());
            lines.push(format!("  baseline_date: {today_str}"));
            lines.push("  experiments:".into());
            lines.push("    - id: exp-001".into());
            lines.push(format!("      hypothesis: \"{proposed_change} will improve KPI achievement\""));
            lines.push(format!("      change: \"{proposed_field}\""));
            lines.push(format!("      started: {today_str}"));
            lines.push("      metric: kpi_achievement".into());
            lines.push(format!("      baseline: \"{:.2}\"", m.kpi_achievement));
            lines.push("      result: null".into());
            lines.push("      verdict: null".into());
            lines.push("```".into());
            lines.push(String::new());
            lines.push("**Duration:** 7 days. Measure before/after KPI achievement and trail quality.".into());
            lines.push(String::new());
        }
    }

    lines.join("\n")
}

// ── Board update ──────────────────────────────────────────────────────────────

fn update_board(
    board_path: &Path,
    today: &str,
    scored: &[(&str, f64)],
    metrics: &HashMap<String, AgentMetrics>,
    dry_run: bool,
) {
    let mut section = format!("\n## Evolution Scores — {today}\n\n");
    section.push_str("| Agent | Score | KPI% | Trail(7d) | Quality | Idle |\n");
    section.push_str("|-------|:-----:|:----:|:---------:|:-------:|:----:|\n");
    for (agent_id, score) in scored {
        let m = &metrics[*agent_id];
        let idle_marker = if m.is_idle { "YES" } else { "" };
        section.push_str(&format!(
            "| {agent_id} | {score:.3} | {:.0}% | {} | {:.2} | {idle_marker} |\n",
            m.kpi_achievement * 100.0,
            m.trail_7d,
            m.trail_quality,
        ));
    }
    section.push_str(&format!("\n_Updated by hex agent evolution on {today}_\n"));

    if dry_run {
        println!("=== DRY RUN: board update ===\n{section}");
        return;
    }

    let existing = fs::read_to_string(board_path).unwrap_or_default();
    // Remove old evolution section
    let pattern = Regex::new(r"\n## Evolution Scores.*?(?=\n## |\z)").unwrap();
    let cleaned = pattern.replace_all(&existing, "").to_string();
    let updated = cleaned.trim_end().to_string() + "\n" + &section;

    let tmp = board_path.with_extension("tmp");
    if let Err(e) = fs::write(&tmp, &updated) {
        eprintln!("BOARD_ERROR: {e}");
        return;
    }
    if let Err(e) = fs::rename(&tmp, board_path) {
        eprintln!("BOARD_ERROR rename: {e}");
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn parse_iso(s: &str) -> Result<DateTime<Utc>, chrono::ParseError> {
    // Try RFC3339 first, then "Z" replacement
    DateTime::parse_from_rfc3339(s)
        .or_else(|_| DateTime::parse_from_rfc3339(&s.replace('Z', "+00:00")))
        .map(|dt| dt.with_timezone(&Utc))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn write_file(dir: &Path, rel: &str, content: &str) {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
    }

    #[test]
    fn test_collect_agent_ids() {
        let tmp = TempDir::new().unwrap();
        write_file(tmp.path(), "alpha/charter.yaml", "id: alpha\n");
        write_file(tmp.path(), "beta/charter.yaml", "id: beta\n");
        let ids = collect_agent_ids(tmp.path());
        assert_eq!(ids, vec!["alpha", "beta"]);
    }

    #[test]
    fn test_load_yaml_kpis() {
        let tmp = TempDir::new().unwrap();
        let charter = r#"id: test-agent
kpis:
  - "Ship 3 features per week"
  - "Maintain <5% bug rate"
"#;
        write_file(tmp.path(), "charter.yaml", charter);
        let kpis = load_yaml_kpis(&tmp.path().join("charter.yaml"));
        assert_eq!(kpis.len(), 2);
        assert!(kpis[0].contains("Ship"));
    }

    #[test]
    fn test_compute_metrics_idle_when_no_trail() {
        let tmp = TempDir::new().unwrap();
        write_file(
            tmp.path(),
            "agent-a/charter.yaml",
            "id: agent-a\nkpis:\n  - \"do stuff\"\n",
        );
        write_file(tmp.path(), "agent-a/state.json", "{\"trail\": [], \"wake_count\": 3}");
        let ledger = HashMap::new();
        let now = Utc::now();
        let m = compute_metrics(
            "agent-a",
            tmp.path(),
            &ledger,
            now - Duration::days(7),
            now - Duration::hours(48),
        );
        assert!(m.is_idle);
        assert_eq!(m.trail_7d, 0);
        assert_eq!(m.wake_count, 3);
    }

    #[test]
    fn test_compute_metrics_productive() {
        let tmp = TempDir::new().unwrap();
        write_file(
            tmp.path(),
            "agent-b/charter.yaml",
            "id: agent-b\nkpis:\n  - \"kpi1\"\n  - \"kpi2\"\n",
        );
        let now = Utc::now();
        // 5 recent entries of mixed types
        let entries: Vec<String> = vec![
            format!("{{\"ts\": \"{}\", \"type\": \"find\"}}", (now - Duration::hours(1)).to_rfc3339()),
            format!("{{\"ts\": \"{}\", \"type\": \"act\"}}", (now - Duration::hours(2)).to_rfc3339()),
            format!("{{\"ts\": \"{}\", \"type\": \"dispatch\"}}", (now - Duration::hours(3)).to_rfc3339()),
            format!("{{\"ts\": \"{}\", \"type\": \"verify\"}}", (now - Duration::hours(4)).to_rfc3339()),
            format!("{{\"ts\": \"{}\", \"type\": \"note\"}}", (now - Duration::hours(5)).to_rfc3339()),
        ];
        let trail_json = format!("[{}]", entries.join(","));
        write_file(tmp.path(), "agent-b/state.json", &format!("{{\"trail\": {trail_json}, \"wake_count\": 10}}"));
        let ledger = HashMap::new();
        let m = compute_metrics(
            "agent-b",
            tmp.path(),
            &ledger,
            now - Duration::days(7),
            now - Duration::hours(48),
        );
        assert!(!m.is_idle, "agent with recent trail should not be idle");
        assert_eq!(m.trail_7d, 5);
        assert_eq!(m.productive_count, 4); // find+act+dispatch+verify
        assert!(m.f2a_ratio > 0.0, "f2a_ratio should be positive");
    }

    #[test]
    fn test_build_report_contains_scorecard() {
        let today = "2026-01-01";
        let mut metrics = HashMap::new();
        metrics.insert("agent-x".to_string(), AgentMetrics {
            kpi_count: 2,
            kpi_achievement: 0.8,
            trail_7d: 10,
            trail_total: 20,
            productive_count: 8,
            action_types: HashMap::new(),
            f2a_ratio: 0.5,
            diversity: 3,
            trail_quality: 0.7,
            is_idle: false,
            last_trail_ts: Some(Utc::now()),
            kpis: vec!["kpi1".into()],
            wake_count: 5,
        });
        let scored = vec![("agent-x", 0.75)];
        let report = build_report(today, &scored, &metrics);
        assert!(report.contains("Fleet Performance Scorecard"));
        assert!(report.contains("agent-x"));
        assert!(report.contains("Top Performer"));
    }
}
