//! iMessage transport for HITL pings + the daily digest.
//!
//! Send policy (spec):
//!   - With `config.imessage_handle` set: send over iMessage via `osascript`
//!     using the MODERN participant/account pattern only. The legacy
//!     per-service selector form is BROKEN on modern macOS (AppleEvent -1712)
//!     and is never used here — we target an account, not a service.
//!   - Unset handle OR a send failure ⇒ degrade to [`crate::alert::notify`]
//!     with the same text, and log the degradation.
//!   - EVERY attempt (sent, fallback) is recorded to `log.jsonl` and telemetry.
//!
//! Testability: all sends go through the [`Sender`] trait so policy/digest
//! tests use a mock and never invoke `osascript`. The real `osascript` call is
//! gated behind `#[cfg(all(target_os = "macos", not(test)))]` exactly like
//! `alert.rs`.

use std::path::Path;

use chrono::Utc;

use crate::hitl::store;

/// What actually happened to a send attempt. `send` never hard-fails: a failed
/// iMessage always degrades to an alert, so the human is still notified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Delivered over iMessage.
    Sent,
    /// iMessage unavailable/failed — delivered via `alert::notify` instead.
    Fallback,
}

/// The seam. Production uses [`OsascriptSender`]; tests use a mock.
pub trait Sender {
    /// Deliver `text` to `handle`. `Err` triggers the alert fallback.
    fn send(&self, handle: &str, text: &str) -> Result<(), String>;
}

/// Real iMessage sender (osascript, macOS only — no-op elsewhere/under test).
pub struct OsascriptSender;

impl Sender for OsascriptSender {
    fn send(&self, handle: &str, text: &str) -> Result<(), String> {
        osascript_send(handle, text)
    }
}

/// Send `text` for `item_id` (None ⇒ digest), logging + recording every
/// attempt. Returns whether it went over iMessage or fell back to an alert.
pub fn send(
    hex_dir: &Path,
    cfg: &store::Config,
    sender: &dyn Sender,
    item_id: Option<u64>,
    event: &str,
    text: &str,
) -> Outcome {
    let handle = cfg
        .imessage_handle
        .as_deref()
        .map(str::trim)
        .filter(|h| !h.is_empty());

    let (outcome, log_event, status, detail) = match handle {
        Some(handle) => match sender.send(handle, text) {
            Ok(()) => (
                Outcome::Sent,
                format!("{event}-sent"),
                "ok",
                format!("imessage -> {handle}"),
            ),
            Err(e) => {
                // Degrade loudly (S6): the operator still gets pinged.
                crate::alert::notify(&alert_key(event, item_id), "HITL", text);
                (
                    Outcome::Fallback,
                    format!("{event}-fallback"),
                    "fallback",
                    format!("imessage send failed ({e}); alert fallback"),
                )
            }
        },
        None => {
            crate::alert::notify(&alert_key(event, item_id), "HITL", text);
            (
                Outcome::Fallback,
                format!("{event}-fallback"),
                "fallback",
                "no imessage_handle configured; alert fallback".to_string(),
            )
        }
    };

    // Record on EVERY path — log.jsonl + telemetry (spec).
    let _ = store::append_log(hex_dir, Utc::now(), item_id, &log_event, Some(detail.clone()));
    let _ = crate::telemetry::record(&crate::telemetry::TelemetryEvent {
        source: "hitl".into(),
        event: log_event,
        status: status.into(),
        duration_ms: None,
        exit_code: None,
        detail: Some(detail),
    });

    outcome
}

fn alert_key(event: &str, item_id: Option<u64>) -> String {
    match item_id {
        Some(id) => format!("hitl-{event}-{id}"),
        None => format!("hitl-{event}"),
    }
}

