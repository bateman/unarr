//! Episodes and episode files.

use chrono::{DateTime, Utc};
use rusqlite::{named_params, params, Connection, Row};

use super::paging::Paging;
use super::{from_json, parse_date, parse_enum, parse_ts, parse_ts_required, quality_from_row, to_json, ts};
use crate::model::{Episode, EpisodeFile, Image, Ratings, ReleaseType};
use crate::time;

pub fn insert_episode(conn: &Connection, e: &Episode) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO episodes (id, series_id, tvdb_id, season_number, episode_number, title, overview,
            episode_file_id, absolute_episode_number, scene_absolute_episode_number, scene_season_number,
            scene_episode_number, unverified_scene_numbering, monitored, air_date, air_date_utc, runtime,
            finale_type, images, ratings_votes, ratings_value, last_search_time, aired_after_season_number,
            aired_before_season_number, aired_before_episode_number)
         VALUES (:id, :series_id, :tvdb_id, :season_number, :episode_number, :title, :overview,
            :episode_file_id, :absolute_episode_number, :scene_absolute_episode_number, :scene_season_number,
            :scene_episode_number, :unverified_scene_numbering, :monitored, :air_date, :air_date_utc, :runtime,
            :finale_type, :images, :ratings_votes, :ratings_value, :last_search_time, :aired_after_season_number,
            :aired_before_season_number, :aired_before_episode_number)",
        named_params! {
            ":id": e.id, ":series_id": e.series_id, ":tvdb_id": e.tvdb_id, ":season_number": e.season_number,
            ":episode_number": e.episode_number, ":title": e.title, ":overview": e.overview,
            ":episode_file_id": e.episode_file_id, ":absolute_episode_number": e.absolute_episode_number,
            ":scene_absolute_episode_number": e.scene_absolute_episode_number,
            ":scene_season_number": e.scene_season_number, ":scene_episode_number": e.scene_episode_number,
            ":unverified_scene_numbering": e.unverified_scene_numbering, ":monitored": e.monitored,
            ":air_date": e.air_date.map(|d| d.to_string()), ":air_date_utc": ts(e.air_date_utc),
            ":runtime": e.runtime, ":finale_type": e.finale_type, ":images": to_json(&e.images)?,
            ":ratings_votes": e.ratings.votes, ":ratings_value": e.ratings.value,
            ":last_search_time": ts(e.last_search_time),
            ":aired_after_season_number": e.aired_after_season_number,
            ":aired_before_season_number": e.aired_before_season_number,
            ":aired_before_episode_number": e.aired_before_episode_number,
        },
    )?;
    Ok(())
}

/// Every column of an existing episode but its file, which only imports
/// and deletions set.
pub fn update_episode(conn: &Connection, e: &Episode) -> anyhow::Result<()> {
    conn.execute(
        "UPDATE episodes SET series_id = :series_id, tvdb_id = :tvdb_id, season_number = :season_number,
            episode_number = :episode_number, title = :title, overview = :overview,
            absolute_episode_number = :absolute_episode_number,
            scene_absolute_episode_number = :scene_absolute_episode_number,
            scene_season_number = :scene_season_number, scene_episode_number = :scene_episode_number,
            unverified_scene_numbering = :unverified_scene_numbering, monitored = :monitored,
            air_date = :air_date, air_date_utc = :air_date_utc, runtime = :runtime,
            finale_type = :finale_type, images = :images, ratings_votes = :ratings_votes,
            ratings_value = :ratings_value, last_search_time = :last_search_time,
            aired_after_season_number = :aired_after_season_number,
            aired_before_season_number = :aired_before_season_number,
            aired_before_episode_number = :aired_before_episode_number
         WHERE id = :id",
        named_params! {
            ":id": e.id, ":series_id": e.series_id, ":tvdb_id": e.tvdb_id, ":season_number": e.season_number,
            ":episode_number": e.episode_number, ":title": e.title, ":overview": e.overview,
            ":absolute_episode_number": e.absolute_episode_number,
            ":scene_absolute_episode_number": e.scene_absolute_episode_number,
            ":scene_season_number": e.scene_season_number, ":scene_episode_number": e.scene_episode_number,
            ":unverified_scene_numbering": e.unverified_scene_numbering, ":monitored": e.monitored,
            ":air_date": e.air_date.map(|d| d.to_string()), ":air_date_utc": ts(e.air_date_utc),
            ":runtime": e.runtime, ":finale_type": e.finale_type, ":images": to_json(&e.images)?,
            ":ratings_votes": e.ratings.votes, ":ratings_value": e.ratings.value,
            ":last_search_time": ts(e.last_search_time),
            ":aired_after_season_number": e.aired_after_season_number,
            ":aired_before_season_number": e.aired_before_season_number,
            ":aired_before_episode_number": e.aired_before_episode_number,
        },
    )?;
    Ok(())
}

