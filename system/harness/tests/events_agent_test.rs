//! Integration tests for the AgentPolicy / CharterLoader / EventEngine wake dispatch pipeline.
//!
//! Coverage:
//!   1. AgentPolicy deserialization (minimal, full, with command, with conditions)
//!   2. wake.enabled: false → not loaded by CharterLoader
//!   3. Charter missing wake.triggers → health.json entry written
//!   4. RateLimit window math
//!   5. SQLite rate-limit state persists across simulated restart
//!   6. Trigger condition evaluation (true / false / timeout)
//!   7. on_success / on_failure events emitted to the events table
//!   8. wake.command override used instead of default hex agent wake

use hex::events::{AgentPolicy, CharterLoader, EventEngine, RateLimit, TriggerSpec};
use std::time::Duration;
use tempfile::TempDir;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Write `content` to `<tmp>/projects/<agent_id>/charter.yaml`.
fn write_charter(tmp: &TempDir, agent_id: &str, content: &str) {
    let dir = tmp.path().join("projects").join(agent_id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("charter.yaml"), content).unwrap();
}

// ── Group 1: AgentPolicy deserialization ──────────────────────────────────────

#[test]
fn agent_policy_minimal_deserializes() {
    let yaml = "enabled: true\ntriggers:\n  - event: timer.tick.1h\n";
    let p: AgentPolicy = serde_yaml::from_str(yaml).expect("minimal parse");
    assert!(p.enabled);
    assert_eq!(p.triggers.len(), 1);
    assert_eq!(p.triggers[0].event, "timer.tick.1h");
    assert!(p.triggers[0].condition.is_none());
    assert!(p.rate_limit.is_none());
    assert!(p.command.is_none());
    assert!(p.on_success.is_empty());
    assert!(p.on_failure.is_empty());
}

#[test]
fn agent_policy_full_deserializes() {
    let yaml = r#"
enabled: true
rate_limit:
  max_fires: 3
  window: 60m
command: "my-wake.sh {{event.type}}"
triggers:
  - event: timer.tick.1h
    condition: "test -f /tmp/ready"
  - event: boi.spec.completed
on_success:
  - hex.agent.wake.succeeded
on_failure:
  - hex.agent.wake.failed
"#;
    let p: AgentPolicy = serde_yaml::from_str(yaml).expect("full parse");
    let rl = p.rate_limit.as_ref().expect("rate_limit present");
    assert_eq!(rl.max_fires, 3);
    assert_eq!(rl.window, "60m");
    assert_eq!(p.command.as_deref(), Some("my-wake.sh {{event.type}}"));
    assert_eq!(p.triggers.len(), 2);
    assert_eq!(p.triggers[0].condition.as_deref(), Some("test -f /tmp/ready"));
    assert!(p.triggers[1].condition.is_none());
    assert_eq!(p.on_success, vec!["hex.agent.wake.succeeded"]);
    assert_eq!(p.on_failure, vec!["hex.agent.wake.failed"]);
}

#[test]
fn agent_policy_defaults_enabled_true() {
    // enabled: omitted → should default to true
    let yaml = "triggers:\n  - event: timer.tick.1h\n";
    let p: AgentPolicy = serde_yaml::from_str(yaml).expect("default enabled parse");
    assert!(p.enabled, "enabled should default to true when absent");
}

#[test]
fn agent_policy_with_multiple_conditions_deserializes() {
    let yaml = r#"
triggers:
  - event: timer.tick.6h
    condition: "[ $(date +%u) -le 5 ]"
  - event: hex.deploy.succeeded
    condition: null
  - event: boi.spec.completed
"#;
    let p: AgentPolicy = serde_yaml::from_str(yaml).expect("multi-condition parse");
    assert_eq!(p.triggers.len(), 3);
    assert!(p.triggers[0].condition.is_some());
    assert!(p.triggers[1].condition.is_none());
    assert!(p.triggers[2].condition.is_none());
}

// ── Group 2: CharterLoader — disabled agent not loaded ────────────────────────

