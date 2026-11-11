//! Persistence of the movie side. Plain functions over a `rusqlite`
//! connection, as `crate::store` is for the series side.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rusqlite::{named_params, params, Connection, Row};

use super::model::*;
use crate::model::Protocol;
use crate::store::paging::Paging;
use crate::store::{from_json, parse_enum, parse_ts, parse_ts_required, quality_from_row, to_json, ts};

// --- metadata and movies

pub fn insert_metadata(conn: &Connection, m: &MovieMetadata) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO movie_metadata (id, tmdb_id, imdb_id, title, sort_title, clean_title, original_title,
            clean_original_title, original_language_id, status, overview, images, genres, keywords, ratings,
            recommendations, runtime, in_cinemas, physical_release, digital_release, year, secondary_year,
            certification, youtube_trailer_id, studio, website, popularity, collection_tmdb_id,
            collection_title, last_info_sync)
         VALUES (:id, :tmdb_id, :imdb_id, :title, :sort_title, :clean_title, :original_title,
            :clean_original_title, :original_language_id, :status, :overview, :images, :genres, :keywords,
            :ratings, :recommendations, :runtime, :in_cinemas, :physical_release, :digital_release, :year,
            :secondary_year, :certification, :youtube_trailer_id, :studio, :website, :popularity,
            :collection_tmdb_id, :collection_title, :last_info_sync)",
        named_params! {
            ":id": m.id, ":tmdb_id": m.tmdb_id, ":imdb_id": m.imdb_id, ":title": m.title,
            ":sort_title": m.sort_title, ":clean_title": m.clean_title, ":original_title": m.original_title,
            ":clean_original_title": m.clean_original_title, ":original_language_id": m.original_language_id,
            ":status": m.status.as_str(), ":overview": m.overview, ":images": to_json(&m.images)?,
            ":genres": to_json(&m.genres)?, ":keywords": to_json(&m.keywords)?, ":ratings": to_json(&m.ratings)?,
            ":recommendations": to_json(&m.recommendations)?, ":runtime": m.runtime,
            ":in_cinemas": ts(m.in_cinemas), ":physical_release": ts(m.physical_release),
            ":digital_release": ts(m.digital_release), ":year": m.year, ":secondary_year": m.secondary_year,
            ":certification": m.certification, ":youtube_trailer_id": m.youtube_trailer_id,
            ":studio": m.studio, ":website": m.website, ":popularity": m.popularity,
            ":collection_tmdb_id": m.collection_tmdb_id, ":collection_title": m.collection_title,
            ":last_info_sync": ts(m.last_info_sync),
        },
    )?;
    Ok(())
}

/// The metadata columns as selected by [`METADATA_COLUMNS`], prefixed `md_`.
const METADATA_COLUMNS: &str = "md.id AS md_id, md.tmdb_id AS md_tmdb_id, md.imdb_id AS md_imdb_id,
    md.title AS md_title, md.sort_title AS md_sort_title, md.clean_title AS md_clean_title,
    md.original_title AS md_original_title, md.clean_original_title AS md_clean_original_title,
    md.original_language_id AS md_original_language_id, md.status AS md_status, md.overview AS md_overview,
    md.images AS md_images, md.genres AS md_genres, md.keywords AS md_keywords, md.ratings AS md_ratings,
    md.recommendations AS md_recommendations, md.runtime AS md_runtime, md.in_cinemas AS md_in_cinemas,
    md.physical_release AS md_physical_release, md.digital_release AS md_digital_release, md.year AS md_year,
    md.secondary_year AS md_secondary_year, md.certification AS md_certification,
    md.youtube_trailer_id AS md_youtube_trailer_id, md.studio AS md_studio, md.website AS md_website,
    md.popularity AS md_popularity, md.collection_tmdb_id AS md_collection_tmdb_id,
    md.collection_title AS md_collection_title, md.last_info_sync AS md_last_info_sync";

