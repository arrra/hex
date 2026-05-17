/// Port of .hex/scripts/doctor-checks/codex.sh
///
/// Codex CLI + config health checks. Verifies:
///   1. codex binary on PATH
///   2. codex --version exits 0
///   3. OPENAI_API_KEY is set (env or ~/.hex-test.env)
///   4. AGENTS.md exists at $HEX_DIR/AGENTS.md
///   5. AGENTS.md contains all required sections
use chrono::{Duration, NaiveDate, Utc};
use regex::Regex;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const AGENTS_MD_REQUIRED_SECTIONS: &[&str] = &[
    "Standing Orders",
    "Session Lifecycle",
    "BOI",
    "Memory",
];

fn pass(msg: &str) {
    println!("PASS  {}", msg);
}

fn warn(msg: &str) {
    println!("WARN  {}", msg);
}

fn error(msg: &str) {
    eprintln!("ERROR {}", msg);
}

fn info(msg: &str) {
    println!("INFO  {}", msg);
}

fn codex_on_path() -> bool {
    Command::new("which")
        .arg("codex")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn check_codex_1(failed: &mut bool) {
    if let Ok(output) = Command::new("which").arg("codex").output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            pass(&format!("codex.cli-on-path: found at {}", path));
            return;
        }
    }
    error("codex.cli-on-path: codex CLI not found — install with: npm install -g @openai/codex");
    *failed = true;
}

fn check_codex_2(failed: &mut bool) {
    if !codex_on_path() {
        info("codex.version-ok: skipped (codex not on PATH)");
        return;
    }
    match Command::new("codex").arg("--version").output() {
        Ok(o) if o.status.success() => {
            let ver = String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            pass(&format!("codex.version-ok: {}", ver));
        }
        _ => {
            error("codex.version-ok: 'codex --version' exited non-zero — fix: reinstall codex CLI");
            *failed = true;
        }
    }
}

