/// Port of .hex/scripts/mirofish-status.sh
/// Checks Mirofish VM status via gcloud and service health via curl.
use std::process::Command;

const PROJECT: &str = "mrap-dev";
const ZONE: &str = "us-east1-b";
const INSTANCE: &str = "mirofish";
const TAILSCALE_IP: &str = "100.108.180.3";

pub fn run_status() {
    println!("[mirofish] VM status:");
    let status = Command::new("gcloud")
        .args([
            "compute",
            "instances",
            "describe",
            INSTANCE,
            &format!("--project={}", PROJECT),
            &format!("--zone={}", ZONE),
            "--format=table(name,status,machineType)",
        ])
        .status();
    match status {
        Err(e) => eprintln!("gcloud: {e}"),
        Ok(s) if !s.success() => { /* gcloud prints its own error */ }
        _ => {}
    }

    println!();
    println!("[mirofish] Service health:");

    let backend_up = Command::new("curl")
        .args(["-sf", "--max-time", "5", &format!("http://{}:5001/", TAILSCALE_IP)])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if backend_up {
        println!("  Backend (5001): UP");
    } else {
        println!("  Backend (5001): DOWN");
    }

    let frontend_up = Command::new("curl")
        .args(["-sf", "--max-time", "5", &format!("http://{}:3000/", TAILSCALE_IP)])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if frontend_up {
        println!("  Frontend (3000): UP");
    } else {
        println!("  Frontend (3000): DOWN");
    }
}

#[cfg(test)]
mod tests {
    /// Verify the constants used to build gcloud/curl commands match the shell script.
    #[test]
    fn constants_match_shell_script() {
        assert_eq!(super::PROJECT, "mrap-dev");
        assert_eq!(super::ZONE, "us-east1-b");
        assert_eq!(super::INSTANCE, "mirofish");
        assert_eq!(super::TAILSCALE_IP, "100.108.180.3");
    }

    #[test]
    fn curl_url_format() {
        let backend = format!("http://{}:5001/", super::TAILSCALE_IP);
        let frontend = format!("http://{}:3000/", super::TAILSCALE_IP);
        assert_eq!(backend, "http://100.108.180.3:5001/");
        assert_eq!(frontend, "http://100.108.180.3:3000/");
    }
}
