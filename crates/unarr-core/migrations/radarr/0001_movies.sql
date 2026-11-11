-- The movie side of Radarr's schema, version 1; the tables every
-- personality has are in ../shared, applied with it. Same conventions.

-- Radarr's quality profiles carry the language a release must be in
-- (-1 any, -2 original); Sonarr's lost theirs to custom formats.
ALTER TABLE quality_profiles ADD COLUMN language INTEGER NOT NULL DEFAULT -1;

-- What the metadata server knows about a movie, the originals'
-- MovieMetadata: one row per TMDB id, for the movies of the library and
-- for those a collection or an import list only names.
CREATE TABLE movie_metadata (
    id                   INTEGER PRIMARY KEY,
    tmdb_id              INTEGER NOT NULL UNIQUE,
    imdb_id              TEXT,
    title                TEXT NOT NULL,
    sort_title           TEXT,
    clean_title          TEXT,
    original_title       TEXT,
    clean_original_title TEXT,
    original_language_id INTEGER NOT NULL DEFAULT 1,
    status               TEXT NOT NULL,
    overview             TEXT,
    images               TEXT NOT NULL DEFAULT '[]',
    genres               TEXT NOT NULL DEFAULT '[]',
    keywords             TEXT NOT NULL DEFAULT '[]',
    ratings              TEXT NOT NULL DEFAULT '{}',
    recommendations      TEXT NOT NULL DEFAULT '[]',
    runtime              INTEGER NOT NULL DEFAULT 0,
    in_cinemas           TEXT,
    physical_release     TEXT,
    digital_release      TEXT,
    year                 INTEGER NOT NULL DEFAULT 0,
    secondary_year       INTEGER,
    certification        TEXT,
    youtube_trailer_id   TEXT,
    studio               TEXT,
    website              TEXT,
    popularity           REAL NOT NULL DEFAULT 0,
    collection_tmdb_id   INTEGER NOT NULL DEFAULT 0,
    collection_title     TEXT,
    last_info_sync       TEXT
);
CREATE INDEX movie_metadata_collection ON movie_metadata(collection_tmdb_id);

-- A movie of the library: the user's side of it.
CREATE TABLE movies (
    id                   INTEGER PRIMARY KEY,
    movie_metadata_id    INTEGER NOT NULL UNIQUE REFERENCES movie_metadata(id),
    path                 TEXT NOT NULL,
    monitored            INTEGER NOT NULL DEFAULT 1,
    quality_profile_id   INTEGER NOT NULL,
    minimum_availability TEXT NOT NULL DEFAULT 'released',
    movie_file_id        INTEGER NOT NULL DEFAULT 0,
    tags                 TEXT NOT NULL DEFAULT '[]',
    added                TEXT,
    add_options          TEXT,
    last_search_time     TEXT
);