fn check_codex_3(failed: &mut bool) {
    if std::env::var("OPENAI_API_KEY").map(|v| !v.is_empty()).unwrap_or(false) {
        pass("codex.api-key: OPENAI_API_KEY found via environment variable");
        return;
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let hex_test_env = PathBuf::from(&home).join(".hex-test.env");
    if hex_test_env.is_file() {
        if let Ok(text) = std::fs::read_to_string(&hex_test_env) {
            if text.lines().any(|l| l.starts_with("OPENAI_API_KEY=")) {
                pass("codex.api-key: OPENAI_API_KEY found via ~/.hex-test.env");
                return;
            }
        }
    }
    warn("codex.api-key: OPENAI_API_KEY not set — Codex will fail at runtime");
    info("  Fix: export OPENAI_API_KEY=sk-... in ~/.hex/scripts/env.sh or add to ~/.hex-test.env");
    *failed = true;
}

fn check_codex_4(hex_dir: &std::path::Path, failed: &mut bool) {
    let agents_md = hex_dir.join("AGENTS.md");
    if agents_md.is_file() {
        let size = std::fs::metadata(&agents_md).map(|m| m.len()).unwrap_or(0);
        pass(&format!("codex.agents-md-exists: AGENTS.md found ({} bytes)", size));
    } else {
        warn(&format!(
            "codex.agents-md-exists: AGENTS.md missing at {}",
            agents_md.display()
        ));
        info("  Fix: create AGENTS.md — Codex reads this as its primary instruction file");
        *failed = true;
    }
}

fn check_codex_5(hex_dir: &std::path::Path, failed: &mut bool) {
    let agents_md = hex_dir.join("AGENTS.md");
    if !agents_md.is_file() {
        info("codex.agents-md-complete: skipped (AGENTS.md not present)");
        return;
    }
    let text = match std::fs::read_to_string(&agents_md) {
        Ok(t) => t.to_lowercase(),
        Err(e) => {
            error(&format!("codex.agents-md-complete: cannot read AGENTS.md: {e}"));
            *failed = true;
            return;
        }
    };
    let missing: Vec<&str> = AGENTS_MD_REQUIRED_SECTIONS
        .iter()
        .filter(|&&section| !text.contains(&section.to_lowercase()))
        .copied()
        .collect();
    if missing.is_empty() {
        pass("codex.agents-md-complete: all required sections present");
    } else {
        warn(&format!(
            "codex.agents-md-complete: AGENTS.md missing sections: {}",
            missing.join(", ")
        ));
        info(&format!(
            "  Required sections: {}",
            AGENTS_MD_REQUIRED_SECTIONS.join(", ")
        ));
        *failed = true;
    }
}

pub fn check_codex(hex_dir: &std::path::Path) {
    let mut failed = false;
    check_codex_1(&mut failed);
    check_codex_2(&mut failed);
    check_codex_3(&mut failed);
    check_codex_4(hex_dir, &mut failed);
    check_codex_5(hex_dir, &mut failed);
    if failed {
        std::process::exit(1);
    }
}

/// Port of .hex/scripts/quality-check.py
/// Gaming detector for BOI initiative loop specs. Delegates to the Python script.
pub fn quality_check(hex_dir: &std::path::Path, spec: Option<&str>, sweep: bool, kr: Option<&str>) -> i32 {
    // Try legacy script (after quarantine) first
    let legacy = hex_dir.join("system/scripts/quality-check.py.legacy.py");
    let primary = hex_dir.join("system/scripts/quality-check.py");
    let script = if legacy.is_file() { legacy } else { primary };

    if !script.is_file() {
        eprintln!("hex doctor quality-check: script not found at {}", script.display());
        return 1;
    }

    let mut cmd = std::process::Command::new("python3");
    cmd.arg(&script);
    if let Some(s) = spec {
        cmd.arg("--spec").arg(s);
    }
    if sweep {
        cmd.arg("--sweep");
    }
    if let Some(k) = kr {
        cmd.arg("--kr").arg(k);
    }
    cmd.env("HEX_DIR", hex_dir);
    cmd.stdout(std::process::Stdio::inherit());
    cmd.stderr(std::process::Stdio::inherit());

    let status = cmd.status().unwrap_or_else(|e| {
        eprintln!("hex doctor quality-check: failed to exec script: {e}");
        std::process::exit(1);
    });
    status.code().unwrap_or(1)
}

/// Port of system/scripts/stale_deps.py
/// Scans todo.md and recent landings for dependency-blocked items older than threshold.
pub fn stale_deps(hex_dir: &Path, threshold_days: u32, json_output: bool) -> i32 {
    let dependency_markers = Regex::new(
        r"(?i)(waiting on|blocked by|pending response|need(?:s|ing)? response|awaiting|waiting for|depends on|need(?:s)? from)"
    ).expect("regex compiles");

    let mut all_items: Vec<(String, String)> = vec![]; // (text, source)

    let todo_path = hex_dir.join("todo.md");
    if todo_path.is_file() {
        if let Ok(text) = fs::read_to_string(&todo_path) {
            for line in text.lines() {
                let stripped = line.trim();
                if stripped.is_empty() { continue; }
                if dependency_markers.is_match(stripped) {
                    let clean = Regex::new(r"^[-*\[\]x ]+").unwrap().replace(stripped, "").trim().to_string();
                    if clean.len() > 10 {
                        all_items.push((clean, "todo.md".to_string()));
                    }
                }
            }
        }
    }

    let landings_dir = hex_dir.join("landings");
    if landings_dir.is_dir() {
        let mut landing_files: Vec<PathBuf> = fs::read_dir(&landings_dir)
            .ok().into_iter().flatten()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("md"))
            .filter(|p| p.file_name().and_then(|n| n.to_str())
                .map(|n| n.len() == 13 && n[..4].parse::<u32>().is_ok())
                .unwrap_or(false))
            .collect();
        landing_files.sort_by(|a, b| b.cmp(a));
        for lf in landing_files.iter().take(3) {
            if let Ok(text) = fs::read_to_string(lf) {
                let src = format!("landings/{}", lf.file_name().unwrap().to_string_lossy());
                for line in text.lines() {
                    let stripped = line.trim();
                    if stripped.is_empty() { continue; }
                    if dependency_markers.is_match(stripped) {
                        let clean = Regex::new(r"^[-*\[\]x ]+").unwrap().replace(stripped, "").trim().to_string();
                        if clean.len() > 10 {
                            all_items.push((clean, src.clone()));
                        }
                    }
                }
            }
        }
    }

    let tracker_path = hex_dir.join(".claude/dependency-tracker.json");
    let mut state: serde_json::Value = if tracker_path.is_file() {
        fs::read_to_string(&tracker_path).ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| serde_json::json!({"items": {}, "last_scan": null}))
    } else {
        serde_json::json!({"items": {}, "last_scan": null})
    };

    let today = Utc::now().format("%Y-%m-%d").to_string();
    let items_map = state["items"].as_object_mut().expect("items is object");

    let mut current_keys: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (text, source) in &all_items {
        let key: String = text[..text.len().min(80)].to_lowercase()
            .split_whitespace().collect::<Vec<_>>().join(" ");
        current_keys.insert(key.clone());
        items_map.entry(key.clone()).or_insert_with(|| serde_json::json!({
            "text": text,
            "source": source,
            "first_seen": today,
            "last_seen": today,
        }));
        if let Some(entry) = items_map.get_mut(&key) {
            entry["last_seen"] = serde_json::Value::String(today.clone());
            entry["source"] = serde_json::Value::String(source.clone());
        }
    }

    let resolved: Vec<String> = items_map.keys()
        .filter(|k| !current_keys.contains(*k))
        .cloned()
        .collect();
    for k in resolved { items_map.remove(&k); }

    state["last_scan"] = serde_json::Value::String(today.clone());

    let mut stale: Vec<serde_json::Value> = vec![];
    for (_key, info) in state["items"].as_object().unwrap() {
        let first_seen_str = info["first_seen"].as_str().unwrap_or(&today);
        if let Ok(first_seen) = NaiveDate::parse_from_str(first_seen_str, "%Y-%m-%d") {
            let age_days = (Utc::now().date_naive() - first_seen).num_days();
            if age_days >= threshold_days as i64 {
                stale.push(serde_json::json!({
                    "text": info["text"],
                    "source": info["source"],
                    "first_seen": first_seen_str,
                    "days_stale": age_days,
                }));
            }
        }
    }
    stale.sort_by(|a, b| b["days_stale"].as_i64().cmp(&a["days_stale"].as_i64()));

    if let Some(parent) = tracker_path.parent() { let _ = fs::create_dir_all(parent); }
    let tmp = tracker_path.with_extension("json.tmp");
    if let Ok(s) = serde_json::to_string_pretty(&state) {
        let _ = fs::write(&tmp, s);
        let _ = fs::rename(&tmp, &tracker_path);
    }

    if json_output {
        let total = state["items"].as_object().map(|m| m.len()).unwrap_or(0);
        println!("{}", serde_json::json!({"stale": stale, "total_tracked": total}));
    } else if stale.is_empty() {
        let total = state["items"].as_object().map(|m| m.len()).unwrap_or(0);
        println!("No stale dependencies (threshold: {threshold_days} days, tracking {total} items).");
    } else {
        println!("STALE DEPENDENCIES ({} items past {threshold_days}-day threshold):", stale.len());
        println!();
        for item in &stale {
            println!("  [{}d] {}", item["days_stale"], item["text"].as_str().unwrap_or(""));
            println!("       Source: {} | First seen: {}",
                item["source"].as_str().unwrap_or(""),
                item["first_seen"].as_str().unwrap_or(""));
            println!();
        }
    }
    0
}

