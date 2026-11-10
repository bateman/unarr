//! The series writes and the endpoints around them: `series/lookup`,
//! `POST`/`PUT`/`DELETE series`, `series/editor`, `seasonpass`,
//! `series/{id}/folder`, the episode monitoring writes, `parse`, the folder
//! browser (`filesystem`) and `MediaCoverProxy`, after the originals'
//! controllers of the same names.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use axum::extract::{Path, State};
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};
use unarr_core::model::{NewItemMonitor, Season, Series, SeriesType};
use unarr_core::parser::title::ParsedEpisodeInfo;
use unarr_core::store;
use unarr_core::tv::monitoring::MonitoringOptions;
use unarr_core::tv::series::{self as service, BulkEdit, NewSeries, SeriesEdit, WriteError};

use crate::error::{ApiError, ApiResult};
use crate::query::Params;
use crate::resources::{
    languages, series_resource, series_resource_plain, SeriesContext, SeriesResource,
    SeriesStatisticsResource,
};
use crate::routes::episodes::{episode_resource, Catalog, Include};
use crate::state::AppState;

fn write_error(e: WriteError) -> ApiError {
    match e {
        WriteError::Invalid(f) => ApiError::Validation(f),
        WriteError::InvalidPlain(f) => ApiError::ValidationPlain(f),
        WriteError::Other(e) => ApiError::Internal(e),
    }
}

/// `MediaCoverProxy`'s cache: the remote URL of each hash, for a day.
fn proxied() -> &'static Mutex<HashMap<String, (String, std::time::Instant)>> {
    static CACHE: OnceLock<Mutex<HashMap<String, (String, std::time::Instant)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

const PROXY_TTL: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

/// `RegisterUrl`: the proxy URL of a remote image, remembered.
fn register_proxy(url_base: &str, remote: &str) -> String {
    let url = unarr_core::tv::covers::proxy_url(url_base, remote);
    let hash = url.rsplit('/').nth(1).unwrap_or("").to_string();
    let mut cache = proxied().lock().unwrap();
    cache.retain(|_, (_, at)| at.elapsed() < PROXY_TTL);
    cache.insert(hash, (remote.to_string(), std::time::Instant::now()));
    url
}

/// `GET /MediaCoverProxy/{hash}/{file}`: the remote image, fetched now.
pub async fn media_cover_proxy(
    State(state): State<AppState>,
    Path((hash, _file)): Path<(String, String)>,
) -> Response {
    let remote = proxied()
        .lock()
        .unwrap()
        .get(&hash)
        .filter(|(_, at)| at.elapsed() < PROXY_TTL)
        .map(|(u, _)| u.clone());
    let Some(remote) = remote else {
        return ApiError::Internal(anyhow::anyhow!("Url no longer in cache")).into_response();
    };
    let Ok(runtime) = state.commands() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match runtime.augment_client().download(&remote).await {
        Ok(bytes) => {
            let content_type = if remote.to_ascii_lowercase().ends_with(".png") {
                "image/png"
            } else {
                "image/jpeg"
            };
            ([(header::CONTENT_TYPE, content_type)], bytes).into_response()
        }
        Err(e) => ApiError::Internal(e).into_response(),
    }
}

/// `SeriesLookupController.MapToResource`: the bare resource, local cover
/// URLs for a series in the library and proxied ones otherwise, the poster
/// URL, the folder the naming would give, empty statistics.
fn lookup_resource(state: &AppState, s: &Series, naming: &unarr_core::model::NamingConfig) -> SeriesResource {
    let settings = state.settings();
    let url_base = &settings.url_base;
    let mut r = series_resource(
        s,
        SeriesContext {
            url_base,
            media_cover_dir: Some(&state.runtime().media_cover_dir),
            root_folder_path: String::new(),
            alternate_titles: Vec::new(),
            season_stats: None,
            include_season_images: false,
        },
    );
    if s.id == 0 {
        for (image, source) in r.images.iter_mut().zip(&s.images) {
            image.url = source.remote_url.as_deref().map(|u| register_proxy(url_base, u));
        }
    }
    r.alternate_titles = None;
    r.root_folder_path = None;
    r.next_airing = None;
    r.previous_airing = None;
    r.last_aired = s.last_aired;
    r.remote_poster = s
        .images
        .iter()
        .find(|i| i.cover_type == "poster")
        .and_then(|i| i.remote_url.clone());
    r.folder = Some(unarr_core::naming::series_folder(
        &unarr_core::mediafiles::approve::naming_series(s),
        naming,
    ));
    r.statistics = Some(empty_statistics(s));
    r
}

