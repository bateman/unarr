-- The season statistics behind every series list aggregate over all the
-- episodes. With these columns in one index the aggregate reads the index
-- alone, in order, instead of fetching each episode's row from the table.
CREATE INDEX episodes_statistics
    ON episodes(series_id, season_number, air_date_utc, monitored, episode_file_id, air_date);