/// Episodes by id, gone.
pub fn delete_episodes(conn: &Connection, ids: &[i64]) -> anyhow::Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    conn.execute(
        "DELETE FROM episodes WHERE id IN (SELECT value FROM json_each(?1))",
        params![serde_json::to_string(ids)?],
    )?;
    Ok(())
}

fn episode_from_row(row: &Row) -> anyhow::Result<Episode> {
    Ok(Episode {
        id: row.get("id")?,
        series_id: row.get("series_id")?,
        tvdb_id: row.get("tvdb_id")?,
        season_number: row.get("season_number")?,
        episode_number: row.get("episode_number")?,
        title: row.get("title")?,
        overview: row.get("overview")?,
        episode_file_id: row.get("episode_file_id")?,
        absolute_episode_number: row.get("absolute_episode_number")?,
        scene_absolute_episode_number: row.get("scene_absolute_episode_number")?,
        scene_season_number: row.get("scene_season_number")?,
        scene_episode_number: row.get("scene_episode_number")?,
        unverified_scene_numbering: row.get("unverified_scene_numbering")?,
        monitored: row.get("monitored")?,
        air_date: parse_date(row.get("air_date")?)?,
        air_date_utc: parse_ts(row.get("air_date_utc")?)?,
        runtime: row.get("runtime")?,
        finale_type: row.get("finale_type")?,
        images: from_json::<Vec<Image>>(row.get("images")?)?,
        ratings: Ratings {
            votes: row.get("ratings_votes")?,
            value: row.get("ratings_value")?,
        },
        last_search_time: parse_ts(row.get("last_search_time")?)?,
        aired_after_season_number: row.get("aired_after_season_number")?,
        aired_before_season_number: row.get("aired_before_season_number")?,
        aired_before_episode_number: row.get("aired_before_episode_number")?,
    })
}

fn query_episodes(
    conn: &Connection,
    sql: &str,
    params: &[&dyn rusqlite::ToSql],
) -> anyhow::Result<Vec<Episode>> {
    let mut stmt = conn.prepare(sql)?;
    let mut rows = stmt.query(params)?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(episode_from_row(row)?);
    }
    Ok(out)
}

/// Episodes of one series, optionally one season, in insertion order as the
/// originals list them.
pub fn list_episodes(conn: &Connection, series_id: i64, season: Option<i64>) -> anyhow::Result<Vec<Episode>> {
    query_episodes(
        conn,
        "SELECT * FROM episodes WHERE series_id = ?1 AND (?2 IS NULL OR season_number = ?2) ORDER BY id",
        &[&series_id, &season],
    )
}

pub fn get_episode(conn: &Connection, id: i64) -> anyhow::Result<Option<Episode>> {
    Ok(query_episodes(conn, "SELECT * FROM episodes WHERE id = ?1", &[&id])?.pop())
}

/// Episodes by id. The ids travel as one JSON list, so a long history does
/// not exceed SQLite's limit on bound parameters.
pub fn list_episodes_by_ids(conn: &Connection, ids: &[i64]) -> anyhow::Result<Vec<Episode>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let json = serde_json::to_string(ids)?;
    query_episodes(
        conn,
        "SELECT * FROM episodes WHERE id IN (SELECT value FROM json_each(?1)) ORDER BY id",
        &[&json],
    )
}

