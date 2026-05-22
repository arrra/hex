use crate::types::{AgentState, Charter};

fn serialize_or_loud(label: &str, val: &impl serde::Serialize) -> String {
    serde_json::to_string_pretty(val).unwrap_or_else(|e| {
        eprintln!("[harness][prompt] serialization failed for {label}: {e}");
        String::new()
    })
}

pub fn build(
    charter_text: &str,
    state: &AgentState,
    trigger: &str,
    payload: &str,
    principles_text: Option<&str>,
    context_files: Option<&str>,
    catalog: Option<&str>,
) -> String {
    let trail_recent: Vec<_> = state.trail.iter().rev().take(20).collect();
    let trail_json = serialize_or_loud("trail", &trail_recent);
    let queue_json = serialize_or_loud("queue", &state.queue);
    let memory_json = serialize_or_loud("memory", &state.memory);
    let inbox_json = serialize_or_loud("inbox", &state.inbox);
    let cost_json = serialize_or_loud("cost", &state.cost);
    let initiatives_json = serialize_or_loud("initiatives", &state.initiatives);

    let principles_section = principles_text
        .map(|p| format!("\n---\n\n{p}\n"))
        .unwrap_or_else(String::new);

    // Catalog section is prepended before charter context files when present.
    let catalog_block = catalog
        .filter(|s| !s.is_empty())
        .map(|c| format!("\n## Capability Catalog\n\n```json\n{c}\n```\n"))
        .unwrap_or_default();

    let ctx_body = context_files.filter(|s| !s.is_empty()).unwrap_or("");
    let context_section = if catalog_block.is_empty() && ctx_body.is_empty() {
        String::new()
    } else {
        format!("\n---\n\n# Context Files\n{catalog_block}{ctx_body}\n")
    };

    format!(
        r#"# Charter

{charter_text}
{principles_section}{context_section}
---

# Wake Context

- Trigger: {trigger}
- Payload: {payload}
- Wake count: {wake_count}
- Last wake: {last_wake}

## Queue

```json
{queue_json}
```

## Inbox (unread messages)

```json
{inbox_json}
```

## Recent trail (last 20 entries)

```json
{trail_json}
```

## Working memory

```json
{memory_json}
```

## Initiatives

```json
{initiatives_json}
```

## Cost

```json
{cost_json}
```

---

# Instructions

Work your active queue. For each item, use the action types below to observe, analyze, decide, and act. You choose the workflow -- the harness logs everything.

When you're done with all active items (or have moved them to blocked/parked), set `active_drained: true`.

## Action types

Each trail entry must have `type` and `detail` fields. Required detail fields per type:

| Type | Required fields |
|------|----------------|
| observe | what, noted |
| find | finding, evidence |
| decide | decision, alternatives, reasoning |
| act | action, result |
| verify | check, evidence, status |
| delegate | initiative_id, to, context |
| park | item_id, reason, resume_condition |
| reframe | abandoned, reason, new_framing |
| message_sent | to, subject, body |

### Mechanical acts require evidence

If an `act` describes a mechanical operation — git push, git tag, dispatching a BOI spec, writing a file, shipping a release — its `detail` MUST include an `evidence` object the harness can verify:

```
detail.evidence = {{"type": "git_tag", "value": "v0.17.3", "repo": "..."}}
detail.evidence = {{"type": "git_push", "repo": "...", "ref": "main"}}
detail.evidence = {{"type": "boi_dispatch", "spec_id": "S1234"}}
detail.evidence = {{"type": "file_written", "path": "/abs/path"}}
```

An `act` claiming a mechanical operation without verifiable evidence is recorded as UNVERIFIED — it does NOT count as done. Never claim a mechanical action you did not actually perform; the harness checks the evidence against reality.

## Keeping the response compact

A truncated response loses work — the harness salvages what it can, but compact responses never truncate. Be concise.

- Each `detail` field value must be terse: ~1 sentence, 200 chars max. No multi-paragraph reasoning.
- `alternatives` is a short list of brief phrases (≤1 line each), not paragraphs.
- Emit at most 12 trail entries per response. If the shift produced more, keep decide/act/park/verify entries and collapse routine observe/find entries into a single summary observe entry.

## Messaging other agents

Send messages via `outbound_messages`. Each message has these fields:

```json
{{
  "to": "agent-id",
  "subject": "...",
  "body": "...",
  "response_requested": true
}}
```

**When you need a reply to proceed, set `response_requested: true`.** The harness will wake the target agent immediately so they can read your message and respond in the same cycle. Use this for:
- Reviews/sign-offs you're blocked on (e.g. security review, release approval)
- Questions where the answer determines your next action
- Collaboration that needs a back-and-forth

If you're just informing another agent (status update, FYI), leave `response_requested: false` — they'll see it on their next scheduled wake.

**Do not park work waiting for an agent reply when you could wake them now.** Moving items to blocked and hoping the other agent wakes up is the slow path. `response_requested: true` is the fast path.

## Response format

Return a single JSON object (AgentResponse) with exactly these fields:

```json
{{
  "outbound_messages": [
    {{"to": "agent-id", "subject": "...", "body": "...", "response_requested": false}}
  ],
  "queue_updates": {{
    "completed": ["t-1"],
    "added_active": [],
    "moved_to_blocked": [],
    "parked": []
  }},
  "active_drained": true,
  "memory_updates": {{"key": "value"}},
  "trail": [
    {{"ts": "ISO-8601", "type": "observe", "detail": {{"what": "...", "noted": "..."}}, "queue_item": "t-1"}}
  ]
}}
```

Emit `trail` LAST. If your response is cut off, the fields above it (messages, queue changes) still take effect.

Respond ONLY with the JSON object. No prose before or after.
"#,
        wake_count = state.wake_count,
        last_wake = state
            .last_wake
            .map(|t| t.to_rfc3339())
            .unwrap_or_else(|| "never".into()),
    )
}

