//! `manualimport`, `rename` and the writes on `episodefile`, after the
//! originals' `ManualImportController`, `RenameEpisodeController` and
//! `EpisodeFileController`.

use axum::extract::{Path, State};
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Map, Value};
use unarr_core::commands::{language_ids_from_json, quality_model_from_json, quality_model_json};
use unarr_core::downloads::queue::hash_int31;
use unarr_core::mediafiles::decide::Environment;
use unarr_core::mediafiles::manual::{self, ManualImportError, ManualImportItem, ReprocessRequest};
use unarr_core::mediafiles::{deletion, rename};
use unarr_core::model::{EpisodeFile, ReleaseType};
use unarr_core::parser::title::is_scene_title;
use unarr_core::quality::SONARR_QUALITIES;
use unarr_core::store;

use crate::blocking;
use crate::error::{ApiError, ApiResult};
use crate::query::Params;
use crate::resources::{languages, series_resource_plain};
use crate::routes::episodes::{episode_file_resource, episode_resource, Catalog, Include};
use crate::state::AppState;

async fn environment(state: &AppState) -> ApiResult<Environment> {
    let now = state.now();
    Ok(state.db().call(move |conn| Environment::load(conn, now)).await?)
}

/// `AddQualityWeight`: the definition's weight plus the revision.
fn quality_weight(q: &unarr_core::model::QualityModel) -> i64 {
    let weight = SONARR_QUALITIES
        .iter()
        .find(|d| d.quality.id == q.quality_id)
        .map(|d| d.weight)
        .unwrap_or(0);
    weight + q.real * 10 + q.version
}

fn rejections_json(item: &ManualImportItem) -> Value {
    Value::Array(
        item.rejections
            .iter()
            .map(|r| json!({"reason": r.message, "type": "permanent"}))
            .collect(),
    )
}

/// `ManualImportResource`.
fn manual_import_resource(env: &Environment, catalog: &Catalog, item: &ManualImportItem) -> Value {
    let mut m = Map::new();
    m.insert("id".into(), json!(hash_int31(&item.path)));
    m.insert("path".into(), json!(item.path));
    m.insert("relativePath".into(), json!(item.relative_path));
    if let Some(folder) = &item.folder_name {
        m.insert("folderName".into(), json!(folder));
    }
    if let Some(name) = &item.name {
        m.insert("name".into(), json!(name));
    }
    m.insert("size".into(), json!(item.size));
    if let Some(series) = &item.series {
        m.insert(
            "series".into(),
            serde_json::to_value(series_resource_plain(series)).unwrap_or(Value::Null),
        );
    }
    if let Some(season) = item.season_number {
        m.insert("seasonNumber".into(), json!(season));
    }
    if let Some(episodes) = &item.episodes {
        m.insert(
            "episodes".into(),
            Value::Array(
                episodes
                    .iter()
                    .map(|e| {
                        serde_json::to_value(episode_resource(catalog, e, None, Include::default()))
                            .unwrap_or(Value::Null)
                    })
                    .collect(),
            ),
        );
    }
    if let Some(id) = item.episode_file_id {
        m.insert("episodeFileId".into(), json!(id));
    }
    if let Some(group) = &item.release_group {
        m.insert("releaseGroup".into(), json!(group));
    }
    if let Some(q) = &item.quality {
        m.insert("quality".into(), quality_model_json(q));
    }
    m.insert("languages".into(), json!(languages(&item.languages)));
    m.insert(
        "qualityWeight".into(),
        json!(item.quality.as_ref().map(quality_weight).unwrap_or(0)),
    );
    if let Some(id) = &item.download_id {
        m.insert("downloadId".into(), json!(id));
    }
    m.insert(
        "customFormats".into(),
        Value::Array(
            item.custom_formats
                .iter()
                .map(|f| json!({"id": f.id, "name": f.name}))
                .collect(),
        ),
    );
    m.insert("customFormatScore".into(), json!(item.custom_format_score(env)));
    m.insert("indexerFlags".into(), json!(item.indexer_flags));
    m.insert("releaseType".into(), json!(item.release_type.as_str()));
    m.insert("rejections".into(), rejections_json(item));
    Value::Object(m)
}