pub fn list_episodes_by_file(conn: &Connection, file_id: i64) -> anyhow::Result<Vec<Episode>> {
    query_episodes(
        conn,
        "SELECT * FROM episodes WHERE episode_file_id = ?1 ORDER BY season_number, episode_number",
        &[&file_id],
    )
}

/// Episodes airing in `[start, end]`. Without `include_unmonitored` only
/// monitored episodes of monitored series qualify. The dates are stored in
/// one spelling, so the text comparison is the time order, once the bounds
/// are within the years that spelling covers.
pub fn episodes_between(
    conn: &Connection,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    include_unmonitored: bool,
) -> anyhow::Result<Vec<Episode>> {
    let (start, end) = (time::sql_bound(start), time::sql_bound(end));
    if include_unmonitored {
        query_episodes(
            conn,
            "SELECT * FROM episodes WHERE air_date_utc >= ?1 AND air_date_utc <= ?2 ORDER BY air_date_utc, id",
            &[&start, &end],
        )
    } else {
        query_episodes(
            conn,
            "SELECT e.* FROM episodes e JOIN series s ON s.id = e.series_id
             WHERE e.air_date_utc >= ?1 AND e.air_date_utc <= ?2 AND e.monitored = 1 AND s.monitored = 1
             ORDER BY e.air_date_utc, e.id",
            &[&start, &end],
        )
    }
}

/// SQL column for a wanted/calendar sort key.
fn episode_sort_column(key: &str) -> &'static str {
    match key {
        "episodes.lastSearchTime" => "e.last_search_time",
        "series.sortTitle" => "s.sort_title",
        _ => "e.air_date_utc",
    }
}

/// Monitored episodes (of monitored series) without a file whose air time plus
/// the series runtime has passed, specials included: `wanted/missing`. With
/// `monitored` false, the unmonitored ones instead.
pub fn missing_episodes(
    conn: &Connection,
    paging: &Paging,
    monitored: bool,
    now: DateTime<Utc>,
) -> anyhow::Result<(Vec<Episode>, i64)> {
    let now = time::format_utc(now);
    let monitored_clause = if monitored {
        "e.monitored = 1 AND s.monitored = 1"
    } else {
        "(e.monitored = 0 OR s.monitored = 0)"
    };
    let base = format!(
        "FROM episodes e JOIN series s ON s.id = e.series_id
         WHERE e.episode_file_id = 0
           AND datetime(e.air_date_utc, '+' || s.runtime || ' minutes') <= datetime(?1)
           AND {monitored_clause}"
    );
    let total: i64 = conn.query_row(&format!("SELECT count(*) {base}"), [&now], |r| r.get(0))?;
    let sql = format!(
        "SELECT e.* {base} ORDER BY {} {}, e.id LIMIT ?2 OFFSET ?3",
        episode_sort_column(&paging.sort_key),
        paging.direction_sql()
    );
    let records = query_episodes(conn, &sql, &[&now, &paging.page_size, &paging.offset()])?;
    Ok((records, total))
}

/// What the cutoff-unmet lists filter and sort on, read without the rest of
/// an episode (overview, images, ratings): the file's quality, the series'
/// profile and the sort keys. The caller checks the cutoff, which depends on
/// each profile's item order, and reads the episodes it keeps in full.
#[derive(Debug, Clone, PartialEq)]
pub struct CutoffRow {
    pub id: i64,
    pub series_id: i64,
    pub quality_id: i64,
    pub quality_profile_id: i64,
    pub air_date_utc: Option<DateTime<Utc>>,
    pub last_search_time: Option<DateTime<Utc>>,
    pub series_sort_title: Option<String>,
}