/// `new SeriesStatistics().ToResource(seasons)`.
fn empty_statistics(s: &Series) -> SeriesStatisticsResource {
    SeriesStatisticsResource {
        season_count: s.seasons.iter().filter(|x| x.season_number > 0).count() as i64,
        episode_file_count: 0,
        episode_count: 0,
        total_episode_count: 0,
        size_on_disk: 0,
        release_groups: None,
        percent_of_episodes: 0.0,
    }
}

/// `GET /series/lookup?term=`.
pub async fn lookup(State(state): State<AppState>, uri: Uri) -> ApiResult<Json<Vec<SeriesResource>>> {
    let term = Params::from_uri(&uri)
        .get("term")
        .map(str::to_string)
        .unwrap_or_default();
    let provider = state.commands()?.metadata();
    let found = service::lookup(state.db(), provider.as_ref(), &term)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("{e}")))?;
    let naming = state.db().call(|conn| store::config::naming(conn)).await?;
    Ok(Json(
        found
            .iter()
            .map(|s| lookup_resource(&state, s, &naming))
            .collect(),
    ))
}

fn str_field(body: &Value, key: &str) -> Option<String> {
    body.get(key).and_then(Value::as_str).map(str::to_string)
}

fn monitor_new_items(body: &Value) -> NewItemMonitor {
    match body
        .get("monitorNewItems")
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("none") => NewItemMonitor::None,
        _ => NewItemMonitor::All,
    }
}

fn series_type(v: Option<&Value>) -> Option<SeriesType> {
    match v?.as_str()?.to_ascii_lowercase().as_str() {
        "standard" => Some(SeriesType::Standard),
        "daily" => Some(SeriesType::Daily),
        "anime" => Some(SeriesType::Anime),
        _ => None,
    }
}

fn seasons(body: &Value) -> Vec<Season> {
    body.get("seasons")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|s| Season {
            season_number: s["seasonNumber"].as_i64().unwrap_or(0),
            monitored: s["monitored"].as_bool().unwrap_or(false),
            images: Vec::new(),
        })
        .collect()
}

fn ints(v: Option<&Value>) -> Vec<i64> {
    v.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_i64)
        .collect()
}

/// The add options as the originals serialise `AddSeriesOptions`: every
/// flag present.
fn add_options(v: Option<&Value>) -> Option<Value> {
    let v = v.filter(|v| v.is_object())?;
    let monitor = MonitoringOptions::from_json(v).monitor;
    let flag = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
    Some(json!({
        "ignoreEpisodesWithFiles": flag("ignoreEpisodesWithFiles"),
        "ignoreEpisodesWithoutFiles": flag("ignoreEpisodesWithoutFiles"),
        "monitor": monitor,
        "searchForMissingEpisodes": flag("searchForMissingEpisodes"),
        "searchForCutoffUnmetEpisodes": flag("searchForCutoffUnmetEpisodes"),
    }))
}

