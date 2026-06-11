# repo-docs — reference

Companion topic doc; read before Phase 1. Sections: §1 Architecture & thresholds · §2 Placement tree · §3 Section vocabulary & bridging · §4 Lifecycle metadata · §5 Drift checks & direction · §6 Protected & out-of-scope files · §7 Edge cases · §8 Accelerators · §9 Ledger, conservation & convergence · §10 Run lifecycle & maintenance record.

## §1 Target architecture & thresholds

| Surface | Budget | Content |
|---|---|---|
| Root entry file (AGENTS.md) | 50–200 lines | 1–2 sentence overview; quick-start commands; ≤15 hard constraints; one-line topic-doc pointers |
| README (human entry) | short orientation | what/why, install, usage, how to verify, links into docs/ — agent operational detail goes to AGENTS.md |
| Topic doc | 50–150 lines | one subject, one audience; contents line if over 100 lines (partial reads must see scope) |
| Nested AGENTS.md | lean, directory-scoped | rules for that directory only; conflict-free with root (§3) |
| Pointer depth | 1 hop from an entry file | deeper chains get partially read and silently lose information |
| Combined instruction chain | lean (~32 KiB ceiling) | some engines silently drop overflow — every level stays small |

Spec rules:

- Hard constraints use MUST / MUST NOT, in one list at the top of the entry file; recap critical invariants at the bottom. Recap lines duplicate top rules by design — exempt from the per-line and DUPLICATE tests. Never place a load-bearing rule mid-file; order rules by importance — compliance is biased toward earlier instructions.
- Keep hard constraints visually distinct from soft guidance — identical formatting destroys the priority signal.
- Per-line test (agent docs): "would removing this cause an agent to make mistakes?" If not, cut — except recap lines and human-audience sections (§2). Cut anything inferable from code, standard conventions, or fast-changing facts.
- Instructions are concrete and verifiable ("run `make test` before committing"), never aspirational. One term per concept across all docs; zero contradictions between any two files.
- Commands reference the repo's task runner (Makefile / justfile / package scripts) rather than duplicating its contents.
- Entry files are de facto stable prompt prefixes for every agent session: keep them byte-stable between real changes — no timestamps, counters, or generated values; order content stable-first, volatile-last. Diff-minimal edits serve git review, prompt caches, and cross-session stability at once.
- Fresh-session bar — from repo contents alone, a cold agent answers all five: what is this system · how is it organized · how do I run it · how do I verify a change (ordered levels with exact commands: unit → integration → e2e) · what is the current progress and next step (state artifacts — progress, decisions, feature-state docs or equivalents — behind pointers).

## §2 Placement decision tree

Classify each content block (a rule, fact, procedure, or section) in order; first match wins. Branches 1, 2, and 6 only point at things that already EXIST — when the code comment, check, or test doesn't exist yet, keep the prose (branches 3–5) and record the conversion as an Upstream proposal. Never delete prose first.

1. **Already visible in code, types, or standard conventions?** → delete the doc copy, citing where it's visible. ("Should be a code comment but isn't yet" → keep the doc line; propose the comment upstream — this pass never writes code.)
2. **Already enforced by an existing CI / lint / hook check?** → replace the prose with a one-line pointer to that check.
3. **Needed on virtually every task?** → always: entry file, within the ≤15-constraint budget. Over budget means blocks compete — keep the most load-bearing, demote the rest.
4. **Needed only in one directory or subsystem?** → co-located-lazy: nested AGENTS.md beside that code.
5. **Needed occasionally, for a nameable task type?** → on-request: topic doc behind a one-line pointer stating when to read it.
6. **Historical rationale ("why")?** → a decision-record doc reached on request; an existing test that encodes it may replace it. Never inline in an entry file.
7. **Volatile (status, progress, inventories, dates)?** → a dedicated state doc behind a pointer; entry files stay byte-stable. (CHANGELOG is read-only — §6.)

Human-audience guard: README and docs/ serve readers too — RETIRE additionally requires failing the human test ("would a reader lose needed information?"). Community, attribution, citation, funding, legal, and project-history sections are exempt from SNR cuts.

Audience split: content needed by both audiences lives once; the other audience gets a pointer. Duplicates drift independently — that is how contradictions are born.

Pointer form — always one line with an applicability condition:

    - Database rules (`docs/database-rules.md`) — read before changing queries, schema, or migrations.

## §3 Section vocabulary & entry-file bridging

Canonical AGENTS.md section order for files this skill CREATES: project overview · setup/build/test commands · code style & conventions · testing · security · PR & commit guidelines · topic-doc pointers. README: what it is · install · usage · how to verify · deeper docs. EXISTING files keep their order and section names — reordering conforming text is churn, not a MISPLACED finding.

