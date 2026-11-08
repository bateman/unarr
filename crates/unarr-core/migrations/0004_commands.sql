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

-- The originals' PendingReleases: releases held back by a delay profile,
-- by a download client that was unavailable, or as a fallback for an
-- episode another release already covers. The release is kept as JSON and
-- the title is parsed again when the row is read.
CREATE TABLE pending_releases (
    id                INTEGER PRIMARY KEY,
    series_id         INTEGER NOT NULL,
    title             TEXT NOT NULL,
    added             TEXT NOT NULL,
    release           TEXT NOT NULL,
    reason            INTEGER NOT NULL DEFAULT 0,
    series_match_type TEXT NOT NULL DEFAULT 'unknown',
    release_source    TEXT NOT NULL DEFAULT 'unknown'
);
CREATE INDEX pending_releases_series_id ON pending_releases (series_id);