/// The episodes with a file, specials excluded, in id order: with
/// `monitored`, those whose episode and series are both monitored; without
/// it, every other one.
pub fn cutoff_rows(conn: &Connection, monitored: bool) -> anyhow::Result<Vec<CutoffRow>> {
    let mut stmt = conn.prepare(
        "SELECT e.id, e.series_id, f.quality_id, s.quality_profile_id, e.air_date_utc, e.last_search_time,
                s.sort_title
         FROM episodes e
         JOIN series s ON s.id = e.series_id
         JOIN episode_files f ON f.id = e.episode_file_id
         WHERE e.episode_file_id != 0 AND e.season_number >= 1
           AND (e.monitored = 1 AND s.monitored = 1) = ?1
         ORDER BY e.id",
    )?;
    let mut rows = stmt.query([monitored])?;
    let mut out = Vec::new();
    while let Some(r) = rows.next()? {
        out.push(CutoffRow {
            id: r.get(0)?,
            series_id: r.get(1)?,
            quality_id: r.get(2)?,
            quality_profile_id: r.get(3)?,
            air_date_utc: parse_ts(r.get(4)?)?,
            last_search_time: parse_ts(r.get(5)?)?,
            series_sort_title: r.get(6)?,
        });
    }
    Ok(out)
}

pub fn insert_file(conn: &Connection, f: &EpisodeFile) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO episode_files (id, series_id, season_number, relative_path, original_file_path, size,
            date_added, scene_name, release_group, release_hash, quality_id, revision_version, revision_real,
            revision_is_repack, languages, media_info, indexer_flags, release_type)
         VALUES (:id, :series_id, :season_number, :relative_path, :original_file_path, :size, :date_added,
            :scene_name, :release_group, :release_hash, :quality_id, :revision_version, :revision_real,
            :revision_is_repack, :languages, :media_info, :indexer_flags, :release_type)",
        named_params! {
            ":id": f.id, ":series_id": f.series_id, ":season_number": f.season_number,
            ":relative_path": f.relative_path, ":original_file_path": f.original_file_path, ":size": f.size,
            ":date_added": ts(Some(f.date_added)), ":scene_name": f.scene_name,
            ":release_group": f.release_group, ":release_hash": f.release_hash,
            ":quality_id": f.quality.quality_id, ":revision_version": f.quality.version,
            ":revision_real": f.quality.real, ":revision_is_repack": f.quality.is_repack,
            ":languages": to_json(&f.languages)?,
            ":media_info": f.media_info.as_ref().map(to_json).transpose()?,
            ":indexer_flags": f.indexer_flags, ":release_type": f.release_type.as_str(),
        },
    )?;
    Ok(())
}

fn file_from_row(row: &Row) -> anyhow::Result<EpisodeFile> {
    Ok(EpisodeFile {
        id: row.get("id")?,
        series_id: row.get("series_id")?,
        season_number: row.get("season_number")?,
        relative_path: row.get("relative_path")?,
        original_file_path: row.get("original_file_path")?,
        size: row.get("size")?,
        date_added: parse_ts_required(row.get("date_added")?)?,
        scene_name: row.get("scene_name")?,
        release_group: row.get("release_group")?,
        release_hash: row.get("release_hash")?,
        quality: quality_from_row(row)?,
        languages: from_json::<Vec<i64>>(row.get("languages")?)?,
        media_info: row
            .get::<_, Option<String>>("media_info")?
            .map(|s| serde_json::from_str(&s))
            .transpose()?,
        indexer_flags: row.get("indexer_flags")?,
        release_type: parse_enum!(ReleaseType, row.get("release_type")?)?,
    })
}

fn query_files(
    conn: &Connection,
    sql: &str,
    params: &[&dyn rusqlite::ToSql],
) -> anyhow::Result<Vec<EpisodeFile>> {
    let mut stmt = conn.prepare(sql)?;
    let mut rows = stmt.query(params)?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(file_from_row(row)?);
    }
    Ok(out)
}

pub fn list_files(conn: &Connection, series_id: i64) -> anyhow::Result<Vec<EpisodeFile>> {
    query_files(
        conn,
        "SELECT * FROM episode_files WHERE series_id = ?1 ORDER BY id",
        &[&series_id],
    )
}

