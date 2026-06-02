#!/usr/bin/env python3
"""Tests for the hex memory system (save).

NOTE: Indexing and search were rustified — the legacy `memory_index.py` and
`memory_search.py` scripts (and their `*.legacy.py` shims) were removed in the
fleet-free teardown. The memory subsystem is now native `hex memory`
(see system/skills/memory/SKILL.md). Only `memory_save.py` remains as a Python
script, so this file only exercises memory save.
"""

import shutil
import sqlite3
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

# Add memory scripts to path
SCRIPT_DIR = Path(__file__).resolve().parent.parent / "system" / "skills" / "memory" / "scripts"
sys.path.insert(0, str(SCRIPT_DIR))


def _create_db(db_path):
    """Create memory.db with the full schema."""
    conn = sqlite3.connect(str(db_path))
    conn.executescript("""
        CREATE TABLE IF NOT EXISTS memories (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            content TEXT NOT NULL,
            tags TEXT DEFAULT '',
            source TEXT DEFAULT '',
            created_at TEXT NOT NULL
        );
        CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
            content, tags, source,
            content=memories, content_rowid=id,
            tokenize='unicode61'
        );
        CREATE TRIGGER IF NOT EXISTS memories_ai AFTER INSERT ON memories BEGIN
            INSERT INTO memories_fts(rowid, content, tags, source)
            VALUES (new.id, new.content, new.tags, new.source);
        END;
        CREATE TRIGGER IF NOT EXISTS memories_ad AFTER DELETE ON memories BEGIN
            INSERT INTO memories_fts(memories_fts, rowid, content, tags, source)
            VALUES ('delete', old.id, old.content, old.tags, old.source);
        END;
        CREATE VIRTUAL TABLE IF NOT EXISTS chunks USING fts5(
            source_path, heading, chunk_index, content,
            tokenize='unicode61'
        );
        CREATE TABLE IF NOT EXISTS files (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            path TEXT UNIQUE NOT NULL,
            mtime REAL NOT NULL,
            content_hash TEXT NOT NULL DEFAULT '',
            indexed_at TEXT NOT NULL,
            chunk_count INTEGER DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS metadata (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
    """)
    conn.commit()
    conn.close()


class MemoryTestBase(unittest.TestCase):
    """Base class: creates a temp hex workspace with memory.db."""

    def setUp(self):
        self.test_dir = Path(tempfile.mkdtemp())
        self.hex_dir = self.test_dir / ".hex"
        self.hex_dir.mkdir()
        (self.test_dir / "CLAUDE.md").write_text("# hex\n")
        self.db_path = self.hex_dir / "memory.db"
        _create_db(self.db_path)

    def tearDown(self):
        shutil.rmtree(self.test_dir)


# ── memory_save tests ──────────────────────────────────────────────

class TestMemorySave(MemoryTestBase):

    def test_save_basic(self):
        import memory_save
        with patch.object(memory_save, 'DB_PATH', self.db_path):
            memory_save.save("JWT tokens use httpOnly cookies", tags="auth", source="review")
        conn = sqlite3.connect(str(self.db_path))
        row = conn.execute("SELECT content, tags, source FROM memories WHERE id = 1").fetchone()
        conn.close()
        self.assertEqual(row[0], "JWT tokens use httpOnly cookies")
        self.assertEqual(row[1], "auth")
        self.assertEqual(row[2], "review")

    def test_save_empty_rejected(self):
        import memory_save
        with patch.object(memory_save, 'DB_PATH', self.db_path):
            with self.assertRaises(SystemExit):
                memory_save.save("")

    def test_save_whitespace_rejected(self):
        import memory_save
        with patch.object(memory_save, 'DB_PATH', self.db_path):
            with self.assertRaises(SystemExit):
                memory_save.save("   ")

    def test_save_fts_trigger_fires(self):
        import memory_save
        with patch.object(memory_save, 'DB_PATH', self.db_path):
            memory_save.save("unique_sentinel_xyz", tags="test")
        conn = sqlite3.connect(str(self.db_path))
        rows = conn.execute(
            "SELECT rowid FROM memories_fts WHERE memories_fts MATCH ?",
            ('"unique_sentinel_xyz"',)
        ).fetchall()
        conn.close()
        self.assertEqual(len(rows), 1)

    def test_save_increments_count(self):
        import memory_save
        with patch.object(memory_save, 'DB_PATH', self.db_path):
            memory_save.save("first memory")
            memory_save.save("second memory")
        conn = sqlite3.connect(str(self.db_path))
        count = conn.execute("SELECT COUNT(*) FROM memories").fetchone()[0]
        conn.close()
        self.assertEqual(count, 2)


if __name__ == "__main__":
    unittest.main()
