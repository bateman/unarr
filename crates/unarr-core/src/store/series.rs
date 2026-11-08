//! Series, their seasons and their tags.

use std::collections::HashMap;

use rusqlite::{named_params, params, Connection, Row};

use super::{from_json, parse_enum, parse_ts, parse_ts_required, to_json, ts};
use crate::model::{Image, NewItemMonitor, Ratings, Season, Series, SeriesStatus, SeriesType};

pub fn insert(conn: &Connection, s: &Series) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO series (id, tvdb_id, tv_rage_id, tv_maze_id, tmdb_id, imdb_id, title, title_slug,
            clean_title, sort_title, status, overview, air_time, network, certification, images, genres,
            actors, path, monitored, season_folder, monitor_new_items, use_scene_numbering, runtime,
            series_type, year, first_aired, last_aired, quality_profile_id, original_language_id,
            ratings_votes, ratings_value, added, add_options, last_info_sync, last_disk_sync, mal_ids,
            anilist_ids)
         VALUES (:id, :tvdb_id, :tv_rage_id, :tv_maze_id, :tmdb_id, :imdb_id, :title, :title_slug,
            :clean_title, :sort_title, :status, :overview, :air_time, :network, :certification, :images,
            :genres, :actors, :path, :monitored, :season_folder, :monitor_new_items, :use_scene_numbering,
            :runtime, :series_type, :year, :first_aired, :last_aired, :quality_profile_id,
            :original_language_id, :ratings_votes, :ratings_value, :added, :add_options, :last_info_sync,
            :last_disk_sync, :mal_ids, :anilist_ids)",
        named_params! {
            ":id": s.id, ":tvdb_id": s.tvdb_id, ":tv_rage_id": s.tv_rage_id, ":tv_maze_id": s.tv_maze_id,
            ":tmdb_id": s.tmdb_id, ":imdb_id": s.imdb_id, ":title": s.title, ":title_slug": s.title_slug,
            ":clean_title": s.clean_title, ":sort_title": s.sort_title, ":status": s.status.as_str(),
            ":overview": s.overview, ":air_time": s.air_time, ":network": s.network,
            ":certification": s.certification, ":images": to_json(&s.images)?, ":genres": to_json(&s.genres)?,
            ":actors": to_json(&s.actors)?, ":path": s.path, ":monitored": s.monitored,
            ":season_folder": s.season_folder, ":monitor_new_items": s.monitor_new_items.as_str(),
            ":use_scene_numbering": s.use_scene_numbering, ":runtime": s.runtime,
            ":series_type": s.series_type.as_str(), ":year": s.year, ":first_aired": ts(s.first_aired),
            ":last_aired": ts(s.last_aired), ":quality_profile_id": s.quality_profile_id,
            ":original_language_id": s.original_language_id, ":ratings_votes": s.ratings.votes,
            ":ratings_value": s.ratings.value, ":added": ts(Some(s.added)),
            ":add_options": s.add_options.as_ref().map(to_json).transpose()?,
            ":last_info_sync": ts(s.last_info_sync), ":last_disk_sync": ts(s.last_disk_sync),
            ":mal_ids": s.mal_ids.as_ref().map(to_json).transpose()?,
            ":anilist_ids": s.anilist_ids.as_ref().map(to_json).transpose()?,
        },
    )?;
    for season in &s.seasons {
        conn.execute(
            "INSERT INTO seasons (series_id, season_number, monitored, images) VALUES (?1, ?2, ?3, ?4)",
            params![
                s.id,
                season.season_number,
                season.monitored,
                to_json(&season.images)?
            ],
        )?;
    }
    for tag in &s.tags {
        conn.execute(
            "INSERT INTO series_tags (series_id, tag_id) VALUES (?1, ?2)",
            params![s.id, tag],
        )?;
    }
    Ok(())
}

