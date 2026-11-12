//! Radarr's `AggregationService` and its aggregators: what a local movie
//! is, read from the file name, the folder name, the download client's
//! title, the grab history and the media info, each source trusted as the
//! original trusts it.

use rusqlite::Connection;

use super::local::{GrabbedMovieRelease, LocalMovie};
use crate::decision::upgradable::compare_revision;
use crate::downloadclients::DownloadClientItem;
use crate::mediafiles::aggregate::AugmentingFailed;
use crate::mediafiles::mediainfo::MediaInfo;
use crate::mediafiles::{extension, file_stem};
use crate::model::QualityModel;
use crate::movies::downloads::tracked::download_event;
use crate::movies::model::{MovieFile, MovieHistoryEntry, MovieHistoryEvent};
use crate::movies::naming::language_by_iso_code;
use crate::movies::parser::language::lang;
use crate::movies::parser::{self, quality as quality_parser};
use crate::movies::quality::{modifier_number, parse_indexer_flags, MovieQuality, RADARR_LANGUAGES};
use crate::movies::store;
use crate::parser::media_extensions::is_media_extension;
use crate::parser::quality::{Detection, ParsedQuality};
use crate::parser::subtitles::{self, LanguageLookup, SubtitleTitleInfo};

/// `QualitySource.UNKNOWN` and `Modifier.NONE`, as the quality table
/// spells them.
const UNKNOWN_SOURCE: &str = "unknown";
const NO_MODIFIER: &str = "none";

/// `Augment`: every aggregator in the order the original runs them (all of
/// order 1 as they are registered, the subtitle one last). `media_info` is
/// what the reader found, or `None` when it found nothing or media info is
/// off for existing files; `item` is the download the file came with.
pub fn augment(
    conn: &Connection,
    local: &mut LocalMovie,
    item: Option<&DownloadClientItem>,
    media_info: Option<MediaInfo>,
) -> Result<(), AugmentingFailed> {
    let is_media_file = is_media_extension(extension(&local.path));
    if local.download_client_movie_info.is_none()
        && local.folder_movie_info.is_none()
        && local.file_movie_info.is_none()
        && is_media_file
    {
        return Err(AugmentingFailed(format!(
            "Unable to parse movie info from path: {}",
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
    aggregate_edition(local);
    aggregate_language(local);
    aggregate_movie(conn, local);
    aggregate_quality(conn, local, item);
    aggregate_release_group(local);
    aggregate_release_info(conn, local, item);
    aggregate_subtitle_info(conn, local);
    Ok(())
}

/// What the original logs when an aggregator throws.
fn warn_unable(local: &LocalMovie, error: impl std::fmt::Display) {
    let movie = local
        .movie
        .as_ref()
        .map(crate::movies::decision::specs::movie_string)
        .unwrap_or_default();
    tracing::warn!(
        "Unable to augment information for file: '{}'. Movie: {movie} Error: {error}",
        local.path
    );
}

/// `SceneNameCalculator.GetSceneName`.
pub fn scene_name(local: &LocalMovie) -> Option<String> {
    let other_files = local.other_video_files;
    if let Some(info) = &local.download_client_movie_info {
        if !other_files {
            return Some(parser::remove_file_extension(&info.release_title));
        }
    }
    let file_name = file_stem(&local.path);
    if parser::scene_title(file_name).is_some() {
        return Some(file_name.to_string());
    }
    if let Some(folder) = &local.folder_movie_info {
        let title = folder.release_title.as_str();
        if !other_files && !title.trim().is_empty() && parser::scene_title(title).is_some() {
            return Some(title.to_string());
        }
    }
    None
}

// --- edition, release group

fn present(value: Option<&str>) -> bool {
    value.is_some_and(|v| !v.trim().is_empty())
}

/// `AggregateEdition`: the client's, else the folder's, else the file's.
fn aggregate_edition(local: &mut LocalMovie) {
    let mut edition = local
        .download_client_movie_info
        .as_ref()
        .map(|i| i.edition.clone());
    if !present(edition.as_deref()) {
        edition = local.folder_movie_info.as_ref().map(|i| i.edition.clone());
    }
    if !present(edition.as_deref()) {
        edition = local.file_movie_info.as_ref().map(|i| i.edition.clone());
    }
    local.edition = edition;
}

/// `AggregateReleaseGroup`: the client's, else the folder's, else the
/// file's.
fn aggregate_release_group(local: &mut LocalMovie) {
    let mut group = local
        .download_client_movie_info
        .as_ref()
        .and_then(|i| i.release_group.clone());
    if !present(group.as_deref()) {
        group = local
            .folder_movie_info
            .as_ref()
            .and_then(|i| i.release_group.clone());
    }
    if !present(group.as_deref()) {
        group = local
            .file_movie_info
            .as_ref()
            .and_then(|i| i.release_group.clone());
    }
    local.release_group = group;
}

// --- movie

/// `AggregateMovie`: the movie as the library has it now.
fn aggregate_movie(conn: &Connection, local: &mut LocalMovie) {
    let Some(id) = local.movie.as_ref().map(|m| m.id) else {
        warn_unable(local, "Object reference not set to an instance of an object.");
        return;
    };
    match store::get_movie(conn, id) {
        Ok(Some(movie)) => local.movie = Some(movie),
        Ok(None) => warn_unable(local, format!("Movie with ID {id} does not exist")),
        Err(e) => warn_unable(local, format!("{e:#}")),
    }
}

// --- languages

/// `AugmentLanguageFromMediaInfo`: the languages the audio tracks name,
/// `None` when no track names one the table knows.
pub(crate) fn languages_from_media_info(info: &MediaInfo) -> Option<Vec<i64>> {
    let mut codes: Vec<String> = Vec::new();
    for code in info.audio_languages() {
        if !codes.contains(&code) {
            codes.push(code);
        }
    }
    let languages: Vec<i64> = codes
        .iter()
        .filter_map(|code| language_by_iso_code(&code.to_lowercase()))
        .collect();
    if languages.is_empty() {
        None
    } else {
        Some(languages)
    }
}

/// The loop of `AggregateLanguage`: each augmenter that says something
/// (more than nothing, more than `Unknown` alone) replaces what the ones
/// before it said; the movie's original language when none does.
pub(crate) fn combine_languages(
    original: i64,
    augmented: impl IntoIterator<Item = Option<Vec<i64>>>,
) -> Vec<i64> {
    let mut languages = vec![original];
    for candidate in augmented.into_iter().flatten() {
        if !candidate.is_empty() && !(candidate.len() == 1 && candidate[0] == lang::UNKNOWN) {
            languages = candidate;
        }
    }
    languages
}

/// `AggregateLanguage`: the file name, then the folder, then the client's
/// title, then the media info.
fn aggregate_language(local: &mut LocalMovie) {
    let original = local
        .movie
        .as_ref()
        .map(|m| m.metadata.original_language_id)
        .unwrap_or(lang::UNKNOWN);
    let augmented = [
        local.file_movie_info.as_ref().map(|i| i.languages.clone()),
        local.folder_movie_info.as_ref().map(|i| i.languages.clone()),
        local
            .download_client_movie_info
            .as_ref()
            .map(|i| i.languages.clone()),
        local.media_info.as_ref().and_then(languages_from_media_info),
    ];
    local.languages = combine_languages(original, augmented);
    tracing::debug!(
        "Selected languages: {}",
        local
            .languages
            .iter()
            .map(|id| crate::movies::quality::MovieLanguage::name_of(*id))
            .collect::<Vec<_>>()
            .join(", ")
    );
}

// --- quality

/// The quality augmenters' `Confidence`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Confidence {
    Fallback,
    Default,
    Tag,
    MediaInfo,
}

/// `AugmentQualityResult`; the revision is `(version, real, is repack)`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AugmentedQuality {
    pub source: &'static str,
    pub source_confidence: Confidence,
    pub resolution: i64,
    pub resolution_confidence: Confidence,
    pub modifier: &'static str,
    pub modifier_confidence: Confidence,
    pub revision: Option<(i64, i64, bool)>,
    pub revision_confidence: Confidence,
}

/// What `AggregateQuality` settles on, with the confidences the original
/// turns into the quality's detection sources (`MediaInfo`, `Fallback`
/// for the extension, the name otherwise).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AggregatedQuality {
    pub quality: QualityModel,
    pub source_confidence: Confidence,
    pub resolution_confidence: Confidence,
    pub revision_confidence: Confidence,
}

/// `AugmentQualityFromFileName`, `...FromFolder`, `...FromDownloadClientItem`
/// and `...FromReleaseName`: a parsed quality, trusted as a tag where the
/// name said it and as a fallback where the extension did. The modifier is
/// never a tag: the original's parser does not say where it read one.
pub(crate) fn from_parsed(q: &ParsedQuality) -> AugmentedQuality {
    let quality = MovieQuality::by_id(q.quality.quality_id);
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
        modifier: quality.modifier,
        modifier_confidence: Confidence::Fallback,
        revision: Some((q.quality.version, q.quality.real, q.quality.is_repack)),
        revision_confidence: confidence(q.revision_detection),
    }
}

