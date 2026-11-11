-- Unarr schema, version 1. One database per personality.
--
-- Conventions: snake_case names, enumerations stored as the camelCase strings
-- the compatible API emits ("ended", "anime", "downloadFolderImported"),
-- timestamps as UTC text "YYYY-MM-DDTHH:MM:SSZ", dates as "YYYY-MM-DD", lists
-- of structured values that Unarr never queries (images, media info, provider
-- settings, profile items) as JSON text.

CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE tags (
    id    INTEGER PRIMARY KEY,
    label TEXT NOT NULL UNIQUE
);

CREATE TABLE root_folders (
    id   INTEGER PRIMARY KEY,
    path TEXT NOT NULL UNIQUE
);

CREATE TABLE quality_definitions (
    id             INTEGER PRIMARY KEY,
    quality_id     INTEGER NOT NULL UNIQUE,
    title          TEXT NOT NULL,
    min_size       REAL,
    max_size       REAL,
    preferred_size REAL
);

CREATE TABLE custom_formats (
    id                    INTEGER PRIMARY KEY,
    name                  TEXT NOT NULL UNIQUE,
    include_when_renaming INTEGER NOT NULL DEFAULT 0,
    specifications        TEXT NOT NULL DEFAULT '[]'
);

CREATE TABLE quality_profiles (
    id                       INTEGER PRIMARY KEY,
    name                     TEXT NOT NULL UNIQUE,
    cutoff                   INTEGER NOT NULL,
    upgrade_allowed          INTEGER NOT NULL DEFAULT 0,
    items                    TEXT NOT NULL,
    format_items             TEXT NOT NULL DEFAULT '[]',
    min_format_score         INTEGER NOT NULL DEFAULT 0,
    cutoff_format_score      INTEGER NOT NULL DEFAULT 0,
    min_upgrade_format_score INTEGER NOT NULL DEFAULT 1
);

-- Indexers, download clients, notifications, import lists and metadata

-- consumers share one shape: an implementation, a settings document and a

-- handful of flags that differ per kind (kept in `attributes`). Each kind has

-- its own id sequence in the originals, and the API exposes them per kind.

CREATE TABLE providers (
    kind            TEXT NOT NULL,
    id              INTEGER NOT NULL,
    name            TEXT NOT NULL,
    implementation  TEXT NOT NULL,
    config_contract TEXT NOT NULL,
    settings        TEXT NOT NULL DEFAULT '{}',
    tags            TEXT NOT NULL DEFAULT '[]',
    attributes      TEXT NOT NULL DEFAULT '{}',
    PRIMARY KEY (kind, id)
);

-- Key/value settings, keys lower-case as the originals store them.

CREATE TABLE config (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE delay_profiles (
    id                                  INTEGER PRIMARY KEY,
    enable_usenet                       INTEGER NOT NULL DEFAULT 1,
    enable_torrent                      INTEGER NOT NULL DEFAULT 1,
    preferred_protocol                  TEXT NOT NULL DEFAULT 'usenet',
    usenet_delay                        INTEGER NOT NULL DEFAULT 0,
    torrent_delay                       INTEGER NOT NULL DEFAULT 0,
    sort_order                          INTEGER NOT NULL,
    tags                                TEXT NOT NULL DEFAULT '[]',
    bypass_if_highest_quality           INTEGER NOT NULL DEFAULT 0,
    bypass_if_above_custom_format_score INTEGER NOT NULL DEFAULT 0,
    minimum_custom_format_score         INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE release_profiles (
    id         INTEGER PRIMARY KEY,
    name       TEXT,
    enabled    INTEGER NOT NULL DEFAULT 1,
    required   TEXT NOT NULL DEFAULT '[]',
    ignored    TEXT NOT NULL DEFAULT '[]',
    indexer_id INTEGER NOT NULL DEFAULT 0,
    tags       TEXT NOT NULL DEFAULT '[]'
);

CREATE TABLE remote_path_mappings (
    id          INTEGER PRIMARY KEY,
    host        TEXT NOT NULL,
    remote_path TEXT NOT NULL,
    local_path  TEXT NOT NULL
);

CREATE TABLE custom_filters (
    id      INTEGER PRIMARY KEY,
    type    TEXT NOT NULL,
    label   TEXT NOT NULL,
    filters TEXT NOT NULL DEFAULT '[]'
);

CREATE TABLE auto_tagging (
    id                        INTEGER PRIMARY KEY,
    name                      TEXT NOT NULL,
    remove_tags_automatically INTEGER NOT NULL DEFAULT 0,
    tags                      TEXT NOT NULL DEFAULT '[]',
    specifications            TEXT NOT NULL DEFAULT '[]'
);

CREATE TABLE users (
    id         INTEGER PRIMARY KEY,
    identifier TEXT NOT NULL,
    username   TEXT NOT NULL,
    password   TEXT NOT NULL,
    salt       TEXT,
    iterations INTEGER
);

CREATE TABLE scheduled_tasks (
    id               INTEGER PRIMARY KEY,
    type_name        TEXT NOT NULL UNIQUE,
    interval_minutes INTEGER NOT NULL,
    last_execution   TEXT,
    last_start_time  TEXT
);

-- Alternate titles from the scene-mapping services, keyed by TVDB id.
