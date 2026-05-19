/// Port of .hex/scripts/doctor-checks/codex.sh
///
/// Codex CLI + config health checks. Verifies:
///   1. codex binary on PATH
///   2. codex --version exits 0
///   3. OPENAI_API_KEY is set (env or ~/.hex-test.env)
///   4. AGENTS.md exists at $HEX_DIR/AGENTS.md
///   5. AGENTS.md contains all required sections
use chrono::{Duration, NaiveDate, Utc};
use regex::Regex;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::UNIX_EPOCH;

const AGENTS_MD_REQUIRED_SECTIONS: &[&str] = &[
    "Standing Orders",
    "Session Lifecycle",
    "BOI",
    "Memory",
];

fn pass(msg: &str) {
    println!("PASS  {}", msg);
}

fn warn(msg: &str) {
    println!("WARN  {}", msg);
}

fn error(msg: &str) {
    eprintln!("ERROR {}", msg);
}

fn info(msg: &str) {
    println!("INFO  {}", msg);
}

fn codex_on_path() -> bool {
    Command::new("which")
        .arg("codex")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn check_codex_1(failed: &mut bool) {
    if let Ok(output) = Command::new("which").arg("codex").output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            pass(&format!("codex.cli-on-path: found at {}", path));
            return;
        }
    }
    error("codex.cli-on-path: codex CLI not found — install with: npm install -g @openai/codex");
    *failed = true;
}

fn check_codex_2(failed: &mut bool) {
    if !codex_on_path() {
        info("codex.version-ok: skipped (codex not on PATH)");
        return;
    }
    match Command::new("codex").arg("--version").output() {
        Ok(o) if o.status.success() => {
            let ver = String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            pass(&format!("codex.version-ok: {}", ver));
        }
        _ => {
            error("codex.version-ok: 'codex --version' exited non-zero — fix: reinstall codex CLI");
            *failed = true;
        }
    }
}

