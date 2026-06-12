//! `hex doctor stale-deps` — scan todo.md + recent landings for dependency-blocked
//! items and track how long each has been stale (ported from stale_deps.py).

use chrono::{NaiveDate, Utc};
use regex::Regex;
use std::fs;
use std::path::{Path, PathBuf};

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
