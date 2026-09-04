// deep-solve — generic two-phase Workflow script.
//
// Phase 1: parallel evidence -> frontier synthesis -> adversarial verification
//          (with one refutation loop) -> finalize -> PROBLEM DOCUMENT
// Phase 2: blind parallel designers -> independent judges -> frontier convergence
//          -> SOLUTION PROPOSAL
//
// TO ADAPT: fill in CONFIG, then rewrite the four evidence-reader prompts for the
// actual problem. Everything below CONFIG is problem-independent.
//
// Model discipline (hex SO 3b): every agent() call sets `model` explicitly. Frontier
// (opus) is used at exactly three seats — synthesis/finalize, mechanism refuter,
// convergence. Leaving `model` unset inherits the caller's frontier model.
//
// Placeholders use [BRACKETS]. Never use {{ }} — it collides with recipe templating.

export const meta = {
  name: 'deep-solve',
  description: 'Understand a hard problem with evidence, then converge on a solution proposal',
  phases: [
    { title: 'Evidence', detail: '4 parallel readers: code path, forensics, contract, prior art' },
    { title: 'Synthesize', detail: 'frontier agent writes the problem document', model: 'opus' },
    { title: 'Verify', detail: 'citations, mechanism refutation, completeness' },
    // Conditional — these three run only if the refuter refutes a core claim (once).
    { title: 'Evidence (redo)', detail: 're-run only the lens(es) the refutation lands on' },
    { title: 'Synthesize (redo)', detail: 're-synthesize on corrected evidence', model: 'opus' },
    { title: 'Verify (redo)', detail: 're-verify the corrected draft' },
    { title: 'Finalize', detail: 'apply corrections, emit the problem document', model: 'opus' },
    { title: 'Design', detail: '3-4 blind designers, one persona each' },
    { title: 'Judge', detail: '3 independent judges score every candidate' },
    { title: 'Converge', detail: 'frontier agent writes the solution proposal', model: 'opus' },
  ],
}

// ---------------------------------------------------------------- CONFIG
const SYSTEM = '[SYSTEM NAME]'
const REPO = '[/abs/path/to/repo]'
const BRANCH = '[branch]'
const VERSION = '[version]'
const EVIDENCE = '[/abs/path/to/db-or-logs]'
const INCIDENTS = '[id1, id2, id3]'
const PROBLEM = `[2-4 sentences: what breaks, the terminal state it lands in, the
confirmed occurrences with identifiers and dates, and what it costs each time.]`
// Any pre-existing suspected cause goes here, NOT into PROBLEM — stated as a hypothesis
// under test with its source. A cause stated as fact is inherited as ground truth by
// every downstream seat and leaves the refuter nothing to attack.
const HYPOTHESIS = `[The current suspected cause, from [SOURCE] — under test, not
established. Leave as "none recorded" if there is no prior theory.]`
const FIX_INTENT = '[one line: what a fix would have to accomplish]'
// Judge criteria beyond the three universal ones, derived from this problem's hard
// constraints. e.g. 'Idempotence — is re-running the fixed path safe?'
const EXTRA_CRITERIA = '[ADDITIONAL CRITERIA DERIVED FROM THIS PROBLEM\'S CONSTRAINTS]'

const COMMON = `You are investigating a confirmed problem in ${SYSTEM} (repo at ${REPO}, branch ${BRANCH}, version ${VERSION}).

THE PROBLEM: ${PROBLEM}

CURRENT HYPOTHESIS (under test — treat as a claim to verify or refute, never as an
established fact, and report evidence against it as readily as evidence for it):
${HYPOTHESIS}

RULES: This is READ-ONLY investigation. Do not edit any file in any git repo. Read queries against databases and logs are fine. Cite everything as file:line with short verbatim code excerpts. Your final message IS your data payload to a synthesis agent — return dense structured findings, no pleasantries.`

// The section contract the synthesis seat must follow. Mirrors
// references/problem-document.md — keep the two in sync when adapting.
const DOC_CONTRACT = `# [Problem title — the symptom, not the suspected cause]
## Summary  (plain language, 5 lines max)
## Symptom & blast radius  (one table row per incident: id, date, where, cost, outcome, current state; then the population at risk)
## Mechanism  (code-cited walkthrough: entry -> the step where behavior diverges from intent -> terminal state; contrast any healthy sibling path; state exactly what the failing step receives each attempt and why that input cannot change)
## Why existing fixes-to-date didn't cover it  (each prior fix: what it reached, what it did not, cited)
## Existing machinery a fix can build on  (cited)
## Hard constraints any fix must respect  (derived strictly from what was read)
## Candidate fix directions  (2-4 entries, each: Sketch / Reuses / Main risk / Open questions)
## Open design questions  (numbered)
## Evidence appendix  (forensic tables, key excerpts, and which lenses came back empty)`

