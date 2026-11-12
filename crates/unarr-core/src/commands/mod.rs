//! Commands: the unit of background work, after the originals'
//! `CommandQueueManager`, `CommandQueue` and `CommandExecutor`. The API
//! pushes them (`POST /command`), the scheduler pushes them when a task is
//! due, and a few workers run them; every command is also written to the
//! `commands` table so it can be read back after it has left the queue.

pub mod executor;
pub mod scheduler;

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;

use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Map, Value};
use tokio::sync::Notify;

use crate::db::Db;
use crate::store;

pub use executor::{Event, HostHooks, Runtime};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandStatus {
    Queued,
    Started,
    Completed,
    Failed,
    Aborted,
    Cancelled,
    Orphaned,
}

impl CommandStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            CommandStatus::Queued => "queued",
            CommandStatus::Started => "started",
            CommandStatus::Completed => "completed",
            CommandStatus::Failed => "failed",
            CommandStatus::Aborted => "aborted",
            CommandStatus::Cancelled => "cancelled",
            CommandStatus::Orphaned => "orphaned",
        }
    }

    pub fn as_i64(self) -> i64 {
        match self {
            CommandStatus::Queued => 0,
            CommandStatus::Started => 1,
            CommandStatus::Completed => 2,
            CommandStatus::Failed => 3,
            CommandStatus::Aborted => 4,
            CommandStatus::Cancelled => 5,
            CommandStatus::Orphaned => 6,
        }
    }

    pub fn from_i64(n: i64) -> CommandStatus {
        match n {
            1 => CommandStatus::Started,
            2 => CommandStatus::Completed,
            3 => CommandStatus::Failed,
            4 => CommandStatus::Aborted,
            5 => CommandStatus::Cancelled,
            6 => CommandStatus::Orphaned,
            _ => CommandStatus::Queued,
        }
    }

    /// `CommandPriorityComparer`: started first, then by value.
    pub fn list_rank(self) -> (i64, i64) {
        if self == CommandStatus::Started {
            (0, 0)
        } else {
            (1, self.as_i64())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum CommandPriority {
    Low,
    #[default]
    Normal,
    High,
}

impl CommandPriority {
    pub fn as_str(self) -> &'static str {
        match self {
            CommandPriority::Low => "low",
            CommandPriority::Normal => "normal",
            CommandPriority::High => "high",
        }
    }

    pub fn as_i64(self) -> i64 {
        match self {
            CommandPriority::Low => -1,
            CommandPriority::Normal => 0,
            CommandPriority::High => 1,
        }
    }

    pub fn from_i64(n: i64) -> CommandPriority {
        match n {
            -1 => CommandPriority::Low,
            1 => CommandPriority::High,
            _ => CommandPriority::Normal,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CommandTrigger {
    #[default]
    Unspecified,
    Manual,
    Scheduled,
}

impl CommandTrigger {
    pub fn as_str(self) -> &'static str {
        match self {
            CommandTrigger::Unspecified => "unspecified",
            CommandTrigger::Manual => "manual",
            CommandTrigger::Scheduled => "scheduled",
        }
    }

    pub fn as_i64(self) -> i64 {
        match self {
            CommandTrigger::Unspecified => 0,
            CommandTrigger::Manual => 1,
            CommandTrigger::Scheduled => 2,
        }
    }

    pub fn from_i64(n: i64) -> CommandTrigger {
        match n {
            1 => CommandTrigger::Manual,
            2 => CommandTrigger::Scheduled,
            _ => CommandTrigger::Unspecified,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CommandResult {
    #[default]
    Unknown,
    Successful,
    Unsuccessful,
}

impl CommandResult {
    pub fn as_str(self) -> &'static str {
        match self {
            CommandResult::Unknown => "unknown",
            CommandResult::Successful => "successful",
            CommandResult::Unsuccessful => "unsuccessful",
        }
    }

    pub fn as_i64(self) -> i64 {
        match self {
            CommandResult::Unknown => 0,
            CommandResult::Successful => 1,
            CommandResult::Unsuccessful => 2,
        }
    }

    pub fn from_i64(n: i64) -> CommandResult {
        match n {
            1 => CommandResult::Successful,
            2 => CommandResult::Unsuccessful,
            _ => CommandResult::Unknown,
        }
    }
}

/// `BackupType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackupType {
    #[default]
    Scheduled,
    Manual,
    Update,
}

/// The commands Unarr knows, with the properties their originals declare.
/// Two commands with equal bodies are the same command for the queue
/// (`CommandEqualityComparer`).
#[derive(Debug, Clone, PartialEq)]
pub enum CommandBody {
    RssSync,
    RefreshMonitoredDownloads,
    ProcessMonitoredDownloads,
    EpisodeSearch {
        episode_ids: Vec<i64>,
    },
    SeasonSearch {
        series_id: i64,
        season_number: i64,
    },
    SeriesSearch {
        series_id: i64,
    },
    MissingEpisodeSearch {
        series_id: Option<i64>,
        monitored: bool,
    },
    CutoffUnmetEpisodeSearch {
        series_id: Option<i64>,
        monitored: bool,
    },
    ClearBlocklist,
    /// The recent log lines, emptied.
    ClearLog,
    /// The log files on disk, deleted.
    DeleteLogFiles,
    /// The updater's log files; there are none.
    DeleteUpdateLogFiles,
    /// The quality definitions' sizes (and titles, when asked) back to the
    /// defaults.
    ResetQualityDefinitions {
        reset_titles: bool,
    },
    /// A new API key.
    ResetApiKey,
    MessagingCleanup,
    RefreshSeries {
        series_ids: Vec<i64>,
        is_new_series: bool,
    },
    RescanSeries {
        series_id: Option<i64>,
    },
    /// `ManualImportCommand`: the files the interactive import chose.
    ManualImport {
        files: Vec<crate::mediafiles::manual::ManualImportFile>,
        import_mode: crate::mediafiles::ImportMode,
    },
    /// `RenameFilesCommand`.
    RenameFiles {
        series_id: i64,
        files: Vec<i64>,
    },
    /// `RenameSeriesCommand`.
    RenameSeries {
        series_ids: Vec<i64>,
    },
    /// `MoveSeriesCommand`: a series folder moved after a path edit.
    MoveSeries {
        series_id: i64,
        source_path: String,
        destination_path: String,
    },
    /// `BulkMoveSeriesCommand`: the editor's move to another root folder,
    /// `(series id, source path)` each.
    BulkMoveSeries {
        destination_root_folder: String,
        series: Vec<(i64, String)>,
    },
    /// `DownloadedEpisodesScanCommand`: a path a third party asks to import.
    DownloadedEpisodesScan {
        path: Option<String>,
        download_client_id: Option<String>,
        import_mode: crate::mediafiles::ImportMode,
    },
    CheckHealth,
    Housekeeping,
    ApplicationUpdateCheck {
        install_major_update: bool,
    },
    UpdateSceneMapping,
    CleanUpRecycleBin,
    ImportListSync {
        definition_id: Option<i64>,
    },
    Backup {
        kind: BackupType,
    },
    /// Radarr's `RescanMovieCommand`: one movie's folder, or every one's.
    RescanMovie {
        movie_id: Option<i64>,
    },
    /// Radarr's `DownloadedMoviesScanCommand`: a path a third party asks
    /// to import.
    DownloadedMoviesScan {
        path: Option<String>,
        download_client_id: Option<String>,
        import_mode: crate::mediafiles::ImportMode,
    },
    /// Radarr's `RenameFilesCommand`: the same name as Sonarr's, about a
    /// movie's files.
    RenameMovieFiles {
        movie_id: i64,
        files: Vec<i64>,
    },
    /// Radarr's `ManualImportCommand`: the same name as Sonarr's, about
    /// movie files.
    ManualImportMovies {
        files: Vec<crate::movies::import::manual::ManualImportFile>,
        import_mode: crate::mediafiles::ImportMode,
    },
    /// Radarr's `RenameMovieCommand`: every file of each movie.
    RenameMovie {
        movie_ids: Vec<i64>,
    },
    /// Radarr's `RefreshMovieCommand`.
    RefreshMovie {
        movie_ids: Vec<i64>,
        is_new_movie: bool,
    },
    /// Radarr's `MoviesSearchCommand`.
    MoviesSearch {
        movie_ids: Vec<i64>,
    },
    /// Radarr's `MissingMoviesSearchCommand`; the filter is the UI's, which
    /// the original keeps and does not read.
    MissingMoviesSearch {
        filter_key: Option<String>,
        filter_value: Option<String>,
    },
    /// Radarr's `CutoffUnmetMoviesSearchCommand`.
    CutoffUnmetMoviesSearch {
        filter_key: Option<String>,
        filter_value: Option<String>,
    },
}

/// `(name, .NET type name)` of every command, for `POST /command` (the
/// name, matched without regard to case) and the scheduled tasks table.
const NAMES: &[(&str, &str)] = &[
    ("RssSync", "NzbDrone.Core.Indexers.RssSyncCommand"),
    (
        "RefreshMonitoredDownloads",
        "NzbDrone.Core.Download.RefreshMonitoredDownloadsCommand",
    ),
    (
        "ProcessMonitoredDownloads",
        "NzbDrone.Core.Download.ProcessMonitoredDownloadsCommand",
    ),
    (
        "EpisodeSearch",
        "NzbDrone.Core.IndexerSearch.EpisodeSearchCommand",
    ),
    ("SeasonSearch", "NzbDrone.Core.IndexerSearch.SeasonSearchCommand"),
    ("SeriesSearch", "NzbDrone.Core.IndexerSearch.SeriesSearchCommand"),
    (
        "MissingEpisodeSearch",
        "NzbDrone.Core.IndexerSearch.MissingEpisodeSearchCommand",
    ),
    (
        "CutoffUnmetEpisodeSearch",
        "NzbDrone.Core.IndexerSearch.CutoffUnmetEpisodeSearchCommand",
    ),
    (
        "ClearBlocklist",
        "NzbDrone.Core.Blocklisting.ClearBlocklistCommand",
    ),
    (
        "ClearLog",
        "NzbDrone.Core.Instrumentation.Commands.ClearLogCommand",
    ),
    (
        "DeleteLogFiles",
        "NzbDrone.Core.Instrumentation.Commands.DeleteLogFilesCommand",
    ),
    (
        "DeleteUpdateLogFiles",
        "NzbDrone.Core.Instrumentation.Commands.DeleteUpdateLogFilesCommand",
    ),
    (
        "ResetQualityDefinitions",
        "NzbDrone.Core.Qualities.Commands.ResetQualityDefinitionsCommand",
    ),
    ("ResetApiKey", "NzbDrone.Core.Configuration.ResetApiKeyCommand"),
    (
        "MessagingCleanup",
        "NzbDrone.Core.Messaging.Commands.MessagingCleanupCommand",
    ),
    ("RefreshSeries", "NzbDrone.Core.Tv.Commands.RefreshSeriesCommand"),
    (
        "RescanSeries",
        "NzbDrone.Core.MediaFiles.Commands.RescanSeriesCommand",
    ),
    (
        "DownloadedEpisodesScan",
        "NzbDrone.Core.MediaFiles.Commands.DownloadedEpisodesScanCommand",
    ),
    (
        "ManualImport",
        "NzbDrone.Core.MediaFiles.EpisodeImport.Manual.ManualImportCommand",
    ),
    (
        "RenameFiles",
        "NzbDrone.Core.MediaFiles.Commands.RenameFilesCommand",
    ),
    (
        "RenameSeries",
        "NzbDrone.Core.MediaFiles.Commands.RenameSeriesCommand",
    ),
    ("MoveSeries", "NzbDrone.Core.Tv.Commands.MoveSeriesCommand"),
    (
        "BulkMoveSeries",
        "NzbDrone.Core.Tv.Commands.BulkMoveSeriesCommand",
    ),
    ("CheckHealth", "NzbDrone.Core.HealthCheck.CheckHealthCommand"),
    ("Housekeeping", "NzbDrone.Core.Housekeeping.HousekeepingCommand"),
    (
        "ApplicationUpdateCheck",
        "NzbDrone.Core.Update.Commands.ApplicationUpdateCheckCommand",
    ),
    (
        "UpdateSceneMapping",
        "NzbDrone.Core.DataAugmentation.Scene.UpdateSceneMappingCommand",
    ),
    (
        "CleanUpRecycleBin",
        "NzbDrone.Core.MediaFiles.Commands.CleanUpRecycleBinCommand",
    ),
    (
        "ImportListSync",
        "NzbDrone.Core.ImportLists.ImportListSyncCommand",
    ),
    ("Backup", "NzbDrone.Core.Backup.BackupCommand"),
    (
        "RescanMovie",
        "NzbDrone.Core.MediaFiles.Commands.RescanMovieCommand",
    ),
    (
        "DownloadedMoviesScan",
        "NzbDrone.Core.MediaFiles.Commands.DownloadedMoviesScanCommand",
    ),
    (
        "RefreshMovie",
        "NzbDrone.Core.Movies.Commands.RefreshMovieCommand",
    ),
    (
        "RenameMovie",
        "NzbDrone.Core.MediaFiles.Commands.RenameMovieCommand",
    ),
    ("MoviesSearch", "NzbDrone.Core.IndexerSearch.MoviesSearchCommand"),
    (
        "MissingMoviesSearch",
        "NzbDrone.Core.IndexerSearch.MissingMoviesSearchCommand",
    ),
    (
        "CutoffUnmetMoviesSearch",
        "NzbDrone.Core.IndexerSearch.CutoffUnmetMoviesSearchCommand",
    ),
];

/// `QualityModel` as the API carries it.
pub fn quality_model_json(q: &crate::model::QualityModel) -> Value {
    json!({
        "quality": crate::quality::Quality::by_id(q.quality_id),
        "revision": {"version": q.version, "real": q.real, "isRepack": q.is_repack},
    })
}

/// The API's quality model, from a command body.
pub fn quality_model_from_json(v: &Value) -> crate::model::QualityModel {
    crate::model::QualityModel {
        quality_id: v["quality"]["id"].as_i64().unwrap_or(0),
        version: v["revision"]["version"].as_i64().unwrap_or(1),
        real: v["revision"]["real"].as_i64().unwrap_or(0),
        is_repack: v["revision"]["isRepack"].as_bool().unwrap_or(false),
    }
}

/// Language ids from the API's `[{id, name}]`.
pub fn language_ids_from_json(v: &Value) -> Vec<i64> {
    v.as_array()
        .map(|a| a.iter().filter_map(|l| l["id"].as_i64()).collect())
        .unwrap_or_default()
}

pub fn languages_json(ids: &[i64]) -> Value {
    Value::Array(
        ids.iter()
            .map(|id| {
                let l = crate::quality::Language::by_id(*id);
                json!({"id": l.id, "name": l.name})
            })
            .collect(),
    )
}

fn manual_import_file(v: &Value) -> crate::mediafiles::manual::ManualImportFile {
    let text = |key: &str| v[key].as_str().map(str::to_string).filter(|s| !s.is_empty());
    crate::mediafiles::manual::ManualImportFile {
        path: v["path"].as_str().unwrap_or("").to_string(),
        folder_name: text("folderName"),
        series_id: v["seriesId"].as_i64().unwrap_or(0),
        episode_ids: ints(&v["episodeIds"]),
        episode_file_id: v["episodeFileId"].as_i64(),
        quality: quality_model_from_json(&v["quality"]),
        languages: language_ids_from_json(&v["languages"]),
        release_group: text("releaseGroup"),
        indexer_flags: v["indexerFlags"].as_i64().unwrap_or(0),
        release_type: v["releaseType"]
            .as_str()
            .and_then(crate::model::ReleaseType::parse)
            .unwrap_or_default(),
        download_id: text("downloadId"),
    }
}

fn manual_import_file_json(f: &crate::mediafiles::manual::ManualImportFile) -> Value {
    let mut m = Map::new();
    m.insert("path".into(), json!(f.path));
    if let Some(folder) = &f.folder_name {
        m.insert("folderName".into(), json!(folder));
    }
    m.insert("seriesId".into(), json!(f.series_id));
    m.insert("episodeIds".into(), json!(f.episode_ids));
    if let Some(id) = f.episode_file_id {
        m.insert("episodeFileId".into(), json!(id));
    }
    m.insert("quality".into(), quality_model_json(&f.quality));
    m.insert("languages".into(), languages_json(&f.languages));
    if let Some(group) = &f.release_group {
        m.insert("releaseGroup".into(), json!(group));
    }
    m.insert("indexerFlags".into(), json!(f.indexer_flags));
    m.insert("releaseType".into(), json!(f.release_type.as_str()));
    if let Some(id) = &f.download_id {
        m.insert("downloadId".into(), json!(id));
    }
    Value::Object(m)
}

/// Radarr's `ManualImportFile`, from a command body.
fn manual_import_movie_file(v: &Value) -> crate::movies::import::manual::ManualImportFile {
    let text = |key: &str| v[key].as_str().map(str::to_string);
    crate::movies::import::manual::ManualImportFile {
        path: v["path"].as_str().unwrap_or("").to_string(),
        folder_name: text("folderName"),
        movie_id: v["movieId"].as_i64().unwrap_or(0),
        quality: quality_model_from_json(&v["quality"]),
        languages: language_ids_from_json(&v["languages"]),
        release_group: text("releaseGroup"),
        indexer_flags: v["indexerFlags"].as_i64().unwrap_or(0),
        download_id: text("downloadId"),
    }
}

/// The same as the command's body shows it, nulls left out.
fn manual_import_movie_file_json(f: &crate::movies::import::manual::ManualImportFile) -> Value {
    use crate::movies::quality::{MovieLanguage, MovieQuality};
    let mut m = Map::new();
    m.insert("path".into(), json!(f.path));
    if let Some(folder) = &f.folder_name {
        m.insert("folderName".into(), json!(folder));
    }
    m.insert(
        "quality".into(),
        json!({
            "quality": MovieQuality::by_id(f.quality.quality_id),
            "revision": {
                "version": f.quality.version,
                "real": f.quality.real,
                "isRepack": f.quality.is_repack,
            },
        }),
    );
    m.insert(
        "languages".into(),
        Value::Array(
            f.languages
                .iter()
                .map(|id| json!({"id": id, "name": MovieLanguage::name_of(*id)}))
                .collect(),
        ),
    );
    if let Some(group) = &f.release_group {
        m.insert("releaseGroup".into(), json!(group));
    }
    m.insert("indexerFlags".into(), json!(f.indexer_flags));
    if let Some(id) = &f.download_id {
        m.insert("downloadId".into(), json!(id));
    }
    m.insert("movieId".into(), json!(f.movie_id));
    Value::Object(m)
}

fn ints(v: &Value) -> Vec<i64> {
    v.as_array()
        .map(|a| a.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default()
}

impl CommandBody {
    /// The command a `POST /command` body asks for, by its `name`; `None`
    /// for a name no command carries.
    pub fn parse(name: &str, body: &Value) -> Option<CommandBody> {
        let canonical = NAMES
            .iter()
            .map(|(n, _)| *n)
            .find(|n| n.eq_ignore_ascii_case(name.trim()))?;
        Some(Self::from_name(canonical, body))
    }

    /// [`CommandBody::parse`] for one application: a name only the other
    /// knows is no command here, as it is none in the original.
    pub fn parse_for(personality: crate::Personality, name: &str, body: &Value) -> Option<CommandBody> {
        let radarr = personality == crate::Personality::Radarr;
        // The names both applications have with different bodies.
        if radarr && name.eq_ignore_ascii_case("RenameFiles") {
            return Some(CommandBody::RenameMovieFiles {
                movie_id: body["movieId"].as_i64().unwrap_or(0),
                files: ints(&body["files"]),
            });
        }
        if radarr && name.eq_ignore_ascii_case("ManualImport") {
            return Some(CommandBody::ManualImportMovies {
                files: body["files"]
                    .as_array()
                    .map(|a| a.iter().map(manual_import_movie_file).collect())
                    .unwrap_or_default(),
                import_mode: body["importMode"]
                    .as_str()
                    .map(crate::mediafiles::ImportMode::parse)
                    .unwrap_or_default(),
            });
        }
        let command = Self::parse(name, body).filter(|command| command.runs_in(personality))?;
        Some(match (personality, command) {
            // Radarr's `BackupCommand.Type` is read from the trigger, not
            // from the body: anything posted is a manual backup.
            (crate::Personality::Radarr, CommandBody::Backup { .. }) => CommandBody::Backup {
                kind: BackupType::Manual,
            },
            (_, command) => command,
        })
    }

    /// Whether the application has this command. Radarr has what its
    /// milestones brought so far: the acquisition commands, the library's
    /// (scans, imports) and the ones with no media in them.
    pub fn runs_in(&self, personality: crate::Personality) -> bool {
        let movies = matches!(
            self,
            CommandBody::MoviesSearch { .. }
                | CommandBody::MissingMoviesSearch { .. }
                | CommandBody::CutoffUnmetMoviesSearch { .. }
                | CommandBody::RescanMovie { .. }
                | CommandBody::DownloadedMoviesScan { .. }
                | CommandBody::RefreshMovie { .. }
                | CommandBody::RenameMovieFiles { .. }
                | CommandBody::ManualImportMovies { .. }
                | CommandBody::RenameMovie { .. }
        );
        match personality {
            crate::Personality::Radarr => {
                movies
                    || matches!(
                        self,
                        CommandBody::RssSync
                            | CommandBody::RefreshMonitoredDownloads
                            | CommandBody::ProcessMonitoredDownloads
                            | CommandBody::MessagingCleanup
                            | CommandBody::CheckHealth
                            | CommandBody::Housekeeping
                            | CommandBody::CleanUpRecycleBin
                            | CommandBody::Backup { .. }
                    )
            }
            _ => !movies,
        }
    }

    /// The command a .NET type name (a scheduled task) names.
    pub fn from_type_name(type_name: &str) -> Option<CommandBody> {
        let short = type_name.rsplit('.').next().unwrap_or(type_name);
        let name = NAMES
            .iter()
            .map(|(n, t)| (*n, *t))
            .find(|(n, t)| *t == type_name || format!("{n}Command").eq_ignore_ascii_case(short))?
            .0;
        Some(Self::from_name(name, &json!({})))
    }

    fn from_name(name: &str, body: &Value) -> CommandBody {
        let monitored = body["monitored"].as_bool().unwrap_or(true);
        match name {
            "RssSync" => CommandBody::RssSync,
            "RefreshMonitoredDownloads" => CommandBody::RefreshMonitoredDownloads,
            "ProcessMonitoredDownloads" => CommandBody::ProcessMonitoredDownloads,
            "EpisodeSearch" => CommandBody::EpisodeSearch {
                episode_ids: ints(&body["episodeIds"]),
            },
            "SeasonSearch" => CommandBody::SeasonSearch {
                series_id: body["seriesId"].as_i64().unwrap_or(0),
                season_number: body["seasonNumber"].as_i64().unwrap_or(0),
            },
            "SeriesSearch" => CommandBody::SeriesSearch {
                series_id: body["seriesId"].as_i64().unwrap_or(0),
            },
            "MissingEpisodeSearch" => CommandBody::MissingEpisodeSearch {
                series_id: body["seriesId"].as_i64(),
                monitored,
            },
            "CutoffUnmetEpisodeSearch" => CommandBody::CutoffUnmetEpisodeSearch {
                series_id: body["seriesId"].as_i64(),
                monitored,
            },
            "ClearBlocklist" => CommandBody::ClearBlocklist,
            "ClearLog" => CommandBody::ClearLog,
            "DeleteLogFiles" => CommandBody::DeleteLogFiles,
            "DeleteUpdateLogFiles" => CommandBody::DeleteUpdateLogFiles,
            "ResetQualityDefinitions" => CommandBody::ResetQualityDefinitions {
                reset_titles: body["resetTitles"].as_bool().unwrap_or(false),
            },
            "ResetApiKey" => CommandBody::ResetApiKey,
            "MessagingCleanup" => CommandBody::MessagingCleanup,
            "RefreshSeries" => CommandBody::RefreshSeries {
                series_ids: {
                    let mut ids = ints(&body["seriesIds"]);
                    if let Some(id) = body["seriesId"].as_i64() {
                        if !ids.contains(&id) {
                            ids.push(id);
                        }
                    }
                    ids
                },
                is_new_series: body["isNewSeries"].as_bool().unwrap_or(false),
            },
            "RescanSeries" => CommandBody::RescanSeries {
                series_id: body["seriesId"].as_i64(),
            },
            "ManualImport" => CommandBody::ManualImport {
                files: body["files"]
                    .as_array()
                    .map(|a| a.iter().map(manual_import_file).collect())
                    .unwrap_or_default(),
                import_mode: body["importMode"]
                    .as_str()
                    .map(crate::mediafiles::ImportMode::parse)
                    .unwrap_or_default(),
            },
            "RenameFiles" => CommandBody::RenameFiles {
                series_id: body["seriesId"].as_i64().unwrap_or(0),
                files: ints(&body["files"]),
            },
            "RenameSeries" => CommandBody::RenameSeries {
                series_ids: ints(&body["seriesIds"]),
            },
            "MoveSeries" => CommandBody::MoveSeries {
                series_id: body["seriesId"].as_i64().unwrap_or(0),
                source_path: body["sourcePath"].as_str().unwrap_or("").to_string(),
                destination_path: body["destinationPath"].as_str().unwrap_or("").to_string(),
            },
            "BulkMoveSeries" => CommandBody::BulkMoveSeries {
                destination_root_folder: body["destinationRootFolder"].as_str().unwrap_or("").to_string(),
                series: body["series"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|s| {
                        (
                            s["seriesId"].as_i64().unwrap_or(0),
                            s["sourcePath"].as_str().unwrap_or("").to_string(),
                        )
                    })
                    .collect(),
            },
            "DownloadedEpisodesScan" => CommandBody::DownloadedEpisodesScan {
                path: body["path"]
                    .as_str()
                    .map(str::to_string)
                    .filter(|p| !p.trim().is_empty()),
                download_client_id: body["downloadClientId"]
                    .as_str()
                    .map(str::to_string)
                    .filter(|p| !p.trim().is_empty()),
                import_mode: body["importMode"]
                    .as_str()
                    .map(crate::mediafiles::ImportMode::parse)
                    .unwrap_or_default(),
            },
            "CheckHealth" => CommandBody::CheckHealth,
            "Housekeeping" => CommandBody::Housekeeping,
            "ApplicationUpdateCheck" => CommandBody::ApplicationUpdateCheck {
                install_major_update: body["installMajorUpdate"].as_bool().unwrap_or(false),
            },
            "UpdateSceneMapping" => CommandBody::UpdateSceneMapping,
            "CleanUpRecycleBin" => CommandBody::CleanUpRecycleBin,
            "ImportListSync" => CommandBody::ImportListSync {
                definition_id: body["definitionId"].as_i64(),
            },
            "Backup" => CommandBody::Backup {
                kind: match body["type"].as_str().map(str::to_ascii_lowercase).as_deref() {
                    Some("manual") => BackupType::Manual,
                    Some("update") => BackupType::Update,
                    _ => BackupType::Scheduled,
                },
            },
            "RescanMovie" => CommandBody::RescanMovie {
                movie_id: body["movieId"].as_i64(),
            },
            "DownloadedMoviesScan" => CommandBody::DownloadedMoviesScan {
                path: body["path"]
                    .as_str()
                    .map(str::to_string)
                    .filter(|p| !p.trim().is_empty()),
                download_client_id: body["downloadClientId"]
                    .as_str()
                    .map(str::to_string)
                    .filter(|p| !p.trim().is_empty()),
                import_mode: body["importMode"]
                    .as_str()
                    .map(crate::mediafiles::ImportMode::parse)
                    .unwrap_or_default(),
            },
            "RenameMovie" => CommandBody::RenameMovie {
                movie_ids: ints(&body["movieIds"]),
            },
            "RefreshMovie" => CommandBody::RefreshMovie {
                movie_ids: ints(&body["movieIds"]),
                is_new_movie: body["isNewMovie"].as_bool().unwrap_or(false),
            },
            "MoviesSearch" => CommandBody::MoviesSearch {
                movie_ids: ints(&body["movieIds"]),
            },
            "MissingMoviesSearch" => CommandBody::MissingMoviesSearch {
                filter_key: body["filterKey"].as_str().map(String::from),
                filter_value: body["filterValue"].as_str().map(String::from),
            },
            "CutoffUnmetMoviesSearch" => CommandBody::CutoffUnmetMoviesSearch {
                filter_key: body["filterKey"].as_str().map(String::from),
                filter_value: body["filterValue"].as_str().map(String::from),
            },
            _ => unreachable!("every name in NAMES is handled"),
        }
    }

    /// `Command.Name`: the class name without `Command`.
    pub fn name(&self) -> &'static str {
        match self {
            CommandBody::RssSync => "RssSync",
            CommandBody::RefreshMonitoredDownloads => "RefreshMonitoredDownloads",
            CommandBody::ProcessMonitoredDownloads => "ProcessMonitoredDownloads",
            CommandBody::EpisodeSearch { .. } => "EpisodeSearch",
            CommandBody::SeasonSearch { .. } => "SeasonSearch",
            CommandBody::SeriesSearch { .. } => "SeriesSearch",
            CommandBody::MissingEpisodeSearch { .. } => "MissingEpisodeSearch",
            CommandBody::CutoffUnmetEpisodeSearch { .. } => "CutoffUnmetEpisodeSearch",
            CommandBody::ClearBlocklist => "ClearBlocklist",
            CommandBody::ClearLog => "ClearLog",
            CommandBody::DeleteLogFiles => "DeleteLogFiles",
            CommandBody::DeleteUpdateLogFiles => "DeleteUpdateLogFiles",
            CommandBody::ResetQualityDefinitions { .. } => "ResetQualityDefinitions",
            CommandBody::ResetApiKey => "ResetApiKey",
            CommandBody::MessagingCleanup => "MessagingCleanup",
            CommandBody::RefreshSeries { .. } => "RefreshSeries",
            CommandBody::RescanSeries { .. } => "RescanSeries",
            CommandBody::DownloadedEpisodesScan { .. } => "DownloadedEpisodesScan",
            CommandBody::ManualImport { .. } => "ManualImport",
            CommandBody::RenameFiles { .. } => "RenameFiles",
            CommandBody::RenameSeries { .. } => "RenameSeries",
            CommandBody::MoveSeries { .. } => "MoveSeries",
            CommandBody::BulkMoveSeries { .. } => "BulkMoveSeries",
            CommandBody::CheckHealth => "CheckHealth",
            CommandBody::Housekeeping => "Housekeeping",
            CommandBody::ApplicationUpdateCheck { .. } => "ApplicationUpdateCheck",
            CommandBody::UpdateSceneMapping => "UpdateSceneMapping",
            CommandBody::CleanUpRecycleBin => "CleanUpRecycleBin",
            CommandBody::ImportListSync { .. } => "ImportListSync",
            CommandBody::Backup { .. } => "Backup",
            CommandBody::RescanMovie { .. } => "RescanMovie",
            CommandBody::DownloadedMoviesScan { .. } => "DownloadedMoviesScan",
            CommandBody::RefreshMovie { .. } => "RefreshMovie",
            CommandBody::RenameMovieFiles { .. } => "RenameFiles",
            CommandBody::ManualImportMovies { .. } => "ManualImport",
            CommandBody::RenameMovie { .. } => "RenameMovie",
            CommandBody::MoviesSearch { .. } => "MoviesSearch",
            CommandBody::MissingMoviesSearch { .. } => "MissingMoviesSearch",
            CommandBody::CutoffUnmetMoviesSearch { .. } => "CutoffUnmetMoviesSearch",
        }
    }

    /// The .NET type name, as the scheduled tasks table keys commands.
    pub fn type_name(&self) -> &'static str {
        let name = self.name();
        NAMES
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, t)| *t)
            .expect("every body has a type name")
    }

    /// `SendUpdatesToClient` as the class declares it (before the API
    /// forces it on).
    pub fn sends_updates(&self) -> bool {
        !matches!(
            self,
            CommandBody::RefreshMonitoredDownloads
                | CommandBody::ProcessMonitoredDownloads
                | CommandBody::DownloadedEpisodesScan { .. }
                | CommandBody::DownloadedMoviesScan { .. }
                | CommandBody::MessagingCleanup
                | CommandBody::CheckHealth
                | CommandBody::Housekeeping
                | CommandBody::UpdateSceneMapping
                | CommandBody::CleanUpRecycleBin
                | CommandBody::ResetQualityDefinitions { .. }
        )
    }

    pub fn is_long_running(&self) -> bool {
        matches!(
            self,
            CommandBody::RssSync
                | CommandBody::RefreshSeries { .. }
                | CommandBody::RefreshMovie { .. }
                | CommandBody::ProcessMonitoredDownloads
                | CommandBody::DownloadedEpisodesScan { .. }
                | CommandBody::DownloadedMoviesScan { .. }
        )
    }

    pub fn is_exclusive(&self) -> bool {
        false
    }

    pub fn requires_disk_access(&self) -> bool {
        matches!(
            self,
            CommandBody::ProcessMonitoredDownloads
                | CommandBody::DownloadedEpisodesScan { .. }
                | CommandBody::DownloadedMoviesScan { .. }
                | CommandBody::ManualImport { .. }
                | CommandBody::RenameFiles { .. }
                | CommandBody::RenameSeries { .. }
                | CommandBody::RenameMovieFiles { .. }
                | CommandBody::ManualImportMovies { .. }
                | CommandBody::RenameMovie { .. }
                | CommandBody::MoveSeries { .. }
                | CommandBody::BulkMoveSeries { .. }
        )
    }

    /// `UpdateScheduledTask`: whether running it counts as the scheduled
    /// task's execution.
    pub fn updates_scheduled_task(&self) -> bool {
        match self {
            CommandBody::RefreshSeries { series_ids, .. } => series_ids.is_empty(),
            CommandBody::RefreshMovie { movie_ids, .. } => movie_ids.is_empty(),
            CommandBody::ImportListSync { definition_id } => definition_id.is_none(),
            CommandBody::Backup { kind } => *kind == BackupType::Scheduled,
            _ => true,
        }
    }

    pub fn completion_message(&self) -> Option<&'static str> {
        match self {
            CommandBody::RefreshSeries { .. } | CommandBody::RefreshMovie { .. } => Some("Completed"),
            _ => None,
        }
    }

    /// The command's own properties, as the originals serialise them.
    fn own_properties(&self) -> Map<String, Value> {
        let mut m = Map::new();
        match self {
            CommandBody::EpisodeSearch { episode_ids } => {
                m.insert("episodeIds".into(), json!(episode_ids));
            }
            CommandBody::ResetQualityDefinitions { reset_titles } => {
                m.insert("resetTitles".into(), json!(reset_titles));
            }
            CommandBody::SeasonSearch {
                series_id,
                season_number,
            } => {
                m.insert("seriesId".into(), json!(series_id));
                m.insert("seasonNumber".into(), json!(season_number));
            }
            CommandBody::SeriesSearch { series_id } => {
                m.insert("seriesId".into(), json!(series_id));
            }
            CommandBody::MissingEpisodeSearch { series_id, monitored }
            | CommandBody::CutoffUnmetEpisodeSearch { series_id, monitored } => {
                if let Some(id) = series_id {
                    m.insert("seriesId".into(), json!(id));
                }
                m.insert("monitored".into(), json!(monitored));
            }
            CommandBody::RefreshSeries {
                series_ids,
                is_new_series,
            } => {
                m.insert("seriesIds".into(), json!(series_ids));
                m.insert("isNewSeries".into(), json!(is_new_series));
            }
            CommandBody::RescanSeries { series_id: Some(id) } => {
                m.insert("seriesId".into(), json!(id));
            }
            CommandBody::ManualImport { files, import_mode } => {
                m.insert(
                    "files".into(),
                    Value::Array(files.iter().map(manual_import_file_json).collect()),
                );
                m.insert("importMode".into(), json!(import_mode.as_str()));
            }
            CommandBody::RenameFiles { series_id, files } => {
                m.insert("seriesId".into(), json!(series_id));
                m.insert("files".into(), json!(files));
            }
            CommandBody::RenameSeries { series_ids } => {
                m.insert("seriesIds".into(), json!(series_ids));
            }
            CommandBody::MoveSeries {
                series_id,
                source_path,
                destination_path,
            } => {
                m.insert("seriesId".into(), json!(series_id));
                m.insert("sourcePath".into(), json!(source_path));
                m.insert("destinationPath".into(), json!(destination_path));
            }
            CommandBody::BulkMoveSeries {
                destination_root_folder,
                series,
            } => {
                m.insert("destinationRootFolder".into(), json!(destination_root_folder));
                m.insert(
                    "series".into(),
                    json!(series
                        .iter()
                        .map(|(id, path)| json!({"seriesId": id, "sourcePath": path}))
                        .collect::<Vec<_>>()),
                );
            }
            CommandBody::DownloadedEpisodesScan {
                path,
                download_client_id,
                import_mode,
            } => {
                if let Some(p) = path {
                    m.insert("path".into(), json!(p));
                }
                if let Some(d) = download_client_id {
                    m.insert("downloadClientId".into(), json!(d));
                }
                m.insert("importMode".into(), json!(import_mode.as_str()));
            }
            CommandBody::ApplicationUpdateCheck { install_major_update } => {
                m.insert("installMajorUpdate".into(), json!(install_major_update));
            }
            CommandBody::ImportListSync {
                definition_id: Some(id),
            } => {
                m.insert("definitionId".into(), json!(id));
            }
            CommandBody::MoviesSearch { movie_ids } => {
                m.insert("movieIds".into(), json!(movie_ids));
            }
            CommandBody::RescanMovie { movie_id: Some(id) } => {
                m.insert("movieId".into(), json!(id));
            }
            CommandBody::ManualImportMovies { files, import_mode } => {
                m.insert(
                    "files".into(),
                    Value::Array(files.iter().map(manual_import_movie_file_json).collect()),
                );
                m.insert("importMode".into(), json!(import_mode.as_str()));
            }
            CommandBody::RenameMovieFiles { movie_id, files } => {
                m.insert("movieId".into(), json!(movie_id));
                m.insert("files".into(), json!(files));
            }
            CommandBody::RenameMovie { movie_ids } => {
                m.insert("movieIds".into(), json!(movie_ids));
            }
            CommandBody::DownloadedMoviesScan {
                path,
                download_client_id,
                import_mode,
            } => {
                if let Some(p) = path {
                    m.insert("path".into(), json!(p));
                }
                if let Some(d) = download_client_id {
                    m.insert("downloadClientId".into(), json!(d));
                }
                m.insert("importMode".into(), json!(import_mode.as_str()));
                // The class's own `SendUpdates`, which nothing sets.
                m.insert("sendUpdates".into(), json!(false));
            }
            CommandBody::RefreshMovie {
                movie_ids,
                is_new_movie,
            } => {
                m.insert("movieIds".into(), json!(movie_ids));
                m.insert("isNewMovie".into(), json!(is_new_movie));
            }
            CommandBody::MissingMoviesSearch {
                filter_key,
                filter_value,
            }
            | CommandBody::CutoffUnmetMoviesSearch {
                filter_key,
                filter_value,
            } => {
                if let Some(key) = filter_key {
                    m.insert("filterKey".into(), json!(key));
                }
                if let Some(value) = filter_value {
                    m.insert("filterValue".into(), json!(value));
                }
            }
            CommandBody::Backup { kind } => {
                m.insert(
                    "type".into(),
                    json!(match kind {
                        BackupType::Scheduled => "scheduled",
                        BackupType::Manual => "manual",
                        BackupType::Update => "update",
                    }),
                );
            }
            _ => {}
        }
        m
    }
}

