//! Radarr's activity and agenda: history, blocklist, the calendar and the
//! wanted lists.

use std::collections::HashMap;

use axum::extract::State;
use axum::http::Uri;
use axum::Json;
use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};
use unarr_core::movies::formats as movie_formats;
use unarr_core::movies::model::{
    AlternativeTitle, Movie, MovieBlocklistEntry, MovieFile, MovieHistoryEntry, MovieHistoryEvent,
};
use unarr_core::movies::store::{self as movies, HistoryFilter, BLOCKLIST_SORT_KEYS, HISTORY_SORT_KEYS};
use unarr_core::store::paging::Paging;
use unarr_core::time;

use super::resources::{
    embedded_movie_resource, formats_resource, languages_resource, movie_resource, quality_resource, Covers,
    FileShown, MovieContext, MovieParts,
};
use crate::error::ApiResult;
use crate::query::Params;
use crate::resources::PagingResource;
use crate::state::AppState;

// --- history

/// `HistoryResource`: the row, the custom formats its release would match
/// today and their score in the movie's profile, the movie when asked for.
fn history_resource(
    ctx: &MovieContext,
    h: &MovieHistoryEntry,
    movie: Option<&Movie>,
    include_movie: bool,
) -> Value {
    let mut out = json!({
        "id": h.id,
        "movieId": h.movie_id,
        "sourceTitle": h.source_title,
        "languages": languages_resource(&h.languages),
        "quality": quality_resource(&h.quality),
        "date": time::format_utc(h.date),
        "eventType": h.event_type.as_str(),
        "data": h.data,
        "customFormats": [],
        "customFormatScore": 0,
        "qualityCutoffNotMet": false,
    });
    if let Some(id) = &h.download_id {
        out["downloadId"] = json!(id);
    }
    if let Some(movie) = movie {
        let matched = movie_formats::history_formats(&ctx.formats, h, movie);
        let ids: Vec<i64> = matched.iter().map(|f| f.id).collect();
        out["customFormats"] = json!(formats_resource(&matched));
        if let Some(profile) = ctx.profile(movie.quality_profile_id) {
            out["customFormatScore"] = json!(unarr_core::profile::custom_format_score(profile, &ids));
            out["qualityCutoffNotMet"] = json!(unarr_core::profile::quality_cutoff_not_met(
                profile,
                h.quality.quality_id
            ));
        }
        if include_movie {
            out["movie"] = embedded_movie_resource(ctx, movie);
        }
    }
    out
}

async fn movies_of(state: &AppState, ids: Vec<i64>) -> anyhow::Result<HashMap<i64, Movie>> {
    Ok(state
        .db()
        .call(move |c| movies::movies_by_ids(c, &ids))
        .await?
        .into_iter()
        .map(|m| (m.id, m))
        .collect())
}

async fn history_resources(
    state: &AppState,
    rows: Vec<MovieHistoryEntry>,
    include_movie: bool,
) -> anyhow::Result<Vec<Value>> {
    let ctx = MovieContext::load(state).await?;
    let mut ids: Vec<i64> = rows.iter().map(|h| h.movie_id).collect();
    ids.sort_unstable();
    ids.dedup();
    let owners = movies_of(state, ids).await?;
    // Every row is matched against every custom format: off the async workers.
    crate::blocking::run(move || {
        rows.iter()
            .map(|h| history_resource(&ctx, h, owners.get(&h.movie_id), include_movie))
            .collect()
    })
    .await
}

/// The event types a request names, by number as the UI sends them.
fn event_types(p: &Params) -> Vec<MovieHistoryEvent> {
    p.ints("eventType")
        .into_iter()
        .filter_map(MovieHistoryEvent::from_number)
        .collect()
}