pub fn list_files_by_ids(conn: &Connection, ids: &[i64]) -> anyhow::Result<Vec<EpisodeFile>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let json = serde_json::to_string(ids)?;
    query_files(
        conn,
        "SELECT * FROM episode_files WHERE id IN (SELECT value FROM json_each(?1)) ORDER BY series_id, id",
        &[&json],
    )
}

pub fn get_file(conn: &Connection, id: i64) -> anyhow::Result<Option<EpisodeFile>> {
    Ok(query_files(conn, "SELECT * FROM episode_files WHERE id = ?1", &[&id])?.pop())
}

/// `EpisodeRepository.Find(seriesId, season, episodeNumber)`.
pub fn find_episode(
    conn: &Connection,
    series_id: i64,
    season: i64,
    number: i64,
) -> anyhow::Result<Option<Episode>> {
    Ok(query_episodes(
        conn,
        "SELECT * FROM episodes WHERE series_id = ?1 AND season_number = ?2 AND episode_number = ?3 ORDER BY id",
        &[&series_id, &season, &number],
    )?
    .into_iter()
    .next())
}

/// `Find(seriesId, absoluteEpisodeNumber)`.
pub fn find_episode_by_absolute(
    conn: &Connection,
    series_id: i64,
    absolute: i64,
) -> anyhow::Result<Option<Episode>> {
    Ok(query_episodes(
        conn,
        "SELECT * FROM episodes WHERE series_id = ?1 AND absolute_episode_number = ?2 ORDER BY id",
        &[&series_id, &absolute],
    )?
    .into_iter()
    .next())
}

/// `FindEpisodesBySceneNumbering(seriesId, seasonNumber, episodeNumber)`.
pub fn find_episodes_by_scene_numbering(
    conn: &Connection,
    series_id: i64,
    scene_season: i64,
    scene_episode: i64,
) -> anyhow::Result<Vec<Episode>> {
    query_episodes(
        conn,
        "SELECT * FROM episodes WHERE series_id = ?1 AND scene_season_number = ?2 AND scene_episode_number = ?3 ORDER BY id",
        &[&series_id, &scene_season, &scene_episode],
    )
}

/// `FindEpisodesBySceneNumbering(seriesId, sceneAbsoluteEpisodeNumber)`.
pub fn find_episodes_by_scene_absolute(
    conn: &Connection,
    series_id: i64,
    scene_absolute: i64,
) -> anyhow::Result<Vec<Episode>> {
    query_episodes(
        conn,
        "SELECT * FROM episodes WHERE series_id = ?1 AND scene_absolute_episode_number = ?2 ORDER BY id",
        &[&series_id, &scene_absolute],
    )
}

/// `GetEpisodesBySceneSeason`.
pub fn list_episodes_by_scene_season(
    conn: &Connection,
    series_id: i64,
    scene_season: i64,
) -> anyhow::Result<Vec<Episode>> {
    query_episodes(
        conn,
        "SELECT * FROM episodes WHERE series_id = ?1 AND scene_season_number = ?2 ORDER BY id",
        &[&series_id, &scene_season],
    )
}

/// `Find(seriesId, date)`: the episodes airing on a `YYYY-MM-DD` date.
pub fn list_episodes_by_air_date(
    conn: &Connection,
    series_id: i64,
    air_date: &str,
) -> anyhow::Result<Vec<Episode>> {
    query_episodes(
        conn,
        "SELECT * FROM episodes WHERE series_id = ?1 AND air_date = ?2 ORDER BY id",
        &[&series_id, &air_date],
    )
}

/// `UpdateLastSearchTime`.
pub fn set_last_search_time(conn: &Connection, ids: &[i64], at: DateTime<Utc>) -> anyhow::Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let json = serde_json::to_string(ids)?;
    conn.execute(
        "UPDATE episodes SET last_search_time = ?1 WHERE id IN (SELECT value FROM json_each(?2))",
        params![ts(Some(at)), json],
    )?;
    Ok(())
}

