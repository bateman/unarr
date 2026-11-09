//! `AggregationService` and its aggregators: what a local episode is, read
//! from the file name, the folder name, the download client's title, the
//! grab history and the media info, each source trusted as the originals
//! trust it.

use rusqlite::Connection;

use super::local::{GrabbedRelease, LocalEpisode};
use super::mediainfo::MediaInfo;
use crate::decision::lookup::Lookup;
use crate::decision::mapping;
use crate::model::{Episode, HistoryEvent, QualityModel, ReleaseType};
use crate::parser;
use crate::parser::language::{self, lang};
use crate::parser::quality::{Detection, ParsedQuality};
use crate::parser::title::ParsedEpisodeInfo;
use crate::quality::{Quality, QualitySource};
use crate::store;

/// `AugmentingFailedException`: nothing about the path could be parsed.
#[derive(Debug)]
pub struct AugmentingFailed(pub String);

/// `Augment`: every aggregator in order. `media_info` is what the probe
/// found, or `None` when the file is not a media file or media info is off
/// for existing files.
pub fn augment(
    conn: &Connection,
    lookup: &dyn Lookup,
    local: &mut LocalEpisode,
    media_info: Option<MediaInfo>,
) -> Result<(), AugmentingFailed> {
    let is_media_file = parser::media_extensions::is_media_extension(super::extension(&local.path));
    if local.download_client_episode_info.is_none()
        && local.folder_episode_info.is_none()
        && local.file_episode_info.is_none()
        && is_media_file
    {
        return Err(AugmentingFailed(format!(
            "Unable to parse episode info from path: {}",
            local.path
        )));
    }
    local.size = std::fs::metadata(&local.path)
        .map(|m| m.len() as i64)
        .unwrap_or(0);
    local.scene_name = if local.scene_source {
        scene_name(local)
    } else {
        None
    };
    if is_media_file {
        local.media_info = media_info;
    }
    aggregate_episodes(lookup, local);
    aggregate_language(local);
    aggregate_quality(conn, local);
    aggregate_release_group(local);
    aggregate_release_hash(local);
    aggregate_release_info(conn, local);
    Ok(())
}

/// `SceneNameCalculator.GetSceneName`.
pub fn scene_name(local: &LocalEpisode) -> Option<String> {
    let other_files = local.other_video_files;
    if let Some(info) = &local.download_client_episode_info {
        if !other_files && !info.full_season {
            return Some(parser::title::remove_file_extension(&info.release_title));
        }
    }
    let file_name = super::file_stem(&local.path);
    if parser::title::is_scene_title(file_name) {
        return Some(file_name.to_string());
    }
    if let Some(folder) = &local.folder_episode_info {
        let title = folder.release_title.trim();
        if !other_files && !folder.full_season && !title.is_empty() && parser::title::is_scene_title(title) {
            return Some(title.to_string());
        }
    }
    None
}

// --- episodes

/// `AggregateEpisodes.GetBestEpisodeInfo`.
fn best_episode_info(lookup: &dyn Lookup, local: &LocalEpisode) -> Option<ParsedEpisodeInfo> {
    let mut parsed = local.file_episode_info.clone();
    let prefer_other = |file: &Option<ParsedEpisodeInfo>, other: &ParsedEpisodeInfo| -> bool {
        match file {
            None => true,
            // A file numbered the normal way beats an absolute other.
            Some(f) => f.is_absolute_numbering() || !other.is_absolute_numbering(),
        }
    };
    if !local.other_video_files && !parser::title::is_scene_title(super::file_stem(&local.path)) {
        if let Some(dl) = &local.download_client_episode_info {
            if !dl.full_season && prefer_other(&parsed, dl) {
                parsed = Some(dl.clone());
            }
        } else if let Some(folder) = &local.folder_episode_info {
            if !folder.full_season && prefer_other(&parsed, folder) {
                parsed = Some(folder.clone());
            }
        }
        // The folder is considered when the client's info was there but
        // not preferred, as the originals' `else if` chain does.
        if parsed == local.file_episode_info {
            if let (Some(dl), Some(folder)) =
                (&local.download_client_episode_info, &local.folder_episode_info)
            {
                let dl_preferred = !dl.full_season && prefer_other(&local.file_episode_info, dl);
                if !dl_preferred && !folder.full_season && prefer_other(&local.file_episode_info, folder) {
                    parsed = Some(folder.clone());
                }
            }
        }
    }
    if parsed.is_none() {
        parsed = special_episode_info(lookup, local, None);
    }
    parsed
}

fn special_episode_info(
    lookup: &dyn Lookup,
    local: &LocalEpisode,
    parsed: Option<&ParsedEpisodeInfo>,
) -> Option<ParsedEpisodeInfo> {
    let series = local.series.as_ref()?;
    let title = super::file_stem(&local.path);
    mapping::parse_special_episode_title_for(lookup, parsed, title, series)
}

