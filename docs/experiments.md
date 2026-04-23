# Experiments

**Status:** Canonical reference  
**Date:** 2026-04-22  
**Relates to:** architecture.md, hex-events.md, multi-agent.md

---

## 1. Why experiments exist

The BOI optimizer diagnosed every bottleneck correctly across 11 wakes and shipped zero verified improvements. The problem: no mechanism existed to prove that a change actually moved the numbers.

Today, system changes are evaluated by the agent that made them — the same agent that proposed the change. This is not peer review; it is self-congratulation. The agent can assert "this worked" without any measurement.

The experiment primitive enforces the scientific method:
- **Pre-registration**: hypothesis and success criteria are written and locked before the change ships
- **Reproducible measurement**: the same shell commands run before and after; methodology cannot change between measurements
- **Automated verdict**: a numeric comparison renders PASS, FAIL, or INCONCLUSIVE — no narrative required
- **Loud failure**: a change that made things worse exits with code 1 and prints rollback commands

No experiment closes without a data-backed verdict.

---

## 2. Core concepts

**Experiment**: a single YAML file encoding a hypothesis, runnable metrics, success criteria, and the full measurement history. Lives in `experiments/`.

**Metric**: a shell command that returns a single number. The same command runs pre-change (baseline) and post-change (measurement). Results are reproducible because the methodology is locked.

**Primary metric**: the one number that determines PASS/FAIL. One per experiment — prevents p-hacking by shopping metrics after the fact.

**Guardrail metric**: a metric that must NOT regress, even if the primary metric improves. Experiments with a passing primary but a failing guardrail are still VERDICT_FAIL.

**Baseline**: the pre-change values, collected once, then locked. Re-baselining is not allowed — create a new experiment.

**Pre-registration lock**: at baseline time, the runner computes `SHA256(hypothesis + success_criteria)` and writes it to `baseline_locked_sha`. Any modification to those fields after baseline is detected at measure time — the runner refuses to proceed.

**Verdict**: the terminal result after comparing baseline vs post-change values against pre-registered criteria. Always one of `VERDICT_PASS`, `VERDICT_FAIL`, or `VERDICT_INCONCLUSIVE`.

---

## 3. File format reference

Experiments are pure YAML (not frontmatter + markdown). The runner parses, rewrites, and appends to these files — a single parseable format is simpler to maintain than a hybrid.

```yaml
# --- Required at creation ---
id: exp-NNN                  # Assigned by runner on create
title: "Short title ≤80 chars"
state: DRAFT                 # See state machine below
hypothesis: >
  Declarative claim: "changing X reduces Y by Z%"
owner: hex                   # Agent or human who proposed this
created: 2026-04-22

change_description: >
  What change will be (or was) shipped.

time_bound:
  measure_by: 2026-05-22              # Auto-INCONCLUSIVE if not measured by this date
  min_cycles_before_measure: 20       # Don't measure until N BOI cycles post-ship

metrics:
  primary:
    name: my_metric_name              # snake_case identifier
    description: One-line description
    command: |
      sqlite3 ~/.boi/boi.db "SELECT ..."
    direction: lower_is_better        # or higher_is_better

  guardrails:
    - name: guardrail_metric
      description: One-line description
      command: |
        sqlite3 ~/.boi/boi.db "SELECT ..."
      direction: higher_is_better
      max_regression_pct: 5.0         # Max allowed regression as % of baseline

success_criteria:
  primary:
    metric: my_metric_name
    type: percent_improvement
    threshold_pct: 50.0               # Minimum improvement % for PASS
  guardrails:
    - metric: guardrail_metric
      must_not_regress_by_pct: 5.0

rollback_plan:
  description: How to undo the change
  commands:
    - "git revert --no-edit HEAD"
    - "boi daemon restart"

# --- Populated by runner (append-only after BASELINE) ---
baseline_locked_sha: null
baseline: null
post_change: null
verdict: null
```

**Metric command constraints:**
- Must return a single number (float or int) on stdout
- Use relative time windows (`datetime('now', '-30 days')`), not absolute dates — same command must work both pre and post
- Non-zero exit = measurement error; runner aborts without corrupting the file

**Cost sentinel:** Add `echo "__experiment_window_cost__"` as a guardrail command to auto-track API spend during the experiment window. The runner substitutes the computed cost from `.hex/cost/ledger.jsonl`.