/// `MediaFileRepository.GetFilesBySeason`.
pub fn list_files_by_season(
    conn: &Connection,
    series_id: i64,
    season: i64,
) -> anyhow::Result<Vec<EpisodeFile>> {
    query_files(
        conn,
        "SELECT * FROM episode_files WHERE series_id = ?1 AND season_number = ?2 ORDER BY id",
        &[&series_id, &season],
    )
}

/// `GetFilesWithRelativePath`: the rows already at a path, case-insensitively
/// as the originals compare paths.
pub fn files_with_relative_path(
    conn: &Connection,
    series_id: i64,
    relative_path: &str,
) -> anyhow::Result<Vec<EpisodeFile>> {
    query_files(
        conn,
        "SELECT * FROM episode_files WHERE series_id = ?1 AND lower(relative_path) = lower(?2) ORDER BY id",
        &[&series_id, &relative_path],
    )
}

/// `MediaFileRepository.Update`: every column but the id.
pub fn update_file(conn: &Connection, f: &EpisodeFile) -> anyhow::Result<()> {
    conn.execute(
        "UPDATE episode_files SET series_id = :series_id, season_number = :season_number,
            relative_path = :relative_path, original_file_path = :original_file_path, size = :size,
            date_added = :date_added, scene_name = :scene_name, release_group = :release_group,
            release_hash = :release_hash, quality_id = :quality_id, revision_version = :revision_version,
            revision_real = :revision_real, revision_is_repack = :revision_is_repack, languages = :languages,
            media_info = :media_info, indexer_flags = :indexer_flags, release_type = :release_type
         WHERE id = :id",
        named_params! {
            ":id": f.id, ":series_id": f.series_id, ":season_number": f.season_number,
            ":relative_path": f.relative_path, ":original_file_path": f.original_file_path, ":size": f.size,
            ":date_added": ts(Some(f.date_added)), ":scene_name": f.scene_name,
            ":release_group": f.release_group, ":release_hash": f.release_hash,
            ":quality_id": f.quality.quality_id, ":revision_version": f.quality.version,
            ":revision_real": f.quality.real, ":revision_is_repack": f.quality.is_repack,
            ":languages": to_json(&f.languages)?,
            ":media_info": f.media_info.as_ref().map(to_json).transpose()?,
            ":indexer_flags": f.indexer_flags, ":release_type": f.release_type.as_str(),
        },
    )?;
    Ok(())
}

/// `MediaFileRepository.Delete`: the row goes; the episodes that pointed at
/// it are detached by the caller, as the originals' event handler does.
pub fn delete_file(conn: &Connection, id: i64) -> anyhow::Result<()> {
    conn.execute("DELETE FROM episode_files WHERE id = ?1", params![id])?;
    Ok(())
}

/// `EpisodeRepository.SetFileId`.
pub fn set_episode_file_id(conn: &Connection, episode_id: i64, file_id: i64) -> anyhow::Result<()> {
    conn.execute(
        "UPDATE episodes SET episode_file_id = ?1 WHERE id = ?2",
        params![file_id, episode_id],
    )?;
    Ok(())
}

/// Detach every episode that points at `file_id`.
pub fn clear_episode_file_id(conn: &Connection, file_id: i64) -> anyhow::Result<()> {
    conn.execute(
        "UPDATE episodes SET episode_file_id = 0 WHERE episode_file_id = ?1",
        params![file_id],
    )?;
    Ok(())
}

/// `EpisodeRepository.SetMonitored`.
/// `SetEpisodeMonitoredBySeason`.
pub fn set_monitored_by_season(
    conn: &Connection,
    series_id: i64,
    season: i64,
    monitored: bool,
) -> anyhow::Result<()> {
    conn.execute(
        "UPDATE episodes SET monitored = ?3 WHERE series_id = ?1 AND season_number = ?2",
        params![series_id, season, monitored],
    )?;
    Ok(())
}

pub fn set_monitored(conn: &Connection, ids: &[i64], monitored: bool) -> anyhow::Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let json = serde_json::to_string(ids)?;
    conn.execute(
        "UPDATE episodes SET monitored = ?1 WHERE id IN (SELECT value FROM json_each(?2))",
        params![monitored, json],
    )?;
    Ok(())
}