/// `GET /history`: one page, newest first unless asked otherwise.
pub async fn history(State(state): State<AppState>, uri: Uri) -> ApiResult<Json<PagingResource<Value>>> {
    let p = Params::from_uri(&uri);
    let paging = Paging::resolve(
        p.i64("page"),
        p.i64("pageSize"),
        p.get("sortKey"),
        p.descending(),
        HISTORY_SORT_KEYS,
        "date",
    );
    let filter = HistoryFilter {
        event_types: event_types(&p),
        download_id: p.get("downloadId").map(str::to_string),
        movie_ids: p.ints("movieIds"),
        languages: p.ints("languages"),
        qualities: p.ints("quality"),
    };
    let page = paging.clone();
    let (rows, total) = state
        .db()
        .call(move |c| movies::history_page(c, &page, &filter))
        .await?;
    let records = history_resources(&state, rows, p.bool("includeMovie", false)).await?;
    Ok(Json(PagingResource::new(&paging, total, records)))
}

/// `GET /history/since?date=`: what happened from a date on.
pub async fn history_since(State(state): State<AppState>, uri: Uri) -> ApiResult<Json<Vec<Value>>> {
    let p = Params::from_uri(&uri);
    let date = p.time("date").unwrap_or(DateTime::<Utc>::MIN_UTC);
    let event = event_types(&p).into_iter().next();
    let rows = state
        .db()
        .call(move |c| movies::history_since(c, date, event))
        .await?;
    Ok(Json(
        history_resources(&state, rows, p.bool("includeMovie", false)).await?,
    ))
}

/// `GET /history/movie?movieId=`: a movie's history, newest first.
pub async fn history_movie(State(state): State<AppState>, uri: Uri) -> ApiResult<Json<Vec<Value>>> {
    let p = Params::from_uri(&uri);
    let movie_id = p.i64("movieId").unwrap_or(0);
    let event = event_types(&p).into_iter().next();
    let rows = state
        .db()
        .call(move |c| movies::history_of_movie(c, movie_id, event))
        .await?;
    Ok(Json(
        history_resources(&state, rows, p.bool("includeMovie", false)).await?,
    ))
}

// --- blocklist

fn blocklist_resource(ctx: &MovieContext, b: &MovieBlocklistEntry, movie: Option<&Movie>) -> Value {
    let mut out = json!({
        "id": b.id,
        "movieId": b.movie_id,
        "sourceTitle": b.source_title,
        "languages": languages_resource(&b.languages),
        "quality": quality_resource(&b.quality),
        "customFormats": [],
        "date": time::format_utc(b.date),
        "protocol": b.protocol.as_str(),
    });
    for (key, value) in [("indexer", &b.indexer), ("message", &b.message)] {
        if let Some(v) = value {
            out[key] = json!(v);
        }
    }
    if let Some(movie) = movie {
        out["customFormats"] = json!(formats_resource(&movie_formats::blocklist_formats(
            &ctx.formats,
            b,
            movie
        )));
        out["movie"] = embedded_movie_resource(ctx, movie);
    }
    out
}

async fn blocklist_resources(state: &AppState, rows: Vec<MovieBlocklistEntry>) -> anyhow::Result<Vec<Value>> {
    let ctx = MovieContext::load(state).await?;
    let mut ids: Vec<i64> = rows.iter().map(|b| b.movie_id).collect();
    ids.sort_unstable();
    ids.dedup();
    let owners = movies_of(state, ids).await?;
    crate::blocking::run(move || {
        rows.iter()
            .map(|b| blocklist_resource(&ctx, b, owners.get(&b.movie_id)))
            .collect()
    })
    .await
}

pub async fn blocklist(State(state): State<AppState>, uri: Uri) -> ApiResult<Json<PagingResource<Value>>> {
    let p = Params::from_uri(&uri);
    let paging = Paging::resolve(
        p.i64("page"),
        p.i64("pageSize"),
        p.get("sortKey"),
        p.descending(),
        BLOCKLIST_SORT_KEYS,
        "date",
    );
    let page = paging.clone();
    let (rows, total) = state.db().call(move |c| movies::blocklist_page(c, &page)).await?;
    let records = blocklist_resources(&state, rows).await?;
    Ok(Json(PagingResource::new(&paging, total, records)))
}