/// Port of system/scripts/detect-failure-pattern.py
/// Detects three-strike failure patterns in the BOI SQLite DB.
pub fn detect_failure_pattern(window_seconds: u64, spec_id: Option<&str>) -> i32 {
    let db_path = dirs_home().join(".boi/boi-rust.db");
    if !db_path.is_file() {
        eprintln!("[detect-failure-pattern] DB not found: {}", db_path.display());
        return 0;
    }

    let conn = match rusqlite::Connection::open(&db_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[detect-failure-pattern] cannot open DB: {e}");
            return 1;
        }
    };

    let cutoff = (Utc::now() - Duration::seconds(window_seconds as i64))
        .to_rfc3339();

    let mut stmt = match conn.prepare(
        "SELECT id, title, completed_at, error FROM specs WHERE status = 'failed' AND completed_at >= ? ORDER BY completed_at DESC"
    ) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[detect-failure-pattern] query error: {e}");
            return 1;
        }
    };

    let failures: Vec<(String, String, String)> = stmt.query_map([&cutoff], |row| {
        Ok((
            row.get::<_, String>(0).unwrap_or_default(),
            row.get::<_, String>(1).unwrap_or_default(),
            row.get::<_, String>(3).unwrap_or_default(),
        ))
    }).ok().into_iter().flatten()
      .filter_map(|r| r.ok())
      .collect();

    const THRESHOLD: usize = 3;
    let window_hours = window_seconds / 3600;

    let mut by_kind: HashMap<String, Vec<String>> = HashMap::new();
    let mut by_title: HashMap<String, Vec<String>> = HashMap::new();
    for (id, title, error_raw) in &failures {
        let kind = serde_json::from_str::<serde_json::Value>(error_raw)
            .ok()
            .and_then(|v| v.get("kind").and_then(|k| k.as_str()).map(|s| s.to_string()))
            .unwrap_or_else(|| if error_raw.is_empty() { "Unknown".to_string() } else { error_raw.clone() });
        by_kind.entry(kind).or_default().push(id.clone());
        if !title.is_empty() {
            let norm = title.replace('\u{2014}', "-").replace('\u{2013}', "-");
            by_title.entry(norm).or_default().push(id.clone());
        }
    }

    for (kind, ids) in &by_kind {
        if ids.len() >= THRESHOLD {
            if let Some(sid) = spec_id { if !ids.contains(&sid.to_string()) { continue; } }
            let p = serde_json::json!({
                "pattern_type": "failure_kind",
                "key": kind,
                "description": format!("Failure kind '{kind}' fired {}x in last {window_hours}h", ids.len()),
                "occurrences": ids,
                "count": ids.len(),
                "recommended_owner": "boi-optimizer",
            });
            println!("{}", p);
        }
    }
    for (title, ids) in &by_title {
        if ids.len() >= THRESHOLD {
            if let Some(sid) = spec_id { if !ids.contains(&sid.to_string()) { continue; } }
            let p = serde_json::json!({
                "pattern_type": "spec_title",
                "key": title,
                "description": format!("Spec title '{title}' failed {}x in last {window_hours}h", ids.len()),
                "occurrences": ids,
                "count": ids.len(),
                "recommended_owner": "boi-optimizer",
            });
            println!("{}", p);
        }
    }
    0
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_sections_nonempty() {
        assert!(!AGENTS_MD_REQUIRED_SECTIONS.is_empty());
        assert!(AGENTS_MD_REQUIRED_SECTIONS.contains(&"Standing Orders"));
        assert!(AGENTS_MD_REQUIRED_SECTIONS.contains(&"BOI"));
    }

    #[test]
    fn check_codex_5_passes_when_all_sections_present() {
        let dir = std::env::temp_dir().join("codex_test_agents_ok");
        std::fs::create_dir_all(&dir).unwrap();
        let content = "# Standing Orders\n# Session Lifecycle\n# BOI\n# Memory\n";
        std::fs::write(dir.join("AGENTS.md"), content).unwrap();
        let mut failed = false;
        check_codex_5(&dir, &mut failed);
        assert!(!failed, "all sections present → should not fail");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn check_codex_5_fails_when_section_missing() {
        let dir = std::env::temp_dir().join("codex_test_agents_missing");
        std::fs::create_dir_all(&dir).unwrap();
        let content = "# Standing Orders\n# BOI\n";
        std::fs::write(dir.join("AGENTS.md"), content).unwrap();
        let mut failed = false;
        check_codex_5(&dir, &mut failed);
        assert!(failed, "missing sections → should fail");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn check_codex_4_fails_when_agents_md_missing() {
        let dir = std::env::temp_dir().join("codex_test_no_agents");
        std::fs::create_dir_all(&dir).unwrap();
        let _ = std::fs::remove_file(dir.join("AGENTS.md"));
        let mut failed = false;
        check_codex_4(&dir, &mut failed);
        assert!(failed, "missing AGENTS.md → should fail");
        std::fs::remove_dir_all(&dir).ok();
    }
}