fn metadata_from_row(row: &Row) -> anyhow::Result<MovieMetadata> {
    Ok(MovieMetadata {
        id: row.get("md_id")?,
        tmdb_id: row.get("md_tmdb_id")?,
        imdb_id: row.get("md_imdb_id")?,
        title: row.get("md_title")?,
        sort_title: row.get("md_sort_title")?,
        clean_title: row.get("md_clean_title")?,
        original_title: row.get("md_original_title")?,
        clean_original_title: row.get("md_clean_original_title")?,
        original_language_id: row.get("md_original_language_id")?,
        status: parse_enum!(MovieStatus, row.get("md_status")?)?,
        overview: row.get("md_overview")?,
        images: from_json(row.get("md_images")?)?,
        genres: from_json(row.get("md_genres")?)?,
        keywords: from_json(row.get("md_keywords")?)?,
        ratings: from_json(row.get("md_ratings")?)?,
        recommendations: from_json(row.get("md_recommendations")?)?,
        runtime: row.get("md_runtime")?,
        in_cinemas: parse_ts(row.get("md_in_cinemas")?)?,
        physical_release: parse_ts(row.get("md_physical_release")?)?,
        digital_release: parse_ts(row.get("md_digital_release")?)?,
        year: row.get("md_year")?,
        secondary_year: row.get("md_secondary_year")?,
        certification: row.get("md_certification")?,
        youtube_trailer_id: row.get("md_youtube_trailer_id")?,
        studio: row.get("md_studio")?,
        website: row.get("md_website")?,
        popularity: row.get("md_popularity")?,
        collection_tmdb_id: row.get("md_collection_tmdb_id")?,
        collection_title: row.get("md_collection_title")?,
        last_info_sync: parse_ts(row.get("md_last_info_sync")?)?,
    })
}

/// Metadata rows by a filter over `md`, in id order.
fn query_metadata(
    conn: &Connection,
    filter: &str,
    params: &[&dyn rusqlite::ToSql],
) -> anyhow::Result<Vec<MovieMetadata>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {METADATA_COLUMNS} FROM movie_metadata md WHERE {filter} ORDER BY md.id"
    ))?;
    let mut rows = stmt.query(params)?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(metadata_from_row(row)?);
    }
    Ok(out)
}

/// The metadata of every movie a collection names, in the library or not.
pub fn metadata_by_collection(
    conn: &Connection,
    collection_tmdb_id: i64,
) -> anyhow::Result<Vec<MovieMetadata>> {
    query_metadata(conn, "md.collection_tmdb_id = ?1", &[&collection_tmdb_id])
}

/// The metadata rows of every collection at once.
pub fn metadata_in_collections(conn: &Connection) -> anyhow::Result<Vec<MovieMetadata>> {
    query_metadata(conn, "md.collection_tmdb_id > 0", &[])
}

pub fn insert_movie(conn: &Connection, m: &Movie) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO movies (id, movie_metadata_id, path, monitored, quality_profile_id, minimum_availability,
            movie_file_id, tags, added, add_options, last_search_time)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            m.id,
            m.metadata.id,
            m.path,
            m.monitored,
            m.quality_profile_id,
            m.minimum_availability.as_str(),
            m.movie_file_id,
            to_json(&m.tags)?,
            ts(m.added),
            m.add_options.as_ref().map(to_json).transpose()?,
            ts(m.last_search_time),
        ],
    )?;
    Ok(())
}

fn movie_from_row(row: &Row) -> anyhow::Result<Movie> {
    Ok(Movie {
        id: row.get("id")?,
        metadata: metadata_from_row(row)?,
        path: row.get("path")?,
        monitored: row.get("monitored")?,
        quality_profile_id: row.get("quality_profile_id")?,
        minimum_availability: parse_enum!(MovieStatus, row.get("minimum_availability")?)?,
        movie_file_id: row.get("movie_file_id")?,
        tags: from_json(row.get("tags")?)?,
        added: parse_ts(row.get("added")?)?,
        add_options: row
            .get::<_, Option<String>>("add_options")?
            .map(|s| serde_json::from_str(&s))
            .transpose()?,
        last_search_time: parse_ts(row.get("last_search_time")?)?,
    })
}

fn query_movies(
    conn: &Connection,
    filter: &str,
    params: &[&dyn rusqlite::ToSql],
) -> anyhow::Result<Vec<Movie>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT m.*, {METADATA_COLUMNS} FROM movies m JOIN movie_metadata md ON md.id = m.movie_metadata_id
         WHERE {filter} ORDER BY m.id"
    ))?;
    let mut rows = stmt.query(params)?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(movie_from_row(row)?);
    }
    Ok(out)
}

/// Every movie of the library, in id order.
pub fn list_movies(conn: &Connection) -> anyhow::Result<Vec<Movie>> {
    query_movies(conn, "1 = 1", &[])
}

pub fn get_movie(conn: &Connection, id: i64) -> anyhow::Result<Option<Movie>> {
    Ok(query_movies(conn, "m.id = ?1", &[&id])?.into_iter().next())
}

pub fn movies_by_tmdb_id(conn: &Connection, tmdb_id: i64) -> anyhow::Result<Vec<Movie>> {
    query_movies(conn, "md.tmdb_id = ?1", &[&tmdb_id])
}

