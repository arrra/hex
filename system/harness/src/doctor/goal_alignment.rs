use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::Path;

const THEME_ORDER: &[&str] = &[
    "Vitality",
    "Ventures",
    "Relationships",
    "Job Search",
    "Brand",
    "Wealth",
    "System",
];

fn theme_agents(theme: &str) -> &'static [&'static str] {
    match theme {
        "Vitality" => &[],
        "Ventures" => &["hex-v2-pm", "hex-v2-arch", "hex-v2-exp", "hex-ops"],
        "Relationships" => &[],
        "Job Search" => &["career", "scout", "prep-coach"],
        "Brand" => &["brand"],
        "Wealth" => &["investments"],
        "System" => &[
            "fleet-lead",
            "cos",
            "hex-autonomy",
            "sentinel",
            "system-arch",
            "dreamer",
            "synthesizer",
            "boi-optimizer",
        ],
        _ => &[],
    }
}

struct OkrKr {
    id: String,
    name: String,
    target: String,
    progress: String,
}

struct OkrTheme {
    objective: String,
    krs: Vec<OkrKr>,
}

struct AgentInfo {
    charter_exists: bool,
    role: String,
    trail_7d: usize,
    dispatches: usize,
    findings: usize,
    actions: usize,
}

pub fn run(hex_dir: &Path, dry_run: bool) -> i32 {
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let projects_dir = hex_dir.join("projects");
    let okr_file = hex_dir.join("okrs/personal/2026-Q2.md");
    let report_dir = projects_dir.join("fleet-lead");
    let report_path = report_dir.join(format!("goal-alignment-{today}.md"));
    let report_tmp = report_dir.join(format!("goal-alignment-{today}.md.tmp"));

    println!("[goal-alignment] Running as of {today}");
    println!("[goal-alignment] OKR file: {}", okr_file.display());
    println!("[goal-alignment] Projects dir: {}", projects_dir.display());

    if let Err(e) = fs::create_dir_all(&report_dir) {
        eprintln!("[goal-alignment] ERROR: cannot create report dir: {e}");
        return 1;
    }

    let okr_themes = parse_okrs(&okr_file);
    let agent_infos = build_agent_infos(&projects_dir);

    let (sections, gaps) = build_sections(&okr_themes, &agent_infos);
    let agent_table = build_agent_table(&agent_infos);
    let gaps_text = build_gaps_text(&gaps);
    let recommendations = build_recommendations(&gaps, &agent_infos);

    let sections_text = sections.join("\n\n");

    let report_content = format!(
        "# Goal Alignment Report — {today}\n\
         \n\
         **Generated:** {today}\n\
         **Horizon:** 2026-Q2 (2026-04-20 → 2026-05-04)\n\
         **Purpose:** Map agent fleet activity to Mike's OKRs. Identify coverage gaps. \
         Answer: are we getting scary good at achieving goals?\n\
         \n\
         ---\n\
         \n\
         {sections_text}\n\
         \n\
         ---\n\
         \n\
         ## Fleet Summary\n\
         \n\
         {agent_table}\n\
         \n\
         ---\n\
         \n\
         ## Coverage Gaps\n\
         \n\
         {gaps_text}\n\
         \n\
         ---\n\
         \n\
         ## Recommendations\n\
         \n\
         {recommendations}\n"
    );

    if !dry_run {
        match fs::File::create(&report_tmp) {
            Ok(mut f) => {
                if let Err(e) = f.write_all(report_content.as_bytes()) {
                    eprintln!("[goal-alignment] ERROR: cannot write report: {e}");
                    return 1;
                }
            }
            Err(e) => {
                eprintln!("[goal-alignment] ERROR: cannot create tmp file: {e}");
                return 1;
            }
        }
        if let Err(e) = fs::rename(&report_tmp, &report_path) {
            eprintln!("[goal-alignment] ERROR: cannot move report into place: {e}");
            return 1;
        }
        println!("[goal-alignment] Report written: {}", report_path.display());
    }

    // Summary to stdout (consumed by hex-events for Slack post)
    let total = agent_infos.len();
    let active = agent_infos.values().filter(|a| a.trail_7d > 0).count();
    let gap_themes: Vec<&str> = gaps.iter().filter(|g| !g.contains(':')).map(|g| g.as_str()).collect();
    let idle_agents: Vec<&str> = gaps
        .iter()
        .filter(|g| g.contains(':'))
        .filter_map(|g| g.splitn(2, ':').nth(1))
        .map(|s| s.trim_start_matches(' '))
        .collect();

    println!("Goal Alignment Report — {today}");
    println!("Agents: {active}/{total} active in last 7 days");
    if !gap_themes.is_empty() {
        println!("Uncovered OKR themes: {}", gap_themes.join(", "));
    }
    if !idle_agents.is_empty() {
        println!("Idle agents: {}", idle_agents.join(", "));
    }
    println!(
        "Full report: projects/fleet-lead/goal-alignment-{today}.md"
    );

    0
}

