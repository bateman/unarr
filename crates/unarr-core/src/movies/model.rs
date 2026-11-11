//! Movie entities, named after the originals' where the concept is the same.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{Image, Protocol, QualityModel};

/// Declare a string-backed enumeration with its API/database spelling and
/// the number the originals store.
macro_rules! numbered_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident = $text:literal = $number:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum $name {
            $(#[serde(rename = $text)] $variant),+
        }

        impl $name {
            pub const fn as_str(self) -> &'static str {
                match self { $($name::$variant => $text),+ }
            }

            pub fn parse(s: &str) -> Option<Self> {
                match s { $($text => Some($name::$variant),)+ _ => None }
            }

            pub const fn number(self) -> i64 {
                match self { $($name::$variant => $number),+ }
            }

            pub fn from_number(n: i64) -> Option<Self> {
                match n { $($number => Some($name::$variant),)+ _ => None }
            }
        }
    };
}

numbered_enum! {
    /// `MovieStatusType`: how far a movie's release has come. Also what
    /// "minimum availability" is expressed in.
    MovieStatus {
        Deleted = "deleted" = -1,
        Tba = "tba" = 0,
        Announced = "announced" = 1,
        InCinemas = "inCinemas" = 2,
        Released = "released" = 3,
    }
}

numbered_enum! {
    /// `MovieHistoryEventType`; the gaps are the originals'.
    MovieHistoryEvent {
        Unknown = "unknown" = 0,
        Grabbed = "grabbed" = 1,
        DownloadFolderImported = "downloadFolderImported" = 3,
        DownloadFailed = "downloadFailed" = 4,
        MovieFileDeleted = "movieFileDeleted" = 6,
        MovieFolderImported = "movieFolderImported" = 7,
        MovieFileRenamed = "movieFileRenamed" = 8,
        DownloadIgnored = "downloadIgnored" = 9,
    }
}

/// What the metadata server knows about a movie (`MovieMetadata`).
#[derive(Debug, Clone, PartialEq)]
pub struct MovieMetadata {
    pub id: i64,
    pub tmdb_id: i64,
    pub imdb_id: Option<String>,
    pub title: String,
    pub sort_title: Option<String>,
    pub clean_title: Option<String>,
    pub original_title: Option<String>,
    pub clean_original_title: Option<String>,
    pub original_language_id: i64,
    pub status: MovieStatus,
    pub overview: Option<String>,
    pub images: Vec<Image>,
    pub genres: Vec<String>,
    pub keywords: Vec<String>,
    /// `{imdb, tmdb, metacritic, rottenTomatoes, trakt}`, each `{votes, value, type}`.
    pub ratings: Value,
    pub recommendations: Vec<i64>,
    pub runtime: i64,
    pub in_cinemas: Option<DateTime<Utc>>,
    pub physical_release: Option<DateTime<Utc>>,
    pub digital_release: Option<DateTime<Utc>>,
    pub year: i64,
    pub secondary_year: Option<i64>,
    pub certification: Option<String>,
    pub youtube_trailer_id: Option<String>,
    pub studio: Option<String>,
    pub website: Option<String>,
    pub popularity: f64,
    pub collection_tmdb_id: i64,
    pub collection_title: Option<String>,
    pub last_info_sync: Option<DateTime<Utc>>,
}

