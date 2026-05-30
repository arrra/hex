use crate::{
    act_evidence, audit, capability_exec, capability_guard, charter, claude, cost, gate, message,
    prompt, queue, registry, state,
};
use chrono::Utc;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

const NON_JSON_REPROMPT: &str = "\n\n⚠️ Your previous response was NOT valid JSON and was rejected. Respond with ONLY the JSON object specified in the Response format section — no prose, no markdown fences, no explanation before or after. Your entire response must start with { and end with }.";

pub(crate) enum RetryOutcome {
    Parsed { response: crate::types::AgentResponse, quality: claude::ResponseParseQuality, was_retried: bool },
    Unrecoverable,
    InvokeError(String),
}

/// Parse a first LLM result text. If it returns `Empty` quality (non-JSON, unsalvageable),
/// attempt ONE retry with a stern JSON-only reprompt.
/// `retry_fn` is called with the augmented retry prompt and must return the result text or error.
pub(crate) fn retry_if_empty<F>(
    first_response: crate::types::AgentResponse,
    first_quality: claude::ResponseParseQuality,
    original_prompt: &str,
    retry_fn: F,
) -> RetryOutcome
where
    F: FnOnce(&str) -> Result<String, String>,
{
    if first_quality != claude::ResponseParseQuality::Empty {
        return RetryOutcome::Parsed { response: first_response, quality: first_quality, was_retried: false };
    }
    let retry_prompt = format!("{original_prompt}{NON_JSON_REPROMPT}");
    match retry_fn(&retry_prompt) {
        Err(e) => RetryOutcome::InvokeError(e),
        Ok(retry_text) => {
            let (r2, q2) = claude::parse_agent_response(&retry_text);
            if q2 == claude::ResponseParseQuality::Empty {
                RetryOutcome::Unrecoverable
            } else {
                RetryOutcome::Parsed { response: r2, quality: q2, was_retried: true }
            }
        }
    }
}