/// A row decoded; the cast (`actors`, the largest JSON column and one no
/// resource carries) is parsed only when asked for.
fn from_row(row: &Row, with_actors: bool) -> anyhow::Result<Series> {
    Ok(Series {
        id: row.get("id")?,
        tvdb_id: row.get("tvdb_id")?,
        tv_rage_id: row.get("tv_rage_id")?,
        tv_maze_id: row.get("tv_maze_id")?,
        tmdb_id: row.get("tmdb_id")?,
        imdb_id: row.get("imdb_id")?,
        title: row.get("title")?,
        title_slug: row.get("title_slug")?,
        clean_title: row.get("clean_title")?,
        sort_title: row.get("sort_title")?,
        status: parse_enum!(SeriesStatus, row.get("status")?)?,
        overview: row.get("overview")?,
        air_time: row.get("air_time")?,
        network: row.get("network")?,
        certification: row.get("certification")?,
        images: from_json::<Vec<Image>>(row.get("images")?)?,
        genres: from_json::<Vec<String>>(row.get("genres")?)?,
        actors: if with_actors {
            from_json(row.get("actors")?)?
        } else {
            serde_json::Value::Array(Vec::new())
        },
        path: row.get("path")?,
        monitored: row.get("monitored")?,
        season_folder: row.get("season_folder")?,
        monitor_new_items: parse_enum!(NewItemMonitor, row.get("monitor_new_items")?)?,
        use_scene_numbering: row.get("use_scene_numbering")?,
        runtime: row.get("runtime")?,
        series_type: parse_enum!(SeriesType, row.get("series_type")?)?,
        year: row.get("year")?,
        first_aired: parse_ts(row.get("first_aired")?)?,
        last_aired: parse_ts(row.get("last_aired")?)?,
        quality_profile_id: row.get("quality_profile_id")?,
        original_language_id: row.get("original_language_id")?,
        ratings: Ratings {
            votes: row.get("ratings_votes")?,
            value: row.get("ratings_value")?,
        },
        added: parse_ts_required(row.get("added")?)?,
        add_options: row
            .get::<_, Option<String>>("add_options")?
            .map(|s| serde_json::from_str(&s))
            .transpose()?,
        last_info_sync: parse_ts(row.get("last_info_sync")?)?,
        last_disk_sync: parse_ts(row.get("last_disk_sync")?)?,
        mal_ids: row
            .get::<_, Option<String>>("mal_ids")?
            .map(|s| serde_json::from_str(&s))
            .transpose()?,
        anilist_ids: row
            .get::<_, Option<String>>("anilist_ids")?
            .map(|s| serde_json::from_str(&s))
            .transpose()?,
        seasons: Vec::new(),
        tags: Vec::new(),
    })
}

/// Attach seasons and tags to the given series, in two queries.
fn attach_children(conn: &Connection, series: &mut [Series]) -> anyhow::Result<()> {
    if series.is_empty() {
        return Ok(());
    }
    let mut by_id: HashMap<i64, usize> = series.iter().enumerate().map(|(i, s)| (s.id, i)).collect();
    // A JSON list of ids keeps the statement fixed whatever their number
    // (SQLite stops at 32,766 bound parameters).
    let ids = serde_json::to_string(&series.iter().map(|s| s.id).collect::<Vec<_>>())?;

    let mut stmt = conn.prepare(
        "SELECT series_id, season_number, monitored, images FROM seasons
         WHERE series_id IN (SELECT value FROM json_each(?1)) ORDER BY series_id, season_number",
    )?;
    let rows = stmt.query_map(params![ids], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, bool>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    })?;
    for row in rows {
        let (series_id, season_number, monitored, images) = row?;
        if let Some(&i) = by_id.get(&series_id) {
            series[i].seasons.push(Season {
                season_number,
                monitored,
                images: from_json(images)?,
            });
        }
    }

    let mut stmt = conn.prepare(
        "SELECT series_id, tag_id FROM series_tags
         WHERE series_id IN (SELECT value FROM json_each(?1)) ORDER BY tag_id",
    )?;
    let rows = stmt.query_map(params![ids], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
    })?;
    for row in rows {
        let (series_id, tag_id) = row?;
        if let Some(i) = by_id.get_mut(&series_id) {
            series[*i].tags.push(tag_id);
        }
    }
    Ok(())
}

