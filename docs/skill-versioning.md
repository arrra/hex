# Skill Versioning — performance comparison before replacement

> Standing requirement (Mike, 2026-06-11): "Make sure we can compare the performance of
> skills as we version them." A skill version bump replaces the old version only after a
> comparison run shows the new one is not worse.

Skills are prompts, and prompt edits regress silently: a rewrite that reads better can
score worse on the work. Hex already has both halves of the answer in practice — this doc
makes them the standard.

## The practice

1. **Preserve the prior version.** When substantially rewriting a skill, keep the old one
   installed under a version-suffixed name (`<skill>-v1`) until the comparison verdict is
   in. Precedent: `design-taste-frontend-v1` (kept for exact-behavior dependents).
2. **Bake off before replacing.** Run old vs new on a FIXED task corpus with independent
   judges — the existing bakeoff harness pattern
   (`projects/system-improvement/bakeoffs/` in the operating instance; precedent:
   `repo-audit-prompt/2026-06-10`, candidates × test repos × judge panel × verdict doc).
   Small skills need 3–5 representative tasks, not a benchmark suite.
3. **Record the verdict next to the bump.** The bakeoff verdict doc (what was compared, on
   what corpus, who judged, result) is linked from the skill's changelog entry / commit
   message. A bump with no verdict link is an incomplete change — same standing as a code
   change without its test.
4. **Losing or mixed verdict → don't replace.** Keep the old version live; the new one
   stays a candidate. Partial adoption (cherry-picking the winning sections) is a new
   candidate, not a shipped version.

## Scope

- Applies to foundation-shipped skills (`system/skills/`) on substantial rewrites — not
  typo/path fixes (judgment call: would a regression be invisible until someone's work
  quietly got worse? then bake off).
- Mechanical enforcement (a release-gate or doctor check that a skill diff ships with a
  verdict link) is a designed follow-up — until built, this is reviewer responsibility,
  same as the architecture-docs update rule (docs/architecture/README.md §2).

## Open follow-ups

- Define the standing corpus per skill as skills come up for revision (repo-audit has one
  from the 2026-06-10 bakeoff; others get theirs on first rewrite).
- Wire the bakeoff invocation into a reusable workflow script so a comparison is one
  command, not a bespoke build each time.