---

## 4. Lifecycle state machine

```
                  create
  (file) ──────────────────→ DRAFT
                                │
                                │ hex experiment baseline
                                ▼
                           BASELINE ──── (re-baseline blocked)
                                │
                                │ hex experiment activate
                                ▼
                            ACTIVE
                                │
                                │ hex experiment measure
                                ▼
                          MEASURING
                                │
                          hex experiment verdict
                    ┌───────────┼───────────────┐
                    ▼           ▼               ▼
             VERDICT_PASS  VERDICT_FAIL  VERDICT_INCONCLUSIVE
              (exit 0)      (exit 1)       (exit 2)
```

All `VERDICT_*` states are terminal — no transitions out.

| State | Meaning |
|-------|---------|
| `DRAFT` | File written and validated; no measurements yet |
| `BASELINE` | Pre-change metrics collected; hypothesis + criteria locked |
| `ACTIVE` | Change shipped; git commit recorded |
| `MEASURING` | Post-change metrics collected; verdict pending |
| `VERDICT_PASS` | Primary metric met threshold; all guardrails held |
| `VERDICT_FAIL` | Primary metric missed threshold, or a guardrail failed |
| `VERDICT_INCONCLUSIVE` | `measure_by` date exceeded before post-change measurement |

---

## 5. Runner CLI reference

```
hex experiment <subcommand> [options]
```

### `hex experiment create <file>`

Validates the YAML file against the schema. Assigns a sequential ID (`exp-NNN`) if `id` is empty. Sets `state: DRAFT`. Writes to `experiments/<id>.yaml`.

```bash
hex experiment create /tmp/my-exp.yaml
# → experiments/exp-004.yaml written (state: DRAFT)
```

Validation checks: required fields present, `metrics.primary.command` non-empty, `time_bound.measure_by` is a future date, measurement fields (`baseline_locked_sha`, `baseline`, `post_change`, `verdict`) are null.

Exit: 0 = success, 1 = validation error with field-level messages.

---

### `hex experiment baseline <id>`

Runs all metric commands (primary + guardrails), records results, computes `baseline_locked_sha`, transitions to `BASELINE`.

```bash
hex experiment baseline exp-001
# Running primary metric: avg_failed_spec_duration_minutes
#   → 47.32
# Running guardrail: completion_rate_pct
#   → 61.50
# Baseline locked. SHA: a3f9...
# State: BASELINE
```

Guard: if `baseline` already set, refuses to overwrite. Re-baselining is blocked — create a new experiment.

---

### `hex experiment activate <id>`

Records that the change has shipped. Captures current git HEAD. Emits `experiment.activated` to hex-events (triggers auto-measure scheduling). Transitions to `ACTIVE`.

```bash
hex experiment activate exp-001
# Records commit: abc123...
# State: ACTIVE
```

---

### `hex experiment measure <id>`

Runs all metric commands again. Validates `baseline_locked_sha` against current hypothesis + success_criteria — hard error if tampered. Records post-change values. Transitions to `MEASURING`.

```bash
hex experiment measure exp-001
# Running primary metric: avg_failed_spec_duration_minutes
#   → 19.40
# State: MEASURING
```

---

### `hex experiment verdict <id>`

Computes verdict from baseline vs post-change using pre-registered criteria.

**Verdict algorithm:**
1. `primary_delta_pct = (post - pre) / pre * 100` (signed, direction-adjusted)
2. Primary PASSES if `primary_delta_pct >= threshold_pct`
3. Each guardrail: check `abs(regression) <= max_regression_pct`
4. `VERDICT_PASS` ← primary passes AND all guardrails pass
5. `VERDICT_FAIL` ← primary fails, OR primary passes but any guardrail fails
6. `VERDICT_INCONCLUSIVE` ← `measure_by` exceeded without post-change measurement

```
EXPERIMENT VERDICT: exp-001
────────────────────────────────────────────────
PRIMARY METRIC: avg_failed_spec_duration_minutes
  Baseline:    47.32 min
  Post-change: 19.40 min
  Delta:       -59.0% improvement
  Threshold:   -50.0% required
  Result:      ✓ PASS

GUARDRAIL: completion_rate_pct
  Baseline:    61.50%
  Post-change: 60.10%
  Regression:  -2.3% (max allowed: -5.0%)
  Result:      ✓ PASS

VERDICT: ✓ PASS
────────────────────────────────────────────────
```