fn parse_okrs(path: &Path) -> HashMap<String, OkrTheme> {
    let mut themes: HashMap<String, OkrTheme> = HashMap::new();
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[goal-alignment] WARN: could not parse OKRs: {e}");
            return themes;
        }
    };

    let mut current_theme: Option<String> = None;
    let mut current_kr: Option<usize> = None; // index into theme.krs

    for line in content.lines() {
        let line = line.trim_end();

        // ## Theme N: NAME
        if let Some(rest) = line.strip_prefix("## Theme ") {
            if let Some(pos) = rest.find(": ") {
                let theme_name = rest[pos + 2..].trim().to_string();
                themes.insert(
                    theme_name.clone(),
                    OkrTheme {
                        objective: String::new(),
                        krs: Vec::new(),
                    },
                );
                current_theme = Some(theme_name);
                current_kr = None;
            }
            continue;
        }

        if let Some(ref t) = current_theme.clone() {
            // **Objective:** TEXT
            if let Some(rest) = line.strip_prefix("**Objective:** ") {
                if let Some(theme) = themes.get_mut(t) {
                    theme.objective = rest.trim().to_string();
                }
                current_kr = None;
                continue;
            }

            // ### KR N.N — NAME
            if let Some(rest) = line.strip_prefix("### KR ") {
                if let Some(pos) = rest.find(" — ") {
                    let kr_id = rest[..pos].trim().to_string();
                    let kr_name = rest[pos + 3..].trim().to_string();
                    if let Some(theme) = themes.get_mut(t) {
                        theme.krs.push(OkrKr {
                            id: kr_id,
                            name: kr_name,
                            target: String::new(),
                            progress: String::new(),
                        });
                        current_kr = Some(theme.krs.len() - 1);
                    }
                }
                continue;
            }

            if let Some(kr_idx) = current_kr {
                if let Some(rest) = line.strip_prefix("- Target:") {
                    if let Some(theme) = themes.get_mut(t) {
                        if let Some(kr) = theme.krs.get_mut(kr_idx) {
                            kr.target = rest.trim().to_string();
                        }
                    }
                    continue;
                }
                if let Some(rest) = line.strip_prefix("- Progress:") {
                    if let Some(theme) = themes.get_mut(t) {
                        if let Some(kr) = theme.krs.get_mut(kr_idx) {
                            kr.progress = rest.trim().to_string();
                        }
                    }
                    continue;
                }
            }
        }
    }

    themes
}

fn parse_charter_role(path: &Path) -> String {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return String::new(),
    };
    for line in content.lines() {
        let stripped = line.trim_end();
        // Match `role: VALUE` or `role:VALUE`
        if stripped.starts_with("role:") || stripped.starts_with("role :")  {
            let val = stripped
                .trim_start_matches("role")
                .trim_start_matches(' ')
                .trim_start_matches(':')
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .to_string();
            if !val.is_empty() {
                return val;
            }
        }
    }
    String::new()
}

