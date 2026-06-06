# Spike: claude --json-schema for answer-or-question worker

**Date:** 2026-06-05
**Status:** PASS
**Owner:** harness / open-loop-questions vertical slice

## Goal

Prove that the hex worker can emit either a final answer OR a structured
follow-up question in a single LLM call, without prose-parsing, by using
Claude's `--json-schema` structured-output flag. This is the mechanism
underpinning the whole "question with options + reply-to-by-id" feature
(see `docs/superpowers/plans/2026-06-05-open-loop-questions.md` Task 0).

## Mechanism (proven)

The worker calls:

```
claude -p --output-format json --json-schema "$FLAT_SCHEMA" <<< "$PROMPT"
```

The envelope returned by `--output-format json` contains a top-level
`structured_output` field — that is where the schema-validated object
lives. The model's raw text reply is NOT what we parse; we parse
`envelope.structured_output` directly.

## Hard facts learned (do NOT redesign)

1. **Schema MUST be FLAT.** The API rejects top-level `oneOf` / `anyOf` /
   `allOf` with HTTP 400. The working shape is a single object with a
   discriminator `kind` enum and per-branch fields all optional at the
   schema level. Per-kind required fields are enforced in the Rust
   parser (`worker/run.rs::parse_worker_json`), not in JSON Schema.

2. **`--output-format json` is required.** Without it, the structured
   payload is not surfaced separately — only the raw assistant text is
   returned, and we'd be back to prose parsing. The structured object
   is reachable only as `envelope.structured_output`.

## The flat schema (verbatim)

```json
{
  "type": "object",
  "properties": {
    "kind":         { "type": "string", "enum": ["answer", "prompt"] },
    "text":         { "type": "string" },
    "label":        { "type": "string" },
    "description":  { "type": "string" },
    "multi":        { "type": "boolean" },
    "free_form":    { "type": "boolean" },
    "options": {
      "type": "array",
      "items": {
        "type": "object",
        "properties": {
          "id":          { "type": "string" },
          "description": { "type": "string" }
        },
        "required": ["id", "description"],
        "additionalProperties": false
      }
    }
  },
  "required": ["kind"],
  "additionalProperties": false
}
```

- `kind="answer"` → parser requires non-empty `text`.
- `kind="prompt"` → parser requires non-empty `label`, `description`,
  and at least one entry in `options` (each with non-empty `id` and
  `description`). `multi` and `free_form` default to `false` when
  absent. Loud failure on any missing field (Standing Order S6).

## Envelope shape (relevant slice)

```json
{
  "type": "result",
  "subtype": "success",
  "is_error": false,
  "result": "...assistant text...",
  "structured_output": { "kind": "answer", "text": "..." }
}
```

`structured_output` is the field the harness consumes; `result` is
ignored for routing.

## Test seam

Real `claude` invocation is gated behind `worker/run.rs::run_worker`.
For tests and the e2e suite, `HEX_QUESTION_WORKER` short-circuits the
shell call: when set, the worker reads stdin (echoed prompt) and
`cat`s the JSON fixture pointed to by the env var, returning it as if
it were `envelope.structured_output`. This lets the e2e suite assert
loud-failure paths, the per-question option-id scoping headline
(E2E-I2), and the cold-process interleaved scramble (E2E-I5) without
spending Claude tokens.

## Verdict

PASS — both hard facts above were observed empirically with the real
`claude` binary on 2026-06-05. The flat schema is accepted; the
envelope's `structured_output` field carries the validated object.
Implementation in `worker/run.rs` and downstream tasks proceeds against
this exact mechanism.
