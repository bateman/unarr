//! `ManualImportService`: what the interactive import shows for a folder,
//! a download or a series, how an item is re-evaluated when the user
//! changes it, and the `ManualImport` command that imports the choices.

use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::Connection;

use super::aggregate;
use super::approve;
use super::decide::{self, Environment, Probes};
use super::downloaded;
use super::events::{self, DownloadCompleted, Events, ImportEvent, UntrackedDownloadCompleted};
use super::local::{
    ImportDecision, ImportMode, ImportRejection, ImportResultType, LocalEpisode, RejectionReason,
};
use super::scan;
use crate::db::Db;
use crate::decision::lookup::StoreLookup;
use crate::decision::mapping;
use crate::downloadclients::DownloadClientItem;
use crate::downloads::{Downloads, TrackedDownload, TrackedDownloadState};
use crate::model::{CustomFormat, Episode, EpisodeFile, QualityModel, ReleaseType, Series};
use crate::parser;
use crate::parser::language::{self, lang};
use crate::store;

/// `ManualImportItem`.
#[derive(Debug, Clone)]
pub struct ManualImportItem {
    pub path: String,
    pub relative_path: String,
    pub folder_name: Option<String>,
    /// `None` for the library's own files, whose `Path` the originals
    /// never fill.
    pub name: Option<String>,
    pub size: i64,
    pub series: Option<Series>,
    pub season_number: Option<i64>,
    /// `None` where the originals leave the list null.
    pub episodes: Option<Vec<Episode>>,
    pub episode_file_id: Option<i64>,
    /// `None` when the file could not be parsed at all.
    pub quality: Option<QualityModel>,
    pub languages: Vec<i64>,
    pub release_group: Option<String>,
    pub download_id: Option<String>,
    pub custom_formats: Vec<CustomFormat>,
    pub indexer_flags: i64,
    pub release_type: ReleaseType,
    pub rejections: Vec<ImportRejection>,
}

impl ManualImportItem {
    fn bare(path: &str, root_folder: &str, download_id: Option<&str>) -> ManualImportItem {
        ManualImportItem {
            path: path.to_string(),
            relative_path: super::relative_path(root_folder, path).to_string(),
            folder_name: None,
            name: Some(super::file_stem(path).to_string()),
            size: file_size(path),
            series: None,
            season_number: None,
            episodes: None,
            episode_file_id: None,
            quality: None,
            languages: Vec::new(),
            release_group: None,
            download_id: download_id.map(String::from),
            custom_formats: Vec::new(),
            indexer_flags: 0,
            release_type: ReleaseType::Unknown,
            rejections: Vec::new(),
        }
    }

    /// `CalculateCustomFormatScore` under the series' profile.
    pub fn custom_format_score(&self, env: &Environment) -> i64 {
        self.series
            .as_ref()
            .and_then(|s| env.catalog.profile(s.quality_profile_id))
            .map(|p| crate::decision::upgradable::format_score(p, &self.custom_formats))
            .unwrap_or(0)
    }
}

/// `ManualImportFile`: one choice of the `ManualImport` command.
#[derive(Debug, Clone, PartialEq)]
pub struct ManualImportFile {
    pub path: String,
    pub folder_name: Option<String>,
    pub series_id: i64,
    pub episode_ids: Vec<i64>,
    pub episode_file_id: Option<i64>,
    pub quality: QualityModel,
    pub languages: Vec<i64>,
    pub release_group: Option<String>,
    pub indexer_flags: i64,
    pub release_type: ReleaseType,
    pub download_id: Option<String>,
}

/// What `ReprocessItem` was asked.
#[derive(Debug, Clone)]
pub struct ReprocessRequest {
    pub path: String,
    pub download_id: Option<String>,
    pub series_id: i64,
    pub season_number: Option<i64>,
    pub episode_ids: Vec<i64>,
    pub release_group: Option<String>,
    pub quality: QualityModel,
    pub languages: Vec<i64>,
    pub indexer_flags: i64,
    pub release_type: ReleaseType,
}

