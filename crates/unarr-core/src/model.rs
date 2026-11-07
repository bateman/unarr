//! Domain entities. Field names follow the originals where the concept is the
//! same, so the importer and the API layer read as mappings, not translations.
//!
//! Enumerations serialise to the camelCase strings the compatible API uses;
//! the same strings are what the database stores.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Declare a string-backed enumeration with its API/database spelling.
macro_rules! string_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident = $text:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum $name {
            $(#[serde(rename = $text)] $variant),+
        }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            pub const fn as_str(self) -> &'static str {
                match self { $($name::$variant => $text),+ }
            }

            pub fn parse(s: &str) -> Option<Self> {
                match s { $($text => Some($name::$variant),)+ _ => None }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

string_enum! {
    /// Whether a series is still producing episodes.
    SeriesStatus { Continuing = "continuing", Ended = "ended", Upcoming = "upcoming", Deleted = "deleted" }
}

string_enum! {
    /// How episodes are numbered and named.
    SeriesType { Standard = "standard", Daily = "daily", Anime = "anime" }
}

string_enum! {
    /// Whether newly discovered seasons are monitored.
    NewItemMonitor { All = "all", None = "none" }
}

string_enum! {
    /// What a release contained.
    ReleaseType {
        Unknown = "unknown",
        SingleEpisode = "singleEpisode",
        MultiEpisode = "multiEpisode",
        SeasonPack = "seasonPack",
        MultiSeasonPack = "multiSeasonPack",
    }
}

string_enum! {
    /// Download protocol.
    Protocol { Unknown = "unknown", Usenet = "usenet", Torrent = "torrent" }
}

string_enum! {
    /// Kind of history entry.
    HistoryEvent {
        Unknown = "unknown",
        Grabbed = "grabbed",
        SeriesFolderImported = "seriesFolderImported",
        DownloadFolderImported = "downloadFolderImported",
        DownloadFailed = "downloadFailed",
        EpisodeFileDeleted = "episodeFileDeleted",
        EpisodeFileRenamed = "episodeFileRenamed",
        DownloadIgnored = "downloadIgnored",
    }
}

string_enum! {
    /// Families of pluggable providers that share one storage shape.
    ProviderKind {
        Indexer = "indexer",
        DownloadClient = "downloadClient",
        Notification = "notification",
        ImportList = "importList",
        Metadata = "metadata",
    }
}

impl HistoryEvent {
    /// The originals number the event types in their database and API.
    pub fn from_number(n: i64) -> Option<HistoryEvent> {
        Some(match n {
            1 => HistoryEvent::Grabbed,
            2 => HistoryEvent::SeriesFolderImported,
            3 => HistoryEvent::DownloadFolderImported,
            4 => HistoryEvent::DownloadFailed,
            5 => HistoryEvent::EpisodeFileDeleted,
            6 => HistoryEvent::EpisodeFileRenamed,
            7 => HistoryEvent::DownloadIgnored,
            _ => return None,
        })
    }
}

/// A quality plus its revision (proper/repack counters).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct QualityModel {
    pub quality_id: i64,
    pub version: i64,
    pub real: i64,
    pub is_repack: bool,
}

/// Community rating.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Ratings {
    pub votes: i64,
    pub value: f64,
}