fn check_codex_3(failed: &mut bool) {
    if std::env::var("OPENAI_API_KEY").map(|v| !v.is_empty()).unwrap_or(false) {
        pass("codex.api-key: OPENAI_API_KEY found via environment variable");
        return;
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let hex_test_env = PathBuf::from(&home).join(".hex-test.env");
    if hex_test_env.is_file() {
        if let Ok(text) = std::fs::read_to_string(&hex_test_env) {
            if text.lines().any(|l| l.starts_with("OPENAI_API_KEY=")) {
                pass("codex.api-key: OPENAI_API_KEY found via ~/.hex-test.env");
                return;
            }
        }
    }
    warn("codex.api-key: OPENAI_API_KEY not set — Codex will fail at runtime");
    info("  Fix: export OPENAI_API_KEY=sk-... in ~/.hex/scripts/env.sh or add to ~/.hex-test.env");
    *failed = true;
}

fn check_codex_4(hex_dir: &std::path::Path, failed: &mut bool) {
    let agents_md = hex_dir.join("AGENTS.md");
    if agents_md.is_file() {
        let size = std::fs::metadata(&agents_md).map(|m| m.len()).unwrap_or(0);
        pass(&format!("codex.agents-md-exists: AGENTS.md found ({} bytes)", size));
    } else {
        warn(&format!(
            "codex.agents-md-exists: AGENTS.md missing at {}",
            agents_md.display()
        ));
        info("  Fix: create AGENTS.md — Codex reads this as its primary instruction file");
        *failed = true;
    }
}

fn check_codex_5(hex_dir: &std::path::Path, failed: &mut bool) {
    let agents_md = hex_dir.join("AGENTS.md");
    if !agents_md.is_file() {
        info("codex.agents-md-complete: skipped (AGENTS.md not present)");
        return;
    }
    let text = match std::fs::read_to_string(&agents_md) {
        Ok(t) => t.to_lowercase(),
        Err(e) => {
            error(&format!("codex.agents-md-complete: cannot read AGENTS.md: {e}"));
            *failed = true;
            return;
        }
    };
    let missing: Vec<&str> = AGENTS_MD_REQUIRED_SECTIONS
        .iter()
        .filter(|&&section| !text.contains(&section.to_lowercase()))
        .copied()
        .collect();
    if missing.is_empty() {
        pass("codex.agents-md-complete: all required sections present");
    } else {
        warn(&format!(
            "codex.agents-md-complete: AGENTS.md missing sections: {}",
            missing.join(", ")
        ));
        info(&format!(
            "  Required sections: {}",
            AGENTS_MD_REQUIRED_SECTIONS.join(", ")
        ));
        *failed = true;
    }
}

pub fn check_codex(hex_dir: &std::path::Path) {
    let mut failed = false;
    check_codex_1(&mut failed);
    check_codex_2(&mut failed);
    check_codex_3(&mut failed);
    check_codex_4(hex_dir, &mut failed);
    check_codex_5(hex_dir, &mut failed);
    if failed {
        std::process::exit(1);
    }
}

/// Port of system/scripts/quality-check.py
/// Gaming detector for BOI initiative loop specs. Native Rust implementation.
pub fn quality_check(_hex_dir: &std::path::Path, spec: Option<&str>, sweep: bool, kr: Option<&str>) -> i32 {
    if let Some(spec_id) = spec {
        match serde_json::to_string_pretty(&qc_analyze_spec(spec_id)) {
            Ok(s) => println!("{}", s),
            Err(e) => eprintln!("quality-check: serialization error: {e}"),
        }
        return 0;
    }
    if sweep {
        match serde_json::to_string_pretty(&qc_sweep()) {
            Ok(s) => println!("{}", s),
            Err(e) => eprintln!("quality-check: serialization error: {e}"),
        }
        return 0;
    }
    if let Some(kr_ref) = kr {
        match serde_json::to_string_pretty(&qc_reality_check_kr(kr_ref)) {
            Ok(s) => println!("{}", s),
            Err(e) => eprintln!("quality-check: serialization error: {e}"),
        }
        return 0;
    }
    eprintln!("hex doctor quality-check: one of --spec, --sweep, or --kr is required");
    1
}

// ── Quality-check internals ──────────────────────────────────────────────────

fn qc_boi_queue() -> PathBuf { dirs_home().join(".boi/queue") }
fn qc_workspace() -> PathBuf { dirs_home().join("hex") }
fn qc_initiatives_dir() -> PathBuf { qc_workspace().join("initiatives") }
fn qc_events_dir() -> PathBuf { dirs_home().join(".hex-events/events") }
fn qc_github_mrap_base() -> PathBuf { dirs_home().join("github.com/mrap") }

const QC_ADMIN_KEYWORDS: &[&str] = &[
    "close", "add closed_at", "update status", "mark complete",
    "kr closure", "close kr", "initiative close", "admin", "housekeeping",
];
const QC_ADMIN_META_SIGNALS: &[&str] = &[
    "status", "yaml", "config", "field", "closed_at", "initiative", "kr",
];

fn qc_file_mtime(path: &Path) -> Option<f64> {
    fs::metadata(path).ok()?.modified().ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
}

fn qc_is_trivially_gameable(cmd: &str) -> (bool, String) {
    if cmd.is_empty() { return (false, String::new()); }
    let s = cmd.trim();
    let trivials: &[&str] = &[
        r#"(?m)^\s*echo\s+[\d.]+\s*$"#,
        r#"(?i)echo\s+['"]?UNMEASURABLE"#,
        r#"\bexit\s+1\b"#,
        r#"(?m)^\s*echo\s+0\s*$"#,
        r#"(?m)^\s*echo\s+1\s*$"#,
        r#"(?m)^\s*echo\s+100\s*$"#,
    ];
    for pat in trivials {
        if Regex::new(pat).map(|r| r.is_match(s)).unwrap_or(false) {
            let preview = &s[..s.len().min(80)];
            return (true, format!("constant/trivial metric command: {:?}", preview));
        }
    }
    if Regex::new(r"(?i)manual.verif|manual.check|echo.*manual")
        .map(|r| r.is_match(s)).unwrap_or(false)
    {
        return (true, "manual verification placeholder — not a runnable metric".to_string());
    }
    (false, String::new())
}

fn qc_parse_metadata(content: &str) -> (String, String) {
    let mut title = String::new();
    let mut mode = String::new();
    for line in content.lines() {
        let s = line.trim();
        if s.starts_with("title:") && title.is_empty() {
            title = s[6..].trim().trim_matches(|c| c == '"' || c == '\'').to_string();
        } else if s.starts_with("mode:") && mode.is_empty() {
            mode = s[5..].trim().trim_matches(|c| c == '"' || c == '\'').to_string();
        }
    }
    (title, mode)
}

fn qc_classify(title: &str, mode: &str) -> (&'static str, String) {
    let tl = title.to_lowercase();
    let ml = mode.to_lowercase();
    for kw in QC_ADMIN_KEYWORDS {
        if tl.contains(kw) {
            return ("admin-closure", format!("title contains admin keyword: '{}'", kw));
        }
    }
    if ml == "update" || ml == "patch" {
        for sig in QC_ADMIN_META_SIGNALS {
            if tl.contains(sig) {
                return ("admin-closure", format!("mode={:?} with metadata title signal: '{}'", ml, sig));
            }
        }
    }
    for pat in ["adding closed_at", "updating initiative yaml"] {
        if tl.contains(pat) {
            return ("admin-closure", format!("title mentions: '{}'", pat));
        }
    }
    ("code", "no admin-closure indicators found".to_string())
}

fn qc_is_drive_kr(title: &str) -> bool {
    Regex::new(r"(?i)Drive KR to Non-Zero|drive.*kr.*non-zero|highest-leverage action for kr")
        .map(|r| r.is_match(title)).unwrap_or(false)
}

fn qc_extract_metric_cmd(content: &str) -> Option<String> {
    Regex::new(r"(?s)Metric command:\s*```\s*\n(.*?)\n```").ok()?
        .captures(content)
        .map(|c| c[1].trim().to_string())
}

fn qc_extract_verify_cmd(content: &str) -> Option<String> {
    if let Some(c) = Regex::new(r"\*\*Verify:\*\*\s*`([^`]+)`").ok()?.captures(content) {
        return Some(c[1].to_string());
    }
    Regex::new(r"(?s)\*\*Verify:\*\*\s*(.*?)(?:\n\n|\z)").ok()?
        .captures(content)
        .map(|c| c[1].trim().to_string())
}

fn qc_spec_duration(spec_id: &str) -> Option<f64> {
    let q = qc_boi_queue();
    let start = qc_file_mtime(&q.join(format!("{}.prompt.md", spec_id)))?;
    let end = qc_file_mtime(&q.join(format!("{}.exit", spec_id)))?;
    Some(end - start)
}

fn qc_dispatch_commit(dispatch_ts_secs: f64, repo: &Path) -> Option<String> {
    let ts = chrono::DateTime::<Utc>::from_timestamp(dispatch_ts_secs as i64, 0)?
        .format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let out = Command::new("git")
        .args(["log", &format!("--before={}", ts), "-1", "--format=%H"])
        .current_dir(repo)
        .output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

fn qc_git_diff_names(repo: &Path, git_ref: &str) -> Vec<String> {
    Command::new("git")
        .args(["diff", "--name-only", git_ref])
        .current_dir(repo)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout)
            .lines().filter(|l| !l.is_empty()).map(str::to_owned).collect())
        .unwrap_or_default()
}

