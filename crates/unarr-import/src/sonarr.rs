//! Import of a Sonarr v4 `sonarr.db` into Unarr's schema.
//!
//! The legacy schema is read table by table through `SELECT *` and columns are
//! fetched by name, so a column added by a later migration is simply absent
//! (and defaulted) on older databases. Sonarr stores sub-documents as JSON
//! text in the style of its Dapper mapping (`Images`, `Seasons`, `Quality`,
//! `Items`, `Settings`...); each is decoded here into the matching model or
//! kept verbatim where Unarr does not interpret it yet.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Context;
use chrono::{DateTime, NaiveDate, Utc};
use rusqlite::types::{FromSql, Value as SqlValue};
use rusqlite::{Connection, OpenFlags, Row, Statement};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use unarr_core::decision::{ReleaseSource, SeriesMatchType};
use unarr_core::downloads::pending::{PendingRelease, PendingReleaseReason};
use unarr_core::indexers::rss::ReleaseInfo;
use unarr_core::model::*;
use unarr_core::store;
use unarr_core::store::download_history::{DownloadHistoryEntry, DownloadHistoryEvent};
use unarr_core::store::import_lists::{ListItem, SyncStatus};
use unarr_core::store::provider_status::ProviderStatus;
use unarr_core::{time, Db};

/// Oldest migration level the mapping was written against (196 added
/// `FinaleType`; everything older is best effort).
pub const MIN_MIGRATION: i64 = 196;
/// Newest migration level checked against the Sonarr sources (develop branch).
pub const MAX_KNOWN_MIGRATION: i64 = 240;

/// What an import did.
#[derive(Debug, Default)]
pub struct ImportReport {
    pub migration_version: i64,
    /// Rows imported per legacy table.
    pub counts: BTreeMap<&'static str, usize>,
    /// Human-readable notes about anything skipped or defaulted.
    pub notes: Vec<String>,
    /// Legacy tables with rows the importer does not carry over, with
    /// their row counts.
    pub skipped: BTreeMap<String, i64>,
}

/// Legacy tables the importer reads. Anything else with rows is logged.
const HANDLED_TABLES: &[&str] = &[
    "Tags",
    "RootFolders",
    "QualityDefinitions",
    "CustomFormats",
    "QualityProfiles",
    "Series",
    "Episodes",
    "EpisodeFiles",
    "History",
    "Indexers",
    "DownloadClients",
    "Notifications",
    "ImportLists",
    "Metadata",
    "DelayProfiles",
    "ReleaseProfiles",
    "RemotePathMappings",
    "CustomFilters",
    "ImportListExclusions",
    "AutoTagging",
    "Users",
    "ScheduledTasks",
    "Blocklist",
    "SceneMappings",
    "NamingConfig",
    "Config",
    "PendingReleases",
    "DownloadHistory",
    "IndexerStatus",
    "DownloadClientStatus",
    "ImportListStatus",
    "ImportListItems",
    "VersionInfo",
    "sqlite_sequence",
];

/// Tables with rows that nothing reads, by name.
fn skipped_tables(legacy: &Connection) -> anyhow::Result<BTreeMap<String, i64>> {
    let mut stmt = legacy.prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")?;
    let names: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<_, _>>()?;
    let mut out = BTreeMap::new();
    for name in names {
        if HANDLED_TABLES.contains(&name.as_str()) {
            continue;
        }
        let rows: i64 = legacy.query_row(&format!("SELECT count(*) FROM \"{name}\""), [], |r| r.get(0))?;
        if rows > 0 {
            out.insert(name, rows);
        }
    }
    Ok(out)
}

/// A release as the originals store it (`ReleaseInfo` serialised by their
/// JSON settings: `downloadProtocol`, languages as objects), mapped to
/// Unarr's.
fn legacy_release(v: &Value) -> ReleaseInfo {
    let text = |k: &str| v[k].as_str().map(String::from);
    let int = |k: &str| v[k].as_i64().unwrap_or(0);
    ReleaseInfo {
        guid: text("guid").unwrap_or_default(),
        title: text("title").unwrap_or_default(),
        size: int("size"),
        download_url: text("downloadUrl"),
        info_url: text("infoUrl"),
        comment_url: text("commentUrl"),
        indexer_id: int("indexerId"),
        indexer: text("indexer").unwrap_or_default(),
        indexer_priority: v["indexerPriority"].as_i64().unwrap_or(25),
        season_search_maximum_single_episode_age: int("seasonSearchMaximumSingleEpisodeAge"),
        protocol: match v["downloadProtocol"]
            .as_str()
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("usenet") => Protocol::Usenet,
            Some("torrent") => Protocol::Torrent,
            _ => protocol(int("downloadProtocol")),
        },
        tvdb_id: int("tvdbId"),
        tv_rage_id: int("tvRageId"),
        imdb_id: text("imdbId"),
        publish_date: text("publishDate")
            .and_then(|s| time::parse_utc(&s))
            .unwrap_or(DateTime::<Utc>::UNIX_EPOCH),
        languages: v["languages"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|l| l["id"].as_i64().or_else(|| l.as_i64()))
                    .collect()
            })
            .unwrap_or_default(),
        indexer_flags: int("indexerFlags"),
        magnet_url: text("magnetUrl"),
        info_hash: text("infoHash"),
        seeders: v["seeders"].as_i64(),
        peers: v["peers"].as_i64(),
        container: text("container"),
        pending_release_reason: None,
    }
}

/// The status rows of a provider kind.
fn read_statuses(legacy: &Connection, table: &'static str) -> anyhow::Result<Vec<(i64, ProviderStatus)>> {
    read_table(legacy, table, |r| {
        Ok((
            r.req("ProviderId")?,
            ProviderStatus {
                initial_failure: r.timestamp("InitialFailure")?,
                most_recent_failure: r.timestamp("MostRecentFailure")?,
                escalation_level: r.or("EscalationLevel", 0)?,
                disabled_till: r.timestamp("DisabledTill")?,
                last_rss_sync_release_info: r
                    .json_opt("LastRssSyncReleaseInfo")?
                    .map(|v| serde_json::to_string(&legacy_release(&v)).unwrap_or_default()),
            },
        ))
    })
}

/// Open the legacy database read-only, and only read-only: the original files
/// are never modified, as the README promises. A leftover `-wal` is replayed
/// by SQLite as long as the `-shm` can be created, which read-only mode allows
/// when the directory is writable; a leftover rollback journal cannot be
/// applied without writing, so that import stops with a message.
fn open_legacy(path: &Path) -> anyhow::Result<Connection> {
    let ro = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(path, ro)
        .with_context(|| format!("opening {} read-only", path.display()))?;
    // Force a read so journal recovery problems surface here.
    conn.query_row("SELECT count(*) FROM VersionInfo", [], |r| r.get::<_, i64>(0))
        .with_context(|| {
            format!(
                "reading {} read-only; if the original application left a journal behind, \
                 stop it or copy its files (the database with any -wal and -shm) and import the copy",
                path.display()
            )
        })?;
    Ok(conn)
}

/// Row with column lookup by name that tolerates absent columns.
struct LegacyRow<'a> {
    row: &'a Row<'a>,
    names: &'a [String],
    table: &'static str,
}