// ---------------------------------------------------------------- PHASE 1
// Readers are keyed so the refutation loop can re-run the ones whose lens covers what
// was refuted, rather than always re-running the code-path reader. `sections` lists the
// problem-document sections each reader is the evidence source for.
const READERS = [
  { key: 'code-path', sections: ['Mechanism'] },
  { key: 'forensics', sections: ['Symptom & blast radius', 'Evidence appendix'] },
  { key: 'contract', sections: ['Mechanism', 'Open design questions'] },
  { key: 'prior-art', sections: ["Why existing fixes-to-date didn't cover it", 'Existing machinery a fix can build on', 'Hard constraints any fix must respect'] },
]

const readerPrompt = {
  'code-path': `${COMMON}

TASK A — trace the relevant code path end to end in ${REPO}. Find and cite:
1. Where the failing operation is scheduled or entered.
2. Where the decisive value is produced and where it is consumed — separately for the FAILING path and for any healthy sibling path handling the same case correctly. Show the exact divergence.
3. Any counter, budget, or cap involved: where it increments, its configured value, and the transition to the terminal state.
4. What input the failing step receives on each attempt, and whether anything can change that input between attempts. Cite where that input is constructed.
5. Confirm — or REFUTE — that no code path exists from [THE TRIGGER] to [THE EXPECTED EFFECT]. If any partial mechanism exists, describe it precisely.
Return: numbered findings with file:line and excerpts, plus a call-graph sketch from entry to terminal state.`,

  'forensics': `${COMMON}

TASK B — forensics on incidents ${INCIDENTS} using ${EVIDENCE} (read queries only). For each incident:
1. Relevant rows with timestamps, counts, and stored payloads. Inspect the schema first; dump any stored verdict, feedback, or error text.
2. Whether the recorded evidence is identical across attempts — this proves or disproves "the input never changed".
3. What state was left behind, and whether it is still there now.
4. Wall-clock and resource cost per incident.
5. What an operator had to do manually afterwards.
Return: one evidence table per incident plus a cross-incident summary (common signature, total cost, current state). If a source is unavailable, say so rather than inferring.`,

  'contract': `${COMMON}

TASK C — documented contract versus reality. Read [DOCS / SPECS / CONFIG / PROMPT TEMPLATES / CHANGELOG] and report everything they claim about this behavior. Separate explicitly WHAT THE DOCS AND CONFIG PROMISE from WHAT THE CODE DOES. Quote exactly any text that would lead a reader — human or model — to expect behavior the code does not implement.
Return: numbered findings with citations, promise-versus-reality clearly split.`,

  'prior-art': `${COMMON}

TASK D — prior art. Find every past fix, decision record, backlog entry, changelog line, and commit touching this area: [SEARCH LOCATIONS]. For each: what it changed, what it explicitly covered, what it left uncovered. You are answering "wasn't this already fixed?" with citations.
Also map every existing mechanism that can move the system out of the state this problem strands it in — a fix will likely reuse one.
Return: numbered findings with citations, ending with a "constraints any fix must respect" list derived strictly from what you read.`,
}

// findings[key] holds the current best evidence from each lens; the redo loop replaces
// only the entries it re-runs.
const findings = {}
const runReaders = async (keys, phaseLabel, extra = '') => {
  const out = await parallel(keys.map(k => () =>
    agent(`${readerPrompt[k]}${extra}`, { label: `read:${k}`, phase: phaseLabel, model: 'sonnet' })
      .then(text => ({ key: k, text }))))
  for (const r of out.filter(Boolean)) findings[r.key] = r.text
  const missing = keys.filter(k => !findings[k])
  if (missing.length) log(`WARNING: evidence lens(es) returned nothing: ${missing.join(', ')} — the document must record them as gaps, not as passed checks`)
}

phase('Evidence')
await runReaders(READERS.map(r => r.key), 'Evidence')