#[derive(Debug, thiserror::Error)]
pub enum ManualImportError {
    #[error("Series with ID {0} does not exist")]
    SeriesNotFound(i64),
    #[error("{0}")]
    Augmenting(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

fn file_size(path: &str) -> i64 {
    std::fs::metadata(path).map(|m| m.len() as i64).unwrap_or(0)
}

/// `SceneSource(series, folder)`: anything outside the series folder.
fn scene_source(series: &Series, folder: &str) -> bool {
    !(super::path_equals(&series.path, folder) || super::is_parent_path(&series.path, folder))
}

fn tracked_for(downloads: &Downloads, download_id: Option<&str>) -> Option<TrackedDownload> {
    download_id
        .filter(|d| !d.trim().is_empty())
        .and_then(|d| downloads.tracked(d))
}

/// `MapItem(decision, rootFolder, downloadId, folderName)`.
fn map_decision(
    env: &Environment,
    decision: ImportDecision,
    root_folder: &str,
    download_id: Option<&str>,
    folder_name: Option<&str>,
) -> ManualImportItem {
    let local = decision
        .local_episode
        .expect("a manual import decision carries its file");
    let mut item = ManualImportItem::bare(&local.path, root_folder, download_id);
    item.folder_name = folder_name.map(String::from);
    let mut seasons: Vec<i64> = local.episodes.iter().map(|e| e.season_number).collect();
    seasons.sort_unstable();
    seasons.dedup();
    if !local.episodes.is_empty() {
        match seasons.len() {
            1 => {
                item.season_number = Some(seasons[0]);
                item.episodes = Some(local.episodes.clone());
            }
            n => tracing::warn!(
                "Expected one season, but found {n} ({}) for: {}",
                seasons
                    .iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
                local.path
            ),
        }
    }
    item.release_group = local.release_group.clone();
    // The originals' quality is null only when nothing could be parsed.
    let unparsed = decision
        .rejections
        .first()
        .is_some_and(|r| r.reason == RejectionReason::UnableToParse);
    item.quality = (!unparsed).then_some(local.quality);
    item.languages = local.languages.clone();
    item.size = file_size(&local.path);
    item.rejections = decision.rejections;
    item.indexer_flags = local.indexer_flags;
    item.release_type = local.release_type;
    if local.series.is_some() {
        item.custom_formats = decide::local_formats(&env.catalog, &local);
        item.series = local.series;
    }
    item
}

/// `MapItem(episodeFile, series, folderName, episodes)`.
fn map_file(
    env: &Environment,
    file: &EpisodeFile,
    series: &Series,
    folder_name: &str,
    episodes: &[Episode],
) -> ManualImportItem {
    let path = super::join(&series.path, &file.relative_path);
    let mut item = ManualImportItem::bare(&path, &series.path, None);
    item.relative_path = file.relative_path.clone();
    item.name = None;
    item.folder_name = Some(folder_name.to_string());
    item.series = Some(series.clone());
    item.season_number = Some(file.season_number);
    item.episodes = Some(
        episodes
            .iter()
            .filter(|e| e.episode_file_id == file.id)
            .cloned()
            .collect(),
    );
    item.release_group = file.release_group.clone();
    item.quality = Some(file.quality);
    item.languages = file.languages.clone();
    item.indexer_flags = file.indexer_flags;
    item.release_type = file.release_type;
    item.episode_file_id = Some(file.id);
    item.custom_formats = crate::decision::formats::file_formats(&env.catalog, file, series);
    item
}

/// `GetMediaFiles(seriesId, seasonNumber)`: the series' files, plus the
/// video files in its folder the library does not know (no season given).
pub fn get_media_files_for_series(
    db: &Db,
    env: &Environment,
    series_id: i64,
    season: Option<i64>,
) -> Result<Vec<ManualImportItem>, ManualImportError> {
    let (series, files, episodes) = db.call_blocking(move |conn| {
        let series = store::series::get(conn, series_id)?;
        let files = match season {
            Some(s) => store::episodes::list_files_by_season(conn, series_id, s)?,
            None => store::episodes::list_files(conn, series_id)?,
        };
        let episodes = store::episodes::list_episodes(conn, series_id, None)?;
        Ok((series, files, episodes))
    })?;
    let series = series.ok_or(ManualImportError::SeriesNotFound(series_id))?;
    let folder_name = super::file_name(&series.path).to_string();
    let mut items: Vec<ManualImportItem> = files
        .iter()
        .map(|f| map_file(env, f, &series, &folder_name, &episodes))
        .collect();
    if season.is_none() {
        let media = scan::filter_paths(&series.path, &scan::video_files(&series.path, true), true);
        for file in decide::filter_existing(&media, &files, &series) {
            let mut item = ManualImportItem::bare(&file, &series.path, None);
            item.folder_name = Some(folder_name.clone());
            item.series = Some(series.clone());
            item.episodes = Some(Vec::new());
            item.release_group = Some(String::new());
            item.quality = Some(QualityModel::default());
            item.languages = vec![lang::UNKNOWN];
            items.push(item);
        }
    }
    Ok(items)
}

/// `GetMediaFiles(path, downloadId, seriesId, filterExistingFiles)`.
pub fn get_media_files(
    db: &Db,
    env: &Environment,
    downloads: &Downloads,
    path: Option<&str>,
    download_id: Option<&str>,
    series_id: Option<i64>,
    filter_existing: bool,
) -> Vec<ManualImportItem> {
    let mut path = path.unwrap_or("").to_string();
    let download_id = download_id.filter(|d| !d.trim().is_empty());
    if let Some(id) = download_id {
        let Some(tracked) = downloads.tracked(id) else {
            return Vec::new();
        };
        path = tracked
            .import_item
            .as_ref()
            .or(Some(&tracked.item))
            .and_then(|i| i.output_path.clone())
            .unwrap_or_default();
    }
    if !Path::new(&path).is_dir() {
        if !Path::new(&path).is_file() {
            return Vec::new();
        }
        let root = super::parent_path(&path).unwrap_or("").to_string();
        return process_file(db, env, downloads, &root, &root, &path, download_id, None)
            .into_iter()
            .collect();
    }
    process_folder(
        db,
        env,
        downloads,
        &path,
        &path,
        download_id,
        series_id,
        filter_existing,
    )
}

/// `ProcessFolder(rootFolder, baseFolder, downloadId, seriesId, filterExistingFiles)`.
#[allow(clippy::too_many_arguments)]
fn process_folder(
    db: &Db,
    env: &Environment,
    downloads: &Downloads,
    root_folder: &str,
    base_folder: &str,
    download_id: Option<&str>,
    series_id: Option<i64>,
    filter_existing: bool,
) -> Vec<ManualImportItem> {
    let folder_name = super::file_name(base_folder).to_string();
    let mut series: Option<Series> = match series_id {
        Some(id) => db
            .call_blocking(move |conn| store::series::get(conn, id))
            .ok()
            .flatten(),
        None => {
            let name = folder_name.clone();
            db.call_blocking(move |conn| {
                let lookup = StoreLookup::new(conn);
                Ok(match mapping::get_series(&lookup, &name) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!("Unable to find series from title: {e:?}");
                        None
                    }
                })
            })
            .ok()
            .flatten()
        }
    };
    let tracked = tracked_for(downloads, download_id);
    let item: Option<DownloadClientItem> = tracked.as_ref().map(|t| t.item.clone());
    if series.is_none() {
        series = tracked
            .as_ref()
            .and_then(|t| t.remote.as_ref())
            .and_then(|r| r.series.clone());
    }
    let Some(series) = series else {
        let files = scan::filter_paths(root_folder, &scan::video_files(base_folder, false), true);
        if files.len() > 100 {
            tracing::warn!(
                "Unable to determine series from folder name and found more than 100 files. Skipping parsing"
            );
            return process_download_directory(root_folder, &files);
        }
        let subfolders: Vec<String> = super::transfer::directories_under(Path::new(base_folder))
            .into_iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        let mut subfolders = scan::filter_paths(root_folder, &subfolders, true);
        subfolders.sort();
        let mut out: Vec<ManualImportItem> = files
            .iter()
            .filter_map(|f| process_file(db, env, downloads, root_folder, base_folder, f, download_id, None))
            .collect();
        for sub in subfolders {
            out.extend(process_folder(
                db,
                env,
                downloads,
                root_folder,
                &sub,
                download_id,
                None,
                filter_existing,
            ));
        }
        return out;
    };
    let folder_info = parser::parse_title(&folder_name);
    let files = scan::filter_paths(root_folder, &scan::video_files(base_folder, true), true);
    let probes = Probes::probe_all(&files);
    let scene = scene_source(&series, base_folder);
    let (env2, series2, item2, folder_info2, files2) = (
        env.clone(),
        series.clone(),
        item.clone(),
        folder_info.clone(),
        files.clone(),
    );
    let decisions = db
        .call_blocking(move |conn| {
            decide::get_import_decisions(
                conn,
                &env2,
                &probes,
                &files2,
                &series2,
                item2.as_ref(),
                folder_info2.as_ref(),
                scene,
                filter_existing,
                None,
            )
        })
        .unwrap_or_default();
    decisions
        .into_iter()
        .map(|d| map_decision(env, d, root_folder, download_id, Some(&folder_name)))
        .collect()
}