/// Every series, by id, without the cast: `actors` is the largest JSON
/// column and nothing that lists series reads it. A series loaded here (or
/// by [`get_many`]) must not be written back whole, or its cast is lost.
pub fn list(conn: &Connection) -> anyhow::Result<Vec<Series>> {
    let mut stmt = conn.prepare("SELECT * FROM series ORDER BY id")?;
    let mut out = Vec::new();
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        out.push(from_row(row, false)?);
    }
    attach_children(conn, &mut out)?;
    Ok(out)
}

pub fn get(conn: &Connection, id: i64) -> anyhow::Result<Option<Series>> {
    let mut stmt = conn.prepare("SELECT * FROM series WHERE id = ?1")?;
    let mut rows = stmt.query(params![id])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let mut out = vec![from_row(row, true)?];
    drop(rows);
    attach_children(conn, &mut out)?;
    Ok(out.pop())
}

/// The series with the given ids, by id. Loading only the series a request
/// touches keeps episode requests from paying for the whole library.
pub fn get_many(conn: &Connection, ids: &[i64]) -> anyhow::Result<Vec<Series>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let json = serde_json::to_string(ids)?;
    let mut stmt =
        conn.prepare("SELECT * FROM series WHERE id IN (SELECT value FROM json_each(?1)) ORDER BY id")?;
    let mut out = Vec::new();
    let mut rows = stmt.query(params![json])?;
    while let Some(row) = rows.next()? {
        out.push(from_row(row, false)?);
    }
    drop(rows);
    attach_children(conn, &mut out)?;
    Ok(out)
}

pub fn count(conn: &Connection) -> anyhow::Result<i64> {
    Ok(conn.query_row("SELECT count(*) FROM series", [], |r| r.get(0))?)
}

fn query_series(
    conn: &Connection,
    sql: &str,
    params: &[&dyn rusqlite::ToSql],
) -> anyhow::Result<Vec<Series>> {
    let mut stmt = conn.prepare(sql)?;
    let mut rows = stmt.query(params)?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(from_row(row, false)?);
    }
    drop(rows);
    attach_children(conn, &mut out)?;
    Ok(out)
}

/// `SeriesRepository.FindByTitle`: every series with this clean title (the
/// caller decides what more than one means).
pub fn find_by_clean_title(conn: &Connection, clean_title: &str) -> anyhow::Result<Vec<Series>> {
    query_series(
        conn,
        "SELECT * FROM series WHERE clean_title = ?1 ORDER BY id",
        &[&clean_title.to_lowercase()],
    )
}

pub fn find_by_clean_title_and_year(
    conn: &Connection,
    clean_title: &str,
    year: i64,
) -> anyhow::Result<Vec<Series>> {
    query_series(
        conn,
        "SELECT * FROM series WHERE clean_title = ?1 AND year = ?2 ORDER BY id",
        &[&clean_title.to_lowercase(), &year],
    )
}

/// `FindByTitleInexact`: series whose clean title is contained in the given one.
pub fn find_by_clean_title_inexact(conn: &Connection, clean_title: &str) -> anyhow::Result<Vec<Series>> {
    query_series(
        conn,
        "SELECT * FROM series WHERE instr(?1, clean_title) > 0 ORDER BY id",
        &[&clean_title],
    )
}

pub fn find_by_tvdb_id(conn: &Connection, tvdb_id: i64) -> anyhow::Result<Option<Series>> {
    Ok(query_series(
        conn,
        "SELECT * FROM series WHERE tvdb_id = ?1 ORDER BY id",
        &[&tvdb_id],
    )?
    .into_iter()
    .next())
}