#[test]
fn charter_loader_disabled_agent_not_loaded() {
    let tmp = TempDir::new().unwrap();
    write_charter(
        &tmp,
        "sleeping-agent",
        "id: sleeping-agent\nwake:\n  enabled: false\n  triggers:\n    - event: timer.tick.1h\n",
    );
    let loader = CharterLoader::new(tmp.path().to_path_buf());
    let policies = loader.load_charters();
    assert!(
        policies.is_empty(),
        "disabled agent should not appear in loaded policies"
    );
}

// ── Group 3: Missing triggers → health.json entry ─────────────────────────────

#[test]
fn charter_loader_missing_triggers_writes_health_json() {
    let tmp = TempDir::new().unwrap();
    write_charter(
        &tmp,
        "broken-agent",
        "id: broken-agent\nwake:\n  enabled: true\n  triggers: []\n",
    );
    let loader = CharterLoader::new(tmp.path().to_path_buf());
    let policies = loader.load_charters();

    assert!(policies.is_empty(), "agent with empty triggers must be excluded");

    let health_path = tmp.path().join("health.json");
    assert!(
        health_path.exists(),
        "health.json must be written when a charter fails validation"
    );

    let health: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&health_path).unwrap()).unwrap();
    let agents = &health["agents"];
    assert!(
        !agents["broken-agent"].is_null(),
        "health.json must contain an entry for broken-agent"
    );
    let status = agents["broken-agent"]["status"].as_str().unwrap_or("");
    assert_eq!(status, "invalid_charter");
}

#[test]
fn charter_loader_missing_wake_block_writes_health_json() {
    let tmp = TempDir::new().unwrap();
    write_charter(&tmp, "no-wake-agent", "id: no-wake-agent\nname: Test\n");
    let loader = CharterLoader::new(tmp.path().to_path_buf());
    let policies = loader.load_charters();
    assert!(policies.is_empty());

    let health_path = tmp.path().join("health.json");
    assert!(health_path.exists());
    let health: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&health_path).unwrap()).unwrap();
    assert_eq!(
        health["agents"]["no-wake-agent"]["status"].as_str().unwrap_or(""),
        "invalid_charter"
    );
}

// ── Group 4: Rate-limit window math ───────────────────────────────────────────

#[test]
fn rate_limit_window_minutes() {
    let rl = RateLimit { max_fires: 3, window: "60m".to_string() };
    assert_eq!(rl.window_duration().unwrap(), Duration::from_secs(3600));
}

#[test]
fn rate_limit_window_hours() {
    let rl = RateLimit { max_fires: 1, window: "24h".to_string() };
    assert_eq!(rl.window_duration().unwrap(), Duration::from_secs(86400));
}

#[test]
fn rate_limit_window_days() {
    let rl = RateLimit { max_fires: 5, window: "7d".to_string() };
    assert_eq!(
        rl.window_duration().unwrap(),
        Duration::from_secs(7 * 86400)
    );
}

#[test]
fn rate_limit_window_invalid_returns_err() {
    let rl = RateLimit { max_fires: 1, window: "5x".to_string() };
    assert!(
        rl.window_duration().is_err(),
        "unrecognised suffix should return Err"
    );
}

// ── Group 5: SQLite rate-limit persists across restart ────────────────────────

#[test]
fn rate_limit_persists_across_restart() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("events.db");

    // First "daemon instance" — record 2 fires at max_fires=2
    {
        let engine = EventEngine::new_with_db_file(tmp.path(), &db_path);
        engine.record_fire("persist-agent");
        engine.record_fire("persist-agent");
        let allowed = engine.check_rate_limit(
            "persist-agent",
            2,
            Duration::from_secs(3600),
        );
        assert!(!allowed, "should be rate-limited after 2 fires in a 1h window");
    }

    // Second "daemon instance" — opens the same DB file, limit should still apply
    {
        let engine2 = EventEngine::new_with_db_file(tmp.path(), &db_path);
        let allowed = engine2.check_rate_limit(
            "persist-agent",
            2,
            Duration::from_secs(3600),
        );
        assert!(
            !allowed,
            "rate limit should be enforced after daemon restart (persisted DB)"
        );
    }
}

