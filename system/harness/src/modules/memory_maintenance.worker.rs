//! `hex-memory-maintenance` — Rust port of
//! `system/iii/workers/memory-maintenance.yaml`.
//!
//! Jobs (the YAML kept index + consolidate_full only; this host is canonical):
//!
//! - id `hex::memory::index`               command `hex memory index`               cron `0 */15 * * * * *`
//! - id `hex::memory::consolidate_quick`   command `hex memory consolidate quick`   cron `0 5,20,35,50 * * * * *`
//! - id `hex::memory::parse_transcripts`   command `hex memory parse-transcripts`   cron `0 */15 * * * * *`
//! - id `hex::memory::consolidate_full`    command `hex memory consolidate full`    cron `0 0 3 * * * *`
//! - id `hex::memory::maintain`            command `hex memory maintain --vacuum --backfill-facts`
//!   cron `0 33 4 * * SUN *`
//!
//! The YAML file is intentionally left in place (additive migration — a later
//! spec removes the YAML-host path).

use hex::worker::{ctx::Ctx, event::Event, Result, Worker};

/// Cron expression for `hex::memory::index` — every 15 minutes.
pub const CRON_INDEX: &str = "0 */15 * * * * *";

/// Quick consolidation — offset from :00 so it never collides with the
/// 03:00:00Z full run (2026-06-10: full lock-skipped behind a quick tick
/// that fired the same second).
pub const CRON_CONSOLIDATE_QUICK: &str = "0 5,20,35,50 * * * * *";

/// Cron expression for `hex::memory::parse_transcripts` — every 15 minutes.
pub const CRON_PARSE_TRANSCRIPTS: &str = "0 */15 * * * * *";

/// Cron expression for `hex::memory::consolidate_full` — 03:00 daily.
pub const CRON_CONSOLIDATE_FULL: &str = "0 0 3 * * * *";

/// Weekly self-repair — Sunday 04:33Z (after the 04:00Z backup).
/// Offset off the :00/:15/:30/:45 boundary so maintain does not START the same
/// second as the 15-minute `hex memory index` tick (CRON_INDEX) — the
/// deterministic same-second collision that BUSY-failed the unlocked VACUUM.
/// Same offset rationale as CRON_CONSOLIDATE_QUICK. NOTE this bounds the START
/// only: VACUUM runs LAST (after sweep/optimize/hygiene/facts-backfill) and can
/// drift into the :35 quick-consolidate or :45 index tick if facts-backfill is
/// slow; that residual VACUUM-vs-writer contention is backstopped by
/// busy_timeout (5s) + a loud failure (S6) + recovery on the next weekly run.
/// The durable fix (a cross-process DB-quiescence lock around VACUUM) is queued
/// in evolution/fix-backlog.md. (cron 0.15, the iii engine's parser, parses the
/// "SUN"|"sunday" day-of-week token → ordinal 1, verified.)
pub const CRON_MAINTAIN: &str = "0 33 4 * * SUN *";

/// Argv for the index job — mirrors the YAML `command:` array.
pub const ARGV_INDEX: &[&str] = &["hex", "memory", "index"];

/// Argv for the quick-consolidation job (Layers 1+2, deterministic).
pub const ARGV_CONSOLIDATE_QUICK: &[&str] = &["hex", "memory", "consolidate", "quick"];

/// Argv for the transcript-parse job.
pub const ARGV_PARSE_TRANSCRIPTS: &[&str] = &["hex", "memory", "parse-transcripts"];

/// Argv for the nightly full-consolidation job.
pub const ARGV_CONSOLIDATE_FULL: &[&str] = &["hex", "memory", "consolidate", "full"];

/// Argv for the weekly self-repair job.
pub const ARGV_MAINTAIN: &[&str] = &["hex", "memory", "maintain", "--vacuum", "--backfill-facts"];

/// Job id logged for `hex memory index` — matches the doc comment at the top
/// of this file.
pub const JOB_ID_INDEX: &str = "hex::memory::index";
/// Job id logged for `hex memory consolidate quick`.
pub const JOB_ID_CONSOLIDATE_QUICK: &str = "hex::memory::consolidate_quick";
/// Job id logged for `hex memory parse-transcripts`.
pub const JOB_ID_PARSE_TRANSCRIPTS: &str = "hex::memory::parse_transcripts";
/// Job id logged for `hex memory consolidate full`.
pub const JOB_ID_CONSOLIDATE_FULL: &str = "hex::memory::consolidate_full";
/// Job id logged for `hex memory maintain --vacuum --backfill-facts`.
pub const JOB_ID_MAINTAIN: &str = "hex::memory::maintain";

