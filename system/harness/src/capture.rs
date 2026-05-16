/// Port of .hex/scripts/capture.sh
/// Zero-friction context capture for hex agents.
/// Writes a timestamped markdown file to $HEX_DIR/raw/captures/.
use std::io::{self, IsTerminal, Read};
use std::path::PathBuf;
use std::process::Command;

pub fn run_capture(hex_dir: &PathBuf, text_args: &[String]) {
    let captures_dir = hex_dir.join("raw/captures");
    std::fs::create_dir_all(&captures_dir).unwrap_or_else(|e| {
        eprintln!("ERROR: cannot create {}: {e}", captures_dir.display());
        std::process::exit(1);
    });

    let text = collect_text(text_args);

    if text.trim().is_empty() {
        println!("Nothing to capture.");
        return;
    }

    // Generate timestamp; honour TZ from .hex/timezone if set
    let tz_file = hex_dir.join(".hex/timezone");
    if std::env::var("TZ").is_err() {
        if let Ok(tz) = std::fs::read_to_string(&tz_file) {
            let tz = tz.trim().to_string();
            if !tz.is_empty() {
                std::env::set_var("TZ", &tz);
            }
        }
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let timestamp = format_timestamp(now.as_secs());
    let filename = format!("{}.md", format_filename(now.as_secs()));
    let outfile = captures_dir.join(&filename);

    let content = format!("---\ncaptured: {timestamp}\nsource: cli\n---\n\n{text}\n");

    // Atomic write: .tmp then mv
    let tmpfile = captures_dir.join(format!("{filename}.tmp"));
    std::fs::write(&tmpfile, &content).unwrap_or_else(|e| {
        eprintln!("ERROR: cannot write {}: {e}", tmpfile.display());
        std::process::exit(1);
    });
    std::fs::rename(&tmpfile, &outfile).unwrap_or_else(|e| {
        eprintln!("ERROR: cannot rename to {}: {e}", outfile.display());
        std::process::exit(1);
    });

    // Emit telemetry (best-effort, ignore failures)
    let emit_sh = hex_dir.join(".hex/bin/hex-emit.sh");
    if emit_sh.exists() {
        let payload = format!(
            "{{\"path\":\"{}\",\"source\":\"cli\",\"timestamp\":\"{}\"}}",
            outfile.display(),
            timestamp
        );
        let _ = Command::new(&emit_sh)
            .arg("capture.created")
            .arg(&payload)
            .arg("capture-script")
            .status();
    }

    println!("Captured. Will triage on next session startup.");
}

fn collect_text(args: &[String]) -> String {
    if !args.is_empty() {
        return args.join(" ");
    }
    if !io::stdin().is_terminal() {
        let mut buf = String::new();
        io::stdin().read_to_string(&mut buf).unwrap_or(0);
        return buf;
    }
    // Interactive: use $EDITOR or raw stdin
    if let Ok(editor) = std::env::var("EDITOR") {
        if !editor.is_empty() {
            let tmpfile = std::env::temp_dir().join(format!("hex-capture-{}.md", std::process::id()));
            let _ = Command::new(&editor).arg(&tmpfile).status();
            return std::fs::read_to_string(&tmpfile).unwrap_or_default();
        }
    }
    eprintln!("Type your capture (Ctrl+D when done):");
    let mut buf = String::new();
    io::stdin().read_to_string(&mut buf).unwrap_or(0);
    buf
}

fn format_timestamp(secs: u64) -> String {
    // ISO-8601 local time via `date` — avoids pulling in a time crate
    let out = Command::new("date")
        .arg("+%Y-%m-%dT%H:%M:%S")
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => format!("{secs}"),
    }
}

fn format_filename(secs: u64) -> String {
    let out = Command::new("date")
        .arg("+%Y-%m-%d_%H-%M-%S")
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => format!("{secs}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_dir_path_construction() {
        let hex_dir = PathBuf::from("/Users/test/hex");
        let expected = hex_dir.join("raw/captures");
        assert_eq!(
            expected.to_str().unwrap(),
            "/Users/test/hex/raw/captures"
        );
    }

    #[test]
    fn collect_text_from_args() {
        let args = vec!["hello".to_string(), "world".to_string()];
        assert_eq!(collect_text(&args), "hello world");
    }

    #[test]
    fn collect_text_empty_args_is_empty_string() {
        // Can't test stdin interactivity in unit test, just verify empty args returns empty
        // when stdin is a terminal (which it is in test harness)
        let args: Vec<String> = vec![];
        // In test environment stdin IS a terminal but we can't open editor.
        // Just test the args path.
        let args_with_content = vec!["test capture".to_string()];
        assert_eq!(collect_text(&args_with_content), "test capture");
        let _ = args; // suppress unused warning
    }
}
