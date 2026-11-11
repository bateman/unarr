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