/// `ProcessFile(rootFolder, baseFolder, file, downloadId, series)`.
#[allow(clippy::too_many_arguments)]
fn process_file(
    db: &Db,
    env: &Environment,
    downloads: &Downloads,
    root_folder: &str,
    base_folder: &str,
    file: &str,
    download_id: Option<&str>,
    series: Option<Series>,
) -> Option<ManualImportItem> {
    let tracked = tracked_for(downloads, download_id);
    let relative = super::relative_path(base_folder, file).to_string();
    let mut series = series;
    if series.is_none() {
        let rel = relative.clone();
        series = db
            .call_blocking(move |conn| {
                let lookup = StoreLookup::new(conn);
                Ok(mapping::get_series(&lookup, &rel).ok().flatten())
            })
            .ok()
            .flatten();
    }
    if series.is_none() {
        series = tracked
            .as_ref()
            .and_then(|t| t.remote.as_ref())
            .and_then(|r| r.series.clone());
    }
    if series.is_none() {
        if let Some(parsed) = parser::parse_path(&relative) {
            let clean = parser::title::clean_series_title(&parsed.series_title);
            series = db
                .call_blocking(move |conn| {
                    Ok(store::series::find_by_clean_title(conn, &clean)?
                        .into_iter()
                        .next())
                })
                .ok()
                .flatten();
        }
    }
    let Some(series) = series else {
        let mut local = LocalEpisode {
            path: file.to_string(),
            release_group: parser::title::parse_release_group(file),
            quality: parser::quality::parse_quality(file).quality,
            languages: language::parse_languages(file),
            ..Default::default()
        };
        local.size = file_size(file);
        let decision = ImportDecision::rejected(
            Some(local),
            ImportRejection::new(RejectionReason::UnknownSeries, "Unknown Series"),
        );
        return Some(map_decision(env, decision, root_folder, download_id, None));
    };
    let probes = Probes::probe_all(std::slice::from_ref(&file.to_string()));
    let scene = scene_source(&series, base_folder);
    let item = tracked.as_ref().map(|t| t.item.clone());
    let (env2, series2, file2) = (env.clone(), series.clone(), file.to_string());
    let decisions = db
        .call_blocking(move |conn| {
            decide::get_import_decisions(
                conn,
                &env2,
                &probes,
                &[file2],
                &series2,
                item.as_ref(),
                None,
                scene,
                false,
                None,
            )
        })
        .unwrap_or_default();
    match decisions.into_iter().next() {
        Some(d) => Some(map_decision(env, d, root_folder, download_id, None)),
        None => Some(ManualImportItem::bare(file, root_folder, download_id)),
    }
}