impl LegacyRow<'_> {
    fn index(&self, name: &str) -> Option<usize> {
        self.names.iter().position(|n| n == name)
    }

    /// Required column.
    fn req<T: FromSql>(&self, name: &str) -> anyhow::Result<T> {
        let i = self
            .index(name)
            .ok_or_else(|| anyhow::anyhow!("{}.{name} is missing in this database", self.table))?;
        self.row
            .get::<_, T>(i)
            .with_context(|| format!("{}.{name}", self.table))
    }

    /// Column that may be NULL or absent.
    fn opt<T: FromSql>(&self, name: &str) -> anyhow::Result<Option<T>> {
        match self.index(name) {
            None => Ok(None),
            Some(i) => self
                .row
                .get::<_, Option<T>>(i)
                .with_context(|| format!("{}.{name}", self.table)),
        }
    }

    /// Column with a default when NULL or absent.
    fn or<T: FromSql>(&self, name: &str, default: T) -> anyhow::Result<T> {
        Ok(self.opt(name)?.unwrap_or(default))
    }

    fn text(&self, name: &str) -> anyhow::Result<Option<String>> {
        // Sonarr stores the empty string for "no value" in several text
        // columns; the API omits those, so they become None here.
        Ok(self.opt::<String>(name)?.filter(|s| !s.is_empty()))
    }

    fn flag(&self, name: &str, default: bool) -> anyhow::Result<bool> {
        Ok(self.or::<i64>(name, default as i64)? != 0)
    }

    fn json<T: DeserializeOwned + Default>(&self, name: &str) -> anyhow::Result<T> {
        match self.opt::<String>(name)? {
            Some(s) if !s.trim().is_empty() => {
                serde_json::from_str(&s).with_context(|| format!("{}.{name} JSON", self.table))
            }
            _ => Ok(T::default()),
        }
    }

    fn json_opt(&self, name: &str) -> anyhow::Result<Option<Value>> {
        match self.opt::<String>(name)? {
            Some(s) if !s.trim().is_empty() => Ok(Some(
                serde_json::from_str(&s).with_context(|| format!("{}.{name} JSON", self.table))?,
            )),
            _ => Ok(None),
        }
    }

    fn timestamp(&self, name: &str) -> anyhow::Result<Option<DateTime<Utc>>> {
        match self.opt::<SqlValue>(name)? {
            None | Some(SqlValue::Null) => Ok(None),
            Some(SqlValue::Text(s)) => time::parse_utc(&s)
                .map(Some)
                .ok_or_else(|| anyhow::anyhow!("{}.{name}: bad timestamp {s:?}", self.table)),
            Some(other) => anyhow::bail!("{}.{name}: unexpected {other:?}", self.table),
        }
    }

    fn timestamp_req(&self, name: &str) -> anyhow::Result<DateTime<Utc>> {
        self.timestamp(name)?
            .ok_or_else(|| anyhow::anyhow!("{}.{name} is NULL", self.table))
    }

    fn date(&self, name: &str) -> anyhow::Result<Option<NaiveDate>> {
        match self.text(name)? {
            None => Ok(None),
            Some(s) => time::parse_date(&s)
                .map(Some)
                .ok_or_else(|| anyhow::anyhow!("{}.{name}: bad date {s:?}", self.table)),
        }
    }
}

/// Run `SELECT * FROM table` and map every row.
fn read_table<T>(
    legacy: &Connection,
    table: &'static str,
    mut map: impl FnMut(&LegacyRow) -> anyhow::Result<T>,
) -> anyhow::Result<Vec<T>> {
    let exists: i64 = legacy.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |r| r.get(0),
    )?;
    if exists == 0 {
        tracing::warn!(table, "legacy table missing, skipping");
        return Ok(Vec::new());
    }
    let mut stmt: Statement = legacy.prepare(&format!("SELECT * FROM \"{table}\" ORDER BY Id"))?;
    let names: Vec<String> = stmt.column_names().map(|s| s.to_string()).collect();
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(map(&LegacyRow {
            row,
            names: &names,
            table,
        })?);
    }
    Ok(out)
}

/// `{"quality": 7, "revision": {"version": 1, "real": 0, "isRepack": false}}`
fn quality_model(v: Option<Value>) -> QualityModel {
    let Some(v) = v else {
        return QualityModel::default();
    };
    QualityModel {
        quality_id: v["quality"].as_i64().unwrap_or(0),
        version: v["revision"]["version"].as_i64().unwrap_or(1),
        real: v["revision"]["real"].as_i64().unwrap_or(0),
        is_repack: v["revision"]["isRepack"].as_bool().unwrap_or(false),
    }
}

fn series_status(v: i64) -> SeriesStatus {
    match v {
        -1 => SeriesStatus::Deleted,
        1 => SeriesStatus::Ended,
        2 => SeriesStatus::Upcoming,
        _ => SeriesStatus::Continuing,
    }
}

fn series_type(v: i64) -> SeriesType {
    match v {
        1 => SeriesType::Daily,
        2 => SeriesType::Anime,
        _ => SeriesType::Standard,
    }
}

fn release_type(v: i64) -> ReleaseType {
    match v {
        1 => ReleaseType::SingleEpisode,
        2 => ReleaseType::MultiEpisode,
        3 => ReleaseType::SeasonPack,
        4 => ReleaseType::MultiSeasonPack,
        _ => ReleaseType::Unknown,
    }
}

fn protocol(v: i64) -> Protocol {
    match v {
        1 => Protocol::Usenet,
        2 => Protocol::Torrent,
        _ => Protocol::Unknown,
    }
}

fn history_event(v: i64) -> HistoryEvent {
    HistoryEvent::from_number(v).unwrap_or(HistoryEvent::Unknown)
}

/// Sonarr's `MonitorTypes`, in declaration order.
fn monitor_type(v: i64) -> &'static str {
    [
        "unknown",
        "all",
        "future",
        "missing",
        "existing",
        "firstSeason",
        "lastSeason",
        "latestSeason",
        "pilot",
        "recent",
        "monitorSpecials",
        "unmonitorSpecials",
        "none",
        "skip",
    ]
    .get(v as usize)
    .copied()
    .unwrap_or("unknown")
}

/// Legacy season document: `{seasonNumber, monitored, images}`.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacySeason {
    season_number: i64,
    monitored: bool,
    #[serde(default)]
    images: Vec<Image>,
}