/// The movies with these ids, as a JSON array of ids travels to SQLite.
pub fn movies_by_ids(conn: &Connection, ids: &[i64]) -> anyhow::Result<Vec<Movie>> {
    let ids = serde_json::to_string(ids)?;
    query_movies(conn, "m.id IN (SELECT value FROM json_each(?1))", &[&ids])
}

pub fn count_movies(conn: &Connection) -> anyhow::Result<i64> {
    Ok(conn.query_row("SELECT count(*) FROM movies", [], |r| r.get(0))?)
}

// --- movie files

pub fn insert_movie_file(conn: &Connection, f: &MovieFile) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO movie_files (id, movie_id, relative_path, original_file_path, size, date_added, scene_name,
            release_group, edition, quality_id, revision_version, revision_real, revision_is_repack, languages,
            media_info, indexer_flags)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        params![
            f.id,
            f.movie_id,
            f.relative_path,
            f.original_file_path,
            f.size,
            ts(Some(f.date_added)),
            f.scene_name,
            f.release_group,
            f.edition,
            f.quality.quality_id,
            f.quality.version,
            f.quality.real,
            f.quality.is_repack,
            to_json(&f.languages)?,
            f.media_info.as_ref().map(to_json).transpose()?,
            f.indexer_flags,
        ],
    )?;
    Ok(())
}

fn movie_file_from_row(row: &Row) -> anyhow::Result<MovieFile> {
    Ok(MovieFile {
        id: row.get("id")?,
        movie_id: row.get("movie_id")?,
        relative_path: row.get("relative_path")?,
        original_file_path: row.get("original_file_path")?,
        size: row.get("size")?,
        date_added: parse_ts_required(row.get("date_added")?)?,
        scene_name: row.get("scene_name")?,
        release_group: row.get("release_group")?,
        edition: row.get("edition")?,
        quality: quality_from_row(row)?,
        languages: from_json(row.get("languages")?)?,
        media_info: row
            .get::<_, Option<String>>("media_info")?
            .map(|s| serde_json::from_str(&s))
            .transpose()?,
        indexer_flags: row.get("indexer_flags")?,
    })
}

fn query_movie_files(
    conn: &Connection,
    filter: &str,
    params: &[&dyn rusqlite::ToSql],
) -> anyhow::Result<Vec<MovieFile>> {
    let mut stmt = conn.prepare(&format!("SELECT * FROM movie_files WHERE {filter} ORDER BY id"))?;
    let mut rows = stmt.query(params)?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(movie_file_from_row(row)?);
    }
    Ok(out)
}

pub fn list_movie_files(conn: &Connection) -> anyhow::Result<Vec<MovieFile>> {
    query_movie_files(conn, "1 = 1", &[])
}

pub fn get_movie_file(conn: &Connection, id: i64) -> anyhow::Result<Option<MovieFile>> {
    Ok(query_movie_files(conn, "id = ?1", &[&id])?.into_iter().next())
}

pub fn movie_files_by_ids(conn: &Connection, ids: &[i64]) -> anyhow::Result<Vec<MovieFile>> {
    let ids = serde_json::to_string(ids)?;
    query_movie_files(conn, "id IN (SELECT value FROM json_each(?1))", &[&ids])
}

pub fn movie_files_by_movies(conn: &Connection, movie_ids: &[i64]) -> anyhow::Result<Vec<MovieFile>> {
    let ids = serde_json::to_string(movie_ids)?;
    query_movie_files(conn, "movie_id IN (SELECT value FROM json_each(?1))", &[&ids])
}

// --- alternative titles, translations, credits

pub fn insert_alternative_title(conn: &Connection, t: &AlternativeTitle) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO alternative_titles (id, movie_metadata_id, title, clean_title, source_type)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![t.id, t.movie_metadata_id, t.title, t.clean_title, t.source_type],
    )?;
    Ok(())
}