/// `AugmentQualityFromMediaInfo`: the resolution from the frame size, the
/// source and the modifier from the container title when it names a
/// source.
pub(crate) fn from_media_info(info: &MediaInfo) -> Option<AugmentedQuality> {
    let (width, height) = (info.width(), info.height());
    let mut source = UNKNOWN_SOURCE;
    let mut source_confidence = Confidence::Default;
    let mut modifier = NO_MODIFIER;
    let mut modifier_confidence = Confidence::Default;
    if let Some(title) = info.title.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        tracing::debug!("Parsing quality from media info title '{title}'");
        let parsed = quality_parser::parse_quality_name(title);
        let q = MovieQuality::by_id(parsed.quality.quality_id);
        // Only when the source is known and the title itself named it.
        if q.source != UNKNOWN_SOURCE && parsed.source_detection == Detection::Name {
            source = q.source;
            source_confidence = Confidence::MediaInfo;
            modifier = q.modifier;
            modifier_confidence = Confidence::MediaInfo;
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
        modifier,
        modifier_confidence,
        revision: None,
        revision_confidence: Confidence::Default,
    })
}

/// The loop of `AggregateQuality`: the most confident source, resolution
/// and modifier, and the best revision among the most confident ones.
pub(crate) fn combine_qualities(augmented: impl IntoIterator<Item = AugmentedQuality>) -> AggregatedQuality {
    let mut source = UNKNOWN_SOURCE;
    let mut source_confidence = Confidence::Default;
    let mut resolution = 0;
    let mut resolution_confidence = Confidence::Default;
    let mut modifier = NO_MODIFIER;
    let mut modifier_confidence = Confidence::Default;
    let mut revision = QualityModel {
        quality_id: 0,
        version: 1,
        real: 0,
        is_repack: false,
    };
    let mut revision_confidence = Confidence::Default;

    for a in augmented {
        if source == UNKNOWN_SOURCE || (a.source_confidence > source_confidence && a.source != UNKNOWN_SOURCE)
        {
            source = a.source;
            source_confidence = a.source_confidence;
        }
        if resolution == 0 || (a.resolution_confidence > resolution_confidence && a.resolution > 0) {
            resolution = a.resolution;
            resolution_confidence = a.resolution_confidence;
        }
        if modifier_number(a.modifier) > modifier_number(modifier)
            || (a.modifier_confidence > modifier_confidence && a.modifier != NO_MODIFIER)
        {
            modifier = a.modifier;
            modifier_confidence = a.modifier_confidence;
        }
        if let Some((version, real, is_repack)) = a.revision {
            let candidate = QualityModel {
                quality_id: 0,
                version,
                real,
                is_repack,
            };
            // A more confident revision replaces the current one, so that
            // an explicit v0 overrides the default v1; among equals the
            // higher one wins.
            if a.revision_confidence > revision_confidence
                || (a.revision_confidence == revision_confidence
                    && compare_revision(&candidate, &revision) == std::cmp::Ordering::Greater)
            {
                revision = candidate;
                revision_confidence = a.revision_confidence;
            }
        }
    }
    AggregatedQuality {
        quality: QualityModel {
            quality_id: quality_parser::find_by_source_and_resolution(source, resolution, modifier),
            ..revision
        },
        source_confidence,
        resolution_confidence,
        revision_confidence,
    }
}

/// `GetLatestGrab(downloadId).SourceTitle`: the title of the newest grab
/// the download history holds for a download.
fn latest_grab_title(conn: &Connection, download_id: &str) -> anyhow::Result<Option<String>> {
    let mut stmt = conn.prepare(
        "SELECT source_title FROM download_history
         WHERE download_id = ?1 AND CAST(event_type AS INTEGER) = ?2
         ORDER BY date DESC, id DESC LIMIT 1",
    )?;
    let mut rows = stmt.query(rusqlite::params![download_id, download_event::GRABBED])?;
    match rows.next()? {
        Some(row) => Ok(Some(row.get(0)?)),
        None => Ok(None),
    }
}

