//! `episode`, `episodefile`, `calendar`, `wanted/missing`, `wanted/cutoff`.

use std::collections::HashMap;

use axum::extract::{Path, State};
use axum::http::Uri;
use axum::Json;
use chrono::Duration;
use unarr_core::customformat::{FormatSet, Input};
use unarr_core::model::{Episode, EpisodeFile, QualityProfile, Series};
use unarr_core::profile;
use unarr_core::store;
use unarr_core::store::paging::Paging;

use crate::error::{found, ApiError, ApiResult};
use crate::mediainfo;
use crate::query::Params;
use crate::resources::{
    languages, plain_images, series_resource_plain, CustomFormatRef, EpisodeFileResource, EpisodeResource,
    PagingResource,
};
use crate::state::AppState;

/// Reference data every episode-shaped response needs.
pub struct Catalog {
    pub series: HashMap<i64, Series>,
    pub profiles: Vec<QualityProfile>,
    pub formats: FormatSet,
}

impl Catalog {
    pub fn load(conn: &rusqlite::Connection) -> anyhow::Result<Catalog> {
        Ok(Catalog {
            series: store::series::list(conn)?
                .into_iter()
                .map(|s| (s.id, s))
                .collect(),
            profiles: store::catalog::list_quality_profiles(conn)?,
            formats: store::catalog::list_custom_formats(conn)?.into(),
        })
    }

    /// Only the given series: what a request about a few episodes needs.
    pub fn load_for(conn: &rusqlite::Connection, series_ids: &[i64]) -> anyhow::Result<Catalog> {
        let mut ids = series_ids.to_vec();
        ids.sort_unstable();
        ids.dedup();
        Ok(Catalog {
            series: store::series::get_many(conn, &ids)?
                .into_iter()
                .map(|s| (s.id, s))
                .collect(),
            profiles: store::catalog::list_quality_profiles(conn)?,
            formats: store::catalog::list_custom_formats(conn)?.into(),
        })
    }

    pub fn profile(&self, id: i64) -> Option<&QualityProfile> {
        self.profiles.iter().find(|p| p.id == id)
    }
}

/// The distinct series ids of a set of episodes.
fn series_ids_of(episodes: &[Episode]) -> Vec<i64> {
    episodes.iter().map(|e| e.series_id).collect()
}

/// Custom formats matching a file, and their score under the series' profile.
pub fn file_formats(catalog: &Catalog, file: &EpisodeFile, series: &Series) -> (Vec<CustomFormatRef>, i64) {
    let filename = file
        .relative_path
        .rsplit('/')
        .next()
        .unwrap_or(&file.relative_path);
    let release_title = file
        .scene_name
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            file.original_file_path
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .map(|p| p.rsplit('/').next().unwrap_or(p))
        })
        .unwrap_or(filename);
    let input = Input {
        release_title,
        filename: Some(filename),
        release_group: file.release_group.as_deref(),
        quality: file.quality,
        languages: &file.languages,
        size: file.size,
        indexer_flags: file.indexer_flags,
        release_type: file.release_type,
        series_original_language: series.original_language_id,
        movie: None,
    };
    let matched = catalog.formats.evaluate(&input);
    let ids: Vec<i64> = matched.iter().map(|f| f.id).collect();
    let score = catalog
        .profile(series.quality_profile_id)
        .map(|p| profile::custom_format_score(p, &ids))
        .unwrap_or(0);
    (
        matched
            .iter()
            .map(|f| CustomFormatRef {
                id: f.id,
                name: f.name.clone(),
            })
            .collect(),
        score,
    )
}

pub fn episode_file_resource(catalog: &Catalog, file: &EpisodeFile, series: &Series) -> EpisodeFileResource {
    let (custom_formats, custom_format_score) = file_formats(catalog, file, series);
    EpisodeFileResource {
        id: file.id,
        series_id: file.series_id,
        season_number: file.season_number,
        relative_path: file.relative_path.clone(),
        path: format!("{}/{}", series.path.trim_end_matches('/'), file.relative_path),
        size: file.size,
        date_added: file.date_added,
        scene_name: file.scene_name.clone(),
        release_group: file.release_group.clone(),
        languages: languages(&file.languages),
        quality: file.quality.into(),
        custom_formats,
        custom_format_score,
        indexer_flags: file.indexer_flags,
        release_type: file.release_type.as_str(),
        media_info: mediainfo::resource(file.media_info.as_ref(), file.scene_name.as_deref()),
        quality_cutoff_not_met: catalog
            .profile(series.quality_profile_id)
            .map(|p| profile::quality_cutoff_not_met(p, file.quality.quality_id))
            .unwrap_or(false),
    }
}