/// `CommandModel` with the `Command` it carries.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandModel {
    pub id: i64,
    pub body: CommandBody,
    pub priority: CommandPriority,
    pub status: CommandStatus,
    pub result: CommandResult,
    pub queued_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub duration: Option<Duration>,
    pub exception: Option<String>,
    pub trigger: CommandTrigger,
    pub message: Option<String>,
    /// `Command.SendUpdatesToClient`: the API forces it on.
    pub send_updates_to_client: bool,
    /// `Command.SuppressMessages`: progress messages the UI should not toast.
    pub suppress_messages: bool,
    pub client_user_agent: Option<String>,
    /// The scheduled task's last run, for commands the scheduler pushes.
    pub last_execution_time: Option<DateTime<Utc>>,
    pub last_start_time: Option<DateTime<Utc>>,
    /// `Command.IsTypeExclusive`, a property Radarr's commands have and
    /// Sonarr's do not: `None` where the application has none to show.
    pub type_exclusive: Option<bool>,
}

impl CommandModel {
    pub fn name(&self) -> &'static str {
        self.body.name()
    }

    pub fn is_active(&self) -> bool {
        matches!(self.status, CommandStatus::Queued | CommandStatus::Started)
    }

    /// `CommandResource.StateChangeTime`.
    pub fn state_change_time(&self) -> Option<DateTime<Utc>> {
        self.started_at.or(self.ended_at)
    }

    /// `Command` as `PolymorphicWriteOnlyJsonConverter` writes it: the
    /// declared properties of the class and its base, nulls left out.
    pub fn body_json(&self) -> Value {
        let mut m = self.body.own_properties();
        m.insert("sendUpdatesToClient".into(), json!(self.send_updates_to_client));
        m.insert(
            "updateScheduledTask".into(),
            json!(self.body.updates_scheduled_task()),
        );
        if let Some(c) = self.body.completion_message() {
            m.insert("completionMessage".into(), json!(c));
        }
        m.insert(
            "requiresDiskAccess".into(),
            json!(self.body.requires_disk_access()),
        );
        m.insert("isExclusive".into(), json!(self.body.is_exclusive()));
        if let Some(exclusive) = self.type_exclusive {
            m.insert("isTypeExclusive".into(), json!(exclusive));
        }
        m.insert("isLongRunning".into(), json!(self.body.is_long_running()));
        m.insert("name".into(), json!(self.body.name()));
        if let Some(t) = self.last_execution_time {
            m.insert("lastExecutionTime".into(), json!(crate::time::format_utc(t)));
        }
        if let Some(t) = self.last_start_time {
            m.insert("lastStartTime".into(), json!(crate::time::format_utc(t)));
        }
        m.insert("trigger".into(), json!(self.trigger.as_str()));
        m.insert("suppressMessages".into(), json!(self.suppress_messages));
        if let Some(ua) = &self.client_user_agent {
            m.insert("clientUserAgent".into(), json!(ua));
        }
        Value::Object(m)
    }
}

