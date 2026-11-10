-- The files that go with an episode file and are not the video: subtitles
-- and whatever else the settings say to import (`SubtitleFiles` and
-- `ExtraFiles` of the originals). They are renamed with the episode file
-- and leave with it.
CREATE TABLE extra_files (
    id              INTEGER PRIMARY KEY,
    -- 'subtitle' or 'other'.
    kind            TEXT NOT NULL,
    series_id       INTEGER NOT NULL REFERENCES series(id) ON DELETE CASCADE,
    season_number   INTEGER,
    episode_file_id INTEGER,
    relative_path   TEXT NOT NULL,
    extension       TEXT NOT NULL,
    added           TEXT,
    last_updated    TEXT NOT NULL,
    -- Subtitles only.
    language        INTEGER NOT NULL DEFAULT 0,
    language_tags   TEXT NOT NULL DEFAULT '[]',
    title           TEXT,
    copy            INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX extra_files_series_id ON extra_files (series_id);
CREATE INDEX extra_files_episode_file_id ON extra_files (episode_file_id);
