//! `RenameEpisodeFileService`: what a series' files would be called under
//! the naming settings, and the renames themselves.

use chrono::{DateTime, Utc};
use rusqlite::Connection;

use super::approve::{self, Failure};
use super::decide::Environment;
use super::events::{self, EpisodeFileRenamed, Events, ImportEvent};
use super::transfer;
use crate::db::Db;
use crate::model::{Episode, EpisodeFile, Series};
use crate::store;

/// `RenameEpisodeFilePreview`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenamePreview {
    pub series_id: i64,
    pub season_number: i64,
    pub episode_numbers: Vec<i64>,
    pub episode_file_id: i64,
    pub existing_path: String,
    pub new_path: String,
}

/// `GetPreviews(series, episodes, files)`: the files whose path would
/// change.
pub fn previews(
    env: &Environment,
    series: &Series,
    episodes: &[Episode],
    files: &[EpisodeFile],
) -> Vec<RenamePreview> {
    let mut out = Vec::new();
    for file in files {
        let in_file: Vec<Episode> = episodes
            .iter()
            .filter(|e| e.episode_file_id == file.id)
            .cloned()
            .collect();
        let current = super::join(&series.path, &file.relative_path);
        if in_file.is_empty() {
            tracing::warn!("File ({current}) is not linked to any episodes");
            continue;
        }
        let formats = crate::decision::formats::file_formats(&env.catalog, file, series);
        let Ok(new_path) = approve::build_file_path(
            env,
            series,
            &in_file,
            file,
            &current,
            super::extension(&current),
            &formats,
            false,
        ) else {
            continue;
        };
        // `PathEquals(..., StringComparison.Ordinal)`: case matters here.
        if current.trim_end_matches('/') != new_path.trim_end_matches('/') {
            out.push(RenamePreview {
                series_id: series.id,
                season_number: in_file[0].season_number,
                episode_numbers: in_file.iter().map(|e| e.episode_number).collect(),
                episode_file_id: file.id,
                existing_path: file.relative_path.clone(),
                new_path: super::relative_path(&series.path, &new_path).to_string(),
            });
        }
    }
    out
}

/// `GetRenamePreviews(seriesId)`: newest season and episode first.
pub fn previews_for_series(
    conn: &Connection,
    env: &Environment,
    series: &Series,
) -> anyhow::Result<Vec<RenamePreview>> {
    let episodes = store::episodes::list_episodes(conn, series.id, None)?;
    let files = store::episodes::list_files(conn, series.id)?;
    let mut out = previews(env, series, &episodes, &files);
    out.sort_by(|a, b| {
        b.season_number
            .cmp(&a.season_number)
            .then(b.episode_numbers.first().cmp(&a.episode_numbers.first()))
    });
    Ok(out)
}

/// `GetRenamePreviews(seriesId, seasonNumber)`.
pub fn previews_for_season(
    conn: &Connection,
    env: &Environment,
    series: &Series,
    season: i64,
) -> anyhow::Result<Vec<RenamePreview>> {
    let episodes = store::episodes::list_episodes(conn, series.id, Some(season))?;
    let files = store::episodes::list_files_by_season(conn, series.id, season)?;
    let mut out = previews(env, series, &episodes, &files);
    out.sort_by(|a, b| b.episode_numbers.first().cmp(&a.episode_numbers.first()));
    Ok(out)
}

/// `RenameFiles(episodeFiles, series)`: each file moved, its row written,
/// the history told; the series' empty folders pruned at the end.
pub fn rename_files(
    db: &Db,
    env: &Environment,
    series: &Series,
    files: Vec<EpisodeFile>,
    now: DateTime<Utc>,
) -> anyhow::Result<(Vec<EpisodeFileRenamed>, Events)> {
    let mut events = Events::default();
    let mut renamed = Vec::new();
    let root_folders = db.call_blocking(|conn| store::catalog::list_root_folders(conn))?;
    for mut file in files {
        let previous_path = super::join(&series.path, &file.relative_path);
        let file_id = file.id;
        let episodes = db.call_blocking(move |conn| store::episodes::list_episodes_by_file(conn, file_id))?;
        tracing::debug!("Renaming episode file: {}", previous_path);
        match approve::move_episode_file(env, &root_folders, series, &episodes, &mut file) {
            Ok(()) => {
                let ev = EpisodeFileRenamed {
                    series: series.clone(),
                    file: file.clone(),
                    episodes,
                    original_path: previous_path.clone(),
                };
                let (file2, ev2) = (file.clone(), ev.clone());
                db.call_blocking(move |conn| {
                    store::episodes::update_file(conn, &file2)?;
                    events::record_renamed(conn, &ev2, now)
                })?;
                tracing::debug!(
                    "Renamed episode file: {}",
                    super::join(&series.path, &file.relative_path)
                );
                renamed.push(ev.clone());
                events.push(ImportEvent::EpisodeFileRenamed(Box::new(ev)));
            }
            Err(Failure::DestinationExists(p)) => {
                tracing::warn!("File not renamed, there is already a file at the destination: {p}");
            }
            Err(Failure::SameFile(p)) => {
                tracing::debug!("File not renamed, source and destination are the same: {p}");
            }
            Err(e) => {
                tracing::error!("Failed to rename file {previous_path}: {}", e.detail());
            }
        }
    }
    if !renamed.is_empty() {
        transfer::remove_empty_subfolders(std::path::Path::new(&series.path));
        events.push(ImportEvent::SeriesRenamed {
            series_id: series.id,
            renamed: renamed.clone(),
        });
    }
    Ok((renamed, events))
}