/// Process a single `capability_add` or `capability_call` trail entry.
///
/// For `capability_add`: runs allowlist check → body scan → immutability guard → persists to
/// registry. Returns `Ok(None)` on success.
///
/// For `capability_call`: runs allowlist check → looks up `created_by` → executes inside the
/// sandbox at `sandbox_dir/run-test.sh`. Returns `Ok(Some(result_entry))` on success, where the
/// result entry is an `act` TrailEntry with stdout/stderr/exit_code in its detail.
///
/// A guard failure (not allowed, dangerous body, write-once violation) returns `Err(String)`.
/// The caller is responsible for treating this as a gate violation (audit + skip persisting).
pub fn apply_capability_entry(
    entry: &crate::types::TrailEntry,
    agent_id: &str,
    hex_dir: &Path,
    wake_n: u64,
    call_count: &mut u32,
    sandbox_dir: &Path,
) -> Result<Option<crate::types::TrailEntry>, String> {
    let registry_dir = hex_dir.join(".hex/registry");

    match entry.entry_type.as_str() {
        "capability_add" => {
            capability_guard::check_allowed(hex_dir, agent_id, "add")?;

            let detail = entry
                .detail
                .as_object()
                .ok_or("capability_add detail must be a JSON object")?;

            let cap_kind = detail
                .get("capability_kind")
                .and_then(|v| v.as_str())
                .ok_or("capability_add: missing capability_kind")?;
            let cap_id = detail
                .get("capability_id")
                .and_then(|v| v.as_str())
                .ok_or("capability_add: missing capability_id")?;
            let description = detail
                .get("description")
                .and_then(|v| v.as_str())
                .ok_or("capability_add: missing description")?;
            let exec_or_event = detail
                .get("exec_or_event")
                .and_then(|v| v.as_str())
                .ok_or("capability_add: missing exec_or_event")?;

            capability_guard::check_body_safe(exec_or_event)?;
            capability_guard::check_immutable(&registry_dir, cap_id)?;

            let now = Utc::now().to_rfc3339();
            let input_schema = detail
                .get("input_schema")
                .cloned()
                .unwrap_or(serde_json::Value::Null);

            if cap_kind == "trigger" {
                let cap = registry::TriggerCapability {
                    id: cap_id.to_string(),
                    kind: cap_kind.to_string(),
                    created_by: agent_id.to_string(),
                    created_at: now.clone(),
                    created_in_wake: wake_n,
                    unprompted: detail.get("unprompted").and_then(|v| v.as_bool()).unwrap_or(false),
                    description: description.to_string(),
                    event: exec_or_event.to_string(),
                    input_schema,
                    callable_by: detail.get("callable_by")
                        .and_then(|v| v.as_array())
                        .map(|arr| arr.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                        .unwrap_or_else(|| registry::load_allowlist(hex_dir).unwrap_or_default()),
                };
                registry::add_trigger(&registry_dir, &cap)?;
            } else {
                let cap = registry::FunctionCapability {
                    id: cap_id.to_string(),
                    kind: cap_kind.to_string(),
                    created_by: agent_id.to_string(),
                    created_at: now.clone(),
                    created_in_wake: wake_n,
                    unprompted: detail.get("unprompted").and_then(|v| v.as_bool()).unwrap_or(false),
                    description: description.to_string(),
                    exec: exec_or_event.to_string(),
                    input_schema,
                    callable_by: detail.get("callable_by")
                        .and_then(|v| v.as_array())
                        .map(|arr| arr.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                        .unwrap_or_else(|| registry::load_allowlist(hex_dir).unwrap_or_default()),
                };
                registry::add_function(&registry_dir, &cap, exec_or_event.as_bytes())?;
            }

            // Append audit row to audit.jsonl — read agent-supplied fields from detail.
            let unprompted_for_audit = detail.get("unprompted").and_then(|v| v.as_bool()).unwrap_or(false);
            let wall_hit_for_audit = detail.get("wall_hit").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let audit_record = serde_json::json!({
                "ts": now,
                "capability_id": cap_id,
                "capability_kind": cap_kind,
                "created_by": agent_id,
                "unprompted": unprompted_for_audit,
                "wall_hit": wall_hit_for_audit,
                "exec_or_event": exec_or_event,
            });
            let _ = registry::append_audit(&registry_dir, &audit_record);

            // Emit ordering signal AFTER capability is fully persisted.
            // Sibling pilots wake on this event (not timer.tick.daily fan-out), so
            // build_catalog on receipt always observes the new capability.
            let _ = registry::emit_capability_added(&registry_dir, cap_id, agent_id);

            Ok(None)
        }

        "capability_call" => {
            capability_guard::check_allowed(hex_dir, agent_id, "call")?;

            let detail = entry
                .detail
                .as_object()
                .ok_or("capability_call detail must be a JSON object")?;

            let cap_id = detail
                .get("capability_id")
                .and_then(|v| v.as_str())
                .ok_or("capability_call: missing capability_id")?;

            let args_val = detail
                .get("args")
                .cloned()
                .unwrap_or(serde_json::Value::Null);

            // Look up the function definition to get the original creator.
            let fn_json_path = registry_dir
                .join("functions")
                .join(format!("{cap_id}.json"));
            if !fn_json_path.exists() {
                return Err(format!(
                    "capability_call: function '{cap_id}' is not registered"
                ));
            }
            let fn_data = std::fs::read_to_string(&fn_json_path)
                .map_err(|e| format!("capability_call: read function def for '{cap_id}': {e}"))?;
            let fn_val: serde_json::Value = serde_json::from_str(&fn_data)
                .map_err(|e| format!("capability_call: parse function def for '{cap_id}': {e}"))?;
            let created_by = fn_val["created_by"]
                .as_str()
                .unwrap_or("")
                .to_string();

            // Callable-by gate: verify the caller is authorized for this specific capability.
            let callable_by: Vec<String> = fn_val["callable_by"]
                .as_array()
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();
            if !callable_by.iter().any(|a| a == agent_id) {
                return Err(format!(
                    "capability_call: agent '{agent_id}' is not in callable_by list for '{cap_id}'"
                ));
            }

            // Convert args to a Vec<String> for the executor.
            let args: Vec<String> = match &args_val {
                serde_json::Value::Array(arr) => arr
                    .iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect(),
                serde_json::Value::Object(_) => {
                    vec![serde_json::to_string(&args_val).unwrap_or_default()]
                }
                _ => vec![],
            };

            let ctx = capability_exec::ExecContext {
                caller: agent_id.to_string(),
                created_by,
                wake_n,
                timeout_secs: 30,
                output_cap_bytes: 65_536,
                calls_per_wake_cap: 10,
            };

            let exec_result = capability_exec::execute_capability(
                &registry_dir,
                cap_id,
                &args,
                &ctx,
                sandbox_dir,
                call_count,
            )?;

            let result_entry = crate::types::TrailEntry {
                ts: Utc::now(),
                entry_type: "act".to_string(),
                detail: serde_json::json!({
                    "action": format!("capability_call:{cap_id}"),
                    "result": {
                        "exit_code": exec_result.exit_code,
                        "stdout": exec_result.stdout,
                        "stderr": exec_result.stderr,
                        "timed_out": exec_result.timed_out,
                        "output_truncated": exec_result.output_truncated,
                    }
                }),
                queue_item: entry.queue_item.clone(),
            };

            Ok(Some(result_entry))
        }

        other => Err(format!(
            "apply_capability_entry: not a capability entry type: '{other}'"
        )),
    }
}

pub struct WakeConfig {
    pub hex_dir: PathBuf,
    pub agent_id: String,
    pub trigger: String,
    pub payload: String,
}

pub fn run(config: WakeConfig) -> Result<i32, Box<dyn std::error::Error>> {
    let hex_dir = &config.hex_dir;
    let audit_dir = hex_dir.join(".hex/audit");
    let cost_dir = hex_dir.join(".hex/cost");
    let msg_dir = hex_dir.join(".hex/messages");

    // 1. Load charter — one canonical path, no fallbacks
    let charter_path = hex_dir.join(format!("projects/{}/charter.yaml", config.agent_id));
    if !charter_path.exists() {
        return Err(format!(
            "no charter at {} — agent '{}' is not registered (charter.yaml IS registration)",
            charter_path.display(),
            config.agent_id
        )
        .into());
    }
    let charter_data = charter::load(&charter_path)?;
    if charter_data.id != config.agent_id {
        return Err(format!(
            "charter id mismatch: CLI arg is '{}' but charter.id is '{}' in {} — these must match exactly",
            config.agent_id, charter_data.id, charter_path.display()
        ).into());
    }
    let charter_text = std::fs::read_to_string(&charter_path)?;

    // 1b. Load fleet-wide principles (optional, hot-updateable)
    let principles_path = hex_dir.join(".hex/principles.md");
    let principles_text = std::fs::read_to_string(&principles_path).ok();

    // 1c. Build capability catalog for allowlisted pilot agents (ordered before charter context).
    let registry_dir = hex_dir.join(".hex/registry");
    let catalog_json: Option<String> = if registry::is_allowed(hex_dir, &config.agent_id) {
        match registry::build_catalog(&registry_dir) {
            Ok(entries) => serde_json::to_string_pretty(&entries).ok(),
            Err(e) => {
                eprintln!("[{}] capability catalog build failed: {e}", config.agent_id);
                None
            }
        }
    } else {
        None
    };

    // 1d. Load context_files declared in charter (injected into prompt)
    let mut context_files_content = String::new();
    for pattern in &charter_data.context_files {
        let expanded = shellexpand::tilde(pattern).to_string();
        let full_pattern = if Path::new(&expanded).is_absolute() {
            expanded
        } else {
            hex_dir.join(&expanded).to_string_lossy().to_string()
        };
        let matches: Vec<_> = glob::glob(&full_pattern)
            .into_iter()
            .flatten()
            .filter_map(|r| r.ok())
            .collect();
        if matches.is_empty() {
            if let Ok(content) = std::fs::read_to_string(&full_pattern) {
                context_files_content.push_str(&format!("\n## {}\n\n{}\n", pattern, content));
            }
        } else {
            for path in matches {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    let rel = path.strip_prefix(hex_dir).unwrap_or(&path);
                    context_files_content.push_str(
                        &format!("\n## {}\n\n{}\n", rel.display(), content)
                    );
                }
            }
        }
    }

    // 2. HALT check
    let kill_switch = shellexpand::tilde(&charter_data.kill_switch).to_string();
    if Path::new(&kill_switch).exists() {
        audit::append(
            &audit_dir,
            &config.agent_id,
            "halted",
            &serde_json::json!({"reason": "kill_switch"}),
        );
        eprintln!(
            "[{}] HALTED: kill switch at {}",
            config.agent_id, kill_switch
        );
        return Ok(0);
    }

    // 3. Load or initialize state — same directory as charter, no fallbacks
    let state_dir = hex_dir.join(format!("projects/{}", config.agent_id));
    std::fs::create_dir_all(&state_dir)?;
    let state_path = state_dir.join("state.json");

    // 3a. Acquire exclusive lock to prevent concurrent wakes from corrupting state
    let lock_path = state_dir.join("state.json.lock");
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&lock_path)
        .map_err(|e| format!("cannot open lock file {}: {e}", lock_path.display()))?;
    use fs2::FileExt;
    match lock_file.try_lock_exclusive() {
        Ok(()) => {}
        Err(_) => {
            eprintln!(
                "[{}] SKIP: another wake is already running (lock held on {})",
                config.agent_id,
                lock_path.display()
            );
            audit::append(
                &audit_dir,
                &config.agent_id,
                "wake-lock-contention",
                &serde_json::json!({"reason": "another wake holds the lock"}),
            );
            return Ok(0);
        }
    }
    // lock_file is held until this function returns (RAII drop)

    let mut agent_state = if state_path.exists() {
        state::load(&state_path)?
    } else {
        state::initialize(&config.agent_id)
    };

    // 4. Reset per-shift cost, increment wake
    agent_state.cost.last_wake_usd = 0.0;
    agent_state.wake_count += 1;
    agent_state.last_wake = Some(Utc::now());

    // 5. Populate inbox
    let inbox_messages = message::receive(&msg_dir, &config.agent_id);
    agent_state.inbox = inbox_messages;
    message::clear_inbox(&msg_dir, &config.agent_id);

    // 6. Queue promotions
    let now = Utc::now();

    // 6b. Auto-promote charter responsibilities not yet in the scheduled queue.
    // On first wake (empty state) all responsibilities seed as due-now.
    // On subsequent wakes, promote_scheduled manages their cadence.
    queue::auto_seed_from_charter(
        &mut agent_state.queue,
        &charter_data.wake.responsibilities,
        &agent_state.cadence_overrides,
        now,
    );

    let sched_promoted = queue::promote_scheduled(&mut agent_state.queue, now);
    let unblocked = queue::promote_unblocked(&mut agent_state.queue);
    let inbox_items = queue::inbox_to_active(&mut agent_state);

    audit::append(
        &audit_dir,
        &config.agent_id,
        "wake-start",
        &serde_json::json!({
            "trigger": config.trigger,
            "wake_count": agent_state.wake_count,
            "scheduled_promoted": sched_promoted,
            "unblocked": unblocked,
            "inbox_items": inbox_items,
            "active_count": agent_state.queue.active.len(),
        }),
    );

    // 7. Nothing actionable?
    if agent_state.queue.active.is_empty() {
        audit::append(
            &audit_dir,
            &config.agent_id,
            "wake-skip",
            &serde_json::json!({"reason": "nothing actionable"}),
        );
        state::save(&agent_state, &state_path)?;
        return Ok(0);
    }

    // 8. Shift loop
    let allowed_tools = ["Bash", "Read", "Write", "Edit", "Grep", "Glob"];
    let mut invocation = 0;

    // health-probe agents (charter `wake.skip_llm: true`) bypass the LLM loop —
    // they exist to validate wake plumbing without paying for a Claude call.
    // Inbox is populated; post-loop state save + audit still runs.
    if charter_data.wake.skip_llm {
        let inbox_count = agent_state.inbox.len();
        let audit_reason = if inbox_count == 0 {
            "charter wake.skip_llm=true; empty inbox"
        } else {
            "charter wake.skip_llm=true"
        };
        audit::append(
            &audit_dir,
            &config.agent_id,
            "wake-skip-llm",
            &serde_json::json!({
                "reason": audit_reason,
                "inbox_count": inbox_count,
            }),
        );
        // Drain inbox-sourced active items to prevent unbounded state.json growth.
        // health-probe charters have no responsibilities so this is safe.
        agent_state.queue.active.retain(|i| !i.id.starts_with("inbox-"));
    } else {

    loop {
        invocation += 1;

        let ctx_files = if context_files_content.is_empty() {
            None
        } else {
            Some(context_files_content.as_str())
        };
        let prompt_text = prompt::build(
            &charter_text,
            &agent_state,
            &config.trigger,
            &config.payload,
            principles_text.as_deref(),
            ctx_files,
            catalog_json.as_deref(),
        );

        let claude_output = match claude::invoke(&prompt_text, "sonnet", &allowed_tools) {
            Ok(out) => out,
            Err(e) => {
                audit::append(
                    &audit_dir,
                    &config.agent_id,
                    "claude-error",
                    &serde_json::json!({
                        "error": e.to_string(),
                        "invocation": invocation,
                    }),
                );
                break;
            }
        };

        cost::record_invocation(&mut agent_state.cost, &claude_output);
        cost::append_ledger(&cost_dir, &config.agent_id, &claude_output);

        let (first_response, first_quality) = claude::parse_agent_response(&claude_output.result);

        let mut retry_claude_output: Option<crate::types::ClaudeOutput> = None;
        let retry_result = retry_if_empty(
            first_response,
            first_quality,
            &prompt_text,
            |retry_prompt| match claude::invoke(retry_prompt, "sonnet", &allowed_tools) {
                Ok(out) => {
                    let text = out.result.clone();
                    retry_claude_output = Some(out);
                    Ok(text)
                }
                Err(e) => Err(e.to_string()),
            },
        );

        if let Some(ref out) = retry_claude_output {
            cost::record_invocation(&mut agent_state.cost, out);
            cost::append_ledger(&cost_dir, &config.agent_id, out);
        }

        let (response, parse_quality) = match retry_result {
            RetryOutcome::Parsed { response, quality, was_retried } => {
                if was_retried {
                    eprintln!("[{}] non-JSON retry succeeded — response recovered", config.agent_id);
                }
                (response, quality)
            }
            RetryOutcome::Unrecoverable => {
                eprintln!(
                    "[{}] WAKE RESPONSE UNRECOVERABLE after retry — shift work lost this iteration",
                    config.agent_id
                );
                audit::append(
                    &audit_dir,
                    &config.agent_id,
                    "wake-response-unrecoverable",
                    &serde_json::json!({"wake": agent_state.wake_count}),
                );
                let emit_script = hex_dir.join(".hex/bin/hex-emit.sh");
                let _ = std::process::Command::new(&emit_script)
                    .arg("hex.agent.response.unrecoverable")
                    .arg(serde_json::json!({
                        "agent": config.agent_id,
                        "wake": agent_state.wake_count,
                    }).to_string())
                    .status();
                break;
            }
            RetryOutcome::InvokeError(e) => {
                audit::append(
                    &audit_dir,
                    &config.agent_id,
                    "claude-error",
                    &serde_json::json!({"error": e, "invocation": invocation}),
                );
                break;
            }
        };

        match &parse_quality {
            claude::ResponseParseQuality::Empty => {
                unreachable!("Empty quality should have been handled by retry_if_empty above")
            }
            claude::ResponseParseQuality::Salvaged { recovered_trail, recovered_messages } => {
                eprintln!(
                    "[{}] WARNING: response truncated — salvaged {} trail entries, {} messages; some agent work was LOST this wake",
                    config.agent_id, recovered_trail, recovered_messages
                );
                audit::append(
                    &audit_dir,
                    &config.agent_id,
                    "response-truncated",
                    &serde_json::json!({
                        "salvaged_trail": recovered_trail,
                        "salvaged_messages": recovered_messages,
                        "invocation": invocation,
                    }),
                );
                let emit_script = hex_dir.join(".hex/bin/hex-emit.sh");
                let payload = serde_json::json!({
                    "agent": config.agent_id,
                    "wake": agent_state.wake_count,
                    "salvaged_trail": recovered_trail,
                    "salvaged_messages": recovered_messages,
                });
                let _ = std::process::Command::new(&emit_script)
                    .arg("hex.agent.response.truncated")
                    .arg(payload.to_string())
                    .status();
            }
            claude::ResponseParseQuality::Clean => {}
        }

        // Validate and append trail entries
        let mut accepted_entries: Vec<crate::types::TrailEntry> = Vec::new();
        // Per-wake call counter for capability_call budget enforcement.
        let mut capability_call_count: u32 = 0;
        for entry in &response.trail {
            match gate::validate(entry) {
                Ok(()) => {
                    let mut entry_to_persist = entry.clone();

                    // For "act" entries that carry an evidence field, verify the claim.
                    // Non-mechanical acts (no evidence field) are persisted as-is.
                    if entry.entry_type == "act" {
                        if let Some(ev_val) = entry.detail.get("evidence") {
                            let action = entry
                                .detail
                                .get("action")
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown");

                            let verify_result =
                                match serde_json::from_value::<crate::types::ActEvidence>(
                                    ev_val.clone(),
                                ) {
                                    Ok(ev) => act_evidence::verify(&ev).map_err(|e| e),
                                    Err(e) => Err(format!("unparseable evidence: {e}")),
                                };

                            if let Err(reason) = verify_result {
                                eprintln!(
                                    "[{}] UNVERIFIED ACT: claimed '{}' — evidence failed: {}",
                                    config.agent_id, action, reason
                                );
                                if let Some(obj) = entry_to_persist.detail.as_object_mut() {
                                    obj.insert(
                                        "verified".to_string(),
                                        serde_json::json!(false),
                                    );
                                    obj.insert(
                                        "evidence_error".to_string(),
                                        serde_json::json!(reason.clone()),
                                    );
                                }
                                audit::append(
                                    &audit_dir,
                                    &config.agent_id,
                                    "act-unverified",
                                    &serde_json::json!({
                                        "action": action,
                                        "evidence": ev_val,
                                        "error": reason,
                                    }),
                                );
                                let emit_script = hex_dir.join(".hex/bin/hex-emit.sh");
                                let _ = std::process::Command::new(&emit_script)
                                    .arg("hex.agent.act.unverified")
                                    .arg(
                                        serde_json::json!({
                                            "agent": &config.agent_id,
                                            "action": action,
                                            "evidence": ev_val,
                                        })
                                        .to_string(),
                                    )
                                    .status();
                            }
                        }
                    }

                    // For capability entries, run guard + processing before persisting to trail.
                    // A guard failure is treated as a gate violation: the entry is not persisted.
                    let mut capability_result: Option<crate::types::TrailEntry> = None;
                    let capability_guard_failed =
                        if entry.entry_type == "capability_add"
                            || entry.entry_type == "capability_call"
                        {
                            let sandbox_dir = hex_dir.join(".hex/containers");
                            match apply_capability_entry(
                                entry,
                                &config.agent_id,
                                hex_dir,
                                agent_state.wake_count,
                                &mut capability_call_count,
                                &sandbox_dir,
                            ) {
                                Ok(maybe_result) => {
                                    capability_result = maybe_result;
                                    false
                                }
                                Err(e) => {
                                    eprintln!(
                                        "[{}] CAPABILITY REJECTED ({}): {e}",
                                        config.agent_id, entry.entry_type
                                    );
                                    audit::append(
                                        &audit_dir,
                                        &config.agent_id,
                                        "capability-rejected",
                                        &serde_json::json!({
                                            "type": entry.entry_type,
                                            "agent": &config.agent_id,
                                            "error": e,
                                        }),
                                    );
                                    true
                                }
                            }
                        } else {
                            false
                        };

                    if !capability_guard_failed {
                        agent_state.trail.push(entry_to_persist.clone());
                        accepted_entries.push(entry_to_persist);
                        audit::append(
                            &audit_dir,
                            &config.agent_id,
                            &format!("gate:{}", entry.entry_type),
                            &entry.detail,
                        );
                        // For capability_call, also inject the execution result into the trail.
                        if let Some(result_entry) = capability_result {
                            agent_state.trail.push(result_entry.clone());
                            accepted_entries.push(result_entry);
                        }
                    }
                }
                Err(violation) => {
                    audit::append(
                        &audit_dir,
                        &config.agent_id,
                        "gate-violation",
                        &serde_json::json!({
                            "type": entry.entry_type,
                            "violation": violation,
                        }),
                    );
                }
            }
        }

        // Loop detection: check accepted observe/verify entries for repetition.
        // Window defaults to 1h; budget-derived `wakes_per_hour` throttling is gone.
        let interval_seconds: u64 = 3600;
        if check_and_handle_loop(
            &mut agent_state,
            &accepted_entries,
            interval_seconds,
            hex_dir,
            &audit_dir,
        ) {
            state::save(&agent_state, &state_path)?;
            return Ok(0);
        }

        // Apply queue updates
        agent_state
            .queue
            .active
            .retain(|item| !response.queue_updates.completed.contains(&item.id));
        for item in response.queue_updates.added_active {
            agent_state.queue.active.push(item);
        }
        for item in response.queue_updates.moved_to_blocked {
            agent_state.queue.blocked.push(item);
        }

        // Apply memory updates
        if let Some(updates) = response.memory_updates {
            if let (Some(mem), Some(upd)) =
                (agent_state.memory.as_object_mut(), updates.as_object())
            {
                for (k, v) in upd {
                    mem.insert(k.clone(), v.clone());
                }
            }
        }

        // Deliver outbound messages
        for msg in &response.outbound_messages {
            match message::send(&msg_dir, msg) {
                Ok(()) => {
                    audit::append(
                        &audit_dir,
                        &config.agent_id,
                        "message-sent",
                        &serde_json::json!({
                            "to": msg.to,
                            "subject": msg.subject,
                        }),
                    );
                    if msg.response_requested {
                        for recipient in &msg.to {
                            auto_wake_target(hex_dir, recipient, &config.agent_id, &audit_dir);
                        }
                    }
                }
                Err(e) => {
                    eprintln!(
                        "[{}] MESSAGE SEND FAILED to {}: {e}",
                        config.agent_id, msg.to.join(", ")
                    );
                    audit::append(
                        &audit_dir,
                        &config.agent_id,
                        "message-send-failed",
                        &serde_json::json!({
                            "to": msg.to,
                            "subject": msg.subject,
                            "error": e.to_string(),
                        }),
                    );
                }
            }
        }

        if response.active_drained || agent_state.queue.active.is_empty() {
            break;
        }
    }

    // 9. Self-assessment phase (runs every N wakes)
    let assess_interval = charter_data
        .assessment
        .as_ref()
        .map(|a| a.every_n_wakes)
        .unwrap_or_else(|| crate::types::AssessmentConfig::default().every_n_wakes);
    let wakes_since_assessment = agent_state
        .wake_count
        .saturating_sub(agent_state.last_assessment_wake);

    if assess_interval > 0 && wakes_since_assessment >= assess_interval {
        audit::append(
            &audit_dir,
            &config.agent_id,
            "assessment-start",
            &serde_json::json!({
                "wake_count": agent_state.wake_count,
                "last_assessment_wake": agent_state.last_assessment_wake,
                "interval": assess_interval,
            }),
        );

        let assess_prompt =
            prompt::build_assessment(&charter_data, &agent_state, principles_text.as_deref());

        match claude::invoke(&assess_prompt, "sonnet", &["Bash", "Read", "Grep", "Glob"]) {
            Ok(assess_output) => {
                cost::record_invocation(&mut agent_state.cost, &assess_output);
                cost::append_ledger(&cost_dir, &config.agent_id, &assess_output);

                match claude::parse_assessment_response(&assess_output.result) {
                    Ok(assessment) => {
                        // Validate and append assessment trail entries
                        for entry in &assessment.trail {
                            match gate::validate(entry) {
                                Ok(()) => {
                                    agent_state.trail.push(entry.clone());
                                    audit::append(
                                        &audit_dir,
                                        &config.agent_id,
                                        &format!("gate:{}", entry.entry_type),
                                        &entry.detail,
                                    );
                                }
                                Err(violation) => {
                                    audit::append(
                                        &audit_dir,
                                        &config.agent_id,
                                        "gate-violation",
                                        &serde_json::json!({"type": entry.entry_type, "violation": violation}),
                                    );
                                }
                            }
                        }

                        // Apply cadence overrides
                        for change in &assessment.cadence_overrides {
                            agent_state
                                .cadence_overrides
                                .insert(change.responsibility.clone(), change.new_interval);
                            let scheduled_id = format!("s-{}", change.responsibility);
                            if let Some(item) = agent_state.queue.scheduled.iter_mut()
                                .find(|s| s.id == scheduled_id) {
                                item.interval_seconds = change.new_interval;
                            }
                            audit::append(
                                &audit_dir,
                                &config.agent_id,
                                "cadence-change",
                                &serde_json::json!({
                                    "responsibility": change.responsibility,
                                    "old_interval": change.old_interval,
                                    "new_interval": change.new_interval,
                                    "reason": change.reason,
                                }),
                            );
                        }

                        // Apply strategy updates to working memory
                        if let Some(ref updates) = assessment.strategy_updates {
                            if let (Some(mem), Some(upd)) =
                                (agent_state.memory.as_object_mut(), updates.as_object())
                            {
                                for (k, v) in upd {
                                    mem.insert(k.clone(), v.clone());
                                }
                            }
                        }

                        // Log recommendations
                        if !assessment.recommendations.is_empty() {
                            audit::append(
                                &audit_dir,
                                &config.agent_id,
                                "assessment-recommendations",
                                &serde_json::json!({"recommendations": assessment.recommendations}),
                            );
                        }

                        agent_state.last_assessment_wake = agent_state.wake_count;

                        audit::append(
                            &audit_dir,
                            &config.agent_id,
                            "assessment-complete",
                            &serde_json::json!({
                                "cadence_changes": assessment.cadence_overrides.len(),
                                "recommendations": assessment.recommendations.len(),
                                "has_strategy_updates": assessment.strategy_updates.is_some(),
                            }),
                        );
                    }
                    Err(e) => {
                        audit::append(
                            &audit_dir,
                            &config.agent_id,
                            "assessment-parse-error",
                            &serde_json::json!({"error": e.to_string()}),
                        );
                        agent_state.last_assessment_wake = agent_state.wake_count;
                    }
                }
            }
            Err(e) => {
                audit::append(
                    &audit_dir,
                    &config.agent_id,
                    "assessment-claude-error",
                    &serde_json::json!({"error": e.to_string()}),
                );
                agent_state.last_assessment_wake = agent_state.wake_count;
            }
        }
    }

    } // end if !skip_llm

    // 10. Save state
    state::save(&agent_state, &state_path)?;

    audit::append(
        &audit_dir,
        &config.agent_id,
        "wake-complete",
        &serde_json::json!({
            "invocations": invocation,
            "shift_cost_usd": agent_state.cost.last_wake_usd,
            "trail_entries": agent_state.trail.len(),
            "active_remaining": agent_state.queue.active.len(),
        }),
    );

    Ok(0)
}