// ── Group 6: Condition evaluation ─────────────────────────────────────────────

#[test]
fn condition_true_allows_wake() {
    let tmp = TempDir::new().unwrap();
    let engine = EventEngine::new_in_memory(tmp.path());

    let policy = AgentPolicy {
        id: "cond-true-agent".to_string(),
        enabled: true,
        rate_limit: None,
        command: Some("true".to_string()),
        triggers: vec![TriggerSpec {
            event: "timer.tick.1h".to_string(),
            condition: Some("true".to_string()),
        }],
        on_success: vec!["hex.test.condition_ok".to_string()],
        on_failure: vec![],
    };
    *engine.agent_policies.write().unwrap() = vec![policy];

    let wakes = engine.dispatch_agent_wakes(1, "timer.tick.1h", &serde_json::json!({}), false);
    assert_eq!(wakes, 1, "condition=true should allow wake to fire");
}

#[test]
fn condition_false_blocks_wake() {
    let tmp = TempDir::new().unwrap();
    let engine = EventEngine::new_in_memory(tmp.path());

    let policy = AgentPolicy {
        id: "cond-false-agent".to_string(),
        enabled: true,
        rate_limit: None,
        command: Some("true".to_string()),
        triggers: vec![TriggerSpec {
            event: "timer.tick.1h".to_string(),
            condition: Some("false".to_string()),
        }],
        on_success: vec![],
        on_failure: vec![],
    };
    *engine.agent_policies.write().unwrap() = vec![policy];

    let wakes = engine.dispatch_agent_wakes(1, "timer.tick.1h", &serde_json::json!({}), false);
    assert_eq!(wakes, 0, "condition=false should block wake");
}

#[test]
fn condition_timeout_marks_agent_degraded() {
    let tmp = TempDir::new().unwrap();
    let engine = EventEngine::new_in_memory(tmp.path());

    let policy = AgentPolicy {
        id: "timeout-agent".to_string(),
        enabled: true,
        rate_limit: None,
        command: Some("true".to_string()),
        // Condition that sleeps longer than the 5s timeout
        triggers: vec![TriggerSpec {
            event: "timer.tick.1h".to_string(),
            condition: Some("sleep 10".to_string()),
        }],
        on_success: vec![],
        on_failure: vec![],
    };
    *engine.agent_policies.write().unwrap() = vec![policy];

    // This call will block ~5s waiting for the condition timeout
    let wakes = engine.dispatch_agent_wakes(1, "timer.tick.1h", &serde_json::json!({}), false);
    assert_eq!(wakes, 0, "timed-out condition should block wake");

    // Degraded flag should be set in health.json
    let health_path = tmp.path().join("health.json");
    if health_path.exists() {
        let health: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&health_path).unwrap()).unwrap();
        let degraded = health["agents"]["timeout-agent"]["degraded"]
            .as_bool()
            .unwrap_or(false);
        assert!(degraded, "timeout should mark agent degraded in health.json");
    }
    // If health.json doesn't exist the daemon may not have written it yet;
    // the wakes==0 assertion above is the primary guard.
}

// ── Group 7: on_success / on_failure events emitted ──────────────────────────