CREATE TABLE movie_files (
    id                 INTEGER PRIMARY KEY,
    movie_id           INTEGER NOT NULL,
    relative_path      TEXT NOT NULL,
    original_file_path TEXT,
    size               INTEGER NOT NULL DEFAULT 0,
    date_added         TEXT NOT NULL,
    scene_name         TEXT,
    release_group      TEXT,
    edition            TEXT,
    quality_id         INTEGER NOT NULL DEFAULT 0,
    revision_version   INTEGER NOT NULL DEFAULT 1,
    revision_real      INTEGER NOT NULL DEFAULT 0,
    revision_is_repack INTEGER NOT NULL DEFAULT 0,
    languages          TEXT NOT NULL DEFAULT '[]',
    media_info         TEXT,
    indexer_flags      INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX movie_files_movie ON movie_files(movie_id);

CREATE TABLE alternative_titles (
    id                INTEGER PRIMARY KEY,
    movie_metadata_id INTEGER NOT NULL,
    title             TEXT NOT NULL,
    clean_title       TEXT,
    source_type       TEXT NOT NULL DEFAULT 'tmdb'
);
CREATE INDEX alternative_titles_metadata ON alternative_titles(movie_metadata_id);

CREATE TABLE movie_translations (
    id                INTEGER PRIMARY KEY,
    movie_metadata_id INTEGER NOT NULL,
    language_id       INTEGER NOT NULL,
    title             TEXT,
    clean_title       TEXT,
    overview          TEXT
);
CREATE INDEX movie_translations_metadata ON movie_translations(movie_metadata_id, language_id);

CREATE TABLE credits (
    id                INTEGER PRIMARY KEY,
    movie_metadata_id INTEGER NOT NULL,
    credit_tmdb_id    TEXT,
    person_tmdb_id    INTEGER NOT NULL DEFAULT 0,
    name              TEXT,
    images            TEXT NOT NULL DEFAULT '[]',
    character         TEXT,
    sort_order        INTEGER NOT NULL DEFAULT 0,
    job               TEXT,
    department        TEXT,
    -- 'cast' or 'crew'.
    type              TEXT NOT NULL
);
CREATE INDEX credits_metadata ON credits(movie_metadata_id);

CREATE TABLE collections (
    id                   INTEGER PRIMARY KEY,
    tmdb_id              INTEGER NOT NULL UNIQUE,
    title                TEXT NOT NULL,
    sort_title           TEXT,
    clean_title          TEXT,
    overview             TEXT,
    images               TEXT NOT NULL DEFAULT '[]',
    monitored            INTEGER NOT NULL DEFAULT 0,
    quality_profile_id   INTEGER NOT NULL,
    root_folder_path     TEXT,
    minimum_availability TEXT NOT NULL DEFAULT 'released',
    search_on_add        INTEGER NOT NULL DEFAULT 0,
    tags                 TEXT NOT NULL DEFAULT '[]',
    added                TEXT,
    last_info_sync       TEXT
);

-- Movies the import lists must not add again, the originals'
-- ImportExclusions.
CREATE TABLE import_exclusions (
    id          INTEGER PRIMARY KEY,
    tmdb_id     INTEGER NOT NULL,
    movie_title TEXT,
    movie_year  INTEGER NOT NULL DEFAULT 0
);

-- What each list returned at its last sync, the originals'
-- ImportListMovies.
CREATE TABLE import_list_movies (
    id                INTEGER PRIMARY KEY,
    import_list_id    INTEGER NOT NULL,
    movie_metadata_id INTEGER NOT NULL
);

CREATE TABLE history (
    id                 INTEGER PRIMARY KEY,
    movie_id           INTEGER NOT NULL,
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
CREATE INDEX history_movie ON history(movie_id);
CREATE INDEX history_download_id ON history(download_id);

CREATE TABLE blocklist (
    id                 INTEGER PRIMARY KEY,
    movie_id           INTEGER NOT NULL,
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
    indexer_flags      INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE download_history (
    id                 INTEGER PRIMARY KEY,
    event_type         TEXT NOT NULL,
    movie_id           INTEGER NOT NULL,
    download_id        TEXT NOT NULL,
    source_title       TEXT NOT NULL,
    date               TEXT NOT NULL,
    protocol           TEXT NOT NULL,
    indexer_id         INTEGER NOT NULL DEFAULT 0,
    download_client_id INTEGER NOT NULL DEFAULT 0,
    release            TEXT,
    data               TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX download_history_download_id ON download_history(download_id);

CREATE TABLE pending_releases (
    id          INTEGER PRIMARY KEY,
    movie_id    INTEGER NOT NULL,
    title       TEXT NOT NULL,
    added       TEXT NOT NULL,
    release     TEXT NOT NULL,
    parsed_info TEXT,
    reason      INTEGER NOT NULL DEFAULT 0,
    additional_info TEXT
);

-- The files that go with a movie file and are not the video: the
-- originals' SubtitleFiles, ExtraFiles and MetadataFiles.
CREATE TABLE extra_files (
    id            INTEGER PRIMARY KEY,
    -- 'subtitle', 'metadata' or 'other'; ids are unique within a kind, as
    -- the originals keep one table for each.
    kind          TEXT NOT NULL,
    source_id     INTEGER NOT NULL,
    movie_id      INTEGER NOT NULL,
    movie_file_id INTEGER,
    relative_path TEXT NOT NULL,
    extension     TEXT NOT NULL,
    added         TEXT,
    last_updated  TEXT,
    -- Subtitles only.
    language      INTEGER NOT NULL DEFAULT 0,
    language_tags TEXT NOT NULL DEFAULT '[]',
    title         TEXT,
    copy          INTEGER NOT NULL DEFAULT 0,
    -- Metadata files only.
    consumer      TEXT,
    metadata_type INTEGER,
    hash          TEXT
);
CREATE INDEX extra_files_movie ON extra_files(movie_id);

CREATE TABLE naming_config (
    id                         INTEGER PRIMARY KEY CHECK (id = 1),
    rename_movies              INTEGER NOT NULL DEFAULT 0,
    replace_illegal_characters INTEGER NOT NULL DEFAULT 1,
    colon_replacement_format   INTEGER NOT NULL DEFAULT 0,
    standard_movie_format      TEXT NOT NULL,
    movie_folder_format        TEXT NOT NULL
);