/// `ProcessDownloadDirectory`: too many files to parse; the bare list.
fn process_download_directory(root_folder: &str, files: &[String]) -> Vec<ManualImportItem> {
    files
        .iter()
        .map(|f| {
            let mut item = ManualImportItem::bare(f, root_folder, None);
            item.quality = Some(QualityModel::default());
            item.languages = vec![lang::UNKNOWN];
            item
        })
        .collect()
}

fn unknown_only(languages: &[i64]) -> bool {
    languages.len() <= 1 && languages.first().copied().unwrap_or(lang::UNKNOWN) == lang::UNKNOWN
}

/// `ReprocessItem(...)`: the item evaluated again with the user's choices.
pub fn reprocess_item(
    db: &Db,
    env: &Environment,
    downloads: &Downloads,
    req: &ReprocessRequest,
) -> Result<ManualImportItem, ManualImportError> {
    let root_folder = super::parent_path(&req.path).unwrap_or("").to_string();
    let series_id = req.series_id;
    let series = db
        .call_blocking(move |conn| store::series::get(conn, series_id))?
        .ok_or(ManualImportError::SeriesNotFound(series_id))?;
    let mut language_parse = language::parse_languages(&req.path);
    if unknown_only(&language_parse) {
        language_parse = vec![series.original_language_id];
        tracing::debug!(
            "Language couldn't be parsed from release, falling back to series original language: {}",
            series.original_language_id
        );
    }
    let tracked = tracked_for(downloads, req.download_id.as_deref());
    let item = tracked.as_ref().map(|t| t.item.clone());
    let release_group = match req.release_group.as_deref().filter(|g| !g.trim().is_empty()) {
        Some(g) => Some(g.to_string()),
        None => parser::title::parse_release_group(&req.path),
    };
    let quality = if req.quality.quality_id == 0 {
        parser::quality::parse_quality(&req.path).quality
    } else {
        req.quality
    };
    if !req.episode_ids.is_empty() {
        let ids = req.episode_ids.clone();
        let episodes = db.call_blocking(move |conn| store::episodes::list_episodes_by_ids(conn, &ids))?;
        let languages = if unknown_only(&req.languages) {
            language_parse
        } else {
            req.languages.clone()
        };
        let mut local = LocalEpisode {
            series: Some(series.clone()),
            episodes: episodes.clone(),
            file_episode_info: parser::parse_path(&req.path),
            download_client_episode_info: item.as_ref().and_then(|i| parser::parse_title(&i.title)),
            download_item: item.clone(),
            path: req.path.clone(),
            scene_source: scene_source(&series, &root_folder),
            existing_file: super::is_parent_path(&series.path, &req.path),
            size: file_size(&req.path),
            release_group: release_group.clone(),
            languages: languages.clone(),
            quality,
            indexer_flags: req.indexer_flags,
            release_type: req.release_type,
            ..Default::default()
        };
        decide::score_formats(&env.catalog, &mut local);
        let probes = Probes::probe_all(std::slice::from_ref(&req.path));
        let media_info = probes.get(&req.path);
        let env2 = env.clone();
        let (indexer_flags, release_type) = (req.indexer_flags, req.release_type);
        let decision = db.call_blocking(move |conn| {
            let lookup = StoreLookup::new(conn);
            if let Err(e) = aggregate::augment(conn, &lookup, &mut local, media_info) {
                return Ok(Err(e.0));
            }
            // The user's choices stand.
            local.series = Some(series);
            local.episodes = episodes;
            local.release_group = release_group;
            local.quality = quality;
            local.languages = languages;
            local.indexer_flags = indexer_flags;
            local.release_type = release_type;
            Ok(Ok(decide::specs_only(&lookup, &env2, local, item.as_ref())))
        })?;
        let decision = decision.map_err(ManualImportError::Augmenting)?;
        return Ok(map_decision(
            env,
            decision,
            &root_folder,
            req.download_id.as_deref(),
            None,
        ));
    }
    if req.season_number.is_some() {
        let local = LocalEpisode {
            series: Some(series.clone()),
            episodes: Vec::new(),
            file_episode_info: parser::parse_path(&req.path),
            download_client_episode_info: item.as_ref().and_then(|i| parser::parse_title(&i.title)),
            download_item: item,
            path: req.path.clone(),
            scene_source: scene_source(&series, &root_folder),
            existing_file: super::is_parent_path(&series.path, &req.path),
            size: file_size(&req.path),
            release_group,
            languages: if unknown_only(&req.languages) {
                language::parse_languages(&req.path)
            } else {
                req.languages.clone()
            },
            quality,
            indexer_flags: req.indexer_flags,
            release_type: req.release_type,
            ..Default::default()
        };
        let decision = ImportDecision::rejected(
            Some(local),
            ImportRejection::new(RejectionReason::NoEpisodes, "Episodes not selected"),
        );
        return Ok(map_decision(
            env,
            decision,
            &root_folder,
            req.download_id.as_deref(),
            None,
        ));
    }
    Ok(process_file(
        db,
        env,
        downloads,
        &root_folder,
        &root_folder,
        &req.path,
        req.download_id.as_deref(),
        Some(series),
    )
    .unwrap_or_else(|| ManualImportItem::bare(&req.path, &root_folder, req.download_id.as_deref())))
}

