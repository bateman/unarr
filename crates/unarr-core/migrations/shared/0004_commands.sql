-- The originals' Commands: every command the queue ran, kept a day so a
-- client can read a command back after it has left the in-memory queue.
CREATE TABLE commands (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL,
    body        TEXT NOT NULL DEFAULT '{}',
    priority    INTEGER NOT NULL DEFAULT 0,
    status      INTEGER NOT NULL DEFAULT 0,
    result      INTEGER NOT NULL DEFAULT 0,
    queued_at   TEXT NOT NULL,
    started_at  TEXT,
    ended_at    TEXT,
    duration_ms INTEGER,
    exception   TEXT,
    trigger     INTEGER NOT NULL DEFAULT 0,
    message     TEXT
);