On `VERDICT_FAIL`: exits with code 1, prints rollback commands from `rollback_plan.commands`, emits `experiment.verdict_fail` to hex-events.

Exit codes: 0 = PASS, 1 = FAIL, 2 = INCONCLUSIVE, 3 = runner error.

---

### `hex experiment status [id]`

Without id: list all experiments.
```
ID       TITLE                        STATE          PRIMARY DELTA   DAYS ACTIVE
exp-001  BOI Early Kill               VERDICT_PASS   -59.0%          9
exp-002  Agent Wake Frequency         ACTIVE         (unmeasured)    3
exp-003  BOI Spec Validation          BASELINE       (unmeasured)    0
```

Stale experiments (ACTIVE but `measure_by` approaching) are marked with `!`.

With id: full detail + formatted verdict.  
With `--json`: machine-readable output for agent consumption.

---

## 6. Integration points

### Agent integration

Agents propose experiments via a new charter action type:

```yaml
- type: experiment_propose
  file: /tmp/exp-draft.yaml
  rationale: "BOI is wasting compute on stalled specs"
```

The harness runs `hex experiment create <file>`, records the experiment ID in the agent's state (`active_experiments: [exp-001]`), and feeds validation errors back on the agent's next wake.

Agents do NOT run `baseline`, `activate`, or `measure` — those require human or hex-events triggers. This separation prevents agents from closing experiments they authored.

Agents query experiment status for reasoning:
```bash
hex experiment status exp-001 --json
```

Harness rate-limits proposals: agents with ≥3 active (non-terminal) experiments cannot propose more. Duplicate hypothesis (exact match) is rejected.

### BOI integration

BOI specs can declare a dependency on an experiment verdict:

```markdown
### t-5: Enable BOI early-kill in production
PENDING

**Blocked by experiment:** exp-001 (must reach VERDICT_PASS before enabling)
```

The BOI daemon checks this annotation before dispatching. If the named experiment is not `VERDICT_PASS`, the task stays PENDING.

For optimizer specs that exist solely to prove a change worked, the final task wraps the verdict command:

```markdown
**Verify:**
hex experiment status exp-001 --json | python3 -c "
import sys, json; d=json.load(sys.stdin)
exit(0 if d['state']=='VERDICT_PASS' else 1)"
```

### hex-events integration

`hex experiment activate` emits `experiment.activated`. A hex-events policy schedules auto-measurement after `time_bound.measure_by`:

```yaml
# ~/.hex/hex-events-policies/experiment-auto-measure.yaml
rules:
  - name: schedule-measure
    trigger: { event: experiment.activated }
    actions:
      - type: emit
        event: experiment.measure_due
        delay: "{{ event.seconds_until_measure_by }}s"
        payload: { experiment_id: "{{ event.experiment_id }}" }
  - name: run-measure
    trigger: { event: experiment.measure_due }
    actions:
      - type: shell
        command: "hex experiment measure {{ event.experiment_id }}"
```

`VERDICT_FAIL` emits `experiment.verdict_fail`, triggering a notification policy that prints the experiment title, primary delta, and rollback commands.

Full event table:

| Event | Emitted when |
|-------|-------------|
| `experiment.created` | `create` |
| `experiment.baseline_collected` | `baseline` |
| `experiment.activated` | `activate` |
| `experiment.measured` | `measure` |
| `experiment.verdict_pass` | `verdict` → PASS |
| `experiment.verdict_fail` | `verdict` → FAIL |
| `experiment.verdict_inconclusive` | `verdict` → INCONCLUSIVE |

### Cost integration

At `measure` time, the runner reads `.hex/cost/ledger.jsonl` and sums API spend from `activated_at` to `now`. Written to `post_change.experiment_window_cost_usd`. Optionally surfaced as a guardrail via the `__experiment_window_cost__` sentinel.

### Telemetry integration

All events land in `.hex/telemetry/events.db` via the existing `emit.py` path. Useful dashboard queries:

