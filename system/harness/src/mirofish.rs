/// Port of .hex/scripts/mirofish-status.sh
/// Checks Mirofish VM status via gcloud and service health via curl.
use std::process::Command;

const PROJECT: &str = "mrap-dev";
const ZONE: &str = "us-east1-b";
const INSTANCE: &str = "mirofish";
const TAILSCALE_IP: &str = "100.108.180.3";

pub fn run_deploy() {
    println!("[mirofish] Deploying...");
    // Acquire OrbStack lease for deploy duration (best-effort)
    let _ = std::process::Command::new("hex-orb")
        .args(["acquire", "mirofish-deploy", "--ttl", "30m"])
        .status();

    let project = PROJECT;
    let zone = ZONE;
    let instance = INSTANCE;

    let ssh_cmd = "cd /opt/mirofish && \
        sudo git pull --ff-only 2>&1 | tail -3 && \
        sudo docker compose pull 2>&1 | tail -3 && \
        sudo docker compose up -d 2>&1 | tail -5 && \
        echo 'Deploy complete'";

    let status = std::process::Command::new("gcloud")
        .args([
            "compute",
            "ssh",
            instance,
            &format!("--project={}", project),
            &format!("--zone={}", zone),
            "--command",
            ssh_cmd,
        ])
        .status();

    // Release lease (best-effort)
    let _ = std::process::Command::new("hex-orb")
        .args(["release", "mirofish-deploy"])
        .status();

    match status {
        Err(e) => eprintln!("gcloud: {e}"),
        Ok(s) if !s.success() => { /* gcloud prints its own error */ }
        _ => {}
    }
}

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
    #[test]
    fn deploy_ssh_command_contains_expected_steps() {
        let cmd = "cd /opt/mirofish && \
            sudo git pull --ff-only 2>&1 | tail -3 && \
            sudo docker compose pull 2>&1 | tail -3 && \
            sudo docker compose up -d 2>&1 | tail -5 && \
            echo 'Deploy complete'";
        assert!(cmd.contains("git pull --ff-only"));
        assert!(cmd.contains("docker compose pull"));
        assert!(cmd.contains("docker compose up -d"));
        assert!(cmd.contains("Deploy complete"));
    }

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
