/// Port of:
///   system/scripts/validate-boi-spec.py  → hex validate boi-spec <file>
///   system/scripts/extension-validate.py → hex validate extension <path>
///   system/scripts/e2e-guard/verify.py   → hex validate e2e <url>
///
/// Commands:
///   hex validate boi-spec <file>      - Check a BOI spec for known anti-patterns
///   hex validate extension <path>     - Validate a hex extension manifest
///   hex validate e2e <url>            - HTTP-level E2E guard (browser tests skipped)
use std::path::Path;

// ---------------------------------------------------------------------------
// hex validate boi-spec
// ---------------------------------------------------------------------------

struct Check {
    id: &'static str,
    description: &'static str,
    severity: Severity,
    detect: fn(&[&str]) -> Vec<(usize, String)>,
}

#[derive(Clone, Copy, PartialEq)]
#[allow(dead_code)]
enum Severity {
    Error,
    Warn,
}

fn check_grep_perl(lines: &[&str]) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for (i, &line) in lines.iter().enumerate() {
        let lower = line.to_lowercase();
        if lower.contains("replaced") || lower.contains("already has") || lower.contains("fixed") {
            continue;
        }
        // grep followed by -P flag anywhere on the line
        if line.contains("grep") && line.contains("-P") {
            // crude but matches the python heuristic
            out.push((i + 1, line.to_string()));
        }
    }
    out
}

fn check_env_grep(lines: &[&str]) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for (i, &line) in lines.iter().enumerate() {
        if line.contains("env") && line.contains("| grep") || line.contains("|grep") {
            if line.contains("env") {
                out.push((i + 1, line.to_string()));
            }
        }
    }
    out
}

fn check_deprecated_models(lines: &[&str]) -> Vec<(usize, String)> {
    const DEPRECATED: &[&str] = &[
        "claude-3-5-haiku-20241022",
        "claude-3-haiku-20240307",
        "claude-3-5-sonnet-20241022",
        "claude-3-5-sonnet-20240620",
    ];
    let mut out = Vec::new();
    for (i, &line) in lines.iter().enumerate() {
        let lower = line.to_lowercase();
        if lower.contains("deprecated") || lower.contains("end-of-life") || lower.contains("eol") {
            continue;
        }
        for dep in DEPRECATED {
            if line.contains(dep) {
                out.push((i + 1, line.to_string()));
                break;
            }
        }
    }
    out
}

const CHECKS: &[Check] = &[
    Check {
        id: "GREP_PERL",
        description: "No grep -P (macOS BSD grep lacks Perl regex)",
        severity: Severity::Error,
        detect: check_grep_perl,
    },
    Check {
        id: "ENV_GREP",
        description: "No env | grep (leaks credentials in logs)",
        severity: Severity::Error,
        detect: check_env_grep,
    },
    Check {
        id: "DEPRECATED_MODEL",
        description: "No deprecated model IDs",
        severity: Severity::Error,
        detect: check_deprecated_models,
    },
];

pub fn run_boi_spec(files: &[String]) -> i32 {
    if files.is_empty() {
        eprintln!("Usage: hex validate boi-spec <file> [<file> ...]");
        return 2;
    }
    let mut overall = 0i32;
    for file in files {
        let path = Path::new(file);
        if !path.is_file() {
            eprintln!("ERROR: file not found: {file}");
            overall = overall.max(2);
            continue;
        }
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("ERROR: cannot read {file}: {e}");
                overall = overall.max(2);
                continue;
            }
        };
        let lines: Vec<&str> = content.lines().collect();
        let name = path.file_name().unwrap_or_default().to_string_lossy();

        let mut errors: Vec<(&Check, Vec<(usize, String)>)> = Vec::new();
        let mut warnings: Vec<(&Check, Vec<(usize, String)>)> = Vec::new();

        for check in CHECKS {
            let violations = (check.detect)(&lines);
            if violations.is_empty() {
                continue;
            }
            match check.severity {
                Severity::Error => errors.push((check, violations)),
                Severity::Warn => warnings.push((check, violations)),
            }
        }

        if errors.is_empty() && warnings.is_empty() {
            println!("PASS  {name} -- all checks passed");
        } else {
            if !errors.is_empty() {
                println!("FAIL  {name} -- {} violation(s) found\n", errors.len());
                for (check, viols) in &errors {
                    println!("  [{}] {}", check.id, check.description);
                    for (ln, text) in viols {
                        println!("    L{ln}: {text}");
                    }
                    println!();
                }
                overall = overall.max(1);
            }
            if !warnings.is_empty() {
                println!("WARN  {name} -- {} warning(s)\n", warnings.len());
                for (check, viols) in &warnings {
                    println!("  [{}] {}", check.id, check.description);
                    for (ln, text) in viols {
                        println!("    L{ln}: {text}");
                    }
                    println!();
                }
            }
        }
    }
    overall
}

