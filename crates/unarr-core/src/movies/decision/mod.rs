//! Radarr's decision engine: a release parsed, mapped to a movie of the
//! library, weighed by the original's specifications and ranked. The twin
//! of `crate::decision`, which does the same for series and episodes; the
//! two share what carries no media notion (the profile comparisons of
//! `decision::upgradable`, the catalogue of profiles and settings, the
//! term matcher, the size helpers) and nothing else, since a movie has no
//! seasons, packs or scene numbering and an episode has no availability,
//! edition or wanted language.

pub mod engine;
pub mod library;
pub mod mapping;
pub mod specs;
pub mod testing;
#[cfg(test)]
mod tests;

use crate::decision::{RejectionKind, ReleaseSource, SeedConfiguration};
use crate::indexers::rss::ReleaseInfo;
use crate::model::CustomFormat;
use crate::movies::model::Movie;
use crate::movies::parser::ParsedMovieInfo;

pub use engine::{decide, prioritize, MovieCatalog};
pub use library::{LibraryMovie, MovieLookup, StoreMovieLookup};

/// Radarr's `DownloadRejectionReason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MovieRejectionReason {
    Unknown,
    UnknownMovie,
    UnableToParse,
    Error,
    DecisionError,
    Availability,
    MinimumAgeDelay,
    MinimumAgeDelayPushed,
    MovieNotMonitored,
    HistoryRecentCutoffMet,
    HistoryCdhDisabledCutoffMet,
    HistoryHigherPreference,
    HistoryHigherRevision,
    HistoryCutoffMet,
    HistoryCustomFormatCutoffMet,
    HistoryCustomFormatScore,
    HistoryCustomFormatScoreIncrement,
    HistoryUpgradesNotAllowed,
    NoMatchingTag,
    PropersDisabled,
    ProperForOldFile,
    WrongMovie,
    UnknownRuntime,
    BelowMinimumSize,
    AboveMaximumSize,
    AlreadyImportedSameHash,
    AlreadyImportedSameName,
    IndexerDisabled,
    Blocklisted,
    CustomFormatMinimumScore,
    MinimumFreeSpace,
    HardcodeSubtitles,
    WantedLanguage,
    MaximumSizeExceeded,
    MinimumAge,
    MaximumAge,
    Sample,
    ProtocolDisabled,
    QualityNotWanted,
    QualityUpgradesDisabled,
    QueueHigherPreference,
    QueueHigherRevision,
    QueueCutoffMet,
    QueueCustomFormatCutoffMet,
    QueueCustomFormatScore,
    QueueCustomFormatScoreIncrement,
    QueueUpgradesNotAllowed,
    QueuePropersDisabled,
    Raw,
    MustContainMissing,
    MustNotContainPresent,
    RepackDisabled,
    RepackUnknownReleaseGroup,
    RepackReleaseGroupDoesNotMatch,
    RequiredFlags,
    MinimumSeeders,
    DiskHigherPreference,
    DiskHigherRevision,
    DiskCutoffMet,
    DiskCustomFormatCutoffMet,
    DiskCustomFormatScore,
    DiskCustomFormatScoreIncrement,
    DiskUpgradesNotAllowed,
}

/// `DownloadRejection`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MovieRejection {
    pub reason: MovieRejectionReason,
    pub message: String,
    pub kind: RejectionKind,
}

impl MovieRejection {
    pub fn permanent(reason: MovieRejectionReason, message: impl Into<String>) -> MovieRejection {
        MovieRejection {
            reason,
            message: message.into(),
            kind: RejectionKind::Permanent,
        }
    }

    pub fn temporary(reason: MovieRejectionReason, message: impl Into<String>) -> MovieRejection {
        MovieRejection {
            reason,
            message: message.into(),
            kind: RejectionKind::Temporary,
        }
    }
}

/// `MovieMatchType`: how a release found its movie.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MovieMatchType {
    #[default]
    Unknown,
    Title,
    Alias,
    Id,
}

impl MovieMatchType {
    pub fn as_str(self) -> &'static str {
        match self {
            MovieMatchType::Unknown => "unknown",
            MovieMatchType::Title => "title",
            MovieMatchType::Alias => "alias",
            MovieMatchType::Id => "id",
        }
    }

    pub fn parse(s: &str) -> Option<MovieMatchType> {
        [
            MovieMatchType::Unknown,
            MovieMatchType::Title,
            MovieMatchType::Alias,
            MovieMatchType::Id,
        ]
        .into_iter()
        .find(|m| m.as_str().eq_ignore_ascii_case(s))
    }
}

/// `RemoteMovie`: a release once the engine has looked at it.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteMovie {
    pub release: ReleaseInfo,
    /// `None` when the title could not be parsed at all.
    pub parsed: Option<ParsedMovieInfo>,
    pub custom_formats: Vec<CustomFormat>,
    pub custom_format_score: i64,
    pub match_type: MovieMatchType,
    pub movie: Option<Movie>,
    pub movie_requested: bool,
    pub download_allowed: bool,
    pub seed_configuration: Option<SeedConfiguration>,
    pub languages: Vec<i64>,
    pub release_source: ReleaseSource,
}

impl RemoteMovie {
    pub fn new(release: ReleaseInfo) -> RemoteMovie {
        RemoteMovie {
            release,
            parsed: None,
            custom_formats: Vec::new(),
            custom_format_score: 0,
            match_type: MovieMatchType::Unknown,
            movie: None,
            movie_requested: false,
            download_allowed: false,
            seed_configuration: None,
            languages: Vec::new(),
            release_source: ReleaseSource::Unknown,
        }
    }
}

/// `DownloadDecision`.
#[derive(Debug, Clone, PartialEq)]
pub struct MovieDecision {
    pub remote: RemoteMovie,
    pub rejections: Vec<MovieRejection>,
}

impl MovieDecision {
    pub fn approved(&self) -> bool {
        self.rejections.is_empty()
    }

    pub fn temporarily_rejected(&self) -> bool {
        !self.rejections.is_empty() && self.rejections.iter().all(|r| r.kind == RejectionKind::Temporary)
    }

    pub fn rejected(&self) -> bool {
        self.rejections.iter().any(|r| r.kind == RejectionKind::Permanent)
    }
}

/// `MovieSearchCriteria`, as the engine reads it: the movie searched for,
/// with the titles it is known by.
#[derive(Debug, Clone, PartialEq)]
pub struct MovieSearch {
    pub movie: LibraryMovie,
    pub user_invoked: bool,
    pub interactive: bool,
}

/// A release in the download queue, as `QueueSpecification` sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct QueuedMovie {
    pub remote: RemoteMovie,
    pub size: i64,
    /// `TrackedDownloadState.FailedPending`: skipped by the queue check.
    pub failed_pending: bool,
}