pub fn find_by_tv_rage_id(conn: &Connection, tv_rage_id: i64) -> anyhow::Result<Option<Series>> {
    Ok(query_series(
        conn,
        "SELECT * FROM series WHERE tv_rage_id = ?1 ORDER BY id",
        &[&tv_rage_id],
    )?
    .into_iter()
    .next())
}

pub fn find_by_imdb_id(conn: &Connection, imdb_id: &str) -> anyhow::Result<Option<Series>> {
    Ok(query_series(
        conn,
        "SELECT * FROM series WHERE imdb_id = ?1 ORDER BY id",
        &[&imdb_id],
    )?
    .into_iter()
    .next())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::db::Db;
    use crate::model::{ProfileItem, QualityProfile, Tag};
    use crate::store::catalog;
    use chrono::TimeZone;

    pub(crate) fn sample(id: i64) -> Series {
        Series {
            id,
            tvdb_id: 1000 + id,
            tv_rage_id: 0,
            tv_maze_id: 0,
            tmdb_id: 0,
            imdb_id: Some("tt0303461".into()),
            title: "Firefly".into(),
            title_slug: "firefly".into(),
            clean_title: "firefly".into(),
            sort_title: Some("firefly".into()),
            status: SeriesStatus::Ended,
            overview: Some("Space western.".into()),
            air_time: None,
            network: Some("FOX".into()),
            certification: Some("TV-14".into()),
            images: vec![Image {
                cover_type: "poster".into(),
                remote_url: Some("https://example.invalid/p.jpg".into()),
                url: None,
            }],
            genres: vec!["Drama".into(), "Science Fiction".into()],
            actors: serde_json::json!([]),
            path: "/data/tv/Firefly".into(),
            monitored: true,
            season_folder: true,
            monitor_new_items: NewItemMonitor::All,
            use_scene_numbering: false,
            runtime: 48,
            series_type: SeriesType::Standard,
            year: 2002,
            first_aired: Some(chrono::Utc.with_ymd_and_hms(2002, 9, 20, 0, 0, 0).unwrap()),
            last_aired: None,
            quality_profile_id: 1,
            original_language_id: 1,
            ratings: Ratings {
                votes: 10,
                value: 8.9,
            },
            added: chrono::Utc.with_ymd_and_hms(2026, 11, 6, 23, 23, 16).unwrap(),
            add_options: None,
            last_info_sync: None,
            last_disk_sync: None,
            mal_ids: None,
            anilist_ids: None,
            seasons: vec![
                Season {
                    season_number: 0,
                    monitored: false,
                    images: vec![],
                },
                Season {
                    season_number: 1,
                    monitored: true,
                    images: vec![],
                },
            ],
            tags: vec![1],
        }
    }

    #[test]
    fn round_trips_with_seasons_and_tags() {
        let db = Db::open_in_memory().unwrap();
        db.call_blocking(|conn| {
            catalog::insert_tag(
                conn,
                &Tag {
                    id: 1,
                    label: "drama".into(),
                },
            )?;
            catalog::insert_quality_profile(
                conn,
                &QualityProfile {
                    id: 1,
                    name: "Any".into(),
                    cutoff: 1,
                    upgrade_allowed: false,
                    items: vec![ProfileItem {
                        id: None,
                        name: None,
                        quality: Some(1),
                        items: vec![],
                        allowed: true,
                    }],
                    format_items: vec![],
                    min_format_score: 0,
                    cutoff_format_score: 0,
                    min_upgrade_format_score: 1,
                },
            )?;
            insert(conn, &sample(1))?;
            insert(conn, &sample(2))?;
            let all = list(conn)?;
            assert_eq!(all.len(), 2);
            assert_eq!(all[0], sample(1));
            assert_eq!(get(conn, 2)?, Some(sample(2)));
            assert_eq!(get(conn, 3)?, None);
            assert_eq!(count(conn)?, 2);
            Ok(())
        })
        .unwrap();
    }
}