/// `AggregateEpisodes`.
fn aggregate_episodes(lookup: &dyn Lookup, local: &mut LocalEpisode) {
    local.episodes = episodes_for(lookup, local);
}

fn episodes_for(lookup: &dyn Lookup, local: &LocalEpisode) -> Vec<Episode> {
    let Some(best) = best_episode_info(lookup, local) else {
        return Vec::new();
    };
    let Some(series) = &local.series else {
        return Vec::new();
    };
    let is_media_file = parser::media_extensions::is_media_extension(super::extension(&local.path));
    if !validate_for_series_type(&best, series, is_media_file) {
        return Vec::new();
    }
    let mut episodes =
        mapping::get_episodes(lookup, &best, series, local.scene_source, None).unwrap_or_default();
    if episodes.is_empty() && best.is_possible_special_episode() {
        if let Some(special) = special_episode_info(lookup, local, Some(&best)) {
            episodes =
                mapping::get_episodes(lookup, &special, series, local.scene_source, None).unwrap_or_default();
        }
    }
    episodes
}

/// `ValidateParsedEpisodeInfo.ValidateForSeriesType(info, series, isMediaFile)`:
/// a daily-style name for a standard series is no good, unless it is not a
/// media file at all.
fn validate_for_series_type(
    parsed: &ParsedEpisodeInfo,
    series: &crate::model::Series,
    is_media_file: bool,
) -> bool {
    if !is_media_file {
        return true;
    }
    mapping::validate_for_series_type(parsed, series)
}

// --- languages

/// `AggregateLanguage`: the file name, then the folder, then the client's
/// title, then the media info, each replacing the last when it says
/// something; the series' language when nothing does.
fn aggregate_language(local: &mut LocalEpisode) {
    let original = local
        .series
        .as_ref()
        .map(|s| s.original_language_id)
        .unwrap_or(lang::UNKNOWN);
    let mut languages = vec![original];
    let title_languages: Vec<Vec<i64>> = local
        .episodes
        .iter()
        .map(|e| language::parse_languages(e.title.as_deref().unwrap_or("")))
        .collect();
    let minus_titles = |mut langs: Vec<i64>| -> Vec<i64> {
        for t in &title_languages {
            langs.retain(|l| !t.contains(l));
        }
        langs
    };
    let candidates: Vec<Option<Vec<i64>>> = vec![
        local
            .file_episode_info
            .as_ref()
            .map(|i| minus_titles(i.languages.clone())),
        local
            .folder_episode_info
            .as_ref()
            .map(|i| minus_titles(i.languages.clone())),
        local
            .download_client_episode_info
            .as_ref()
            .map(|i| minus_titles(i.languages.clone())),
        local.media_info.as_ref().and_then(|m| {
            let mut found = Vec::new();
            for code in m.audio_languages() {
                if let Some(id) = super::language_from_iso(&code) {
                    if !found.contains(&id) {
                        found.push(id);
                    }
                }
            }
            if found.is_empty() {
                None
            } else {
                Some(found)
            }
        }),
    ];
    for candidate in candidates.into_iter().flatten() {
        if !candidate.is_empty() && !(candidate.len() == 1 && candidate[0] == lang::UNKNOWN) {
            languages = candidate;
        }
    }
    local.languages = languages;
}

// --- quality

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Confidence {
    Fallback,
    Default,
    Tag,
    MediaInfo,
}

struct AugmentedQuality {
    source: QualitySource,
    source_confidence: Confidence,
    resolution: i64,
    resolution_confidence: Confidence,
    revision: Option<(i64, i64, bool)>,
    revision_confidence: Confidence,
}

fn from_parsed(q: &ParsedQuality) -> AugmentedQuality {
    let quality = Quality::by_id(q.quality.quality_id);
    let confidence = |d: Detection| {
        if d == Detection::Name {
            Confidence::Tag
        } else {
            Confidence::Fallback
        }
    };
    AugmentedQuality {
        source: quality.source,
        source_confidence: confidence(q.source_detection),
        resolution: quality.resolution,
        resolution_confidence: confidence(q.resolution_detection),
        revision: Some((q.quality.version, q.quality.real, q.quality.is_repack)),
        revision_confidence: confidence(q.revision_detection),
    }
}

