/// hex doctor charter-triggers
///
/// Validates the charter → policy contract for every agent in the fleet.
/// Pre-migration mode (default): emits WARNs for drift between charter and policy.
/// Post-migration mode: WARNs become FAILs; stale policy files are FAILs.

use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq)]
enum Level {
    Pass,
    Warn,
    Fail,
}

fn print_result(level: &Level, agent_id: &str, msg: &str) {
    match level {
        Level::Pass => println!("\x1b[32mPASS\x1b[0m  [{}] {}", agent_id, msg),
        Level::Warn => println!("\x1b[33mWARN\x1b[0m  [{}] {}", agent_id, msg),
        Level::Fail => println!("\x1b[31mFAIL\x1b[0m  [{}] {}", agent_id, msg),
    }
}

/// Match an event string against a glob-style pattern where `*` matches one `.`-separated segment.
pub fn matches_pattern(event: &str, pattern: &str) -> bool {
    let event_parts: Vec<&str> = event.split('.').collect();
    let pattern_parts: Vec<&str> = pattern.split('.').collect();
    if event_parts.len() != pattern_parts.len() {
        return false;
    }
    event_parts
        .iter()
        .zip(pattern_parts.iter())
        .all(|(e, p)| *p == "*" || *e == *p)
}

pub fn event_in_allowlist(event: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|p| matches_pattern(event, p))
}