/// `AggregateQuality`: file name, folder, client title, media info, then
/// the grab's title.
fn aggregate_quality(conn: &Connection, local: &mut LocalMovie, item: Option<&DownloadClientItem>) {
    let mut augmented: Vec<AugmentedQuality> = Vec::new();
    if let Some(i) = &local.file_movie_info {
        augmented.push(from_parsed(&i.quality));
    }
    if let Some(i) = &local.folder_movie_info {
        augmented.push(from_parsed(&i.quality));
    }
    if let Some(i) = &local.download_client_movie_info {
        augmented.push(from_parsed(&i.quality));
    }
    if let Some(a) = local.media_info.as_ref().and_then(from_media_info) {
        augmented.push(a);
    }
    // `AugmentQualityFromReleaseName`: the title the grab recorded.
    if let Some(item) = item {
        match latest_grab_title(conn, &item.download_id) {
            Ok(Some(title)) => augmented.push(from_parsed(&quality_parser::parse_quality(&title))),
            Ok(None) => {}
            Err(e) => {
                warn_unable(local, format!("{e:#}"));
                return;
            }
        }
    }
    let aggregated = combine_qualities(augmented);
    tracing::debug!(
        "Using quality: {}",
        crate::movies::decision::specs::quality_string(&aggregated.quality)
    );
    local.quality = aggregated.quality;
}

// --- release info

/// `AggregateReleaseInfo`: what the grab history says about the download.
fn aggregate_release_info(conn: &Connection, local: &mut LocalMovie, item: Option<&DownloadClientItem>) {
    let Some(item) = item else {
        return;
    };
    let history = match store::history_by_download_id(conn, &item.download_id) {
        Ok(history) => history,
        Err(e) => {
            warn_unable(local, format!("{e:#}"));
            return;
        }
    };
    let grabbed: Vec<MovieHistoryEntry> = history
        .into_iter()
        .filter(|h| h.event_type == MovieHistoryEvent::Grabbed)
        .collect();
    if grabbed.is_empty() {
        return;
    }
    local.release = Some(grabbed_release(&grabbed));
}

/// `GrabbedReleaseInfo(grabbedHistories)`: the newest grab (the first of
/// the newest, as `MaxBy`), and every movie the grabs were for.
pub fn grabbed_release(grabbed: &[MovieHistoryEntry]) -> GrabbedMovieRelease {
    let newest = grabbed
        .iter()
        .reduce(|best, h| if h.date > best.date { h } else { best })
        .expect("at least one grab");
    let mut movie_ids: Vec<i64> = Vec::new();
    for h in grabbed {
        if !movie_ids.contains(&h.movie_id) {
            movie_ids.push(h.movie_id);
        }
    }
    let data = |key: &str| newest.data.get(key).and_then(|v| v.as_str());
    GrabbedMovieRelease {
        title: newest.source_title.clone().unwrap_or_default(),
        indexer: data("indexer").map(String::from),
        size: data("size").and_then(|s| s.parse().ok()).unwrap_or(0),
        indexer_flags: data("indexerFlags").and_then(parse_indexer_flags).unwrap_or(0),
        movie_ids,
    }
}

// --- subtitles

/// `AggregateSubtitleInfo`: for a subtitle file, what its name says beyond
/// the movie file's.
fn aggregate_subtitle_info(conn: &Connection, local: &mut LocalMovie) {
    if !super::extras::is_subtitle_extension(extension(&local.path)) {
        return;
    }
    let Some(movie) = &local.movie else {
        return;
    };
    match store::get_movie_file(conn, movie.movie_file_id) {
        Ok(Some(file)) => {
            local.subtitle_info = Some(clean_subtitle_title_info(
                &file,
                &local.path,
                local.file_name_before_rename.as_deref(),
            ));
        }
        Ok(None) => warn_unable(local, "Object reference not set to an instance of an object."),
        Err(e) => warn_unable(local, format!("{e:#}")),
    }
}

/// Radarr's languages for the subtitle parser, which the series side
/// shares: its own ISO table and its own `Language.All`.
pub(crate) const MOVIE_SUBTITLE_LANGUAGES: LanguageLookup = LanguageLookup {
    by_iso_code: crate::movies::naming::language_by_iso_code,
    by_name_at_end: movie_language_named_at_end,
};

/// The first of Radarr's `Language.All` whose name ends `lower`: `Unknown`,
/// the languages, then `Any` and `Original`.
fn movie_language_named_at_end(lower: &str) -> Option<i64> {
    let named = |id: i64| {
        RADARR_LANGUAGES
            .iter()
            .any(|l| l.id == id && lower.ends_with(&l.name.to_lowercase()))
    };
    let highest = RADARR_LANGUAGES.iter().map(|l| l.id).max().unwrap_or(0);
    (0..=highest).chain([-1, -2]).find(|id| named(*id))
}

/// `AggregateSubtitleInfo.CleanSubtitleTitleInfo`: what the subtitle's
/// name says, less what it only repeats from the movie file's name.
pub fn clean_subtitle_title_info(
    movie_file: &MovieFile,
    path: &str,
    file_name_before_rename: Option<&str>,
) -> SubtitleTitleInfo {
    let mut info = subtitles::parse_subtitle_information_in(&MOVIE_SUBTITLE_LANGUAGES, path);
    let file_title = file_stem(file_name_before_rename.unwrap_or(&movie_file.relative_path)).to_lowercase();
    let original_title = movie_file
        .original_file_path
        .as_deref()
        .map(|p| file_stem(p).to_lowercase())
        .unwrap_or_default();
    if info.title_first {
        if let Some(raw) = info.raw_title.as_deref().map(str::to_lowercase) {
            if file_title.contains(&raw) || original_title.contains(&raw) {
                tracing::debug!(
                    "Subtitle title '{raw}' is in movie file title '{file_title}'. Removing from subtitle title."
                );
                info = subtitles::parse_basic_subtitle_in(&MOVIE_SUBTITLE_LANGUAGES, path);
            }
        }
    }
    info.language_tags
        .retain(|tag| !file_title.contains(&tag.to_lowercase()));
    info
}

#[cfg(test)]
mod tests {
    //! The original's `GetSceneNameFixture` and the fixtures under
    //! `MovieImport/Aggregation`: the aggregators that the originals test
    //! with mocked augmenters are tested through the loop they run.