/// `AugmentQualityFromMediaInfo`: the resolution from the frame size, the
/// source from the container title when it names one.
fn from_media_info(info: &MediaInfo) -> Option<AugmentedQuality> {
    let (width, height) = (info.width(), info.height());
    let mut source = QualitySource::Unknown;
    let mut source_confidence = Confidence::Default;
    if let Some(title) = info.title.as_deref().filter(|t| !t.trim().is_empty()) {
        let parsed = parser::quality::parse_quality_name(title.trim());
        let q = Quality::by_id(parsed.quality.quality_id);
        if q.source != QualitySource::Unknown && parsed.source_detection == Detection::Name {
            source = q.source;
            source_confidence = Confidence::MediaInfo;
        }
    }
    let resolution = if width >= 3200 || height >= 2100 {
        2160
    } else if width >= 1800 || height >= 1000 {
        1080
    } else if width >= 1200 || height >= 700 {
        720
    } else if width >= 1000 || height >= 560 {
        576
    } else if width > 0 && height > 0 {
        480
    } else {
        return None;
    };
    Some(AugmentedQuality {
        source,
        source_confidence,
        resolution,
        resolution_confidence: Confidence::MediaInfo,
        revision: None,
        revision_confidence: Confidence::Default,
    })
}

/// `AggregateQuality`: file name, folder, client title, media info, then
/// the grab's title, the most confident source and resolution winning.
fn aggregate_quality(conn: &Connection, local: &mut LocalEpisode) {
    let mut source = QualitySource::Unknown;
    let mut source_confidence = Confidence::Default;
    let mut resolution = 0;
    let mut resolution_confidence = Confidence::Default;
    let mut revision = (1, 0, false);
    let mut revision_confidence = Confidence::Default;

    let mut augmented: Vec<AugmentedQuality> = Vec::new();
    if let Some(i) = &local.file_episode_info {
        augmented.push(from_parsed(&i.quality));
    }
    if let Some(i) = &local.folder_episode_info {
        augmented.push(from_parsed(&i.quality));
    }
    if let Some(i) = &local.download_client_episode_info {
        augmented.push(from_parsed(&i.quality));
    }
    if let Some(m) = &local.media_info {
        if let Some(a) = from_media_info(m) {
            augmented.push(a);
        }
    }
    // `AugmentQualityFromReleaseName`: the title the grab history recorded.
    if let Some(item) = &local.download_item {
        if let Ok(Some(grab)) = store::download_history::latest_grab(conn, &item.download_id) {
            augmented.push(from_parsed(&parser::quality::parse_quality(&grab.source_title)));
        }
    }
    for a in augmented {
        if source == QualitySource::Unknown
            || (a.source_confidence > source_confidence && a.source != QualitySource::Unknown)
        {
            source = a.source;
            source_confidence = a.source_confidence;
        }
        if resolution == 0 || (a.resolution_confidence > resolution_confidence && a.resolution > 0) {
            resolution = a.resolution;
            resolution_confidence = a.resolution_confidence;
        }
        if let Some(rev) = a.revision {
            if a.revision_confidence > revision_confidence
                || (a.revision_confidence == revision_confidence && revision_greater(rev, revision))
            {
                revision = rev;
                revision_confidence = a.revision_confidence;
            }
        }
    }
    local.quality = QualityModel {
        quality_id: parser::quality::find_by_source_and_resolution(source, resolution),
        version: revision.0,
        real: revision.1,
        is_repack: revision.2,
    };
}

/// `Revision.CompareTo`: by `real`, then by `version`.
fn revision_greater(a: (i64, i64, bool), b: (i64, i64, bool)) -> bool {
    (a.1, a.0) > (b.1, b.0)
}

// --- release group and hash

fn group_of(info: Option<&ParsedEpisodeInfo>, skip_full_season: bool) -> Option<String> {
    let info = info?;
    if info.full_season && skip_full_season {
        return None;
    }
    info.release_group
        .as_deref()
        .filter(|g| !g.trim().is_empty())
        .map(String::from)
}

/// `AggregateReleaseGroup`: the client's and folder's group unless they are
/// season packs, then the file's, then the packs'.
fn aggregate_release_group(local: &mut LocalEpisode) {
    local.release_group = group_of(local.download_client_episode_info.as_ref(), true)
        .or_else(|| group_of(local.folder_episode_info.as_ref(), true))
        .or_else(|| group_of(local.file_episode_info.as_ref(), false))
        .or_else(|| group_of(local.download_client_episode_info.as_ref(), false))
        .or_else(|| group_of(local.folder_episode_info.as_ref(), false));
}

fn hash_of(info: Option<&ParsedEpisodeInfo>) -> Option<String> {
    let info = info?;
    if info.full_season {
        return None;
    }
    Some(info.release_hash.clone()).filter(|h| !h.trim().is_empty())
}

/// `AggregateReleaseHash`.
fn aggregate_release_hash(local: &mut LocalEpisode) {
    local.release_hash = hash_of(local.file_episode_info.as_ref())
        .or_else(|| hash_of(local.download_client_episode_info.as_ref()))
        .or_else(|| hash_of(local.folder_episode_info.as_ref()));
}

