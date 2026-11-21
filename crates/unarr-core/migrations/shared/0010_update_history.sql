-- The originals' UpdateHistory (kept in their log database): the versions
-- the application was started with, oldest first. A start with another
-- version than the last row's is an update, which `UpdateInstalledEvent`
-- tells the notifications about. Unarr records its own version: the one
-- the originals answer as moves only with a new pin.
CREATE TABLE update_history (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    date       TEXT NOT NULL,
    version    TEXT NOT NULL,
    -- `UpdateHistoryEventType`: 1 Installed.
    event_type INTEGER NOT NULL
);