/// One series as `GET /series/{id}` renders it, with the empty statistics
/// of a series that has no episode yet.
async fn series_by_id(state: &AppState, id: i64) -> ApiResult<SeriesResource> {
    let mut r = crate::routes::series::series_resource_by_id(state, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    if r.statistics.is_none() {
        let s = state
            .db()
            .call(move |conn| store::series::get(conn, id))
            .await?
            .ok_or(ApiError::NotFound)?;
        r.statistics = Some(empty_statistics(&s));
    }
    Ok(r)
}

/// `POST /series`.
pub async fn add(State(state): State<AppState>, Json(body): Json<Value>) -> ApiResult<Response> {
    if !body.is_object() {
        return Err(ApiError::BadRequest(
            "The request body must be a JSON object".into(),
        ));
    }
    let new = NewSeries {
        tvdb_id: body.get("tvdbId").and_then(Value::as_i64).unwrap_or(0),
        title: str_field(&body, "title"),
        path: str_field(&body, "path").filter(|p| !p.trim().is_empty()),
        root_folder_path: str_field(&body, "rootFolderPath"),
        quality_profile_id: body.get("qualityProfileId").and_then(Value::as_i64).unwrap_or(0),
        season_folder: body.get("seasonFolder").and_then(Value::as_bool).unwrap_or(false),
        monitored: body.get("monitored").and_then(Value::as_bool).unwrap_or(false),
        monitor_new_items: monitor_new_items(&body),
        series_type: series_type(body.get("seriesType")).unwrap_or(SeriesType::Standard),
        tags: ints(body.get("tags")),
        seasons: seasons(&body),
        add_options: add_options(body.get("addOptions")),
    };
    let check = new.clone();
    let failures = state
        .db()
        .call(move |conn| service::validate_post(conn, &check))
        .await?;
    if !failures.is_empty() {
        return Err(ApiError::Validation(failures));
    }
    let provider = state.commands()?.metadata();
    let added = service::add(state.db(), provider.as_ref(), new, state.now())
        .await
        .map_err(write_error)?;
    state.commands()?.series_added(added.id).await?;
    let resource = series_by_id(&state, added.id).await?;
    let location = format!("{}/api/v3/series/{}", state.settings().url_base, added.id);
    Ok((
        StatusCode::CREATED,
        [(header::LOCATION, location)],
        Json(resource),
    )
        .into_response())
}

/// `PUT /series/{id}[?moveFiles=true]`: the id the body carries is the
/// one updated, as the originals read it.
pub async fn update(
    State(state): State<AppState>,
    Path(_route_id): Path<i64>,
    uri: Uri,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    if !body.is_object() {
        return Err(ApiError::BadRequest(
            "The request body must be a JSON object".into(),
        ));
    }
    let move_files = Params::from_uri(&uri).bool("moveFiles", false);
    let id = body.get("id").and_then(Value::as_i64).unwrap_or(0);
    let edit = SeriesEdit {
        id,
        tvdb_id: body.get("tvdbId").and_then(Value::as_i64).unwrap_or(0),
        path: str_field(&body, "path").unwrap_or_default(),
        quality_profile_id: body.get("qualityProfileId").and_then(Value::as_i64).unwrap_or(0),
        season_folder: body.get("seasonFolder").and_then(Value::as_bool).unwrap_or(false),
        monitored: body.get("monitored").and_then(Value::as_bool).unwrap_or(false),
        monitor_new_items: monitor_new_items(&body),
        series_type: series_type(body.get("seriesType")).unwrap_or(SeriesType::Standard),
        tags: ints(body.get("tags")),
        seasons: seasons(&body),
    };
    let check = edit.clone();
    let failures = state
        .db()
        .call(move |conn| service::validate_put(conn, &check))
        .await?;
    if !failures.is_empty() {
        return Err(ApiError::Validation(failures));
    }
    let stored_path = state
        .db()
        .call(move |conn| Ok(store::series::get_many(conn, &[id])?.pop().map(|s| s.path)))
        .await?
        .ok_or(ApiError::NotFound)?;
    if move_files {
        let runtime = state.commands()?;
        runtime
            .queue()
            .push(
                unarr_core::commands::CommandBody::MoveSeries {
                    series_id: id,
                    source_path: stored_path,
                    destination_path: edit.path.clone(),
                },
                unarr_core::commands::CommandPriority::Normal,
                unarr_core::commands::CommandTrigger::Manual,
                state.now(),
            )
            .await?;
    }
    state
        .db()
        .call(move |conn| {
            let tx = conn.unchecked_transaction()?;
            service::update(&tx, &edit)?;
            tx.commit()?;
            Ok(())
        })
        .await?;
    let resource = series_by_id(&state, id).await?;
    state.hub().broadcast(
        "series",
        json!({"action": "updated", "resource": resource.clone()}),
    );
    Ok((StatusCode::ACCEPTED, Json(resource)).into_response())
}

/// `DELETE /series/{id}?deleteFiles=&addImportListExclusion=`.
pub async fn delete(State(state): State<AppState>, Path(id): Path<i64>, uri: Uri) -> ApiResult<Response> {
    let p = Params::from_uri(&uri);
    delete_many(
        &state,
        vec![id],
        p.bool("deleteFiles", false),
        p.bool("addImportListExclusion", false),
    )
    .await?;
    Ok(StatusCode::OK.into_response())
}

async fn delete_many(state: &AppState, ids: Vec<i64>, delete_files: bool, exclude: bool) -> ApiResult<()> {
    let deleted = state
        .db()
        .call(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let out = service::delete(&tx, &ids, exclude)?;
            tx.commit()?;
            Ok(out)
        })
        .await?;
    for s in &deleted {
        state
            .hub()
            .broadcast("series", json!({"action": "deleted", "resource": {"id": s.id}}));
    }
    state.commands()?.series_deleted(deleted, delete_files).await;
    Ok(())
}