/// `AggregateReleaseInfo`: what the grab history says about the download.
fn aggregate_release_info(conn: &Connection, local: &mut LocalEpisode) {
    let Some(item) = &local.download_item else {
        return;
    };
    let Ok(history) = store::history::find_by_download_id(conn, &item.download_id) else {
        return;
    };
    let grabbed: Vec<_> = history
        .into_iter()
        .filter(|h| h.event_type == HistoryEvent::Grabbed)
        .collect();
    if grabbed.is_empty() {
        return;
    }
    local.release = Some(grabbed_release(&grabbed));
}

/// `GrabbedReleaseInfo(grabbedHistories)`.
pub fn grabbed_release(grabbed: &[crate::model::HistoryEntry]) -> GrabbedRelease {
    let newest = grabbed.iter().max_by_key(|h| h.date).expect("at least one grab");
    let mut episode_ids: Vec<i64> = grabbed.iter().map(|h| h.episode_id).collect();
    let mut seen = std::collections::HashSet::new();
    episode_ids.retain(|id| seen.insert(*id));
    GrabbedRelease {
        title: newest.source_title.clone().unwrap_or_default(),
        indexer: newest.data["indexer"].as_str().map(String::from),
        size: newest.data["size"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0),
        release_type: newest.data["releaseType"]
            .as_str()
            .and_then(release_type_from_data)
            .unwrap_or(ReleaseType::Unknown),
        episode_ids,
    }
}

/// The history `data` spells release types in Pascal case (`SingleEpisode`);
/// `Enum.TryParse` ignores case.
pub fn release_type_from_data(s: &str) -> Option<ReleaseType> {
    ReleaseType::ALL
        .iter()
        .copied()
        .find(|t| t.as_str().eq_ignore_ascii_case(s.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(title: &str) -> ParsedEpisodeInfo {
        parser::parse_title(title).expect("parses")
    }

    #[test]
    fn release_group_prefers_the_download_then_the_file() {
        let mut local = LocalEpisode {
            file_episode_info: Some(parsed("Series.Title.S01E01.720p.HDTV.x264-FILE")),
            download_client_episode_info: Some(parsed("Series.Title.S01E01.720p.HDTV.x264-CLIENT")),
            ..Default::default()
        };
        aggregate_release_group(&mut local);
        assert_eq!(local.release_group.as_deref(), Some("CLIENT"));
        // A season pack's group is only a fallback.
        local.download_client_episode_info = Some(parsed("Series.Title.S01.720p.HDTV.x264-PACK"));
        aggregate_release_group(&mut local);
        assert_eq!(local.release_group.as_deref(), Some("FILE"));
        local.file_episode_info = Some(parsed("Series.Title.S01E01.720p.HDTV.x264"));
        aggregate_release_group(&mut local);
        assert_eq!(local.release_group.as_deref(), Some("PACK"));
    }

    #[test]
    fn media_info_resolution_wins_over_the_name() {
        let info = |w: i64, h: i64| MediaInfo {
            model: serde_json::json!({"width": w, "height": h}),
            title: None,
        };
        assert_eq!(from_media_info(&info(1920, 1080)).unwrap().resolution, 1080);
        assert_eq!(from_media_info(&info(1280, 720)).unwrap().resolution, 720);
        assert_eq!(from_media_info(&info(3840, 2160)).unwrap().resolution, 2160);
        assert_eq!(from_media_info(&info(1024, 576)).unwrap().resolution, 576);
        assert_eq!(from_media_info(&info(64, 64)).unwrap().resolution, 480);
        assert!(from_media_info(&info(0, 0)).is_none());
    }

    #[test]
    fn scene_name_comes_from_the_client_title_or_a_scene_file_name() {
        let local = LocalEpisode {
            path: "/d/Series.Title.S01E01.720p.HDTV.x264-GRP/file.mkv".into(),
            download_client_episode_info: Some(parsed("Series.Title.S01E01.720p.HDTV.x264-GRP.mkv")),
            ..Default::default()
        };
        assert_eq!(
            scene_name(&local).as_deref(),
            Some("Series.Title.S01E01.720p.HDTV.x264-GRP")
        );
        let local = LocalEpisode {
            path: "/d/Series.Title.S01E01.720p.HDTV.x264-GRP.mkv".into(),
            ..Default::default()
        };
        assert_eq!(
            scene_name(&local).as_deref(),
            Some("Series.Title.S01E01.720p.HDTV.x264-GRP")
        );
        let local = LocalEpisode {
            path: "/d/07f9c8d0e1b2a3c4d5e6f7a8b9c0d1e2.mkv".into(),
            ..Default::default()
        };
        assert_eq!(scene_name(&local), None);
    }
}
