# Assess: {{PROJECT_NAME}}

**Mode:** execute
**Workspace:** docker
**Target repo:** {{PROJECT_PATH}}

## Context

Automated assessment of `{{PROJECT_PATH}}` using static analysis, dependency mapping,
git history, and security scanning. This is Phase 1 of the vibe-to-production pipeline.

Supports mixed-language codebases (Python + bash). Python files get radon/vulture/bandit/ruff.
Bash scripts get shellcheck. All outputs go to `{{OUTPUT_DIR}}/raw/`.

**CRITICAL: All file writes MUST use absolute paths.**

**Target:** `{{PROJECT_PATH}}`
**Output:** `{{OUTPUT_DIR}}/`

## Tasks

### t-1: Install tools and run Python static analysis
PENDING

**Spec:** Install Python analysis tools and run against all `.py` files in `{{PROJECT_PATH}}`.
Skip pydeps if the project isn't a Python package (no setup.py/pyproject.toml).

```bash
#!/usr/bin/env bash
set -uo pipefail

pip install vulture radon bandit ruff 2>/dev/null || pip3 install vulture radon bandit ruff

mkdir -p "{{OUTPUT_DIR}}/raw"

# Check if any .py files exist
PY_COUNT=$(find "{{PROJECT_PATH}}" -name "*.py" -type f | wc -l)
if [ "$PY_COUNT" -eq 0 ]; then
  echo "No Python files found. Skipping Python analysis."
  echo '{}' > "{{OUTPUT_DIR}}/raw/complexity.json"
  echo '{}' > "{{OUTPUT_DIR}}/raw/maintainability.json"
  echo '{}' > "{{OUTPUT_DIR}}/raw/raw_metrics.json"
  echo '{"results":[]}' > "{{OUTPUT_DIR}}/raw/security.json"
  exit 0
fi

echo "=== [1/5] Vulture: Dead Code Detection ($PY_COUNT .py files) ==="
vulture "{{PROJECT_PATH}}" --min-confidence 60 > "{{OUTPUT_DIR}}/raw/vulture_all.txt" 2>&1 || true
vulture "{{PROJECT_PATH}}" --min-confidence 80 > "{{OUTPUT_DIR}}/raw/vulture_high.txt" 2>&1 || true
vulture "{{PROJECT_PATH}}" --min-confidence 90 > "{{OUTPUT_DIR}}/raw/vulture_critical.txt" 2>&1 || true

echo "=== [2/5] Radon: Complexity & Maintainability ==="
radon cc "{{PROJECT_PATH}}" -a -s -j > "{{OUTPUT_DIR}}/raw/complexity.json" 2>&1 || true
radon mi "{{PROJECT_PATH}}" -s -j > "{{OUTPUT_DIR}}/raw/maintainability.json" 2>&1 || true
radon raw "{{PROJECT_PATH}}" -s -j > "{{OUTPUT_DIR}}/raw/raw_metrics.json" 2>&1 || true
radon cc "{{PROJECT_PATH}}" -a -s -n C > "{{OUTPUT_DIR}}/raw/complex_functions.txt" 2>&1 || true

echo "=== [3/5] Pydeps: Dependencies & Cycles ==="
if [ -f "{{PROJECT_PATH}}/setup.py" ] || [ -f "{{PROJECT_PATH}}/pyproject.toml" ]; then
  pip install pydeps 2>/dev/null || pip3 install pydeps 2>/dev/null
  pydeps "{{PROJECT_PATH}}" --no-show --no-output -T json > "{{OUTPUT_DIR}}/raw/deps.json" 2>&1 || true
  pydeps "{{PROJECT_PATH}}" --show-cycles > "{{OUTPUT_DIR}}/raw/cycles.txt" 2>&1 || true
else
  echo "Not a Python package (no setup.py/pyproject.toml). Skipping pydeps."
fi

echo "=== [4/5] Bandit: Security Scan ==="
bandit -r "{{PROJECT_PATH}}" -f json > "{{OUTPUT_DIR}}/raw/security.json" 2>&1 || true
bandit -r "{{PROJECT_PATH}}" -f txt > "{{OUTPUT_DIR}}/raw/security.txt" 2>&1 || true

echo "=== [5/5] Ruff: Lint Check ==="
ruff check "{{PROJECT_PATH}}" --output-format json > "{{OUTPUT_DIR}}/raw/ruff.json" 2>&1 || true
ruff check "{{PROJECT_PATH}}" > "{{OUTPUT_DIR}}/raw/ruff.txt" 2>&1 || true

echo "=== Python analysis complete ($PY_COUNT files) ==="
ls -la "{{OUTPUT_DIR}}/raw/"
```