/// `PUT /series/editor`.
pub async fn editor_update(State(state): State<AppState>, Json(body): Json<Value>) -> ApiResult<Response> {
    let edit = BulkEdit {
        series_ids: ints(body.get("seriesIds")),
        monitored: body.get("monitored").and_then(Value::as_bool),
        monitor_new_items: body.get("monitorNewItems").and_then(Value::as_str).map(|v| {
            if v.eq_ignore_ascii_case("none") {
                NewItemMonitor::None
            } else {
                NewItemMonitor::All
            }
        }),
        quality_profile_id: body.get("qualityProfileId").and_then(Value::as_i64),
        series_type: series_type(body.get("seriesType")),
        season_folder: body.get("seasonFolder").and_then(Value::as_bool),
        root_folder_path: str_field(&body, "rootFolderPath"),
        tags: body.get("tags").filter(|t| t.is_array()).map(|t| ints(Some(t))),
        apply_tags: str_field(&body, "applyTags").unwrap_or_else(|| "add".into()),
        move_files: body.get("moveFiles").and_then(Value::as_bool).unwrap_or(false),
    };
    let root = edit.root_folder_path.clone();
    let (updated, moves) = state
        .db()
        .call(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let out = service::update_many(&tx, &edit)?;
            tx.commit()?;
            Ok(out)
        })
        .await?;
    if !moves.is_empty() {
        state
            .commands()?
            .queue()
            .push(
                unarr_core::commands::CommandBody::BulkMoveSeries {
                    destination_root_folder: root.unwrap_or_default(),
                    series: moves,
                },
                unarr_core::commands::CommandPriority::Normal,
                unarr_core::commands::CommandTrigger::Unspecified,
                state.now(),
            )
            .await?;
    }
    let out: Vec<SeriesResource> = updated
        .iter()
        .map(|s| {
            let mut r = series_resource_plain(s);
            r.last_aired = s.last_aired;
            r
        })
        .collect();
    for s in &updated {
        if let Ok(Some(r)) = crate::routes::series::series_resource_by_id(&state, s.id).await {
            state
                .hub()
                .broadcast("series", json!({"action": "updated", "resource": r}));
        }
    }
    Ok((StatusCode::ACCEPTED, Json(out)).into_response())
}

/// `DELETE /series/editor`.
pub async fn editor_delete(State(state): State<AppState>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    delete_many(
        &state,
        ints(body.get("seriesIds")),
        body.get("deleteFiles").and_then(Value::as_bool).unwrap_or(false),
        body.get("addImportListExclusion")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    )
    .await?;
    Ok(Json(json!({})))
}

/// `POST /seasonpass`.
pub async fn season_pass(State(state): State<AppState>, Json(body): Json<Value>) -> ApiResult<Response> {
    let options = body
        .get("monitoringOptions")
        .filter(|o| o.is_object())
        .map(MonitoringOptions::from_json);
    let now = state.now();
    let items: Vec<(i64, Option<bool>, Vec<Season>)> = body
        .get("series")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|s| {
            (
                s["id"].as_i64().unwrap_or(0),
                s["monitored"].as_bool(),
                seasons(s),
            )
        })
        .collect();
    let ids: Vec<i64> = items.iter().map(|(id, _, _)| *id).collect();
    state
        .db()
        .call(move |conn| {
            let tx = conn.unchecked_transaction()?;
            for (id, monitored, seasons) in &items {
                service::season_pass(&tx, *id, *monitored, seasons, options.as_ref(), now)?;
            }
            tx.commit()?;
            Ok(())
        })
        .await?;
    for id in ids {
        if let Ok(Some(r)) = crate::routes::series::series_resource_by_id(&state, id).await {
            state
                .hub()
                .broadcast("series", json!({"action": "updated", "resource": r}));
        }
    }
    Ok((StatusCode::ACCEPTED, Json(json!({}))).into_response())
}

