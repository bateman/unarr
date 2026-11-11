-- Per-provider failure state, the originals' IndexerStatus and
-- DownloadClientStatus: when failures began, the last one, how far the
-- back-off has escalated and until when the provider is left alone. For
-- indexers, the newest release the last RSS sync saw, so the next sync
-- knows when it has caught up.
CREATE TABLE provider_status (
    kind                       TEXT NOT NULL,
    provider_id                INTEGER NOT NULL,
    initial_failure            TEXT,
    most_recent_failure        TEXT,
    escalation_level           INTEGER NOT NULL DEFAULT 0,
    disabled_till              TEXT,
    last_rss_sync_release_info TEXT,
    PRIMARY KEY (kind, provider_id)
);