// ---------------------------------------------------------------------------
// hex validate extension
// ---------------------------------------------------------------------------

pub fn run_extension(path: &str) -> i32 {
    let ext_path = Path::new(path);
    let manifest_path = if ext_path.is_dir() {
        ext_path.join("extension.yaml")
    } else {
        ext_path.to_path_buf()
    };

    let ext_label = manifest_path
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    println!("Validating extension: {ext_label}");
    println!("Manifest:  {}", manifest_path.display());
    println!();

    if !manifest_path.is_file() {
        eprintln!("  ✗  File not found: {}", manifest_path.display());
        println!("\nResult: INVALID (1 error(s))");
        return 1;
    }

    let content = match std::fs::read_to_string(&manifest_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("  ✗  Cannot read manifest: {e}");
            println!("\nResult: INVALID (1 error(s))");
            return 1;
        }
    };

    let mut errors: Vec<String> = Vec::new();
    let warnings: Vec<String> = Vec::new();

    // Required fields
    for field in &["name:", "version:", "description:", "type:"] {
        if !content.contains(field) {
            errors.push(format!("Missing required field: '{}'", field.trim_end_matches(':')));
        }
    }

    // type must be static | reactive | full
    if let Some(type_line) = content.lines().find(|l| l.trim_start().starts_with("type:")) {
        let val = type_line.split(':').nth(1).unwrap_or("").trim().trim_matches('"');
        if !matches!(val, "static" | "reactive" | "full") && !val.is_empty() {
            errors.push(format!("type must be one of [full, reactive, static], got: '{val}'"));
        }
    }

    // version must look like semver
    if let Some(ver_line) = content.lines().find(|l| l.trim_start().starts_with("version:")) {
        let val = ver_line.split(':').nth(1).unwrap_or("").trim().trim_matches('"');
        if !val.is_empty() {
            let parts: Vec<&str> = val.split('.').collect();
            let ok = parts.len() == 3 && parts.iter().all(|p| p.parse::<u32>().is_ok());
            if !ok {
                errors.push(format!("version must be semver (e.g. '1.0.0'), got: '{val}'"));
            }
        }
    }

    for line in &warnings {
        println!("  ⚠  {line}");
    }
    for line in &errors {
        println!("  ✗  {line}");
    }
    if errors.is_empty() && warnings.is_empty() {
        println!("  ✓  All checks passed");
    }

    println!();
    if !errors.is_empty() {
        println!("Result: INVALID ({} error(s), {} warning(s))", errors.len(), warnings.len());
        1
    } else if !warnings.is_empty() {
        println!("Result: VALID with {} warning(s)", warnings.len());
        0
    } else {
        println!("Result: VALID");
        0
    }
}

// ---------------------------------------------------------------------------
// hex validate e2e
// ---------------------------------------------------------------------------