fn manual_error(e: ManualImportError) -> ApiError {
    match e {
        ManualImportError::SeriesNotFound(_) => ApiError::ModelNotFound(e.to_string()),
        ManualImportError::Augmenting(m) => ApiError::Internal(anyhow::anyhow!("{m}")),
        ManualImportError::Other(e) => ApiError::Internal(e),
    }
}

/// `GET /manualimport`.
pub async fn manual_import(State(state): State<AppState>, uri: Uri) -> ApiResult<Json<Value>> {
    let p = Params::from_uri(&uri);
    let env = environment(&state).await?;
    let series_id = p.i64("seriesId");
    let season = p.i64("seasonNumber");
    let folder = p.get("folder").map(str::to_string);
    let download_id = p.get("downloadId").map(str::to_string);
    let filter_existing = p.bool("filterExistingFiles", true);
    let (db, downloads, env2) = (state.db().clone(), state.downloads().clone(), env.clone());
    let items = blocking::run(move || match series_id {
        Some(id) => manual::get_media_files_for_series(&db, &env2, id, season),
        None => Ok(manual::get_media_files(
            &db,
            &env2,
            &downloads,
            folder.as_deref(),
            download_id.as_deref(),
            None,
            filter_existing,
        )),
    })
    .await?
    .map_err(manual_error)?;
    let series_ids: Vec<i64> = items
        .iter()
        .filter_map(|i| i.series.as_ref().map(|s| s.id))
        .collect();
    let catalog = state
        .db()
        .call(move |conn| Catalog::load_for(conn, &series_ids))
        .await?;
    Ok(Json(Value::Array(
        items
            .iter()
            .map(|i| manual_import_resource(&env, &catalog, i))
            .collect(),
    )))
}

/// `POST /manualimport`: `ReprocessItems`.
pub async fn reprocess(State(state): State<AppState>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let env = environment(&state).await?;
    let items = body.as_array().cloned().unwrap_or_default();
    let mut out = Vec::new();
    for item in items {
        let req = ReprocessRequest {
            path: item["path"].as_str().unwrap_or("").to_string(),
            download_id: item["downloadId"].as_str().map(str::to_string),
            series_id: item["seriesId"].as_i64().unwrap_or(0),
            season_number: item["seasonNumber"].as_i64(),
            episode_ids: item["episodeIds"]
                .as_array()
                .map(|a| a.iter().filter_map(Value::as_i64).collect())
                .unwrap_or_default(),
            release_group: item["releaseGroup"].as_str().map(str::to_string),
            quality: quality_model_from_json(&item["quality"]),
            languages: language_ids_from_json(&item["languages"]),
            indexer_flags: item["indexerFlags"].as_i64().unwrap_or(0),
            release_type: item["releaseType"]
                .as_str()
                .and_then(ReleaseType::parse)
                .unwrap_or_default(),
        };
        let (db, downloads, env2) = (state.db().clone(), state.downloads().clone(), env.clone());
        let processed = blocking::run(move || manual::reprocess_item(&db, &env2, &downloads, &req))
            .await?
            .map_err(manual_error)?;
        let series_ids: Vec<i64> = processed.series.iter().map(|s| s.id).collect();
        let catalog = state
            .db()
            .call(move |conn| Catalog::load_for(conn, &series_ids))
            .await?;
        let mut m = item.as_object().cloned().unwrap_or_default();
        match processed.season_number {
            Some(s) => m.insert("seasonNumber".into(), json!(s)),
            None => m.insert("seasonNumber".into(), Value::Null),
        };
        m.insert(
            "episodes".into(),
            Value::Array(
                processed
                    .episodes
                    .clone()
                    .unwrap_or_default()
                    .iter()
                    .map(|e| {
                        serde_json::to_value(episode_resource(&catalog, e, None, Include::default()))
                            .unwrap_or(Value::Null)
                    })
                    .collect(),
            ),
        );
        m.insert("releaseType".into(), json!(processed.release_type.as_str()));
        m.insert("indexerFlags".into(), json!(processed.indexer_flags));
        m.insert("rejections".into(), rejections_json(&processed));
        m.insert(
            "customFormats".into(),
            Value::Array(
                processed
                    .custom_formats
                    .iter()
                    .map(|f| json!({"id": f.id, "name": f.name}))
                    .collect(),
            ),
        );
        m.insert(
            "customFormatScore".into(),
            json!(processed.custom_format_score(&env)),
        );
        let given_languages = language_ids_from_json(&item["languages"]);
        let unknown_only = given_languages.len() <= 1 && given_languages.first().copied().unwrap_or(0) == 0;
        if unknown_only && !processed.languages.is_empty() {
            m.insert("languages".into(), json!(languages(&processed.languages)));
        }
        if item["quality"]["quality"]["id"].as_i64().unwrap_or(0) == 0 {
            if let Some(q) = &processed.quality {
                m.insert("quality".into(), quality_model_json(q));
            }
        }
        if item["releaseGroup"]
            .as_str()
            .is_some_and(|g| !g.trim().is_empty())
        {
            match &processed.release_group {
                Some(g) => m.insert("releaseGroup".into(), json!(g)),
                None => m.insert("releaseGroup".into(), Value::Null),
            };
        }
        m.remove("episodeIds");
        out.push(Value::Object(m));
    }
    Ok(Json(Value::Array(out)))
}