/// A movie of the library, with its metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct Movie {
    pub id: i64,
    pub metadata: MovieMetadata,
    pub path: String,
    pub monitored: bool,
    pub quality_profile_id: i64,
    pub minimum_availability: MovieStatus,
    pub movie_file_id: i64,
    pub tags: Vec<i64>,
    pub added: Option<DateTime<Utc>>,
    pub add_options: Option<Value>,
    pub last_search_time: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MovieFile {
    pub id: i64,
    pub movie_id: i64,
    pub relative_path: String,
    pub original_file_path: Option<String>,
    pub size: i64,
    pub date_added: DateTime<Utc>,
    pub scene_name: Option<String>,
    pub release_group: Option<String>,
    pub edition: Option<String>,
    pub quality: QualityModel,
    pub languages: Vec<i64>,
    pub media_info: Option<Value>,
    pub indexer_flags: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AlternativeTitle {
    pub id: i64,
    pub movie_metadata_id: i64,
    pub title: String,
    pub clean_title: Option<String>,
    pub source_type: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MovieTranslation {
    pub id: i64,
    pub movie_metadata_id: i64,
    pub language_id: i64,
    pub title: Option<String>,
    pub clean_title: Option<String>,
    pub overview: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Credit {
    pub id: i64,
    pub movie_metadata_id: i64,
    pub credit_tmdb_id: Option<String>,
    pub person_tmdb_id: i64,
    pub name: Option<String>,
    pub images: Vec<Image>,
    pub character: Option<String>,
    pub order: i64,
    pub job: Option<String>,
    pub department: Option<String>,
    /// `cast` or `crew`.
    pub credit_type: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Collection {
    pub id: i64,
    pub tmdb_id: i64,
    pub title: String,
    pub sort_title: Option<String>,
    pub clean_title: Option<String>,
    pub overview: Option<String>,
    pub images: Vec<Image>,
    pub monitored: bool,
    pub quality_profile_id: i64,
    pub root_folder_path: Option<String>,
    pub minimum_availability: MovieStatus,
    pub search_on_add: bool,
    pub tags: Vec<i64>,
    pub added: Option<DateTime<Utc>>,
    pub last_info_sync: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImportExclusion {
    pub id: i64,
    pub tmdb_id: i64,
    pub movie_title: Option<String>,
    pub movie_year: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MovieHistoryEntry {
    pub id: i64,
    pub movie_id: i64,
    pub source_title: Option<String>,
    pub date: DateTime<Utc>,
    pub event_type: MovieHistoryEvent,
    pub quality: QualityModel,
    pub languages: Vec<i64>,
    /// Free-form string map, as the originals keep it.
    pub data: Value,
    pub download_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MovieBlocklistEntry {
    pub id: i64,
    pub movie_id: i64,
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
}

/// A row of the originals' `DownloadHistory`, kept for the queue of 4b.
#[derive(Debug, Clone, PartialEq)]
pub struct MovieDownloadHistoryEntry {
    pub id: i64,
    pub event_type: i64,
    pub movie_id: i64,
    pub download_id: String,
    pub source_title: String,
    pub date: DateTime<Utc>,
    pub protocol: Protocol,
    pub indexer_id: i64,
    pub download_client_id: i64,
    pub release: Option<Value>,
    pub data: Value,
}

/// The files that go with a movie file: the originals keep subtitles,
/// metadata files and other extras in a table each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtraKind {
    Subtitle,
    Metadata,
    Other,
}

impl ExtraKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            ExtraKind::Subtitle => "subtitle",
            ExtraKind::Metadata => "metadata",
            ExtraKind::Other => "other",
        }
    }

    pub fn parse(s: &str) -> Option<ExtraKind> {
        match s {
            "subtitle" => Some(ExtraKind::Subtitle),
            "metadata" => Some(ExtraKind::Metadata),
            "other" => Some(ExtraKind::Other),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MovieExtraFile {
    pub kind: ExtraKind,
    /// The id within its kind, the one the API shows.
    pub id: i64,
    pub movie_id: i64,
    pub movie_file_id: Option<i64>,
    pub relative_path: String,
    pub extension: String,
    pub added: Option<DateTime<Utc>>,
    pub last_updated: Option<DateTime<Utc>>,
    pub language: i64,
    pub language_tags: Vec<String>,
    pub title: Option<String>,
    pub copy: i64,
    pub consumer: Option<String>,
    pub metadata_type: Option<i64>,
    pub hash: Option<String>,
}

/// `NamingConfig` as Radarr has it.
#[derive(Debug, Clone, PartialEq)]
pub struct MovieNamingConfig {
    pub rename_movies: bool,
    pub replace_illegal_characters: bool,
    pub colon_replacement_format: i64,
    pub standard_movie_format: String,
    pub movie_folder_format: String,
}

impl Default for MovieNamingConfig {
    /// A new installation's, as the originals seed it.
    fn default() -> Self {
        MovieNamingConfig {
            rename_movies: false,
            replace_illegal_characters: true,
            colon_replacement_format: 0,
            standard_movie_format: "{Movie Title} ({Release Year}) {Quality Full}".into(),
            movie_folder_format: "{Movie Title} ({Release Year})".into(),
        }
    }
}