/// `GET /blocklist/movie?movieId=`.
pub async fn blocklist_movie(State(state): State<AppState>, uri: Uri) -> ApiResult<Json<Vec<Value>>> {
    let movie_id = Params::from_uri(&uri).i64("movieId").unwrap_or(0);
    let rows = state
        .db()
        .call(move |c| movies::blocklist_of_movie(c, movie_id))
        .await?;
    Ok(Json(blocklist_resources(&state, rows).await?))
}

// --- calendar and wanted

/// The library with each movie's file and alternative titles.
struct Shelf {
    movies: Vec<Movie>,
    files: HashMap<i64, MovieFile>,
    titles: HashMap<i64, Vec<AlternativeTitle>>,
}

async fn load_shelf(state: &AppState) -> anyhow::Result<Shelf> {
    state
        .db()
        .call(|conn| {
            let mut titles: HashMap<i64, Vec<AlternativeTitle>> = HashMap::new();
            for t in movies::list_alternative_titles(conn)? {
                titles.entry(t.movie_metadata_id).or_default().push(t);
            }
            Ok(Shelf {
                movies: movies::list_movies(conn)?,
                files: movies::list_movie_files(conn)?
                    .into_iter()
                    .map(|f| (f.id, f))
                    .collect(),
                titles,
            })
        })
        .await
}

/// `GET /calendar?start=&end=`: the movies with a cinema, digital or
/// physical release in the range (today and the two days after it when
/// none is given), by cinema date; `unmonitored` adds those not monitored,
/// `tags` keeps those carrying one of them.
pub async fn calendar(State(state): State<AppState>, uri: Uri) -> ApiResult<Json<Vec<Value>>> {
    let p = Params::from_uri(&uri);
    let today = state
        .now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .map(|t| t.and_utc())
        .unwrap_or_else(|| state.now());
    let start = p.time("start").unwrap_or(today);
    let end = p.time("end").unwrap_or(today + Duration::days(2));
    let unmonitored = p.bool("unmonitored", false);
    let tags: Vec<i64> = p
        .get("tags")
        .map(|t| t.split(',').filter_map(|s| s.trim().parse().ok()).collect())
        .unwrap_or_default();
    let ctx = MovieContext::load(&state).await?;
    let shelf = load_shelf(&state).await?;
    let within = |d: Option<DateTime<Utc>>| d.is_some_and(|d| d >= start && d <= end);
    let mut found: Vec<&Movie> = shelf
        .movies
        .iter()
        .filter(|m| {
            let md = &m.metadata;
            within(md.in_cinemas) || within(md.physical_release) || within(md.digital_release)
        })
        .filter(|m| unmonitored || m.monitored)
        .filter(|m| tags.is_empty() || m.tags.iter().any(|t| tags.contains(t)))
        .collect();
    // `OrderBy(InCinemas).ThenBy(DigitalRelease).ThenBy(PhysicalRelease)`:
    // a movie without a date before those with one. The sort is stable, so
    // movies equal on all three stay as the library lists them.
    found.sort_by_key(|m| {
        let md = &m.metadata;
        (md.in_cinemas, md.digital_release, md.physical_release)
    });
    Ok(Json(
        found
            .iter()
            .map(|m| {
                movie_resource(
                    &ctx,
                    m,
                    &MovieParts {
                        alternative_titles: shelf
                            .titles
                            .get(&m.metadata.id)
                            .map(Vec::as_slice)
                            .unwrap_or(&[]),
                        file: shelf.files.get(&m.movie_file_id),
                        covers: Covers::Remote,
                        root_folder: false,
                        statistics: true,
                        movie_file: FileShown::WithFormats,
                    },
                )
            })
            .collect(),
    ))
}

/// The sort keys of the wanted lists.
const WANTED_SORT_KEYS: &[&str] = &[
    "movieMetadata.sortTitle",
    "movieMetadata.year",
    "movieMetadata.inCinemas",
    "movieMetadata.digitalRelease",
    "movieMetadata.physicalRelease",
    "movies.added",
    "movies.lastSearchTime",
    "movies.id",
];