/// A cover image as the originals record it: kind plus the remote URL. The
/// local `/MediaCover/...` URL is derived when serving.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Image {
    pub cover_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Series {
    pub id: i64,
    pub tvdb_id: i64,
    pub tv_rage_id: i64,
    pub tv_maze_id: i64,
    pub tmdb_id: i64,
    pub imdb_id: Option<String>,
    pub title: String,
    pub title_slug: String,
    pub clean_title: String,
    pub sort_title: Option<String>,
    pub status: SeriesStatus,
    pub overview: Option<String>,
    pub air_time: Option<String>,
    pub network: Option<String>,
    pub certification: Option<String>,
    pub images: Vec<Image>,
    pub genres: Vec<String>,
    /// Cast, kept as the originals' JSON (`name`, `character`, `images`).
    pub actors: Value,
    pub path: String,
    pub monitored: bool,
    pub season_folder: bool,
    pub monitor_new_items: NewItemMonitor,
    pub use_scene_numbering: bool,
    pub runtime: i64,
    pub series_type: SeriesType,
    pub year: i64,
    pub first_aired: Option<DateTime<Utc>>,
    pub last_aired: Option<DateTime<Utc>>,
    pub quality_profile_id: i64,
    pub original_language_id: i64,
    pub ratings: Ratings,
    pub added: DateTime<Utc>,
    pub add_options: Option<Value>,
    pub last_info_sync: Option<DateTime<Utc>>,
    pub last_disk_sync: Option<DateTime<Utc>>,
    pub mal_ids: Option<Value>,
    pub anilist_ids: Option<Value>,
    pub seasons: Vec<Season>,
    pub tags: Vec<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Season {
    pub season_number: i64,
    pub monitored: bool,
    pub images: Vec<Image>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Episode {
    pub id: i64,
    pub series_id: i64,
    pub tvdb_id: i64,
    pub season_number: i64,
    pub episode_number: i64,
    pub title: Option<String>,
    pub overview: Option<String>,
    /// 0 when the episode has no file.
    pub episode_file_id: i64,
    pub absolute_episode_number: Option<i64>,
    pub scene_absolute_episode_number: Option<i64>,
    pub scene_season_number: Option<i64>,
    pub scene_episode_number: Option<i64>,
    pub unverified_scene_numbering: bool,
    pub monitored: bool,
    pub air_date: Option<NaiveDate>,
    pub air_date_utc: Option<DateTime<Utc>>,
    pub runtime: i64,
    pub finale_type: Option<String>,
    pub images: Vec<Image>,
    pub ratings: Ratings,
    pub last_search_time: Option<DateTime<Utc>>,
    pub aired_after_season_number: Option<i64>,
    pub aired_before_season_number: Option<i64>,
    pub aired_before_episode_number: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EpisodeFile {
    pub id: i64,
    pub series_id: i64,
    pub season_number: i64,
    pub relative_path: String,
    pub original_file_path: Option<String>,
    pub size: i64,
    pub date_added: DateTime<Utc>,
    pub scene_name: Option<String>,
    pub release_group: Option<String>,
    pub release_hash: Option<String>,
    pub quality: QualityModel,
    pub languages: Vec<i64>,
    /// ffprobe-derived media information, kept as the originals' JSON.
    pub media_info: Option<Value>,
    pub indexer_flags: i64,
    pub release_type: ReleaseType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    pub id: i64,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootFolder {
    pub id: i64,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QualityDefinition {
    pub id: i64,
    pub quality_id: i64,
    pub title: String,
    pub min_size: Option<f64>,
    pub max_size: Option<f64>,
    pub preferred_size: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CustomFormat {
    pub id: i64,
    pub name: String,
    pub include_when_renaming: bool,
    /// Specification list as the originals store it: `[{type, body}]`.
    pub specifications: Value,
}

/// One entry of a quality profile: a single quality or a named group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileItem {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<i64>,
    #[serde(default)]
    pub items: Vec<ProfileItem>,
    pub allowed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileFormatItem {
    pub format: i64,
    pub score: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualityProfile {
    pub id: i64,
    pub name: String,
    pub cutoff: i64,
    pub upgrade_allowed: bool,
    pub items: Vec<ProfileItem>,
    pub format_items: Vec<ProfileFormatItem>,
    pub min_format_score: i64,
    pub cutoff_format_score: i64,
    pub min_upgrade_format_score: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HistoryEntry {
    pub id: i64,
    pub series_id: i64,
    pub episode_id: i64,
    pub source_title: Option<String>,
    pub date: DateTime<Utc>,
    pub event_type: HistoryEvent,
    pub quality: QualityModel,
    pub languages: Vec<i64>,
    /// Free-form string map, as the originals keep it.
    pub data: Value,
    pub download_id: Option<String>,
}

/// An indexer, download client, notification, import list or metadata
/// consumer. `attributes` carries the per-kind flags (`enableRss`, `onGrab`,
/// `priority`...) under their API names.
#[derive(Debug, Clone, PartialEq)]
pub struct Provider {
    pub kind: ProviderKind,
    pub id: i64,
    pub name: String,
    pub implementation: String,
    pub config_contract: String,
    pub settings: Value,
    pub tags: Vec<i64>,
    pub attributes: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamingConfig {
    pub rename_episodes: bool,
    pub replace_illegal_characters: bool,
    pub colon_replacement_format: i64,
    pub custom_colon_replacement_format: String,
    pub multi_episode_style: i64,
    pub standard_episode_format: String,
    pub daily_episode_format: String,
    pub anime_episode_format: String,
    pub series_folder_format: String,
    pub season_folder_format: String,
    pub specials_folder_format: String,
}

impl Default for NamingConfig {
    /// Sonarr's defaults.
    fn default() -> Self {
        NamingConfig {
            rename_episodes: false,
            replace_illegal_characters: true,
            colon_replacement_format: 4,
            custom_colon_replacement_format: String::new(),
            multi_episode_style: 5,
            standard_episode_format:
                "{Series Title} - S{season:00}E{episode:00} - {Episode Title} {Quality Full}".into(),
            daily_episode_format: "{Series Title} - {Air-Date} - {Episode Title} {Quality Full}".into(),
            anime_episode_format:
                "{Series Title} - S{season:00}E{episode:00} - {Episode Title} {Quality Full}".into(),
            series_folder_format: "{Series Title}".into(),
            season_folder_format: "Season {season}".into(),
            specials_folder_format: "Specials".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelayProfile {
    pub id: i64,
    pub enable_usenet: bool,
    pub enable_torrent: bool,
    pub preferred_protocol: Protocol,
    pub usenet_delay: i64,
    pub torrent_delay: i64,
    pub order: i64,
    pub tags: Vec<i64>,
    pub bypass_if_highest_quality: bool,
    pub bypass_if_above_custom_format_score: bool,
    pub minimum_custom_format_score: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseProfile {
    pub id: i64,
    pub name: Option<String>,
    pub enabled: bool,
    pub required: Vec<String>,
    pub ignored: Vec<String>,
    pub indexer_id: i64,
    pub tags: Vec<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemotePathMapping {
    pub id: i64,
    pub host: String,
    pub remote_path: String,
    pub local_path: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CustomFilter {
    pub id: i64,
    pub filter_type: String,
    pub label: String,
    pub filters: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportListExclusion {
    pub id: i64,
    pub tvdb_id: i64,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AutoTagging {
    pub id: i64,
    pub name: String,
    pub remove_tags_automatically: bool,
    pub tags: Vec<i64>,
    pub specifications: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub id: i64,
    pub identifier: String,
    pub username: String,
    pub password: String,
    pub salt: Option<String>,
    pub iterations: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduledTask {
    pub id: i64,
    pub type_name: String,
    pub interval_minutes: i64,
    pub last_execution: Option<DateTime<Utc>>,
    pub last_start_time: Option<DateTime<Utc>>,
}

/// An alternate title from the scene-mapping services (TheXEM, Servarr).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneMapping {
    pub id: i64,
    pub tvdb_id: i64,
    pub season_number: Option<i64>,
    pub search_term: Option<String>,
    pub parse_term: Option<String>,
    pub title: String,
    pub mapping_type: Option<String>,
    pub scene_season_number: Option<i64>,
    pub filter_regex: Option<String>,
    pub scene_origin: Option<String>,
    pub search_mode: Option<i64>,
    pub comment: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BlocklistEntry {
    pub id: i64,
    pub series_id: i64,
    pub episode_ids: Vec<i64>,
    pub source_title: Option<String>,
    pub quality: QualityModel,
    pub date: DateTime<Utc>,
    pub published_date: Option<DateTime<Utc>>,
    pub size: Option<i64>,
    pub protocol: Protocol,
    pub indexer: Option<String>,
    pub message: Option<String>,
    pub torrent_info_hash: Option<String>,
    pub languages: Vec<i64>,
    pub indexer_flags: i64,
    pub release_type: ReleaseType,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enums_spell_like_the_api() {
        assert_eq!(
            HistoryEvent::DownloadFolderImported.as_str(),
            "downloadFolderImported"
        );
        assert_eq!(
            HistoryEvent::parse("episodeFileDeleted"),
            Some(HistoryEvent::EpisodeFileDeleted)
        );
        assert_eq!(
            serde_json::to_value(SeriesType::Anime).unwrap(),
            serde_json::json!("anime")
        );
        assert_eq!(ReleaseType::parse("nope"), None);
    }
}
