//! The decision engine: a release is parsed, mapped to a series and its
//! episodes, scored against the custom formats, and then run through the
//! originals' specifications (`DownloadDecisionMaker`); approved decisions
//! are ordered by preference (`DownloadDecisionPriorizationService`).
//!
//! Everything here is synchronous and reads through [`lookup::Lookup`], so a
//! batch of releases is decided inside one database call, and tests run on
//! an in-memory lookup.

pub mod engine;
pub mod formats;
pub mod lookup;
pub mod mapping;
pub mod scene;
pub mod specs;
pub mod testing;
pub mod upgradable;

use chrono::{DateTime, Utc};

use crate::indexers::rss::ReleaseInfo;
use crate::model::{CustomFormat, Episode, SceneMapping, Series};
use crate::parser::title::ParsedEpisodeInfo;

pub use engine::{decide, prioritize, DecisionInput};
pub use lookup::{
    Catalog, EngineConfig, IndexerInfo, Lookup, ProperDownloadTypes, QueueSource, QueuedRelease, StoreLookup,
};

/// `DownloadRejectionReason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectionReason {
    Unknown,
    UnknownSeries,
    UnknownEpisode,
    MatchesAnotherSeries,
    UnableToParse,
    Error,
    DecisionError,
    MinimumAgeDelay,
    SeriesNotMonitored,
    EpisodeNotMonitored,
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
    WrongEpisode,
    WrongSeason,
    WrongSeries,
    FullSeason,
    UnknownRuntime,
    BelowMinimumSize,
    AboveMaximumSize,
    AlreadyImportedSameHash,
    AlreadyImportedSameName,
    UnknownReleaseGroup,
    ReleaseGroupDoesNotMatch,
    IndexerDisabled,
    Blocklisted,
    CustomFormatMinimumScore,
    MinimumFreeSpace,
    FullSeasonNotAired,
    MaximumSizeExceeded,
    MinimumAge,
    MaximumAge,
    MultiSeason,
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
    ExistingFileHasMoreEpisodes,
    AmbiguousNumbering,
    NotSeasonPack,
    SplitEpisode,
    MinimumSeeders,
    DiskHigherPreference,
    DiskHigherRevision,
    DiskCutoffMet,
    DiskCustomFormatCutoffMet,
    DiskCustomFormatScore,
    DiskCustomFormatScoreIncrement,
    DiskUpgradesNotAllowed,
}

/// `RejectionType`: a temporary rejection keeps the release pending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectionKind {
    Permanent,
    Temporary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejection {
    pub reason: RejectionReason,
    pub message: String,
    pub kind: RejectionKind,
}

impl Rejection {
    pub fn permanent(reason: RejectionReason, message: impl Into<String>) -> Rejection {
        Rejection {
            reason,
            message: message.into(),
            kind: RejectionKind::Permanent,
        }
    }

    pub fn temporary(reason: RejectionReason, message: impl Into<String>) -> Rejection {
        Rejection {
            reason,
            message: message.into(),
            kind: RejectionKind::Temporary,
        }
    }
}

impl std::fmt::Display for Rejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self.kind {
            RejectionKind::Permanent => "Permanent",
            RejectionKind::Temporary => "Temporary",
        };
        write!(f, "[{kind}] {}", self.message)
    }
}

/// `ReleaseSourceType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReleaseSource {
    #[default]
    Unknown,
    Rss,
    Search,
    UserInvokedSearch,
    InteractiveSearch,
    ReleasePush,
}

impl ReleaseSource {
    pub fn as_str(self) -> &'static str {
        match self {
            ReleaseSource::Unknown => "unknown",
            ReleaseSource::Rss => "rss",
            ReleaseSource::Search => "search",
            ReleaseSource::UserInvokedSearch => "userInvokedSearch",
            ReleaseSource::InteractiveSearch => "interactiveSearch",
            ReleaseSource::ReleasePush => "releasePush",
        }
    }

    pub fn parse(s: &str) -> Option<ReleaseSource> {
        [
            ReleaseSource::Unknown,
            ReleaseSource::Rss,
            ReleaseSource::Search,
            ReleaseSource::UserInvokedSearch,
            ReleaseSource::InteractiveSearch,
            ReleaseSource::ReleasePush,
        ]
        .into_iter()
        .find(|r| r.as_str().eq_ignore_ascii_case(s))
    }
}