/// One page of `wanted`: the movies sorted as the originals' query sorts
/// them (text as SQLite compares it, a missing date before any date, ties
/// in id order whatever the direction).
fn wanted_page(
    ctx: &MovieContext,
    shelf: &Shelf,
    mut wanted: Vec<&Movie>,
    paging: &Paging,
) -> PagingResource<Value> {
    let date = |d: Option<DateTime<Utc>>| d.map(|d| d.timestamp()).unwrap_or(i64::MIN);
    let key = |m: &Movie| -> (i64, String) {
        match paging.sort_key.as_str() {
            "movieMetadata.year" => (m.metadata.year, String::new()),
            "movieMetadata.inCinemas" => (date(m.metadata.in_cinemas), String::new()),
            "movieMetadata.digitalRelease" => (date(m.metadata.digital_release), String::new()),
            "movieMetadata.physicalRelease" => (date(m.metadata.physical_release), String::new()),
            "movies.added" => (date(m.added), String::new()),
            "movies.lastSearchTime" => (date(m.last_search_time), String::new()),
            "movies.id" => (m.id, String::new()),
            _ => (0, m.metadata.sort_title.clone().unwrap_or_default()),
        }
    };
    wanted.sort_by(|a, b| {
        let order = key(a).cmp(&key(b));
        let order = if paging.descending { order.reverse() } else { order };
        order.then_with(|| a.id.cmp(&b.id))
    });
    let total = wanted.len() as i64;
    let records = paging
        .slice(&wanted)
        .iter()
        .map(|m| {
            movie_resource(
                ctx,
                m,
                &MovieParts {
                    alternative_titles: &[],
                    file: shelf.files.get(&m.movie_file_id),
                    covers: Covers::Local,
                    root_folder: false,
                    statistics: true,
                    movie_file: FileShown::No,
                },
            )
        })
        .collect();
    PagingResource::new(paging, total, records)
}

fn wanted_paging(p: &Params) -> Paging {
    Paging::resolve(
        p.i64("page"),
        p.i64("pageSize"),
        p.get("sortKey"),
        p.descending(),
        WANTED_SORT_KEYS,
        "movieMetadata.sortTitle",
    )
}

/// `GET /wanted/missing`: the monitored movies without a file (the
/// unmonitored ones with `monitored=false`). A movie without a year is
/// not listed: asked of the original, which leaves out one whose year is
/// zero and keeps one that has a year and no release date.
pub async fn missing(State(state): State<AppState>, uri: Uri) -> ApiResult<Json<PagingResource<Value>>> {
    let p = Params::from_uri(&uri);
    let monitored = p.bool("monitored", true);
    let ctx = MovieContext::load(&state).await?;
    let shelf = load_shelf(&state).await?;
    let wanted = shelf
        .movies
        .iter()
        .filter(|m| m.movie_file_id == 0 && m.monitored == monitored && m.metadata.year > 0)
        .collect();
    Ok(Json(wanted_page(&ctx, &shelf, wanted, &wanted_paging(&p))))
}

/// `GET /wanted/cutoff`: the movies whose file is below the cutoff of
/// their quality profile.
pub async fn cutoff(State(state): State<AppState>, uri: Uri) -> ApiResult<Json<PagingResource<Value>>> {
    let p = Params::from_uri(&uri);
    let monitored = p.bool("monitored", true);
    let ctx = MovieContext::load(&state).await?;
    let shelf = load_shelf(&state).await?;
    let wanted = shelf
        .movies
        .iter()
        .filter(|m| m.monitored == monitored)
        .filter(|m| {
            let (Some(file), Some(profile)) = (
                shelf.files.get(&m.movie_file_id),
                ctx.profile(m.quality_profile_id),
            ) else {
                return false;
            };
            unarr_core::profile::quality_cutoff_not_met(profile, file.quality.quality_id)
        })
        .collect();
    Ok(Json(wanted_page(&ctx, &shelf, wanted, &wanted_paging(&p))))
}
