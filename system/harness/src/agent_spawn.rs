use chrono::Utc;
use serde::Deserialize;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

// ── Role-spec YAML types ──────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(default)]
struct RoleSpec {
    id: String,
    name: String,
    role: String,
    scope: String,
    reason: String,
    parent: String,
    escalation_channel: String,
    wake_triggers: Vec<String>,
    authority: Authority,
    memory_access: MemoryAccess,
}

impl Default for RoleSpec {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            role: String::new(),
            scope: String::new(),
            reason: String::new(),
            parent: String::new(),
            escalation_channel: String::new(),
            wake_triggers: vec![],
            authority: Authority::default(),
            memory_access: MemoryAccess::default(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct Authority {
    green: Vec<String>,
    yellow: Vec<String>,
    red: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct MemoryAccess {
    read_tiers: Vec<String>,
    write_paths: Vec<String>,
}

// ── Rollback tracker ──────────────────────────────────────────────────────────

struct Rollback {
    files: Vec<PathBuf>,
    dirs: Vec<PathBuf>,
    halt_file: Option<PathBuf>,
    agents_md: Option<(PathBuf, u64)>, // path + lines to trim
}

impl Rollback {
    fn new() -> Self {
        Self { files: vec![], dirs: vec![], halt_file: None, agents_md: None }
    }

    fn run(&self) {
        for f in &self.files {
            if f.exists() {
                let _ = fs::remove_file(f);
            }
        }
        for d in &self.dirs {
            if d.exists() {
                let _ = fs::remove_dir_all(d);
            }
        }
        if let Some(ref h) = self.halt_file {
            if h.exists() {
                let _ = fs::remove_file(h);
            }
        }
        if let Some((ref path, trim)) = self.agents_md {
            if path.exists() && trim > 0 {
                if let Ok(content) = fs::read_to_string(path) {
                    let lines: Vec<&str> = content.lines().collect();
                    if lines.len() >= trim as usize {
                        let keep = &lines[..lines.len() - trim as usize];
                        let _ = fs::write(path, keep.join("\n") + "\n");
                    }
                }
            }
        }
    }
}

// ── Template rendering ────────────────────────────────────────────────────────

fn indent_list(items: &[String], prefix: &str) -> String {
    if items.is_empty() {
        return format!("{prefix}(none)");
    }
    items.iter().map(|i| format!("{prefix}{i}")).collect::<Vec<_>>().join("\n")
}

fn build_wake_triggers_rules(agent_id: &str, hex_dir: &Path, triggers: &[String]) -> String {
    triggers
        .iter()
        .map(|trigger| {
            let safe_name = trigger.replace('.', "-");
            let wake_path = hex_dir.join(format!(".hex/bin/{agent_id}-wake.sh"));
            format!(
                "  - name: wake-on-{safe_name}\n\
                 \x20   trigger:\n\
                 \x20     event: {trigger}\n\
                 \x20   actions:\n\
                 \x20     - type: shell\n\
                 \x20       command: bash {wake} {trigger} '{{{{ event | tojson }}}}'\n\
                 \x20       timeout: 600\n\
                 \x20       on_success:\n\
                 \x20         - type: emit\n\
                 \x20           event: hex.agent.{agent_id}.wake\n\
                 \x20           payload:\n\
                 \x20             trigger: {trigger}\n\
                 \x20             timestamp: \"{{{{ now.isoformat() }}}}\"",
                wake = wake_path.display(),
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_template(content: &str, vars: &[(&str, &str)]) -> String {
    let mut out = content.to_string();
    for (k, v) in vars {
        out = out.replace(*k, v);
    }
    out
}

fn atomic_write(path: &Path, content: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("create_dir_all {}: {e}", parent.display()))?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, content).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    fs::rename(&tmp, path).map_err(|e| format!("rename {}: {e}", path.display()))?;
    Ok(())
}

// ── Spawn-rate check ──────────────────────────────────────────────────────────

fn count_parent_spawns_today(spawns_log: &Path, parent: &str) -> u32 {
    if !spawns_log.exists() {
        return 0;
    }
    fs::read_to_string(spawns_log)
        .unwrap_or_default()
        .lines()
        .filter(|line| {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                v.get("parent").and_then(|p| p.as_str()) == Some(parent)
            } else {
                false
            }
        })
        .count() as u32
}

// ── Main spawn logic ──────────────────────────────────────────────────────────

pub fn run_spawn(spec_file: &Path, dry_run: bool) -> i32 {
    run_spawn_inner(spec_file, dry_run, None, None)
}

fn run_spawn_inner(
    spec_file: &Path,
    dry_run: bool,
    hex_dir_override: Option<PathBuf>,
    home_override: Option<PathBuf>,
) -> i32 {
    // ── 1. Parse YAML ─────────────────────────────────────────────────────────
    let spec_text = match fs::read_to_string(spec_file) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("ERROR: cannot read spec file {}: {e}", spec_file.display());
            return 1;
        }
    };
    let spec: RoleSpec = match serde_yaml::from_str(&spec_text) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("ERROR: invalid role-spec YAML: {e}");
            return 1;
        }
    };

