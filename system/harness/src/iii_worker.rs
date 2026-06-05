//! Generic iii worker host: `hex iii worker run <config.yaml>`.
//!
//! Reads a declarative job config (id + command + cron) and hosts it on the iii
//! engine: each job becomes a function that execs a shell command, bound to a
//! cron trigger. The hex binary IS the worker host — no node, no per-worker
//! binary, nothing to build or sync beyond the (text) config. New workers are
//! just new config files (declarative module model).
//!
//! Standing Order S6: a nonzero command exit is logged LOUD (stderr) and
//! returned as a function error so the engine records the failure.

use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct WorkerConfig {
    pub worker_name: String,
    pub jobs: Vec<Job>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Job {
    /// Namespaced function id, e.g. `hex::memory::index`.
    pub id: String,
    /// argv to exec. `${HEX_DIR}` is expanded from the environment.
    pub command: Vec<String>,
    /// Legacy bare cron expression (7-field: sec min hour day month weekday year).
    /// Sugar for `trigger: { cron: { expression: ... } }`. Mutually exclusive
    /// with the structured `trigger` block — exactly one of {cron, trigger}
    /// must be present per job (S6: validation is loud, not silent).
    #[serde(default)]
    pub cron: Option<String>,
    /// Structured trigger spec (cron|state|queue). Mutually exclusive with the
    /// bare `cron` field above.
    #[serde(default)]
    pub trigger: Option<TriggerSpec>,
    #[serde(default)]
    pub description: String,
}

/// Declarative trigger spec. The YAML key (`cron`/`state`/`queue`) selects the
/// trigger family; the inner block is the typed config. Modeled as a struct of
/// optional fields (not a Rust enum) so that serde_yaml's externally-tagged
/// map form works uniformly with serde_json. Exactly one of the inner fields
/// must be Some — enforced in `build_trigger`.
///
/// Maps 1:1 to `iii_sdk::builtin_triggers::IIITrigger::{Cron,State,Queue}`.
#[derive(Debug, Deserialize, Clone, Default)]
pub struct TriggerSpec {
    #[serde(default)]
    pub cron: Option<CronSpec>,
    #[serde(default)]
    pub state: Option<StateSpec>,
    #[serde(default)]
    pub queue: Option<QueueSpec>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct CronSpec {
    pub expression: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct StateSpec {
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub key: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct QueueSpec {
    /// Queue topic / name to subscribe to.
    pub queue: String,
}

/// Pure builder — maps a Job to a `RegisterTriggerInput` for the engine.
///
/// Exactly one of {`job.cron`, `job.trigger`} must be present. Zero or both is
/// a LOUD validation error (S6); the caller (serve) exits nonzero on Err.
pub fn build_trigger(
    job: &Job,
) -> Result<iii_sdk::protocol::RegisterTriggerInput, String> {
    use iii_sdk::builtin_triggers::{
        CronTriggerConfig, IIITrigger, QueueTriggerConfig, StateTriggerConfig,
    };
    if job.cron.is_some() && job.trigger.is_some() {
        return Err(format!(
            "job {}: must specify exactly one of `cron` or `trigger`, not both",
            job.id
        ));
    }
    if let Some(expr) = &job.cron {
        let t = IIITrigger::Cron(CronTriggerConfig::new(expr.clone()));
        return Ok(t.for_function(job.id.clone()));
    }
    let trig = job.trigger.as_ref().ok_or_else(|| {
        format!(
            "job {}: must specify exactly one of `cron` or `trigger` (both missing)",
            job.id
        )
    })?;
    let set_count = [trig.cron.is_some(), trig.state.is_some(), trig.queue.is_some()]
        .into_iter()
        .filter(|b| *b)
        .count();
    if set_count == 0 {
        return Err(format!(
            "job {}: `trigger` block must specify exactly one of cron/state/queue (none set)",
            job.id
        ));
    }
    if set_count > 1 {
        return Err(format!(
            "job {}: `trigger` block must specify exactly one of cron/state/queue (multiple set)",
            job.id
        ));
    }
    let iii_trigger = if let Some(c) = &trig.cron {
        IIITrigger::Cron(CronTriggerConfig::new(c.expression.clone()))
    } else if let Some(s) = &trig.state {
        let mut cfg = StateTriggerConfig::new();
        if let Some(scope) = &s.scope {
            cfg = cfg.scope(scope.clone());
        }
        if let Some(key) = &s.key {
            cfg = cfg.key(key.clone());
        }
        IIITrigger::State(cfg)
    } else if let Some(q) = &trig.queue {
        IIITrigger::Queue(QueueTriggerConfig::new(q.queue.clone()))
    } else {
        unreachable!("set_count==1 guarantees one branch");
    };
    Ok(iii_trigger.for_function(job.id.clone()))
}

/// Entry point for `hex iii worker run <config>`. Returns a process exit code.
pub fn run(config_path: &Path) -> i32 {
    let text = match std::fs::read_to_string(config_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("iii worker: cannot read config {}: {e}", config_path.display());
            return 1;
        }
    };
    let cfg: WorkerConfig = match serde_yaml::from_str(&text) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("iii worker: invalid config {}: {e}", config_path.display());
            return 1;
        }
    };
    let rt = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("iii worker: failed to start tokio runtime: {e}");
            return 1;
        }
    };
    rt.block_on(serve(cfg))
}

