/// Port of .hex/scripts/workspace.sh
/// Launches the hex tmux workspace: LLM CLI main pane + landings dashboard + BOI status.
use std::path::Path;
use std::process::Command;

const SESSION: &str = "hex";
const DASH_WIDTH: &str = "10%";
const BOI_BIN: &str = ".local/bin/boi";

fn tmux(args: &[&str]) -> bool {
    Command::new("tmux")
        .args(args)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn tmux_output(args: &[&str]) -> String {
    Command::new("tmux")
        .args(args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn in_tmux() -> bool {
    std::env::var("TMUX").map(|v| !v.is_empty()).unwrap_or(false)
}

fn boi_bin(home: &Path) -> std::path::PathBuf {
    home.join(BOI_BIN)
}

fn first_window() -> String {
    tmux_output(&[
        "list-windows",
        "-t",
        SESSION,
        "-F",
        "#{window_index}",
    ])
    .lines()
    .next()
    .unwrap_or("0")
    .to_string()
}

pub fn run_launch(hex_dir: &Path) {
    let home = dirs_home();

    // Start hex-watcher (idempotent)
    let watcher = hex_dir.join(".hex/scripts/hex-watcher");
    if watcher.exists() {
        let _ = Command::new(&watcher).arg("start").status();
    }

    // Start hex-bot (idempotent)
    let bot = hex_dir.join(".hex/scripts/hex-bot");
    if bot.exists() {
        let _ = Command::new(&bot).arg("start").status();
    }

    // Already inside the hex session?
    if in_tmux() {
        let current = tmux_output(&["display-message", "-p", "#S"]);
        if current == SESSION {
            // Ensure panes are set up
            let pane_count: usize = tmux_output(&["list-panes"])
                .lines()
                .count();
            let dashboard = hex_dir.join(".hex/scripts/landings-dashboard.sh");
            if pane_count == 1 {
                let dash_cmd = format!(
                    "HEX_DIR='{}' bash '{}' --watch",
                    hex_dir.display(),
                    dashboard.display()
                );
                tmux(&["split-window", "-h", "-l", DASH_WIDTH, &dash_cmd]);
                let w = first_window();
                let dash_pane = tmux_output(&[
                    "list-panes",
                    "-t",
                    &format!("{}:{}", SESSION, w),
                    "-F",
                    "#{pane_index}",
                ])
                .lines()
                .last()
                .unwrap_or("1")
                .to_string();
                let boi = boi_bin(&home);
                if boi.exists() {
                    let target = format!("{}:{}.{}", SESSION, w, dash_pane);
                    tmux(&[
                        "split-window",
                        "-t", &target,
                        "-v", "-l", "35%",
                        "-c", &hex_dir.to_string_lossy(),
                        &format!("'{}' status --compact", boi.display()),
                    ]);
                }
                let main_pane = tmux_output(&[
                    "list-panes",
                    "-t",
                    &format!("{}:{}", SESSION, w),
                    "-F",
                    "#{pane_index}",
                ])
                .lines()
                .next()
                .unwrap_or("0")
                .to_string();
                tmux(&[
                    "select-pane",
                    "-t",
                    &format!("{}:{}.{}", SESSION, w, main_pane),
                ]);
            } else if pane_count == 2 {
                let boi = boi_bin(&home);
                if boi.exists() {
                    let w = first_window();
                    let dash_pane = tmux_output(&[
                        "list-panes",
                        "-t",
                        &format!("{}:{}", SESSION, w),
                        "-F",
                        "#{pane_index}",
                    ])
                    .lines()
                    .last()
                    .unwrap_or("1")
                    .to_string();
                    let target = format!("{}:{}.{}", SESSION, w, dash_pane);
                    tmux(&[
                        "split-window",
                        "-t", &target,
                        "-v", "-l", "35%",
                        "-c", &hex_dir.to_string_lossy(),
                        &format!("'{}' status --compact", boi.display()),
                    ]);
                }
            }
            return;
        }
    }

    // Session already exists? Attach.
    let session_exists = Command::new("tmux")
        .args(["has-session", "-t", SESSION])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if session_exists {
        if in_tmux() {
            tmux(&["switch-client", "-t", SESSION]);
        } else {
            let _ = Command::new("tmux")
                .args(["attach-session", "-t", SESSION])
                .exec_replace();
        }
        return;
    }

    // Create new session
    let user_shell = std::env::var("SHELL").unwrap_or_else(|_| "bash".to_string());
    let shell_name = Path::new(&user_shell)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("bash");
    let shell_cmd = if shell_name == "zsh" { "zsh -ic" } else { "bash -ic" };

    // Detect LLM CLI name (prefer claude)
    let cli_name = detect_cli_name();

    let launch_cmd = format!("{shell_cmd} \"{cli_name} '/hex-startup'\"");
    tmux(&[
        "new-session",
        "-d",
        "-s", SESSION,
        "-c", &hex_dir.to_string_lossy(),
        &launch_cmd,
    ]);

    let dashboard = hex_dir.join(".hex/scripts/landings-dashboard.sh");
    let dash_cmd = format!(
        "sleep 0.5 && HEX_DIR='{}' bash '{}' --watch",
        hex_dir.display(),
        dashboard.display()
    );
    tmux(&[
        "split-window",
        "-h",
        "-t", SESSION,
        "-l", DASH_WIDTH,
        "-c", &hex_dir.to_string_lossy(),
        &dash_cmd,
    ]);

    let w = first_window();
    let dash_pane = tmux_output(&[
        "list-panes",
        "-t",
        &format!("{}:{}", SESSION, w),
        "-F",
        "#{pane_index}",
    ])
    .lines()
    .last()
    .unwrap_or("1")
    .to_string();

    let boi = boi_bin(&home);
    if boi.exists() {
        let target = format!("{}:{}.{}", SESSION, w, dash_pane);
        tmux(&[
            "split-window",
            "-t", &target,
            "-v", "-l", "35%",
            "-c", &hex_dir.to_string_lossy(),
            &format!("sleep 0.5 && '{}' status --compact", boi.display()),
        ]);
    }

    let main_pane = tmux_output(&[
        "list-panes",
        "-t",
        &format!("{}:{}", SESSION, w),
        "-F",
        "#{pane_index}",
    ])
    .lines()
    .next()
    .unwrap_or("0")
    .to_string();
    tmux(&[
        "select-pane",
        "-t",
        &format!("{}:{}.{}", SESSION, w, main_pane),
    ]);

    // Register context and bind picker
    let ctx_lib = hex_dir.join(".hex/scripts/hex-context-lib.sh");
    if ctx_lib.exists() {
        let _ = Command::new("bash")
            .arg("-c")
            .arg(format!(
                "source '{}' && ctx_register main",
                ctx_lib.display()
            ))
            .status();
    }
    let picker = hex_dir.join(".hex/scripts/hex-picker.sh");
    if picker.exists() {
        tmux(&[
            "bind-key",
            "-T", "root",
            r"C-\",
            "run-shell",
            &format!("bash '{}'", picker.display()),
        ]);
    }

    // Set status-right
    let ctx_status = hex_dir.join(".hex/scripts/hex-context-status.sh");
    if ctx_status.exists() {
        tmux(&[
            "set-option",
            "-t", SESSION,
            "status-right",
            &format!("#(bash '{}') %H:%M", ctx_status.display()),
        ]);
        tmux(&["set-option", "-t", SESSION, "status-interval", "5"]);
    }

    // Attach
    if in_tmux() {
        tmux(&["switch-client", "-t", SESSION]);
    } else {
        let _ = Command::new("tmux")
            .args(["attach-session", "-t", SESSION])
            .exec_replace();
    }
}

fn dirs_home() -> std::path::PathBuf {
    std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("/tmp"))
}

fn detect_cli_name() -> &'static str {
    // Prefer claude (Claude Code CLI)
    if Command::new("which")
        .arg("claude")
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        return "claude";
    }
    "llm"
}

/// Replaces the current process with tmux attach (exec semantics).
trait ExecReplace {
    fn exec_replace(&mut self) -> std::io::Error;
}

impl ExecReplace for Command {
    fn exec_replace(&mut self) -> std::io::Error {
        use std::os::unix::process::CommandExt;
        self.exec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_name_constant() {
        assert_eq!(SESSION, "hex");
    }

    #[test]
    fn dash_width_constant() {
        assert_eq!(DASH_WIDTH, "10%");
    }

    #[test]
    fn boi_bin_path_constructed_from_home() {
        let home = std::path::PathBuf::from("/Users/test");
        let path = boi_bin(&home);
        assert_eq!(path, std::path::PathBuf::from("/Users/test/.local/bin/boi"));
    }

    #[test]
    fn dirs_home_is_nonempty() {
        let h = dirs_home();
        assert!(!h.as_os_str().is_empty());
    }
}