    // ── 2. Resolve HEX_DIR ────────────────────────────────────────────────────
    let hex_dir = match hex_dir_override.or_else(|| {
        std::env::var("HEX_DIR").ok().filter(|v| !v.is_empty()).map(PathBuf::from)
    }) {
        Some(p) => p,
        None => {
            eprintln!("ERROR: HEX_DIR not set — source env.sh first");
            return 1;
        }
    };

    let effective_home = home_override.unwrap_or_else(dirs_home);

    // ── 3. Validate reserved IDs ──────────────────────────────────────────────
    const RESERVED: &[&str] = &["mike", "hex", "hex-main", "hex-agents", "hex-v2-team"];
    if RESERVED.contains(&spec.id.as_str()) {
        eprintln!("ERROR: id '{}' is reserved", spec.id);
        return 1;
    }

    // ── 4. Check no collision ─────────────────────────────────────────────────
    let state_dir = hex_dir.join(format!("projects/{}", spec.id));
    if state_dir.exists() {
        eprintln!("ERROR: projects/{}/ already exists", spec.id);
        return 1;
    }
    let wake_script_path = hex_dir.join(format!(".hex/bin/{}-wake.sh", spec.id));
    if wake_script_path.exists() {
        eprintln!("ERROR: {}-wake.sh already exists", spec.id);
        return 1;
    }
    let policies_dir = effective_home.join(".hex-events/policies");
    let policy_path = policies_dir.join(format!("{}-agent.yaml", spec.id));
    if policy_path.exists() {
        eprintln!("ERROR: policy {}-agent.yaml already exists", spec.id);
        return 1;
    }

    // ── 5. Spawn rate limit ───────────────────────────────────────────────────
    let today = Utc::now().format("%Y-%m-%d").to_string();
    let spawns_log = hex_dir.join(format!("projects/hex-agents/_spawns/{today}.jsonl"));
    let parent_count = count_parent_spawns_today(&spawns_log, &spec.parent);
    if parent_count >= 5 {
        eprintln!(
            "ERROR: parent '{}' has reached the 5 spawns/day limit",
            spec.parent
        );
        return 1;
    }

    if dry_run {
        println!("DRY RUN: validation passed for agent '{}'", spec.id);
        println!("  id:     {}", spec.id);
        println!("  name:   {}", spec.name);
        println!("  parent: {}", spec.parent);
        return 0;
    }

    // ── 6. Compute paths ──────────────────────────────────────────────────────
    let spawn_ts = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let charter_path = state_dir.join("charter.md");
    let board_path = state_dir.join("board.md");
    let log_path = state_dir.join("log.jsonl");
    let halt_file = effective_home.join(format!(".hex-{}-HALT", spec.id));
    let decisions_dir = hex_dir.join("me/decisions");

    // ── 7. Build template variables ───────────────────────────────────────────
    let wake_triggers_block = indent_list(&spec.wake_triggers, "    - ");
    let green_block = indent_list(&spec.authority.green, "    - ");
    let yellow_block = indent_list(&spec.authority.yellow, "    - ");
    let red_block = indent_list(&spec.authority.red, "    - ");
    let read_tiers_block = indent_list(&spec.memory_access.read_tiers, "    - ");
    let write_paths_block = indent_list(&spec.memory_access.write_paths, "      - ");
    let wt_rules_events = indent_list(&spec.wake_triggers, "    - ");
    let wt_rules = build_wake_triggers_rules(&spec.id, &hex_dir, &spec.wake_triggers);

