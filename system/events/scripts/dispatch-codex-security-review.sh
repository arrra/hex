#!/usr/bin/env bash
# Fetch a PR's diff and post a security review via OpenAI o3 (or $CODEX_SECURITY_MODEL).
# Called by hex-events on github.pr.opened. Idempotent: skips if security review comment exists.
set -uo pipefail

REPO="${EVENT_REPO:-}"
NUMBER="${EVENT_NUMBER:-}"
URL="${EVENT_URL:-}"
TITLE="${EVENT_TITLE:-}"

MODEL="${CODEX_SECURITY_MODEL:-o3}"
OPENAI_API_KEY="${OPENAI_API_KEY:-}"
COMMENT_PREFIX="🔒 Security review (hex-events + Codex / o3)"
MAX_DIFF_LINES=5000
LOG_DIR="$HOME/.hex-events/logs"
LOG_FILE="$LOG_DIR/codex-security-review.log"

mkdir -p "$LOG_DIR"

_log() {
  local step="$1" msg="$2"
  local ts; ts="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
  printf '[%s] [%s] [%s#%s] %s\n' "$ts" "$step" "$REPO" "$NUMBER" "$msg" >> "$LOG_FILE"
}

_fail() {
  local step="$1" detail="$2"
  _log "$step" "FAIL: $detail"
  exit 1
}

# Step 1: validate required env
if [[ -z "$REPO" || -z "$NUMBER" ]]; then
  _fail "env" "EVENT_REPO and EVENT_NUMBER must be set"
fi

if [[ -z "$OPENAI_API_KEY" ]]; then
  _fail "env" "OPENAI_API_KEY is not set"
fi

_log "start" "model=$MODEL url=$URL"

# Step 2: idempotency — skip if security review comment already posted
EXISTING_COMMENT="$(gh pr view "$NUMBER" --repo "$REPO" --json comments \
  --jq "[.comments[] | select(.body | startswith(\"$COMMENT_PREFIX\"))] | length" 2>/dev/null || echo "0")"

if [[ "$EXISTING_COMMENT" != "0" ]]; then
  _log "skip" "security review comment already exists"
  exit 0
fi

# Step 3: fetch PR metadata + diff
PR_BODY="$(gh pr view "$NUMBER" --repo "$REPO" --json body --jq '.body // ""' 2>/dev/null || true)"
DIFF="$(gh pr diff "$NUMBER" --repo "$REPO" 2>/dev/null || true)"

if [[ -z "$DIFF" ]]; then
  _log "diff" "empty diff — skipping"
  exit 0
fi

# Step 4: truncate diff if over limit
DIFF_LINES="$(printf '%s\n' "$DIFF" | wc -l | tr -d ' ')"
MAX_TOKENS=8192
TRUNCATED_NOTE=""
if [[ "$DIFF_LINES" -gt "$MAX_DIFF_LINES" ]]; then
  DIFF="$(printf '%s\n' "$DIFF" | head -n "$MAX_DIFF_LINES")"
  TRUNCATED_NOTE="

[Diff truncated at $MAX_DIFF_LINES lines (full diff: $DIFF_LINES lines). Security review covers first $MAX_DIFF_LINES lines only.]"
  MAX_TOKENS=4096
  _log "diff" "truncated from $DIFF_LINES to $MAX_DIFF_LINES lines"
fi

# Step 5: build JSON payload via Python for safe escaping (jq not required)
SYSTEM_PROMPT="You are a senior application security engineer conducting a thorough security review. Analyze the PR diff for security vulnerabilities. Be specific, actionable, and direct. Reference file paths and line numbers.

Security checklist to cover:
- Auth/authorization fail-open patterns (missing auth guards, privilege escalation)
- Input validation and injection risks (SQL injection, command injection, XML injection, XSS)
- Signature verification correctness (HMAC timing attacks, missing verification, bypass)
- Secrets exposure in logs, error responses, or version control
- Rate limiting and abuse prevention gaps
- Idempotency issues in write operations (double-charge, duplicate records)
- Race conditions in concurrent code paths (TOCTOU, check-then-act)
- Sensitive PII handling (logging, storage, transmission, access controls)
- TCPA/legal-compliance code patterns (consent recording, opt-out handling, audit trails)

Format findings as: [CRITICAL|HIGH|MEDIUM|LOW|INFO] file:line — description. Group by severity."

USER_PROMPT="$(printf 'PR: %s\n\nDescription:\n%s%s\n\nDiff:\n%s' \
  "$TITLE" "$PR_BODY" "$TRUNCATED_NOTE" "$DIFF")"

PAYLOAD="$(python3 -c "
import json, sys
model = sys.argv[1]
system = sys.argv[2]
user = sys.argv[3]
max_tokens = int(sys.argv[4])
payload = {
    'model': model,
    'input': [
        {'role': 'system', 'content': system},
        {'role': 'user', 'content': user},
    ],
    'text': {'format': {'type': 'text'}},
    'max_output_tokens': max_tokens,
}
# o3 has built-in reasoning; add reasoning_effort only for non-o3 models
if not model.startswith('o3'):
    payload['reasoning_effort'] = 'high'
print(json.dumps(payload))
" "$MODEL" "$SYSTEM_PROMPT" "$USER_PROMPT" "$MAX_TOKENS")" || {
  _fail "payload" "python3 JSON construction failed"
}

# Step 6: call OpenAI Responses API
_log "api" "calling openai responses API model=$MODEL"

RESPONSE="$(curl -sS --fail-with-body \
  -X POST "https://api.openai.com/v1/responses" \
  -H "Authorization: Bearer $OPENAI_API_KEY" \
  -H "Content-Type: application/json" \
  -d "$PAYLOAD" 2>&1)" || {
    _fail "api" "curl failed: $RESPONSE"
  }

# Extract text from response via Python (jq not required)
REVIEW_TEXT="$(python3 -c "
import json, sys
data = json.loads(sys.stdin.read())
# Responses API: output[].content[].text
for item in data.get('output', []):
    if item.get('type') == 'message':
        for part in item.get('content', []):
            if part.get('type') == 'output_text':
                print(part['text'])
                sys.exit(0)
# Fallback: legacy choices structure
choices = data.get('choices', [])
if choices:
    print(choices[0].get('message', {}).get('content', ''))
" <<< "$RESPONSE" 2>/dev/null || true)"

if [[ -z "$REVIEW_TEXT" ]]; then
  _fail "parse" "could not extract review text from API response: $(printf '%s\n' "$RESPONSE" | head -5)"
fi

# Step 7: post PR comment
COMMENT_BODY="$(printf '%s\n\n%s' "$COMMENT_PREFIX" "$REVIEW_TEXT")"

gh pr comment "$NUMBER" --repo "$REPO" --body "$COMMENT_BODY" || {
  _fail "comment" "gh pr comment failed"
}

_log "done" "security review posted successfully"
exit 0