    use chrono::{DateTime, Duration, Utc};
    use serde_json::json;

    use super::*;
    use crate::db::Db;
    use crate::downloadclients::{ClientKind, ItemClientInfo};
    use crate::model::Protocol;
    use crate::movies::decision::testing;
    use crate::movies::model::MovieDownloadHistoryEntry;
    use crate::movies::parser::quality::{
        BLURAY1080P, BLURAY720P, HDTV1080P, HDTV720P, SDTV, WEBDL1080P, WEBDL480P,
    };
    use crate::movies::parser::{parse_movie_title, ParsedMovieInfo};
    use crate::Personality;

    fn parsed(title: &str) -> ParsedMovieInfo {
        parse_movie_title(title, false).expect("parses")
    }

    /// A parsed name that holds nothing but its release title.
    fn release_title(title: &str) -> ParsedMovieInfo {
        let mut info = testing::parsed(testing::quality(0, 1));
        info.release_title = title.to_string();
        info
    }

    fn item(download_id: &str, title: &str) -> DownloadClientItem {
        DownloadClientItem::new(
            ItemClientInfo {
                protocol: Protocol::Usenet,
                kind: ClientKind::Sabnzbd,
                id: 1,
                name: "SABnzbd".into(),
                remove_completed_downloads: true,
                has_post_import_category: false,
            },
            download_id,
            title,
        )
    }

    // GetSceneNameFixture

    const MOVIE_NAME: &str = "movie.title.2022.dvdrip.x264-ingot";

    fn scene_local() -> LocalMovie {
        let mut movie = testing::movie(1, "Movie Title", 2022);
        movie.path = "/Test/Movies/Movie Title".into();
        LocalMovie {
            path: format!("{}/Movie Title - 2022 - Episode Title.mkv", movie.path),
            movie: Some(movie),
            quality: testing::quality(BLURAY720P, 1),
            release_group: Some("DRONE".into()),
            ..Default::default()
        }
    }

    #[test]
    fn should_use_download_client_item_title_as_scene_name() {
        let mut local = scene_local();
        local.download_client_movie_info = Some(release_title(MOVIE_NAME));
        assert_eq!(scene_name(&local).as_deref(), Some(MOVIE_NAME));
    }

    #[test]
    fn should_not_use_download_client_item_title_as_scene_name_if_there_are_other_video_files() {
        let mut local = scene_local();
        local.other_video_files = true;
        local.download_client_movie_info = Some(release_title(MOVIE_NAME));
        local.path = format!("/Test/Unsorted Movies/{MOVIE_NAME}");
        assert_eq!(scene_name(&local), None);
    }

    #[test]
    fn should_use_file_name_as_scenename_only_if_it_looks_like_scenename() {
        let mut local = scene_local();
        local.path = format!("/Test/Unsorted Movies/{MOVIE_NAME}.mkv");
        assert_eq!(scene_name(&local).as_deref(), Some(MOVIE_NAME));
    }

    #[test]
    fn should_not_use_file_name_as_scenename_if_it_doesnt_look_like_scenename() {
        let mut local = scene_local();
        local.path = format!("/Test/Unsorted Movies/{MOVIE_NAME}/aaaaa.mkv");
        assert_eq!(scene_name(&local), None);
    }

    #[test]
    fn should_use_folder_name_as_scenename_only_if_it_looks_like_scenename() {
        let mut local = scene_local();
        local.folder_movie_info = Some(release_title(MOVIE_NAME));
        assert_eq!(scene_name(&local).as_deref(), Some(MOVIE_NAME));
    }

    #[test]
    fn should_not_use_folder_name_as_scenename_if_it_doesnt_look_like_scenename() {
        let mut local = scene_local();
        local.path = format!("/Test/Unsorted Movies/{MOVIE_NAME}/aaaaa.mkv");
        local.folder_movie_info = Some(release_title("aaaaa"));
        assert_eq!(scene_name(&local), None);
    }

    #[test]
    fn should_not_use_folder_name_as_scenename_if_there_are_other_video_files() {
        let mut local = scene_local();
        local.other_video_files = true;
        local.path = format!("/Test/Unsorted Movies/{MOVIE_NAME}/aaaaa.mkv");
        local.folder_movie_info = Some(release_title(MOVIE_NAME));
        assert_eq!(scene_name(&local), None);
    }

    #[test]
    fn should_remove_extension_from_nzb_title_for_scene_name() {
        for extension in [".mkv", ".par2", ".nzb"] {
            let mut local = scene_local();
            local.download_client_movie_info = Some(release_title(&format!("{MOVIE_NAME}{extension}")));
            assert_eq!(scene_name(&local).as_deref(), Some(MOVIE_NAME), "{extension}");
        }
    }

    // AggregateLanguageFixture

    const ORIGINAL: i64 = lang::ENGLISH;

    fn languages(
        file: Option<Vec<i64>>,
        folder: Option<Vec<i64>>,
        client: Option<Vec<i64>>,
        media_info: Option<Vec<i64>>,
    ) -> Vec<i64> {
        combine_languages(ORIGINAL, [file, folder, client, media_info])
    }

    #[test]
    fn should_return_default_if_no_info_is_known() {
        assert_eq!(languages(None, None, None, None), vec![ORIGINAL]);
        // Through the aggregator, for a file nothing is known of.
        let mut local = LocalMovie {
            movie: Some(testing::movie(1, "Movie Title", 2008)),
            ..Default::default()
        };
        aggregate_language(&mut local);
        assert_eq!(local.languages, vec![ORIGINAL]);
        let mut local = LocalMovie::default();
        aggregate_language(&mut local);
        assert_eq!(local.languages, vec![lang::UNKNOWN], "no movie either");
    }

    #[test]
    fn should_return_file_language_when_only_file_info_is_known() {
        assert_eq!(
            languages(Some(vec![lang::FRENCH]), None, None, None),
            vec![lang::FRENCH]
        );
    }

    #[test]
    fn should_return_folder_language_when_folder_info_is_known() {
        assert_eq!(
            languages(Some(vec![lang::FRENCH]), Some(vec![lang::GERMAN]), None, None),
            vec![lang::GERMAN]
        );
    }

    #[test]
    fn should_return_download_client_item_language_when_download_client_item_info_is_known() {
        assert_eq!(
            languages(
                Some(vec![lang::FRENCH]),
                Some(vec![lang::GERMAN]),
                Some(vec![lang::SPANISH]),
                None
            ),
            vec![lang::SPANISH]
        );
    }

