-- Example migration: create a table for the hello extension.
CREATE TABLE IF NOT EXISTS hello_greetings (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    message   TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