fn query_alternative_titles(
    conn: &Connection,
    filter: &str,
    params: &[&dyn rusqlite::ToSql],
) -> anyhow::Result<Vec<AlternativeTitle>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT id, movie_metadata_id, title, clean_title, source_type FROM alternative_titles
         WHERE {filter} ORDER BY id"
    ))?;
    let rows = stmt.query_map(params, |r| {
        Ok(AlternativeTitle {
            id: r.get(0)?,
            movie_metadata_id: r.get(1)?,
            title: r.get(2)?,
            clean_title: r.get(3)?,
            source_type: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub fn list_alternative_titles(conn: &Connection) -> anyhow::Result<Vec<AlternativeTitle>> {
    query_alternative_titles(conn, "1 = 1", &[])
}

pub fn alternative_titles_of(
    conn: &Connection,
    movie_metadata_id: i64,
) -> anyhow::Result<Vec<AlternativeTitle>> {
    query_alternative_titles(conn, "movie_metadata_id = ?1", &[&movie_metadata_id])
}

pub fn get_alternative_title(conn: &Connection, id: i64) -> anyhow::Result<Option<AlternativeTitle>> {
    Ok(query_alternative_titles(conn, "id = ?1", &[&id])?
        .into_iter()
        .next())
}

pub fn insert_translation(conn: &Connection, t: &MovieTranslation) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO movie_translations (id, movie_metadata_id, language_id, title, clean_title, overview)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            t.id,
            t.movie_metadata_id,
            t.language_id,
            t.title,
            t.clean_title,
            t.overview
        ],
    )?;
    Ok(())
}

/// The translations into one language, by metadata id.
pub fn translations_in(
    conn: &Connection,
    language_id: i64,
) -> anyhow::Result<HashMap<i64, MovieTranslation>> {
    let mut stmt = conn.prepare(
        "SELECT id, movie_metadata_id, language_id, title, clean_title, overview FROM movie_translations
         WHERE language_id = ?1 ORDER BY id",
    )?;
    let rows = stmt.query_map([language_id], |r| {
        Ok(MovieTranslation {
            id: r.get(0)?,
            movie_metadata_id: r.get(1)?,
            language_id: r.get(2)?,
            title: r.get(3)?,
            clean_title: r.get(4)?,
            overview: r.get(5)?,
        })
    })?;
    let mut out = HashMap::new();
    for t in rows {
        let t = t?;
        out.entry(t.movie_metadata_id).or_insert(t);
    }
    Ok(out)
}

pub fn insert_credit(conn: &Connection, c: &Credit) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO credits (id, movie_metadata_id, credit_tmdb_id, person_tmdb_id, name, images, character,
            sort_order, job, department, type)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            c.id,
            c.movie_metadata_id,
            c.credit_tmdb_id,
            c.person_tmdb_id,
            c.name,
            to_json(&c.images)?,
            c.character,
            c.order,
            c.job,
            c.department,
            c.credit_type
        ],
    )?;
    Ok(())
}

fn query_credits(
    conn: &Connection,
    filter: &str,
    params: &[&dyn rusqlite::ToSql],
) -> anyhow::Result<Vec<Credit>> {
    let mut stmt = conn.prepare(&format!("SELECT * FROM credits WHERE {filter} ORDER BY id"))?;
    let mut rows = stmt.query(params)?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(Credit {
            id: row.get("id")?,
            movie_metadata_id: row.get("movie_metadata_id")?,
            credit_tmdb_id: row.get("credit_tmdb_id")?,
            person_tmdb_id: row.get("person_tmdb_id")?,
            name: row.get("name")?,
            images: from_json(row.get("images")?)?,
            character: row.get("character")?,
            order: row.get("sort_order")?,
            job: row.get("job")?,
            department: row.get("department")?,
            credit_type: row.get("type")?,
        });
    }
    Ok(out)
}

pub fn list_credits(conn: &Connection) -> anyhow::Result<Vec<Credit>> {
    query_credits(conn, "1 = 1", &[])
}

pub fn credits_of(conn: &Connection, movie_metadata_id: i64) -> anyhow::Result<Vec<Credit>> {
    query_credits(conn, "movie_metadata_id = ?1", &[&movie_metadata_id])
}

pub fn get_credit(conn: &Connection, id: i64) -> anyhow::Result<Option<Credit>> {
    Ok(query_credits(conn, "id = ?1", &[&id])?.into_iter().next())
}

// --- collections and exclusions

pub fn insert_collection(conn: &Connection, c: &Collection) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO collections (id, tmdb_id, title, sort_title, clean_title, overview, images, monitored,
            quality_profile_id, root_folder_path, minimum_availability, search_on_add, tags, added, last_info_sync)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            c.id,
            c.tmdb_id,
            c.title,
            c.sort_title,
            c.clean_title,
            c.overview,
            to_json(&c.images)?,
            c.monitored,
            c.quality_profile_id,
            c.root_folder_path,
            c.minimum_availability.as_str(),
            c.search_on_add,
            to_json(&c.tags)?,
            ts(c.added),
            ts(c.last_info_sync),
        ],
    )?;
    Ok(())
}