/// `GET /series/{id}/folder`.
pub async fn folder(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let folder = state
        .db()
        .call(move |conn| {
            let naming = store::config::naming(conn)?;
            Ok(store::series::get_many(conn, &[id])?.pop().map(|s| {
                unarr_core::naming::series_folder(
                    &unarr_core::mediafiles::approve::naming_series(&s),
                    &naming,
                )
            }))
        })
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(json!({ "folder": folder })))
}

fn bare_episode(catalog: &Catalog, e: &unarr_core::model::Episode) -> Value {
    serde_json::to_value(episode_resource(
        catalog,
        e,
        None,
        Include {
            series: false,
            file: false,
            images: false,
        },
    ))
    .unwrap_or(Value::Null)
}

/// `PUT /episode/monitor`: `{episodeIds, monitored}`.
pub async fn episodes_monitor(State(state): State<AppState>, Json(body): Json<Value>) -> ApiResult<Response> {
    let ids = ints(body.get("episodeIds"));
    let monitored = body.get("monitored").and_then(Value::as_bool).unwrap_or(false);
    let out = state
        .db()
        .call(move |conn| {
            store::episodes::set_monitored(conn, &ids, monitored)?;
            let episodes = store::episodes::list_episodes_by_ids(conn, &ids)?;
            let series: Vec<i64> = episodes.iter().map(|e| e.series_id).collect();
            let catalog = Catalog::load_for(conn, &series)?;
            Ok(episodes
                .iter()
                .map(|e| bare_episode(&catalog, e))
                .collect::<Vec<_>>())
        })
        .await?;
    for e in &out {
        state
            .hub()
            .broadcast("episode", json!({"action": "updated", "resource": e}));
    }
    Ok((StatusCode::ACCEPTED, Json(out)).into_response())
}

/// `PUT /episode/{id}`: only the monitoring changes.
pub async fn episode_update(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    let id = body.get("id").and_then(Value::as_i64).unwrap_or(id);
    let monitored = body.get("monitored").and_then(Value::as_bool).unwrap_or(false);
    let out = state
        .db()
        .call(move |conn| {
            store::episodes::set_monitored(conn, &[id], monitored)?;
            let Some(e) = store::episodes::get_episode(conn, id)? else {
                return Ok(None);
            };
            let catalog = Catalog::load_for(conn, &[e.series_id])?;
            Ok(Some(bare_episode(&catalog, &e)))
        })
        .await?
        .ok_or(ApiError::NotFound)?;
    state
        .hub()
        .broadcast("episode", json!({"action": "updated", "resource": out.clone()}));
    Ok((StatusCode::ACCEPTED, Json(out)).into_response())
}