    #[test]
    fn should_return_multi_language() {
        assert_eq!(
            languages(
                Some(vec![lang::UNKNOWN]),
                Some(vec![lang::FRENCH, lang::GERMAN]),
                Some(vec![lang::UNKNOWN]),
                None
            ),
            vec![lang::FRENCH, lang::GERMAN]
        );
    }

    #[test]
    fn should_use_mediainfo_over_others() {
        assert_eq!(
            languages(
                Some(vec![lang::UNKNOWN]),
                Some(vec![lang::FRENCH, lang::GERMAN]),
                Some(vec![lang::UNKNOWN]),
                Some(vec![lang::JAPANESE, lang::ENGLISH])
            ),
            vec![lang::JAPANESE, lang::ENGLISH]
        );
    }

    #[test]
    fn should_not_use_mediainfo_if_unknown() {
        assert_eq!(
            languages(
                Some(vec![lang::UNKNOWN]),
                Some(vec![lang::FRENCH, lang::GERMAN]),
                Some(vec![lang::UNKNOWN]),
                Some(vec![lang::UNKNOWN])
            ),
            vec![lang::FRENCH, lang::GERMAN]
        );
    }

    #[test]
    fn the_language_aggregator_reads_the_names_then_the_media_info() {
        let mut local = LocalMovie {
            movie: Some(testing::movie(1, "Movie Title", 2008)),
            file_movie_info: Some(parsed("Movie.Title.2008.FRENCH.1080p.BluRay.x264-GRP")),
            folder_movie_info: Some(parsed("Movie.Title.2008.German.1080p.BluRay.x264-GRP")),
            ..Default::default()
        };
        aggregate_language(&mut local);
        assert_eq!(local.languages, vec![lang::GERMAN]);
        local.media_info = Some(audio(&["jpn", "eng"]));
        aggregate_language(&mut local);
        assert_eq!(local.languages, vec![lang::JAPANESE, lang::ENGLISH]);
    }

    // AugmentLanguageFromMediaInfoFixture

    fn audio(codes: &[&str]) -> MediaInfo {
        MediaInfo {
            model: json!({"audioLanguages": codes}),
            title: None,
        }
    }

    #[test]
    fn should_return_language_for_single_known_language() {
        assert_eq!(
            languages_from_media_info(&audio(&["eng"])),
            Some(vec![lang::ENGLISH])
        );
    }

    #[test]
    fn should_only_return_one_when_language_duplicated() {
        assert_eq!(
            languages_from_media_info(&audio(&["eng", "eng"])),
            Some(vec![lang::ENGLISH])
        );
    }

    #[test]
    fn should_return_null_if_all_unknown() {
        assert_eq!(languages_from_media_info(&audio(&["pirate", "pirate"])), None);
        assert_eq!(languages_from_media_info(&audio(&[])), None);
    }

    #[test]
    fn should_return_known_languages_only() {
        assert_eq!(
            languages_from_media_info(&audio(&["eng", "pirate"])),
            Some(vec![lang::ENGLISH])
        );
    }

    #[test]
    fn should_return_multiple_known_languages() {
        assert_eq!(
            languages_from_media_info(&audio(&["eng", "ger"])),
            Some(vec![lang::ENGLISH, lang::GERMAN])
        );
    }

    // AggregateQualityFixture

    fn augmented(
        source: &'static str,
        source_confidence: Confidence,
        resolution: i64,
        resolution_confidence: Confidence,
        version: Option<i64>,
        revision_confidence: Confidence,
    ) -> AugmentedQuality {
        AugmentedQuality {
            source,
            source_confidence,
            resolution,
            resolution_confidence,
            modifier: NO_MODIFIER,
            modifier_confidence: Confidence::Fallback,
            revision: version.map(|v| (v, 0, false)),
            revision_confidence,
        }
    }

    /// `_fileExtensionAugmenter`: HDTV-720p, all of it a fallback.
    fn file_extension() -> AugmentedQuality {
        use Confidence::Fallback;
        augmented("tv", Fallback, 720, Fallback, Some(1), Fallback)
    }

    /// `_nameAugmenter`: SDTV by default.
    fn name(version: i64, revision_confidence: Confidence) -> AugmentedQuality {
        use Confidence::Default;
        augmented("tv", Default, 480, Default, Some(version), revision_confidence)
    }

    /// `_mediaInfoAugmenter`: `ResolutionOnly(1080p, MediaInfo)`.
    fn media_info_1080() -> AugmentedQuality {
        augmented(
            UNKNOWN_SOURCE,
            Confidence::Default,
            1080,
            Confidence::MediaInfo,
            None,
            Confidence::Default,
        )
    }

    /// `_releaseNameAugmenter`: `SourceOnly(WEBDL, MediaInfo)`.
    fn release_name() -> AugmentedQuality {
        augmented(
            "webdl",
            Confidence::MediaInfo,
            0,
            Confidence::Default,
            None,
            Confidence::Default,
        )
    }

    #[test]
    fn should_return_hdtv720_from_extension_when_other_augments_are_null() {
        let result = combine_qualities([file_extension()]);
        assert_eq!(result.source_confidence, Confidence::Fallback);
        assert_eq!(result.resolution_confidence, Confidence::Fallback);
        assert_eq!(result.quality.quality_id, HDTV720P);
    }

    #[test]
    fn should_return_sdtv_when_hdtv720_came_from_extension() {
        let result = combine_qualities([file_extension(), name(1, Confidence::Default)]);
        assert_eq!(result.source_confidence, Confidence::Default);
        assert_eq!(result.resolution_confidence, Confidence::Default);
        assert_eq!(result.quality.quality_id, SDTV);
    }

    #[test]
    fn should_return_hdtv1080p_when_hdtv720_came_from_extension_and_mediainfo_indicates_1080() {
        let result = combine_qualities([file_extension(), media_info_1080()]);
        assert_eq!(result.source_confidence, Confidence::Fallback);
        assert_eq!(result.resolution_confidence, Confidence::MediaInfo);
        assert_eq!(result.quality.quality_id, HDTV1080P);
    }

    #[test]
    fn should_return_hdtv1080p_when_sdtv_came_from_name_and_mediainfo_indicates_1080() {
        let result = combine_qualities([name(1, Confidence::Default), media_info_1080()]);
        assert_eq!(result.source_confidence, Confidence::Default);
        assert_eq!(result.resolution_confidence, Confidence::MediaInfo);
        assert_eq!(result.quality.quality_id, HDTV1080P);
    }

