//! The importer's events and the grabs on SignalR, as the originals'
//! controllers broadcast them: `episodefile` updated or deleted, the
//! episodes touched, `series` updated with its statistics.
//!
//! An episode goes out under three names, one for each page that lists
//! episodes (`EpisodeControllerWithSignalR` and the controllers built on
//! it): `episode`, `wanted/missing` and `wanted/cutoff`. The calendar is a
//! fourth such controller and its resource is named `episode` too, so the
//! originals send that one twice; Unarr sends it once.

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
        | ImportEvent::UntrackedDownloadCompleted(_)
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

/// The names an episode is announced under.
const EPISODE_NAMES: [&str; 3] = ["wanted/cutoff", "wanted/missing", "episode"];

/// `Handle(EpisodeGrabbedEvent)`: the episodes as they are, marked as
/// grabbed, without their series, file or images.
pub async fn relay_grab(state: &AppState, episode_ids: &[i64]) {
    let ids = episode_ids.to_vec();
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
        let Ok(mut resource) = serde_json::to_value(&r) else {
            continue;
        };
        resource["grabbed"] = json!(true);
        for name in EPISODE_NAMES {
            state
                .hub()
                .broadcast(name, json!({"action": "updated", "resource": resource}));
        }
    }
}

/// The episodes a file came to or left, read again whole: with their
/// series, their file and their images, as the originals' controllers
/// answer `GET` by id.
async fn broadcast_episodes(state: &AppState, ids: &[i64]) {
    let ids = ids.to_vec();
    let resources = state
        .db()
        .call(move |conn| {
            let episodes = store::episodes::list_episodes_by_ids(conn, &ids)?;
            let series_ids: Vec<i64> = episodes.iter().map(|e| e.series_id).collect();
            let catalog = Catalog::load_for(conn, &series_ids)?;
            let file_ids: Vec<i64> = episodes
                .iter()
                .map(|e| e.episode_file_id)
                .filter(|id| *id > 0)
                .collect();
            let files = store::episodes::list_files_by_ids(conn, &file_ids)?;
            let include = Include {
                series: true,
                file: true,
                images: true,
            };
            Ok(episodes
                .iter()
                .map(|e| {
                    let file = files.iter().find(|f| f.id == e.episode_file_id);
                    episode_resource(&catalog, e, file, include)
                })
                .collect::<Vec<_>>())
        })
        .await
        .unwrap_or_default();
    for r in resources {
        for name in EPISODE_NAMES {
            state
                .hub()
                .broadcast(name, json!({"action": "updated", "resource": r}));
        }
    }
}

pub(crate) async fn broadcast_series(state: &AppState, series_id: i64) {
    if let Ok(Some(r)) = series_resource_by_id(state, series_id).await {
        state
            .hub()
            .broadcast("series", json!({"action": "updated", "resource": r}));
    }
}
