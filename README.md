# hex-foundation

A minimal, installable template for the hex agent system — a persistent AI workspace for Claude Code that accumulates context, learns your patterns, and improves itself over time.

**For:** engineers on Claude Code who are tired of their agent starting from zero every session.

---

## Quick start

```bash
git clone https://github.com/mrap/hex-foundation /tmp/hex-setup
bash /tmp/hex-setup/install.sh
cd ~/hex && claude
```

Your agent walks you through setup on first run. Three questions, then you're working.

### Prerequisites

- Python 3.9+
- git
- [Claude Code CLI](https://docs.anthropic.com/en/docs/agents-and-tools/claude-code) (`claude`) — warning-only; install separately

The installer also clones two companion repos into `~/.boi` and `~/.hex-events`. Versions pinned in [`VERSIONS`](./VERSIONS).

### Install options

```bash
bash install.sh              # installs to ~/hex
bash install.sh ~/my-hex     # custom location
```

To use a fork of the companions, set `HEX_BOI_REPO` and/or `HEX_EVENTS_REPO` before running install.

---

## What you get

After install, `~/hex/` contains:

```
~/hex/
├── CLAUDE.md         Operating model for Claude Code (system zone + your zone)
├── AGENTS.md         Operating model for other agents (Codex, Cursor, etc.)
├── todo.md           Your priorities and action items
├── me/               About you — me.md (stable), learnings.md (observed patterns)
├── projects/         Per-project context, decisions, meetings, drafts
├── people/           Profiles and relationship notes
├── evolution/        Self-improvement engine — observations, suggestions, changelog
├── landings/         Daily outcome targets
├── raw/              Transcripts, handoffs, unprocessed input
├── specs/            BOI spec drafts
├── .hex/             System files (scripts, skills, memory.db) — managed
└── .claude/commands/ Claude Code slash commands — managed
```

Companion systems installed alongside:

- **[`~/.boi`](https://github.com/mrap/boi)** — parallel Claude Code worker dispatch
- **[`~/.hex-events`](https://github.com/mrap/hex-events)** — reactive event policies

---

## Core ideas

**Persistent memory.** Every observation, decision, and learning gets written to a file — not summarized into a chat bubble that disappears. A SQLite FTS5 index at `.hex/memory.db` makes all of it searchable.

**Operating model.** `CLAUDE.md` ships with 20 core standing orders, a learning engine that records observations to `me/learnings.md` with evidence and dates, and an improvement engine that detects friction, proposes fixes after 3+ occurrences, and tracks what ships.

**Two-zone CLAUDE.md.** The system zone is managed by upgrades; your zone is preserved byte-for-byte. Add your own rules without losing them on every update.

```markdown
<!-- hex:system-start — DO NOT EDIT BELOW THIS LINE -->
... managed by hex
<!-- hex:system-end -->

<!-- hex:user-start — YOUR CUSTOMIZATIONS GO HERE -->
- Always check Jira before starting feature work
- Prefer rebase over merge
<!-- hex:user-end -->
```

---

## Slash commands (inside a Claude Code session)

These are Claude Code slash commands, not shell CLIs. Use them inside a `claude` session running in your hex directory.

| Command | What it does |
|---------|--------------|
| `/hex-startup` | Session init. Loads priorities, today's landings, pending reflection fixes. Triggers onboarding on first run. |
| `/hex-checkpoint` | Mid-session save. Distill pass, handoff file, landings update. |
| `/hex-shutdown` | Session close. Quick distill, deregister session. |
| `/hex-reflect` | Session reflection. Extract learnings, identify failures, propose standing order candidates. |
| `/hex-consolidate` | System hygiene. Audit operating model for contradictions, staleness, orphaned refs. |
| `/hex-debrief` | Weekly walk-through of projects, org signals, relationships, career. |
| `/hex-decide` | Structured decision framework — context, options, reasoning, impact. |
| `/hex-triage` | Route untriaged content from `raw/` to the right files. |
| `/hex-doctor` | Health check. Validate structure, missing files, stale config. |
| `/hex-upgrade` | Pull latest system files from hex-foundation. Runs doctor after. |

---

## Upgrading

Inside your hex instance directory:

```bash
bash .hex/scripts/upgrade.sh
```

Options:

- `--dry-run` — show what would change
- `--skip-boi` / `--skip-events` — skip a companion

What it does:

1. Backs up `.hex/` to `.hex-upgrade-backup-YYYYMMDD/`
2. Fetches the latest `hex-foundation` release
3. Replaces `.hex/` (preserving `memory.db`)
4. Merges `CLAUDE.md`: system zone replaced, user zone preserved
5. Runs `doctor.sh`

Your data (`me/`, `projects/`, `people/`, `evolution/`, `landings/`, `raw/`, `todo.md`) is never touched.

You can also run the upgrade from inside Claude Code via `/hex-upgrade`.

---

## Multi-agent support

`AGENTS.md` ships for Codex, Cursor, Gemini CLI, Aider, or any agent that reads a markdown operating-model file. Slash commands are Claude Code-specific.

---

## Project layout (this repo)

```
hex-foundation/
├── install.sh           Single install entrypoint
├── VERSIONS             Pinned boi / hex-events versions
├── system/              → becomes ~/hex/.hex/ on install
│   ├── scripts/         startup.sh, doctor.sh, upgrade.sh, today.sh
│   ├── commands/        → copied to ~/hex/.claude/commands/ (Claude Code slash commands)
│   ├── skills/memory/   memory_index.py, memory_save.py, memory_search.py
│   └── version.txt
├── templates/           Seeds for CLAUDE.md, AGENTS.md, me.md, todo.md, etc.
├── docs/architecture.md System overview
└── tests/               E2E, full-stack, and memory tests
```

---

## Roadmap

v0.1.0 is the foundation release. Next up:

- Hooks pack: transcript backup, reflection dispatch
- Session lifecycle automation (warming → hot → checkpoint transitions)
- More skills (landings, triage, debrief) split out of CLAUDE.md

Open an issue or PR — the system is meant to evolve.

---

## License

MIT. See [LICENSE](./LICENSE).