    #[test]
    fn should_return_webdl480p_when_file_name_has_hdtv480p_but_release_name_indicates_webdl_source() {
        let result = combine_qualities([name(1, Confidence::Default), release_name()]);
        assert_eq!(result.source_confidence, Confidence::MediaInfo);
        assert_eq!(result.resolution_confidence, Confidence::Default);
        assert_eq!(result.quality.quality_id, WEBDL480P);
    }

    #[test]
    fn should_return_version_1_when_no_version_specified() {
        let result = combine_qualities([name(1, Confidence::Default), release_name()]);
        assert_eq!(result.quality.version, 1);
        assert_eq!(result.revision_confidence, Confidence::Default);
    }

    #[test]
    fn should_return_version_2_when_name_indicates_proper() {
        let result = combine_qualities([name(2, Confidence::Tag), release_name()]);
        assert_eq!(result.quality.version, 2);
        assert_eq!(result.revision_confidence, Confidence::Tag);
    }

    #[test]
    fn should_return_version_0_when_file_name_indicates_v0() {
        let result = combine_qualities([name(0, Confidence::Tag), release_name()]);
        assert_eq!(result.quality.version, 0);
        assert_eq!(result.revision_confidence, Confidence::Tag);
    }

    #[test]
    fn should_return_version_2_when_file_name_indicates_v0_and_release_name_indicates_v2() {
        let result = combine_qualities([name(0, Confidence::Tag), name(2, Confidence::Tag)]);
        assert_eq!(result.quality.version, 2);
        assert_eq!(result.revision_confidence, Confidence::Tag);
    }

    #[test]
    fn a_modifier_is_kept_from_whichever_source_names_the_higher_one() {
        let remux = from_parsed(&quality_parser::parse_quality(
            "Movie.Title.2008.1080p.BluRay.REMUX.AVC.DTS-HD.MA.5.1-GRP",
        ));
        assert_eq!(remux.modifier, "remux");
        assert_eq!(remux.modifier_confidence, Confidence::Fallback);
        let plain = from_parsed(&quality_parser::parse_quality(
            "Movie.Title.2008.1080p.BluRay.x264-GRP",
        ));
        let remux_1080p = crate::movies::parser::quality::REMUX1080P;
        assert_eq!(
            combine_qualities([plain.clone(), remux.clone()])
                .quality
                .quality_id,
            remux_1080p
        );
        assert_eq!(
            combine_qualities([remux, plain.clone()]).quality.quality_id,
            remux_1080p
        );
        assert_eq!(combine_qualities([plain]).quality.quality_id, BLURAY1080P);
    }

    // AugmentQualityFromMediaInfoFixture

    fn frame(width: i64, height: i64, title: Option<&str>) -> MediaInfo {
        MediaInfo {
            model: json!({"width": width, "height": height}),
            title: title.map(String::from),
        }
    }

    #[test]
    fn should_return_null_if_media_info_width_is_zero() {
        assert_eq!(from_media_info(&frame(0, 1, None)), None);
    }

    #[test]
    fn should_return_closest_resolution() {
        for (width, height, expected) in [
            (4096, 1, 2160), // True 4K
            (4000, 1, 2160),
            (3840, 1, 2160), // 4K UHD
            (3200, 1, 2160),
            (2000, 1, 1080),
            (1920, 1, 1080),    // Full HD
            (1440, 1080, 1080), // 4:3 FullHD
            (1800, 1, 1080),
            (1490, 1, 720),
            (1280, 1, 720), // HD
            (1200, 1, 720),
            (1024, 1, 576),
            (768, 576, 576),
            (800, 1, 480),
            (720, 1, 480), // SDTV
            (600, 1, 480),
            (100, 1, 480),
        ] {
            let result = from_media_info(&frame(width, height, None)).expect("a resolution");
            assert_eq!(result.resolution, expected, "{width}x{height}");
            assert_eq!(result.resolution_confidence, Confidence::MediaInfo);
            assert_eq!(result.source, UNKNOWN_SOURCE);
            assert_eq!(result.revision, None);
        }
    }

    #[test]
    fn should_include_source_if_extracted_from_title() {
        let result = from_media_info(&frame(1920, 1080, Some("Movie.Title.2008.WEB.x264-Radarr"))).unwrap();
        assert_eq!(result.resolution, 1080);
        assert_eq!(result.source, "webdl");
        assert_eq!(result.source_confidence, Confidence::MediaInfo);
    }

    #[test]
    fn should_have_unknown_source_if_no_source_extracted_from_title() {
        let result = from_media_info(&frame(1920, 1080, Some("Movie.Title.2008.x264-Radarr"))).unwrap();
        assert_eq!(result.resolution, 1080);
        assert_eq!(result.source, UNKNOWN_SOURCE);
        assert_eq!(result.source_confidence, Confidence::Default);
    }

    // AugmentQualityFromReleaseNameFixture

    #[test]
    fn should_return_augmented_quality() {
        use Confidence::{Fallback, Tag};
        for (title, source, source_confidence, resolution, resolution_confidence) in [
            ("Series.Title.S01E01.1080p.WEB.x264", "webdl", Tag, 1080, Tag),
            ("Series.Title.S01E01.WEB.x264", "webdl", Tag, 480, Fallback),
            ("Series.Title.S01E01.720p.x264", "tv", Fallback, 720, Tag),
        ] {
            let result = from_parsed(&quality_parser::parse_quality(title));
            assert_eq!(result.source, source, "{title}");
            assert_eq!(result.source_confidence, source_confidence, "{title}");
            assert_eq!(result.resolution, resolution, "{title}");
            assert_eq!(result.resolution_confidence, resolution_confidence, "{title}");
        }
    }

    // AggregateReleaseGroupFixture

    fn release_group(file: &str, folder: &str, client: &str) -> Option<String> {
        let mut local = LocalMovie {
            file_movie_info: Some(parsed(file)),
            folder_movie_info: Some(parsed(folder)),
            download_client_movie_info: Some(parsed(client)),
            path: "/Test/Unsorted Movies/Movie.Title.2008/Movie.Title.2008.WEB-DL.mkv".into(),
            movie: Some(testing::movie(1, "Movie Title", 2008)),
            ..Default::default()
        };
        aggregate_release_group(&mut local);
        local.release_group
    }

    #[test]
    fn should_prefer_downloadclient() {
        assert_eq!(
            release_group(
                "Movie.Title.2008.WEB-DL-Wizzy",
                "Movie.Title.2008.WEB-DL-Drone",
                "Movie.Title.2008.WEB-DL-Viva"
            )
            .as_deref(),
            Some("Viva")
        );
    }

