//! Durable on-disk outbox for shutdown-window emission deferral.
//!
//! Append JSON lines to `$HEX_DIR/.hex/harness/outbox.jsonl`. On replay,
//! each entry is POPPED from the file BEFORE being delivered — pop-then-
//! deliver gives at-most-once on crash mid-replay (rather than
//! at-least-once / double-delivery).

use serde_json::Value;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

/// A single deferred emission: the event name + envelope data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Emission {
    pub event: String,
    pub data: Value,
}

impl Emission {
    fn to_json_line(&self) -> serde_json::Result<String> {
        let v = serde_json::json!({ "event": self.event, "data": self.data });
        serde_json::to_string(&v)
    }

    fn from_json_line(line: &str) -> anyhow::Result<Self> {
        let v: Value = serde_json::from_str(line)?;
        let event = v
            .get("event")
            .and_then(|e| e.as_str())
            .ok_or_else(|| anyhow::anyhow!("outbox line missing `event` string"))?
            .to_string();
        let data = v.get("data").cloned().unwrap_or(Value::Null);
        Ok(Emission { event, data })
    }
}

/// Append-only durable outbox backed by a JSON-lines file on disk.
pub struct Outbox {
    pub path: PathBuf,
}

impl Outbox {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    /// Append one emission as a JSON line. Durable on success.
    pub fn append(&self, emission: &Emission) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let line = emission.to_json_line()?;
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        f.write_all(line.as_bytes())?;
        f.write_all(b"\n")?;
        f.sync_all()?;
        Ok(())
    }

    /// Replay all queued emissions: for each entry, POP it from the
    /// durable store FIRST, THEN call `deliver`. If `deliver` panics or
    /// the process crashes mid-call, the popped entry is lost (at-most-
    /// once) rather than re-delivered.
    pub fn replay<F>(&self, mut deliver: F) -> anyhow::Result<usize>
    where
        F: FnMut(Emission) -> anyhow::Result<()>,
    {
        // Empty / nonexistent outbox => no-op.
        if !self.path.exists() {
            return Ok(0);
        }
        let lines: Vec<String> = {
            let f = File::open(&self.path)?;
            BufReader::new(f)
                .lines()
                .collect::<std::io::Result<Vec<_>>>()?
                .into_iter()
                .filter(|l| !l.trim().is_empty())
                .collect()
        };

        let mut delivered = 0usize;
        for i in 0..lines.len() {
            // POP FIRST: rewrite the durable file with only the lines
            // that come AFTER the one we're about to deliver. This must
            // happen BEFORE `deliver` is invoked so a crash mid-deliver
            // loses the entry (at-most-once) instead of re-delivering it.
            let remaining = &lines[i + 1..];
            write_lines_atomic(&self.path, remaining)?;

            let emission = Emission::from_json_line(&lines[i])?;
            deliver(emission)?;
            delivered += 1;
        }
        Ok(delivered)
    }
}

/// Atomically replace the file contents with the given lines (one per
/// line, trailing newline on each). Uses a sibling temp file + rename so
/// the swap is durable and a crash mid-write can't leave a torn file.
fn write_lines_atomic(path: &Path, lines: &[String]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("jsonl.tmp");
    {
        let mut f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp)?;
        for l in lines {
            f.write_all(l.as_bytes())?;
            f.write_all(b"\n")?;
        }
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    fn tempdir() -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "hex-outbox-test-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    /// Replay on an empty / nonexistent outbox is a no-op (returns 0,
    /// does NOT call the deliver fn).
    #[test]
    fn empty_outbox_replay_is_noop() {
        let dir = tempdir();
        let ob = Outbox::new(dir.join("outbox.jsonl"));
        let called = Arc::new(Mutex::new(0u32));
        let called_c = called.clone();
        let n = ob
            .replay(move |_e| {
                *called_c.lock().unwrap() += 1;
                Ok(())
            })
            .expect("empty replay");
        assert_eq!(n, 0);
        assert_eq!(*called.lock().unwrap(), 0);
    }

    /// append + replay round-trips: every appended emission is delivered
    /// once, in order, and the outbox is drained afterward.
    #[test]
    fn append_and_replay_round_trips() {
        let dir = tempdir();
        let ob = Outbox::new(dir.join("outbox.jsonl"));

        ob.append(&Emission {
            event: "landings.updated".to_string(),
            data: json!({ "spec_id": "S1" }),
        })
        .unwrap();
        ob.append(&Emission {
            event: "landings.updated".to_string(),
            data: json!({ "spec_id": "S2" }),
        })
        .unwrap();

        let delivered = Arc::new(Mutex::new(Vec::<Emission>::new()));
        let d2 = delivered.clone();
        let n = ob
            .replay(move |e| {
                d2.lock().unwrap().push(e);
                Ok(())
            })
            .unwrap();
        assert_eq!(n, 2);
        let got = delivered.lock().unwrap().clone();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].data["spec_id"], "S1");
        assert_eq!(got[1].data["spec_id"], "S2");

        // After replay the outbox is fully drained — a second replay
        // delivers nothing.
        let n2 = ob.replay(|_| Ok(())).unwrap();
        assert_eq!(n2, 0);
    }

    /// CORE INVARIANT: replay POPS the entry from the durable store
    /// BEFORE calling deliver. If deliver panics, the popped entry is
    /// already gone (at-most-once on crash) — it must NOT be re-
    /// delivered on a subsequent replay.
    #[test]
    fn replay_pops_before_delivering() {
        let dir = tempdir();
        let path = dir.join("outbox.jsonl");
        let ob = Outbox::new(&path);

        ob.append(&Emission {
            event: "x".to_string(),
            data: json!({ "n": 1 }),
        })
        .unwrap();

        // First replay: deliver panics. Catch the panic and assert the
        // entry was already removed from the durable file BEFORE the
        // panic happened.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = ob.replay(|_e| -> anyhow::Result<()> {
                // At this point the entry MUST already be popped.
                let on_disk = std::fs::read_to_string(&path).unwrap_or_default();
                assert!(
                    on_disk
                        .lines()
                        .filter(|l| !l.trim().is_empty())
                        .next()
                        .is_none(),
                    "entry must be popped from outbox BEFORE deliver is called; \
                     found on disk: {:?}",
                    on_disk
                );
                panic!("simulated delivery crash");
            });
        }));
        assert!(result.is_err(), "deliver was expected to panic");

        // Second replay: the entry must NOT come back (at-most-once).
        let count = Arc::new(Mutex::new(0u32));
        let c2 = count.clone();
        let _ = ob.replay(move |_e| {
            *c2.lock().unwrap() += 1;
            Ok(())
        });
        assert_eq!(
            *count.lock().unwrap(),
            0,
            "popped entry must not be re-delivered after a mid-replay crash"
        );
    }
}
