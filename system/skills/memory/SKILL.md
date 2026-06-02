---
name: memory
description: >
  Hex memory system — save, search, and retrieve persistent memories across
  sessions. Provides FTS5 + optional vector hybrid search over indexed files.
tags: memory, search, recall, persistence, knowledge
trigger: >
  Agent needs to save or recall information across sessions, or search
  existing memories.
version: 1
---

# Memory System

## Overview

The hex memory system stores persistent facts across sessions using SQLite FTS5 (full-text search) with optional vector embedding for semantic recall. Memories are markdown files with YAML frontmatter stored in `.hex/memory/`.

## Commands

- **hex memory search** — Search memories by keyword or semantic similarity
- **hex memory index** — Index filesystem content into the SQLite database
- **hex memory parse-transcripts** — Parse JSONL transcripts into markdown

> The old Python scripts (`memory_search.py`, `memory_index.py`) were removed —
> the memory subsystem is native to the `hex` binary now. Use the `hex memory`
> subcommands instead.

## Usage

### Search
```bash
hex memory search "query terms"
hex memory search --top 5 "phrase"
hex memory search --compact "keyword"
hex memory search --private "keyword"          # excludes me/, people/, raw/
hex memory search --file PATTERN "keyword"     # restrict to matching file paths
hex memory search --context N "keyword"        # show N lines of context
```

### Index
```bash
hex memory index                # incremental (skips unchanged files)
hex memory index --full         # force re-index all files
hex memory index --stats        # show index statistics
```

### Parse Transcripts
```bash
hex memory parse-transcripts    # parse JSONL transcripts → markdown
```

## Memory Format

Memories are markdown files with YAML frontmatter:

```markdown
---
name: memory-name
description: One-line description for index lookup
type: user | feedback | project | reference
---

Memory content here.
```

## Hybrid Search

If `sqlite_vec` and `fastembed` are installed, search uses vector embeddings for semantic recall. Falls back to FTS5-only when deps are absent.