const synthesize = (extra = '') => agent(`You are a principal engineer writing the definitive PROBLEM document for ${SYSTEM} (${REPO}, ${BRANCH} @ ${VERSION}). Investigators returned the findings below.

Understand the problem deeply and DOCUMENT it clearly. This goes to a separate implementation team — it must let them design a fix without redoing the investigation.

Write to this exact section contract, in this order:
${DOC_CONTRACT}

The final content section is "Candidate fix directions": 2-4 entries, each with Sketch / Reuses / Main risk / Open questions. No section ranks them and no entry is marked recommended; ordering is arbitrary and the document says so.

Rules: every mechanism claim carries a file:line citation from the findings. Where investigators disagree or evidence is thin, say so in place rather than smoothing over. Plain, direct prose. Return ONLY the markdown document.
${extra}
${READERS.map(r => `=== INVESTIGATOR ${r.key} ===\n${findings[r.key] || '(this lens returned nothing — record it as a gap in the Evidence appendix)'}`).join('\n\n')}`,
  { label: 'synthesize:problem-doc', phase: 'Synthesize', model: 'opus', effort: 'high' })

phase('Synthesize')
let draft = await synthesize()

const REFUTER_SCHEMA = {
  type: 'object', additionalProperties: false, required: ['claims', 'any_refuted'],
  properties: {
    any_refuted: { type: 'boolean', description: 'true if any CORE causal claim was refuted' },
    claims: {
      type: 'array',
      items: {
        type: 'object', additionalProperties: false,
        required: ['claim', 'verdict', 'evidence', 'doc_section'],
        properties: {
          claim: { type: 'string' },
          verdict: { type: 'string', enum: ['CONFIRMED', 'REFUTED', 'UNVERIFIABLE'] },
          evidence: { type: 'string', description: 'file:line evidence for the verdict' },
          doc_section: {
            type: 'string',
            description: 'the exact problem-document section heading this claim appears under — used to route a refutation back to the evidence lens that produced it',
          },
        },
      },
    },
  },
}

const verify = (d) => parallel([
  () => agent(`Adversarial verification, lens: CITATION ACCURACY. Below is a draft problem document about ${SYSTEM} (${REPO}, ${BRANCH}). Check EVERY file:line citation and code claim against the actual source. For each wrong, stale, or unverifiable citation return: the claim, why it is wrong, and the corrected citation or "unverifiable". Read-only. Return a numbered corrections list, or "ALL CITATIONS VERIFIED" if clean.

${d}`, { label: 'verify:citations', phase: 'Verify', model: 'sonnet' }),

  () => agent(`Adversarial verification, lens: MECHANISM. Below is a draft problem document about ${SYSTEM} (${REPO}, ${BRANCH}).

Try hard to REFUTE its core causal claims by reading the source. Is there ANY code path that would produce an effect the document says is impossible? Is the "input cannot change between attempts" claim exactly true? Are the "why prior fixes didn't cover this" claims correct?

You are not reviewing the document's quality. You are trying to prove it wrong. Read-only.

For every claim you assess, record the exact document section heading it appears under —
a refutation is routed back to the evidence lens that produced that section.

${d}`, { label: 'verify:mechanism', phase: 'Verify', model: 'opus', effort: 'high', schema: REFUTER_SCHEMA }),

  () => agent(`Adversarial verification, lens: COMPLETENESS for an implementation team. Below is a draft problem document for a fix that will ${FIX_INTENT} in ${REPO}.

What is MISSING that the implementation team would trip over? Check against the source (read-only): crash or restart mid-operation; concurrency and resource limits; budget and counter semantics after a fix; lifecycle races; the ambiguous case where the trigger does not name a target; downstream surfaces that assume the current terminal state is final.

Return a numbered list of gaps with citations, or "NO MATERIAL GAPS".

${d}`, { label: 'verify:completeness', phase: 'Verify', model: 'sonnet' }),
])

phase('Verify')
let [citations, mechanism, completeness] = await verify(draft)

// A schema'd agent() returns a parsed object, but it can also come back null/undefined
// if the seat failed — every surveyed workflow guards with .filter(Boolean). A partial
// object is just as dangerous: `{any_refuted: true}` with claims omitted would pass a
// looser guard and then crash. An absent or malformed verdict is NOT a clean verdict:
// say so loudly, and carry the warning into the document itself (SO S6).
const mechanismUsable = (m) => !!m
  && typeof m.any_refuted === 'boolean'
  && Array.isArray(m.claims)

const UNVERIFIED_NOTICE = `
VERIFIER 2 (mechanism) RETURNED NO USABLE VERDICT. The document's core causal claims are
UNVERIFIED — this is NOT the same as verified-clean. State this explicitly in the document:
add it to Open design questions and note in the Evidence appendix that mechanism
verification did not complete.
`
const warnUnverified = () => log('WARNING: mechanism refuter returned no usable verdict. The core causal claims are UNVERIFIED — do not read this as confirmation. Re-run the refuter or stop and surface it.')