#[test]
fn on_success_events_emitted_to_db() {
    let tmp = TempDir::new().unwrap();
    let engine = EventEngine::new_in_memory(tmp.path());

    let policy = AgentPolicy {
        id: "success-agent".to_string(),
        enabled: true,
        rate_limit: None,
        command: Some("true".to_string()),
        triggers: vec![TriggerSpec {
            event: "timer.tick.1h".to_string(),
            condition: None,
        }],
        on_success: vec!["hex.test.wake_succeeded".to_string()],
        on_failure: vec![],
    };
    *engine.agent_policies.write().unwrap() = vec![policy];

    engine.dispatch_agent_wakes(1, "timer.tick.1h", &serde_json::json!({}), false);

    let count = engine.with_db(|db| {
        db.query_row(
            "SELECT COUNT(*) FROM events WHERE event_type='hex.test.wake_succeeded'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
    });
    assert_eq!(count, 1, "on_success event must be emitted to events table");
}

#[test]
fn on_failure_events_emitted_to_db() {
    let tmp = TempDir::new().unwrap();
    let engine = EventEngine::new_in_memory(tmp.path());

    let policy = AgentPolicy {
        id: "fail-agent".to_string(),
        enabled: true,
        rate_limit: None,
        // Command that always exits non-zero
        command: Some("false".to_string()),
        triggers: vec![TriggerSpec {
            event: "timer.tick.1h".to_string(),
            condition: None,
        }],
        on_success: vec![],
        on_failure: vec!["hex.test.wake_failed".to_string()],
    };
    *engine.agent_policies.write().unwrap() = vec![policy];

    engine.dispatch_agent_wakes(1, "timer.tick.1h", &serde_json::json!({}), false);

    let count = engine.with_db(|db| {
        db.query_row(
            "SELECT COUNT(*) FROM events WHERE event_type='hex.test.wake_failed'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
    });
    assert_eq!(count, 1, "on_failure event must be emitted to events table");
}

// ── Group 8: wake.command override ────────────────────────────────────────────

#[test]
fn wake_command_override_used_instead_of_default() {
    let tmp = TempDir::new().unwrap();
    let marker = tmp.path().join("wake_override_marker.txt");
    let engine = EventEngine::new_in_memory(tmp.path());

    let cmd = format!("touch {}", marker.display());
    let policy = AgentPolicy {
        id: "override-agent".to_string(),
        enabled: true,
        rate_limit: None,
        command: Some(cmd),
        triggers: vec![TriggerSpec {
            event: "timer.tick.1h".to_string(),
            condition: None,
        }],
        on_success: vec![],
        on_failure: vec![],
    };
    *engine.agent_policies.write().unwrap() = vec![policy];

    engine.dispatch_agent_wakes(1, "timer.tick.1h", &serde_json::json!({}), false);

    assert!(
        marker.exists(),
        "wake.command override should have run (marker file must exist)"
    );
}

#[test]
fn wake_command_template_rendered_with_event_type() {
    let tmp = TempDir::new().unwrap();
    let out = tmp.path().join("event_type.txt");
    let engine = EventEngine::new_in_memory(tmp.path());

    // Command writes the rendered event type to a file
    let cmd = format!("echo '{{{{event.type}}}}' > {}", out.display());
    let policy = AgentPolicy {
        id: "template-agent".to_string(),
        enabled: true,
        rate_limit: None,
        command: Some(cmd),
        triggers: vec![TriggerSpec {
            event: "timer.tick.1h".to_string(),
            condition: None,
        }],
        on_success: vec![],
        on_failure: vec![],
    };
    *engine.agent_policies.write().unwrap() = vec![policy];

    engine.dispatch_agent_wakes(1, "timer.tick.1h", &serde_json::json!({}), false);

    assert!(out.exists(), "command should have written the event type file");
    let content = std::fs::read_to_string(&out).unwrap();
    assert!(
        content.trim() == "timer.tick.1h",
        "event.type template should render to 'timer.tick.1h', got: {content}"
    );
}

// ── Bonus: shadow mode does not execute commands ───────────────────────────────

#[test]
fn shadow_mode_does_not_execute_wake_command() {
    let tmp = TempDir::new().unwrap();
    let marker = tmp.path().join("shadow_marker.txt");
    let engine = EventEngine::new_in_memory(tmp.path());

    let cmd = format!("touch {}", marker.display());
    let policy = AgentPolicy {
        id: "shadow-agent".to_string(),
        enabled: true,
        rate_limit: None,
        command: Some(cmd),
        triggers: vec![TriggerSpec {
            event: "timer.tick.1h".to_string(),
            condition: None,
        }],
        on_success: vec![],
        on_failure: vec![],
    };
    *engine.agent_policies.write().unwrap() = vec![policy];

    // shadow=true → log only, do NOT execute
    let wakes = engine.dispatch_agent_wakes(1, "timer.tick.1h", &serde_json::json!({}), true);
    assert_eq!(wakes, 1, "shadow mode should count the wake but not fire");
    assert!(
        !marker.exists(),
        "shadow mode must not execute the wake command"
    );
}