fn read_trail(projects_dir: &Path, agent_id: &str) -> (usize, usize, usize, usize) {
    // Returns (trail_7d_count, dispatches, findings, actions)
    let state_path = projects_dir.join(agent_id).join("state.json");
    let content = match fs::read_to_string(&state_path) {
        Ok(c) => c,
        Err(_) => return (0, 0, 0, 0),
    };
    let state: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return (0, 0, 0, 0),
    };
    let trail = match state.get("trail").and_then(|v| v.as_array()) {
        Some(t) => t,
        None => return (0, 0, 0, 0),
    };

    let now = chrono::Utc::now();
    let seven_days_ago = now - chrono::Duration::days(7);

    let mut count = 0usize;
    let mut dispatches = 0usize;
    let mut findings = 0usize;
    let mut actions = 0usize;

    for entry in trail {
        let ts_str = match entry.get("ts").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => continue,
        };
        let ts_normalized = ts_str.replace('Z', "+00:00");
        let ts = match chrono::DateTime::parse_from_rfc3339(&ts_normalized) {
            Ok(t) => t.with_timezone(&chrono::Utc),
            Err(_) => continue,
        };
        if ts < seven_days_ago {
            continue;
        }
        count += 1;
        match entry.get("type").and_then(|v| v.as_str()).unwrap_or("") {
            "dispatch" => dispatches += 1,
            "finding" => findings += 1,
            "action" | "write" | "file_write" => actions += 1,
            _ => {}
        }
    }

    (count, dispatches, findings, actions)
}

fn build_agent_infos(projects_dir: &Path) -> HashMap<String, AgentInfo> {
    let mut all_agents: Vec<String> = Vec::new();
    for theme in THEME_ORDER {
        for agent in theme_agents(theme) {
            let s = agent.to_string();
            if !all_agents.contains(&s) {
                all_agents.push(s);
            }
        }
    }
    all_agents.sort();

    let mut infos = HashMap::new();
    for agent_id in &all_agents {
        let charter_path = projects_dir.join(agent_id).join("charter.yaml");
        let charter_exists = charter_path.exists();
        let role = if charter_exists {
            parse_charter_role(&charter_path)
        } else {
            String::new()
        };
        let (trail_7d, dispatches, findings, actions) = read_trail(projects_dir, agent_id);
        infos.insert(
            agent_id.clone(),
            AgentInfo {
                charter_exists,
                role,
                trail_7d,
                dispatches,
                findings,
                actions,
            },
        );
    }
    infos
}

fn build_sections(
    okr_themes: &HashMap<String, OkrTheme>,
    agent_infos: &HashMap<String, AgentInfo>,
) -> (Vec<String>, Vec<String>) {
    let mut sections = Vec::new();
    let mut gaps = Vec::new();

    for &theme_key in THEME_ORDER {
        let agent_ids = theme_agents(theme_key);
        let okr_theme = okr_themes.get(theme_key);
        let objective = okr_theme.map(|t| t.objective.as_str()).unwrap_or("N/A");
        let krs = okr_theme.map(|t| &t.krs[..]).unwrap_or(&[]);

        // Filter out test/placeholder KRs
        let real_krs: Vec<&OkrKr> = krs
            .iter()
            .filter(|kr| !kr.name.contains("test-e2e") && !kr.name.contains("wave4-container"))
            .collect();

        let mut section = Vec::new();
        section.push(format!("## {theme_key}"));
        section.push(format!("**Objective:** {objective}"));
        section.push(String::new());
        section.push("### OKR Progress".to_string());

        if real_krs.is_empty() {
            section.push("- No KRs defined for this theme".to_string());
        } else {
            for kr in &real_krs {
                section.push(format!("- **KR {}** {}", kr.id, kr.name));
                section.push(format!("  - Target: {}", kr.target));
                section.push(format!("  - Progress: {}", kr.progress));
            }
        }

        section.push(String::new());
        section.push("### Agent Coverage".to_string());

        if agent_ids.is_empty() {
            section.push("- **GAP: No agents assigned to this theme**".to_string());
            gaps.push(theme_key.to_string());
        } else {
            for &agent_id in agent_ids {
                let d = match agent_infos.get(agent_id) {
                    Some(d) => d,
                    None => {
                        section.push(format!("- `{agent_id}` — charter not found (gap)"));
                        continue;
                    }
                };
                if !d.charter_exists {
                    section.push(format!("- `{agent_id}` — charter not found (gap)"));
                    continue;
                }
                let active_label = if d.trail_7d > 0 {
                    "active"
                } else {
                    "IDLE (0 trail entries in 7d)"
                };
                section.push(format!(
                    "- `{agent_id}` — {} — {active_label}",
                    d.role
                ));
                section.push(format!(
                    "  - Trail entries (7d): {} | Dispatches: {} | Findings: {} | Actions: {}",
                    d.trail_7d, d.dispatches, d.findings, d.actions
                ));
                if d.trail_7d == 0 {
                    gaps.push(format!("{theme_key}:{agent_id} (idle)"));
                }
            }
        }

        section.push(String::new());

        // Coverage assessment
        let coverage = if agent_ids.is_empty() {
            "UNCOVERED — no agents assigned".to_string()
        } else {
            let active_count = agent_ids
                .iter()
                .filter(|&&aid| agent_infos.get(aid).map(|d| d.trail_7d > 0).unwrap_or(false))
                .count();
            let total = agent_ids.len();
            if active_count == total {
                format!("FULL — all {total} agent(s) active in last 7 days")
            } else if active_count > 0 {
                format!("PARTIAL — {active_count}/{total} agents active in last 7 days")
            } else {
                format!("STALE — {total} agent(s) assigned but none active in last 7 days")
            }
        };

        section.push(format!("**Coverage:** {coverage}"));
        section.push(String::new());
        sections.push(section.join("\n"));
    }

    (sections, gaps)
}

