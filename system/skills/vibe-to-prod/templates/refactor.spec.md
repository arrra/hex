# Refactor: {{MODULE_NAME}}

**Mode:** execute

## Context

Module: `{{MODULE_PATH}}`
Project: `{{PROJECT_PATH}}`
Before-metrics: CC={{CC}}, MI={{MI}}
Characterization tests: `{{PROJECT_PATH}}/tests/characterization/test_{{MODULE_NAME}}.py`

Refactor this module to reduce cyclomatic complexity and improve maintainability.
Constraints apply to every task below.

**Global Constraints:**
- Do NOT change any function signatures or return types
- Do NOT add new dependencies (no new imports from outside the project)
- Do NOT modify any files outside `{{MODULE_PATH}}`
- All characterization tests must pass after every change

## Tasks

### t-1: Verify characterization tests pass (pre-check)
PENDING

**Spec:** Run the characterization tests for this module to confirm the baseline is green before any changes are made. If tests fail, do NOT proceed — mark this task FAILED with the failure output.

**Verify:**
```bash
cd {{PROJECT_PATH}} && python -m pytest tests/characterization/test_{{MODULE_NAME}}.py -v 2>&1 | tail -20
```

### t-2: Refactor to reduce cyclomatic complexity
PENDING

**Spec:** Refactor `{{MODULE_PATH}}` to reduce cyclomatic complexity. Apply these techniques:
1. Extract complex conditional logic into named helper functions (one concept per function)
2. Replace nested if/else chains with early returns or guard clauses
3. Extract repeated logic into shared helpers within the same module
4. Use list/dict comprehensions instead of loops where appropriate
5. Break down functions with CC > 10 into smaller, focused functions

Do NOT change function signatures, return types, or public API. Do NOT add new imports outside the standard library or existing project dependencies. Do NOT modify any file other than `{{MODULE_PATH}}`.

**Verify:**
```bash
cd {{PROJECT_PATH}} && python -m pytest tests/characterization/test_{{MODULE_NAME}}.py -v 2>&1 | tail -10
radon cc {{MODULE_PATH}} -a -s
```

### t-3: Verify behavior preserved and measure improvement
PENDING

**Spec:** Run the characterization tests and capture the new metrics. Report both before and after values.

Before: CC={{CC}}, MI={{MI}}

Run:
```bash
cd {{PROJECT_PATH}} && python -m pytest tests/characterization/test_{{MODULE_NAME}}.py -v
radon cc {{MODULE_PATH}} -a -s
radon mi {{MODULE_PATH}} -s
```

Confirm:
- All characterization tests pass (zero failures)
- Max CC has decreased from {{CC}}
- MI has increased from {{MI}}

If characterization tests fail, revert the refactoring and mark this task FAILED.

**Verify:**
```bash
cd {{PROJECT_PATH}} && python -m pytest tests/characterization/test_{{MODULE_NAME}}.py -v 2>&1 | grep -E "passed|failed|error"
radon cc {{MODULE_PATH}} -a -s 2>&1 | tail -5
radon mi {{MODULE_PATH}} -s 2>&1
```

### t-4: Security regression check
PENDING

**Spec:** Run bandit on the refactored module and compare against the pre-refactoring baseline. No new HIGH or MEDIUM severity findings may be introduced.

If new HIGH or MEDIUM findings are present that were not in the original module, revert the change that introduced them and mark this task FAILED with details.

**Verify:**
```bash
bandit {{MODULE_PATH}} -f json 2>/dev/null | python3 -c "
import json, sys
data = json.load(sys.stdin)
results = data.get('results', [])
high_med = [r for r in results if r['issue_severity'] in ('HIGH', 'MEDIUM')]
print(f'HIGH/MEDIUM findings: {len(high_med)}')
for r in high_med:
    print(f\"  {r['issue_severity']}: {r['issue_text']} at {r['filename']}:{r['line_number']}\")
"
```

### t-5: Full test suite regression check
PENDING

**Spec:** Run the complete project test suite to catch any regressions beyond what characterization tests cover. All tests that passed before refactoring must still pass.

If any previously-passing tests now fail, investigate and fix the regression before marking this task complete.

**Verify:**
```bash
cd {{PROJECT_PATH}} && python -m pytest --tb=short 2>&1 | tail -20
```