/// `ParsedEpisodeInfo` as the originals serialise it.
pub fn parsed_info_json(p: &ParsedEpisodeInfo) -> Value {
    let mut title_info = json!({
        "title": p.series_title_info.title,
        "titleWithoutYear": p.series_title_info.title_without_year,
        "year": p.series_title_info.year,
    });
    if !p.series_title_info.all_titles.is_empty() {
        title_info["allTitles"] = json!(p.series_title_info.all_titles);
    }
    let mut o = json!({
        "releaseTitle": p.release_title,
        "seriesTitle": p.series_title,
        "seriesTitleInfo": title_info,
        "quality": serde_json::to_value(crate::resources::QualityModelResource::from(p.quality.quality)).unwrap_or(Value::Null),
        "seasonNumber": p.season_number,
        "episodeNumbers": p.episode_numbers,
        "absoluteEpisodeNumbers": p.absolute_episode_numbers,
        "specialAbsoluteEpisodeNumbers": p.special_absolute_episode_numbers,
        "languages": languages(&p.languages),
        "fullSeason": p.full_season,
        "isPartialSeason": p.is_partial_season,
        "isMultiSeason": p.is_multi_season,
        "isSeasonExtra": p.is_season_extra,
        "isSplitEpisode": p.is_split_episode,
        "isMiniSeries": p.is_mini_series,
        "special": p.special,
        "releaseHash": p.release_hash,
        "seasonPart": p.season_part,
        "releaseTokens": p.release_tokens,
        "isDaily": p.is_daily(),
        "isAbsoluteNumbering": p.is_absolute_numbering(),
        "isPossibleSpecialEpisode": p.is_possible_special_episode(),
        "isPossibleSceneSeasonSpecial": p.is_possible_scene_season_special(),
        "releaseType": p.release_type().as_str(),
    });
    if let Some(d) = &p.air_date {
        o["airDate"] = json!(d);
    }
    if let Some(g) = &p.release_group {
        o["releaseGroup"] = json!(g);
    }
    if let Some(part) = p.daily_part {
        o["dailyPart"] = json!(part);
    }
    o
}

/// `GET /parse?title=[&path=]`: nothing without a title.
pub async fn parse(State(state): State<AppState>, uri: Uri) -> ApiResult<Response> {
    let p = Params::from_uri(&uri);
    let Some(title) = p
        .get("title")
        .map(str::to_string)
        .filter(|t| !t.trim().is_empty())
    else {
        return Ok(StatusCode::NO_CONTENT.into_response());
    };
    let path = p.get("path").map(str::to_string).filter(|t| !t.trim().is_empty());
    let parsed = match &path {
        Some(path) => unarr_core::parser::title::parse_path(path),
        None => unarr_core::parser::title::parse_title(&title),
    };
    let Some(parsed) = parsed else {
        return Ok(Json(json!({ "title": title })).into_response());
    };
    let now = state.now();
    let out = state
        .db()
        .call(move |conn| {
            let lookup = unarr_core::decision::lookup::StoreLookup::new(conn);
            let release = unarr_core::indexers::rss::ReleaseInfo {
                title: title.clone(),
                ..Default::default()
            };
            let Ok(mut remote) = unarr_core::decision::mapping::map(&lookup, release, parsed.clone(), None) else {
                return Ok(json!({ "title": title, "parsedEpisodeInfo": parsed_info_json(&parsed) }));
            };
            let engine_catalog = unarr_core::decision::lookup::Catalog::load(conn, now)?;
            unarr_core::decision::engine::augment_tracked(&engine_catalog, &lookup, now, &mut remote, 0);
            let catalog = Catalog::load(conn)?;
            let mut o = json!({
                "title": title,
                "parsedEpisodeInfo": parsed_info_json(remote.parsed.as_ref().unwrap_or(&parsed)),
                "episodes": remote.episodes.iter().map(|e| bare_episode(&catalog, e)).collect::<Vec<_>>(),
                "languages": languages(&remote.languages),
                "customFormats": remote.custom_formats.iter().map(|f| json!({"id": f.id, "name": f.name})).collect::<Vec<_>>(),
                "customFormatScore": remote.custom_format_score,
            });
            if let Some(series) = &remote.series {
                let mut r = series_resource_plain(series);
                r.last_aired = series.last_aired;
                o["series"] = serde_json::to_value(r)?;
            }
            Ok(o)
        })
        .await?;
    Ok(Json(out).into_response())
}

/// `FileSystemLookupService.LookupContents`: the folders (and files) of a
/// path, its parent; nothing for a path that does not exist.
pub async fn filesystem(State(_state): State<AppState>, uri: Uri) -> ApiResult<Json<Value>> {
    let p = Params::from_uri(&uri);
    let path = p.get("path").map(str::to_string).unwrap_or_default();
    let include_files = p.bool("includeFiles", false);
    let allow_without_slash = p.bool("allowFoldersWithoutTrailingSlashes", false);
    let out = crate::blocking::run(move || listing(&path, include_files, allow_without_slash)).await??;
    Ok(Json(out))
}