async fn serve(cfg: WorkerConfig) -> i32 {
    let url = std::env::var("III_URL").unwrap_or_else(|_| "ws://127.0.0.1:49134".to_string());
    let iii = iii_sdk::register_worker(&url, iii_sdk::InitOptions::default());

    for job in &cfg.jobs {
        let argv = expand_args(&job.command);
        let id_for_handler = job.id.clone();
        let worker_name_for_handler = cfg.worker_name.clone();
        iii.register_function(
            job.id.clone(),
            iii_sdk::RegisterFunction::new_async(move |_input: serde_json::Value| {
                let argv = argv.clone();
                let id = id_for_handler.clone();
                let worker_name = worker_name_for_handler.clone();
                async move { run_command(&worker_name, &id, &argv).await }
            })
            .description(job.description.clone()),
        );
        let trigger_input = match build_trigger(job) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("iii worker: invalid trigger for {}: {e}", job.id);
                return 1;
            }
        };
        let trigger_type = trigger_input.trigger_type.clone();
        if let Err(e) = iii.register_trigger(trigger_input) {
            eprintln!(
                "iii worker: failed to register {trigger_type} trigger for {}: {e}",
                job.id
            );
            return 1;
        }
        println!("  registered {} ({})", job.id, trigger_type);
    }

    println!("iii worker '{}' running ({} job(s))", cfg.worker_name, cfg.jobs.len());

    // Keep the process alive; the SDK serves cron fires on its background task
    // and auto-reconnects if the engine restarts.
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
    }
}

/// Expand `${HEX_DIR}` in each argv token.
fn expand_args(args: &[String]) -> Vec<String> {
    let hex_dir = std::env::var("HEX_DIR").unwrap_or_default();
    args.iter().map(|a| a.replace("${HEX_DIR}", &hex_dir)).collect()
}