/// `CommandQueue` plus the parts of `CommandQueueManager` that keep it: the
/// commands not yet cleaned up, in memory, each also stored.
pub struct CommandQueue {
    db: Db,
    items: Mutex<Vec<CommandModel>>,
    next_id: AtomicI64,
    /// Pulsed whenever the queue changes, for the workers and for
    /// `wait_idle`.
    changed: Notify,
    /// The application the commands are of.
    personality: crate::Personality,
}

impl CommandQueue {
    /// A queue over `db`, continuing its ids after the stored commands;
    /// commands left started by an earlier run are orphaned, queued ones
    /// run again.
    pub async fn open(db: Db) -> anyhow::Result<CommandQueue> {
        Self::open_for(db, crate::Personality::Sonarr).await
    }

    /// A queue of one application's commands.
    pub async fn open_for(db: Db, personality: crate::Personality) -> anyhow::Result<CommandQueue> {
        let now = Utc::now();
        let (next_id, queued) = db
            .call(move |conn| {
                store::commands::orphan_started(conn, now)?;
                Ok((store::commands::next_id(conn)?, store::commands::queued(conn)?))
            })
            .await?;
        Ok(CommandQueue {
            db,
            items: Mutex::new(queued),
            next_id: AtomicI64::new(next_id),
            changed: Notify::new(),
            personality,
        })
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    /// `Push`: queue a command unless an equal one is queued or started, in
    /// which case that one is returned.
    pub async fn push(
        &self,
        body: CommandBody,
        priority: CommandPriority,
        trigger: CommandTrigger,
        now: DateTime<Utc>,
    ) -> anyhow::Result<CommandModel> {
        self.push_with(body, priority, trigger, now, |_| {}).await
    }

    /// `Push` with the model adjusted before it is stored (the API forces
    /// client updates on, the scheduler adds the task's last run).
    pub async fn push_with(
        &self,
        body: CommandBody,
        priority: CommandPriority,
        trigger: CommandTrigger,
        now: DateTime<Utc>,
        adjust: impl FnOnce(&mut CommandModel),
    ) -> anyhow::Result<CommandModel> {
        let model = {
            let mut items = self.items.lock().unwrap();
            if let Some(existing) = items.iter().find(|c| c.is_active() && c.body == body) {
                tracing::trace!(name = body.name(), "command is already in progress");
                return Ok(existing.clone());
            }
            let sends = body.sends_updates();
            let mut model = CommandModel {
                id: self.next_id.fetch_add(1, Ordering::SeqCst),
                body,
                priority,
                status: CommandStatus::Queued,
                result: CommandResult::Unknown,
                queued_at: now,
                started_at: None,
                ended_at: None,
                duration: None,
                exception: None,
                trigger,
                message: None,
                send_updates_to_client: sends,
                suppress_messages: !sends,
                client_user_agent: None,
                last_execution_time: None,
                last_start_time: None,
                type_exclusive: (self.personality == crate::Personality::Radarr).then_some(false),
            };
            adjust(&mut model);
            items.push(model.clone());
            model
        };
        let stored = model.clone();
        self.db
            .call(move |conn| store::commands::insert(conn, &stored))
            .await?;
        self.changed.notify_waiters();
        Ok(model)
    }

    /// Every command still in the queue, oldest first.
    pub fn all(&self) -> Vec<CommandModel> {
        self.items.lock().unwrap().clone()
    }

    pub fn find(&self, id: i64) -> Option<CommandModel> {
        self.items.lock().unwrap().iter().find(|c| c.id == id).cloned()
    }

    /// `Get`: from the queue, or from the table once cleaned up.
    pub async fn get(&self, id: i64) -> anyhow::Result<Option<CommandModel>> {
        if let Some(c) = self.find(id) {
            return Ok(Some(c));
        }
        self.db.call(move |conn| store::commands::get(conn, id)).await
    }

    /// `RemoveIfQueued`: `false` when the command is not queued (it is
    /// running, done, or unknown).
    pub fn cancel(&self, id: i64) -> bool {
        let removed = {
            let mut items = self.items.lock().unwrap();
            match items
                .iter()
                .position(|c| c.id == id && c.status == CommandStatus::Queued)
            {
                Some(i) => {
                    items.remove(i);
                    true
                }
                None => false,
            }
        };
        if removed {
            self.changed.notify_waiters();
        }
        removed
    }

    /// `CleanCommands`: drop from the queue what ended more than five
    /// minutes ago and from the table what ended more than a day ago.
    pub async fn clean(&self, now: DateTime<Utc>) -> anyhow::Result<()> {
        {
            let mut items = self.items.lock().unwrap();
            items.retain(|c| !c.ended_at.is_some_and(|t| t < now - Duration::minutes(5)));
        }
        self.db.call(move |conn| store::commands::trim(conn, now)).await?;
        self.changed.notify_waiters();
        Ok(())
    }

    /// `TryGet`: the next command to run, marked started; `None` when
    /// nothing may start now (nothing queued, or an exclusive or
    /// long-running command holds the others back).
    pub fn try_start(&self, now: DateTime<Utc>) -> Option<CommandModel> {
        let mut items = self.items.lock().unwrap();
        let started: Vec<CommandBody> = items
            .iter()
            .filter(|c| c.status == CommandStatus::Started)
            .map(|c| c.body.clone())
            .collect();
        let disk_busy = started.iter().any(|b| b.requires_disk_access());
        let long_running = started.iter().any(|b| b.is_long_running());
        let exclusive_running = started.iter().any(|b| b.is_exclusive());
        let candidate = items
            .iter()
            .enumerate()
            .filter(|(_, c)| c.status == CommandStatus::Queued)
            .filter(|(_, c)| !(disk_busy && c.body.requires_disk_access()))
            .filter(|(_, c)| !(long_running && c.body.is_exclusive()))
            .max_by(|(ia, a), (ib, b)| {
                // Highest priority first, then the earliest queued: the
                // reverse of the originals' OrderByDescending/ThenBy.
                a.priority
                    .cmp(&b.priority)
                    .then_with(|| b.queued_at.cmp(&a.queued_at))
                    .then_with(|| ib.cmp(ia))
            })
            .map(|(i, _)| i)?;
        if exclusive_running {
            return None;
        }
        if items[candidate].body.is_exclusive() && !started.is_empty() {
            return None;
        }
        let c = &mut items[candidate];
        c.status = CommandStatus::Started;
        c.started_at = Some(now);
        Some(c.clone())
    }

    /// Wait until the queue changes.
    pub async fn changed(&self) {
        self.changed.notified().await;
    }

    /// Any command queued or started.
    pub fn is_busy(&self) -> bool {
        self.items.lock().unwrap().iter().any(|c| c.is_active())
    }

    /// Any command started and not yet ended.
    pub fn has_running(&self) -> bool {
        self.items
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.status == CommandStatus::Started)
    }

