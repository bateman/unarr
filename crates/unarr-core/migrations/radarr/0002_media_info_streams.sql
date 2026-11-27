-- Radarr's migration 243 (`mediainfo_multiple_streams`), for the databases
-- Unarr already wrote, and run by the importer on a database older than
-- 243. The media info of a movie file keeps one entry per audio stream
-- (`audioStreams`) and per subtitle stream (`subtitleStreams`) instead of
-- the primary audio stream's fields and two lists of languages. The
-- primary stream's fields move to the first audio stream, which is `und`
-- when no stream had a language; a JSON null stays where the field was
-- missing, as the original writes it. A row already in
-- the new shape, or with neither list, is left as it is; a row that is not
-- JSON, or whose lists are not lists, loses its media info, which the next
-- scan reads again. The movies' metadata is marked stale, as the original
-- does, so that the next refresh reads every movie and its files again.
UPDATE movie_files SET media_info = NULL
WHERE media_info IS NOT NULL AND trim(media_info) <> '' AND NOT json_valid(media_info);

UPDATE movie_files SET media_info = NULL
WHERE json_valid(media_info)
  AND NOT (json_type(media_info, '$.audioStreams') IS 'array'
           AND json_type(media_info, '$.subtitleStreams') IS 'array')
  AND (coalesce(json_type(media_info, '$.audioLanguages'), 'null') <> 'null'
       OR coalesce(json_type(media_info, '$.subtitles'), 'null') <> 'null')
  AND (json_type(media_info, '$.audioLanguages') IS NOT 'array'
       OR json_type(media_info, '$.subtitles') IS NOT 'array');

UPDATE movie_files SET media_info = json_remove(
    json_set(
        media_info,
        '$.audioStreams', json(CASE
            WHEN json_array_length(media_info, '$.audioLanguages') = 0 THEN json_array(json_object(
                'language', 'und',
                'format', json_extract(media_info, '$.audioFormat'),
                'codecId', json_extract(media_info, '$.audioCodecID'),
                'profile', json_extract(media_info, '$.audioProfile'),
                'bitrate', json_extract(media_info, '$.audioBitrate'),
                'channels', json_extract(media_info, '$.audioChannels'),
                'channelPositions', json_extract(media_info, '$.audioChannelPositions')))
            ELSE (SELECT json_group_array(CASE WHEN l.key = 0 THEN json_object(
                    'language', l.value,
                    'format', json_extract(movie_files.media_info, '$.audioFormat'),
                    'codecId', json_extract(movie_files.media_info, '$.audioCodecID'),
                    'profile', json_extract(movie_files.media_info, '$.audioProfile'),
                    'bitrate', json_extract(movie_files.media_info, '$.audioBitrate'),
                    'channels', json_extract(movie_files.media_info, '$.audioChannels'),
                    'channelPositions', json_extract(movie_files.media_info, '$.audioChannelPositions'))
                  ELSE json_object('language', l.value) END)
                  FROM (SELECT key, value FROM json_each(movie_files.media_info, '$.audioLanguages')
                        ORDER BY key) AS l)
        END),
        '$.subtitleStreams', json((SELECT json_group_array(json_object('language', s.value))
            FROM (SELECT key, value FROM json_each(movie_files.media_info, '$.subtitles')
                  ORDER BY key) AS s))),
    '$.audioLanguages', '$.audioFormat', '$.audioCodecID', '$.audioProfile',
    '$.audioBitrate', '$.audioChannels', '$.audioChannelPositions', '$.subtitles')
WHERE json_valid(media_info)
  AND NOT (json_type(media_info, '$.audioStreams') IS 'array'
           AND json_type(media_info, '$.subtitleStreams') IS 'array')
  AND json_type(media_info, '$.audioLanguages') IS 'array'
  AND json_type(media_info, '$.subtitles') IS 'array';

UPDATE movie_metadata SET last_info_sync = '2026-01-01T00:00:00Z';
