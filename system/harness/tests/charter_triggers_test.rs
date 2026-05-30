use hex::charter_triggers;
use std::path::Path;
use tempfile::TempDir;

// ── helpers ──────────────────────────────────────────────────────────────────

fn setup_home(tmp: &TempDir, patterns: &[&str]) {
    let hex_events = tmp.path().join(".hex-events");
    std::fs::create_dir_all(hex_events.join("policies")).unwrap();
    let items: String = patterns
        .iter()
        .map(|p| format!("  - {}\n", p))
        .collect();
    std::fs::write(
        hex_events.join("known-event-patterns.yaml"),
        format!("patterns:\n{}", items),
    )
    .unwrap();
}

fn make_charter(hex_dir: &Path, agent_id: &str, wake_yaml: &str) {
    let dir = hex_dir.join("projects").join(agent_id);
    std::fs::create_dir_all(&dir).unwrap();
    let content = [
        format!("id: {}", agent_id),
        format!("name: {}", agent_id),
        "version: \"1.0\"".to_string(),
        "role: test".to_string(),
        "scope: test".to_string(),
        "parent: cos".to_string(),
        "objective: test".to_string(),
        wake_yaml.to_string(),
        "authority:".to_string(),
        "  green: []".to_string(),
        "  yellow: []".to_string(),
        "  red: []".to_string(),
        "memory:".to_string(),
        "  max_size_kb: 64".to_string(),
        "hooks:".to_string(),
        "  on_find: null".to_string(),
        "  on_decide: null".to_string(),
        "  on_act: null".to_string(),
        "  on_verify: null".to_string(),
        "escalation_channel: \"#test\"".to_string(),
        "kill_switch: /tmp/.halt".to_string(),
    ]
    .join("\n");
    std::fs::write(dir.join("charter.yaml"), content).unwrap();
}

fn make_policy(home: &TempDir, agent_id: &str, events: &[&str], rate_limit: bool) {
    let events_yaml: String = events
        .iter()
        .map(|e| format!("    - {}\n", e))
        .collect();
    let rate = if rate_limit {
        "rate_limit:\n  max_fires: 4\n  window: 60m\n"
    } else {
        ""
    };
    let content = format!(
        "name: {}-agent\nrequires:\n  events:\n{}{}",
        agent_id, events_yaml, rate
    );
    let path = home
        .path()
        .join(".hex-events/policies")
        .join(format!("{}-agent.yaml", agent_id));
    std::fs::write(path, content).unwrap();
}

// ── test cases ────────────────────────────────────────────────────────────────

#[test]
fn test_valid_charter_all_triggers_in_policy() {
    let home = TempDir::new().unwrap();
    let hex_dir = TempDir::new().unwrap();
    setup_home(&home, &["timer.tick.*", "inbox.message"]);
    make_charter(
        hex_dir.path(),
        "alpha",
        "wake:\n  triggers:\n    - timer.tick.daily\n  responsibilities: []\n",
    );
    make_policy(&home, "alpha", &["timer.tick.daily"], false);
    let exit = charter_triggers::run_with_home(
        hex_dir.path(),
        "pre-migration",
        home.path().to_str().unwrap(),
    );
    assert_eq!(exit, 0, "all triggers in policy should PASS");
}

#[test]
fn test_charter_trigger_missing_from_policy_pre_migration() {
    let home = TempDir::new().unwrap();
    let hex_dir = TempDir::new().unwrap();
    setup_home(&home, &["timer.tick.*", "inbox.message"]);
    make_charter(
        hex_dir.path(),
        "beta",
        "wake:\n  triggers:\n    - timer.tick.daily\n  responsibilities: []\n",
    );
    // policy exists but declares a different event
    make_policy(&home, "beta", &["inbox.message"], false);
    let exit = charter_triggers::run_with_home(
        hex_dir.path(),
        "pre-migration",
        home.path().to_str().unwrap(),
    );
    // pre-migration: drift is WARN, not FAIL → exit 0
    assert_eq!(exit, 0, "drift in pre-migration should be WARN (exit 0)");
}