/// `GET /rename?seriesId=&seasonNumber=`.
pub async fn rename_preview(State(state): State<AppState>, uri: Uri) -> ApiResult<Json<Value>> {
    let p = Params::from_uri(&uri);
    let series_id = p.i64("seriesId").unwrap_or(0);
    let season = p.i64("seasonNumber");
    let now = state.now();
    let previews = state
        .db()
        .call(move |conn| {
            let env = Environment::load(conn, now)?;
            let Some(series) = store::series::get(conn, series_id)? else {
                return Ok(None);
            };
            Ok(Some(match season {
                Some(s) => rename::previews_for_season(conn, &env, &series, s)?,
                None => rename::previews_for_series(conn, &env, &series)?,
            }))
        })
        .await?
        .ok_or_else(|| ApiError::ModelNotFound(format!("Series with ID {series_id} does not exist")))?;
    Ok(Json(Value::Array(
        previews
            .iter()
            .map(|r| {
                json!({
                    "seriesId": r.series_id,
                    "seasonNumber": r.season_number,
                    "episodeNumbers": r.episode_numbers,
                    "episodeFileId": r.episode_file_id,
                    "existingPath": r.existing_path,
                    "newPath": r.new_path,
                })
            })
            .collect(),
    )))
}

/// The edits `PUT /episodefile/{id}`, `/editor` and `/bulk` apply to a
/// file from a body, as the originals read it.
fn apply_edits(file: &mut EpisodeFile, body: &Value, with_flags: bool) {
    if body.get("languages").is_some_and(|l| !l.is_null()) {
        file.languages = language_ids_from_json(&body["languages"]);
    }
    if body.get("quality").is_some_and(|q| !q.is_null()) {
        file.quality = quality_model_from_json(&body["quality"]);
    }
    if let Some(scene) = body["sceneName"].as_str() {
        if is_scene_title(scene) {
            file.scene_name = Some(scene.to_string());
        }
    }
    if let Some(group) = body["releaseGroup"].as_str() {
        file.release_group = Some(group.to_string());
    }
    if with_flags {
        if let Some(flags) = body["indexerFlags"].as_i64() {
            file.indexer_flags = flags;
        }
        if let Some(rt) = body["releaseType"].as_str().and_then(ReleaseType::parse) {
            file.release_type = rt;
        }
    }
}

fn accepted(body: Value) -> Response {
    (StatusCode::ACCEPTED, Json(body)).into_response()
}

async fn file_resources(state: &AppState, ids: Vec<i64>) -> ApiResult<Vec<Value>> {
    Ok(state
        .db()
        .call(move |conn| {
            let files = store::episodes::list_files_by_ids(conn, &ids)?;
            let catalog = Catalog::load_for(conn, &files.iter().map(|f| f.series_id).collect::<Vec<_>>())?;
            Ok(files
                .iter()
                .filter_map(|f| {
                    catalog.series.get(&f.series_id).map(|s| {
                        serde_json::to_value(episode_file_resource(&catalog, f, s)).unwrap_or(Value::Null)
                    })
                })
                .collect::<Vec<_>>())
        })
        .await?)
}