**Verify:**
```bash
test -d {{OUTPUT_DIR}}/raw && echo "OUTPUT_DIR exists"
test -f {{OUTPUT_DIR}}/raw/complexity.json && echo "complexity OK"
test -f {{OUTPUT_DIR}}/raw/security.json && echo "security OK"
ls {{OUTPUT_DIR}}/raw/ | wc -l
```

### t-1b: Run bash static analysis (shellcheck)
PENDING

**Spec:** Install shellcheck and run against all `.sh` files in `{{PROJECT_PATH}}`.

```bash
#!/usr/bin/env bash
set -uo pipefail

mkdir -p "{{OUTPUT_DIR}}/raw"

# Install shellcheck if not present
if ! command -v shellcheck &>/dev/null; then
  if command -v apt-get &>/dev/null; then
    apt-get update -qq && apt-get install -y shellcheck
  elif command -v brew &>/dev/null; then
    brew install shellcheck
  else
    echo "ERROR: Cannot install shellcheck" >&2
    exit 1
  fi
fi

# Find all .sh files
SH_FILES=$(find "{{PROJECT_PATH}}" -name "*.sh" -type f 2>/dev/null)
SH_COUNT=$(echo "$SH_FILES" | grep -c . || echo 0)

if [ "$SH_COUNT" -eq 0 ]; then
  echo "No bash scripts found. Skipping shellcheck."
  echo '[]' > "{{OUTPUT_DIR}}/raw/shellcheck.json"
  exit 0
fi

echo "=== Shellcheck: $SH_COUNT bash scripts ==="

# JSON output for machine parsing
echo "$SH_FILES" | xargs shellcheck -f json > "{{OUTPUT_DIR}}/raw/shellcheck.json" 2>&1 || true

# Human-readable output
echo "$SH_FILES" | xargs shellcheck -f tty > "{{OUTPUT_DIR}}/raw/shellcheck.txt" 2>&1 || true

# Severity summary
echo "=== Severity breakdown ==="
python3 -c "
import json, sys
try:
    data = json.load(open('{{OUTPUT_DIR}}/raw/shellcheck.json'))
    counts = {}
    for item in data:
        level = item.get('level', 'unknown')
        counts[level] = counts.get(level, 0) + 1
    for level in ['error', 'warning', 'info', 'style']:
        print(f'  {level}: {counts.get(level, 0)}')
    print(f'  total: {len(data)}')
except Exception as e:
    print(f'Could not parse shellcheck output: {e}', file=sys.stderr)
"

# Syntax validation
echo "=== Bash syntax validation ==="
SYNTAX_ERRORS=0
echo "$SH_FILES" | while read -r f; do
  if ! bash -n "$f" 2>> "{{OUTPUT_DIR}}/raw/syntax_errors.txt"; then
    SYNTAX_ERRORS=$((SYNTAX_ERRORS + 1))
  fi
done
echo "Syntax errors: $(wc -l < "{{OUTPUT_DIR}}/raw/syntax_errors.txt" 2>/dev/null || echo 0)"

# Bash LOC counts
echo "$SH_FILES" | xargs wc -l | sort -rn > "{{OUTPUT_DIR}}/raw/bash_loc.txt" 2>&1 || true

echo "=== Bash analysis complete ($SH_COUNT files) ==="
```

**Verify:**
```bash
test -f {{OUTPUT_DIR}}/raw/shellcheck.json && echo "shellcheck OK"
test -f {{OUTPUT_DIR}}/raw/bash_loc.txt && echo "bash_loc OK"
python3 -c "import json; d=json.load(open('{{OUTPUT_DIR}}/raw/shellcheck.json')); print(f'{len(d)} shellcheck findings')"
```