#[test]
fn test_charter_trigger_missing_from_policy_post_migration() {
    let home = TempDir::new().unwrap();
    let hex_dir = TempDir::new().unwrap();
    setup_home(&home, &["timer.tick.*", "inbox.message"]);
    make_charter(
        hex_dir.path(),
        "gamma",
        "wake:\n  triggers:\n    - timer.tick.daily\n  responsibilities: []\n",
    );
    // policy file exists → post-migration considers stale → FAIL
    make_policy(&home, "gamma", &["timer.tick.daily"], false);
    let exit = charter_triggers::run_with_home(
        hex_dir.path(),
        "post-migration",
        home.path().to_str().unwrap(),
    );
    assert_eq!(exit, 1, "stale policy in post-migration should FAIL");
}

#[test]
fn test_invalid_trigger_pattern_not_in_allowlist() {
    let home = TempDir::new().unwrap();
    let hex_dir = TempDir::new().unwrap();
    setup_home(&home, &["timer.tick.*", "inbox.message"]);
    make_charter(
        hex_dir.path(),
        "delta",
        "wake:\n  triggers:\n    - totally.unknown.event\n  responsibilities: []\n",
    );
    let exit = charter_triggers::run_with_home(
        hex_dir.path(),
        "pre-migration",
        home.path().to_str().unwrap(),
    );
    assert_eq!(exit, 1, "trigger not in allowlist should FAIL");
}

#[test]
fn test_empty_triggers_fails() {
    let home = TempDir::new().unwrap();
    let hex_dir = TempDir::new().unwrap();
    setup_home(&home, &["timer.tick.*"]);
    make_charter(
        hex_dir.path(),
        "epsilon",
        "wake:\n  triggers: []\n  responsibilities: []\n",
    );
    let exit = charter_triggers::run_with_home(
        hex_dir.path(),
        "pre-migration",
        home.path().to_str().unwrap(),
    );
    assert_eq!(exit, 1, "empty triggers should FAIL");
}

#[test]
fn test_missing_triggers_fails() {
    let home = TempDir::new().unwrap();
    let hex_dir = TempDir::new().unwrap();
    setup_home(&home, &["timer.tick.*"]);
    // charter has no wake.triggers field at all
    make_charter(
        hex_dir.path(),
        "zeta",
        "wake:\n  responsibilities: []\n",
    );
    let exit = charter_triggers::run_with_home(
        hex_dir.path(),
        "pre-migration",
        home.path().to_str().unwrap(),
    );
    assert_eq!(exit, 1, "missing wake.triggers should FAIL");
}

#[test]
fn test_stale_policy_post_migration_fails() {
    let home = TempDir::new().unwrap();
    let hex_dir = TempDir::new().unwrap();
    setup_home(&home, &["timer.tick.*"]);
    make_charter(
        hex_dir.path(),
        "eta",
        "wake:\n  triggers:\n    - timer.tick.daily\n  responsibilities: []\n",
    );
    // any policy file in post-migration mode → stale → FAIL
    make_policy(&home, "eta", &["timer.tick.daily"], false);
    let exit = charter_triggers::run_with_home(
        hex_dir.path(),
        "post-migration",
        home.path().to_str().unwrap(),
    );
    assert_eq!(exit, 1, "stale policy file in post-migration should FAIL");
}

#[test]
fn test_no_policy_file_pre_migration_passes() {
    let home = TempDir::new().unwrap();
    let hex_dir = TempDir::new().unwrap();
    setup_home(&home, &["timer.tick.*", "inbox.message"]);
    // charter is valid, no policy file → no drift checks → PASS
    make_charter(
        hex_dir.path(),
        "theta",
        "wake:\n  triggers:\n    - timer.tick.daily\n  responsibilities: []\n",
    );
    let exit = charter_triggers::run_with_home(
        hex_dir.path(),
        "pre-migration",
        home.path().to_str().unwrap(),
    );
    assert_eq!(exit, 0, "valid charter with no policy file should PASS");
}
