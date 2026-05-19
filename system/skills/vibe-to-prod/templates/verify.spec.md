# Verify: {{PROJECT_NAME}} Refactoring Results

**Mode:** execute

## Context

Project: `{{PROJECT_PATH}}`
Output directory: `{{OUTPUT_DIR}}`
Before-metrics: `{{OUTPUT_DIR}}/before-metrics.json`

Re-run all assessment tools after refactoring to capture after-metrics, then compare against before-metrics to produce an improvement report.

## Tasks

### t-1: Re-run all assessment tools and save after-* files
PENDING

**Spec:** Re-run the full assessment suite against `{{PROJECT_PATH}}` and save all raw outputs to `{{OUTPUT_DIR}}/raw/` with `after-` prefixes. Run these tools in order:

1. `radon cc {{PROJECT_PATH}} -a -s -j` → `{{OUTPUT_DIR}}/raw/after-complexity.json`
2. `radon mi {{PROJECT_PATH}} -s -j` → `{{OUTPUT_DIR}}/raw/after-maintainability.json`
3. `radon raw {{PROJECT_PATH}} -s -j` → `{{OUTPUT_DIR}}/raw/after-raw-metrics.json`
4. `bandit -r {{PROJECT_PATH}} -f json` → `{{OUTPUT_DIR}}/raw/after-security.json`
5. `vulture {{PROJECT_PATH}} --min-confidence 80 --output-format json` → `{{OUTPUT_DIR}}/raw/after-dead-code.json` (if json output unsupported, redirect text output)

Also run the characterization and full test suites and record exit codes:

```bash
cd {{PROJECT_PATH}} && python -m pytest tests/characterization/ -v 2>&1 | tee {{OUTPUT_DIR}}/raw/after-char-tests.txt
cd {{PROJECT_PATH}} && python -m pytest --tb=short 2>&1 | tee {{OUTPUT_DIR}}/raw/after-full-tests.txt
```

**Verify:**
```bash
test -f {{OUTPUT_DIR}}/raw/after-complexity.json && echo "complexity OK"
test -f {{OUTPUT_DIR}}/raw/after-maintainability.json && echo "maintainability OK"
test -f {{OUTPUT_DIR}}/raw/after-security.json && echo "security OK"
test -f {{OUTPUT_DIR}}/raw/after-char-tests.txt && echo "char tests captured"
```

### t-2: Generate after-metrics.json
PENDING

**Spec:** Read `{{OUTPUT_DIR}}/raw/after-complexity.json`, `{{OUTPUT_DIR}}/raw/after-maintainability.json`, and `{{OUTPUT_DIR}}/raw/after-security.json`. Produce `{{OUTPUT_DIR}}/after-metrics.json` using the **same nested schema** as `{{OUTPUT_DIR}}/before-metrics.json` (produced by assess.spec.md t-3):

```json
{
  "project_path": "{{PROJECT_PATH}}",
  "project_name": "{{PROJECT_NAME}}",
  "timestamp": "<ISO timestamp>",
  "phase": "after",
  "metrics": {
    "total_functions": <int>,
    "max_cyclomatic_complexity": <float>,
    "avg_cyclomatic_complexity": <float>,
    "functions_cc_above_10": <int>,
    "functions_cc_above_15": <int>,
    "functions_cc_above_25": <int>,
    "avg_maintainability_index": <float>,
    "min_maintainability_index": <float>,
    "modules_mi_below_20": <int>,
    "total_modules": <int>,
    "total_loc": <int>,
    "total_sloc": <int>,
    "comment_ratio": <float>,
    "security_high_findings": <int>,
    "security_medium_findings": <int>,
    "dead_code_items_90pct": <int>
  }
}
```

**IMPORTANT:** This schema must exactly match `before-metrics.json` so that `compare_metrics.py` can diff them correctly. Use `phase: "after"` (not `"before"`). Use the same field names under `"metrics"`. Use Python (stdlib only) to compute all values from the raw after-* files.

**Verify:**
```bash
python3 -c "
import json
data = json.load(open('{{OUTPUT_DIR}}/after-metrics.json'))
assert data.get('phase') == 'after', 'missing phase=after'
m = data.get('metrics', {})
assert 'avg_cyclomatic_complexity' in m, 'missing avg_cyclomatic_complexity'
assert 'avg_maintainability_index' in m, 'missing avg_maintainability_index'
assert 'security_high_findings' in m, 'missing security_high_findings'
print('after-metrics.json OK (nested schema)')
print(f\"  avg_cc={m['avg_cyclomatic_complexity']:.1f}  avg_mi={m['avg_maintainability_index']:.1f}\")
print(f\"  security={m['security_high_findings']}H / {m['security_medium_findings']}M\")
"
```

### t-3: Run comparison script and produce improvement report
PENDING

**Spec:** Run the comparison script against before and after metrics to produce the improvement report:

```bash
python3 {{SKILL_DIR}}/scripts/compare_metrics.py {{OUTPUT_DIR}}
```

Then perform the final verification checklist. Read `{{OUTPUT_DIR}}/raw/after-char-tests.txt` and `{{OUTPUT_DIR}}/raw/after-full-tests.txt` to extract test pass/fail status. Report results for each check:

| Check | Result |
|-------|--------|
| Characterization tests pass | PASS / FAIL |
| Full test suite passes | PASS / FAIL |
| Avg CC decreased | PASS / FAIL / NO CHANGE |
| Avg MI increased | PASS / FAIL / NO CHANGE |
| Security issues unchanged or decreased | PASS / FAIL |

If any check is FAIL, report the details clearly so the engineer can investigate.

**Verify:**
```bash
test -f {{OUTPUT_DIR}}/improvement-report.md && echo "report EXISTS"
grep -c "Avg Complexity" {{OUTPUT_DIR}}/improvement-report.md
grep -c "Avg Maintainability" {{OUTPUT_DIR}}/improvement-report.md
```