    #[test]
    fn should_prefer_folder() {
        assert_eq!(
            release_group(
                "Movie.Title.2008.WEB-DL-Wizzy",
                "Movie.Title.2008.WEB-DL-Drone",
                "Movie.Title.2008.WEB-DL"
            )
            .as_deref(),
            Some("Drone")
        );
    }

    #[test]
    fn should_fallback_to_file() {
        assert_eq!(
            release_group(
                "Movie.Title.2008.WEB-DL-Wizzy",
                "Movie.Title.2008.WEB-DL",
                "Movie.Title.2008.WEB-DL"
            )
            .as_deref(),
            Some("Wizzy")
        );
    }

    #[test]
    fn should_not_use_imdb_id() {
        assert_eq!(
            release_group(
                "Lock Stock and Two Smoking Barrels (1998) [imdb-tt0120735][Bluray-1080p][8bit][x264][DTS-HD MA 5.1]-FraMeSToR",
                "Lock Stock and Two Smoking Barrels (1998) {imdb-tt0120735}",
                "Movie.Title.2008.WEB-DL"
            )
            .as_deref(),
            Some("FraMeSToR")
        );
    }

    #[test]
    fn should_not_use_tmdb_id() {
        assert_eq!(
            release_group(
                "Lock Stock and Two Smoking Barrels (1998) [tmdb-100][Bluray-1080p][8bit][x264][DTS-HD MA 5.1]-FraMeSToR",
                "Lock Stock and Two Smoking Barrels (1998) {tmdb-100}",
                "Movie.Title.2008.WEB-DL"
            )
            .as_deref(),
            Some("FraMeSToR")
        );
    }

    // AggregateEdition has no fixture of its own.

    #[test]
    fn the_edition_is_the_clients_then_the_folders_then_the_files() {
        let mut local = LocalMovie {
            file_movie_info: Some(parsed("Movie.Title.2008.Extended.1080p.BluRay.x264-GRP")),
            folder_movie_info: Some(parsed("Movie.Title.2008.Directors.Cut.1080p.BluRay.x264-GRP")),
            download_client_movie_info: Some(parsed("Movie.Title.2008.1080p.BluRay.x264-GRP")),
            ..Default::default()
        };
        aggregate_edition(&mut local);
        assert_eq!(local.edition.as_deref(), Some("Directors Cut"));
        local.folder_movie_info = None;
        aggregate_edition(&mut local);
        assert_eq!(local.edition.as_deref(), Some("Extended"));
        // None names one: the file's empty edition, as the original leaves it.
        local.file_movie_info = Some(parsed("Movie.Title.2008.1080p.BluRay.x264-GRP"));
        aggregate_edition(&mut local);
        assert_eq!(local.edition.as_deref(), Some(""));
        let mut nothing = LocalMovie::default();
        aggregate_edition(&mut nothing);
        assert_eq!(nothing.edition, None);
    }

    // AggregateSubtitleInfoFixture

    fn movie_file(relative_path: &str, original_file_path: Option<&str>) -> MovieFile {
        MovieFile {
            id: 1,
            movie_id: 1,
            relative_path: relative_path.into(),
            original_file_path: original_file_path.map(String::from),
            size: 0,
            date_added: Utc::now(),
            scene_name: None,
            release_group: None,
            edition: None,
            quality: testing::quality(BLURAY1080P, 1),
            languages: vec![lang::ENGLISH],
            media_info: None,
            indexer_flags: 0,
        }
    }

    #[test]
    fn should_do_basic_parse() {
        const AAC: &str = "Name (2020)/Name (2020) - [AAC 2.0].mkv";
        const FLAC: &str = "Name (2020)/Name (2020) - [FLAC 2.0].mkv";
        for (relative_path, original_file_path, path, file_name_before_rename) in [
            (AAC, "", "Name (2020) - [AAC 2.0].default.eng.forced.ass", None),
            (AAC, "", "Name (2020) - [AAC 2.0].eng.default.ass", None),
            (AAC, "", "Name (2020) - [AAC 2.0].fra.ass", None),
            (
                "",
                AAC,
                "Name (2020) - [AAC 2.0].default.eng.forced.ass",
                Some(FLAC),
            ),
            ("", AAC, "Name (2020) - [AAC 2.0].eng.default.ass", None),
            ("", AAC, "Name (2020) - [AAC 2.0].fra.ass", None),
        ] {
            let file = movie_file(relative_path, Some(original_file_path));
            let info = clean_subtitle_title_info(&file, path, file_name_before_rename);
            assert_eq!(info.title, None, "{path}");
            assert_eq!(info.copy, 0, "{path}");
        }
    }

    /// A subtitle's language is read with Radarr's own tables: its
    /// numbers where they differ from Sonarr's, and the languages only it
    /// knows.
    #[test]
    fn the_language_of_a_subtitle_is_one_of_radarrs() {
        let file = movie_file("Name (2020)/Name (2020).mkv", None);
        for (path, language) in [
            ("Name (2020).en.srt", lang::ENGLISH),
            ("Name (2020).ro.srt", 27),
            ("Name (2020).rum.srt", 27),
            ("Name (2020).te.srt", 45),
            ("Name (2020).kan.forced.srt", 49),
            ("Name (2020).bn.srt", 34),
            ("Name (2020).Albanian.srt", 50),
            ("Name (2020).xx.srt", lang::UNKNOWN),
            ("Name (2020).srt", lang::UNKNOWN),
        ] {
            let info = clean_subtitle_title_info(&file, path, None);
            assert_eq!(info.language, language, "{path}");
        }
    }

    #[test]
    fn should_not_parse_default() {
        const RELATIVE: &str = "Default (2020)/Default (2020) - [AAC 2.0].mkv";
        for path in [
            "Default (2020) - [AAC 2.0].default.eng.forced.ass",
            "Default (2020) - [AAC 2.0].eng.default.ass",
            "Default (2020) - [AAC 2.0].default.eng.testtitle.forced.ass",
            "Default (2020) - [AAC 2.0].testtitle.eng.default.ass",
        ] {
            let info = clean_subtitle_title_info(&movie_file(RELATIVE, None), path, None);
            assert!(!info.language_tags.contains(&"default".to_string()), "{path}");
        }
    }