/// Import `legacy_db` into `db`, replacing nothing: the target must be empty.
pub fn import(legacy_db: &Path, db: &Db) -> anyhow::Result<ImportReport> {
    let legacy = open_legacy(legacy_db).with_context(|| format!("opening {}", legacy_db.display()))?;
    let migration_version: i64 = legacy
        .query_row("SELECT max(Version) FROM VersionInfo", [], |r| r.get(0))
        .context("reading VersionInfo: is this a Sonarr database?")?;
    let mut report = ImportReport {
        migration_version,
        ..ImportReport::default()
    };
    tracing::info!(
        version = report.migration_version,
        "legacy database migration level"
    );
    anyhow::ensure!(
        report.migration_version >= MIN_MIGRATION,
        "sonarr.db is at migration {}, older than {} which this importer supports; upgrade Sonarr first",
        report.migration_version,
        MIN_MIGRATION
    );
    if report.migration_version > MAX_KNOWN_MIGRATION {
        let note = format!(
            "sonarr.db is at migration {}, newer than {} which this importer knows; unknown columns are ignored",
            report.migration_version, MAX_KNOWN_MIGRATION
        );
        tracing::warn!("{note}");
        report.notes.push(note);
    }

    // --- read everything first, so a mapping error leaves the target untouched
    let tags = read_table(&legacy, "Tags", |r| {
        Ok(Tag {
            id: r.req("Id")?,
            label: r.req("Label")?,
        })
    })?;
    let root_folders = read_table(&legacy, "RootFolders", |r| {
        Ok(RootFolder {
            id: r.req("Id")?,
            path: r.req("Path")?,
        })
    })?;
    let quality_definitions = read_table(&legacy, "QualityDefinitions", |r| {
        Ok(QualityDefinition {
            id: r.req("Id")?,
            quality_id: r.req("Quality")?,
            title: r.req("Title")?,
            min_size: r.opt("MinSize")?,
            max_size: r.opt("MaxSize")?,
            preferred_size: r.opt("PreferredSize")?,
        })
    })?;
    let custom_formats = read_table(&legacy, "CustomFormats", |r| {
        Ok(CustomFormat {
            id: r.req("Id")?,
            name: r.req("Name")?,
            include_when_renaming: r.flag("IncludeCustomFormatWhenRenaming", false)?,
            specifications: r.json_opt("Specifications")?.unwrap_or(json!([])),
        })
    })?;
    let quality_profiles = read_table(&legacy, "QualityProfiles", |r| {
        Ok(QualityProfile {
            id: r.req("Id")?,
            name: r.req("Name")?,
            cutoff: r.req("Cutoff")?,
            upgrade_allowed: r.flag("UpgradeAllowed", false)?,
            items: r.json("Items")?,
            format_items: r.json("FormatItems")?,
            min_format_score: r.or("MinFormatScore", 0)?,
            cutoff_format_score: r.or("CutoffFormatScore", 0)?,
            min_upgrade_format_score: r.or("MinUpgradeFormatScore", 1)?,
        })
    })?;
    let series = read_table(&legacy, "Series", |r| {
        let seasons: Vec<LegacySeason> = r.json("Seasons")?;
        let ratings: Ratings = r.json("Ratings")?;
        Ok(Series {
            id: r.req("Id")?,
            tvdb_id: r.req("TvdbId")?,
            tv_rage_id: r.or("TvRageId", 0)?,
            tv_maze_id: r.or("TvMazeId", 0)?,
            tmdb_id: r.or("TmdbId", 0)?,
            imdb_id: r.text("ImdbId")?,
            title: r.req("Title")?,
            title_slug: r.req("TitleSlug")?,
            clean_title: r.req("CleanTitle")?,
            sort_title: r.text("SortTitle")?,
            status: series_status(r.or("Status", 0)?),
            overview: r.text("Overview")?,
            air_time: r.text("AirTime")?,
            network: r.text("Network")?,
            certification: r.text("Certification")?,
            images: r.json("Images")?,
            genres: r.json("Genres")?,
            actors: r.json_opt("Actors")?.unwrap_or(json!([])),
            path: r.req("Path")?,
            monitored: r.flag("Monitored", true)?,
            season_folder: r.flag("SeasonFolder", true)?,
            monitor_new_items: if r.or("MonitorNewItems", 0)? == 1 {
                NewItemMonitor::None
            } else {
                NewItemMonitor::All
            },
            use_scene_numbering: r.flag("UseSceneNumbering", false)?,
            runtime: r.or("Runtime", 0)?,
            series_type: series_type(r.or("SeriesType", 0)?),
            year: r.or("Year", 0)?,
            first_aired: r.timestamp("FirstAired")?,
            last_aired: r.timestamp("LastAired")?,
            quality_profile_id: r.req("QualityProfileId")?,
            original_language_id: r.or("OriginalLanguage", 1)?,
            ratings,
            added: r.timestamp_req("Added")?,
            add_options: r.json_opt("AddOptions")?,
            last_info_sync: r.timestamp("LastInfoSync")?,
            last_disk_sync: r.timestamp("LastDiskSync")?,
            mal_ids: r.json_opt("MalIds")?,
            anilist_ids: r.json_opt("AniListIds")?,
            seasons: seasons
                .into_iter()
                .map(|s| Season {
                    season_number: s.season_number,
                    monitored: s.monitored,
                    images: s.images,
                })
                .collect(),
            tags: r.json("Tags")?,
        })
    })?;
    let episodes = read_table(&legacy, "Episodes", |r| {
        Ok(Episode {
            id: r.req("Id")?,
            series_id: r.req("SeriesId")?,
            tvdb_id: r.or("TvdbId", 0)?,
            season_number: r.req("SeasonNumber")?,
            episode_number: r.req("EpisodeNumber")?,
            title: r.text("Title")?,
            overview: r.text("Overview")?,
            episode_file_id: r.or("EpisodeFileId", 0)?,
            absolute_episode_number: r.opt("AbsoluteEpisodeNumber")?,
            scene_absolute_episode_number: r.opt("SceneAbsoluteEpisodeNumber")?,
            scene_season_number: r.opt("SceneSeasonNumber")?,
            scene_episode_number: r.opt("SceneEpisodeNumber")?,
            unverified_scene_numbering: r.flag("UnverifiedSceneNumbering", false)?,
            monitored: r.flag("Monitored", true)?,
            air_date: r.date("AirDate")?,
            air_date_utc: r.timestamp("AirDateUtc")?,
            runtime: r.or("Runtime", 0)?,
            finale_type: r.text("FinaleType")?,
            images: r.json("Images")?,
            ratings: r.json("Ratings")?,
            last_search_time: r.timestamp("LastSearchTime")?,
            aired_after_season_number: r.opt("AiredAfterSeasonNumber")?,
            aired_before_season_number: r.opt("AiredBeforeSeasonNumber")?,
            aired_before_episode_number: r.opt("AiredBeforeEpisodeNumber")?,
        })
    })?;
    let episode_files = read_table(&legacy, "EpisodeFiles", |r| {
        Ok(EpisodeFile {
            id: r.req("Id")?,
            series_id: r.req("SeriesId")?,
            season_number: r.req("SeasonNumber")?,
            relative_path: r.req("RelativePath")?,
            original_file_path: r.text("OriginalFilePath")?,
            size: r.or("Size", 0)?,
            date_added: r.timestamp_req("DateAdded")?,
            scene_name: r.text("SceneName")?,
            release_group: r.text("ReleaseGroup")?,
            release_hash: r.text("ReleaseHash")?,
            quality: quality_model(r.json_opt("Quality")?),
            languages: r.json("Languages")?,
            media_info: r.json_opt("MediaInfo")?,
            indexer_flags: r.or("IndexerFlags", 0)?,
            release_type: release_type(r.or("ReleaseType", 0)?),
        })
    })?;
    let history = read_table(&legacy, "History", |r| {
        Ok(HistoryEntry {
            id: r.req("Id")?,
            series_id: r.or("SeriesId", 0)?,
            episode_id: r.or("EpisodeId", 0)?,
            source_title: r.text("SourceTitle")?,
            date: r.timestamp_req("Date")?,
            event_type: history_event(r.or("EventType", 0)?),
            quality: quality_model(r.json_opt("Quality")?),
            languages: r.json("Languages")?,
            data: r.json_opt("Data")?.unwrap_or(json!({})),
            download_id: r.text("DownloadId")?,
        })
    })?;

    let provider = |r: &LegacyRow, kind: ProviderKind, attributes: Value| -> anyhow::Result<Provider> {
        Ok(Provider {
            kind,
            id: r.req("Id")?,
            name: r.req("Name")?,
            implementation: r.req("Implementation")?,
            config_contract: r.or("ConfigContract", String::new())?,
            settings: r.json_opt("Settings")?.unwrap_or(json!({})),
            tags: r.json("Tags")?,
            attributes,
        })
    };
    let mut providers = read_table(&legacy, "Indexers", |r| {
        let attributes = json!({
            "enableRss": r.flag("EnableRss", false)?,
            "enableAutomaticSearch": r.flag("EnableAutomaticSearch", false)?,
            "enableInteractiveSearch": r.flag("EnableInteractiveSearch", false)?,
            "priority": r.or("Priority", 25)?,
            "downloadClientId": r.or("DownloadClientId", 0)?,
            "seasonSearchMaximumSingleEpisodeAge": r.or("SeasonSearchMaximumSingleEpisodeAge", 0)?,
        });
        provider(r, ProviderKind::Indexer, attributes)
    })?;
    providers.extend(read_table(&legacy, "DownloadClients", |r| {
        let attributes = json!({
            "enable": r.flag("Enable", false)?,
            "priority": r.or("Priority", 1)?,
            "removeCompletedDownloads": r.flag("RemoveCompletedDownloads", true)?,
            "removeFailedDownloads": r.flag("RemoveFailedDownloads", true)?,
        });
        provider(r, ProviderKind::DownloadClient, attributes)
    })?);
    providers.extend(read_table(&legacy, "Notifications", |r| {
        let mut attributes = serde_json::Map::new();
        for (column, name) in [
            ("OnGrab", "onGrab"),
            ("OnDownload", "onDownload"),
            ("OnUpgrade", "onUpgrade"),
            ("OnImportComplete", "onImportComplete"),
            ("OnRename", "onRename"),
            ("OnSeriesAdd", "onSeriesAdd"),
            ("OnSeriesDelete", "onSeriesDelete"),
            ("OnEpisodeFileDelete", "onEpisodeFileDelete"),
            ("OnEpisodeFileDeleteForUpgrade", "onEpisodeFileDeleteForUpgrade"),
            ("OnHealthIssue", "onHealthIssue"),
            ("IncludeHealthWarnings", "includeHealthWarnings"),
            ("OnHealthRestored", "onHealthRestored"),
            ("OnApplicationUpdate", "onApplicationUpdate"),
            ("OnManualInteractionRequired", "onManualInteractionRequired"),
        ] {
            attributes.insert(name.into(), json!(r.flag(column, false)?));
        }
        provider(r, ProviderKind::Notification, Value::Object(attributes))
    })?);
    providers.extend(read_table(&legacy, "ImportLists", |r| {
        let attributes = json!({
            "enableAutomaticAdd": r.flag("EnableAutomaticAdd", false)?,
            "searchForMissingEpisodes": r.flag("SearchForMissingEpisodes", false)?,
            "shouldMonitor": monitor_type(r.or("ShouldMonitor", 0)?),
            "monitorNewItems": if r.or("MonitorNewItems", 0)? == 1 { "none" } else { "all" },
            "rootFolderPath": r.text("RootFolderPath")?,
            "qualityProfileId": r.or("QualityProfileId", 0)?,
            "seriesType": series_type(r.or("SeriesType", 0)?),
            "seasonFolder": r.flag("SeasonFolder", true)?,
        });
        provider(r, ProviderKind::ImportList, attributes)
    })?);
    providers.extend(read_table(&legacy, "Metadata", |r| {
        let attributes = json!({ "enable": r.flag("Enable", false)? });
        provider(r, ProviderKind::Metadata, attributes)
    })?);

    // AUTOINCREMENT sequences of the provider tables, so ids created later
    // continue where the original left off rather than reusing deleted ones.
    let sequences: Vec<(ProviderKind, i64)> = [
        ("Indexers", ProviderKind::Indexer),
        ("DownloadClients", ProviderKind::DownloadClient),
        ("Notifications", ProviderKind::Notification),
        ("ImportLists", ProviderKind::ImportList),
        ("Metadata", ProviderKind::Metadata),
    ]
    .into_iter()
    .filter_map(|(table, kind)| {
        legacy
            .query_row("SELECT seq FROM sqlite_sequence WHERE name = ?1", [table], |r| {
                r.get::<_, i64>(0)
            })
            .ok()
            .map(|seq| (kind, seq))
    })
    .collect();

    let delay_profiles = read_table(&legacy, "DelayProfiles", |r| {
        Ok(DelayProfile {
            id: r.req("Id")?,
            enable_usenet: r.flag("EnableUsenet", true)?,
            enable_torrent: r.flag("EnableTorrent", true)?,
            preferred_protocol: protocol(r.or("PreferredProtocol", 1)?),
            usenet_delay: r.or("UsenetDelay", 0)?,
            torrent_delay: r.or("TorrentDelay", 0)?,
            order: r.or("Order", i32::MAX as i64)?,
            tags: r.json("Tags")?,
            bypass_if_highest_quality: r.flag("BypassIfHighestQuality", false)?,
            bypass_if_above_custom_format_score: r.flag("BypassIfAboveCustomFormatScore", false)?,
            minimum_custom_format_score: r.or("MinimumCustomFormatScore", 0)?,
        })
    })?;
    // Older schemas kept Required/Ignored as newline-separated text.
    let term_list = |r: &LegacyRow, column: &str| -> anyhow::Result<Vec<String>> {
        match r.opt::<String>(column)? {
            None => Ok(Vec::new()),
            Some(s) if s.trim_start().starts_with('[') => Ok(serde_json::from_str(&s)?),
            Some(s) => Ok(s
                .split(['\n', ','])
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect()),
        }
    };
    let release_profiles = read_table(&legacy, "ReleaseProfiles", |r| {
        Ok(ReleaseProfile {
            id: r.req("Id")?,
            name: r.text("Name")?,
            enabled: r.flag("Enabled", true)?,
            required: term_list(r, "Required")?,
            ignored: term_list(r, "Ignored")?,
            indexer_id: r.or("IndexerId", 0)?,
            tags: r.json("Tags")?,
        })
    })?;
    let remote_path_mappings = read_table(&legacy, "RemotePathMappings", |r| {
        Ok(RemotePathMapping {
            id: r.req("Id")?,
            host: r.req("Host")?,
            remote_path: r.req("RemotePath")?,
            local_path: r.req("LocalPath")?,
        })
    })?;
    let custom_filters = read_table(&legacy, "CustomFilters", |r| {
        Ok(CustomFilter {
            id: r.req("Id")?,
            filter_type: r.req("Type")?,
            label: r.req("Label")?,
            filters: r.json_opt("Filters")?.unwrap_or(json!([])),
        })
    })?;
    let exclusions = read_table(&legacy, "ImportListExclusions", |r| {
        Ok(ImportListExclusion {
            id: r.req("Id")?,
            tvdb_id: r.req("TvdbId")?,
            title: r.or("Title", String::new())?,
        })
    })?;
    let auto_tagging = read_table(&legacy, "AutoTagging", |r| {
        Ok(AutoTagging {
            id: r.req("Id")?,
            name: r.req("Name")?,
            remove_tags_automatically: r.flag("RemoveTagsAutomatically", false)?,
            tags: r.json("Tags")?,
            specifications: r.json_opt("Specifications")?.unwrap_or(json!([])),
        })
    })?;
    let users = read_table(&legacy, "Users", |r| {
        Ok(User {
            id: r.req("Id")?,
            identifier: r.req("Identifier")?,
            username: r.req("Username")?,
            password: r.req("Password")?,
            salt: r.text("Salt")?,
            iterations: r.opt("Iterations")?,
        })
    })?;
    let scheduled_tasks = read_table(&legacy, "ScheduledTasks", |r| {
        Ok(ScheduledTask {
            id: r.req("Id")?,
            type_name: r.req("TypeName")?,
            interval_minutes: r.or("Interval", 0)?,
            last_execution: r.timestamp("LastExecution")?,
            last_start_time: r.timestamp("LastStartTime")?,
        })
    })?;
    let blocklist = read_table(&legacy, "Blocklist", |r| {
        Ok(BlocklistEntry {
            id: r.req("Id")?,
            series_id: r.or("SeriesId", 0)?,
            episode_ids: r.json("EpisodeIds")?,
            source_title: r.text("SourceTitle")?,
            quality: quality_model(r.json_opt("Quality")?),
            date: r.timestamp_req("Date")?,
            published_date: r.timestamp("PublishedDate")?,
            size: r.opt("Size")?,
            protocol: protocol(r.or("Protocol", 0)?),
            indexer: r.text("Indexer")?,
            message: r.text("Message")?,
            torrent_info_hash: r.text("TorrentInfoHash")?,
            languages: r.json("Languages")?,
            indexer_flags: r.or("IndexerFlags", 0)?,
            release_type: release_type(r.or("ReleaseType", 0)?),
        })
    })?;
    let scene_mappings = read_table(&legacy, "SceneMappings", |r| {
        Ok(SceneMapping {
            id: r.req("Id")?,
            tvdb_id: r.req("TvdbId")?,
            season_number: r.opt("SeasonNumber")?,
            search_term: r.text("SearchTerm")?,
            parse_term: r.text("ParseTerm")?,
            title: r.or("Title", String::new())?,
            mapping_type: r.text("Type")?,
            scene_season_number: r.opt("SceneSeasonNumber")?,
            filter_regex: r.text("FilterRegex")?,
            scene_origin: r.text("SceneOrigin")?,
            search_mode: r.opt("SearchMode")?,
            comment: r.text("Comment")?,
        })
    })?;
    let naming = read_table(&legacy, "NamingConfig", |r| {
        let d = NamingConfig::default();
        Ok(NamingConfig {
            rename_episodes: r.flag("RenameEpisodes", d.rename_episodes)?,
            replace_illegal_characters: r.flag("ReplaceIllegalCharacters", d.replace_illegal_characters)?,
            colon_replacement_format: r.or("ColonReplacementFormat", d.colon_replacement_format)?,
            custom_colon_replacement_format: r.or("CustomColonReplacementFormat", String::new())?,
            multi_episode_style: r.or("MultiEpisodeStyle", d.multi_episode_style)?,
            standard_episode_format: r.or("StandardEpisodeFormat", d.standard_episode_format)?,
            daily_episode_format: r.or("DailyEpisodeFormat", d.daily_episode_format)?,
            anime_episode_format: r.or("AnimeEpisodeFormat", d.anime_episode_format)?,
            series_folder_format: r.or("SeriesFolderFormat", d.series_folder_format)?,
            season_folder_format: r.or("SeasonFolderFormat", d.season_folder_format)?,
            specials_folder_format: r.or("SpecialsFolderFormat", d.specials_folder_format)?,
        })
    })?;
    let config = read_table(&legacy, "Config", |r| {
        Ok((r.req::<String>("Key")?, r.or("Value", String::new())?))
    })?;

    // Tags referenced by a series but missing from Tags would break the
    // foreign key; the originals tolerate that, so note and drop them.
    let tag_ids: std::collections::HashSet<i64> = tags.iter().map(|t| t.id).collect();
    let series: Vec<Series> = series
        .into_iter()
        .map(|mut s| {
            let before = s.tags.len();
            s.tags.retain(|t| tag_ids.contains(t));
            if s.tags.len() != before {
                report.notes.push(format!(
                    "series {} ({}): dropped {} unknown tag id(s)",
                    s.id,
                    s.title,
                    before - s.tags.len()
                ));
            }
            s
        })
        .collect();

    // The originals do not enforce the link between a series and its episodes
    // and files; Unarr's schema does. Rows left behind by a removed series
    // would stop the import, so note and drop them.
    let series_ids: std::collections::HashSet<i64> = series.iter().map(|s| s.id).collect();
    let before = episodes.len();
    let episodes: Vec<Episode> = episodes
        .into_iter()
        .filter(|e| series_ids.contains(&e.series_id))
        .collect();
    if episodes.len() != before {
        report.notes.push(format!(
            "dropped {} episode(s) whose series no longer exists",
            before - episodes.len()
        ));
    }
    let before = episode_files.len();
    let episode_files: Vec<EpisodeFile> = episode_files
        .into_iter()
        .filter(|f| series_ids.contains(&f.series_id))
        .collect();
    if episode_files.len() != before {
        report.notes.push(format!(
            "dropped {} episode file(s) whose series no longer exists",
            before - episode_files.len()
        ));
    }

    let pending_releases = read_table(&legacy, "PendingReleases", |r| {
        let additional = r.json_opt("AdditionalInfo")?.unwrap_or(Value::Null);
        let enum_text = |v: &Value| {
            v.as_str()
                .map(str::to_string)
                .or_else(|| v.as_i64().map(|n| n.to_string()))
        };
        Ok(PendingRelease {
            id: r.req("Id")?,
            series_id: r.req("SeriesId")?,
            title: r.req("Title")?,
            added: r.timestamp_req("Added")?,
            release: legacy_release(&r.json_opt("Release")?.unwrap_or(Value::Null)),
            reason: PendingReleaseReason::from_i64(r.or("Reason", 0)?),
            series_match_type: enum_text(&additional["seriesMatchType"])
                .and_then(|s| match s.as_str() {
                    "0" => Some(SeriesMatchType::Unknown),
                    "1" => Some(SeriesMatchType::Title),
                    "2" => Some(SeriesMatchType::Alias),
                    "3" => Some(SeriesMatchType::Id),
                    other => SeriesMatchType::parse(other),
                })
                .unwrap_or_default(),
            release_source: enum_text(&additional["releaseSource"])
                .and_then(|s| match s.as_str() {
                    "0" => Some(ReleaseSource::Unknown),
                    "1" => Some(ReleaseSource::Rss),
                    "2" => Some(ReleaseSource::Search),
                    "3" => Some(ReleaseSource::UserInvokedSearch),
                    "4" => Some(ReleaseSource::InteractiveSearch),
                    "5" => Some(ReleaseSource::ReleasePush),
                    other => ReleaseSource::parse(other),
                })
                .unwrap_or_default(),
        })
    })?;
    let download_history = read_table(&legacy, "DownloadHistory", |r| {
        Ok(DownloadHistoryEntry {
            id: r.req("Id")?,
            event_type: match r.or("EventType", 0)? {
                1 => DownloadHistoryEvent::DownloadGrabbed,
                2 => DownloadHistoryEvent::DownloadImported,
                3 => DownloadHistoryEvent::DownloadFailed,
                4 => DownloadHistoryEvent::DownloadIgnored,
                5 => DownloadHistoryEvent::FileImported,
                other => anyhow::bail!("DownloadHistory.EventType: unknown value {other}"),
            },
            series_id: r.or("SeriesId", 0)?,
            download_id: r.or("DownloadId", String::new())?,
            source_title: r.or("SourceTitle", String::new())?,
            date: r.timestamp_req("Date")?,
            protocol: protocol(r.or("Protocol", 0)?),
            indexer_id: r.or("IndexerId", 0)?,
            download_client_id: r.or("DownloadClientId", 0)?,
            release: r.json_opt("Release")?,
            data: r.json_opt("Data")?.unwrap_or(json!({})),
        })
    })?;
    let indexer_statuses = read_statuses(&legacy, "IndexerStatus")?;
    let download_client_statuses = read_statuses(&legacy, "DownloadClientStatus")?;
    let import_list_statuses = read_table(&legacy, "ImportListStatus", |r| {
        Ok((
            r.req::<i64>("ProviderId")?,
            ProviderStatus {
                initial_failure: r.timestamp("InitialFailure")?,
                most_recent_failure: r.timestamp("MostRecentFailure")?,
                escalation_level: r.or("EscalationLevel", 0)?,
                disabled_till: r.timestamp("DisabledTill")?,
                last_rss_sync_release_info: None,
            },
            SyncStatus {
                last_info_sync: r.timestamp("LastInfoSync")?,
                has_removed_item_since_last_clean: r.flag("HasRemovedItemSinceLastClean", false)?,
            },
        ))
    })?;
    let import_list_items = read_table(&legacy, "ImportListItems", |r| {
        Ok(ListItem {
            id: 0,
            import_list_id: r.req("ImportListId")?,
            title: r.or("Title", String::new())?,
            tvdb_id: r.or("TvdbId", 0)?,
            year: r.or("Year", 0)?,
            tmdb_id: r.or("TmdbId", 0)?,
            imdb_id: r.text("ImdbId")?,
            mal_id: r.or("MalId", 0)?,
            ani_list_id: r.or("AniListId", 0)?,
        })
    })?;
    // Rows of a provider that no longer exists would never be read.
    let provider_exists =
        |kind: ProviderKind, id: i64| providers.iter().any(|p| p.kind == kind && p.id == id);
    let indexer_statuses: Vec<_> = indexer_statuses
        .into_iter()
        .filter(|(id, _)| provider_exists(ProviderKind::Indexer, *id))
        .collect();
    let download_client_statuses: Vec<_> = download_client_statuses
        .into_iter()
        .filter(|(id, _)| provider_exists(ProviderKind::DownloadClient, *id))
        .collect();
    let import_list_statuses: Vec<_> = import_list_statuses
        .into_iter()
        .filter(|(id, _, _)| provider_exists(ProviderKind::ImportList, *id))
        .collect();
    let import_list_items: Vec<_> = import_list_items
        .into_iter()
        .filter(|i| provider_exists(ProviderKind::ImportList, i.import_list_id))
        .collect();

    report.skipped = skipped_tables(&legacy)?;
    for (table, rows) in &report.skipped {
        tracing::info!(table, rows, "legacy table not imported");
    }

    // --- write in one transaction
    let counts = [
        ("Tags", tags.len()),
        ("RootFolders", root_folders.len()),
        ("QualityDefinitions", quality_definitions.len()),
        ("CustomFormats", custom_formats.len()),
        ("QualityProfiles", quality_profiles.len()),
        ("Series", series.len()),
        ("Episodes", episodes.len()),
        ("EpisodeFiles", episode_files.len()),
        ("History", history.len()),
        (
            "Indexers",
            providers
                .iter()
                .filter(|p| p.kind == ProviderKind::Indexer)
                .count(),
        ),
        (
            "DownloadClients",
            providers
                .iter()
                .filter(|p| p.kind == ProviderKind::DownloadClient)
                .count(),
        ),
        (
            "Notifications",
            providers
                .iter()
                .filter(|p| p.kind == ProviderKind::Notification)
                .count(),
        ),
        (
            "ImportLists",
            providers
                .iter()
                .filter(|p| p.kind == ProviderKind::ImportList)
                .count(),
        ),
        (
            "Metadata",
            providers
                .iter()
                .filter(|p| p.kind == ProviderKind::Metadata)
                .count(),
        ),
        ("DelayProfiles", delay_profiles.len()),
        ("ReleaseProfiles", release_profiles.len()),
        ("RemotePathMappings", remote_path_mappings.len()),
        ("CustomFilters", custom_filters.len()),
        ("ImportListExclusions", exclusions.len()),
        ("AutoTagging", auto_tagging.len()),
        ("Users", users.len()),
        ("ScheduledTasks", scheduled_tasks.len()),
        ("Blocklist", blocklist.len()),
        ("SceneMappings", scene_mappings.len()),
        ("NamingConfig", naming.len()),
        ("Config", config.len()),
        ("PendingReleases", pending_releases.len()),
        ("DownloadHistory", download_history.len()),
        ("IndexerStatus", indexer_statuses.len()),
        ("DownloadClientStatus", download_client_statuses.len()),
        ("ImportListStatus", import_list_statuses.len()),
        ("ImportListItems", import_list_items.len()),
    ];
    report.counts.extend(counts);
    let migration_version = report.migration_version;
    let source = legacy_db.display().to_string();

    db.call_blocking(move |conn| {
        anyhow::ensure!(
            store::series::count(conn)? == 0,
            "target database already holds data; import into an empty database"
        );
        let tx = conn.transaction()?;
        for t in &tags {
            store::catalog::insert_tag(&tx, t)?;
        }
        for r in &root_folders {
            store::catalog::insert_root_folder(&tx, r)?;
        }
        for d in &quality_definitions {
            store::catalog::insert_quality_definition(&tx, d)?;
        }
        for c in &custom_formats {
            store::catalog::insert_custom_format(&tx, c)?;
        }
        for p in &quality_profiles {
            store::catalog::insert_quality_profile(&tx, p)?;
        }
        for s in &series {
            store::series::insert(&tx, s).with_context(|| format!("series {} ({})", s.id, s.title))?;
        }
        for f in &episode_files {
            store::episodes::insert_file(&tx, f).with_context(|| format!("episode file {}", f.id))?;
        }
        for e in &episodes {
            store::episodes::insert_episode(&tx, e).with_context(|| format!("episode {}", e.id))?;
        }
        for h in &history {
            store::history::insert(&tx, h).with_context(|| format!("history {}", h.id))?;
        }
        // The originals never reuse an id once its row is deleted; the
        // counters continue where theirs stopped.
        for (table, ours) in [
            ("EpisodeFiles", "episode_files"),
            ("History", "history"),
            ("Episodes", "episodes"),
            ("Series", "series"),
            ("SceneMappings", "scene_mappings"),
            ("Tags", "tags"),
            ("RootFolders", "root_folders"),
            ("QualityProfiles", "quality_profiles"),
            ("CustomFormats", "custom_formats"),
            ("DelayProfiles", "delay_profiles"),
            ("ReleaseProfiles", "release_profiles"),
            ("RemotePathMappings", "remote_path_mappings"),
            ("ImportListExclusions", "import_list_exclusions"),
            ("CustomFilters", "custom_filters"),
            ("AutoTagging", "auto_tagging"),
        ] {
            if let Ok(seq) =
                legacy.query_row("SELECT seq FROM sqlite_sequence WHERE name = ?1", [table], |r| {
                    r.get::<_, i64>(0)
                })
            {
                store::sequence::seed(&tx, ours, seq)?;
            }
        }
        for p in &providers {
            store::providers::insert_provider(&tx, p)?;
        }
        for d in &delay_profiles {
            store::providers::insert_delay_profile(&tx, d)?;
        }
        for r in &release_profiles {
            store::providers::insert_release_profile(&tx, r)?;
        }
        for m in &remote_path_mappings {
            store::providers::insert_remote_path_mapping(&tx, m)?;
        }
        for f in &custom_filters {
            store::providers::insert_custom_filter(&tx, f)?;
        }
        for e in &exclusions {
            store::providers::insert_import_list_exclusion(&tx, e)?;
        }
        for a in &auto_tagging {
            store::providers::insert_auto_tagging(&tx, a)?;
        }
        for u in &users {
            store::providers::insert_user(&tx, u)?;
        }
        // A blank first start seeds the default tasks with ids of its own;
        // the original's rows, last runs included, take their place.
        tx.execute("DELETE FROM scheduled_tasks", [])?;
        for t in &scheduled_tasks {
            store::providers::insert_scheduled_task(&tx, t)?;
        }
        for b in &blocklist {
            store::history::insert_blocklist(&tx, b)?;
        }
        for m in &scene_mappings {
            store::scene::insert(&tx, m)?;
        }
        for p in &pending_releases {
            store::pending::insert(&tx, p)?;
        }
        for h in &download_history {
            store::download_history::insert(&tx, h)?;
        }
        for (id, status) in &indexer_statuses {
            store::provider_status::upsert_provider_status(&tx, ProviderKind::Indexer, *id, status)?;
        }
        for (id, status) in &download_client_statuses {
            store::provider_status::upsert_provider_status(&tx, ProviderKind::DownloadClient, *id, status)?;
        }
        for (id, status, sync) in &import_list_statuses {
            store::provider_status::upsert_provider_status(&tx, ProviderKind::ImportList, *id, status)?;
            store::import_lists::set_sync_status(&tx, *id, sync)?;
        }
        for item in &import_list_items {
            store::import_lists::insert_item(&tx, item)?;
        }
        if let Some(n) = naming.first() {
            store::config::set_naming(&tx, n)?;
        }
        for (k, v) in &config {
            store::config::set_config(&tx, k, v)?;
        }
        for (kind, last_id) in &sequences {
            store::providers::set_provider_sequence(&tx, *kind, *last_id)?;
        }
        store::config::set_meta(&tx, "source.app", "Sonarr")?;
        store::config::set_meta(&tx, "source.path", &source)?;
        store::config::set_meta(&tx, "source.migration", &migration_version.to_string())?;
        store::config::set_meta(&tx, "imported_at", &time::format_utc(Utc::now()))?;
        tx.commit()?;
        Ok(())
    })?;

    for (table, n) in &report.counts {
        tracing::info!(table, rows = n, "imported");
    }
    for note in &report.notes {
        tracing::warn!("{note}");
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/sonarr/sonarr.db"
    );

    /// Episodes and files left behind by a removed series must not stop the
    /// import: the originals have no foreign keys, Unarr does.
    #[test]
    fn drops_rows_whose_series_is_gone() {
        let dir = std::env::temp_dir().join(format!("unarr-orphans-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let copy = dir.join("sonarr.db");
        std::fs::copy(FIXTURE, &copy).unwrap();
        {
            let legacy = rusqlite::Connection::open(&copy).unwrap();
            legacy
                .execute_batch(
                    "INSERT INTO Episodes (SeriesId, SeasonNumber, EpisodeNumber, Title, EpisodeFileId, Monitored, UnverifiedSceneNumbering, Runtime)
                       VALUES (999999, 1, 1, 'Orphan', 0, 1, 0, 0);
                     INSERT INTO EpisodeFiles (SeriesId, Quality, Size, DateAdded, SeasonNumber, RelativePath, Languages, IndexerFlags, ReleaseType)
                       VALUES (999999, '{\"quality\": 1, \"revision\": {\"version\": 1, \"real\": 0, \"isRepack\": false}}', 1, '2026-01-01 00:00:00Z', 1, 'orphan.mkv', '[1]', 0, 1);",
                )
                .unwrap();
        }
        let db = Db::open_in_memory().unwrap();
        let report = import(&copy, &db).unwrap();
        assert_eq!(report.counts["Episodes"], 1012);
        assert_eq!(report.counts["EpisodeFiles"], 346);
        assert_eq!(
            report.notes,
            vec![
                "dropped 1 episode(s) whose series no longer exists".to_string(),
                "dropped 1 episode file(s) whose series no longer exists".to_string(),
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn imports_the_fixture_with_matching_row_counts() {
        let db = Db::open_in_memory().unwrap();
        let report = import(Path::new(FIXTURE), &db).unwrap();
        assert_eq!(report.migration_version, 217);
        assert!(report.notes.is_empty(), "{:?}", report.notes);
        let expect = [
            ("Series", 38),
            ("Episodes", 1012),
            ("EpisodeFiles", 346),
            ("History", 1998),
            ("Blocklist", 220),
            ("QualityProfiles", 10),
            ("QualityDefinitions", 22),
            ("CustomFormats", 40),
            ("Tags", 2),
            ("RootFolders", 1),
            ("Indexers", 15),
            ("DownloadClients", 3),
            ("Notifications", 3),
            ("ImportLists", 1),
            ("Metadata", 5),
            ("ReleaseProfiles", 0),
            ("DelayProfiles", 2),
            ("CustomFilters", 1),
            ("RemotePathMappings", 0),
            ("ScheduledTasks", 11),
            ("Config", 16),
            ("NamingConfig", 1),
            ("Users", 1),
            ("SceneMappings", 16064),
            ("DownloadHistory", 1726),
            ("PendingReleases", 0),
            ("IndexerStatus", 0),
            ("DownloadClientStatus", 0),
            ("ImportListStatus", 0),
            ("ImportListItems", 1),
        ];
        for (table, n) in expect {
            assert_eq!(report.counts[table], n, "{table}");
        }
        // Tables with rows nothing reads are reported, not silently dropped.
        assert_eq!(
            report.skipped.keys().map(String::as_str).collect::<Vec<_>>(),
            ["MetadataFiles", "SubtitleFiles"],
            "{:?}",
            report.skipped
        );

        db.call_blocking(|conn| {
            // The Pitt: continuing, tagged, two seasons (specials and 1).
            let pitt = store::series::get(conn, 1)?.unwrap();
            assert_eq!(pitt.title, "The Pitt");
            assert_eq!(pitt.status, SeriesStatus::Continuing);
            assert_eq!(pitt.seasons.len(), 2);
            assert_eq!(pitt.tags, vec![1]);
            assert_eq!(pitt.quality_profile_id, 10);
            assert_eq!(pitt.images.len(), 4);
            assert_eq!(pitt.genres[0], "Drama");
            assert_eq!(pitt.path, "/tv/The Pitt");
            // Silicon Valley: ended, the series with most files.
            let sv = store::series::get(conn, 22)?.unwrap();
            assert_eq!(sv.status, SeriesStatus::Ended);
            assert_eq!(sv.title_slug, "silicon-valley");

            let eps = store::episodes::list_episodes(conn, 1, Some(1))?;
            assert_eq!(eps.len(), 15);
            assert_eq!(eps[0].title.as_deref(), Some("7:00 A.M."));
            assert_eq!(
                eps[0].air_date.map(|d| d.to_string()).as_deref(),
                Some("2025-01-09")
            );
            let file = store::episodes::get_file(conn, 361)?.unwrap();
            assert_eq!(file.series_id, 22);
            assert_eq!(file.quality.quality_id, 3);
            assert!(file.quality.is_repack);
            assert_eq!(file.quality.version, 2);
            assert_eq!(file.release_type, ReleaseType::SingleEpisode);
            assert_eq!(file.languages, vec![1]);
            Ok(())
        })
        .unwrap();
    }

    /// The acquisition tables: a pending release and the statuses of an
    /// indexer and a client, as the originals store them.
    #[test]
    fn imports_pending_releases_and_provider_statuses() {
        let dir = std::env::temp_dir().join(format!("unarr-acquisition-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let copy = dir.join("sonarr.db");
        std::fs::copy(FIXTURE, &copy).unwrap();
        {
            let legacy = rusqlite::Connection::open(&copy).unwrap();
            legacy
                .execute_batch(
                    r#"INSERT INTO PendingReleases (SeriesId, Title, Added, ParsedEpisodeInfo, Release, Reason, AdditionalInfo)
                       VALUES (1, 'The.Pitt.S01E01.iTALiAN.1080p.WEB.H264-MeM.GP', '2026-03-06 07:48:20.6457169Z', '{}',
                               '{"guid": "http://indexer.invalid/abc", "title": "The.Pitt.S01E01.iTALiAN.1080p.WEB.H264-MeM.GP", "size": 2254857830,
                                 "downloadUrl": "http://indexer.invalid/abc.nzb", "indexerId": 1, "indexer": "Indexer", "indexerPriority": 25,
                                 "downloadProtocol": "usenet", "tvdbId": 448176, "publishDate": "2025-01-11T09:05:00Z",
                                 "languages": [{"id": 5, "name": "Italian"}], "age": 3, "ageHours": 72.5}',
                               1, '{"seriesMatchType": "title", "releaseSource": "rss"}');
                     INSERT INTO IndexerStatus (ProviderId, InitialFailure, MostRecentFailure, EscalationLevel, DisabledTill, LastRssSyncReleaseInfo)
                       VALUES (1, '2026-03-06 07:00:00Z', '2026-03-06 07:48:20Z', 3, '2026-03-06 08:03:20Z',
                               '{"guid": "http://indexer.invalid/last", "title": "Last.One.S01E01", "downloadUrl": "http://indexer.invalid/last.nzb", "publishDate": "2026-03-06T07:00:00Z", "downloadProtocol": "usenet"}');
                     INSERT INTO IndexerStatus (ProviderId, EscalationLevel) VALUES (999, 1);
                     INSERT INTO DownloadClientStatus (ProviderId, InitialFailure, MostRecentFailure, EscalationLevel, DisabledTill)
                       VALUES (2, '2026-03-06 07:00:00Z', '2026-03-06 07:48:20Z', 2, NULL);"#,
                )
                .unwrap();
        }
        let db = Db::open_in_memory().unwrap();
        let report = import(&copy, &db).unwrap();
        assert_eq!(report.counts["PendingReleases"], 1);
        assert_eq!(
            report.counts["IndexerStatus"], 1,
            "the status of a provider that is gone is dropped"
        );
        assert_eq!(report.counts["DownloadClientStatus"], 1);
        db.call_blocking(|conn| {
            let pending = store::pending::all(conn)?;
            assert_eq!(pending.len(), 1);
            let p = &pending[0];
            assert!(p.id > 0, "the legacy id is kept");
            assert_eq!(p.reason, PendingReleaseReason::DownloadClientUnavailable);
            assert_eq!(p.series_match_type, SeriesMatchType::Title);
            assert_eq!(p.release_source, ReleaseSource::Rss);
            assert_eq!(p.release.protocol, Protocol::Usenet);
            assert_eq!(p.release.languages, vec![5]);
            assert_eq!(p.release.size, 2254857830);
            assert_eq!(time::format_utc(p.release.publish_date), "2025-01-11T09:05:00Z");
            let status = store::provider_status::provider_status(conn, ProviderKind::Indexer, 1)?;
            assert_eq!(status.escalation_level, 3);
            assert_eq!(
                time::format_utc(status.disabled_till.unwrap()),
                "2026-03-06T08:03:20Z"
            );
            let last: serde_json::Value =
                serde_json::from_str(status.last_rss_sync_release_info.as_deref().unwrap())?;
            assert_eq!(last["downloadUrl"], "http://indexer.invalid/last.nzb");
            assert_eq!(last["publishDate"], "2026-03-06T07:00:00Z");
            let client = store::provider_status::provider_status(conn, ProviderKind::DownloadClient, 2)?;
            assert_eq!(client.escalation_level, 2);
            assert_eq!(client.disabled_till, None);
            let history =
                store::download_history::latest_for_download_id(conn, "0d4b6b4be80f4c06a5218e0d4ca92856")?
                    .unwrap();
            assert_eq!(
                history.event_type,
                DownloadHistoryEvent::DownloadFailed,
                "the latest row of that download"
            );
            assert_eq!(history.download_client_id, 2);
            Ok(())
        })
        .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A blank first start seeds the default scheduled tasks; the import
    /// that follows must replace them, not collide with their ids.
    #[test]
    fn imports_after_a_blank_start_seeded_the_scheduled_tasks() {
        let db = Db::open_in_memory().unwrap();
        let now = chrono::Utc::now();
        db.call_blocking(move |conn| unarr_core::commands::scheduler::ensure_defaults(conn, now))
            .unwrap();
        let seeded = db
            .call_blocking(|conn| store::providers::list_scheduled_tasks(conn))
            .unwrap();
        assert!(!seeded.is_empty());
        let report = import(Path::new(FIXTURE), &db).unwrap();
        let after = db
            .call_blocking(|conn| store::providers::list_scheduled_tasks(conn))
            .unwrap();
        assert_eq!(after.len(), report.counts["ScheduledTasks"]);
        // The original's rows, not the seeded ones.
        assert!(after.iter().all(|t| t.last_execution != Some(now)));
    }

    #[test]
    fn refuses_a_non_empty_target() {
        let db = Db::open_in_memory().unwrap();
        import(Path::new(FIXTURE), &db).unwrap();
        let err = import(Path::new(FIXTURE), &db).unwrap_err();
        assert!(err.to_string().contains("already holds data"), "{err}");
    }

    #[test]
    fn rejects_a_database_that_is_not_sonarr() {
        let dir = std::env::temp_dir().join(format!("unarr-import-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("other.db");
        Connection::open(&path)
            .unwrap()
            .execute_batch("CREATE TABLE t (x)")
            .unwrap();
        let db = Db::open_in_memory().unwrap();
        assert!(import(&path, &db).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