/// What `Execute(ManualImportCommand)` leaves for the runtime: the events,
/// and the tracked downloads it marked imported.
#[derive(Debug, Default)]
pub struct ManualImportOutcome {
    pub events: Events,
    pub completed: Vec<TrackedDownload>,
}

/// `Execute(ManualImportCommand)`.
pub fn execute(
    db: &Db,
    env: &Environment,
    downloads: &Downloads,
    files: &[ManualImportFile],
    mode: ImportMode,
    now: DateTime<Utc>,
    progress: &mut dyn FnMut(String),
) -> Result<ManualImportOutcome, ManualImportError> {
    progress(format!(
        "Manually importing {} files using mode {}",
        files.len(),
        mode.as_str()
    ));
    let mut outcome = ManualImportOutcome::default();
    let mut imported: Vec<super::local::ImportResult> = Vec::new();
    // `(tracked download, result)` for the files of a tracked download.
    let mut tracked_results: Vec<(TrackedDownload, super::local::ImportResult)> = Vec::new();
    // Where in `imported` each of those results is.
    let mut tracked_at: Vec<usize> = Vec::new();
    for (i, file) in files.iter().enumerate() {
        progress(format!("Processing file {} of {}", i + 1, files.len()));
        let (series_id, ids) = (file.series_id, file.episode_ids.clone());
        let (series, episodes) = db.call_blocking(move |conn| {
            Ok((
                store::series::get(conn, series_id)?,
                store::episodes::list_episodes_by_ids(conn, &ids)?,
            ))
        })?;
        let series = series.ok_or(ManualImportError::SeriesNotFound(series_id))?;
        let existing_file = super::is_parent_path(&series.path, &file.path);
        let tracked = tracked_for(downloads, file.download_id.as_deref());
        let mut local = LocalEpisode {
            existing_file,
            episodes: episodes.clone(),
            file_episode_info: Some(parser::parse_path(&file.path).unwrap_or_default()),
            path: file.path.clone(),
            release_group: file.release_group.clone(),
            quality: file.quality,
            languages: file.languages.clone(),
            indexer_flags: file.indexer_flags,
            release_type: file.release_type,
            series: Some(series.clone()),
            size: 0,
            ..Default::default()
        };
        if let Some(t) = &tracked {
            local.download_client_episode_info = t.remote.as_ref().and_then(|r| r.parsed.clone());
            local.download_item = Some(t.item.clone());
        }
        if let Some(folder) = file.folder_name.as_deref().filter(|f| !f.trim().is_empty()) {
            local.folder_episode_info = parser::parse_title(folder);
            local.scene_source = !existing_file;
        }
        let probes = Probes::probe_all(std::slice::from_ref(&file.path));
        let media_info = probes.get(&file.path);
        let (file2, series2, episodes2) = (file.clone(), series.clone(), episodes.clone());
        let catalog = env.catalog.clone();
        let local = db.call_blocking(move |conn| {
            let lookup = StoreLookup::new(conn);
            if let Err(e) = aggregate::augment(conn, &lookup, &mut local, media_info) {
                return Ok(Err(e.0));
            }
            // The user's choices stand.
            local.series = Some(series2);
            local.episodes = episodes2;
            local.release_group = file2.release_group;
            local.quality = file2.quality;
            local.languages = file2.languages;
            local.indexer_flags = file2.indexer_flags;
            local.release_type = file2.release_type;
            decide::score_formats(&catalog, &mut local);
            Ok(Ok(local))
        })?;
        let local = local.map_err(ManualImportError::Augmenting)?;
        let decision = ImportDecision::new(local, Vec::new());
        match tracked {
            None => {
                let result = approve::import(db, env, vec![decision], !existing_file, None, mode, now);
                outcome.events.extend(result.events);
                imported.extend(result.results);
            }
            Some(t) => {
                let result = approve::import(db, env, vec![decision], true, Some(&t.item), mode, now);
                outcome.events.extend(result.events);
                if let Some(r) = result.results.into_iter().next() {
                    tracked_at.push(imported.len());
                    imported.push(r.clone());
                    tracked_results.push((t, r));
                }
            }
        }
    }
    if !imported.is_empty() {
        progress(format!("Manually imported {} files", imported.len()));
    }
    for e in untracked_downloads_completed(&imported, &tracked_at) {
        outcome.events.push(e);
    }
    // `importedTrackedDownload.GroupBy(DownloadId)`: a download whose
    // episodes are all in is complete, its folder removed when only
    // samples remain.
    let mut seen: Vec<String> = Vec::new();
    for (t, _) in &tracked_results {
        let id = t.item.download_id.clone();
        if seen.contains(&id) {
            continue;
        }
        seen.push(id.clone());
        let mut tracked = t.clone();
        let results: Vec<&super::local::ImportResult> = tracked_results
            .iter()
            .filter(|(x, _)| x.item.download_id == id)
            .map(|(_, r)| r)
            .collect();
        let Some(first_local) = imported.first().and_then(|r| r.decision.local_episode.as_ref()) else {
            continue;
        };
        let Some(imported_series) = first_local.series.clone() else {
            continue;
        };
        let output_path = tracked
            .import_item
            .as_ref()
            .or(Some(&tracked.item))
            .and_then(|i| i.output_path.clone())
            .unwrap_or_default();
        if Path::new(&output_path).is_dir()
            && downloaded::should_delete_folder(&output_path, &imported_series)
            && tracked.item.can_move_files
        {
            if let Err(e) = std::fs::remove_dir_all(&output_path) {
                tracing::debug!(error = %e, "unable to remove the download folder {output_path}");
            }
        }
        let imported_results: Vec<&super::local::ImportResult> = results
            .iter()
            .copied()
            .filter(|r| r.result() == ImportResultType::Imported)
            .collect();
        let count: usize = imported_results
            .iter()
            .map(|r| {
                r.decision
                    .local_episode
                    .as_ref()
                    .map(|l| l.episodes.len())
                    .unwrap_or(0)
            })
            .sum();
        let expected = tracked
            .remote
            .as_ref()
            .map(|r| r.episodes.len())
            .unwrap_or(1)
            .max(1);
        if count >= expected {
            tracked.state = TrackedDownloadState::Imported;
            let ev = DownloadCompleted {
                tracked: tracked.clone(),
                series_id: imported_series.id,
                files: imported_results
                    .iter()
                    .filter_map(|r| r.episode_file.clone())
                    .collect(),
                release: imported_results
                    .first()
                    .and_then(|r| r.decision.local_episode.as_ref())
                    .and_then(|l| l.release.clone()),
            };
            let ev2 = ev.clone();
            if let Err(e) = db.call_blocking(move |conn| events::record_download_completed(conn, &ev2, now)) {
                tracing::error!(error = %e, "failed to record the completed download");
            }
            outcome.events.push(ImportEvent::DownloadCompleted(Box::new(ev)));
            outcome.completed.push(tracked);
        }
    }
    Ok(outcome)
}