fn qc_filter_by_window(repo: &Path, files: &[String], start: Option<f64>, end: Option<f64>) -> Vec<String> {
    files.iter().filter(|f| {
        let p = repo.join(f);
        if !p.exists() { return false; }
        if let (Some(st), Some(et)) = (start, end) {
            qc_file_mtime(&p).map(|mt| mt >= st - 60.0 && mt <= et + 1800.0).unwrap_or(false)
        } else {
            true
        }
    }).cloned().collect()
}

fn qc_untracked_in_window(repo: &Path, start: Option<f64>, end: Option<f64>) -> Vec<serde_json::Value> {
    let out = Command::new("git").args(["status", "--short"]).current_dir(repo)
        .output().unwrap_or_else(|_| return std::process::Output {
            status: std::process::ExitStatus::default(),
            stdout: vec![], stderr: vec![],
        });
    String::from_utf8_lossy(&out.stdout).lines()
        .filter(|l| l.starts_with("?? "))
        .filter_map(|l| {
            let rel = l[3..].trim().to_string();
            let fpath = repo.join(&rel);
            if !fpath.exists() || fpath.is_dir() { return None; }
            if let (Some(st), Some(et)) = (start, end) {
                let mt = qc_file_mtime(&fpath)?;
                if mt < st - 60.0 || mt > et + 1800.0 { return None; }
            }
            Some(serde_json::json!({"type": "untracked", "file": rel, "source": "untracked"}))
        })
        .collect()
}

fn qc_scan_repo(repo: &Path, start: Option<f64>, end: Option<f64>) -> (Vec<serde_json::Value>, Vec<String>) {
    let label = repo.file_name().and_then(|n| n.to_str()).unwrap_or("?").to_string();
    let mut changes: Vec<serde_json::Value> = vec![];
    let mut evidence: Vec<String> = vec![];

    let dispatch_commit = start.and_then(|st| qc_dispatch_commit(st, repo));
    let git_ref = dispatch_commit.as_deref()
        .map(|c| format!("{}..HEAD", c))
        .unwrap_or_else(|| "HEAD".to_string());
    if dispatch_commit.is_none() {
        evidence.push(format!("[{}] dispatch-time commit unavailable, using HEAD diff", label));
    }

    let all_changed = qc_git_diff_names(repo, &git_ref);
    let in_window = qc_filter_by_window(repo, &all_changed, start, end);

    let code_files: Vec<_> = in_window.iter()
        .filter(|f| ["py","rs","sh","js","ts"].iter().any(|e| f.ends_with(&format!(".{}", e))))
        .collect();
    let doc_files: Vec<_> = in_window.iter().filter(|f| f.ends_with(".md")).collect();

    if !code_files.is_empty() {
        evidence.push(format!("[{}] code files changed: {:?}", label, code_files));
        for f in &code_files {
            changes.push(serde_json::json!({"type": "code", "file": f, "repo": label}));
        }
    }
    if !doc_files.is_empty() {
        evidence.push(format!("[{}] doc files changed: {:?}", label, doc_files));
        for f in &doc_files {
            changes.push(serde_json::json!({"type": "doc", "file": f, "repo": label}));
        }
    }

    let untracked = qc_untracked_in_window(repo, start, end);
    if !untracked.is_empty() {
        let names: Vec<_> = untracked.iter()
            .filter_map(|u| u["file"].as_str()).collect();
        evidence.push(format!("[{}] untracked files: {:?}", label, names));
        changes.extend(untracked);
    }
    (changes, evidence)
}

fn qc_scan_cross_repos_in_window(start: f64, end: Option<f64>) -> Vec<PathBuf> {
    let base = qc_github_mrap_base();
    if !base.exists() { return vec![]; }
    let since_ts = chrono::DateTime::<Utc>::from_timestamp((start - 60.0) as i64, 0)
        .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default();
    let until_val = end.unwrap_or(start + 7200.0);
    let until_ts = chrono::DateTime::<Utc>::from_timestamp(until_val as i64, 0)
        .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default();

    fs::read_dir(&base).ok().into_iter().flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.join(".git").exists())
        .filter(|p| {
            Command::new("git")
                .args(["log", &format!("--since={}", since_ts), &format!("--until={}", until_ts), "--oneline"])
                .current_dir(p)
                .output()
                .map(|o| !o.stdout.is_empty())
                .unwrap_or(false)
        })
        .collect()
}

