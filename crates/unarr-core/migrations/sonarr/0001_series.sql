-- The series side of schema version 1; the tables every personality has
-- are in ../shared/0001_core.sql, applied with it.

CREATE TABLE series (
    id                   INTEGER PRIMARY KEY,
    tvdb_id              INTEGER NOT NULL UNIQUE,
    tv_rage_id           INTEGER NOT NULL DEFAULT 0,
    tv_maze_id           INTEGER NOT NULL DEFAULT 0,
    tmdb_id              INTEGER NOT NULL DEFAULT 0,
    imdb_id              TEXT,
    title                TEXT NOT NULL,
    title_slug           TEXT NOT NULL,
    clean_title          TEXT NOT NULL,
    sort_title           TEXT,
    status               TEXT NOT NULL,
    overview             TEXT,
    air_time             TEXT,
    network              TEXT,
    certification        TEXT,
    images               TEXT NOT NULL DEFAULT '[]',
    genres               TEXT NOT NULL DEFAULT '[]',
    actors               TEXT NOT NULL DEFAULT '[]',
    path                 TEXT NOT NULL,
    monitored            INTEGER NOT NULL DEFAULT 1,
    season_folder        INTEGER NOT NULL DEFAULT 1,
    monitor_new_items    TEXT NOT NULL DEFAULT 'all',
    use_scene_numbering  INTEGER NOT NULL DEFAULT 0,
    runtime              INTEGER NOT NULL DEFAULT 0,
    series_type          TEXT NOT NULL DEFAULT 'standard',
    year                 INTEGER NOT NULL DEFAULT 0,
    first_aired          TEXT,
    last_aired           TEXT,
    quality_profile_id   INTEGER NOT NULL,
    original_language_id INTEGER NOT NULL DEFAULT 1,
    ratings_votes        INTEGER NOT NULL DEFAULT 0,
    ratings_value        REAL NOT NULL DEFAULT 0,
    added                TEXT NOT NULL,
    add_options          TEXT,
    last_info_sync       TEXT,
    last_disk_sync       TEXT,
    mal_ids              TEXT,
    anilist_ids          TEXT
);

CREATE TABLE seasons (
    series_id     INTEGER NOT NULL REFERENCES series(id) ON DELETE CASCADE,
    season_number INTEGER NOT NULL,
    monitored     INTEGER NOT NULL DEFAULT 1,
    images        TEXT NOT NULL DEFAULT '[]',
    PRIMARY KEY (series_id, season_number)
);

CREATE TABLE series_tags (
    series_id INTEGER NOT NULL REFERENCES series(id) ON DELETE CASCADE,
    tag_id    INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
    PRIMARY KEY (series_id, tag_id)
);

CREATE TABLE episode_files (
    id                 INTEGER PRIMARY KEY,
    series_id          INTEGER NOT NULL REFERENCES series(id) ON DELETE CASCADE,
    season_number      INTEGER NOT NULL,
    relative_path      TEXT NOT NULL,
    original_file_path TEXT,
    size               INTEGER NOT NULL DEFAULT 0,
    date_added         TEXT NOT NULL,
    scene_name         TEXT,
    release_group      TEXT,
    release_hash       TEXT,
    quality_id         INTEGER NOT NULL DEFAULT 0,
    revision_version   INTEGER NOT NULL DEFAULT 1,
    revision_real      INTEGER NOT NULL DEFAULT 0,
    revision_is_repack INTEGER NOT NULL DEFAULT 0,
    languages          TEXT NOT NULL DEFAULT '[]',
    media_info         TEXT,
    indexer_flags      INTEGER NOT NULL DEFAULT 0,
    release_type       TEXT NOT NULL DEFAULT 'unknown'
);

CREATE INDEX episode_files_series ON episode_files(series_id);

