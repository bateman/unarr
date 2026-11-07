//! `series` and `series/{id}`.

use std::collections::HashMap;

use axum::extract::{Path, State};
use axum::http::Uri;
use axum::Json;
use unarr_core::model::{SceneMapping, Series};
use unarr_core::store;

use crate::error::{found, ApiResult};
use crate::query::Params;
use crate::resources::{
    best_root_folder_path, series_resource, AlternateTitleResource, SeriesContext, SeriesResource,
};
use crate::state::AppState;

fn alternate_titles(mappings: &[SceneMapping]) -> Vec<AlternateTitleResource> {
    mappings
        .iter()
        .map(|m| AlternateTitleResource {
            title: m.title.clone(),
            season_number: m.season_number,
            scene_season_number: m.scene_season_number,
            scene_origin: m.scene_origin.clone(),
            comment: match (&m.comment, &m.filter_regex) {
                (Some(c), _) if !c.trim().is_empty() => Some(c.clone()),
                (_, Some(r)) if !r.trim().is_empty() => Some("Limited matching".into()),
                _ => None,
            },
        })
        .collect()
}

/// Everything needed to render a set of series, fetched in a few queries.
struct Loaded {
    series: Vec<Series>,
    root_folders: Vec<String>,
    mappings: HashMap<i64, Vec<SceneMapping>>,
    stats: HashMap<i64, Vec<store::stats::SeasonStats>>,
}

async fn load(state: &AppState, id: Option<i64>, tvdb_id: Option<i64>) -> anyhow::Result<Loaded> {
    let now = state.now();
    state
        .db()
        .call(move |conn| {
            let mut series = match id {
                Some(id) => store::series::get(conn, id)?.into_iter().collect(),
                None => store::series::list(conn)?,
            };
            if let Some(tvdb) = tvdb_id {
                series.retain(|s| s.tvdb_id == tvdb);
            }
            let root_folders = store::catalog::list_root_folders(conn)?
                .into_iter()
                .map(|r| r.path)
                .collect();
            let tvdb_ids: Vec<i64> = series.iter().map(|s| s.tvdb_id).collect();
            let mappings = store::scene::by_tvdb_ids(conn, &tvdb_ids)?;
            let stats = store::stats::season_statistics(conn, id, now)?;
            Ok(Loaded {
                series,
                root_folders,
                mappings,
                stats,
            })
        })
        .await
}

fn render(state: &AppState, loaded: &Loaded, include_season_images: bool) -> Vec<SeriesResource> {
    let url_base = &state.settings().url_base;
    loaded
        .series
        .iter()
        .map(|s| {
            series_resource(
                s,
                SeriesContext {
                    url_base,
                    root_folder_path: best_root_folder_path(&s.path, &loaded.root_folders),
                    alternate_titles: loaded
                        .mappings
                        .get(&s.tvdb_id)
                        .map(|m| alternate_titles(m))
                        .unwrap_or_default(),
                    // A series without episodes has no statistics row; the
                    // originals then leave `statistics` out entirely.
                    season_stats: loaded.stats.get(&s.id).map(Vec::as_slice),
                    include_season_images,
                },
            )
        })
        .collect()
}

pub async fn list(State(state): State<AppState>, uri: Uri) -> ApiResult<Json<Vec<SeriesResource>>> {
    let p = Params::from_uri(&uri);
    let loaded = load(&state, None, p.i64("tvdbId")).await?;
    Ok(Json(render(
        &state,
        &loaded,
        p.bool("includeSeasonImages", false),
    )))
}

pub async fn get(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    uri: Uri,
) -> ApiResult<Json<SeriesResource>> {
    let p = Params::from_uri(&uri);
    let loaded = load(&state, Some(id), None).await?;
    let one = render(&state, &loaded, p.bool("includeSeasonImages", false))
        .into_iter()
        .next();
    Ok(Json(found(one)?))
}
