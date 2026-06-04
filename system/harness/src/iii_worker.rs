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
    /// 7-field cron expression (sec min hour day month weekday year).
    pub cron: String,
    #[serde(default)]
    pub description: String,
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
        if let Err(e) = iii.register_trigger(iii_sdk::RegisterTriggerInput {
            trigger_type: "cron".to_string(),
            function_id: job.id.clone(),
            config: serde_json::json!({ "expression": job.cron }),
            metadata: None,
        }) {
            eprintln!("iii worker: failed to register cron for {}: {e}", job.id);
            return 1;
        }
        println!("  registered {} (cron {})", job.id, job.cron);
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
        std::env::set_var("HEX_DIR", "/tmp/hx");
        let out = expand_args(&["bash".into(), "${HEX_DIR}/s.sh".into()]);
        assert_eq!(out, vec!["bash", "/tmp/hx/s.sh"]);
    }

    /// Red test for Tf7tqfhpp: run_command must record a telemetry event
    /// (with worker name, event id, and duration) on a successful job outcome.
    #[tokio::test]
    async fn run_command_records_telemetry_on_success() {
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("HEX_DIR", tmp.path());

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
}