/// Spawn a background wake for a target agent when response_requested is true.
/// Fire-and-forget: the current wake doesn't block on the target's wake.
pub fn auto_wake_target(hex_dir: &Path, target_id: &str, sender_id: &str, audit_dir: &Path) {
    let charter_path = hex_dir.join(format!("projects/{}/charter.yaml", target_id));
    if !charter_path.exists() {
        eprintln!(
            "[{}] SKIP auto-wake: target '{}' has no charter",
            sender_id, target_id
        );
        return;
    }
    let binary = hex_dir.join(".hex/bin/hex-agent");
    if !binary.exists() {
        eprintln!(
            "[{}] SKIP auto-wake: hex-agent binary not found at {}",
            sender_id,
            binary.display()
        );
        return;
    }
    let trigger = format!("inbox-from-{}", sender_id);
    match std::process::Command::new(&binary)
        .arg("wake")
        .arg(target_id)
        .arg("--trigger")
        .arg(&trigger)
        .env("HEX_DIR", hex_dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(_) => {
            audit::append(
                audit_dir,
                sender_id,
                "auto-wake-spawned",
                &serde_json::json!({
                    "target": target_id,
                    "trigger": trigger,
                }),
            );
        }
        Err(e) => {
            eprintln!(
                "[{}] auto-wake FAILED for '{}': {e}",
                sender_id, target_id
            );
            audit::append(
                audit_dir,
                sender_id,
                "auto-wake-failed",
                &serde_json::json!({
                    "target": target_id,
                    "error": e.to_string(),
                }),
            );
        }
    }
}

