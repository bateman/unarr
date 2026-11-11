-- Phase 3a: completed download handling, rename, notifications.

-- The originals' tables use AUTOINCREMENT, so an id is never reused once a
-- row is deleted; files come and go, and the ids the API hands out must
-- keep counting as the originals' would. One row per table, seeded by the
-- importer from the original's sqlite_sequence and otherwise from max(id).
CREATE TABLE sequences (
    name  TEXT PRIMARY KEY,
    value INTEGER NOT NULL
);