/// `UntrackedDownloadCompletedEvent`, one per series and season of the
/// files imported. The original counts as untracked a result for which
/// no tracked download's result differs from it: with no tracked download
/// that is every file imported, with one it is that download's own file,
/// with more it is none. Kept as it is: it decides which Import Complete
/// notifications go out.
fn untracked_downloads_completed(
    imported: &[super::local::ImportResult],
    tracked_at: &[usize],
) -> Vec<ImportEvent> {
    let untracked = imported
        .iter()
        .enumerate()
        .filter(|(i, r)| r.result() == ImportResultType::Imported && tracked_at.iter().all(|t| t == i))
        .filter_map(|(_, r)| Some((r.decision.local_episode.as_ref()?, r.episode_file.as_ref()?)));
    // `GroupBy(SeriesId, SeasonNumber)`, in the order the groups appear.
    let mut groups: Vec<((i64, i64), Vec<(&LocalEpisode, &EpisodeFile)>)> = Vec::new();
    for (local, file) in untracked {
        let key = (file.series_id, file.season_number);
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, members)) => members.push((local, file)),
            None => groups.push((key, vec![(local, file)])),
        }
    }
    groups
        .into_iter()
        .filter_map(|(_, members)| {
            let (first, _) = members.first()?;
            let series = first.series.clone()?;
            let paths: Vec<String> = members.iter().map(|(l, _)| l.path.clone()).collect();
            Some(ImportEvent::UntrackedDownloadCompleted(Box::new(
                UntrackedDownloadCompleted {
                    series,
                    episodes: members.iter().flat_map(|(l, _)| l.episodes.clone()).collect(),
                    files: members.iter().map(|(_, f)| (*f).clone()).collect(),
                    parsed: first
                        .folder_episode_info
                        .clone()
                        .or_else(|| first.file_episode_info.clone()),
                    source_path: super::longest_common_path(&paths),
                },
            )))
        })
        .collect()
}

