use std::path::PathBuf;

/// Resolve the hex workspace directory.
///
/// Resolution order:
///   1. $HEX_DIR environment variable
///   2. $HOME/hex (consistent with v2 install.sh default)
///   3. dirs::home_dir().join("hex") if $HOME is unset
///
/// Panics with a clear message if none of these resolve.
pub fn hex_dir() -> PathBuf {
    if let Ok(v) = std::env::var("HEX_DIR") {
        return PathBuf::from(v);
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join("hex");
    }
    if let Some(home) = dirs::home_dir() {
        return home.join("hex");
    }
    panic!("paths::hex_dir: cannot resolve hex workspace (no HEX_DIR, HOME, or home_dir)");
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hex_dir_uses_env_var_first() {
        // SAFETY: tests must serialize env var mutation; this single test sets+restores.
        let prev = std::env::var("HEX_DIR").ok();
        std::env::set_var("HEX_DIR", "/tmp/hex-paths-test");
        assert_eq!(hex_dir(), PathBuf::from("/tmp/hex-paths-test"));
        if let Some(p) = prev { std::env::set_var("HEX_DIR", p); } else { std::env::remove_var("HEX_DIR"); }
    }
}