/// Exec a command; Ok on exit 0, loud Err otherwise (S6).
///
/// Every outcome is also recorded to the local telemetry store via
/// `crate::telemetry::record_loud` — this is the chokepoint that auto-traces
/// every iii job (no per-worker opt-in needed). The telemetry write is
/// loud-but-not-fatal: a write failure logs to stderr but never fails the
/// observed job.
async fn run_command(
    worker_name: &str,
    id: &str,
    argv: &[String],
) -> Result<serde_json::Value, iii_sdk::IIIError> {
    if argv.is_empty() {
        return Err(iii_sdk::IIIError::Handler(format!("{id}: empty command")));
    }
    // Run from $HEX_DIR so cwd-relative resources resolve (e.g. fastembed's
    // .fastembed_cache for `hex memory index`). Defense-in-depth: the plist also
    // sets WorkingDirectory, but don't rely on it.
    let hex_dir = std::env::var("HEX_DIR").unwrap_or_else(|_| ".".to_string());
    let started = std::time::Instant::now();
    let out = tokio::process::Command::new(&argv[0])
        .args(&argv[1..])
        .current_dir(&hex_dir)
        .output()
        .await;
    let duration_ms = started.elapsed().as_millis() as i64;
    match out {
        Ok(o) if o.status.success() => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            let mut tail: Vec<char> = stdout.chars().rev().take(300).collect();
            tail.reverse();
            let tail: String = tail.into_iter().collect();
            println!("iii worker: {id} OK — {}", tail.replace('\n', " "));
            crate::telemetry::record_loud(&crate::telemetry::TelemetryEvent {
                source: worker_name.to_string(),
                event: id.to_string(),
                status: "ok".to_string(),
                duration_ms: Some(duration_ms),
                exit_code: Some(o.status.code().unwrap_or(0) as i64),
                detail: Some(tail),
            });
            Ok(serde_json::json!({ "ok": true, "id": id }))
        }
        Ok(o) => {
            let code = o.status.code().unwrap_or(-1);
            let stderr = String::from_utf8_lossy(&o.stderr);
            eprintln!("iii worker: {id} FAILED (exit {code}): {}", stderr);
            let mut tail: Vec<char> = stderr.chars().rev().take(300).collect();
            tail.reverse();
            let tail: String = tail.into_iter().collect();
            crate::telemetry::record_loud(&crate::telemetry::TelemetryEvent {
                source: worker_name.to_string(),
                event: id.to_string(),
                status: "error".to_string(),
                duration_ms: Some(duration_ms),
                exit_code: Some(code as i64),
                detail: Some(tail),
            });
            Err(iii_sdk::IIIError::Handler(format!("{id} exited {code}")))
        }
        Err(e) => {
            eprintln!("iii worker: {id} spawn failed: {e}");
            crate::telemetry::record_loud(&crate::telemetry::TelemetryEvent {
                source: worker_name.to_string(),
                event: id.to_string(),
                status: "spawn_error".to_string(),
                duration_ms: Some(duration_ms),
                exit_code: None,
                detail: Some(format!("spawn failed: {e}")),
            });
            Err(iii_sdk::IIIError::Handler(format!("{id} spawn failed: {e}")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_worker_config() {
        let yaml = r#"
worker_name: test-worker
jobs:
  - id: a::b
    command: [echo, hi]
    cron: "0 */15 * * * * *"
    description: test
  - id: c::d
    command: [bash, "${HEX_DIR}/x.sh"]
    cron: "0 0 4 * * * *"
"#;
        let cfg: WorkerConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.worker_name, "test-worker");
        assert_eq!(cfg.jobs.len(), 2);
        assert_eq!(cfg.jobs[0].id, "a::b");
        assert_eq!(cfg.jobs[1].command, vec!["bash", "${HEX_DIR}/x.sh"]);
    }

    #[test]
    fn expand_args_substitutes_hex_dir() {
        let _guard = crate::telemetry::test_support::lock_env();
        std::env::set_var("HEX_DIR", "/tmp/hx");
        let out = expand_args(&["bash".into(), "${HEX_DIR}/s.sh".into()]);
        assert_eq!(out, vec!["bash", "/tmp/hx/s.sh"]);
    }

    /// Red test for Tf7tqfhpp: run_command must record a telemetry event
    /// (with worker name, event id, and duration) on a successful job outcome.
    #[tokio::test]
    async fn run_command_records_telemetry_on_success() {
        let (_tmp, _guard) = crate::telemetry::test_support::isolate();

        let argv: Vec<String> = vec!["true".into()];
        let res = run_command(
            "telemetry-test-worker",
            "hex::telemetry::redtest::ok",
            &argv,
        )
        .await;
        assert!(res.is_ok(), "expected successful run, got {res:?}");

        let rows = crate::telemetry::recent(50)
            .expect("telemetry::recent must succeed after a recorded run");
        let found = rows.iter().find(|r| {
            r.event == "hex::telemetry::redtest::ok"
                && r.source == "telemetry-test-worker"
                && r.status == "ok"
        });
        assert!(
            found.is_some(),
            "expected telemetry row for the successful run, got rows: {rows:?}"
        );
        let row = found.unwrap();
        assert!(
            row.duration_ms.is_some(),
            "telemetry row must include duration_ms"
        );
    }

    // -- Red tests for Tfry002tv: event triggers (cron back-compat + state + queue) --

    /// Legacy bare top-level `cron: "..."` must still parse and must build a
    /// RegisterTriggerInput with trigger_type == "cron".
    #[test]
    fn legacy_bare_cron_parses_and_builds_cron_trigger() {
        let yaml = r#"
worker_name: legacy
jobs:
  - id: legacy::job
    command: [echo, hi]
    cron: "0 */15 * * * * *"
"#;
        let cfg: WorkerConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.jobs.len(), 1);
        let job = &cfg.jobs[0];
        let input = build_trigger(job).expect("legacy cron must build a trigger");
        assert_eq!(input.trigger_type, "cron");
        assert_eq!(input.function_id, "legacy::job");
        // Cron expression should be carried through in the config payload.
        let s = serde_json::to_string(&input.config).unwrap();
        assert!(
            s.contains("0 */15 * * * * *"),
            "cron expression missing from config payload: {s}"
        );
    }

    /// A `trigger: { state: { scope: ... } }` job must parse and build a
    /// RegisterTriggerInput with trigger_type == "state".
    #[test]
    fn state_trigger_parses_and_builds_state_trigger() {
        let yaml = r#"
worker_name: reactive
jobs:
  - id: react::on_boi
    command: [echo, hi]
    trigger:
      state:
        scope: boi
"#;
        let cfg: WorkerConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.jobs.len(), 1);
        let job = &cfg.jobs[0];
        let input = build_trigger(job).expect("state trigger must build");
        assert_eq!(input.trigger_type, "state");
        assert_eq!(input.function_id, "react::on_boi");
        let s = serde_json::to_string(&input.config).unwrap();
        assert!(s.contains("boi"), "scope 'boi' missing from config: {s}");
    }

    /// A `trigger: { queue: { queue: ... } }` job must parse and build a queue trigger.
    #[test]
    fn queue_trigger_parses_and_builds_queue_trigger() {
        let yaml = r#"
worker_name: q
jobs:
  - id: q::handle
    command: [echo, hi]
    trigger:
      queue:
        queue: emails
"#;
        let cfg: WorkerConfig = serde_yaml::from_str(yaml).unwrap();
        let input = build_trigger(&cfg.jobs[0]).expect("queue trigger must build");
        assert_eq!(input.trigger_type, "queue");
    }

    /// A job with neither cron nor trigger must be a loud validation error (S6).
    #[test]
    fn neither_cron_nor_trigger_is_error() {
        let yaml = r#"
worker_name: bad
jobs:
  - id: bad::none
    command: [echo, hi]
"#;
        let cfg: WorkerConfig = serde_yaml::from_str(yaml).unwrap();
        let res = build_trigger(&cfg.jobs[0]);
        assert!(
            res.is_err(),
            "job with neither cron nor trigger must Err, got {res:?}"
        );
    }

    /// A job specifying BOTH a bare `cron` and a `trigger` block must Err.
    #[test]
    fn both_cron_and_trigger_is_error() {
        let yaml = r#"
worker_name: bad
jobs:
  - id: bad::both
    command: [echo, hi]
    cron: "0 0 * * * * *"
    trigger:
      state:
        scope: boi
"#;
        let cfg: WorkerConfig = serde_yaml::from_str(yaml).unwrap();
        let res = build_trigger(&cfg.jobs[0]);
        assert!(
            res.is_err(),
            "job with both cron and trigger must Err, got {res:?}"
        );
    }
}
