# /hex-doctor — Health Check

Validate that the hex installation is healthy and auto-repair what's fixable.

Run the full check, applying auto-fixes:

```bash
hex doctor run --fix
```

`run` executes the registered DoctorCheck framework (env, structure, memory DB,
vector search, reflection liveness, codex, LLM provider, integrations, …).
`--fix` repairs anything auto-fixable; exit 0 = all clear, 2 = warnings.

For anything `--fix` can't repair, diagnose each finding and propose a concrete fix.
Use `--filter <substr>` to focus (e.g. `hex doctor run --filter codex`).