fn qc_parse_initiative_yaml(raw: &str) -> Vec<HashMap<String, serde_json::Value>> {
    let mut krs: Vec<HashMap<String, serde_json::Value>> = vec![];
    let mut current: Option<HashMap<String, serde_json::Value>> = None;
    let mut in_krs = false;
    let mut in_metric = false;

    for line in raw.lines() {
        let stripped = line.trim();
        if stripped == "key_results:" {
            in_krs = true;
        } else if in_krs && stripped.starts_with("- id:") {
            if let Some(kr) = current.take() { krs.push(kr); }
            let mut m = HashMap::new();
            m.insert("id".into(), serde_json::Value::String(
                stripped[5..].trim().trim_matches(|c| c == '"' || c == '\'').to_string()
            ));
            m.insert("metric".into(), serde_json::json!({}));
            current = Some(m);
        } else if in_krs {
            if let Some(ref mut kr) = current {
                if stripped.starts_with("description:") {
                    kr.insert("description".into(), serde_json::Value::String(
                        stripped[12..].trim().trim_matches(|c| c == '"' || c == '\'').to_string()
                    ));
                } else if stripped.starts_with("target:") {
                    let v = stripped[7..].trim();
                    kr.insert("target".into(), v.parse::<f64>()
                        .map(serde_json::Value::from)
                        .unwrap_or_else(|_| serde_json::Value::String(v.to_string())));
                } else if stripped.starts_with("current:") {
                    let v = stripped[8..].trim();
                    kr.insert("current".into(), v.parse::<f64>()
                        .map(serde_json::Value::from)
                        .unwrap_or_else(|_| serde_json::Value::String(v.to_string())));
                } else if stripped.starts_with("status:") {
                    kr.insert("status".into(), serde_json::Value::String(
                        stripped[7..].trim().trim_matches(|c| c == '"' || c == '\'').to_string()
                    ));
                } else if stripped == "metric:" {
                    in_metric = true;
                } else if in_metric && stripped.starts_with("command:") {
                    let cmd = stripped[8..].trim().trim_matches(|c| c == '"' || c == '\'').to_string();
                    if let Some(metric) = kr.get_mut("metric") {
                        if let Some(obj) = metric.as_object_mut() {
                            obj.insert("command".into(), serde_json::Value::String(cmd));
                        }
                    }
                } else if in_metric && stripped.starts_with("direction:") {
                    let dir = stripped[10..].trim().trim_matches(|c| c == '"' || c == '\'').to_string();
                    if let Some(metric) = kr.get_mut("metric") {
                        if let Some(obj) = metric.as_object_mut() {
                            obj.insert("direction".into(), serde_json::Value::String(dir));
                        }
                    }
                    in_metric = false;
                }
            }
        }
    }
    if let Some(kr) = current { krs.push(kr); }
    krs
}

fn qc_find_kr(init_id: &str, kr_id: &str) -> Option<serde_json::Value> {
    let name = init_id.replace("init-", "");
    let inits_dir = qc_initiatives_dir();
    let candidates = [init_id.to_string(), name.clone(), format!("init-{}", name)];
    for candidate in &candidates {
        let path = inits_dir.join(format!("{}.yaml", candidate));
        if let Ok(raw) = fs::read_to_string(&path) {
            let krs = qc_parse_initiative_yaml(&raw);
            for kr in krs {
                if kr.get("id").and_then(|v| v.as_str()) == Some(kr_id) {
                    let mut out = serde_json::json!(kr);
                    let raw_cmd = Regex::new(&format!(r"(?s)id:\s*{}\b.*?command:\s*(.*?)(?:\n\s+direction:|\n\s*target:|\n\s*current:|\z)", regex::escape(kr_id)))
                        .ok()
                        .and_then(|re| re.captures(&raw))
                        .map(|c| c[1].trim().trim_matches(|c| c == '"' || c == '\'').to_string())
                        .unwrap_or_default();
                    out["_raw_command"] = serde_json::Value::String(raw_cmd);
                    out["_initiative_id"] = serde_json::Value::String(init_id.to_string());
                    return Some(out);
                }
            }
        }
    }
    None
}

fn qc_kr_lower_better_math_error(kr: &serde_json::Value) -> bool {
    let direction = kr["metric"]["direction"].as_str().unwrap_or("higher_is_better");
    if direction != "lower_is_better" { return false; }
    if kr["status"].as_str() != Some("met") { return false; }
    let current = kr["current"].as_f64().unwrap_or(f64::NAN);
    let target = kr["target"].as_f64().unwrap_or(f64::NAN);
    current > target
}