fn build_agent_table(agent_infos: &HashMap<String, AgentInfo>) -> String {
    let header = "| Agent                  | Charter | Trail 7d | Dispatches | Findings | Actions | Status |";
    let sep = "|------------------------|---------|----------|------------|----------|---------|--------|";

    let mut rows: Vec<(String, &AgentInfo)> = agent_infos
        .iter()
        .map(|(k, v)| (k.clone(), v))
        .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));

    let mut lines = vec![header.to_string(), sep.to_string()];
    for (aid, info) in &rows {
        let status = if info.trail_7d > 0 { "OK  " } else { "IDLE" };
        let charter = if info.charter_exists { "yes    " } else { "NO     " };
        lines.push(format!(
            "| {:<22} | {:<7} | {:>8} | {:>10} | {:>8} | {:>7} | {:<4} |",
            aid, charter, info.trail_7d, info.dispatches, info.findings, info.actions, status
        ));
    }
    lines.join("\n")
}

fn build_gaps_text(gaps: &[String]) -> String {
    if gaps.is_empty() {
        "### No Coverage Gaps Detected".to_string()
    } else {
        let mut lines = vec!["### Gaps Requiring Attention".to_string()];
        for g in gaps {
            lines.push(format!("- {g}"));
        }
        lines.join("\n")
    }
}

fn build_recommendations(gaps: &[String], agent_infos: &HashMap<String, AgentInfo>) -> String {
    let mut recs: Vec<String> = Vec::new();

    if gaps.iter().any(|g| g == "Vitality") {
        recs.push(
            "- **Create a Vitality agent** — No agent is tracking exercise/sleep. \
             This is a core life OKR with zero coverage. Consider a lightweight daily-nudge agent."
                .to_string(),
        );
    }
    if gaps.iter().any(|g| g == "Relationships") {
        recs.push(
            "- **Create a Relationships agent** — No agent tracks Whitney activities, \
             friend outreach, or gathering planning. Low-weight agent could track and nudge."
                .to_string(),
        );
    }

    let mut idle_agents: Vec<&str> = agent_infos
        .iter()
        .filter(|(_, info)| info.trail_7d == 0 && info.charter_exists)
        .map(|(id, _)| id.as_str())
        .collect();
    idle_agents.sort();

    if !idle_agents.is_empty() {
        recs.push(format!(
            "- **Investigate idle agents:** {} — 0 trail entries in 7 days. \
             Are they wired to hex-events? Are they halted?",
            idle_agents.join(", ")
        ));
    }

    if recs.is_empty() {
        recs.push(
            "- No critical gaps detected. System is operating with full coverage.".to_string(),
        );
    }

    recs.join("\n")
}