/// A connection-level helper for the API: the series a manual import names.
pub fn series_exists(conn: &Connection, id: i64) -> anyhow::Result<bool> {
    Ok(store::series::get(conn, id)?.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mediafiles::local::ImportResult;

    fn file(id: i64, season: i64) -> EpisodeFile {
        EpisodeFile {
            id,
            series_id: 1,
            season_number: season,
            relative_path: format!("Season {season:02}/{id}.mkv"),
            original_file_path: None,
            size: 1,
            date_added: chrono::Utc::now(),
            scene_name: None,
            release_group: None,
            release_hash: None,
            quality: QualityModel::default(),
            languages: Vec::new(),
            media_info: None,
            indexer_flags: 0,
            release_type: ReleaseType::default(),
        }
    }

    /// An imported file of season `season`, or one that was not imported.
    fn result(id: i64, season: i64, folder: Option<&str>, imported: bool) -> ImportResult {
        let local = LocalEpisode {
            series: Some(store::series::tests::sample(1)),
            path: format!("/downloads/manual/{id}/{id}.mkv"),
            file_episode_info: parser::parse_path(&format!(
                "/downloads/manual/{id}/Show.S{season:02}E{id:02}.mkv"
            )),
            folder_episode_info: folder.and_then(parser::parse_title),
            ..Default::default()
        };
        let decision = ImportDecision::new(local, Vec::new());
        if imported {
            ImportResult::imported(decision, file(id, season))
        } else {
            ImportResult::failed(decision, vec!["no".into()])
        }
    }

    fn untracked(events: &[ImportEvent]) -> Vec<(Vec<i64>, String, Option<String>)> {
        events
            .iter()
            .map(|e| match e {
                ImportEvent::UntrackedDownloadCompleted(ev) => (
                    ev.files.iter().map(|f| f.id).collect(),
                    ev.source_path.clone(),
                    ev.parsed.as_ref().map(|p| p.release_title.clone()),
                ),
                other => panic!("not an untracked download: {other:?}"),
            })
            .collect()
    }

    /// `UntrackedDownloadCompletedEvent` after a manual import: one per
    /// series and season of the files imported with no download, the
    /// folder's release named over the file's.
    #[test]
    fn files_without_a_download_complete_untracked_downloads() {
        let imported = [
            result(1, 1, Some("Show.S01.1080p.WEB-GRP"), true),
            result(2, 2, None, true),
            result(3, 1, None, true),
            result(4, 1, None, false),
        ];
        assert_eq!(
            untracked(&untracked_downloads_completed(&imported, &[])),
            [
                (
                    vec![1, 3],
                    "/downloads/manual".to_string(),
                    Some("Show.S01.1080p.WEB-GRP".to_string())
                ),
                (
                    vec![2],
                    "/downloads/manual/2".to_string(),
                    Some("Show.S02E02".to_string())
                ),
            ]
        );
    }

    /// The original's test for "untracked" lets a tracked download's own
    /// file through when it is the only one, and nothing when there are
    /// two: kept, since it decides the notifications.
    #[test]
    fn the_originals_reading_of_untracked_is_kept() {
        let imported = [
            result(1, 1, None, true),
            result(2, 1, None, true),
            result(3, 1, None, true),
        ];
        assert_eq!(
            untracked(&untracked_downloads_completed(&imported, &[1]))
                .into_iter()
                .map(|(ids, _, _)| ids)
                .collect::<Vec<_>>(),
            [vec![2]]
        );
        assert!(untracked_downloads_completed(&imported, &[0, 2]).is_empty());
    }
}