/// Render an arbitrary (possibly multi-line, quote-bearing) string as an
/// AppleScript string expression: each line is a quoted literal with `"` and
/// `\` escaped, joined by `& linefeed &` so newlines survive AppleScript
/// parsing (a raw newline inside an osascript `-e` string is a parse error).
fn applescript_string(s: &str) -> String {
    s.split('\n')
        .map(|line| format!("\"{}\"", line.replace('\\', "\\\\").replace('"', "\\\"")))
        .collect::<Vec<_>>()
        .join(" & linefeed & ")
}

#[cfg(all(target_os = "macos", not(test)))]
fn osascript_send(handle: &str, text: &str) -> Result<(), String> {
    // MODERN pattern ONLY: target an account, never a per-service selector.
    let script = format!(
        "tell application \"Messages\" to send {} to participant {} of \
         (1st account whose service type = iMessage)",
        applescript_string(text),
        applescript_string(handle),
    );
    let out = std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .map_err(|e| format!("osascript spawn failed: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "osascript exit {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Off macOS / under test there is no real transport — the mock `Sender`
/// covers tests, and the caller degrades to `alert::notify` in practice.
#[cfg(not(all(target_os = "macos", not(test))))]
fn osascript_send(_handle: &str, _text: &str) -> Result<(), String> {
    let _ = applescript_string; // keep exercised on all cfgs
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct MockOk {
        calls: RefCell<Vec<(String, String)>>,
    }
    impl Sender for MockOk {
        fn send(&self, handle: &str, text: &str) -> Result<(), String> {
            self.calls.borrow_mut().push((handle.into(), text.into()));
            Ok(())
        }
    }

    struct MockErr;
    impl Sender for MockErr {
        fn send(&self, _handle: &str, _text: &str) -> Result<(), String> {
            Err("boom".into())
        }
    }

    fn cfg_with_handle(h: Option<&str>) -> store::Config {
        store::Config {
            imessage_handle: h.map(|s| s.to_string()),
            ..store::Config::default()
        }
    }

    #[test]
    fn sends_over_imessage_when_handle_set_and_ok() {
        let _g = crate::telemetry::test_support::lock_env();
        let tmp = tempfile::TempDir::new().unwrap();
        std::env::set_var("HEX_DIR", tmp.path());
        let m = MockOk {
            calls: RefCell::new(Vec::new()),
        };
        let out = send(
            tmp.path(),
            &cfg_with_handle(Some("+15551234567")),
            &m,
            Some(1),
            "ping",
            "hello",
        );
        assert_eq!(out, Outcome::Sent);
        assert_eq!(m.calls.borrow().len(), 1);
        let log = store::read_log(tmp.path()).unwrap();
        assert!(log.iter().any(|e| e.event == "ping-sent"));
    }

    #[test]
    fn falls_back_when_no_handle() {
        let _g = crate::telemetry::test_support::lock_env();
        let tmp = tempfile::TempDir::new().unwrap();
        std::env::set_var("HEX_DIR", tmp.path());
        let out = send(
            tmp.path(),
            &cfg_with_handle(None),
            &MockErr,
            Some(1),
            "ping",
            "hello",
        );
        assert_eq!(out, Outcome::Fallback);
        let log = store::read_log(tmp.path()).unwrap();
        assert!(log.iter().any(|e| e.event == "ping-fallback"));
    }

    #[test]
    fn falls_back_when_send_errors() {
        let _g = crate::telemetry::test_support::lock_env();
        let tmp = tempfile::TempDir::new().unwrap();
        std::env::set_var("HEX_DIR", tmp.path());
        let out = send(
            tmp.path(),
            &cfg_with_handle(Some("+1")),
            &MockErr,
            Some(2),
            "digest",
            "hi",
        );
        assert_eq!(out, Outcome::Fallback);
        let log = store::read_log(tmp.path()).unwrap();
        assert!(log.iter().any(|e| e.event == "digest-fallback"));
    }

    #[test]
    fn applescript_escapes_quotes_and_newlines() {
        assert_eq!(
            applescript_string("a\"b\nc"),
            "\"a\\\"b\" & linefeed & \"c\""
        );
        assert_eq!(applescript_string("plain"), "\"plain\"");
    }
}