fn qc_analyze_spec(spec_id: &str) -> serde_json::Value {
    let q = qc_boi_queue();
    let spec_path = q.join(format!("{}.spec.md", spec_id));
    let content = match fs::read_to_string(&spec_path) {
        Ok(c) => c,
        Err(_) => return serde_json::json!({
            "spec_id": spec_id,
            "verdict": "UNKNOWN",
            "evidence": [format!("Spec file not found: {}", spec_path.display())],
            "files_changed": [],
            "metric_changes": [],
            "code_changes": [],
        }),
    };

    let mut evidence: Vec<String> = vec![];
    let mut metric_changes: Vec<serde_json::Value> = vec![];
    let mut code_changes: Vec<serde_json::Value> = vec![];
    let mut gaming_signals: i32 = 0;
    let mut real_signals: i32 = 0;

    let (title, mode) = qc_parse_metadata(&content);
    let (spec_type, classification_reason) = qc_classify(&title, &mode);
    evidence.push(format!("spec classification: {} ({})", spec_type, classification_reason));

    if spec_type == "admin-closure" {
        return serde_json::json!({
            "spec_id": spec_id,
            "verdict": "ADMIN",
            "spec_type": "admin-closure",
            "classification_reason": classification_reason,
            "gaming_signals": 0,
            "real_signals": 0,
            "evidence": evidence,
            "files_changed": [],
            "metric_changes": [],
            "code_changes": [],
            "is_drive_kr": false,
            "duration_seconds": serde_json::Value::Null,
        });
    }

    let is_drive_kr = qc_is_drive_kr(&title);
    if is_drive_kr {
        evidence.push("spec type: Drive KR to Non-Zero (high-risk template)".to_string());
        gaming_signals += 1;
    }

    let embedded_metric = qc_extract_metric_cmd(&content);
    if let Some(ref em) = embedded_metric {
        let (gamed, reason) = qc_is_trivially_gameable(em);
        if gamed {
            evidence.push(format!("embedded metric was trivially gameable: {}", reason));
            metric_changes.push(serde_json::json!({"type": "trivial_metric", "command": &em[..em.len().min(100)]}));
            gaming_signals += 2;
        }
    }

    let verify_cmd = qc_extract_verify_cmd(&content);
    if let (Some(ref vc), Some(ref em)) = (&verify_cmd, &embedded_metric) {
        if vc.contains(&em.trim()[..em.trim().len().min(30)]) {
            evidence.push("verify command re-runs same metric — can be gamed by metric rewrite".to_string());
            gaming_signals += 1;
        }
    }
    if let Some(ref vc) = verify_cmd {
        let (gamed, reason) = qc_is_trivially_gameable(vc);
        if gamed {
            evidence.push(format!("verify command is trivially passable: {}", reason));
            gaming_signals += 2;
        }
    }

    let duration = qc_spec_duration(spec_id);
    if let Some(dur) = duration {
        if dur < 300.0 && is_drive_kr {
            evidence.push(format!("completed in {:.0}s (<5min) for a build spec", dur));
            gaming_signals += 1;
        } else if dur > 0.0 {
            evidence.push(format!("completion time: {:.0}s", dur));
            if dur > 600.0 { real_signals += 1; }
        }
    }

    let workspace = qc_workspace();
    let mut files_changed: Vec<String> = vec![];
    let prompt_mtime = qc_file_mtime(&q.join(format!("{}.prompt.md", spec_id)));
    let exit_mtime = qc_file_mtime(&q.join(format!("{}.exit", spec_id)));

    if workspace.exists() {
        let dispatch_commit = prompt_mtime.and_then(|st| qc_dispatch_commit(st, &workspace));
        let git_ref = dispatch_commit.as_deref()
            .map(|c| format!("{}..HEAD", c))
            .unwrap_or_else(|| "HEAD".to_string());
        if dispatch_commit.is_none() {
            evidence.push("warning: dispatch-time commit unavailable, falling back to HEAD diff".to_string());
        }
        let all_changed = qc_git_diff_names(&workspace, &git_ref);
        let in_window = qc_filter_by_window(&workspace, &all_changed, prompt_mtime, exit_mtime);
        files_changed = in_window.clone();

        let code_files: Vec<_> = in_window.iter()
            .filter(|f| ["py","rs","sh","js","ts"].iter().any(|e| f.ends_with(&format!(".{}", e))))
            .collect();
        let doc_files: Vec<_> = in_window.iter().filter(|f| f.ends_with(".md")).collect();
        let init_yaml: Vec<_> = in_window.iter()
            .filter(|f| f.starts_with("initiatives/") || f.starts_with("experiments/"))
            .collect();

        if !init_yaml.is_empty() && code_files.is_empty() {
            evidence.push(format!("only initiative/experiment YAML files changed: {:?}", init_yaml));
            for f in &init_yaml {
                metric_changes.push(serde_json::json!({"type": "yaml_only", "file": f}));
            }
            gaming_signals += 2;
        } else if !code_files.is_empty() {
            evidence.push(format!("real code files changed: {:?}", code_files));
            for f in &code_files {
                code_changes.push(serde_json::json!({"type": "code", "file": f}));
            }
            real_signals += code_files.len() as i32;
        } else if !doc_files.is_empty() {
            evidence.push(format!("doc files changed: {:?}", doc_files));
            real_signals += 1;
        }

        let untracked = qc_untracked_in_window(&workspace, prompt_mtime, exit_mtime);
        if !untracked.is_empty() {
            let names: Vec<_> = untracked.iter().filter_map(|u| u["file"].as_str()).collect();
            evidence.push(format!("untracked files modified during spec window: {:?}", names));
            code_changes.extend(untracked.clone());
            real_signals += untracked.len() as i32;
        }
    }

    if let Some(st) = prompt_mtime {
        let cross_repos = qc_scan_cross_repos_in_window(st, exit_mtime);
        for repo in &cross_repos {
            if repo == &workspace { continue; }
            let (changes, ev) = qc_scan_repo(repo, prompt_mtime, exit_mtime);
            evidence.extend(ev);
            if !changes.is_empty() {
                let label = repo.file_name().and_then(|n| n.to_str()).unwrap_or("?");
                evidence.push(format!("cross-repo scan found changes in {}", label));
                real_signals += changes.len() as i32;
                code_changes.extend(changes);
            }
        }
    }

    let verdict = if gaming_signals >= 4 { "GAMING" }
        else if gaming_signals >= 2 { "SUSPECT" }
        else if real_signals >= 2 { "LEGITIMATE" }
        else { "UNKNOWN" };

    serde_json::json!({
        "spec_id": spec_id,
        "verdict": verdict,
        "gaming_signals": gaming_signals,
        "real_signals": real_signals,
        "evidence": evidence,
        "files_changed": files_changed,
        "metric_changes": metric_changes,
        "code_changes": code_changes,
        "is_drive_kr": is_drive_kr,
        "duration_seconds": duration,
    })
}