fn query_collections(
    conn: &Connection,
    filter: &str,
    params: &[&dyn rusqlite::ToSql],
) -> anyhow::Result<Vec<Collection>> {
    let mut stmt = conn.prepare(&format!("SELECT * FROM collections WHERE {filter} ORDER BY id"))?;
    let mut rows = stmt.query(params)?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(Collection {
            id: row.get("id")?,
            tmdb_id: row.get("tmdb_id")?,
            title: row.get("title")?,
            sort_title: row.get("sort_title")?,
            clean_title: row.get("clean_title")?,
            overview: row.get("overview")?,
            images: from_json(row.get("images")?)?,
            monitored: row.get("monitored")?,
            quality_profile_id: row.get("quality_profile_id")?,
            root_folder_path: row.get("root_folder_path")?,
            minimum_availability: parse_enum!(MovieStatus, row.get("minimum_availability")?)?,
            search_on_add: row.get("search_on_add")?,
            tags: from_json(row.get("tags")?)?,
            added: parse_ts(row.get("added")?)?,
            last_info_sync: parse_ts(row.get("last_info_sync")?)?,
        });
    }
    Ok(out)
}

pub fn list_collections(conn: &Connection) -> anyhow::Result<Vec<Collection>> {
    query_collections(conn, "1 = 1", &[])
}

pub fn get_collection(conn: &Connection, id: i64) -> anyhow::Result<Option<Collection>> {
    Ok(query_collections(conn, "id = ?1", &[&id])?.into_iter().next())
}

pub fn collections_by_tmdb_id(conn: &Connection, tmdb_id: i64) -> anyhow::Result<Vec<Collection>> {
    query_collections(conn, "tmdb_id = ?1", &[&tmdb_id])
}

pub fn insert_import_exclusion(conn: &Connection, e: &ImportExclusion) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO import_exclusions (id, tmdb_id, movie_title, movie_year) VALUES (?1, ?2, ?3, ?4)",
        params![e.id, e.tmdb_id, e.movie_title, e.movie_year],
    )?;
    Ok(())
}

pub fn list_import_exclusions(conn: &Connection) -> anyhow::Result<Vec<ImportExclusion>> {
    let mut stmt =
        conn.prepare("SELECT id, tmdb_id, movie_title, movie_year FROM import_exclusions ORDER BY id")?;
    let rows = stmt.query_map([], |r| {
        Ok(ImportExclusion {
            id: r.get(0)?,
            tmdb_id: r.get(1)?,
            movie_title: r.get(2)?,
            movie_year: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub fn insert_import_list_movie(
    conn: &Connection,
    id: i64,
    import_list_id: i64,
    movie_metadata_id: i64,
) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO import_list_movies (id, import_list_id, movie_metadata_id) VALUES (?1, ?2, ?3)",
        params![id, import_list_id, movie_metadata_id],
    )?;
    Ok(())
}

// --- history and blocklist

pub fn insert_history(conn: &Connection, h: &MovieHistoryEntry) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO history (id, movie_id, source_title, date, event_type, quality_id, revision_version,
            revision_real, revision_is_repack, languages, data, download_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            h.id,
            h.movie_id,
            h.source_title,
            ts(Some(h.date)),
            h.event_type.as_str(),
            h.quality.quality_id,
            h.quality.version,
            h.quality.real,
            h.quality.is_repack,
            to_json(&h.languages)?,
            to_json(&h.data)?,
            h.download_id,
        ],
    )?;
    Ok(())
}

fn history_from_row(row: &Row) -> anyhow::Result<MovieHistoryEntry> {
    Ok(MovieHistoryEntry {
        id: row.get("id")?,
        movie_id: row.get("movie_id")?,
        source_title: row.get("source_title")?,
        date: parse_ts_required(row.get("date")?)?,
        event_type: parse_enum!(MovieHistoryEvent, row.get("event_type")?)?,
        quality: quality_from_row(row)?,
        languages: from_json(row.get("languages")?)?,
        data: from_json(row.get("data")?)?,
        download_id: row.get("download_id")?,
    })
}

fn query_history(
    conn: &Connection,
    sql: &str,
    params: &[&dyn rusqlite::ToSql],
) -> anyhow::Result<Vec<MovieHistoryEntry>> {
    let mut stmt = conn.prepare(sql)?;
    let mut rows = stmt.query(params)?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(history_from_row(row)?);
    }
    Ok(out)
}

/// Filters of the paged `history` endpoint.
#[derive(Debug, Default, Clone)]
pub struct HistoryFilter {
    pub event_types: Vec<MovieHistoryEvent>,
    pub download_id: Option<String>,
    pub movie_ids: Vec<i64>,
    pub languages: Vec<i64>,
    pub qualities: Vec<i64>,
}

