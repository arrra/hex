## What's Added

Two new hex-events reviewers running in parallel with the existing Opus 4.7 reviewer:

| Reviewer | Script | Policy | Comment Prefix |
|----------|--------|--------|----------------|
| Codex PR review | `dispatch-codex-pr-review.sh` | `codex-pr-review-dispatch.yaml` | `🤖 Auto-review (hex-events + Codex / gpt-5)` |
| Codex Security review | `dispatch-codex-security-review.sh` | `codex-security-review-dispatch.yaml` | `🔒 Security review (hex-events + Codex / o3)` |

Each policy fires independently on `github.pr.opened`. All three reviewers (Opus 4.7 + gpt-5 + o3) post their own distinctively-prefixed comments so idempotency dedup works per-reviewer.

## Why

After ultrareview surfaced 30+ findings on PRs that my Opus 4.7 retrospective review missed (including critical wire-up failures), single-family review is clearly insufficient for high-stakes code. The Vista Hills TCPA + recording-consent pipeline makes review depth a legal requirement, not a nicety.

Cross-family review independence means:
- **gpt-5** catches architectural and correctness issues from a different training distribution
- **o3** (reasoning model) applies structured security analysis: auth fail-open patterns, injection vectors, TCPA/PII handling, race conditions

## Required Env Vars

Configure these on your machine before the reviewers activate:

```bash
export OPENAI_API_KEY="sk-..."          # Required — reviewers fail-fast without this
export CODEX_PR_MODEL="gpt-5"           # Optional — defaults to gpt-5
export CODEX_SECURITY_MODEL="o3"        # Optional — defaults to o3
```

Add to `~/.zshrc` or your secrets manager. The hex-events daemon picks up env from the shell that started it; restart the daemon after adding the vars.

## Cost Estimate

At typical PR volume (~20-40 PRs/month) with gpt-5 + o3:
- gpt-5 at `reasoning_effort=high`: ~$2-4/PR → ~$40-160/month
- o3 security review: ~$1-3/PR → ~$20-120/month
- **Total estimate: ~$60-280/month** on top of existing Opus 4.7 costs

Reduce by setting `CODEX_PR_MODEL=gpt-4o` for general review (~10x cheaper) if cost is a concern.

## Smoke Test Results (t-5)

Trigger-level smoke: **passed**. Emitted event `github.pr.opened → arrra/vista-hills-senior-care-inc#12`. Both codex policies fired (dispatch-codex-pr-review, dispatch-codex-security-review appear in `action_log`). Scripts exited cleanly at env-check step with `FAIL: OPENAI_API_KEY is not set` — correct behavior before key is configured.

Full end-to-end smoke (actual PR comments) pending `OPENAI_API_KEY` configuration.

## Future Work

- Codex CLI agent mode (vs API mode) once OpenAI ships stable agent endpoints — enables file-aware review with repo context
- Per-path security gating: only fire o3 on PRs touching `app/api/`, `lib/`, `platform/` or diffs >100 LoC
- Cost telemetry: log token usage to `~/.hex-events/logs/` for monthly cost tracking