pub fn build_assessment(
    charter: &Charter,
    state: &AgentState,
    principles_text: Option<&str>,
) -> String {
    let trail_recent: Vec<_> = state.trail.iter().rev().take(50).collect();
    let trail_json = serialize_or_loud("trail", &trail_recent);
    let memory_json = serialize_or_loud("memory", &state.memory);
    let cost_json = serialize_or_loud("cost", &state.cost);
    let cadence_json = serialize_or_loud("cadence_overrides", &state.cadence_overrides);

    let kpis_section = charter
        .kpis
        .as_ref()
        .map(|kpis| {
            kpis.iter()
                .map(|k| format!("- {k}"))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_else(|| "(none defined)".to_string());

    let responsibilities_section = charter
        .wake
        .responsibilities
        .iter()
        .map(|r| {
            let effective = state
                .cadence_overrides
                .get(&r.name)
                .copied()
                .or(r.interval);
            let override_note = if state.cadence_overrides.contains_key(&r.name) {
                format!(" (overridden from charter default {}s)", r.interval.map_or_else(|| "event".to_string(), |i| i.to_string()))
            } else {
                String::new()
            };
            let interval_str = effective.map_or_else(|| "event-triggered".to_string(), |i| format!("every {}s", i));
            format!(
                "- *{}*: {} {} — {}",
                r.name,
                interval_str,
                override_note,
                r.description.trim()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let principles_section = principles_text
        .map(|p| format!("\n---\n\n{p}\n"))
        .unwrap_or_else(String::new);

    format!(
        r#"# Self-Assessment Phase

You are {name} ({role}).
{principles_section}
## Your Objective

{objective}

## Your KPIs

{kpis_section}

## Current Responsibility Cadences

{responsibilities_section}

## Active Cadence Overrides

```json
{cadence_json}
```

## Recent Trail (last 50 entries)

```json
{trail_json}
```

## Working Memory

```json
{memory_json}
```

## Cost

```json
{cost_json}
```

---

# Instructions

Step back and assess your own effectiveness. This is not a work phase — this is reflection.

Consider:
1. Are your current cadences right? Too frequent wastes budget. Too infrequent misses opportunities.
2. Is your strategy working? Look at your trail — are you making progress on KPIs or spinning?
3. What should you do differently next shift?

For each observation, log a trail entry with type "assess".

## Response format

Return a single JSON object (AssessmentResponse):

```json
{{{{
  "trail": [
    {{{{"ts": "ISO-8601", "type": "assess", "detail": {{{{"area": "cadence", "finding": "experiment-execution runs every 6h but experiments take 3-5 days to produce signal", "adjustment": "moving to 24h"}}}}}}}}
  ],
  "cadence_overrides": [
    {{{{"responsibility": "experiment-execution", "old_interval": 21600, "new_interval": 86400, "reason": "experiments need days not hours to produce signal"}}}}
  ],
  "strategy_updates": {{{{"key": "value to persist in working memory"}}}},
  "recommendations": ["any recommendations for the fleet or for Mike"]
}}}}
```

Rules:
- Every cadence change MUST have a reason grounded in evidence from your trail.
- If everything is working, say so and return empty overrides. Don't change for the sake of change.
- Strategy updates persist to your working memory for future wakes.

Respond ONLY with the JSON object. No prose before or after.
"#,
        name = charter.name,
        role = charter.role,
        objective = charter.objective.as_deref().unwrap_or("(none)"),
    )
}