    /// Wake whoever waits on [`CommandQueue::changed`].
    pub fn notify(&self) {
        self.changed.notify_waiters();
    }

    /// Wait until no command is queued or started. For tests and the
    /// contract replay, where the originals' recording slept instead.
    pub async fn wait_idle(&self) {
        loop {
            if !self.is_busy() {
                return;
            }
            tokio::select! {
                _ = self.changed() => {}
                _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {}
            }
        }
    }

    /// Change a command in the queue; `None` when it is not there.
    fn update(&self, id: i64, f: impl FnOnce(&mut CommandModel)) -> Option<CommandModel> {
        let mut items = self.items.lock().unwrap();
        let c = items.iter_mut().find(|c| c.id == id)?;
        f(c);
        Some(c.clone())
    }

    /// `SetMessage`: a progress message (`ProgressInfo`).
    pub fn set_message(&self, id: i64, message: &str) -> Option<CommandModel> {
        self.update(id, |c| c.message = Some(message.to_string()))
    }

    /// `CommandResultReporter.Report`: a result the handler sets before the
    /// command ends (an unsuccessful import scan completes all the same).
    pub fn set_result(&self, id: i64, result: CommandResult) -> Option<CommandModel> {
        self.update(id, |c| c.result = result)
    }

    /// `Start`: the started state written to the table.
    pub async fn record_started(&self, model: &CommandModel) -> anyhow::Result<()> {
        let model = model.clone();
        self.db
            .call(move |conn| store::commands::update(conn, &model))
            .await
    }