/// `SeriesMatchType`: how the series was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SeriesMatchType {
    #[default]
    Unknown,
    Title,
    Alias,
    Id,
}

impl SeriesMatchType {
    pub fn as_str(self) -> &'static str {
        match self {
            SeriesMatchType::Unknown => "unknown",
            SeriesMatchType::Title => "title",
            SeriesMatchType::Alias => "alias",
            SeriesMatchType::Id => "id",
        }
    }

    pub fn parse(s: &str) -> Option<SeriesMatchType> {
        [
            SeriesMatchType::Unknown,
            SeriesMatchType::Title,
            SeriesMatchType::Alias,
            SeriesMatchType::Id,
        ]
        .into_iter()
        .find(|r| r.as_str().eq_ignore_ascii_case(s))
    }
}

/// `TorrentSeedConfiguration`: what the download client is told to seed to.
#[derive(Debug, Clone, PartialEq)]
pub struct SeedConfiguration {
    pub ratio: Option<f64>,
    pub seed_time_minutes: Option<i64>,
}

/// `RemoteEpisode`: a release once the engine has looked at it.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteEpisode {
    pub release: ReleaseInfo,
    /// `None` when the title could not be parsed at all.
    pub parsed: Option<ParsedEpisodeInfo>,
    pub scene_mapping: Option<SceneMapping>,
    pub mapped_season_number: i64,
    pub series: Option<Series>,
    pub episodes: Vec<Episode>,
    pub episode_requested: bool,
    pub download_allowed: bool,
    pub seed_configuration: Option<SeedConfiguration>,
    pub custom_formats: Vec<CustomFormat>,
    pub custom_format_score: i64,
    pub series_match_type: SeriesMatchType,
    pub languages: Vec<i64>,
    pub release_source: ReleaseSource,
}

impl RemoteEpisode {
    pub fn new(release: ReleaseInfo) -> RemoteEpisode {
        RemoteEpisode {
            release,
            parsed: None,
            scene_mapping: None,
            mapped_season_number: 0,
            series: None,
            episodes: Vec::new(),
            episode_requested: false,
            download_allowed: false,
            seed_configuration: None,
            custom_formats: Vec::new(),
            custom_format_score: 0,
            series_match_type: SeriesMatchType::Unknown,
            languages: Vec::new(),
            release_source: ReleaseSource::Unknown,
        }
    }

    /// `IsRecentEpisode`: any episode aired in the last two weeks.
    pub fn is_recent_episode(&self, now: DateTime<Utc>) -> bool {
        let cutoff = now.date_naive().and_hms_opt(0, 0, 0).unwrap().and_utc() - chrono::Duration::days(14);
        self.episodes
            .iter()
            .any(|e| e.air_date_utc.is_some_and(|d| d >= cutoff))
    }

    /// The parsed info, or an empty one for the specs that read it anyway.
    pub fn parsed_or_default(&self) -> std::borrow::Cow<'_, ParsedEpisodeInfo> {
        match &self.parsed {
            Some(p) => std::borrow::Cow::Borrowed(p),
            None => std::borrow::Cow::Owned(ParsedEpisodeInfo::default()),
        }
    }
}

/// `DownloadDecision`.
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub remote: RemoteEpisode,
    pub rejections: Vec<Rejection>,
}