    let vars: &[(&str, &str)] = &[
        ("{{ID}}", &spec.id),
        ("{{NAME}}", &spec.name),
        ("{{ROLE}}", &spec.role),
        ("{{SCOPE}}", &spec.scope),
        ("{{PARENT}}", &spec.parent),
        ("{{SPAWN_TIMESTAMP}}", &spawn_ts),
        ("{{ESCALATION_CHANNEL}}", &spec.escalation_channel),
        ("{{STATE_DIR}}", &state_dir.to_string_lossy()),
        ("{{HALT_FILE}}", &halt_file.to_string_lossy()),
        ("{{CHARTER_PATH}}", &charter_path.to_string_lossy()),
        ("{{BOARD_PATH}}", &board_path.to_string_lossy()),
        ("{{LOG_PATH}}", &log_path.to_string_lossy()),
        ("{{WAKE_SCRIPT_PATH}}", &wake_script_path.to_string_lossy()),
        ("{{KILL_SWITCH_PATH}}", &halt_file.to_string_lossy()),
        ("{{WAKE_TRIGGERS}}", &wake_triggers_block),
        ("{{GREEN_ACTIONS}}", &green_block),
        ("{{YELLOW_ACTIONS}}", &yellow_block),
        ("{{RED_ACTIONS}}", &red_block),
        ("{{READ_TIERS}}", &read_tiers_block),
        ("{{WRITE_PATHS}}", &write_paths_block),
        ("{{WAKE_TRIGGERS_RULES_EVENTS}}", &wt_rules_events),
        ("{{WAKE_TRIGGERS_RULES}}", &wt_rules),
    ];

    // ── 8. Load templates ─────────────────────────────────────────────────────
    let tpl_dir = hex_dir.join(".hex/templates/agent");
    let load_tpl = |name: &str| -> Result<String, String> {
        let path = tpl_dir.join(name);
        fs::read_to_string(&path)
            .map(|t| render_template(&t, vars))
            .map_err(|e| format!("read template {name}: {e}"))
    };

    let charter_yaml_content = match load_tpl("charter.yaml.tpl") {
        Ok(c) => c,
        Err(e) => { eprintln!("ERROR: {e}"); return 1; }
    };
    let charter_md_content = match load_tpl("charter.md.tpl") {
        Ok(c) => c,
        Err(e) => { eprintln!("ERROR: {e}"); return 1; }
    };
    let wake_sh_content = match load_tpl("wake.sh.tpl") {
        Ok(c) => c,
        Err(e) => { eprintln!("ERROR: {e}"); return 1; }
    };
    let policy_content = match load_tpl("policy.yaml.tpl") {
        Ok(c) => c,
        Err(e) => { eprintln!("ERROR: {e}"); return 1; }
    };

    // ── 9. Write files with rollback ──────────────────────────────────────────
    let mut rb = Rollback::new();

    let write = |path: &Path, content: &str, rb: &mut Rollback| -> bool {
        match atomic_write(path, content) {
            Ok(()) => { rb.files.push(path.to_path_buf()); true }
            Err(e) => { eprintln!("ERROR: {e}"); rb.run(); false }
        }
    };

    // Create state dir
    if let Err(e) = fs::create_dir_all(&state_dir) {
        eprintln!("ERROR: create state dir: {e}");
        return 1;
    }
    rb.dirs.push(state_dir.clone());

    if !write(&state_dir.join("charter.yaml"), &charter_yaml_content, &mut rb) { return 1; }
    if !write(&state_dir.join("charter.md"), &charter_md_content, &mut rb) { return 1; }

    let board_content = format!(
        "# {name} — board\n\n\
         **State:** HALTED (pending activation)\n\
         **Created:** {ts}\n\
         **Parent:** {parent}\n\n\
         ## Backlog\n_Empty — activate agent to begin_\n\n\
         ## In Progress\n_None_\n\n\
         ## Done\n_None_\n",
        name = spec.name,
        ts = spawn_ts,
        parent = spec.parent,
    );
    if !write(&board_path, &board_content, &mut rb) { return 1; }
    if !write(&log_path, "", &mut rb) { return 1; }
    if !write(&state_dir.join("checkpoint.md"), "", &mut rb) { return 1; }
    if !write(&state_dir.join("state.md"), "{}", &mut rb) { return 1; }