```sql
-- Active (non-terminal) experiments
SELECT json_extract(payload, '$.experiment_id') AS id, event_type, created_at
FROM events
WHERE event_type LIKE 'experiment.%'
  AND event_type NOT LIKE 'experiment.verdict%'
ORDER BY created_at DESC;

-- Stale: activated but never measured after 7 days
SELECT json_extract(a.payload, '$.experiment_id') AS id,
       JULIANDAY('now') - JULIANDAY(a.created_at) AS days_since_activation
FROM events a
WHERE a.event_type = 'experiment.activated'
  AND NOT EXISTS (
    SELECT 1 FROM events m
    WHERE m.event_type = 'experiment.measured'
      AND json_extract(m.payload, '$.experiment_id') = json_extract(a.payload, '$.experiment_id')
  )
  AND JULIANDAY('now') - JULIANDAY(a.created_at) > 7;
```

### Fleet-wide view

The hex-ops agent wake includes `hex experiment status` in its context when ≥1 non-terminal experiment exists. The doctor watchdog flags stale experiments as part of health checks.

---

## 7. Examples

### Full lifecycle: BOI early-kill

```bash
# 1. Create
hex experiment create experiments/exp-001-boi-early-kill.yaml
# → state: DRAFT

# 2. Collect baseline (before shipping the change)
hex experiment baseline exp-001
# → records avg_failed_spec_duration_minutes=47.32, completion_rate_pct=61.50
# → hypothesis + success_criteria locked
# → state: BASELINE

# 3. Ship the change (add early-kill to BOI daemon)
git commit -am "feat: BOI early-kill at iteration 3 with zero progress"
hex experiment activate exp-001
# → records commit SHA, emits experiment.activated
# → state: ACTIVE

# 4. Wait for min_cycles_before_measure (20 BOI cycles), or time_bound
# hex-events auto-fires measure on 2026-05-22, or run manually:
hex experiment measure exp-001
# → state: MEASURING

# 5. Render verdict
hex experiment verdict exp-001
# → VERDICT_PASS (exit 0) or VERDICT_FAIL (exit 1)
```

### Checking experiment status from an agent

```python
import subprocess, json

result = subprocess.run(
    ["hex", "experiment", "status", "exp-001", "--json"],
    capture_output=True, text=True
)
data = json.loads(result.stdout)

if data["state"] == "VERDICT_PASS":
    # safe to build on this change
    pass
elif data["state"] == "VERDICT_FAIL":
    # propose rollback
    pass
```

### Existing experiment files

| File | Hypothesis |
|------|-----------|
| `exp-001-boi-early-kill.yaml` | Early-kill at iteration 3 reduces failed spec duration 50%+ |
| `exp-002-agent-wake-frequency.yaml` | Halving hex-ops wake frequency doesn't degrade action throughput |
| `exp-003-boi-spec-validation.yaml` | Pre-iteration backup + auto-repair reduces corruption failures 80%+ |

---

## 8. Anti-patterns

**Changing the hypothesis after baseline.**  
The runner detects `baseline_locked_sha` mismatches and refuses to measure. If your hypothesis was wrong, create a new experiment — don't mutate the locked one.

**Using absolute dates in metric commands.**  
`WHERE created_at > '2026-04-01'` means the pre-change and post-change windows are different sizes. Use `datetime('now', '-30 days')` so the window is always the same relative span.

**Multiple primary metrics.**  
One primary metric per experiment. If you're tempted to add a second, you're hedging — one of them is the real hypothesis, the other is an escape hatch. Pick one.

**Measuring too soon.**  
`min_cycles_before_measure` exists for a reason. The system needs enough cycles post-change to generate statistically meaningful signal. A change to BOI measured after 2 cycles is noise.

**Closing experiments with prose.**  
"It seems to have improved" is not a verdict. Do not mark an experiment done without running `hex experiment verdict`. The exit code IS the verdict.

**Agent self-closing.**  
Agents propose experiments but cannot baseline, activate, or measure them. This separation exists because an agent cannot objectively measure a change it authored and has incentive to validate.

**Too many concurrent experiments.**  
The harness caps agents at 3 active experiments. Humans should apply similar discipline — too many open experiments means none get properly measured.

**Forgetting rollback plans.**  
Every experiment file requires a `rollback_plan.commands` block. Write it before you ship the change, not after you discover the verdict is FAIL.
