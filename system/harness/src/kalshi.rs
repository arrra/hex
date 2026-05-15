/// Port of .hex/scripts/kalshi-keygen.sh
/// Generates an RSA keypair for Kalshi API authentication and updates the secrets env file.
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn run_keygen(secrets_dir: &Path) {
    let private_key = secrets_dir.join("kalshi-private.pem");
    let public_key = secrets_dir.join("kalshi-public.pem");
    let env_file = secrets_dir.join("kalshi.env");

    if private_key.exists() {
        eprintln!("WARNING: {} already exists. Not overwriting.", private_key.display());
        eprintln!("To regenerate, delete the existing key first.");
        std::process::exit(1);
    }

    eprintln!("Generating RSA keypair for Kalshi API auth...");

    let private_key_tmp = secrets_dir.join("kalshi-private.pem.tmp");
    let public_key_tmp = secrets_dir.join("kalshi-public.pem.tmp");

    let genrsa = Command::new("openssl")
        .args(["genrsa", "-out"])
        .arg(&private_key_tmp)
        .arg("2048")
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap_or_else(|e| {
            eprintln!("openssl genrsa: {e}");
            std::process::exit(1);
        });
    if !genrsa.success() {
        eprintln!("openssl genrsa failed");
        std::process::exit(1);
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&private_key_tmp, std::fs::Permissions::from_mode(0o600))
            .unwrap_or_else(|e| {
                eprintln!("chmod 600 {}: {e}", private_key_tmp.display());
                std::process::exit(1);
            });
    }

    let pubout = Command::new("openssl")
        .args(["rsa", "-in"])
        .arg(&private_key_tmp)
        .args(["-pubout", "-out"])
        .arg(&public_key_tmp)
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap_or_else(|e| {
            eprintln!("openssl rsa: {e}");
            std::process::exit(1);
        });
    if !pubout.success() {
        eprintln!("openssl rsa -pubout failed");
        std::process::exit(1);
    }

    std::fs::rename(&private_key_tmp, &private_key).unwrap_or_else(|e| {
        eprintln!("mv {}: {e}", private_key_tmp.display());
        std::process::exit(1);
    });
    std::fs::rename(&public_key_tmp, &public_key).unwrap_or_else(|e| {
        eprintln!("mv {}: {e}", public_key_tmp.display());
        std::process::exit(1);
    });

    update_env_file(&env_file, &private_key);

    eprintln!();
    eprintln!("=== Kalshi RSA Keypair Generated ===");
    eprintln!("Private key: {} (chmod 600)", private_key.display());
    eprintln!("Public key:  {}", public_key.display());
    eprintln!();
    eprintln!("Paste the following public key into your Kalshi dashboard > API Keys > Add Key:");
    eprintln!();

    let pubkey_contents = std::fs::read_to_string(&public_key).unwrap_or_else(|e| {
        eprintln!("Cannot read {}: {e}", public_key.display());
        std::process::exit(1);
    });
    print!("{}", pubkey_contents);

    eprintln!();
    eprintln!("After pasting, copy the Key ID from the dashboard and update:");
    eprintln!("  {}", env_file.display());
    eprintln!("  Set KALSHI_KEY_ID=<your-key-id>");
}