pub fn compute_action_hash(agent_id: &str, trail_type: &str, detail: &serde_json::Value) -> String {
    let sorted_detail = if let Some(obj) = detail.as_object() {
        let mut keys: Vec<&String> = obj.keys().collect();
        keys.sort();
        let sorted: serde_json::Map<String, serde_json::Value> =
            keys.iter().map(|k| (k.to_string(), obj[*k].clone())).collect();
        serde_json::to_string(&serde_json::Value::Object(sorted)).unwrap_or_default()
    } else {
        detail.to_string()
    };
    let input = format!("{}:{}:{}", agent_id, trail_type, sorted_detail);
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let result = hasher.finalize();
    result.iter().map(|b| format!("{b:02x}")).collect::<String>()[..16].to_string()
}

pub fn check_and_handle_loop(
    agent_state: &mut crate::types::AgentState,
    new_entries: &[crate::types::TrailEntry],
    interval_seconds: u64,
    hex_dir: &Path,
    audit_dir: &Path,
) -> bool {
    let now_unix = Utc::now().timestamp() as u64;

    for entry in new_entries {
        if entry.entry_type == "observe" || entry.entry_type == "verify" {
            let hash = compute_action_hash(&agent_state.agent_id, &entry.entry_type, &entry.detail);
            agent_state.recent_action_hashes.push((hash, now_unix));
        }
    }

    let prune_cutoff = now_unix.saturating_sub(interval_seconds * 10);
    agent_state.recent_action_hashes.retain(|(_, ts)| *ts >= prune_cutoff);

    let loop_cutoff = now_unix.saturating_sub(interval_seconds * 6);
    let hashes_snapshot: Vec<String> = agent_state
        .recent_action_hashes
        .iter()
        .map(|(h, _)| h.clone())
        .collect();

    for candidate_hash in &hashes_snapshot {
        let count = agent_state
            .recent_action_hashes
            .iter()
            .filter(|(h, ts)| h == candidate_hash && *ts >= loop_cutoff)
            .count();
        if count >= 3 {
            let home = std::env::var("HOME").unwrap_or_default();
            let halt_path = format!("{}/.hex-{}-HALT-loop", home, agent_state.agent_id);
            let _ = std::fs::write(&halt_path, "Loop detected: same action repeated 3x. Manual review required.");

            let emit_script = hex_dir.join(".hex/bin/hex-emit.sh");
            let payload = serde_json::json!({
                "agent_id": agent_state.agent_id,
                "action_hash": candidate_hash,
                "count": count,
            });
            let _ = std::process::Command::new(&emit_script)
                .arg("hex.agent.loop.detected")
                .arg(payload.to_string())
                .status();

            audit::append(
                audit_dir,
                &agent_state.agent_id,
                "loop-halt",
                &serde_json::json!({
                    "hash": candidate_hash,
                    "count": count,
                    "action_sample": format!("observe/verify:{}", candidate_hash),
                }),
            );
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::ResponseParseQuality;
    use crate::types::AgentResponse;

    fn valid_agent_response_json() -> String {
        r#"{"trail":[],"queue_updates":{"completed":[],"added_active":[],"moved_to_blocked":[],"parked":[]},"memory_updates":null,"outbound_messages":[],"active_drained":false}"#.to_string()
    }

    #[test]
    fn retry_non_json_first_then_valid_json() {
        let mut call_count = 0usize;
        let outcome = retry_if_empty(
            AgentResponse::default(),
            ResponseParseQuality::Empty,
            "original prompt",
            |retry_prompt| {
                call_count += 1;
                assert!(
                    retry_prompt.contains("Your previous response was NOT valid JSON"),
                    "retry prompt must contain stern reprompt"
                );
                Ok(valid_agent_response_json())
            },
        );
        assert_eq!(call_count, 1, "exactly one retry invocation");
        match outcome {
            RetryOutcome::Parsed { was_retried, quality, .. } => {
                assert!(was_retried, "was_retried must be true");
                assert_eq!(quality, ResponseParseQuality::Clean);
            }
            _ => panic!("expected Parsed outcome"),
        }
    }

    #[test]
    fn retry_non_json_first_non_json_second_gives_up() {
        let mut call_count = 0usize;
        let outcome = retry_if_empty(
            AgentResponse::default(),
            ResponseParseQuality::Empty,
            "original prompt",
            |_| {
                call_count += 1;
                Ok("still not json at all".to_string())
            },
        );
        assert_eq!(call_count, 1, "exactly one retry — no third invocation");
        assert!(matches!(outcome, RetryOutcome::Unrecoverable));
    }

    #[test]
    fn retry_no_retry_on_clean_response() {
        let mut call_count = 0usize;
        let outcome = retry_if_empty(
            AgentResponse::default(),
            ResponseParseQuality::Clean,
            "original prompt",
            |_| {
                call_count += 1;
                Ok("this should never be called".to_string())
            },
        );
        assert_eq!(call_count, 0, "zero retries for clean first response");
        assert!(matches!(outcome, RetryOutcome::Parsed { was_retried: false, .. }));
    }
}