/// Format a completed job's captured stdout/stderr into log lines prefixed
/// with `[job_id]`, followed by exactly one final `[job_id] exit=<code>
/// elapsed_ms=<n>` line. Pure formatter — no I/O — so the prefixing/exit/
/// wall-time contract is checkable without capturing process-global stdout.
/// `Ctx::run` returns a fully-captured `std::process::Output` (see
/// `src/worker/ctx.rs`), so nothing needs to stream.
fn job_log_lines(
    job_id: &str,
    output: &std::process::Output,
    elapsed: std::time::Duration,
) -> Vec<String> {
    let mut lines = Vec::new();
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        lines.push(format!("[{job_id}] {line}"));
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    for line in stderr.lines() {
        lines.push(format!("[{job_id}] {line}"));
    }
    let code = output
        .status
        .code()
        .map(|c| c.to_string())
        .unwrap_or_else(|| "signal".to_string());
    lines.push(format!(
        "[{job_id}] exit={code} elapsed_ms={}",
        elapsed.as_millis()
    ));
    lines
}

/// Run `argv` via `ctx.run`, timing it, and forward the child's stdout+stderr
/// (plus an exit-status/wall-time line) into `out`/`err`. Production calls
/// this through `run_and_log`, which points `out`/`err` at real
/// `io::stdout()`/`io::stderr()` — the same fds `println!`/`eprintln!` write
/// to, and the ones `harness/supervise.rs` routes to
/// `.hex/logs/com.hex.harness.log`. Writers are injected (rather than hardcoded
/// `println!`/`eprintln!`) so tests can exercise this exact log-forwarding
/// logic — the one every job runner calls — without depending on `cargo
/// test`'s stdout capture semantics. Uses `Ctx::run_output` (Output regardless
/// of exit) so a FAILING job's captured stdout/stderr are forwarded through
/// `job_log_lines` too, and only then is the non-zero exit propagated as an
/// `Err` via `exit_error` (spec-review finding G1).
fn run_and_log_to<W: std::io::Write, E: std::io::Write>(
    job_id: &str,
    ctx: &Ctx,
    argv: &[String],
    mut out: W,
    mut err: E,
) -> Result<()> {
    let started = std::time::Instant::now();
    // G1 (spec review): capture the Output REGARDLESS of exit status so a
    // failing job's stdout/stderr still reach the harness log through
    // job_log_lines; only THEN judge the exit and propagate it as an Err.
    let output = match ctx.run_output(argv) {
        Ok(output) => output,
        Err(e) => {
            let elapsed = started.elapsed();
            let _ = writeln!(
                err,
                "[{job_id}] exit=error elapsed_ms={} error={e}",
                elapsed.as_millis()
            );
            return Err(e);
        }
    };
    let elapsed = started.elapsed();
    for line in job_log_lines(job_id, &output, elapsed) {
        let _ = writeln!(out, "{line}");
    }
    let program = argv.first().map(String::as_str).unwrap_or("");
    if let Some(e) = hex::worker::ctx::exit_error(program, &output) {
        let _ = writeln!(
            err,
            "[{job_id}] exit=error elapsed_ms={} error={e}",
            elapsed.as_millis()
        );
        return Err(e);
    }
    Ok(())
}

/// Production entry point used by every job runner below — forwards to
/// `run_and_log_to` with real stdout/stderr.
fn run_and_log(job_id: &str, ctx: &Ctx, argv: &[String]) -> Result<()> {
    run_and_log_to(job_id, ctx, argv, std::io::stdout(), std::io::stderr())
}

fn run_index(_e: Event, ctx: Ctx) -> Result<()> {
    let argv: Vec<String> = ARGV_INDEX.iter().map(|s| s.to_string()).collect();
    run_and_log(JOB_ID_INDEX, &ctx, &argv)
}

fn run_consolidate_quick(_e: Event, ctx: Ctx) -> Result<()> {
    let argv: Vec<String> = ARGV_CONSOLIDATE_QUICK
        .iter()
        .map(|s| s.to_string())
        .collect();
    run_and_log(JOB_ID_CONSOLIDATE_QUICK, &ctx, &argv)
}

fn run_parse_transcripts(_e: Event, ctx: Ctx) -> Result<()> {
    let argv: Vec<String> = ARGV_PARSE_TRANSCRIPTS
        .iter()
        .map(|s| s.to_string())
        .collect();
    run_and_log(JOB_ID_PARSE_TRANSCRIPTS, &ctx, &argv)
}

