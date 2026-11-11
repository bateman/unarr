//! `ImportDecisionMaker`: every video file of a download or a folder
//! parsed, augmented and run through the specifications.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rusqlite::Connection;

use super::aggregate;
use super::local::{ImportDecision, ImportRejection, LocalEpisode, RejectionReason as R};
use super::mediainfo::{self, MediaInfo};
use super::sample::{self, DetectSampleResult};
use super::specs;
use crate::customformat::{self, Input};
use crate::decision::lookup::{Catalog, Lookup, StoreLookup};
use crate::decision::upgradable;
use crate::downloadclients::DownloadClientItem;
use crate::model::{CustomFormat, NamingConfig, ReleaseType, Series};
use crate::parser;
use crate::parser::title::ParsedEpisodeInfo;
use crate::store;
use crate::store::config::MediaManagementConfig;

/// What one import run reads once.
#[derive(Debug, Clone)]
pub struct Environment {
    pub catalog: Catalog,
    pub config: MediaManagementConfig,
    pub naming: NamingConfig,
    pub now: DateTime<Utc>,
}

impl Environment {
    pub fn load(conn: &Connection, now: DateTime<Utc>) -> anyhow::Result<Environment> {
        Ok(Environment {
            catalog: Catalog::load(conn, now)?,
            config: MediaManagementConfig::load(conn)?,
            naming: store::config::naming(conn)?,
            now,
        })
    }
}

/// The media info of each file, probed before the database is asked
/// anything, so that ffprobe never runs on the database thread.
#[derive(Debug, Default, Clone)]
pub struct Probes {
    pub by_path: HashMap<String, Option<MediaInfo>>,
}

impl Probes {
    /// Probe `files` (those that are media files; disk images and stream
    /// files yield nothing, as the originals' reader returns null).
    pub fn probe_all(files: &[String]) -> Probes {
        let mut by_path = HashMap::new();
        for f in files {
            let info = mediainfo::probe(std::path::Path::new(f));
            by_path.insert(f.clone(), info);
        }
        Probes { by_path }
    }

    pub fn get(&self, path: &str) -> Option<MediaInfo> {
        self.by_path.get(path).cloned().flatten()
    }
}

/// `ParseCustomFormat(LocalEpisode)`.
pub fn local_formats(catalog: &Catalog, local: &LocalEpisode) -> Vec<CustomFormat> {
    let file_name = super::file_name(&local.path);
    let release_title = local
        .scene_name
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(file_name);
    let input = Input {
        release_title,
        filename: Some(file_name),
        release_group: local.release_group.as_deref(),
        quality: local.quality,
        languages: &local.languages,
        size: local.size,
        indexer_flags: local.indexer_flags,
        release_type: local.release_type,
        series_original_language: local.series.as_ref().map(|s| s.original_language_id).unwrap_or(0),
        movie: None,
    };
    customformat::evaluate(&catalog.formats, &input)
        .into_iter()
        .cloned()
        .collect()
}

/// The custom format score of a local episode under its series' profile.
pub fn score_formats(catalog: &Catalog, local: &mut LocalEpisode) {
    local.custom_formats = local_formats(catalog, local);
    local.custom_format_score = local
        .series
        .as_ref()
        .and_then(|s| catalog.profile(s.quality_profile_id))
        .map(|p| upgradable::format_score(p, &local.custom_formats))
        .unwrap_or(0);
}

/// `GetImportDecisions(videoFiles, series, downloadClientItem, folderInfo, sceneSource, filterExistingFiles)`.
#[allow(clippy::too_many_arguments)]
pub fn get_import_decisions(
    conn: &Connection,
    env: &Environment,
    probes: &Probes,
    video_files: &[String],
    series: &Series,
    item: Option<&DownloadClientItem>,
    folder_info: Option<&ParsedEpisodeInfo>,
    scene_source: bool,
    filter_existing_files: bool,
    tracked_flags: Option<i64>,
) -> anyhow::Result<Vec<ImportDecision>> {
    let new_files: Vec<String> = if filter_existing_files {
        let existing = store::episodes::list_files(conn, series.id)?;
        filter_existing(video_files, &existing, series)
    } else {
        video_files.to_vec()
    };
    tracing::debug!("Analyzing {}/{} files.", new_files.len(), video_files.len());
    let item_info = item.and_then(|i| parser::parse_title(&i.title));
    // Files that are not samples; when importing from the series folder
    // every file counts, as the originals skip the probe there.
    let non_sample_count = if scene_source {
        let is_possible_special = item_info
            .as_ref()
            .is_some_and(|i| i.is_possible_special_episode())
            || folder_info.is_some_and(|f| f.is_possible_special_episode());
        new_files
            .iter()
            .filter(|f| sample_of(series, f, is_possible_special, probes) != DetectSampleResult::Sample)
            .count()
    } else {
        video_files.len()
    };
    let lookup = StoreLookup::new(conn);
    let mut decisions = Vec::new();
    for file in &new_files {
        let local = LocalEpisode {
            series: Some(series.clone()),
            download_client_episode_info: item_info.clone(),
            download_item: item.cloned(),
            folder_episode_info: folder_info.cloned(),
            path: file.clone(),
            scene_source,
            existing_file: super::is_parent_path(&series.path, file),
            other_video_files: non_sample_count > 1,
            ..Default::default()
        };
        decisions.push(decide(conn, &lookup, env, probes, local, item, tracked_flags));
    }
    Ok(decisions)
}

