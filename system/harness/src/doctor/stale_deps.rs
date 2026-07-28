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
                if stripped.is_empty() {
                    continue;
                }
                if dependency_markers.is_match(stripped) {
                    let clean = Regex::new(r"^[-*\[\]x ]+")
                        .unwrap()
                        .replace(stripped, "")
                        .trim()
                        .to_string();
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
            .ok()
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("md"))
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    // `get(..4)` returns None when byte 4 is not a char
                    // boundary, so a multi-byte filename can never panic here.
                    .map(|n| {
                        n.len() == 13
                            && n.get(..4)
                                .map(|y| y.parse::<u32>().is_ok())
                                .unwrap_or(false)
                    })
                    .unwrap_or(false)
            })
            .collect();
        landing_files.sort_by(|a, b| b.cmp(a));
        for lf in landing_files.iter().take(3) {
            if let Ok(text) = fs::read_to_string(lf) {
                let src = format!("landings/{}", lf.file_name().unwrap().to_string_lossy());
                for line in text.lines() {
                    let stripped = line.trim();
                    if stripped.is_empty() {
                        continue;
                    }
                    if dependency_markers.is_match(stripped) {
                        let clean = Regex::new(r"^[-*\[\]x ]+")
                            .unwrap()
                            .replace(stripped, "")
                            .trim()
                            .to_string();
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
        fs::read_to_string(&tracker_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| serde_json::json!({"items": {}, "last_scan": null}))
    } else {
        serde_json::json!({"items": {}, "last_scan": null})
    };

    let today = Utc::now().format("%Y-%m-%d").to_string();
    let items_map = state["items"].as_object_mut().expect("items is object");

    let mut current_keys: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (text, source) in &all_items {
        // Truncate to the first 80 chars for the dedup key. `chars().take(80)` is
        // char-boundary safe (a byte slice `text[..80]` panics when a multi-byte
        // char straddles offset 80); for ASCII text this is identical to 80 bytes.
        let truncated: String = text.chars().take(80).collect();
        let key: String = truncated
            .to_lowercase()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        current_keys.insert(key.clone());
        items_map.entry(key.clone()).or_insert_with(|| {
            serde_json::json!({
                "text": text,
                "source": source,
                "first_seen": today,
                "last_seen": today,
            })
        });
        if let Some(entry) = items_map.get_mut(&key) {
            entry["last_seen"] = serde_json::Value::String(today.clone());
            entry["source"] = serde_json::Value::String(source.clone());
        }
    }

    let resolved: Vec<String> = items_map
        .keys()
        .filter(|k| !current_keys.contains(*k))
        .cloned()
        .collect();
    for k in resolved {
        items_map.remove(&k);
    }

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

    if let Some(parent) = tracker_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let tmp = tracker_path.with_extension("json.tmp");
    if let Ok(s) = serde_json::to_string_pretty(&state) {
        let _ = fs::write(&tmp, s);
        let _ = fs::rename(&tmp, &tracker_path);
    }

    if json_output {
        let total = state["items"].as_object().map(|m| m.len()).unwrap_or(0);
        println!(
            "{}",
            serde_json::json!({"stale": stale, "total_tracked": total})
        );
    } else if stale.is_empty() {
        let total = state["items"].as_object().map(|m| m.len()).unwrap_or(0);
        println!(
            "No stale dependencies (threshold: {threshold_days} days, tracking {total} items)."
        );
    } else {
        println!(
            "STALE DEPENDENCIES ({} items past {threshold_days}-day threshold):",
            stale.len()
        );
        println!();
        for item in &stale {
            println!(
                "  [{}d] {}",
                item["days_stale"],
                item["text"].as_str().unwrap_or("")
            );
            println!(
                "       Source: {} | First seen: {}",
                item["source"].as_str().unwrap_or(""),
                item["first_seen"].as_str().unwrap_or("")
            );
            println!();
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Regression test for a char-boundary panic: `n[..4]` at line ~50 was
    /// guarded only by `n.len() == 13` (a byte length check), not a
    /// char-boundary check. A 13-byte landings filename with a multi-byte
    /// character straddling byte offset 4 panics the slice.
    #[test]
    fn stale_deps_does_not_panic_on_multibyte_landings_filename() {
        let tmp = std::env::temp_dir().join(format!(
            "hex_stale_deps_test_{}_{}",
            std::process::id(),
            "multibyte"
        ));
        let landings = tmp.join("landings");
        fs::create_dir_all(&landings).expect("create temp landings dir");

        // "126é07-22.md": 'é' is a 2-byte UTF-8 char occupying bytes 3-4,
        // so byte offset 4 falls mid-character (not a char boundary).
        // Total byte length is 13, matching the `n.len() == 13` guard.
        let fname = "126\u{e9}07-22.md";
        assert_eq!(fname.len(), 13, "fixture filename must be exactly 13 bytes");

        fs::write(
            landings.join(fname),
            "- waiting on someone to respond about deployment\n",
        )
        .expect("write fixture landings file");

        // Should scan without panicking and return a clean exit code.
        let exit = stale_deps(&tmp, 9999, true);
        assert_eq!(exit, 0);

        let _ = fs::remove_dir_all(&tmp);
    }

    /// Regression test for a char-boundary panic: `text[..text.len().min(80)]`
    /// (the dedup-key truncation) is guarded only by a byte-length `.min()`,
    /// not a char-boundary check. A todo.md item whose cleaned text is longer
    /// than 80 bytes and has a multi-byte character straddling byte offset 80
    /// panics the slice.
    #[test]
    fn stale_deps_does_not_panic_on_multibyte_item_text() {
        let tmp = std::env::temp_dir().join(format!(
            "hex_stale_deps_test_{}_{}",
            std::process::id(),
            "multibyte_text"
        ));
        fs::create_dir_all(&tmp).expect("create temp hex dir");

        // "waiting on " (11 bytes) + 68 'a' bytes = 79 bytes, then 'é'
        // (2-byte UTF-8, occupying byte offsets 79-80) so byte offset 80
        // falls mid-character (not a char boundary), then trailing text so
        // the cleaned item is well over 80 bytes total.
        let item_text = format!("waiting on {}\u{e9}{}", "a".repeat(68), "b".repeat(20));
        assert!(
            item_text.len() > 80,
            "fixture item text must exceed 80 bytes"
        );
        assert!(
            !item_text.is_char_boundary(80),
            "fixture must place a multi-byte char straddling byte offset 80"
        );

        fs::write(tmp.join("todo.md"), format!("- {item_text}\n")).expect("write fixture todo.md");

        // Should scan without panicking and return a clean exit code.
        let exit = stale_deps(&tmp, 9999, true);
        assert_eq!(exit, 0);

        let _ = fs::remove_dir_all(&tmp);
    }
}
