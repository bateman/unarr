-- Phase 3b: import lists.

-- What each list returned at its last sync, the originals' ImportListItems:
-- the library is cleaned against these rows, and a sync that makes rows
-- disappear is what asks for a clean.
CREATE TABLE import_list_items (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    import_list_id INTEGER NOT NULL,
    title          TEXT NOT NULL,
    tvdb_id        INTEGER NOT NULL DEFAULT 0,
    year           INTEGER NOT NULL DEFAULT 0,
    tmdb_id        INTEGER NOT NULL DEFAULT 0,
    imdb_id        TEXT,
    mal_id         INTEGER NOT NULL DEFAULT 0,
    ani_list_id    INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX import_list_items_list ON import_list_items (import_list_id);