fn history_filter_sql(f: &HistoryFilter) -> anyhow::Result<(String, Vec<String>)> {
    let mut clauses = vec!["1 = 1".to_string()];
    let mut params = Vec::new();
    if !f.event_types.is_empty() {
        let names: Vec<&str> = f.event_types.iter().map(|e| e.as_str()).collect();
        params.push(serde_json::to_string(&names)?);
        clauses.push(format!(
            "h.event_type IN (SELECT value FROM json_each(?{}))",
            params.len()
        ));
    }
    if let Some(id) = &f.download_id {
        params.push(id.clone());
        clauses.push(format!("h.download_id = ?{}", params.len()));
    }
    if !f.movie_ids.is_empty() {
        params.push(serde_json::to_string(&f.movie_ids)?);
        clauses.push(format!(
            "h.movie_id IN (SELECT value FROM json_each(?{}))",
            params.len()
        ));
    }
    if !f.qualities.is_empty() {
        params.push(serde_json::to_string(&f.qualities)?);
        clauses.push(format!(
            "h.quality_id IN (SELECT value FROM json_each(?{}))",
            params.len()
        ));
    }
    if !f.languages.is_empty() {
        params.push(serde_json::to_string(&f.languages)?);
        clauses.push(format!(
            "EXISTS (SELECT 1 FROM json_each(h.languages) l WHERE l.value IN (SELECT value FROM json_each(?{})))",
            params.len()
        ));
    }
    Ok((clauses.join(" AND "), params))
}

/// The sort keys of the paged history, as the originals' paging names them.
pub const HISTORY_SORT_KEYS: &[&str] = &[
    "date",
    "movieMetadata.sortTitle",
    "movies.sortTitle",
    "quality",
    "languages",
    "eventType",
    "sourceTitle",
    "id",
];

fn history_order(paging: &Paging) -> String {
    let dir = paging.direction_sql();
    // Ties keep insertion order, whatever the direction; by date the id
    // follows the direction, since rows written in one second are told
    // apart by the fraction this schema does not keep.
    match paging.sort_key.as_str() {
        "movieMetadata.sortTitle" | "movies.sortTitle" => format!("md.sort_title {dir}, h.id ASC"),
        "quality" => format!("h.quality_id {dir}, h.id ASC"),
        "languages" => format!("h.languages {dir}, h.id ASC"),
        "eventType" => format!("h.event_type {dir}, h.id ASC"),
        "sourceTitle" => format!("h.source_title {dir}, h.id ASC"),
        "id" => format!("h.id {dir}"),
        _ => format!("h.date {dir}, h.id {dir}"),
    }
}

/// One page of history and the total the filter matches. Rows of a movie
/// that is gone are left out, as the originals' join leaves them.
pub fn history_page(
    conn: &Connection,
    paging: &Paging,
    filter: &HistoryFilter,
) -> anyhow::Result<(Vec<MovieHistoryEntry>, i64)> {
    let (clauses, params) = history_filter_sql(filter)?;
    let from = "FROM history h JOIN movies m ON m.id = h.movie_id
                JOIN movie_metadata md ON md.id = m.movie_metadata_id";
    let refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p as &dyn rusqlite::ToSql).collect();
    let total: i64 = conn.query_row(
        &format!("SELECT count(*) {from} WHERE {clauses}"),
        refs.as_slice(),
        |r| r.get(0),
    )?;
    let rows = query_history(
        conn,
        &format!(
            "SELECT h.* {from} WHERE {clauses} ORDER BY {} LIMIT {} OFFSET {}",
            history_order(paging),
            paging.page_size,
            paging.offset()
        ),
        refs.as_slice(),
    )?;
    Ok((rows, total))
}

/// `GetByMovieId`: a movie's history, newest first.
pub fn history_of_movie(
    conn: &Connection,
    movie_id: i64,
    event_type: Option<MovieHistoryEvent>,
) -> anyhow::Result<Vec<MovieHistoryEntry>> {
    match event_type {
        Some(event) => query_history(
            conn,
            "SELECT h.* FROM history h WHERE h.movie_id = ?1 AND h.event_type = ?2 ORDER BY h.date DESC, h.id DESC",
            &[&movie_id, &event.as_str()],
        ),
        None => query_history(
            conn,
            "SELECT h.* FROM history h WHERE h.movie_id = ?1 ORDER BY h.date DESC, h.id DESC",
            &[&movie_id],
        ),
    }
}