if (!mechanismUsable(mechanism)) warnUnverified()

// Refutation loop — runs at most once. A refuted core claim means the evidence was
// wrong, not that the prose needs editing: re-read, then re-synthesize.
// Loop phases carry distinct labels rather than re-entering 'Evidence'/'Verify' — a
// re-entered phase label is untested against the progress UI.
if (mechanismUsable(mechanism) && mechanism.any_refuted) {
  const refutations = mechanism.claims.filter(c => c.verdict === 'REFUTED')

  // Route each refutation back to the lens(es) that own its document section. The
  // refuter interrogates prior-art and contract claims too, not just the mechanism —
  // always re-running the code-path reader would leave those uncorrected. Unrecognized
  // or missing sections fall back to re-running every lens rather than guessing.
  const norm = (s) => String(s || '').toLowerCase().replace(/[^a-z]/g, '')
  const matched = new Set()
  let unroutable = false
  for (const r of refutations) {
    const hits = READERS.filter(rd => rd.sections.some(sec => {
      const a = norm(sec), b = norm(r.doc_section)
      return b && (a === b || a.includes(b) || b.includes(a))
    }))
    if (hits.length) hits.forEach(h => matched.add(h.key))
    else unroutable = true
  }
  const redoKeys = (unroutable || matched.size === 0) ? READERS.map(r => r.key) : [...matched]
  if (unroutable) log('a refutation named no recognizable document section — re-running every evidence lens')
  log(`mechanism refuted ${refutations.length} core claim(s) in section(s) [${refutations.map(r => r.doc_section).join(', ')}] — re-running lens(es): ${redoKeys.join(', ')}`)

  phase('Evidence (redo)')
  await runReaders(redoKeys, 'Evidence (redo)', `

A verifier REFUTED claims in the draft problem document that rest on YOUR lens. Re-examine
with these refutations as your starting point and report what is ACTUALLY true, with
file:line citations. Do not defend the earlier findings.

REFUTATIONS:
${JSON.stringify(refutations, null, 2)}`)

  phase('Synthesize (redo)')
  draft = await synthesize(`
A prior draft had core claims refuted. The findings below for [${redoKeys.join(', ')}] are
the CORRECTED ones. Do not restate the refuted claims.
`)
  phase('Verify (redo)')
  ;[citations, mechanism, completeness] = await verify(draft)
  if (!mechanismUsable(mechanism)) warnUnverified()
  else if (mechanism.any_refuted) {
    log('STILL REFUTED after one loop — surfacing to the user instead of looping again')
  }
}

// Computed from the FINAL verdict, after any redo — a redo that returns a malformed
// verdict must not finalize silently.
const mechanismNotice = mechanismUsable(mechanism) ? '' : UNVERIFIED_NOTICE

phase('Finalize')
const problemDoc = await agent(`You are finalizing the problem document you drafted. Apply the three verification reports below: fix every confirmed citation error; wherever a claim was REFUTED, state what is actually true instead; fold material completeness gaps into the Hard constraints and Open design questions sections.

Keep the section contract. Do not pad. Return ONLY the final markdown document.
${mechanismNotice}
=== DRAFT ===
${draft}

=== VERIFIER 1 (citations) ===
${citations}

=== VERIFIER 2 (mechanism) ===
${JSON.stringify(mechanism, null, 2)}

=== VERIFIER 3 (completeness) ===
${completeness}`, { label: 'finalize:problem-doc', phase: 'Finalize', model: 'opus', effort: 'high' })

// The caller writes problemDoc to <slug>-problem-YYYY-MM-DD.md, surfaces the path and
// the Summary section, and continues into Phase 2 unless a checkpoint was requested.

// ---------------------------------------------------------------- PHASE 2
phase('Design')
const PERSONAS = [
  { key: 'minimal-diff', stance: 'Minimal diff. The smallest change that makes the failure impossible. Every added line is a liability.' },
  { key: 'reuse-machinery', stance: 'Reuse existing machinery. The codebase already contains the parts; the fix is wiring, not invention.' },
  { key: 'first-principles', stance: 'First principles. Assume the current structure is the reason the bug exists. Design what this should have been.' },
  { key: 'operational', stance: 'Operational simplicity. Optimize for the person on call at 3am: predictable, observable, loud on failure, easy to reverse.' },
]

