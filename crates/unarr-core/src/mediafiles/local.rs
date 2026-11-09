//! `LocalEpisode`, `ImportDecision`, `ImportRejection`, `ImportResult`: the
//! shapes the import pipeline passes along.

use serde_json::Value;

use crate::downloadclients::DownloadClientItem;
use crate::model::{CustomFormat, Episode, EpisodeFile, QualityModel, ReleaseType, Series};
use crate::parser::title::ParsedEpisodeInfo;

use super::mediainfo::MediaInfo;

/// `ImportMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImportMode {
    #[default]
    Auto,
    Move,
    Copy,
}

impl ImportMode {
    pub fn parse(s: &str) -> ImportMode {
        match s.trim().to_ascii_lowercase().as_str() {
            "move" | "1" => ImportMode::Move,
            "copy" | "2" => ImportMode::Copy,
            _ => ImportMode::Auto,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ImportMode::Auto => "auto",
            ImportMode::Move => "move",
            ImportMode::Copy => "copy",
        }
    }
}

/// `DeleteMediaFileReason`, in the spelling the history `data` carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteReason {
    MissingFromDisk,
    Manual,
    Upgrade,
    NoLinkedEpisodes,
    ManualOverride,
}

impl DeleteReason {
    pub fn as_str(self) -> &'static str {
        match self {
            DeleteReason::MissingFromDisk => "MissingFromDisk",
            DeleteReason::Manual => "Manual",
            DeleteReason::Upgrade => "Upgrade",
            DeleteReason::NoLinkedEpisodes => "NoLinkedEpisodes",
            DeleteReason::ManualOverride => "ManualOverride",
        }
    }
}

/// `GrabbedReleaseInfo`: what the grab history says about a download.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GrabbedRelease {
    pub title: String,
    pub indexer: Option<String>,
    pub size: i64,
    pub release_type: ReleaseType,
    pub episode_ids: Vec<i64>,
}

/// `LocalEpisode`: one video file on its way into the library.
#[derive(Debug, Clone, Default)]
pub struct LocalEpisode {
    pub path: String,
    pub size: i64,
    pub file_episode_info: Option<ParsedEpisodeInfo>,
    pub download_client_episode_info: Option<ParsedEpisodeInfo>,
    pub download_item: Option<DownloadClientItem>,
    pub folder_episode_info: Option<ParsedEpisodeInfo>,
    pub series: Option<Series>,
    pub episodes: Vec<Episode>,
    pub old_files: Vec<DeletedEpisodeFile>,
    pub quality: QualityModel,
    pub languages: Vec<i64>,
    pub indexer_flags: i64,
    pub release_type: ReleaseType,
    pub media_info: Option<MediaInfo>,
    pub existing_file: bool,
    pub scene_source: bool,
    pub release_group: Option<String>,
    pub release_hash: Option<String>,
    pub scene_name: Option<String>,
    pub other_video_files: bool,
    pub custom_formats: Vec<CustomFormat>,
    pub custom_format_score: i64,
    pub release: Option<GrabbedRelease>,
    pub file_name_before_rename: Option<String>,
}

impl LocalEpisode {
    /// `SeasonNumber`: the one season the episodes share.
    pub fn season_number(&self) -> Result<i64, String> {
        let mut seasons: Vec<i64> = self.episodes.iter().map(|e| e.season_number).collect();
        seasons.sort_unstable();
        seasons.dedup();
        match seasons.as_slice() {
            [] => Err("Expected one season, but found none".into()),
            [one] => Ok(*one),
            many => Err(format!(
                "Expected one season, but found {} ({})",
                many.len(),
                many.iter().map(|s| s.to_string()).collect::<Vec<_>>().join(", ")
            )),
        }
    }

    pub fn is_special(&self) -> bool {
        self.season_number() == Ok(0)
    }

    pub fn series_ref(&self) -> &Series {
        self.series.as_ref().expect("a local episode carries its series")
    }
}

/// `DeletedEpisodeFile`: a file an import replaced, and where the recycle
/// bin put it.
#[derive(Debug, Clone, PartialEq)]
pub struct DeletedEpisodeFile {
    pub file: EpisodeFile,
    pub recycle_bin_path: Option<String>,
}

/// `ImportRejectionReason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectionReason {
    Unknown,
    FileLocked,
    UnknownSeries,
    DangerousFile,
    ExecutableFile,
    ArchiveFile,
    SeriesFolder,
    InvalidFilePath,
    UnsupportedExtension,
    PartialSeason,
    SeasonExtra,
    InvalidSeasonOrEpisode,
    UnableToParse,
    Error,
    DecisionError,
    NoEpisodes,
    MissingAbsoluteEpisodeNumber,
    EpisodeAlreadyImported,
    TitleMissing,
    TitleTba,
    MinimumFreeSpace,
    FullSeason,
    NoAudio,
    EpisodeUnexpected,
    EpisodeNotFoundInRelease,
    Sample,
    SampleIndeterminate,
    Unpacking,
    ExistingFileHasMoreEpisodes,
    SplitEpisode,
    UnverifiedSceneMapping,
    NotQualityUpgrade,
    NotRevisionUpgrade,
    NotCustomFormatUpgrade,
}

/// `ImportRejection`: every one the originals raise is permanent.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportRejection {
    pub reason: RejectionReason,
    pub message: String,
}

impl ImportRejection {
    pub fn new(reason: RejectionReason, message: impl Into<String>) -> ImportRejection {
        ImportRejection {
            reason,
            message: message.into(),
        }
    }
}

/// `ImportDecision`.
#[derive(Debug, Clone)]
pub struct ImportDecision {
    /// `None` for the results that have no file behind them (an unknown
    /// series folder, a mapped series folder).
    pub local_episode: Option<LocalEpisode>,
    pub rejections: Vec<ImportRejection>,
}

impl ImportDecision {
    pub fn new(local_episode: LocalEpisode, rejections: Vec<ImportRejection>) -> ImportDecision {
        ImportDecision {
            local_episode: Some(local_episode),
            rejections,
        }
    }

    pub fn approved(&self) -> bool {
        self.rejections.is_empty()
    }

    pub fn rejected(local_episode: Option<LocalEpisode>, rejection: ImportRejection) -> ImportDecision {
        ImportDecision {
            local_episode,
            rejections: vec![rejection],
        }
    }
}

/// `ImportResultType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportResultType {
    Imported,
    Rejected,
    Skipped,
}

/// `ImportResult`.
#[derive(Debug, Clone)]
pub struct ImportResult {
    pub decision: ImportDecision,
    pub errors: Vec<String>,
    pub episode_file: Option<EpisodeFile>,
}

impl ImportResult {
    pub fn imported(decision: ImportDecision, file: EpisodeFile) -> ImportResult {
        ImportResult {
            decision,
            errors: Vec::new(),
            episode_file: Some(file),
        }
    }

    pub fn failed(decision: ImportDecision, errors: Vec<String>) -> ImportResult {
        ImportResult {
            decision,
            errors,
            episode_file: None,
        }
    }

    /// `Result`: imported, rejected (by the decision) or skipped (approved
    /// but not imported).
    pub fn result(&self) -> ImportResultType {
        if self.episode_file.is_some() {
            ImportResultType::Imported
        } else if !self.decision.approved() {
            ImportResultType::Rejected
        } else {
            ImportResultType::Skipped
        }
    }
}

/// The media info JSON of a local episode, if a probe produced one.
pub fn media_info_json(local: &LocalEpisode) -> Option<Value> {
    local.media_info.as_ref().map(|m| m.model.clone())
}