    #[test]
    fn a_subtitle_language_is_numbered_as_radarr_numbers_it() {
        let file = movie_file("Name (2020)/Name (2020).mkv", None);
        let info = clean_subtitle_title_info(&file, "Name (2020).fra.ass", None);
        assert_eq!(info.language, lang::FRENCH);
        // Hindi is 26 to Radarr and something else to Sonarr.
        let info = clean_subtitle_title_info(&file, "Name (2020).hin.ass", None);
        assert_eq!(info.language, lang::HINDI);
        let info = clean_subtitle_title_info(&file, "Name (2020).ass", None);
        assert_eq!(info.language, lang::UNKNOWN);
    }

    // GrabbedReleaseInfo, AggregateReleaseInfo and AugmentQualityFromReleaseName
    // against the store.

    fn grab(id: i64, movie_id: i64, title: &str, date: DateTime<Utc>) -> MovieHistoryEntry {
        MovieHistoryEntry {
            id,
            movie_id,
            source_title: Some(title.into()),
            date,
            event_type: MovieHistoryEvent::Grabbed,
            quality: testing::quality(WEBDL1080P, 1),
            languages: vec![lang::ENGLISH],
            data: json!({"indexer": "Indexer", "size": "1234", "indexerFlags": "G_Freeleech, G_Scene"}),
            download_id: Some("DownloadId1".into()),
        }
    }

    #[test]
    fn a_grabbed_release_is_the_newest_grab_and_every_movie_grabbed() {
        let now = Utc::now();
        let grabbed = vec![
            grab(1, 1, "Old.Title.2008.1080p.WEB-DL-GRP", now - Duration::days(2)),
            grab(2, 2, "New.Title.2008.1080p.WEB-DL-GRP", now),
            grab(3, 1, "Same.Date.2008.1080p.WEB-DL-GRP", now),
        ];
        let release = grabbed_release(&grabbed);
        assert_eq!(release.title, "New.Title.2008.1080p.WEB-DL-GRP");
        assert_eq!(release.indexer.as_deref(), Some("Indexer"));
        assert_eq!(release.size, 1234);
        assert_eq!(
            release.indexer_flags,
            parse_indexer_flags("G_Freeleech, G_Scene").unwrap()
        );
        assert_ne!(release.indexer_flags, 0);
        assert_eq!(release.movie_ids, vec![1, 2]);
        let mut bare = grab(4, 3, "Bare", now);
        bare.data = json!({});
        let release = grabbed_release(&[bare]);
        assert_eq!(
            (release.indexer, release.size, release.indexer_flags),
            (None, 0, 0)
        );
    }

    #[test]
    fn the_grab_history_gives_the_release_and_its_title_the_quality() {
        let db = Db::open_in_memory_for(Personality::Radarr).unwrap();
        let now: DateTime<Utc> = "2026-11-11T12:00:00Z".parse().unwrap();
        db.call_blocking(move |conn| {
            let movie = testing::movie(7, "Movie Title", 2008);
            store::insert_metadata(conn, &movie.metadata)?;
            store::insert_movie(conn, &movie)?;
            let title = "Movie.Title.2008.1080p.BluRay.x264-GRP";
            let mut history = grab(1, movie.id, title, now);
            history.id = store::next_history_id(conn)?;
            store::insert_history(conn, &history)?;
            let mut imported = grab(0, movie.id, title, now);
            imported.id = store::next_history_id(conn)?;
            imported.event_type = MovieHistoryEvent::DownloadFolderImported;
            store::insert_history(conn, &imported)?;
            let download =
                |event_type: i64, source_title: &str, date: DateTime<Utc>| MovieDownloadHistoryEntry {
                    id: 0,
                    event_type,
                    movie_id: movie.id,
                    download_id: "DownloadId1".into(),
                    source_title: source_title.into(),
                    date,
                    protocol: Protocol::Usenet,
                    indexer_id: 1,
                    download_client_id: 1,
                    release: None,
                    data: json!({}),
                };
            store::add_download_history(conn, &download(download_event::GRABBED, title, now))?;
            // A newer row that is not a grab is not the latest grab.
            store::add_download_history(
                conn,
                &download(
                    download_event::IMPORTED,
                    "Movie.Title.2008.720p.HDTV.x264-GRP",
                    now + Duration::hours(1),
                ),
            )?;
            assert_eq!(latest_grab_title(conn, "DownloadId1")?.as_deref(), Some(title));
            assert_eq!(latest_grab_title(conn, "Other")?, None);

            // A file whose own name says nothing of its quality.
            let mut local = LocalMovie {
                path: "/Test/Unsorted/Movie.Title.2008/movie.mkv".into(),
                movie: Some(movie.clone()),
                file_movie_info: Some(parsed("Movie.Title.2008")),
                ..Default::default()
            };
            let item = item("DownloadId1", title);
            augment(conn, &mut local, Some(&item), None).expect("augments");
            assert_eq!(local.quality.quality_id, BLURAY1080P);
            let release = local.release.clone().expect("a grabbed release");
            assert_eq!(release.title, title);
            assert_eq!(release.movie_ids, vec![movie.id]);
            assert_eq!(local.movie.as_ref().map(|m| m.id), Some(movie.id));
            assert_eq!(local.languages, vec![movie.metadata.original_language_id]);
            assert_eq!(local.scene_name, None, "not a scene source");

            // Without the client's item the history is not asked.
            let mut local = LocalMovie {
                path: "/Test/Unsorted/Movie.Title.2008/movie.mkv".into(),
                movie: Some(movie),
                file_movie_info: Some(parsed("Movie.Title.2008")),
                ..Default::default()
            };
            augment(conn, &mut local, None, None).expect("augments");
            assert!(local.release.is_none());
            assert_ne!(local.quality.quality_id, BLURAY1080P);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn a_media_file_nothing_parses_for_cannot_be_augmented() {
        let db = Db::open_in_memory_for(Personality::Radarr).unwrap();
        db.call_blocking(|conn| {
            let mut local = LocalMovie {
                path: "/Test/Unsorted/aaaaa.mkv".into(),
                movie: Some(testing::movie(1, "Movie Title", 2008)),
                ..Default::default()
            };
            let error = augment(conn, &mut local, None, None).unwrap_err();
            assert_eq!(
                error.0,
                "Unable to parse movie info from path: /Test/Unsorted/aaaaa.mkv"
            );
            // A file that is no media file is augmented all the same.
            local.path = "/Test/Unsorted/aaaaa.nfo".into();
            assert!(augment(conn, &mut local, None, None).is_ok());
            Ok(())
        })
        .unwrap();
    }
}