/// `PUT /episodefile/{id}`: `SetQuality`.
pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    let body2 = body.clone();
    let found = state
        .db()
        .call(move |conn| {
            let Some(mut file) = store::episodes::get_file(conn, id)? else {
                return Ok(false);
            };
            // The quality is always taken, as the originals assign it outright.
            file.quality = quality_model_from_json(&body2["quality"]);
            apply_edits(&mut file, &body2, false);
            store::episodes::update_file(conn, &file)?;
            Ok(true)
        })
        .await?;
    if !found {
        return Err(ApiError::ModelNotFound(format!(
            "EpisodeFile with ID {id} does not exist"
        )));
    }
    let mut resources = file_resources(&state, vec![id]).await?;
    Ok(accepted(resources.pop().unwrap_or(Value::Null)))
}

/// `PUT /episodefile/editor`.
pub async fn editor(State(state): State<AppState>, Json(body): Json<Value>) -> ApiResult<Response> {
    let ids: Vec<i64> = body["episodeFileIds"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default();
    let (ids2, body2) = (ids.clone(), body.clone());
    state
        .db()
        .call(move |conn| {
            for mut file in store::episodes::list_files_by_ids(conn, &ids2)? {
                apply_edits(&mut file, &body2, false);
                store::episodes::update_file(conn, &file)?;
            }
            Ok(())
        })
        .await?;
    Ok(accepted(Value::Array(file_resources(&state, ids).await?)))
}

/// `PUT /episodefile/bulk`: `SetPropertiesBulk`.
pub async fn bulk_update(State(state): State<AppState>, Json(body): Json<Value>) -> ApiResult<Response> {
    let items = body.as_array().cloned().unwrap_or_default();
    let ids: Vec<i64> = items.iter().filter_map(|i| i["id"].as_i64()).collect();
    let (ids2, items2) = (ids.clone(), items.clone());
    state
        .db()
        .call(move |conn| {
            for mut file in store::episodes::list_files_by_ids(conn, &ids2)? {
                if let Some(item) = items2.iter().find(|i| i["id"].as_i64() == Some(file.id)) {
                    apply_edits(&mut file, item, true);
                    store::episodes::update_file(conn, &file)?;
                }
            }
            Ok(())
        })
        .await?;
    Ok(accepted(Value::Array(file_resources(&state, ids).await?)))
}

/// `MediaFileDeletionService.DeleteEpisodeFile` for one file, with the
/// originals' status codes.
async fn delete_one(state: &AppState, id: i64) -> ApiResult<()> {
    let now = state.now();
    let (env, series, file) = state
        .db()
        .call(move |conn| {
            let Some(file) = store::episodes::get_file(conn, id)? else {
                return Ok(None);
            };
            let series = store::series::get(conn, file.series_id)?;
            Ok(Some((Environment::load(conn, now)?, series, file)))
        })
        .await?
        .ok_or_else(|| ApiError::ModelNotFound(format!("EpisodeFile with ID {id} does not exist")))?;
    let series = series.ok_or_else(|| {
        ApiError::ModelNotFound(format!("Series with ID {} does not exist", file.series_id))
    })?;
    let db = state.db().clone();
    let result =
        blocking::run(move || deletion::delete_episode_file(&db, &env.config, &series, &file, now)).await?;
    match result {
        Ok(events) => {
            if let Ok(runtime) = state.commands() {
                runtime.publish_import_events(events.into_vec()).await;
            }
            Ok(())
        }
        Err(e) => Err(match e.status() {
            409 => ApiError::ConflictMessage(e.to_string()),
            _ => ApiError::ServerMessage(e.to_string()),
        }),
    }
}

/// `DELETE /episodefile/{id}`.
pub async fn delete(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Response> {
    delete_one(&state, id).await?;
    Ok(StatusCode::OK.into_response())
}

/// `DELETE /episodefile/bulk`.
pub async fn bulk_delete(State(state): State<AppState>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let ids: Vec<i64> = body["episodeFileIds"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default();
    let ids2 = ids.clone();
    let existing: Vec<i64> = state
        .db()
        .call(move |conn| {
            Ok(store::episodes::list_files_by_ids(conn, &ids2)?
                .into_iter()
                .map(|f| f.id)
                .collect())
        })
        .await?;
    for id in existing {
        delete_one(&state, id).await?;
    }
    Ok(Json(json!({})))
}