impl Decision {
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

/// What a search asked for (`SearchCriteriaBase` and its subclasses), as the
/// specifications read it.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchCriteria {
    pub series: Series,
    pub episodes: Vec<Episode>,
    pub kind: SearchKind,
    pub monitored_episodes_only: bool,
    pub user_invoked_search: bool,
    pub interactive_search: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SearchKind {
    SingleEpisode {
        season_number: i64,
        episode_number: i64,
    },
    Season {
        season_number: i64,
    },
    AnimeEpisode {
        season_number: i64,
        episode_number: i64,
        absolute_episode_number: i64,
        is_season_search: bool,
    },
    AnimeSeason {
        season_number: i64,
    },
    DailyEpisode {
        air_date: chrono::NaiveDate,
    },
    DailySeason {
        year: i64,
    },
    SpecialEpisode,
}

/// `1024 * 1024 * n`.
pub fn megabytes(n: i64) -> i64 {
    n * 1024 * 1024
}

/// `long.SizeSuffix()`: `"{0:n1} {1}"` in the invariant culture.
pub fn size_suffix(bytes: i64) -> String {
    if bytes < 0 {
        // `i64::MIN` has no negative: saturate instead of recursing for ever.
        return format!("-{}", size_suffix(bytes.saturating_neg()));
    }
    if bytes == 0 {
        return "0 B".to_string();
    }
    const SUFFIXES: [&str; 9] = ["B", "KB", "MB", "GB", "TB", "PB", "EB", "ZB", "YB"];
    let mag = ((bytes as f64).ln() / 1024f64.ln()).floor() as usize;
    let mag = mag.min(SUFFIXES.len() - 1);
    let adjusted = bytes as f64 / 1024f64.powi(mag as i32);
    format!("{} {}", thousands_one_decimal(adjusted), SUFFIXES[mag])
}

/// `{0:n1}`: one decimal, digit groups separated by commas.
fn thousands_one_decimal(value: f64) -> String {
    let rounded = (value * 10.0).round() / 10.0;
    let whole = rounded.trunc() as i64;
    let tenths = ((rounded - rounded.trunc()) * 10.0).round() as i64;
    let digits = whole.to_string();
    let mut grouped = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(c);
    }
    format!("{grouped}.{tenths}")
}

/// `QualityModel.ToString()`: `"{name} v{version}[ Real:{real}]"`.
pub fn quality_model_string(q: &crate::model::QualityModel) -> String {
    let mut s = format!(
        "{} v{}",
        crate::quality::Quality::by_id(q.quality_id).name,
        q.version
    );
    if q.real > 0 {
        s.push_str(&format!(" Real:{}", q.real));
    }
    s
}

/// `Revision.ToString()`.
pub fn revision_string(q: &crate::model::QualityModel) -> String {
    let mut s = format!("v{}", q.version);
    if q.real > 0 {
        s.push_str(&format!(" Real:{}", q.real));
    }
    s
}

/// `QualityProfileQualityItem.ToString()`.
pub fn profile_item_string(item: &crate::model::ProfileItem) -> String {
    let qualities: Vec<&str> = crate::profile::qualities_of(item)
        .into_iter()
        .map(|id| crate::quality::Quality::by_id(id).name)
        .collect();
    let joined = qualities.join(", ");
    match item.name.as_deref().filter(|n| !n.trim().is_empty()) {
        Some(name) => format!("{name} ({joined})"),
        None => joined,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_format_like_the_originals() {
        assert_eq!(size_suffix(0), "0 B");
        assert_eq!(size_suffix(1023), "1,023.0 B");
        assert_eq!(size_suffix(megabytes(250)), "250.0 MB");
        assert_eq!(size_suffix(megabytes(1500)), "1.5 GB");
        assert_eq!(size_suffix(1_234_567_890_123), "1.1 TB");
        assert_eq!(size_suffix(-megabytes(1)), "-1.0 MB");
        // `-i64::MIN` is `i64::MIN` again: no endless recursion.
        assert!(size_suffix(i64::MIN).starts_with("-8.0 EB"));
    }

    #[test]
    fn quality_strings() {
        let q = crate::model::QualityModel {
            quality_id: 3,
            version: 2,
            real: 0,
            is_repack: false,
        };
        assert_eq!(quality_model_string(&q), "WEBDL-1080p v2");
        assert_eq!(revision_string(&q), "v2");
    }
}