fn qc_sweep() -> serde_json::Value {
    let q = qc_boi_queue();
    let cutoff = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH).unwrap_or_default().as_secs_f64() - 86400.0;

    let spec_ids: Vec<String> = fs::read_dir(&q).ok().into_iter().flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("exit"))
        .filter(|e| qc_file_mtime(&e.path()).map(|m| m >= cutoff).unwrap_or(false))
        .filter_map(|e| {
            let stem = e.path().file_stem()?.to_str()?.to_string();
            if q.join(format!("{}.spec.md", stem)).exists() { Some(stem) } else { None }
        })
        .collect();

    let mut results: Vec<serde_json::Value> = vec![];
    let (mut gaming, mut suspect, mut legitimate, mut unknown) = (0i32, 0i32, 0i32, 0i32);

    for sid in &spec_ids {
        let r = qc_analyze_spec(sid);
        match r["verdict"].as_str().unwrap_or("UNKNOWN") {
            "GAMING"     => { gaming += 1;     qc_emit_gaming_event(&r); }
            "SUSPECT"    => { suspect += 1; }
            "LEGITIMATE" => { legitimate += 1; }
            _            => { unknown += 1; }
        }
        results.push(r);
    }

    let total = spec_ids.len() as f64;
    serde_json::json!({
        "total": spec_ids.len(),
        "gaming": gaming,
        "suspect": suspect,
        "legitimate": legitimate,
        "unknown": unknown,
        "gaming_rate_pct": if total > 0.0 { (gaming as f64 / total * 100.0 * 10.0).round() / 10.0 } else { 0.0 },
        "sweep_time": Utc::now().to_rfc3339(),
        "specs": results,
    })
}

fn qc_emit_gaming_event(result: &serde_json::Value) {
    let events_dir = qc_events_dir();
    if fs::create_dir_all(&events_dir).is_err() { return; }
    let ts = Utc::now().format("%Y%m%dT%H%M%S").to_string();
    let spec_id = result["spec_id"].as_str().unwrap_or("unknown");
    let event_file = events_dir.join(format!("quality-gaming-{}-{}.json", spec_id, ts));
    let event = serde_json::json!({
        "event": "hex.quality.gaming.detected",
        "ts": Utc::now().to_rfc3339(),
        "spec_id": spec_id,
        "evidence": result["evidence"],
        "gaming_signals": result["gaming_signals"],
    });
    let _ = fs::write(&event_file, serde_json::to_string_pretty(&event).unwrap_or_default());
}

fn qc_reality_check_kr(kr_ref: &str) -> serde_json::Value {
    let parts: Vec<&str> = kr_ref.trim_matches('/').splitn(2, '/').collect();
    if parts.len() != 2 {
        return serde_json::json!({"kr_id": kr_ref, "error": "format must be <init-id>/<kr-id>"});
    }
    let (init_id, kr_id) = (parts[0], parts[1]);
    let kr = match qc_find_kr(init_id, kr_id) {
        Some(k) => k,
        None => return serde_json::json!({"kr_id": kr_ref, "error": format!("KR not found: {}/{}", init_id, kr_id)}),
    };

    let claimed_value = kr["current"].clone();
    let claimed_status = kr["status"].as_str().unwrap_or("open").to_string();
    let target = kr["target"].clone();
    let direction = kr["metric"]["direction"].as_str().unwrap_or("higher_is_better").to_string();
    let description = kr["description"].as_str().unwrap_or("").to_string();
    let metric_cmd = kr["_raw_command"].as_str()
        .or_else(|| kr["metric"]["command"].as_str())
        .unwrap_or("").to_string();

    let mut evidence: Vec<String> = vec![];
    let mut independent_check_value: Option<f64> = None;
    let mut match_result: Option<bool> = None;
    let mut fraud_detected = false;

    let (is_gamed, reason) = qc_is_trivially_gameable(&metric_cmd);
    if is_gamed {
        evidence.push(format!("metric command is trivially gameable: {}", reason));
        fraud_detected = true;
    }

    if qc_kr_lower_better_math_error(&kr) {
        evidence.push(format!(
            "MATH ERROR: lower_is_better but current={} > target={}, yet status=met",
            claimed_value, target
        ));
        fraud_detected = true;
        independent_check_value = claimed_value.as_f64();
        match_result = Some(false);
    }

    if !is_gamed && !metric_cmd.is_empty() && !fraud_detected {
        let workspace = qc_workspace();
        let run = Command::new("bash")
            .args(["-c", &format!("cd {} && {}", workspace.display(), metric_cmd)])
            .output();
        match run {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !stdout.is_empty() {
                    if let Ok(val) = stdout.split_whitespace().last().unwrap_or("").parse::<f64>() {
                        independent_check_value = Some(val);
                        if let Some(claimed_f) = claimed_value.as_f64() {
                            let diff = (val - claimed_f).abs();
                            let rel_diff = diff / claimed_f.abs().max(1.0);
                            match_result = Some(rel_diff <= 0.05);
                            if match_result == Some(false) {
                                evidence.push(format!(
                                    "independent measurement {} differs from claimed {} by {:.1}%",
                                    val, claimed_f, rel_diff * 100.0
                                ));
                            } else {
                                evidence.push(format!(
                                    "independent measurement {} matches claimed {}",
                                    val, claimed_f
                                ));
                            }
                        }
                    } else {
                        evidence.push(format!("could not parse metric output: {:?}", &stdout[..stdout.len().min(100)]));
                    }
                } else {
                    evidence.push(format!("metric command produced no output (exit={})", out.status));
                    if !out.status.success() {
                        fraud_detected = true;
                        evidence.push("metric command failed — claimed value may be stale/false".to_string());
                    }
                }
            }
            Err(e) => evidence.push(format!("error running metric: {}", e)),
        }
    }

    if claimed_status == "met" && !fraud_detected {
        if let (Some(iv), Some(tgt)) = (independent_check_value, target.as_f64()) {
            if direction == "higher_is_better" && iv < tgt {
                evidence.push(format!("status=met but independent check {} < target {}", iv, tgt));
                fraud_detected = true;
            } else if direction == "lower_is_better" && iv > tgt {
                evidence.push(format!("status=met but independent check {} > target {}", iv, tgt));
                fraud_detected = true;
            }
        }
    }

    let verdict = if fraud_detected { "SUSPECT" }
        else if match_result == Some(true) { "VERIFIED" }
        else { "UNVERIFIED" };

    serde_json::json!({
        "kr_id": kr_ref,
        "description": description,
        "claimed_value": claimed_value,
        "claimed_status": claimed_status,
        "target": target,
        "direction": direction,
        "independent_check_value": independent_check_value,
        "match": match_result,
        "fraud_detected": fraud_detected,
        "verdict": verdict,
        "evidence": evidence,
        "metric_command_preview": if metric_cmd.is_empty() { None } else { Some(&metric_cmd[..metric_cmd.len().min(120)]) },
    })
}