    /// `Complete` / `Fail`: the end written to the queue and the table.
    pub async fn end(
        &self,
        id: i64,
        status: CommandStatus,
        message: Option<String>,
        exception: Option<String>,
        now: DateTime<Utc>,
    ) -> anyhow::Result<Option<CommandModel>> {
        let model = self.update(id, |c| {
            if status == CommandStatus::Completed && c.result == CommandResult::Unknown {
                c.result = CommandResult::Successful;
            }
            c.message = message;
            c.exception = exception;
            c.ended_at = Some(now);
            c.duration = Some(now - c.started_at.unwrap_or(now));
            c.status = status;
        });
        if let Some(model) = &model {
            let stored = model.clone();
            self.db
                .call(move |conn| store::commands::update(conn, &stored))
                .await?;
        }
        self.changed.notify_waiters();
        Ok(model)
    }
}

/// `SplitCamelCase`: `RefreshMonitoredDownloads` -> `Refresh Monitored Downloads`.
pub fn split_camel_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && c.is_ascii_uppercase() {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_match_without_regard_to_case_and_carry_their_properties() {
        let body = CommandBody::parse("episodesearch", &json!({"episodeIds": [1, 2]})).unwrap();
        assert_eq!(
            body,
            CommandBody::EpisodeSearch {
                episode_ids: vec![1, 2]
            }
        );
        assert_eq!(body.name(), "EpisodeSearch");
        assert!(CommandBody::parse("Nope", &json!({})).is_none());
        let missing = CommandBody::parse("MissingEpisodeSearch", &json!({"seriesId": 2})).unwrap();
        assert_eq!(
            missing,
            CommandBody::MissingEpisodeSearch {
                series_id: Some(2),
                monitored: true
            }
        );
        assert_eq!(
            CommandBody::from_type_name("NzbDrone.Core.Indexers.RssSyncCommand"),
            Some(CommandBody::RssSync)
        );
        assert_eq!(
            CommandBody::RssSync.type_name(),
            "NzbDrone.Core.Indexers.RssSyncCommand"
        );
        assert_eq!(
            split_camel_case("RefreshMonitoredDownloads"),
            "Refresh Monitored Downloads"
        );
    }

    #[tokio::test]
    async fn the_queue_dedupes_orders_and_cancels() {
        let db = Db::open_in_memory().unwrap();
        let queue = CommandQueue::open(db).await.unwrap();
        let now = Utc::now();
        let a = queue
            .push(
                CommandBody::RssSync,
                CommandPriority::Normal,
                CommandTrigger::Manual,
                now,
            )
            .await
            .unwrap();
        let again = queue
            .push(
                CommandBody::RssSync,
                CommandPriority::Normal,
                CommandTrigger::Scheduled,
                now,
            )
            .await
            .unwrap();
        assert_eq!(
            a.id, again.id,
            "an equal active command is returned, not queued twice"
        );
        let b = queue
            .push(
                CommandBody::RefreshMonitoredDownloads,
                CommandPriority::High,
                CommandTrigger::Scheduled,
                now + Duration::seconds(1),
            )
            .await
            .unwrap();
        assert_eq!(queue.all().len(), 2);
        // The high priority one starts first although queued later.
        let first = queue.try_start(now).unwrap();
        assert_eq!(first.id, b.id);
        assert_eq!(first.status, CommandStatus::Started);
        assert!(!queue.cancel(b.id), "a started command cannot be cancelled");
        assert!(queue.cancel(a.id));
        assert!(queue.find(a.id).is_none());
        queue
            .end(
                b.id,
                CommandStatus::Completed,
                Some("done".into()),
                None,
                now + Duration::seconds(2),
            )
            .await
            .unwrap();
        let done = queue.get(b.id).await.unwrap().unwrap();
        assert_eq!(done.result, CommandResult::Successful);
        assert_eq!(done.duration, Some(Duration::seconds(2)));
        assert!(!queue.is_busy());
        // Cleaning drops it from the queue but the table still answers.
        queue.clean(now + Duration::minutes(10)).await.unwrap();
        assert!(queue.find(b.id).is_none());
        assert_eq!(
            queue.get(b.id).await.unwrap().unwrap().message.as_deref(),
            Some("done")
        );
    }
}