/// Every file of the library with its series' path, for the checks that
/// look at the disk.
pub fn list_all_files(conn: &Connection) -> anyhow::Result<Vec<EpisodeFile>> {
    query_files(conn, "SELECT * FROM episode_files ORDER BY series_id, id", &[])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::model::QualityModel;
    use chrono::TimeZone;

    #[test]
    fn round_trips_episodes_and_files() {
        let db = Db::open_in_memory().unwrap();
        db.call_blocking(|conn| {
            // The series row and its tag are needed for the foreign keys.
            crate::store::catalog::insert_tag(
                conn,
                &crate::model::Tag {
                    id: 1,
                    label: "drama".into(),
                },
            )?;
            crate::store::catalog::insert_quality_profile(
                conn,
                &crate::model::QualityProfile {
                    id: 1,
                    name: "Any".into(),
                    cutoff: 1,
                    upgrade_allowed: false,
                    items: vec![],
                    format_items: vec![],
                    min_format_score: 0,
                    cutoff_format_score: 0,
                    min_upgrade_format_score: 1,
                },
            )?;
            crate::store::series::insert(conn, &crate::store::series::tests::sample(1))?;
            let file = EpisodeFile {
                id: 2,
                series_id: 1,
                season_number: 1,
                relative_path: "Season 1/Firefly - S01E01.mkv".into(),
                original_file_path: None,
                size: 407344,
                date_added: chrono::Utc.with_ymd_and_hms(2026, 11, 6, 23, 23, 20).unwrap(),
                scene_name: Some("Firefly.S01E01.1080p.BluRay.x264-DEMAND".into()),
                release_group: Some("DEMAND".into()),
                release_hash: None,
                quality: QualityModel {
                    quality_id: 7,
                    version: 1,
                    real: 0,
                    is_repack: false,
                },
                languages: vec![1],
                media_info: Some(serde_json::json!({"videoCodec": "x264"})),
                indexer_flags: 0,
                release_type: ReleaseType::SingleEpisode,
            };
            insert_file(conn, &file)?;
            let ep = Episode {
                id: 7,
                series_id: 1,
                tvdb_id: 297989,
                season_number: 1,
                episode_number: 1,
                title: Some("The Train Job".into()),
                overview: None,
                episode_file_id: 2,
                absolute_episode_number: Some(2),
                scene_absolute_episode_number: None,
                scene_season_number: None,
                scene_episode_number: None,
                unverified_scene_numbering: false,
                monitored: true,
                air_date: Some(chrono::NaiveDate::from_ymd_opt(2002, 9, 20).unwrap()),
                air_date_utc: Some(chrono::Utc.with_ymd_and_hms(2002, 9, 20, 4, 0, 0).unwrap()),
                runtime: 44,
                finale_type: None,
                images: vec![],
                ratings: Ratings::default(),
                last_search_time: None,
                aired_after_season_number: None,
                aired_before_season_number: None,
                aired_before_episode_number: None,
            };
            insert_episode(conn, &ep)?;
            assert_eq!(list_episodes(conn, 1, None)?, vec![ep.clone()]);
            assert_eq!(list_episodes(conn, 1, Some(2))?, vec![]);
            assert_eq!(get_episode(conn, 7)?, Some(ep));
            assert_eq!(list_files(conn, 1)?, vec![file.clone()]);
            assert_eq!(get_file(conn, 2)?, Some(file));
            // A range that ends past year 9999 (a huge `futureDays` on the
            // feed) still holds the episode.
            let since = chrono::Utc.with_ymd_and_hms(2002, 1, 1, 0, 0, 0).unwrap();
            let far = chrono::Utc.with_ymd_and_hms(12000, 1, 1, 0, 0, 0).unwrap();
            let ids = |v: Vec<Episode>| v.into_iter().map(|e| e.id).collect::<Vec<_>>();
            assert_eq!(ids(episodes_between(conn, since, far, true)?), vec![7]);
            assert_eq!(ids(episodes_between(conn, since, far, false)?), vec![7]);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn cutoff_rows_split_monitored_from_the_rest() {
        let db = Db::open_in_memory().unwrap();
        db.call_blocking(|conn| {
            crate::store::catalog::insert_tag(
                conn,
                &crate::model::Tag {
                    id: 1,
                    label: "drama".into(),
                },
            )?;
            crate::store::catalog::insert_quality_profile(
                conn,
                &crate::model::QualityProfile {
                    id: 1,
                    name: "Any".into(),
                    cutoff: 1,
                    upgrade_allowed: false,
                    items: vec![],
                    format_items: vec![],
                    min_format_score: 0,
                    cutoff_format_score: 0,
                    min_upgrade_format_score: 1,
                },
            )?;
            crate::store::series::insert(conn, &crate::store::series::tests::sample(1))?;
            let mut other = crate::store::series::tests::sample(2);
            other.monitored = false;
            other.title_slug = "serenity".into();
            other.path = "/data/tv/Serenity".into();
            other.sort_title = Some("serenity".into());
            crate::store::series::insert(conn, &other)?;
            for (id, series_id, quality_id) in [(10, 1, 4), (11, 1, 4), (12, 1, 7), (13, 2, 9)] {
                insert_file(
                    conn,
                    &EpisodeFile {
                        id,
                        series_id,
                        season_number: 1,
                        relative_path: format!("S01E{id}.mkv"),
                        original_file_path: None,
                        size: 1,
                        date_added: chrono::Utc.with_ymd_and_hms(2026, 11, 6, 0, 0, 0).unwrap(),
                        scene_name: None,
                        release_group: None,
                        release_hash: None,
                        quality: QualityModel {
                            quality_id,
                            version: 1,
                            real: 0,
                            is_repack: false,
                        },
                        languages: vec![1],
                        media_info: None,
                        indexer_flags: 0,
                        release_type: ReleaseType::SingleEpisode,
                    },
                )?;
            }
            let aired = chrono::Utc.with_ymd_and_hms(2002, 9, 20, 4, 0, 0).unwrap();
            // (id, series, season, file, monitored)
            for (id, series_id, season_number, episode_file_id, monitored) in [
                (1, 1, 1, 10, true),  // monitored, with a file
                (2, 1, 1, 0, true),   // no file
                (3, 1, 0, 11, true),  // a special
                (4, 1, 1, 12, false), // the episode unmonitored
                (5, 2, 1, 13, true),  // the series unmonitored
                (6, 1, 1, 99, true),  // a file that is gone
            ] {
                insert_episode(
                    conn,
                    &Episode {
                        id,
                        series_id,
                        tvdb_id: 100 + id,
                        season_number,
                        episode_number: id,
                        title: None,
                        overview: None,
                        episode_file_id,
                        absolute_episode_number: None,
                        scene_absolute_episode_number: None,
                        scene_season_number: None,
                        scene_episode_number: None,
                        unverified_scene_numbering: false,
                        monitored,
                        air_date: None,
                        air_date_utc: Some(aired),
                        runtime: 44,
                        finale_type: None,
                        images: vec![],
                        ratings: Ratings::default(),
                        last_search_time: None,
                        aired_after_season_number: None,
                        aired_before_season_number: None,
                        aired_before_episode_number: None,
                    },
                )?;
            }
            let row = |id, series_id, quality_id, sort_title: &str| CutoffRow {
                id,
                series_id,
                quality_id,
                quality_profile_id: 1,
                air_date_utc: Some(aired),
                last_search_time: None,
                series_sort_title: Some(sort_title.into()),
            };
            let firefly = crate::store::series::tests::sample(1).sort_title.unwrap();
            assert_eq!(cutoff_rows(conn, true)?, vec![row(1, 1, 4, &firefly)]);
            assert_eq!(
                cutoff_rows(conn, false)?,
                vec![row(4, 1, 7, &firefly), row(5, 2, 9, "serenity")]
            );
            Ok(())
        })
        .unwrap();
    }
}