/// Port of system/scripts/stale_deps.py
/// Scans todo.md and recent landings for dependency-blocked items older than threshold.
pub fn stale_deps(hex_dir: &Path, threshold_days: u32, json_output: bool) -> i32 {
    let dependency_markers = Regex::new(
        r"(?i)(waiting on|blocked by|pending response|need(?:s|ing)? response|awaiting|waiting for|depends on|need(?:s)? from)"
    ).expect("regex compiles");

    let mut all_items: Vec<(String, String)> = vec![]; // (text, source)

    let todo_path = hex_dir.join("todo.md");
    if todo_path.is_file() {
        if let Ok(text) = fs::read_to_string(&todo_path) {
            for line in text.lines() {
                let stripped = line.trim();
                if stripped.is_empty() { continue; }
                if dependency_markers.is_match(stripped) {
                    let clean = Regex::new(r"^[-*\[\]x ]+").unwrap().replace(stripped, "").trim().to_string();
                    if clean.len() > 10 {
                        all_items.push((clean, "todo.md".to_string()));
                    }
                }
            }
        }
    }

    let landings_dir = hex_dir.join("landings");
    if landings_dir.is_dir() {
        let mut landing_files: Vec<PathBuf> = fs::read_dir(&landings_dir)
            .ok().into_iter().flatten()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("md"))
            .filter(|p| p.file_name().and_then(|n| n.to_str())
                .map(|n| n.len() == 13 && n[..4].parse::<u32>().is_ok())
                .unwrap_or(false))
            .collect();
        landing_files.sort_by(|a, b| b.cmp(a));
        for lf in landing_files.iter().take(3) {
            if let Ok(text) = fs::read_to_string(lf) {
                let src = format!("landings/{}", lf.file_name().unwrap().to_string_lossy());
                for line in text.lines() {
                    let stripped = line.trim();
                    if stripped.is_empty() { continue; }
                    if dependency_markers.is_match(stripped) {
                        let clean = Regex::new(r"^[-*\[\]x ]+").unwrap().replace(stripped, "").trim().to_string();
                        if clean.len() > 10 {
                            all_items.push((clean, src.clone()));
                        }
                    }
                }
            }
        }
    }

    let tracker_path = hex_dir.join(".claude/dependency-tracker.json");
    let mut state: serde_json::Value = if tracker_path.is_file() {
        fs::read_to_string(&tracker_path).ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| serde_json::json!({"items": {}, "last_scan": null}))
    } else {
        serde_json::json!({"items": {}, "last_scan": null})
    };

    let today = Utc::now().format("%Y-%m-%d").to_string();
    let items_map = state["items"].as_object_mut().expect("items is object");

    let mut current_keys: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (text, source) in &all_items {
        let key: String = text[..text.len().min(80)].to_lowercase()
            .split_whitespace().collect::<Vec<_>>().join(" ");
        current_keys.insert(key.clone());
        items_map.entry(key.clone()).or_insert_with(|| serde_json::json!({
            "text": text,
            "source": source,
            "first_seen": today,
            "last_seen": today,
        }));
        if let Some(entry) = items_map.get_mut(&key) {
            entry["last_seen"] = serde_json::Value::String(today.clone());
            entry["source"] = serde_json::Value::String(source.clone());
        }
    }

    let resolved: Vec<String> = items_map.keys()
        .filter(|k| !current_keys.contains(*k))
        .cloned()
        .collect();
    for k in resolved { items_map.remove(&k); }

    state["last_scan"] = serde_json::Value::String(today.clone());

    let mut stale: Vec<serde_json::Value> = vec![];
    for (_key, info) in state["items"].as_object().unwrap() {
        let first_seen_str = info["first_seen"].as_str().unwrap_or(&today);
        if let Ok(first_seen) = NaiveDate::parse_from_str(first_seen_str, "%Y-%m-%d") {
            let age_days = (Utc::now().date_naive() - first_seen).num_days();
            if age_days >= threshold_days as i64 {
                stale.push(serde_json::json!({
                    "text": info["text"],
                    "source": info["source"],
                    "first_seen": first_seen_str,
                    "days_stale": age_days,
                }));
            }
        }
    }
    stale.sort_by(|a, b| b["days_stale"].as_i64().cmp(&a["days_stale"].as_i64()));

    if let Some(parent) = tracker_path.parent() { let _ = fs::create_dir_all(parent); }
    let tmp = tracker_path.with_extension("json.tmp");
    if let Ok(s) = serde_json::to_string_pretty(&state) {
        let _ = fs::write(&tmp, s);
        let _ = fs::rename(&tmp, &tracker_path);
    }

    if json_output {
        let total = state["items"].as_object().map(|m| m.len()).unwrap_or(0);
        println!("{}", serde_json::json!({"stale": stale, "total_tracked": total}));
    } else if stale.is_empty() {
        let total = state["items"].as_object().map(|m| m.len()).unwrap_or(0);
        println!("No stale dependencies (threshold: {threshold_days} days, tracking {total} items).");
    } else {
        println!("STALE DEPENDENCIES ({} items past {threshold_days}-day threshold):", stale.len());
        println!();
        for item in &stale {
            println!("  [{}d] {}", item["days_stale"], item["text"].as_str().unwrap_or(""));
            println!("       Source: {} | First seen: {}",
                item["source"].as_str().unwrap_or(""),
                item["first_seen"].as_str().unwrap_or(""));
            println!();
        }
    }
    0
}