Bridging: if AGENTS.md and a tool-specific entry file (CLAUDE.md, GEMINI.md, …) coexist — or the repo has ONLY a tool file — make AGENTS.md canonical and the tool file a thin bridge (symlink or one-line include, per that tool's convention), in the SAME Batch A commit as the migration. The bridge is core layout for any tool the team uses that can't read AGENTS.md natively: never deferred to §8, never a DUPLICATE finding, exempt from §8's deletability test. Never leave a tool's instruction surface empty.

Nesting: the published spec says nearest-file-wins, but engines diverge — several concatenate with later-position bias, and at least one major tool documents conflict resolution as non-deterministic. Portable rule: write nested files as conflict-free, scope-local additions whose wording reads true under every load order. A nested override that follows the spec, in a repo whose primary tool honors nearest-wins, is a DOWNGRADED portability note — not a CONFIRMED CONFLICT; recommend scoping the root rule's wording ("except in `pkg/legacy/` — see its AGENTS.md") without forcing the edit.

## §4 Lifecycle metadata

Every non-obvious rule should answer: why it exists (source), when it applies, when it can die (expiry). Encode cheaply, in a form harmless to every tool — the pointer's applicability condition, the rule's own wording ("when touching X"), or a trailing note / HTML comment where non-obvious:

    <!-- source: 2024-11 prod OOM | applies: worker-pool changes | expires: when the queue is rewritten -->

A rule whose expiry condition has arrived is a DEAD finding on the next run. A rule with no statable applicability condition is usually a platitude — delete it. Provenance describes the rule or its evidence (a content hash or commit SHA of the verified source is fine — §10), never the visit: no edit-date stamps, and no boilerplate metadata on rules that don't need it — that is SNR damage and diff churn.

## §5 Drift checks & direction

| Claim type | Check | Drifted when |
|---|---|---|
| Command (`make test`, `npm run x`) | target exists in the task runner / manifest; run it when cheap and side-effect-free | non-zero exit or target absent |
| Path / filename | `git ls-files` or `ls` | missing or moved |
| Symbol / API / signature | grep the source, read the definition | absent, or name/arity/shape differs |
| Version / dependency | manifest + lockfile | mismatch |
| Behavior claim ("retries 3 times") | read the implementing code; cite file:line | code contradicts the claim |
| Link | resolve it (checker or grep) | broken — flag it, don't guess a target |
| Structure claim ("X lives in src/y/") | list the directory | layout differs |

**Direction check — mandatory before any doc-side FIX:** the doc may be right and the code regressed. Grep the tests, recent `git log` for the cited code, and issues/changelog; if ANY artifact supports the doc's claim, classify it as a possible code bug → Open questions / Upstream proposal, never a doc FIX. Each drift-fix ledger row records "doc-side because: <evidence>".

Deterministic checks gate; judgment applies only to what remains. UNVERIFIED claims stay unedited, flagged in Open questions, never rewritten on plausibility. EVERY finding — drift or structural (line counts, budgets, hop counts, ordering) — records its exact reproducible check (command, path, numeric threshold); Phase 5 and any future run re-run it verbatim.

Skip conditions (not drift, no edit): internal refactors with identical documented behavior, renames the docs never mention, formatting or performance changes that preserve behavior.

## §6 Protected & out-of-scope files

Out of inventory scope: LICENSE* / NOTICE* / COPYING* (legal text — never cut), dependency manifests, build files; `*.txt` only when the user opts in. Read-only by default: CHANGELOG (machine-parsed by release tooling). Platform well-known filenames are never renamed or relocated — README, LICENSE, SECURITY.md, CONTRIBUTING.md, CODE_OF_CONDUCT.md, CHANGELOG, PR/issue templates (content may be routed; the filename and its platform role stay).

Treat as read-only; planned edits go to Upstream proposals with the owning repo, template, or generator input named:

- A marker anywhere in the file: `DO NOT EDIT`, `@generated`, `generated by`, `AUTO-GENERATED`, or any sync/template marker the repo's own tooling documents (check CONTRIBUTING and sync scripts).
- Files owned by a repo-level template/sync config even without in-file markers — check in Phase 0 for `cruft.json`, `.copier-answers.yml`, `copier.yml`, `.github/sync.yml`, and similar.
- Files a README or CONTRIBUTING describes as synced or templated from another repository.
- Build outputs of doc generators (typedoc, sphinx, rustdoc, OpenAPI) — fix the generator's source instead.

## §7 Edge cases

- **No docs at all:** bootstrap a minimal entry file covering all four initialization sections — start commands (verified), current state (verified: dependencies install, test framework runs, one passing check), structure map, and task breakdown with measurable acceptance criteria (confirmed with the user — never invented). Do not invent conventions the repo does not exhibit.
- **Only a tool file (CLAUDE.md, …), no AGENTS.md:** migrate content to canonical AGENTS.md AND create the thin bridge in the same Batch A commit (§3) — never strand that tool with zero instructions.
- **Monorepo:** root entry file holds repo-wide defaults plus a routing map to per-package nested AGENTS.md; package-specific commands live in the package file, not the root.
- **Multiple divergent entry files:** converge on canonical AGENTS.md per §3, bridge in the same Batch A commit. Byte-identical lines dedup as RETIRE; lines that DIFFER are CONFLICT findings — a code citation (§5) picks the survivor, recorded in the ledger's evidence column. Never discard a variant's unique claim unverified.

## §8 Optional tool-specific accelerators — never load-bearing

NET-NEW accelerator files only (path-scoped rule files, skills directories, import mechanisms); converting an existing tool entry file into a bridge is core Phase 4 work (§3), not §8. Only after Phase 5 passes, as a separate commit, only for tools the team actually uses — then RE-RUN the Phase 5.1 proof pass; never deliver an unproven HEAD. Accelerators mirror the neutral structure, never replace it. Acceptance test: delete every §8 accelerator and the repo remains fully usable via the neutral palette (entry files + bridges + pointers).

## §9 Ledger, conservation & convergence

Ledger — one row per op: `doc:line(s) | content | op | destination or reason | evidence`. FIX rows quote the removed line(s) verbatim. RETIRE rows covering >3 lines record the section heading, line count, and first + last lines verbatim. A reviewer must account for every removed line without reading raw diffs.

Conservation cross-check (Phase 5.3) — run over the WHOLE diff, no path filter (a non-doc path appearing is itself a violation):

    git diff -U0 <base>..HEAD | grep '^-' | grep -v '^--- ' || true

Empty output (additions only) is success — grep exits 1 on no matches; handle it explicitly, never in a bare `&&` chain. Self-test the command before trusting it: it must surface a known removed bullet line (e.g. `- some rule`) from a synthetic diff. Then map each removed line to exactly one ledger row: FIX rows match their quoted lines; MOVE/SPLIT lines must appear verbatim at the destination — verify per line with `git grep -F '<line>' -- <dest-file>`; trivial structural lines (fences, blanks, `| --- |` rules) are instead verified by block-level byte comparison of the moved block.

Idempotence-residue causes, in observed order: rewording during "mechanical" moves · timestamps or generated values · list/section reordering · whitespace normalization · a check that is not reproducible (different command or cwd on the second pass) · thresholds applied as taste rather than numbers · capped scope (findings beyond the ~30 cap — defer them explicitly in Open questions; never chase them silently).

Report skeleton (drafted in the out-of-tree plan file; becomes the PR body):

    ## Summary            — what changed and why; attrition; every touched path
    ## Router map         — before → after; each doc with audience, load trigger, applicability condition
    ## Drift fixes        — doc line + code citation + "doc-side because: <evidence>"
    ## Change ledger      — table as above
    ## Convergence proof  — in-scope-empty second pass; conservation + churn-gate output
    ## Upstream proposals — protected/generated sources, code-side bugs, proposed CI gates
    ## Open questions     — UNVERIFIED claims, DEFERRED findings, fresh-session gaps
    ## Declined changes   — improvements rejected, with reasons

## §10 Run lifecycle & maintenance record

- **Re-entry:** before branching, look for a prior run — `git branch --list '*docs*'`, `git worktree list`, open PRs. An unmerged prior branch → resume or rebase it, never start a parallel duplicate; merged → normal run. The zero-diff guarantee is measured against the repo WITH the prior run merged. Create worktrees OUTSIDE the repo directory so leftovers can't dirty the tree.
- **Plan durability:** Phase 3 writes the plan + ledger to a file OUTSIDE the repo (a sibling or temp path — record it). It survives session death and becomes the PR body (`--body-file`). Local-only repo (no remote): the full report goes into the final commit message or an annotated tag — never a tracked scratch file.
- **Maintenance record** (in-repo, an ADD op, behind a pointer — e.g. `docs/doc-maintenance.md`): the source-dir → doc mapping table (the routing backbone — name an owner); the cross-doc claim table from the Phase 1 sweep (subject · claim · stating files — future runs diff against it instead of rebuilding, and deliberate divergences recorded here are exempt from the cohesion gate); each claim's reproducible check command, machine-readable; declined changes and do-NOT-touch rationale; UNVERIFIED claims. Optional provenance anchors per claim: content hash or commit SHA of the verified source (evidence provenance — not edit-date stamps, which stay banned). Future runs read it in Phase 0; prior declined-with-reason items stand as refutations unless the cited code changed. Propose a recurring CI gate (link checker + drift/freshness check) in Upstream proposals — detection that runs on every merge beats a one-shot pass.
- **Teardown:** after delivery, `git worktree remove` the worktree. Push and open a PR only when asked; never push the default branch.