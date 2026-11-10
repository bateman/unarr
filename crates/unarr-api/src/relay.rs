//! The importer's events on SignalR, as the originals' controllers
//! broadcast them: `episodefile` updated or deleted, `episode` updated for
//! each episode touched, `series` updated with its statistics.

use serde_json::json;
use unarr_core::mediafiles::{DeleteReason, ImportEvent};
use unarr_core::store;

use crate::routes::episodes::{episode_file_resource, episode_resource, Catalog, Include};
use crate::routes::series::series_resource_by_id;
use crate::state::AppState;

/// `BroadcastResourceChange` for what one event touched.
pub async fn relay_import(state: &AppState, e: &ImportEvent) {
    match e {
        ImportEvent::EpisodeImported(ev) => {
            let episode_ids: Vec<i64> = ev.local.episodes.iter().map(|e| e.id).collect();
            broadcast_episode_file(state, ev.file.id).await;
            broadcast_episodes(state, &episode_ids).await;
            broadcast_series(state, ev.file.series_id).await;
        }
        ImportEvent::EpisodeFileDeleted(ev) => {
            let episode_ids: Vec<i64> = ev.episodes.iter().map(|e| e.id).collect();
            state.hub().broadcast(
                "episodefile",
                json!({"action": "deleted", "resource": {"id": ev.file.id}}),
            );
            broadcast_episodes(state, &episode_ids).await;
            if ev.reason != DeleteReason::Upgrade {
                broadcast_series(state, ev.file.series_id).await;
            }
        }
        ImportEvent::EpisodeFileRenamed(ev) => {
            broadcast_episode_file(state, ev.file.id).await;
        }
        ImportEvent::SeriesRenamed { series_id, .. } => broadcast_series(state, *series_id).await,
        ImportEvent::DownloadCompleted(_)
        | ImportEvent::ManualInteractionRequired(_)
        | ImportEvent::EpisodeImportFailed(_)
        | ImportEvent::SeriesScanned { .. }
        | ImportEvent::SeriesScanSkipped { .. }
        | ImportEvent::DeleteCompleted
        | ImportEvent::RescanRequested { .. } => {}
    }
}

async fn broadcast_episode_file(state: &AppState, file_id: i64) {
    let resource = state
        .db()
        .call(move |conn| {
            let Some(file) = store::episodes::get_file(conn, file_id)? else {
                return Ok(None);
            };
            let catalog = Catalog::load_for(conn, &[file.series_id])?;
            let Some(series) = catalog.series.get(&file.series_id) else {
                return Ok(None);
            };
            Ok(Some(episode_file_resource(&catalog, &file, series)))
        })
        .await;
    if let Ok(Some(r)) = resource {
        state
            .hub()
            .broadcast("episodefile", json!({"action": "updated", "resource": r}));
    }
}

async fn broadcast_episodes(state: &AppState, ids: &[i64]) {
    let ids = ids.to_vec();
    let resources = state
        .db()
        .call(move |conn| {
            let episodes = store::episodes::list_episodes_by_ids(conn, &ids)?;
            let series_ids: Vec<i64> = episodes.iter().map(|e| e.series_id).collect();
            let catalog = Catalog::load_for(conn, &series_ids)?;
            Ok(episodes
                .iter()
                .map(|e| episode_resource(&catalog, e, None, Include::default()))
                .collect::<Vec<_>>())
        })
        .await
        .unwrap_or_default();
    for r in resources {
        state
            .hub()
            .broadcast("episode", json!({"action": "updated", "resource": r}));
    }
}

pub(crate) async fn broadcast_series(state: &AppState, series_id: i64) {
    if let Ok(Some(r)) = series_resource_by_id(state, series_id).await {
        state
            .hub()
            .broadcast("series", json!({"action": "updated", "resource": r}));
    }
}