/// Port of system/scripts/detect-failure-pattern.py
/// Detects three-strike failure patterns in the BOI SQLite DB.
pub fn detect_failure_pattern(window_seconds: u64, spec_id: Option<&str>) -> i32 {
    let db_path = dirs_home().join(".boi/boi-rust.db");
    if !db_path.is_file() {
        eprintln!("[detect-failure-pattern] DB not found: {}", db_path.display());
        return 0;
    }

    let conn = match rusqlite::Connection::open(&db_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[detect-failure-pattern] cannot open DB: {e}");
            return 1;
        }
    };

    let cutoff = (Utc::now() - Duration::seconds(window_seconds as i64))
        .to_rfc3339();

    let mut stmt = match conn.prepare(
        "SELECT id, title, completed_at, error FROM specs WHERE status = 'failed' AND completed_at >= ? ORDER BY completed_at DESC"
    ) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[detect-failure-pattern] query error: {e}");
            return 1;
        }
    };

    let failures: Vec<(String, String, String)> = stmt.query_map([&cutoff], |row| {
        Ok((
            row.get::<_, String>(0).unwrap_or_default(),
            row.get::<_, String>(1).unwrap_or_default(),
            row.get::<_, String>(3).unwrap_or_default(),
        ))
    }).ok().into_iter().flatten()
      .filter_map(|r| r.ok())
      .collect();

    const THRESHOLD: usize = 3;
    let window_hours = window_seconds / 3600;

    let mut by_kind: HashMap<String, Vec<String>> = HashMap::new();
    let mut by_title: HashMap<String, Vec<String>> = HashMap::new();
    for (id, title, error_raw) in &failures {
        let kind = serde_json::from_str::<serde_json::Value>(error_raw)
            .ok()
            .and_then(|v| v.get("kind").and_then(|k| k.as_str()).map(|s| s.to_string()))
            .unwrap_or_else(|| if error_raw.is_empty() { "Unknown".to_string() } else { error_raw.clone() });
        by_kind.entry(kind).or_default().push(id.clone());
        if !title.is_empty() {
            let norm = title.replace('\u{2014}', "-").replace('\u{2013}', "-");
            by_title.entry(norm).or_default().push(id.clone());
        }
    }

    for (kind, ids) in &by_kind {
        if ids.len() >= THRESHOLD {
            if let Some(sid) = spec_id { if !ids.contains(&sid.to_string()) { continue; } }
            let p = serde_json::json!({
                "pattern_type": "failure_kind",
                "key": kind,
                "description": format!("Failure kind '{kind}' fired {}x in last {window_hours}h", ids.len()),
                "occurrences": ids,
                "count": ids.len(),
                "recommended_owner": "boi-optimizer",
            });
            println!("{}", p);
        }
    }
    for (title, ids) in &by_title {
        if ids.len() >= THRESHOLD {
            if let Some(sid) = spec_id { if !ids.contains(&sid.to_string()) { continue; } }
            let p = serde_json::json!({
                "pattern_type": "spec_title",
                "key": title,
                "description": format!("Spec title '{title}' failed {}x in last {window_hours}h", ids.len()),
                "occurrences": ids,
                "count": ids.len(),
                "recommended_owner": "boi-optimizer",
            });
            println!("{}", p);
        }
    }
    0
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_sections_nonempty() {
        assert!(!AGENTS_MD_REQUIRED_SECTIONS.is_empty());
        assert!(AGENTS_MD_REQUIRED_SECTIONS.contains(&"Standing Orders"));
        assert!(AGENTS_MD_REQUIRED_SECTIONS.contains(&"BOI"));
    }

    #[test]
    fn check_codex_5_passes_when_all_sections_present() {
        let dir = std::env::temp_dir().join("codex_test_agents_ok");
        std::fs::create_dir_all(&dir).unwrap();
        let content = "# Standing Orders\n# Session Lifecycle\n# BOI\n# Memory\n";
        std::fs::write(dir.join("AGENTS.md"), content).unwrap();
        let mut failed = false;
        check_codex_5(&dir, &mut failed);
        assert!(!failed, "all sections present → should not fail");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn check_codex_5_fails_when_section_missing() {
        let dir = std::env::temp_dir().join("codex_test_agents_missing");
        std::fs::create_dir_all(&dir).unwrap();
        let content = "# Standing Orders\n# BOI\n";
        std::fs::write(dir.join("AGENTS.md"), content).unwrap();
        let mut failed = false;
        check_codex_5(&dir, &mut failed);
        assert!(failed, "missing sections → should fail");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn check_codex_4_fails_when_agents_md_missing() {
        let dir = std::env::temp_dir().join("codex_test_no_agents");
        std::fs::create_dir_all(&dir).unwrap();
        let _ = std::fs::remove_file(dir.join("AGENTS.md"));
        let mut failed = false;
        check_codex_4(&dir, &mut failed);
        assert!(failed, "missing AGENTS.md → should fail");
        std::fs::remove_dir_all(&dir).ok();
    }
}