### t-2: Run git hotspot analysis
PENDING

**Spec:** Run git log to identify frequently-changed files (hotspots) for both Python and bash.

```bash
#!/usr/bin/env bash
set -uo pipefail

mkdir -p "{{OUTPUT_DIR}}/raw"

GIT_ROOT=$(git -C "{{PROJECT_PATH}}" rev-parse --show-toplevel 2>/dev/null || echo "{{PROJECT_PATH}}")
cd "$GIT_ROOT"

# Change frequency for Python AND bash files (last 6 months)
git log --since="6 months ago" --format=format: --name-only 2>/dev/null | \
  grep -E '\.(py|sh)$' | sort | uniq -c | sort -rn > "{{OUTPUT_DIR}}/raw/change_freq.txt" || true

# Churn: lines added + deleted per file (last 6 months)
git log --since="6 months ago" --numstat --format=format: 2>/dev/null | \
  grep -E '\.(py|sh)$' | \
  awk '{print $1+$2, $3}' | sort -rn > "{{OUTPUT_DIR}}/raw/churn.txt" || true

echo "Git hotspot analysis complete."
wc -l "{{OUTPUT_DIR}}/raw/change_freq.txt"
```

**Verify:**
```bash
test -f {{OUTPUT_DIR}}/raw/change_freq.txt && echo "change_freq OK"
test -f {{OUTPUT_DIR}}/raw/churn.txt && echo "churn OK"
wc -l {{OUTPUT_DIR}}/raw/change_freq.txt
```

### t-3: Generate before-metrics.json
PENDING

**Spec:** Run the following Python script (stdlib only) to aggregate all raw tool outputs
into `{{OUTPUT_DIR}}/before-metrics.json` — the baseline for before/after comparison.