/// What to embed in an episode resource.
#[derive(Clone, Copy, Default)]
pub struct Include {
    pub series: bool,
    pub file: bool,
    pub images: bool,
}

pub fn episode_resource(
    catalog: &Catalog,
    e: &Episode,
    file: Option<&EpisodeFile>,
    include: Include,
) -> EpisodeResource {
    let series = catalog.series.get(&e.series_id);
    EpisodeResource {
        id: e.id,
        series_id: e.series_id,
        tvdb_id: e.tvdb_id,
        episode_file_id: e.episode_file_id,
        season_number: e.season_number,
        episode_number: e.episode_number,
        title: e.title.clone(),
        air_date: e.air_date,
        air_date_utc: e.air_date_utc,
        last_search_time: e.last_search_time,
        runtime: e.runtime,
        finale_type: e.finale_type.clone(),
        overview: e.overview.clone(),
        episode_file: match (include.file, file, series) {
            (true, Some(f), Some(s)) if e.episode_file_id != 0 => Some(episode_file_resource(catalog, f, s)),
            _ => None,
        },
        has_file: e.episode_file_id > 0,
        monitored: e.monitored,
        absolute_episode_number: e.absolute_episode_number,
        scene_absolute_episode_number: e.scene_absolute_episode_number,
        scene_episode_number: e.scene_episode_number,
        scene_season_number: e.scene_season_number,
        unverified_scene_numbering: e.unverified_scene_numbering,
        series: if include.series {
            series.map(series_resource_plain)
        } else {
            None
        },
        images: include.images.then(|| plain_images(&e.images)),
    }
}

/// Files of the given episodes, by id, when `include.file` is set.
fn files_for(
    conn: &rusqlite::Connection,
    episodes: &[Episode],
    include: Include,
) -> anyhow::Result<HashMap<i64, EpisodeFile>> {
    if !include.file {
        return Ok(HashMap::new());
    }
    let ids: Vec<i64> = episodes
        .iter()
        .map(|e| e.episode_file_id)
        .filter(|id| *id > 0)
        .collect();
    Ok(store::episodes::list_files_by_ids(conn, &ids)?
        .into_iter()
        .map(|f| (f.id, f))
        .collect())
}

fn render(
    catalog: &Catalog,
    episodes: &[Episode],
    files: &HashMap<i64, EpisodeFile>,
    include: Include,
) -> Vec<EpisodeResource> {
    episodes
        .iter()
        .map(|e| episode_resource(catalog, e, files.get(&e.episode_file_id), include))
        .collect()
}

fn include_from(p: &Params, images_key: &str) -> Include {
    Include {
        series: p.bool("includeSeries", false),
        file: p.bool("includeEpisodeFile", false),
        images: p.bool(images_key, false),
    }
}

// --- episode

pub async fn episodes(State(state): State<AppState>, uri: Uri) -> ApiResult<Json<Vec<EpisodeResource>>> {
    let p = Params::from_uri(&uri);
    let include = include_from(&p, "includeImages");
    let series_id = p.i64("seriesId");
    let season = p.i64("seasonNumber");
    let ids = p.ints("episodeIds");
    let file_id = p.i64("episodeFileId");
    if series_id.is_none() && ids.is_empty() && file_id.is_none() {
        return Err(ApiError::BadRequest(
            "seriesId or episodeIds must be provided".into(),
        ));
    }
    let out = state
        .db()
        .call(move |conn| {
            let episodes = if let Some(sid) = series_id {
                store::episodes::list_episodes(conn, sid, season)?
            } else if !ids.is_empty() {
                store::episodes::list_episodes_by_ids(conn, &ids)?
            } else {
                store::episodes::list_episodes_by_file(conn, file_id.unwrap_or(0))?
            };
            let catalog = Catalog::load_for(conn, &series_ids_of(&episodes))?;
            let files = files_for(conn, &episodes, include)?;
            Ok(render(&catalog, &episodes, &files, include))
        })
        .await?;
    Ok(Json(out))
}