fn modified(meta: &std::fs::Metadata) -> String {
    meta.modified()
        .ok()
        .map(|t| {
            chrono::DateTime::<chrono::Utc>::from(t)
                .format("%Y-%m-%dT%H:%M:%SZ")
                .to_string()
        })
        .unwrap_or_default()
}

fn listing(path: &str, include_files: bool, allow_without_slash: bool) -> anyhow::Result<Value> {
    if path.trim().is_empty() || path == "/" {
        return Ok(entries("/", Some("/"), None, include_files));
    }
    let folder = if path.ends_with('/') || allow_without_slash {
        path.to_string()
    } else {
        // Not a folder until the trailing slash: the folder it is in.
        match path.rfind('/') {
            Some(i) => path[..=i].to_string(),
            None => "/".to_string(),
        }
    };
    let trimmed = folder.trim_end_matches('/');
    let parent = match trimmed.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(i) => format!("{}/", &trimmed[..i]),
    };
    if !std::path::Path::new(&folder).is_dir() {
        return Ok(json!({ "parent": parent, "directories": [], "files": [] }));
    }
    Ok(entries(&folder, None, Some(parent), include_files))
}

fn entries(folder: &str, root: Option<&str>, parent: Option<String>, include_files: bool) -> Value {
    let base = if folder.ends_with('/') {
        folder.to_string()
    } else {
        format!("{folder}/")
    };
    let mut directories = Vec::new();
    let mut files = Vec::new();
    if let Ok(read) = std::fs::read_dir(&base) {
        let mut items: Vec<_> = read.flatten().collect();
        items.sort_by_key(|e| e.file_name().to_string_lossy().to_lowercase());
        for e in items {
            let name = e.file_name().to_string_lossy().into_owned();
            let Ok(meta) = e.metadata() else { continue };
            if meta.is_dir() {
                if name.starts_with('.') || (root.is_some() && is_system_folder(&name)) {
                    continue;
                }
                directories.push(json!({
                    "type": "folder",
                    "name": name,
                    "path": format!("{base}{name}/"),
                    "size": 0,
                    "lastModified": modified(&meta),
                }));
            } else if include_files && meta.is_file() {
                files.push(json!({
                    "type": "file",
                    "name": name,
                    "path": format!("{base}{name}"),
                    "extension": unarr_core::mediafiles::extension(&name),
                    "size": meta.len(),
                    "lastModified": modified(&meta),
                }));
            }
        }
    }
    json!({ "parent": parent, "directories": directories, "files": files })
}

/// The root folders the originals hide in the browser on Linux.
fn is_system_folder(name: &str) -> bool {
    matches!(
        name,
        "proc"
            | "sys"
            | "dev"
            | "run"
            | "boot"
            | "lost+found"
            | "snap"
            | "etc"
            | "bin"
            | "sbin"
            | "lib"
            | "lib64"
            | "usr"
    )
}

/// `GET /filesystem/type?path=`.
pub async fn filesystem_type(uri: Uri) -> Json<Value> {
    let path = Params::from_uri(&uri)
        .get("path")
        .map(str::to_string)
        .unwrap_or_default();
    let p = std::path::Path::new(&path);
    let kind = if p.is_file() { "file" } else { "folder" };
    Json(json!({ "type": kind }))
}

/// `GET /filesystem/mediafiles?path=`: the video files under a folder.
pub async fn filesystem_media_files(uri: Uri) -> ApiResult<Json<Value>> {
    let path = Params::from_uri(&uri)
        .get("path")
        .map(str::to_string)
        .unwrap_or_default();
    let out = crate::blocking::run(move || -> anyhow::Result<Value> {
        if !std::path::Path::new(&path).is_dir() {
            return Ok(json!([]));
        }
        let files = unarr_core::mediafiles::scan::video_files(&path, true);
        Ok(Value::Array(
            files
                .iter()
                .map(|f| {
                    json!({
                        "path": f,
                        "relativePath": unarr_core::mediafiles::relative_path(&path, f),
                        "name": unarr_core::mediafiles::file_name(f),
                    })
                })
                .collect(),
        ))
    })
    .await??;
    Ok(Json(out))
}