const candidates = await parallel(PERSONAS.map(p => () =>
  agent(`You are designing a fix. Below is a finalized problem document produced by an investigation team; it is your ONLY input. You are working independently — other designers are solving the same problem in parallel and you will not see their work.

YOUR DESIGN STANCE: ${p.stance}

Design the fix your stance leads you to. Work the WHOLE problem from that stance — your stance is a lens, not an assignment of which fix to advocate.

Return:
1. Design — what changes, where, and the resulting control flow. Concrete enough to implement; reference the existing machinery named in the document.
2. Constraint satisfaction — one line per constraint in the document's "Hard constraints" section, saying how your design satisfies it. Do not skip constraints; if your design violates one, say so.
3. Blast radius — files and behaviors touched.
4. Failure modes — how your design breaks, and what it does when it breaks.
5. What you deliberately did NOT do, and why.

=== PROBLEM DOCUMENT ===
${problemDoc}`, { label: `design:${p.key}`, phase: 'Design', model: 'sonnet' })
    .then(text => ({ persona: p.key, text }))))

const CANDIDATE_BLOCK = candidates
  .map(c => `=== CANDIDATE ${c.persona} ===\n${c.text}`)
  .join('\n\n')

phase('Judge')
const JUDGE_PROMPT = `You are judging candidate fixes for the problem below. Judge independently; you are one of several judges and will not see the others.

Score every candidate 1-5 on each criterion. Criteria derive from the problem document's "Hard constraints" section:
- Correctness and crash-safety — does it actually make the failure impossible, and does it survive interruption at any point?
- Blast radius and simplicity — how much surface changes; how much new state or concurrency is introduced.
- Operability and loudness — can an operator see it work and see it fail; is failure loud; is it reversible.
- ${EXTRA_CRITERIA}

Those three are the floor. Before scoring, read the problem document's "Hard constraints"
section and add a criterion for any constraint the three above do not already cover —
the criteria must actually derive from THIS problem's constraints, not from a generic
checklist. List the criteria you settled on before your scores.

For each candidate also give: its single strongest property, its single fatal-if-true weakness, and whether any stated constraint is violated (quote the constraint).

Finish with your ranking and the one sentence that decides it.

=== PROBLEM DOCUMENT ===
${problemDoc}

${CANDIDATE_BLOCK}`

const judges = await parallel([1, 2, 3].map(n => () =>
  agent(JUDGE_PROMPT, { label: `judge:${n}`, phase: 'Judge', model: 'sonnet' })))

phase('Converge')
const solutionDoc = await agent(`You are converging a solution. Below: the problem document, ${candidates.length} independently designed candidate fixes, and ${judges.length} independent judge reports.

Produce the SOLUTION PROPOSAL. Your job is not to pick the highest average score — it is to build the best design that satisfies the problem document's hard constraints, taking the winner as a base and grafting in what the runners-up got right.

Section contract, in this order:
# [Title] — proposal  (with problem-document path, designer count, judge count)
## Verdict  (plain language, 5 lines max: what to build and the one reason it wins)
## How it works  (the winning design, implementable, citing existing machinery)
## Grafts  (table: idea | from candidate | why grafted in)
## Constraint satisfaction  (table: one row per constraint from the problem document — none dropped)
## Rejected alternatives  (each candidate that did not win: Sketch + "Rejected because" with a SPECIFIC reason)
## Judge panel  (score table; then Disagreements — name the split and the constraint that breaks the tie; then Independent convergence if blind designers agreed)
## Risks and falsifiers  (what this bets on; what observation would prove it wrong)
## Open questions for implementation  (numbered)
## Handoff  (files in scope, the verification that proves the fix works, what must not regress)

Requirements:
- Every constraint from the problem document gets a row. Every losing candidate gets a rejection entry with a specific reason — which constraint it violates, which scenario breaks it, what it costs. Not "less elegant".
- Where judges disagreed, name the disagreement and state which constraint breaks the tie. Never average scores into a silent winner.
- If two or more blind designers converged on substantially the same design, record that as independent corroboration.
- Cite the problem document's file:line citations when describing what the fix touches.

Return ONLY the markdown document.

=== PROBLEM DOCUMENT ===
${problemDoc}

${CANDIDATE_BLOCK}

${judges.map((j, i) => `=== JUDGE ${i + 1} ===\n${j}`).join('\n\n')}`,
  { label: 'converge:solution-doc', phase: 'Converge', model: 'opus', effort: 'high' })

// The caller writes solutionDoc to <slug>-solution-YYYY-MM-DD.md, next to the problem
// document. The skill ends here — implementation is a separate handoff.
return { problemDoc, solutionDoc, candidates, judges }
