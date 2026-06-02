---
name: hex-save
description: >
  Save current session. Parses transcripts into readable daily files
  and rebuilds the memory search index.
---

# /hex-save — Save Session

## Steps

1. **Parse transcripts**: Convert raw .jsonl session data into readable daily markdown.

```bash
hex memory parse-transcripts
```

2. **Rebuild memory index**: Update the search index with any new or changed files.

```bash
hex memory index
```

3. **Report**: Tell the user what was saved.

Format: "Saved. [N] transcript(s) parsed, [M] files indexed."