```python
#!/usr/bin/env python3
"""
Generate before-metrics.json for {{PROJECT_NAME}}.
Captures aggregate numeric metrics from raw analysis outputs.
stdlib only — no pip dependencies.
"""

import json
import os
import sys
from datetime import datetime, timezone

OUTPUT_DIR = "{{OUTPUT_DIR}}"
RAW_DIR = os.path.join(OUTPUT_DIR, "raw")


def load_json(filename):
    path = os.path.join(RAW_DIR, filename)
    try:
        with open(path) as f:
            return json.load(f)
    except (FileNotFoundError, json.JSONDecodeError) as e:
        print(f"Warning: could not load {path}: {e}", file=sys.stderr)
        return {}


def count_nonempty_lines(filename):
    path = os.path.join(RAW_DIR, filename)
    try:
        with open(path) as f:
            return sum(1 for line in f if line.strip())
    except FileNotFoundError:
        return 0


def compute_metrics():
    complexity = load_json("complexity.json")
    maintainability = load_json("maintainability.json")
    raw_metrics = load_json("raw_metrics.json")
    security = load_json("security.json")

    # Complexity metrics
    all_cc = []
    for module, functions in complexity.items():
        if isinstance(functions, list):
            for fn in functions:
                all_cc.append(fn.get("complexity", 0))

    total_functions = len(all_cc)
    max_cc = max(all_cc) if all_cc else 0
    avg_cc = sum(all_cc) / len(all_cc) if all_cc else 0
    functions_cc_above_10 = sum(1 for c in all_cc if c > 10)
    functions_cc_above_15 = sum(1 for c in all_cc if c > 15)
    functions_cc_above_25 = sum(1 for c in all_cc if c > 25)

    # Maintainability metrics
    mi_values = []
    for module, data in maintainability.items():
        if isinstance(data, dict):
            mi_values.append(data.get("mi", 0))
        elif isinstance(data, (int, float)):
            mi_values.append(float(data))

    avg_mi = sum(mi_values) / len(mi_values) if mi_values else 0
    min_mi = min(mi_values) if mi_values else 0
    modules_mi_below_20 = sum(1 for m in mi_values if m < 20)

    # Size metrics
    total_loc = 0
    total_sloc = 0
    total_comments = 0
    total_modules = 0
    for module, data in raw_metrics.items():
        if isinstance(data, dict):
            total_loc += data.get("loc", 0)
            total_sloc += data.get("sloc", 0)
            total_comments += data.get("comments", 0) + data.get("multi", 0)
            total_modules += 1

    comment_ratio = total_comments / total_sloc if total_sloc else 0

    # Security metrics
    security_results = security.get("results", [])
    security_high = sum(1 for r in security_results if r.get("issue_severity") == "HIGH")
    security_medium = sum(1 for r in security_results if r.get("issue_severity") == "MEDIUM")

    # Dead code
    dead_code_count = count_nonempty_lines("vulture_critical.txt")

    # Bash metrics (shellcheck)
    shellcheck_data = load_json("shellcheck.json")
    bash_errors = bash_warnings = bash_info = bash_style = 0
    if isinstance(shellcheck_data, list):
        for item in shellcheck_data:
            level = item.get("level", "")
            if level == "error": bash_errors += 1
            elif level == "warning": bash_warnings += 1
            elif level == "info": bash_info += 1
            elif level == "style": bash_style += 1

    # Bash LOC
    bash_loc = 0
    bash_scripts_total = 0
    bash_loc_path = os.path.join(RAW_DIR, "bash_loc.txt")
    try:
        with open(bash_loc_path) as f:
            for line in f:
                parts = line.strip().split()
                if len(parts) >= 2 and parts[0].isdigit() and "total" not in parts[1]:
                    bash_loc += int(parts[0])
                    bash_scripts_total += 1
    except FileNotFoundError:
        pass

    metrics = {
        "project_path": "{{PROJECT_PATH}}",
        "project_name": "{{PROJECT_NAME}}",
        "timestamp": datetime.now(timezone.utc).isoformat(),
        "phase": "before",
        "metrics": {
            "total_functions": total_functions,
            "max_cyclomatic_complexity": max_cc,
            "avg_cyclomatic_complexity": round(avg_cc, 2),
            "functions_cc_above_10": functions_cc_above_10,
            "functions_cc_above_15": functions_cc_above_15,
            "functions_cc_above_25": functions_cc_above_25,
            "avg_maintainability_index": round(avg_mi, 2),
            "min_maintainability_index": round(min_mi, 2),
            "modules_mi_below_20": modules_mi_below_20,
            "total_modules": total_modules,
            "total_loc": total_loc,
            "total_sloc": total_sloc,
            "comment_ratio": round(comment_ratio, 4),
            "security_high_findings": security_high,
            "security_medium_findings": security_medium,
            "dead_code_items_90pct": dead_code_count,
            "bash_scripts_total": bash_scripts_total,
            "bash_loc": bash_loc,
            "bash_shellcheck_errors": bash_errors,
            "bash_shellcheck_warnings": bash_warnings,
            "bash_shellcheck_info": bash_info,
            "bash_shellcheck_style": bash_style,
        },
    }
    return metrics


if __name__ == "__main__":
    metrics = compute_metrics()
    path = os.path.join(OUTPUT_DIR, "before-metrics.json")
    with open(path, "w") as f:
        json.dump(metrics, f, indent=2)
    print(f"Wrote {path}")
    m = metrics["metrics"]
    print(f"  {m['total_functions']} functions, {m['total_modules']} modules")
    print(f"  Max CC: {m['max_cyclomatic_complexity']}, Avg MI: {m['avg_maintainability_index']}")
    print(f"  Security: {m['security_high_findings']}H / {m['security_medium_findings']}M")
    print(f"  Dead code (90%+): {m['dead_code_items_90pct']} items")
```

Save to `{{OUTPUT_DIR}}/generate_metrics.py` and execute:
```bash
python3 {{OUTPUT_DIR}}/generate_metrics.py
```

**Verify:**
```bash
test -f {{OUTPUT_DIR}}/before-metrics.json && echo "before-metrics.json OK"
python3 -c "import json; d=json.load(open('{{OUTPUT_DIR}}/before-metrics.json')); print(f'{len(d[\"metrics\"])} metrics captured')"
```
