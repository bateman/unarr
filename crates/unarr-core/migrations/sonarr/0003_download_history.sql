-- The originals' DownloadHistory: one row per thing that happened to a
-- download as the client reported it (grabbed, imported, failed,
-- ignored), by download id. The queue reads the latest row to know the
-- state of a download it finds in a client after a restart.
CREATE TABLE download_history (
    id                 INTEGER PRIMARY KEY,
    event_type         TEXT NOT NULL,
    series_id          INTEGER NOT NULL,
    download_id        TEXT NOT NULL,
    source_title       TEXT NOT NULL,
    date               TEXT NOT NULL,
    protocol           TEXT NOT NULL,
    indexer_id         INTEGER NOT NULL DEFAULT 0,
    download_client_id INTEGER NOT NULL DEFAULT 0,
    release            TEXT,
    data               TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX download_history_download_id ON download_history (download_id);