pub async fn episode(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Json<EpisodeResource>> {
    let include = Include {
        series: true,
        file: true,
        images: true,
    };
    let out = state
        .db()
        .call(move |conn| {
            let Some(e) = store::episodes::get_episode(conn, id)? else {
                return Ok(None);
            };
            let catalog = Catalog::load_for(conn, &[e.series_id])?;
            let files = files_for(conn, std::slice::from_ref(&e), include)?;
            Ok(render(&catalog, &[e], &files, include).pop())
        })
        .await?;
    Ok(Json(found(out)?))
}

// --- episodefile

pub async fn episode_files(
    State(state): State<AppState>,
    uri: Uri,
) -> ApiResult<Json<Vec<EpisodeFileResource>>> {
    let p = Params::from_uri(&uri);
    let series_id = p.i64("seriesId");
    let ids = p.ints("episodeFileIds");
    if series_id.is_none() && ids.is_empty() {
        return Err(ApiError::BadRequest(
            "seriesId or episodeFileIds must be provided".into(),
        ));
    }
    let out = state
        .db()
        .call(move |conn| {
            let files = match series_id {
                Some(sid) => store::episodes::list_files(conn, sid)?,
                None => store::episodes::list_files_by_ids(conn, &ids)?,
            };
            let catalog = Catalog::load_for(conn, &files.iter().map(|f| f.series_id).collect::<Vec<_>>())?;
            Ok(files
                .iter()
                .filter_map(|f| {
                    catalog
                        .series
                        .get(&f.series_id)
                        .map(|s| episode_file_resource(&catalog, f, s))
                })
                .collect::<Vec<_>>())
        })
        .await?;
    Ok(Json(out))
}

pub async fn episode_file(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<EpisodeFileResource>> {
    let out = state
        .db()
        .call(move |conn| {
            let Some(f) = store::episodes::get_file(conn, id)? else {
                return Ok(None);
            };
            let catalog = Catalog::load_for(conn, &[f.series_id])?;
            Ok(catalog
                .series
                .get(&f.series_id)
                .map(|s| episode_file_resource(&catalog, &f, s)))
        })
        .await?;
    Ok(Json(found(out)?))
}

// --- calendar

pub async fn calendar(State(state): State<AppState>, uri: Uri) -> ApiResult<Json<Vec<EpisodeResource>>> {
    let p = Params::from_uri(&uri);
    let today = state.now().date_naive().and_hms_opt(0, 0, 0).unwrap().and_utc();
    let start = p.time("start").unwrap_or(today);
    let end = p.time("end").unwrap_or(today + Duration::days(2));
    let unmonitored = p.bool("unmonitored", false);
    let include = include_from(&p, "includeEpisodeImages");
    let tags: Vec<String> = p
        .get("tags")
        .unwrap_or("")
        .split(',')
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();
    let out = state
        .db()
        .call(move |conn| {
            let tag_ids: Vec<i64> = if tags.is_empty() {
                Vec::new()
            } else {
                let all = store::catalog::list_tags(conn)?;
                tags.iter()
                    .filter_map(|t| {
                        t.parse::<i64>()
                            .ok()
                            .or_else(|| all.iter().find(|x| x.label.eq_ignore_ascii_case(t)).map(|x| x.id))
                    })
                    .collect()
            };
            let in_range = store::episodes::episodes_between(conn, start, end, unmonitored)?;
            // Only the series the range shows, not the whole library.
            let catalog = Catalog::load_for(conn, &series_ids_of(&in_range))?;
            let episodes: Vec<Episode> = in_range
                .into_iter()
                .filter(|e| match catalog.series.get(&e.series_id) {
                    None => false,
                    Some(_) if tag_ids.is_empty() => true,
                    Some(s) => s.tags.iter().any(|t| tag_ids.contains(t)),
                })
                .collect();
            let files = files_for(conn, &episodes, include)?;
            let mut out = render(&catalog, &episodes, &files, include);
            out.sort_by_key(|e| e.air_date_utc);
            Ok(out)
        })
        .await?;
    Ok(Json(out))
}

// --- wanted

const WANTED_SORT_KEYS: &[&str] = &[
    "episodes.airDateUtc",
    "episodes.lastSearchTime",
    "series.sortTitle",
];

fn wanted_paging(p: &Params) -> Paging {
    Paging::resolve(
        p.i64("page"),
        p.i64("pageSize"),
        p.get("sortKey"),
        p.descending(),
        WANTED_SORT_KEYS,
        "episodes.airDateUtc",
    )
}

pub async fn missing(
    State(state): State<AppState>,
    uri: Uri,
) -> ApiResult<Json<PagingResource<EpisodeResource>>> {
    let p = Params::from_uri(&uri);
    let paging = wanted_paging(&p);
    let monitored = p.bool("monitored", true);
    let include = Include {
        series: p.bool("includeSeries", false),
        file: false,
        images: p.bool("includeImages", false),
    };
    let now = state.now();
    let out = state
        .db()
        .call(move |conn| {
            let (episodes, total) = store::episodes::missing_episodes(conn, &paging, monitored, now)?;
            // Only the page's series, not the whole library.
            let catalog = Catalog::load_for(conn, &series_ids_of(&episodes))?;
            let records = render(&catalog, &episodes, &HashMap::new(), include);
            Ok(PagingResource::new(&paging, total, records))
        })
        .await?;
    Ok(Json(out))
}

pub async fn cutoff(
    State(state): State<AppState>,
    uri: Uri,
) -> ApiResult<Json<PagingResource<EpisodeResource>>> {
    let p = Params::from_uri(&uri);
    let paging = wanted_paging(&p);
    let monitored = p.bool("monitored", true);
    let include = include_from(&p, "includeImages");
    let out = state
        .db()
        .call(move |conn| {
            let profiles = store::catalog::list_quality_profiles(conn)?;
            let below: HashMap<i64, Vec<i64>> = profiles
                .iter()
                .map(|pr| (pr.id, profile::qualities_below_cutoff(pr)))
                .collect();
            // The light rows are filtered and sorted; only the page's
            // episodes are then read in full.
            let mut rows: Vec<_> = store::episodes::cutoff_rows(conn, monitored)?
                .into_iter()
                .filter(|r| {
                    below
                        .get(&r.quality_profile_id)
                        .is_some_and(|q| q.contains(&r.quality_id))
                })
                .collect();
            // Ties keep id order whatever the direction, as SQLite's sorter
            // does for the originals' single-column ORDER BY.
            rows.sort_by(|a, b| {
                let by_key = match paging.sort_key.as_str() {
                    "series.sortTitle" => a.series_sort_title.cmp(&b.series_sort_title),
                    "episodes.lastSearchTime" => a.last_search_time.cmp(&b.last_search_time),
                    _ => a.air_date_utc.cmp(&b.air_date_utc),
                };
                let by_key = if paging.descending {
                    by_key.reverse()
                } else {
                    by_key
                };
                by_key.then(a.id.cmp(&b.id))
            });
            let total = rows.len() as i64;
            let ids: Vec<i64> = paging.slice(&rows).iter().map(|r| r.id).collect();
            // `list_episodes_by_ids` reads in id order: back to the page's order.
            let mut by_id: HashMap<i64, Episode> = store::episodes::list_episodes_by_ids(conn, &ids)?
                .into_iter()
                .map(|e| (e.id, e))
                .collect();
            let page: Vec<Episode> = ids.iter().filter_map(|id| by_id.remove(id)).collect();
            // Only the page's series, not the whole library.
            let catalog = Catalog::load_for(conn, &series_ids_of(&page))?;
            let files = files_for(conn, &page, include)?;
            let records = render(&catalog, &page, &files, include);
            Ok(PagingResource::new(&paging, total, records))
        })
        .await?;
    Ok(Json(out))
}