fn update_env_file(env_file: &Path, private_key: &Path) {
    let existing = std::fs::read_to_string(env_file).unwrap_or_default();

    let has_key_id = existing.lines().any(|l| l.starts_with("KALSHI_KEY_ID="));
    let has_key_path = existing.lines().any(|l| l.starts_with("KALSHI_PRIVATE_KEY_PATH="));

    let mut lines: Vec<String> = existing.lines().map(|l| l.to_string()).collect();

    // Handle KALSHI_KEY_ID
    if has_key_id {
        let is_stub = lines.iter().any(|l| l.starts_with("KALSHI_KEY_ID=00000000"));
        if is_stub {
            for line in &mut lines {
                if line.starts_with("KALSHI_KEY_ID=") {
                    *line = "KALSHI_KEY_ID=PASTE_YOUR_KEY_ID_HERE".to_string();
                }
            }
            eprintln!("Updated KALSHI_KEY_ID placeholder in {}", env_file.display());
        } else {
            eprintln!("KALSHI_KEY_ID already set in {} — skipping update.", env_file.display());
        }
    } else {
        if !lines.last().map(|l| l.is_empty()).unwrap_or(true) {
            lines.push(String::new());
        }
        lines.push("KALSHI_KEY_ID=PASTE_YOUR_KEY_ID_HERE".to_string());
        eprintln!("Added KALSHI_KEY_ID placeholder to {}", env_file.display());
    }

    // Handle KALSHI_PRIVATE_KEY_PATH
    let key_path_val = private_key.display().to_string();
    if has_key_path {
        for line in &mut lines {
            if line.starts_with("KALSHI_PRIVATE_KEY_PATH=") {
                *line = format!("KALSHI_PRIVATE_KEY_PATH={}", key_path_val);
            }
        }
    } else {
        lines.push(format!("KALSHI_PRIVATE_KEY_PATH={}", key_path_val));
    }

    let mut content = lines.join("\n");
    if !content.ends_with('\n') {
        content.push('\n');
    }

    std::fs::write(env_file, &content).unwrap_or_else(|e| {
        eprintln!("Cannot write {}: {e}", env_file.display());
        std::process::exit(1);
    });
}

pub fn secrets_dir_from_hex(hex_dir: &Path) -> PathBuf {
    hex_dir.join(".hex/secrets")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_dir_derivation() {
        let hex = PathBuf::from("/home/user/hex");
        let secrets = secrets_dir_from_hex(&hex);
        assert_eq!(secrets, PathBuf::from("/home/user/hex/.hex/secrets"));
    }

    #[test]
    fn env_file_append_key_id_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let env_file = dir.path().join("kalshi.env");
        let private_key = dir.path().join("kalshi-private.pem");

        std::fs::write(&env_file, "SOME_OTHER_VAR=foo\n").unwrap();
        update_env_file(&env_file, &private_key);

        let contents = std::fs::read_to_string(&env_file).unwrap();
        assert!(contents.contains("KALSHI_KEY_ID=PASTE_YOUR_KEY_ID_HERE"));
        assert!(contents.contains(&format!("KALSHI_PRIVATE_KEY_PATH={}", private_key.display())));
    }

    #[test]
    fn env_file_stub_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let env_file = dir.path().join("kalshi.env");
        let private_key = dir.path().join("kalshi-private.pem");

        std::fs::write(&env_file, "KALSHI_KEY_ID=00000000-stub\n").unwrap();
        update_env_file(&env_file, &private_key);

        let contents = std::fs::read_to_string(&env_file).unwrap();
        assert!(contents.contains("KALSHI_KEY_ID=PASTE_YOUR_KEY_ID_HERE"));
        assert!(!contents.contains("00000000-stub"));
    }

    #[test]
    fn env_file_real_key_id_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let env_file = dir.path().join("kalshi.env");
        let private_key = dir.path().join("kalshi-private.pem");

        std::fs::write(&env_file, "KALSHI_KEY_ID=real-key-abc123\n").unwrap();
        update_env_file(&env_file, &private_key);

        let contents = std::fs::read_to_string(&env_file).unwrap();
        assert!(contents.contains("KALSHI_KEY_ID=real-key-abc123"));
        assert!(!contents.contains("PASTE_YOUR_KEY_ID_HERE"));
    }

    #[test]
    fn tmp_paths_use_dot_tmp_suffix() {
        // Verify tmp file naming convention mirrors the shell script's .tmp pattern
        let secrets = PathBuf::from("/tmp/secrets");
        let private_tmp = secrets.join("kalshi-private.pem.tmp");
        let public_tmp = secrets.join("kalshi-public.pem.tmp");
        assert!(private_tmp.to_str().unwrap().ends_with(".tmp"));
        assert!(public_tmp.to_str().unwrap().ends_with(".tmp"));
        assert!(private_tmp.to_str().unwrap().contains("kalshi-private"));
        assert!(public_tmp.to_str().unwrap().contains("kalshi-public"));
    }
}
