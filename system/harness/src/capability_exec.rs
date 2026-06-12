use crate::registry;
use chrono::Utc;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

pub struct ExecContext {
    pub caller: String,
    pub created_by: String,
    pub wake_n: u64,
    pub timeout_secs: u64,
    pub output_cap_bytes: usize,
    pub calls_per_wake_cap: u32,
}

#[derive(Debug)]
pub struct ExecResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub timed_out: bool,
    pub output_truncated: bool,
}

/// Send SIGKILL to the process with the given PID.
///
/// Called after the wall-clock timeout fires, before reaping with wait().
/// Declaring kill(2) directly avoids a libc crate dependency.
#[cfg(unix)]
fn kill_by_pid(pid: u32) {
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    unsafe {
        kill(pid as i32, 9 /* SIGKILL */);
    }
}

#[cfg(not(unix))]
fn kill_by_pid(_pid: u32) {}

/// Execute `registry_dir/bin/<fn_id>` through `sandbox_dir/run-test.sh`.
///
/// Hard requirements enforced here (not in gate.rs):
/// - The sandbox script MUST exist — no bare-host fallback.
/// - Wall-clock timeout: process is killed and timed_out=true is set.
/// - Output cap: stdout and stderr are each bounded to `ctx.output_cap_bytes`.
/// - Per-wake call count: `call_count` must be < `ctx.calls_per_wake_cap`.
///
/// On success, appends one JSON-L row to `registry_dir/calls.jsonl`.
/// The harness always sets `caller` and `created_by`; the agent cannot override them.
pub fn execute_capability(
    registry_dir: &Path,
    fn_id: &str,
    args: &[String],
    ctx: &ExecContext,
    sandbox_dir: &Path,
    call_count: &mut u32,
) -> Result<ExecResult, String> {
    // Guard: refuse to execute outside sandbox
    let sandbox_script = sandbox_dir.join("run-test.sh");
    if !sandbox_script.exists() {
        return Err(format!(
            "capability_exec: sandbox script not found at {} \
             — refusing bare-host execution of capability '{fn_id}'",
            sandbox_script.display()
        ));
    }

    // Guard: per-wake call cap
    if *call_count >= ctx.calls_per_wake_cap {
        return Err(format!(
            "capability_exec: per-wake call cap ({}) exceeded for capability '{fn_id}' in wake {}",
            ctx.calls_per_wake_cap, ctx.wake_n
        ));
    }
    *call_count += 1;

    let bin_path = registry_dir.join("bin").join(fn_id);
    // cap+1 so we can detect truncation: if we read exactly cap+1 bytes, output was longer
    let read_limit = (ctx.output_cap_bytes as u64) + 1;

    let mut child = Command::new(&sandbox_script)
        .arg(&bin_path)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("capability_exec: spawn failed for '{fn_id}': {e}"))?;

    let pid = child.id();
    let stdout_pipe = child.stdout.take().expect("stdout was piped");
    let stderr_pipe = child.stderr.take().expect("stderr was piped");

    // Bounded readers run in background threads so the main thread can enforce the timeout.
    let (out_tx, out_rx) = mpsc::channel::<Vec<u8>>();
    let (err_tx, err_rx) = mpsc::channel::<Vec<u8>>();

    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout_pipe.take(read_limit).read_to_end(&mut buf);
        let _ = out_tx.send(buf);
    });
    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr_pipe.take(read_limit).read_to_end(&mut buf);
        let _ = err_tx.send(buf);
    });

    // Poll for child exit with a hard wall-clock timeout.
    let start = Instant::now();
    let timeout = Duration::from_secs(ctx.timeout_secs);
    let mut timed_out = false;

    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if start.elapsed() >= timeout {
                    kill_by_pid(pid);
                    timed_out = true;
                    break;
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(format!("capability_exec: wait error for '{fn_id}': {e}")),
        }
    }

    // Reap the child (blocks until the OS confirms exit; safe after SIGKILL).
    let raw_exit = child.wait().ok().and_then(|s| s.code());
    let exit_code: i32 = if timed_out { -1 } else { raw_exit.unwrap_or(-1) };

    // Collect bounded output — the reader threads will have returned by now because
    // either the child exited naturally or SIGKILL closed its pipe ends.
    let drain_timeout = Duration::from_secs(2);
    let mut stdout_bytes = out_rx.recv_timeout(drain_timeout).unwrap_or_default();
    let mut stderr_bytes = err_rx.recv_timeout(drain_timeout).unwrap_or_default();

    let cap = ctx.output_cap_bytes;
    let out_truncated = stdout_bytes.len() > cap;
    let err_truncated = stderr_bytes.len() > cap;
    let output_truncated = out_truncated || err_truncated;

    stdout_bytes.truncate(cap);
    stderr_bytes.truncate(cap);

    let stdout = String::from_utf8_lossy(&stdout_bytes).into_owned();
    let stderr = String::from_utf8_lossy(&stderr_bytes).into_owned();

    // Append immutable call record; harness owns caller/created_by — never the agent.
    let record = serde_json::json!({
        "ts": Utc::now().to_rfc3339(),
        "fn_id": fn_id,
        "caller": ctx.caller,
        "created_by": ctx.created_by,
        "wake_n": ctx.wake_n,
        "exit_code": exit_code,
    });
    registry::append_call(registry_dir, &record)?;

    Ok(ExecResult {
        stdout,
        stderr,
        exit_code,
        timed_out,
        output_truncated,
    })
}