CREATE TABLE episodes (
    id                            INTEGER PRIMARY KEY,
    series_id                     INTEGER NOT NULL REFERENCES series(id) ON DELETE CASCADE,
    tvdb_id                       INTEGER NOT NULL DEFAULT 0,
    season_number                 INTEGER NOT NULL,
    episode_number                INTEGER NOT NULL,
    title                         TEXT,
    overview                      TEXT,
    episode_file_id               INTEGER NOT NULL DEFAULT 0,
    absolute_episode_number       INTEGER,
    scene_absolute_episode_number INTEGER,
    scene_season_number           INTEGER,
    scene_episode_number          INTEGER,
    unverified_scene_numbering    INTEGER NOT NULL DEFAULT 0,
    monitored                     INTEGER NOT NULL DEFAULT 1,
    air_date                      TEXT,
    air_date_utc                  TEXT,
    runtime                       INTEGER NOT NULL DEFAULT 0,
    finale_type                   TEXT,
    images                        TEXT NOT NULL DEFAULT '[]',
    ratings_votes                 INTEGER NOT NULL DEFAULT 0,
    ratings_value                 REAL NOT NULL DEFAULT 0,
    last_search_time              TEXT,
    aired_after_season_number     INTEGER,
    aired_before_season_number    INTEGER,
    aired_before_episode_number   INTEGER
);

CREATE INDEX episodes_series ON episodes(series_id, season_number, episode_number);

CREATE INDEX episodes_air_date_utc ON episodes(air_date_utc);

CREATE INDEX episodes_file ON episodes(episode_file_id);

CREATE TABLE history (
    id                 INTEGER PRIMARY KEY,
    series_id          INTEGER NOT NULL,
    episode_id         INTEGER NOT NULL,
    source_title       TEXT,
    date               TEXT NOT NULL,
    event_type         TEXT NOT NULL,
    quality_id         INTEGER NOT NULL DEFAULT 0,
    revision_version   INTEGER NOT NULL DEFAULT 1,
    revision_real      INTEGER NOT NULL DEFAULT 0,
    revision_is_repack INTEGER NOT NULL DEFAULT 0,
    languages          TEXT NOT NULL DEFAULT '[]',
    data               TEXT NOT NULL DEFAULT '{}',
    download_id        TEXT
);

CREATE INDEX history_date ON history(date);

CREATE INDEX history_series ON history(series_id);

CREATE INDEX history_episode ON history(episode_id);

CREATE TABLE naming_config (
    id                              INTEGER PRIMARY KEY CHECK (id = 1),
    rename_episodes                 INTEGER NOT NULL DEFAULT 0,
    replace_illegal_characters      INTEGER NOT NULL DEFAULT 1,
    colon_replacement_format        INTEGER NOT NULL DEFAULT 4,
    custom_colon_replacement_format TEXT NOT NULL DEFAULT '',
    multi_episode_style             INTEGER NOT NULL DEFAULT 5,
    standard_episode_format         TEXT NOT NULL,
    daily_episode_format            TEXT NOT NULL,
    anime_episode_format            TEXT NOT NULL,
    series_folder_format            TEXT NOT NULL,
    season_folder_format            TEXT NOT NULL,
    specials_folder_format          TEXT NOT NULL
);

CREATE TABLE import_list_exclusions (
    id      INTEGER PRIMARY KEY,
    tvdb_id INTEGER NOT NULL,
    title   TEXT NOT NULL
);

CREATE TABLE scene_mappings (
    id                  INTEGER PRIMARY KEY,
    tvdb_id             INTEGER NOT NULL,
    season_number       INTEGER,
    search_term         TEXT,
    parse_term          TEXT,
    title               TEXT NOT NULL,
    type                TEXT,
    scene_season_number INTEGER,
    filter_regex        TEXT,
    scene_origin        TEXT,
    search_mode         INTEGER,
    comment             TEXT
);

CREATE INDEX scene_mappings_tvdb ON scene_mappings(tvdb_id);

CREATE TABLE blocklist (
    id                 INTEGER PRIMARY KEY,
    series_id          INTEGER NOT NULL,
    episode_ids        TEXT NOT NULL DEFAULT '[]',
    source_title       TEXT,
    quality_id         INTEGER NOT NULL DEFAULT 0,
    revision_version   INTEGER NOT NULL DEFAULT 1,
    revision_real      INTEGER NOT NULL DEFAULT 0,
    revision_is_repack INTEGER NOT NULL DEFAULT 0,
    date               TEXT NOT NULL,
    published_date     TEXT,
    size               INTEGER,
    protocol           TEXT NOT NULL DEFAULT 'unknown',
    indexer            TEXT,
    message            TEXT,
    torrent_info_hash  TEXT,
    languages          TEXT NOT NULL DEFAULT '[]',
    indexer_flags      INTEGER NOT NULL DEFAULT 0,
    release_type       TEXT NOT NULL DEFAULT 'unknown'
);