fn run_consolidate_full(_e: Event, ctx: Ctx) -> Result<()> {
    let argv: Vec<String> = ARGV_CONSOLIDATE_FULL
        .iter()
        .map(|s| s.to_string())
        .collect();
    run_and_log(JOB_ID_CONSOLIDATE_FULL, &ctx, &argv)
}

fn run_maintain(_e: Event, ctx: Ctx) -> Result<()> {
    let argv: Vec<String> = ARGV_MAINTAIN.iter().map(|s| s.to_string()).collect();
    run_and_log(JOB_ID_MAINTAIN, &ctx, &argv)
}

/// Build the `hex-memory-maintenance` worker.
pub fn worker() -> Worker {
    Worker::new("hex-memory-maintenance")
        .on_cron_named("index", CRON_INDEX, run_index)
        .on_cron_named("quick", CRON_CONSOLIDATE_QUICK, run_consolidate_quick)
        .on_cron_named("parse-transcripts", CRON_PARSE_TRANSCRIPTS, run_parse_transcripts)
        .on_cron_named("consolidate-full", CRON_CONSOLIDATE_FULL, run_consolidate_full)
        .on_cron_named("maintain-weekly", CRON_MAINTAIN, run_maintain)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CONTRACT pinned here (not the internals): every job runner must forward
    /// its child's stdout AND stderr into the harness log, one line per child
    /// line, prefixed with the job id `[job_id] <line>`, followed by exactly
    /// one final line reporting exit status and wall time. `Ctx::run` (see
    /// `src/worker/ctx.rs`) returns a fully-captured `std::process::Output` on
    /// success, so nothing needs to stream — this crate has no `tracing`/`log`
    /// dependency (grepped: none in Cargo.toml or src/), and the task may not
    /// add one, so the emission mechanism is the same `println!`/`eprintln!`
    /// pattern already used for op lines in `src/memory/consolidate.rs` and
    /// `src/harness/supervise.rs` routes that stdout+stderr to
    /// `.hex/logs/com.hex.harness.log` via the launchd `log_path` wiring. The
    /// seam this test exercises is a pure formatter — `job_log_lines` — so the
    /// prefixing/exit/wall-time contract is checkable without capturing
    /// process-global stdout.
    ///
    /// Was RED: `job_log_lines` did not exist yet; now green.
    /// Spec-review finding G1 (S2fmt9jyc): a FAILING job must not lose its
    /// captured output. Both streams are forwarded through `job_log_lines`
    /// (prefixed, plus the exit line), and the non-zero exit is STILL
    /// propagated as an `Err`. Was RED: the old path matched on `Ctx::run`'s
    /// `Err` and logged only the error line.
    #[test]
    fn failing_job_forwards_both_streams_then_propagates_nonzero_exit() {
        let ctx = Ctx::new();
        let argv: Vec<String> =
            vec!["sh".into(), "-c".into(), "echo out1; echo err1 >&2; exit 3".into()];
        let mut out: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();

        let res = run_and_log_to("hex::memory::index", &ctx, &argv, &mut out, &mut err);

        assert!(res.is_err(), "a non-zero exit must still propagate as Err");
        let out_s = String::from_utf8(out).expect("utf8");
        let err_s = String::from_utf8(err).expect("utf8");
        assert!(out_s.contains("[hex::memory::index] out1"), "stdout line lost: {out_s}");
        assert!(out_s.contains("[hex::memory::index] err1"), "stderr line lost: {out_s}");
        assert!(out_s.contains("exit=3"), "exit-status line missing: {out_s}");
        assert!(err_s.contains("exited 3"), "propagated error missing: {err_s}");
    }

    #[test]
    fn job_log_lines_prefixes_stdout_and_appends_exit_status_line() {
        let ctx = Ctx::new();
        let argv: Vec<String> = vec!["sh".into(), "-c".into(), "echo line1; echo line2".into()];

        let started = std::time::Instant::now();
        let output = ctx
            .run(&argv)
            .expect("a zero-exit fake job must not error via Ctx::run");
        let elapsed = started.elapsed();

        let lines = job_log_lines("hex::memory::index", &output, elapsed);

        assert_eq!(
            lines.len(),
            3,
            "expected 2 prefixed stdout lines + 1 exit-status line, got: {lines:?}"
        );
        assert_eq!(lines[0], "[hex::memory::index] line1");
        assert_eq!(lines[1], "[hex::memory::index] line2");
        assert!(
            lines[2].starts_with("[hex::memory::index] exit=0"),
            "final line must report exit status; got: {}",
            lines[2]
        );
        assert!(
            lines[2].contains("elapsed_ms="),
            "final line must report wall time; got: {}",
            lines[2]
        );
    }

    /// Same contract on the stderr side — a job that only writes to stderr
    /// still gets its lines prefixed and forwarded (the task requires BOTH
    /// stdout and stderr, not just stdout).
    ///
    /// Was RED: `job_log_lines` did not exist yet; now green.
    #[test]
    fn job_log_lines_prefixes_stderr_lines_too() {
        let ctx = Ctx::new();
        let argv: Vec<String> = vec!["sh".into(), "-c".into(), "echo err1 >&2".into()];

        let output = ctx
            .run(&argv)
            .expect("a zero-exit fake job must not error via Ctx::run");
        let lines = job_log_lines("hex::memory::maintain", &output, std::time::Duration::ZERO);

        assert!(
            lines.contains(&"[hex::memory::maintain] err1".to_string()),
            "stderr line must be forwarded with the job-id prefix; got: {lines:?}"
        );
        assert!(
            lines
                .last()
                .expect("must have at least the exit-status line")
                .starts_with("[hex::memory::maintain] exit=0"),
            "final line must report exit status; got: {lines:?}"
        );
    }

    /// Closes the gap the prior review flagged (G1): the tests above exercise
    /// `Ctx::run` and `job_log_lines` separately but never the production
    /// log-forwarding function every job runner (`run_index`,
    /// `run_consolidate_quick`, ...) actually calls. This test runs
    /// `run_and_log_to` — the exact logic behind `run_and_log`, which each job
    /// runner invokes unconditionally — end to end: a fake job argv that
    /// prints two stdout lines must yield two id-prefixed lines followed by
    /// one exit-status line in the captured writer. If the forwarding loop in
    /// `run_and_log_to` were deleted, this test fails (empty buffer); the
    /// pure-formatter tests above would still pass.
    ///
    /// Was RED: `run_and_log_to` did not exist yet; now green.
    #[test]
    fn run_and_log_to_forwards_stdout_lines_and_exit_status() {
        let ctx = Ctx::new();
        let argv: Vec<String> = vec!["sh".into(), "-c".into(), "echo alpha; echo beta".into()];
        let mut out: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();

        let result = run_and_log_to("hex::memory::index", &ctx, &argv, &mut out, &mut err);
        assert!(result.is_ok(), "a zero-exit fake job must not error");

        let captured = String::from_utf8(out).expect("captured stdout must be valid utf8");
        let lines: Vec<&str> = captured.lines().collect();
        assert_eq!(
            lines.len(),
            3,
            "expected 2 forwarded stdout lines + 1 exit-status line, got: {captured:?}"
        );
        assert_eq!(lines[0], "[hex::memory::index] alpha");
        assert_eq!(lines[1], "[hex::memory::index] beta");
        assert!(
            lines[2].starts_with("[hex::memory::index] exit=0"),
            "final line must report exit status; got: {}",
            lines[2]
        );
        assert!(
            lines[2].contains("elapsed_ms="),
            "final line must report wall time; got: {}",
            lines[2]
        );
        assert!(
            err.is_empty(),
            "a successful job must not write to the error writer"
        );
    }

    /// Same contract on the failure path: `Ctx::run` surfaces a non-zero exit
    /// as an `Err`, so `run_and_log_to` must log an `exit=error` line to the
    /// error writer (not the stdout writer) and propagate the `Err`.
    ///
    /// Was RED: `run_and_log_to` did not exist yet; now green.
    #[test]
    fn run_and_log_to_logs_to_err_writer_on_ctx_run_failure() {
        let ctx = Ctx::new();
        // A nonexistent binary makes `Ctx::run` itself fail (spawn error),
        // matching the `Err` branch this test targets.
        let argv: Vec<String> = vec!["hex-maintenance-worker-test-nonexistent-binary".into()];
        let mut out: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();

        let result = run_and_log_to("hex::memory::maintain", &ctx, &argv, &mut out, &mut err);
        assert!(result.is_err(), "a spawn failure must propagate as Err");
        assert!(out.is_empty(), "a failed job must not write to the stdout writer");

        let captured = String::from_utf8(err).expect("captured stderr must be valid utf8");
        assert!(
            captured.starts_with("[hex::memory::maintain] exit=error"),
            "error line must be prefixed with the job id and report exit=error; got: {captured:?}"
        );
    }
}