/// `GetDecision(localEpisode, downloadClientItem)`: the specifications
/// only, for a local episode the caller filled in.
pub fn specs_only(
    lookup: &dyn Lookup,
    env: &Environment,
    local: LocalEpisode,
    item: Option<&DownloadClientItem>,
) -> ImportDecision {
    let ctx = specs::Context {
        lookup,
        catalog: &env.catalog,
        config: &env.config,
        naming: &env.naming,
        now: env.now,
    };
    let rejections = specs::evaluate(&ctx, &local, item);
    ImportDecision::new(local, rejections)
}

/// `IsSample(series, path, isSpecial)` with the probe already taken.
fn sample_of(series: &Series, path: &str, is_special: bool, probes: &Probes) -> DetectSampleResult {
    sample::is_sample_probe(series, path, is_special, probes.get(path).as_ref())
}

/// `GetDecision(localEpisode, downloadClientItem, otherFiles)`: parse,
/// augment, then the specifications; a parse failure or an error is a
/// rejection, as the originals turn their exceptions into one.
fn decide(
    conn: &Connection,
    lookup: &dyn Lookup,
    env: &Environment,
    probes: &Probes,
    mut local: LocalEpisode,
    item: Option<&DownloadClientItem>,
    tracked_flags: Option<i64>,
) -> ImportDecision {
    local.file_episode_info = parser::parse_path(&local.path);
    local.size = std::fs::metadata(&local.path)
        .map(|m| m.len() as i64)
        .unwrap_or(0);
    local.release_type = local
        .download_client_episode_info
        .as_ref()
        .map(|i| i.release_type())
        .or_else(|| local.folder_episode_info.as_ref().map(|i| i.release_type()))
        .or_else(|| local.file_episode_info.as_ref().map(|i| i.release_type()))
        .unwrap_or(ReleaseType::Unknown);
    let media_info = if !local.existing_file || env.config.enable_media_info {
        probes.get(&local.path)
    } else {
        None
    };
    if let Err(e) = aggregate::augment(conn, lookup, &mut local, media_info) {
        tracing::debug!("{}", e.0);
        return ImportDecision::rejected(
            Some(local),
            ImportRejection::new(R::UnableToParse, "Unable to parse file"),
        );
    }
    if local.episodes.is_empty() {
        let infos = [
            local.download_client_episode_info.as_ref(),
            local.folder_episode_info.as_ref(),
            local.file_episode_info.as_ref(),
        ];
        let rejection = if infos.iter().flatten().any(|i| i.is_partial_season) {
            ImportRejection::new(R::PartialSeason, "Partial season packs are not supported")
        } else if infos.iter().flatten().any(|i| i.is_season_extra) {
            ImportRejection::new(R::SeasonExtra, "Extras are not supported")
        } else {
            ImportRejection::new(R::InvalidSeasonOrEpisode, "Invalid season or episode")
        };
        return ImportDecision::rejected(Some(local), rejection);
    }
    if let Some(flags) = tracked_flags {
        local.indexer_flags = flags;
    }
    score_formats(&env.catalog, &mut local);
    specs_only(lookup, env, local, item)
}

/// `MediaFileService.FilterExistingFiles`: the paths the library does not
/// hold yet.
pub fn filter_existing(
    files: &[String],
    existing: &[crate::model::EpisodeFile],
    series: &Series,
) -> Vec<String> {
    let known: Vec<String> = existing
        .iter()
        .map(|f| super::join(&series.path, &f.relative_path))
        .collect();
    if known.is_empty() {
        return files.to_vec();
    }
    files
        .iter()
        .filter(|f| !known.iter().any(|k| super::path_equals(k, f)))
        .cloned()
        .collect()
}