    let undo_content = format!(
        "# UNDO — {name} ({id})\n\n\
         Run these commands to fully dissolve this agent:\n\n\
         ```bash\n\
         rm -rf {state}\n\
         rm -f  {wake}\n\
         rm -f  {policy}\n\
         rm -f  {halt}\n\
         sed -i '' '/{id}/d' {agents_md}\n\
         ```\n\n\
         Also remove the spawn entry from `projects/hex-agents/_spawns/{today}.jsonl`.\n",
        name = spec.name,
        id = spec.id,
        state = state_dir.display(),
        wake = wake_script_path.display(),
        policy = policy_path.display(),
        halt = halt_file.display(),
        agents_md = hex_dir.join("projects/hex-agents/AGENTS.md").display(),
    );
    if !write(&state_dir.join("UNDO.md"), &undo_content, &mut rb) { return 1; }

    // Wake script
    if let Some(parent) = wake_script_path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            eprintln!("ERROR: create wake script dir: {e}");
            rb.run();
            return 1;
        }
    }
    if !write(&wake_script_path, &wake_sh_content, &mut rb) { return 1; }
    // chmod +x
    if let Err(e) = fs::set_permissions(&wake_script_path, fs::Permissions::from_mode(0o755)) {
        eprintln!("ERROR: chmod wake script: {e}");
        rb.run();
        return 1;
    }

    // Policy
    if let Err(e) = fs::create_dir_all(&policies_dir) {
        eprintln!("ERROR: create policies dir: {e}");
        rb.run();
        return 1;
    }
    if !write(&policy_path, &policy_content, &mut rb) { return 1; }

    // HALT file
    if let Err(e) = fs::write(&halt_file, "") {
        eprintln!("ERROR: create halt file: {e}");
        rb.run();
        return 1;
    }
    rb.halt_file = Some(halt_file.clone());

    // AGENTS.md registry row
    let agents_md = hex_dir.join("projects/hex-agents/AGENTS.md");
    let registry_row = format!(
        "| {} | {} | `projects/{}/` | `.hex/bin/{}-wake.sh` | `~/.hex-events/policies/{}-agent.yaml` | `touch {}` |\n",
        spec.id, spec.scope, spec.id, spec.id, spec.id, halt_file.display()
    );
    match std::fs::OpenOptions::new().append(true).create(true).open(&agents_md) {
        Ok(mut f) => {
            use std::io::Write;
            if let Err(e) = f.write_all(registry_row.as_bytes()) {
                eprintln!("ERROR: append AGENTS.md: {e}");
                rb.run();
                return 1;
            }
            rb.agents_md = Some((agents_md.clone(), 1));
        }
        Err(e) => {
            eprintln!("ERROR: open AGENTS.md: {e}");
            rb.run();
            return 1;
        }
    }

    // Spawn audit JSONL
    if let Err(e) = fs::create_dir_all(spawns_log.parent().unwrap()) {
        eprintln!("ERROR: create spawns dir: {e}");
        rb.run();
        return 1;
    }
    let audit_entry = serde_json::json!({
        "ts": spawn_ts,
        "parent": spec.parent,
        "child_id": spec.id,
        "spec_path": spec_file.display().to_string(),
        "role": spec.role,
        "scope": spec.scope,
    });
    match std::fs::OpenOptions::new().append(true).create(true).open(&spawns_log) {
        Ok(mut f) => {
            use std::io::Write;
            if let Err(e) = writeln!(f, "{}", audit_entry) {
                eprintln!("ERROR: write spawn audit: {e}");
                rb.run();
                return 1;
            }
        }
        Err(e) => {
            eprintln!("ERROR: open spawns log: {e}");
            rb.run();
            return 1;
        }
    }

    // Decision record
    let decision_content = format!(
        "# Spawn Decision: {name} ({id})\n\n\
         **Date:** {today}\n\
         **Parent agent:** {parent}\n\
         **Spawned by:** hex agent spawn\n\
         **Spec file:** {spec_path}\n\n\
         ## Reason\n\n{reason}\n\n\
         ## Agent summary\n\n\
         - **Role:** {role}\n\
         - **Scope:** {scope}\n\
         - **Escalation:** {esc}\n\n\
         ## Status\n\n\
         Agent starts **HALTED**. To activate: `rm {halt}`\n",
        name = spec.name,
        id = spec.id,
        parent = spec.parent,
        spec_path = spec_file.display(),
        reason = spec.reason,
        role = spec.role,
        scope = spec.scope,
        esc = spec.escalation_channel,
        halt = halt_file.display(),
    );
    let decision_path = decisions_dir.join(format!("spawn-{}-{today}.md", spec.id));
    if let Err(e) = fs::create_dir_all(&decisions_dir) {
        eprintln!("ERROR: create decisions dir: {e}");
        rb.run();
        return 1;
    }
    if !write(&decision_path, &decision_content, &mut rb) { return 1; }

    // ── 10. Validate wake script ──────────────────────────────────────────────
    let wake_text = match fs::read_to_string(&wake_script_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("ERROR: re-read wake script: {e}");
            rb.run();
            return 1;
        }
    };
    let mut wake_errors = 0;
    if !wake_text.contains("source") || !wake_text.contains("env.sh") {
        eprintln!("FATAL: wake script does not source env.sh");
        wake_errors += 1;
    }
    // Reject hardcoded absolute claude paths (e.g. /Users/…/.local/bin/claude)
    if regex::Regex::new(r"/Users/[^/]+/\.local/bin/claude").unwrap().is_match(&wake_text) {
        eprintln!("FATAL: wake script hardcodes absolute claude path");
        wake_errors += 1;
    }
    if wake_errors > 0 {
        eprintln!("Wake script validation failed — rolling back");
        rb.run();
        return 1;
    }

    // ── 11. Validate policy if hex-events available ───────────────────────────
    if let Ok(output) = std::process::Command::new("hex-events")
        .arg("validate")
        .arg(&policy_path)
        .output()
    {
        if !output.status.success() {
            eprintln!("ERROR: policy validation failed");
            eprintln!("{}", String::from_utf8_lossy(&output.stderr));
            rb.run();
            return 1;
        }
    }
    // (hex-events absent = skip validation, matching shell script behavior)

    // ── 12. Print success ─────────────────────────────────────────────────────
    println!("\n✓ Agent '{}' spawned successfully.", spec.id);
    println!("  State dir:   {}", state_dir.display());
    println!("  Wake script: {}", wake_script_path.display());
    println!("  Policy:      {}", policy_path.display());
    println!("  HALT file:   {} (agent is HALTED)", halt_file.display());
    println!();
    println!(
        "To activate: bash {}/.hex/bin/hex-agent-activate.sh {}",
        hex_dir.display(), spec.id
    );
    0
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_hex_dir_with_templates(dir: &TempDir) {
        for sub in &[".hex/bin", ".hex/scripts", ".hex/templates/agent",
                     "projects/hex-agents/_spawns", "projects/hex-agents"] {
            fs::create_dir_all(dir.path().join(sub)).unwrap();
        }
        // Write minimal templates matching the real ones
        fs::write(
            dir.path().join(".hex/templates/agent/charter.yaml.tpl"),
            "name: {{NAME}}\nid: {{ID}}\nscope: {{SCOPE}}\n",
        ).unwrap();
        fs::write(
            dir.path().join(".hex/templates/agent/charter.md.tpl"),
            "# {{NAME}}\n{{KILL_SWITCH_PATH}}\n",
        ).unwrap();
        fs::write(
            dir.path().join(".hex/templates/agent/wake.sh.tpl"),
            "#!/usr/bin/env bash\nsource \"$(dirname $BASH_SOURCE)/env.sh\"\n# {{ID}}\n",
        ).unwrap();
        fs::write(
            dir.path().join(".hex/templates/agent/policy.yaml.tpl"),
            "name: {{ID}}-agent\nrules:\n{{WAKE_TRIGGERS_RULES}}\n",
        ).unwrap();
        fs::write(
            dir.path().join("projects/hex-agents/AGENTS.md"),
            "| id | scope | state | wake | policy | halt |\n|--|--|--|--|--|--|\n",
        ).unwrap();
    }

    fn write_test_spec(path: &Path) {
        fs::write(path, concat!(
            "id: test-noop\n",
            "name: Test Noop Agent\n",
            "role: noop\n",
            "scope: CI shadow test\n",
            "reason: Shadow test for hex agent spawn\n",
            "parent: hex\n",
            "escalation_channel: dev\n",
            "wake_triggers:\n",
            "  - timer.tick.1h\n",
            "authority:\n",
            "  green:\n",
            "    - read any file\n",
            "  yellow:\n",
            "    - write log\n",
            "  red:\n",
            "    - modify production\n",
            "memory_access:\n",
            "  read_tiers:\n",
            "    - tier1\n",
            "  write_paths:\n",
            "    - projects/test-noop/\n",
        )).unwrap();
    }

    #[test]
    fn test_dry_run_passes_validation() {
        let dir = tempfile::tempdir().unwrap();
        make_hex_dir_with_templates(&dir);
        let home_tmp = tempfile::tempdir().unwrap();
        let spec_path = dir.path().join("spec.yaml");
        write_test_spec(&spec_path);

        let rc = run_spawn_inner(&spec_path, true, Some(dir.path().to_path_buf()), Some(home_tmp.path().to_path_buf()));
        assert_eq!(rc, 0, "dry-run should pass");
    }

    #[test]
    fn test_reserved_id_rejected() {
        let dir = tempfile::tempdir().unwrap();
        make_hex_dir_with_templates(&dir);
        let home_tmp = tempfile::tempdir().unwrap();

        let spec_path = dir.path().join("spec.yaml");
        fs::write(&spec_path, concat!(
            "id: mike\n",
            "name: Bad Agent\n",
            "role: noop\n",
            "scope: test\n",
            "reason: test\n",
            "parent: hex\n",
            "escalation_channel: test\n",
            "wake_triggers: [timer.tick.1h]\n",
            "authority:\n",
            "  green: []\n",
            "  yellow: []\n",
            "  red: []\n",
            "memory_access:\n",
            "  read_tiers: []\n",
            "  write_paths: []\n",
        )).unwrap();

        let rc = run_spawn_inner(&spec_path, true, Some(dir.path().to_path_buf()), Some(home_tmp.path().to_path_buf()));
        assert_eq!(rc, 1, "reserved id should fail");
    }

    #[test]
    fn test_spawn_rate_limit() {
        let dir = tempfile::tempdir().unwrap();
        make_hex_dir_with_templates(&dir);
        let home_tmp = tempfile::tempdir().unwrap();
        let today = Utc::now().format("%Y-%m-%d").to_string();
        let spawns_log = dir.path().join(format!("projects/hex-agents/_spawns/{today}.jsonl"));
        fs::create_dir_all(spawns_log.parent().unwrap()).unwrap();
        let mut content = String::new();
        for _ in 0..5 {
            content.push_str("{\"parent\":\"hex\",\"child_id\":\"x\"}\n");
        }
        fs::write(&spawns_log, &content).unwrap();

        let spec_path = dir.path().join("spec.yaml");
        write_test_spec(&spec_path);

        let rc = run_spawn_inner(&spec_path, true, Some(dir.path().to_path_buf()), Some(home_tmp.path().to_path_buf()));
        assert_eq!(rc, 1, "rate limit should reject");
    }

    #[test]
    fn test_indent_list_empty() {
        let result = indent_list(&[], "    - ");
        assert_eq!(result, "    - (none)");
    }

    #[test]
    fn test_indent_list_items() {
        let items = vec!["a".to_string(), "b".to_string()];
        let result = indent_list(&items, "    - ");
        assert_eq!(result, "    - a\n    - b");
    }

    #[test]
    fn test_render_template() {
        let t = "Hello {{NAME}}, id={{ID}}";
        let vars: &[(&str, &str)] = &[("{{NAME}}", "Alice"), ("{{ID}}", "alice-1")];
        assert_eq!(render_template(t, vars), "Hello Alice, id=alice-1");
    }
}
