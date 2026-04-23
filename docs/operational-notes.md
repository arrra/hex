# Operational Notes

Hard-won lessons from production use. hex-upgrade should surface these to new instances.

## BOI

### Spec format: markdown only for worker execution
BOI `dispatch --spec` validates both YAML and markdown specs. But the **worker only parses markdown task headings** (`### t-N: Title`). YAML specs pass validation (task count is correct) but workers see "No PENDING tasks" and exit immediately with 0/N completed.

**Rule:** Always use `.spec.md` markdown format for dispatchable specs. YAML format is for initiatives and experiments, not BOI specs.

**Incident:** 2026-04-23 — 5 specs (q-675 through q-680) all failed silently. Each ran 5 iterations, each iteration found "No PENDING tasks." Root cause: YAML `- id: t-1` format instead of `### t-1:` markdown headings.

### Spec names must be human-readable
When re-dispatching, use the original spec path, not the queue copy path.

## Browser Automation

### Chrome CDP on port 9222
Mike's Chrome exposes CDP on `http://127.0.0.1:9222`. Connect via:
```javascript
const pw = require('playwright-core');
const browser = await pw.chromium.connectOverCDP('http://127.0.0.1:9222');
```

This gives full control over Mike's actual browser with all logged-in sessions (Slack, Google, etc.). Use for any feature that lacks an API — Slack sidebar sections, web app automation, etc.

List tabs: `curl -s http://127.0.0.1:9222/json`

### Playwright MCP tools
Two sets exist:
- `mcp__playwright__` — dev-browser plugin, launches its own browser (no persistence)
- `mcp__plugin_ecc_playwright__` — ECC plugin with `--extension` flag (needs browser extension + token)

For most automation, skip both and use CDP directly via playwright-core.

## Slack

### Sidebar sections are client-side only
No Slack API for creating sidebar sections. Automate via Chrome CDP + playwright-core right-clicking the Channels heading → Create → Create section → fill modal.

### Channel creation
Use bot token (`$MRAP_HEX_SLACK_BOT_TOKEN`) for `conversations.create`, `conversations.setTopic`, `conversations.invite`. Never ask the user to create channels manually.