pub fn run_e2e(url: &str, check_api: &str, check_sse: &str, timeout: u64) -> i32 {
    use std::time::{Duration, Instant};

    println!("E2E guard: {url}");
    println!();

    let mut passed = 0u32;
    let mut failed = 0u32;

    // Test: basic HTTP reachability
    let t0 = Instant::now();
    let reachable = probe_url(url, Duration::from_secs(timeout));
    let elapsed = t0.elapsed().as_millis();
    match reachable {
        Ok(status) if (200..400).contains(&status) => {
            println!("  PASS  page_loads ({status}) [{elapsed}ms]");
            passed += 1;
        }
        Ok(status) => {
            println!("  FAIL  page_loads (HTTP {status}) [{elapsed}ms]");
            failed += 1;
        }
        Err(e) => {
            println!("  FAIL  page_loads ({e}) [{elapsed}ms]");
            failed += 1;
        }
    }

    // Test: API health endpoint
    if !check_api.is_empty() {
        let api_url = join_url(url, check_api);
        let t0 = Instant::now();
        let result = probe_url(&api_url, Duration::from_secs(timeout));
        let elapsed = t0.elapsed().as_millis();
        match result {
            Ok(status) if (200..300).contains(&status) => {
                println!("  PASS  api_health ({status}) [{elapsed}ms]");
                passed += 1;
            }
            Ok(status) => {
                println!("  FAIL  api_health (HTTP {status}) [{elapsed}ms]");
                failed += 1;
            }
            Err(e) => {
                println!("  FAIL  api_health ({e}) [{elapsed}ms]");
                failed += 1;
            }
        }
    } else {
        println!("  SKIP  api_health (no --check-api)");
    }

    // Test: SSE endpoint emits at least one event
    if !check_sse.is_empty() {
        let sse_url = join_url(url, check_sse);
        let t0 = Instant::now();
        let result = probe_sse(&sse_url, Duration::from_secs(timeout));
        let elapsed = t0.elapsed().as_millis();
        match result {
            Ok(true) => {
                println!("  PASS  live_data (SSE event received) [{elapsed}ms]");
                passed += 1;
            }
            Ok(false) => {
                println!("  FAIL  live_data (no SSE events in {timeout}s) [{elapsed}ms]");
                failed += 1;
            }
            Err(e) => {
                println!("  FAIL  live_data ({e}) [{elapsed}ms]");
                failed += 1;
            }
        }
    } else {
        println!("  SKIP  live_data (no --check-sse)");
    }

    println!();
    let verdict = if failed == 0 { "PASS" } else { "FAIL" };
    println!("Verdict: {verdict} (passed={passed} failed={failed})");

    if failed > 0 { 1 } else { 0 }
}

fn probe_url(url: &str, timeout: std::time::Duration) -> Result<u16, String> {
    let output = std::process::Command::new("curl")
        .args([
            "-s",
            "-o", "/dev/null",
            "-w", "%{http_code}",
            "--max-time", &timeout.as_secs().to_string(),
            url,
        ])
        .output()
        .map_err(|e| e.to_string())?;
    let code_str = String::from_utf8_lossy(&output.stdout);
    code_str
        .trim()
        .parse::<u16>()
        .map_err(|_| format!("unexpected curl output: {code_str}"))
}

fn probe_sse(url: &str, timeout: std::time::Duration) -> Result<bool, String> {
    let output = std::process::Command::new("curl")
        .args([
            "-s",
            "-H", "Accept: text/event-stream",
            "--max-time", &timeout.as_secs().to_string(),
            url,
        ])
        .output()
        .map_err(|e| e.to_string())?;
    let body = String::from_utf8_lossy(&output.stdout);
    Ok(body.lines().any(|l| l.starts_with("data:")))
}

fn join_url(base: &str, path: &str) -> String {
    if path.starts_with('/') {
        // Strip scheme+host from base, replace path
        if let Some(rest) = base.split("://").nth(1) {
            if let Some(slash) = rest.find('/') {
                let host = &rest[..slash];
                let scheme = base.split("://").next().unwrap_or("http");
                return format!("{scheme}://{host}{path}");
            } else {
                let scheme = base.split("://").next().unwrap_or("http");
                return format!("{scheme}://{rest}{path}");
            }
        }
    }
    format!("{}/{}", base.trim_end_matches('/'), path.trim_start_matches('/'))
}