pub fn load_allowlist(home: &str) -> Result<Vec<String>, String> {
    let path = PathBuf::from(home).join(".hex-events/known-event-patterns.yaml");
    if !path.is_file() {
        return Err(format!("allowlist not found: {}", path.display()));
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read allowlist: {e}"))?;
    let doc: serde_yaml::Value =
        serde_yaml::from_str(&text).map_err(|e| format!("YAML parse error in allowlist: {e}"))?;
    let patterns = doc["patterns"]
        .as_sequence()
        .ok_or_else(|| "allowlist missing 'patterns' key".to_string())?
        .iter()
        .filter_map(|v| {
            if let Some(s) = v.as_str() {
                Some(s.to_string())
            } else {
                v.as_mapping()
                    .and_then(|m| m.get("pattern"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            }
        })
        .collect();
    Ok(patterns)
}

/// Returns (policy_events, has_rate_limit) or None if no policy file exists.
pub fn load_policy_events(home: &str, agent_id: &str) -> Option<(Vec<String>, bool)> {
    let path =
        PathBuf::from(home).join(format!(".hex-events/policies/{}-agent.yaml", agent_id));
    if !path.is_file() {
        return None;
    }
    let text = std::fs::read_to_string(&path).ok()?;
    let doc: serde_yaml::Value = serde_yaml::from_str(&text).ok()?;
    let events = doc["requires"]["events"]
        .as_sequence()
        .map(|seq| {
            seq.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let has_rate_limit = doc["rate_limit"].is_mapping();
    Some((events, has_rate_limit))
}

pub fn run(hex_dir: &Path, mode: &str) -> i32 {
    let post_migration = mode == "post-migration";
    let home = std::env::var("HOME").unwrap_or_default();

    let allowlist = match load_allowlist(&home) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("\x1b[31mFAIL\x1b[0m  [allowlist] {e}");
            return 1;
        }
    };

    let charter_glob = hex_dir.join("projects/*/charter.yaml");
    let charter_glob_str = charter_glob.to_string_lossy();

    let mut charter_paths: Vec<PathBuf> = glob::glob(&charter_glob_str)
        .unwrap_or_else(|_| glob::glob("/dev/null").unwrap())
        .filter_map(|r| r.ok())
        .collect();
    charter_paths.sort();

    if charter_paths.is_empty() {
        eprintln!(
            "\x1b[33mWARN\x1b[0m  [fleet] no charter.yaml files found in {}",
            hex_dir.join("projects").display()
        );
        return 0;
    }

    println!(
        "hex doctor charter-triggers  mode={}  agents={}\n",
        mode,
        charter_paths.len()
    );

    let mut any_fail = false;

    for charter_path in &charter_paths {
        let text = match std::fs::read_to_string(charter_path) {
            Ok(t) => t,
            Err(e) => {
                print_result(
                    &Level::Fail,
                    "?",
                    &format!("cannot read {}: {e}", charter_path.display()),
                );
                any_fail = true;
                continue;
            }
        };

        let doc: serde_yaml::Value = match serde_yaml::from_str(&text) {
            Ok(d) => d,
            Err(e) => {
                print_result(
                    &Level::Fail,
                    "?",
                    &format!("YAML parse error in {}: {e}", charter_path.display()),
                );
                any_fail = true;
                continue;
            }
        };

        let agent_id = doc["id"].as_str().unwrap_or("unknown").to_string();
        let mut findings: Vec<(Level, String)> = Vec::new();

        let triggers_node = &doc["wake"]["triggers"];
        if triggers_node.is_null() || !triggers_node.is_sequence() {
            findings.push((Level::Fail, "charter missing wake.triggers".to_string()));
        } else {
            let triggers: Vec<String> = triggers_node
                .as_sequence()
                .unwrap()
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect();

            if triggers.is_empty() {
                findings.push((Level::Fail, "charter wake.triggers is empty list".to_string()));
            } else {
                for trigger in &triggers {
                    if !event_in_allowlist(trigger, &allowlist) {
                        findings.push((
                            Level::Fail,
                            format!(
                                "trigger '{}' not in known-event-patterns allowlist",
                                trigger
                            ),
                        ));
                    }
                }

                let policy = load_policy_events(&home, &agent_id);
                match policy {
                    None => {}
                    Some((policy_events, has_rate_limit)) => {
                        if post_migration {
                            findings.push((
                                Level::Fail,
                                format!(
                                    "stale policy file exists: ~/.hex-events/policies/{}-agent.yaml",
                                    agent_id
                                ),
                            ));
                        }

                        for ct in &triggers {
                            if !policy_events.contains(ct) {
                                findings.push((
                                    if post_migration { Level::Fail } else { Level::Warn },
                                    format!(
                                        "charter declares trigger '{}' not in policy",
                                        ct
                                    ),
                                ));
                            }
                        }

                        for pe in &policy_events {
                            if !triggers.contains(pe) {
                                findings.push((
                                    if post_migration { Level::Fail } else { Level::Warn },
                                    format!(
                                        "policy declares trigger '{}' not in charter",
                                        pe
                                    ),
                                ));
                            }
                        }

                        if has_rate_limit {
                            findings.push((
                                if post_migration { Level::Fail } else { Level::Warn },
                                "policy has rate_limit but charter has no rate_limit (drift)"
                                    .to_string(),
                            ));
                        }
                    }
                }
            }
        }

        if findings.is_empty() {
            print_result(&Level::Pass, &agent_id, "all charter triggers valid");
        } else {
            let has_fail = findings.iter().any(|(l, _)| *l == Level::Fail);
            if has_fail {
                any_fail = true;
            }
            for (level, msg) in &findings {
                print_result(level, &agent_id, msg);
            }
        }
    }

    println!();
    if any_fail {
        println!("\x1b[31mResult: FAIL\x1b[0m — one or more agents have failing checks");
        1
    } else {
        println!("\x1b[32mResult: PASS\x1b[0m — all agents passed charter-triggers validation");
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pattern_match_exact() {
        assert!(matches_pattern("timer.tick.daily", "timer.tick.*"));
        assert!(matches_pattern("timer.tick.6h", "timer.tick.*"));
        assert!(!matches_pattern("timer.tick.daily.extra", "timer.tick.*"));
    }

    #[test]
    fn pattern_match_middle_wildcard() {
        assert!(matches_pattern(
            "hex.agent.cos.attention.needed",
            "hex.agent.*.attention.needed"
        ));
        assert!(!matches_pattern(
            "hex.agent.cos.other.needed",
            "hex.agent.*.attention.needed"
        ));
    }

    #[test]
    fn pattern_match_two_wildcards() {
        assert!(matches_pattern(
            "hex.agent.sentinel.vet.requested",
            "hex.agent.*.*.requested"
        ));
        assert!(!matches_pattern(
            "hex.agent.sentinel.vet.done",
            "hex.agent.*.*.requested"
        ));
    }

    #[test]
    fn event_in_allowlist_checks_all_patterns() {
        let patterns = vec![
            "timer.tick.*".to_string(),
            "hex.agent.*.attention.needed".to_string(),
            "inbox.message".to_string(),
        ];
        assert!(event_in_allowlist("timer.tick.daily", &patterns));
        assert!(event_in_allowlist("inbox.message", &patterns));
        assert!(!event_in_allowlist("unknown.event.here", &patterns));
    }

    #[test]
    fn load_allowlist_parses_real_file() {
        let home = std::env::var("HOME").unwrap_or_default();
        if !home.is_empty() {
            let path = std::path::PathBuf::from(&home).join(".hex-events/known-event-patterns.yaml");
            if path.is_file() {
                let result = load_allowlist(&home);
                assert!(result.is_ok(), "allowlist should parse: {:?}", result);
                let patterns = result.unwrap();
                assert!(patterns.len() >= 10, "expected >= 10 patterns, got {}", patterns.len());
            }
        }
    }
}