/// `Since`: what happened from `date` on, oldest first.
pub fn history_since(
    conn: &Connection,
    date: DateTime<Utc>,
    event_type: Option<MovieHistoryEvent>,
) -> anyhow::Result<Vec<MovieHistoryEntry>> {
    let date = ts(Some(date));
    match event_type {
        Some(event) => query_history(
            conn,
            "SELECT h.* FROM history h WHERE h.date >= ?1 AND h.event_type = ?2 ORDER BY h.date ASC, h.id ASC",
            &[&date, &event.as_str()],
        ),
        None => query_history(
            conn,
            "SELECT h.* FROM history h WHERE h.date >= ?1 ORDER BY h.date ASC, h.id ASC",
            &[&date],
        ),
    }
}

pub fn insert_blocklist(conn: &Connection, b: &MovieBlocklistEntry) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO blocklist (id, movie_id, source_title, quality_id, revision_version, revision_real,
            revision_is_repack, date, published_date, size, protocol, indexer, message, torrent_info_hash,
            languages, indexer_flags)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        params![
            b.id,
            b.movie_id,
            b.source_title,
            b.quality.quality_id,
            b.quality.version,
            b.quality.real,
            b.quality.is_repack,
            ts(Some(b.date)),
            ts(b.published_date),
            b.size,
            b.protocol.as_str(),
            b.indexer,
            b.message,
            b.torrent_info_hash,
            to_json(&b.languages)?,
            b.indexer_flags,
        ],
    )?;
    Ok(())
}

fn blocklist_from_row(row: &Row) -> anyhow::Result<MovieBlocklistEntry> {
    Ok(MovieBlocklistEntry {
        id: row.get("id")?,
        movie_id: row.get("movie_id")?,
        source_title: row.get("source_title")?,
        quality: quality_from_row(row)?,
        date: parse_ts_required(row.get("date")?)?,
        published_date: parse_ts(row.get("published_date")?)?,
        size: row.get("size")?,
        protocol: parse_enum!(Protocol, row.get("protocol")?)?,
        indexer: row.get("indexer")?,
        message: row.get("message")?,
        torrent_info_hash: row.get("torrent_info_hash")?,
        languages: from_json(row.get("languages")?)?,
        indexer_flags: row.get("indexer_flags")?,
    })
}

pub const BLOCKLIST_SORT_KEYS: &[&str] = &[
    "date",
    "movieMetadata.sortTitle",
    "movies.sortTitle",
    "sourceTitle",
    "quality",
    "languages",
    "indexer",
    "id",
];

/// One page of the blocklist and its total.
pub fn blocklist_page(conn: &Connection, paging: &Paging) -> anyhow::Result<(Vec<MovieBlocklistEntry>, i64)> {
    let from = "FROM blocklist b JOIN movies m ON m.id = b.movie_id
                JOIN movie_metadata md ON md.id = m.movie_metadata_id";
    let total: i64 = conn.query_row(&format!("SELECT count(*) {from}"), [], |r| r.get(0))?;
    let dir = paging.direction_sql();
    let order = match paging.sort_key.as_str() {
        "movieMetadata.sortTitle" | "movies.sortTitle" => format!("md.sort_title {dir}, b.id ASC"),
        "sourceTitle" => format!("b.source_title {dir}, b.id ASC"),
        "quality" => format!("b.quality_id {dir}, b.id ASC"),
        "languages" => format!("b.languages {dir}, b.id ASC"),
        "indexer" => format!("b.indexer {dir}, b.id ASC"),
        "id" => format!("b.id {dir}"),
        _ => format!("b.date {dir}, b.id {dir}"),
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT b.* {from} ORDER BY {order} LIMIT {} OFFSET {}",
        paging.page_size,
        paging.offset()
    ))?;
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(blocklist_from_row(row)?);
    }
    Ok((out, total))
}

/// `BlocklistedByMovie`, newest first.
pub fn blocklist_of_movie(conn: &Connection, movie_id: i64) -> anyhow::Result<Vec<MovieBlocklistEntry>> {
    let mut stmt = conn.prepare("SELECT * FROM blocklist WHERE movie_id = ?1 ORDER BY date DESC, id DESC")?;
    let mut rows = stmt.query([movie_id])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(blocklist_from_row(row)?);
    }
    Ok(out)
}

pub fn insert_download_history(conn: &Connection, h: &MovieDownloadHistoryEntry) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO download_history (id, event_type, movie_id, download_id, source_title, date, protocol,
            indexer_id, download_client_id, release, data)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            h.id,
            h.event_type.to_string(),
            h.movie_id,
            h.download_id,
            h.source_title,
            ts(Some(h.date)),
            h.protocol.as_str(),
            h.indexer_id,
            h.download_client_id,
            h.release.as_ref().map(to_json).transpose()?,
            to_json(&h.data)?,
        ],
    )?;
    Ok(())
}

// --- extra files

pub fn insert_extra_file(conn: &Connection, e: &MovieExtraFile) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO extra_files (kind, source_id, movie_id, movie_file_id, relative_path, extension, added,
            last_updated, language, language_tags, title, copy, consumer, metadata_type, hash)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            e.kind.as_str(),
            e.id,
            e.movie_id,
            e.movie_file_id,
            e.relative_path,
            e.extension,
            ts(e.added),
            ts(e.last_updated),
            e.language,
            to_json(&e.language_tags)?,
            e.title,
            e.copy,
            e.consumer,
            e.metadata_type,
            e.hash,
        ],
    )?;
    Ok(())
}

/// The extras of a movie: subtitles, then metadata files, then the others,
/// each in id order, as the originals concatenate their three tables.
pub fn extra_files_of(conn: &Connection, movie_id: i64) -> anyhow::Result<Vec<MovieExtraFile>> {
    let mut stmt = conn.prepare(
        "SELECT * FROM extra_files WHERE movie_id = ?1
         ORDER BY CASE kind WHEN 'subtitle' THEN 0 WHEN 'metadata' THEN 1 ELSE 2 END, source_id",
    )?;
    let mut rows = stmt.query([movie_id])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let kind: String = row.get("kind")?;
        out.push(MovieExtraFile {
            kind: ExtraKind::parse(&kind).ok_or_else(|| anyhow::anyhow!("bad extra file kind {kind:?}"))?,
            id: row.get("source_id")?,
            movie_id: row.get("movie_id")?,
            movie_file_id: row.get("movie_file_id")?,
            relative_path: row.get("relative_path")?,
            extension: row.get("extension")?,
            added: parse_ts(row.get("added")?)?,
            last_updated: parse_ts(row.get("last_updated")?)?,
            language: row.get("language")?,
            language_tags: from_json(row.get("language_tags")?)?,
            title: row.get("title")?,
            copy: row.get("copy")?,
            consumer: row.get("consumer")?,
            metadata_type: row.get("metadata_type")?,
            hash: row.get("hash")?,
        });
    }
    Ok(out)
}

// --- settings

pub fn set_naming(conn: &Connection, n: &MovieNamingConfig) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO naming_config (id, rename_movies, replace_illegal_characters, colon_replacement_format,
            standard_movie_format, movie_folder_format)
         VALUES (1, ?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (id) DO UPDATE SET rename_movies = excluded.rename_movies,
            replace_illegal_characters = excluded.replace_illegal_characters,
            colon_replacement_format = excluded.colon_replacement_format,
            standard_movie_format = excluded.standard_movie_format,
            movie_folder_format = excluded.movie_folder_format",
        params![
            n.rename_movies,
            n.replace_illegal_characters,
            n.colon_replacement_format,
            n.standard_movie_format,
            n.movie_folder_format
        ],
    )?;
    Ok(())
}

pub fn naming(conn: &Connection) -> anyhow::Result<MovieNamingConfig> {
    let found = conn
        .query_row(
            "SELECT rename_movies, replace_illegal_characters, colon_replacement_format, standard_movie_format,
                movie_folder_format FROM naming_config WHERE id = 1",
            [],
            |r| {
                Ok(MovieNamingConfig {
                    rename_movies: r.get(0)?,
                    replace_illegal_characters: r.get(1)?,
                    colon_replacement_format: r.get(2)?,
                    standard_movie_format: r.get(3)?,
                    movie_folder_format: r.get(4)?,
                })
            },
        )
        .ok();
    Ok(found.unwrap_or_default())
}

/// The language of each quality profile (`-1` any, `-2` original), which
/// Radarr's profiles carry next to what both applications share.
pub fn set_profile_language(conn: &Connection, profile_id: i64, language_id: i64) -> anyhow::Result<()> {
    conn.execute(
        "UPDATE quality_profiles SET language = ?2 WHERE id = ?1",
        params![profile_id, language_id],
    )?;
    Ok(())
}

pub fn profile_languages(conn: &Connection) -> anyhow::Result<HashMap<i64, i64>> {
    let mut stmt = conn.prepare("SELECT id, language FROM quality_profiles")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
    Ok(rows.collect::<Result<_, _>>()?)
}
