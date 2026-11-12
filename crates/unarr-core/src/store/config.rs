//! Naming configuration, key/value settings and import metadata.

use rusqlite::{params, Connection, OptionalExtension};

use crate::model::NamingConfig;

pub fn set_naming(conn: &Connection, n: &NamingConfig) -> anyhow::Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO naming_config (id, rename_episodes, replace_illegal_characters,
            colon_replacement_format, custom_colon_replacement_format, multi_episode_style,
            standard_episode_format, daily_episode_format, anime_episode_format, series_folder_format,
            season_folder_format, specials_folder_format)
         VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            n.rename_episodes,
            n.replace_illegal_characters,
            n.colon_replacement_format,
            n.custom_colon_replacement_format,
            n.multi_episode_style,
            n.standard_episode_format,
            n.daily_episode_format,
            n.anime_episode_format,
            n.series_folder_format,
            n.season_folder_format,
            n.specials_folder_format
        ],
    )?;
    Ok(())
}

/// The naming configuration, or Sonarr's defaults when none was stored.
pub fn naming(conn: &Connection) -> anyhow::Result<NamingConfig> {
    let found = conn
        .query_row("SELECT * FROM naming_config WHERE id = 1", [], |r| {
            Ok(NamingConfig {
                rename_episodes: r.get("rename_episodes")?,
                replace_illegal_characters: r.get("replace_illegal_characters")?,
                colon_replacement_format: r.get("colon_replacement_format")?,
                custom_colon_replacement_format: r.get("custom_colon_replacement_format")?,
                multi_episode_style: r.get("multi_episode_style")?,
                standard_episode_format: r.get("standard_episode_format")?,
                daily_episode_format: r.get("daily_episode_format")?,
                anime_episode_format: r.get("anime_episode_format")?,
                series_folder_format: r.get("series_folder_format")?,
                season_folder_format: r.get("season_folder_format")?,
                specials_folder_format: r.get("specials_folder_format")?,
            })
        })
        .optional()?;
    Ok(found.unwrap_or_default())
}

/// Store a setting. Keys are lower-cased, as the originals do.
pub fn set_config(conn: &Connection, key: &str, value: &str) -> anyhow::Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO config (key, value) VALUES (?1, ?2)",
        params![key.to_ascii_lowercase(), value],
    )?;
    Ok(())
}

pub fn config(conn: &Connection, key: &str) -> anyhow::Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT value FROM config WHERE key = ?1",
            params![key.to_ascii_lowercase()],
            |r| r.get(0),
        )
        .optional()?)
}

/// Every setting as (key, value), for building the config resources.
pub fn all_config(conn: &Connection) -> anyhow::Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare("SELECT key, value FROM config ORDER BY key")?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// The settings `config/indexer` serves, with the originals' defaults
/// (`IndexerConfigResource`, `ConfigService`).
#[derive(Debug, Clone, PartialEq)]
pub struct IndexerConfig {
    /// Minutes a usenet release must be old before it is grabbed.
    pub minimum_age: i64,
    /// Days a usenet release may be old before it is skipped; 0 is unlimited.
    pub retention: i64,
    /// Megabytes a release may weigh; 0 is unlimited.
    pub maximum_size: i64,
    /// Minutes between RSS syncs; 0 disables the sync.
    pub rss_sync_interval: i64,
}

impl Default for IndexerConfig {
    fn default() -> Self {
        IndexerConfig {
            minimum_age: 0,
            retention: 0,
            maximum_size: 0,
            rss_sync_interval: 15,
        }
    }
}

pub fn indexer_config(conn: &Connection) -> anyhow::Result<IndexerConfig> {
    let int = |key: &str, default: i64| -> anyhow::Result<i64> {
        Ok(config(conn, key)?
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(default))
    };
    let defaults = IndexerConfig::default();
    Ok(IndexerConfig {
        minimum_age: int("MinimumAge", defaults.minimum_age)?,
        retention: int("Retention", defaults.retention)?,
        maximum_size: int("MaximumSize", defaults.maximum_size)?,
        rss_sync_interval: int("RssSyncInterval", defaults.rss_sync_interval)?,
    })
}

pub fn set_meta(conn: &Connection, key: &str, value: &str) -> anyhow::Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
        params![key, value],
    )?;
    Ok(())
}

pub fn meta(conn: &Connection, key: &str) -> anyhow::Result<Option<String>> {
    Ok(conn
        .query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| {
            r.get(0)
        })
        .optional()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    #[test]
    fn naming_defaults_then_overrides() {
        let db = Db::open_in_memory().unwrap();
        db.call_blocking(|conn| {
            assert_eq!(naming(conn)?, NamingConfig::default());
            let custom = NamingConfig {
                rename_episodes: true,
                ..NamingConfig::default()
            };
            set_naming(conn, &custom)?;
            set_naming(conn, &custom)?;
            assert_eq!(naming(conn)?, custom);
            set_config(conn, "EnableCompletedDownloadHandling", "True")?;
            assert_eq!(
                config(conn, "enablecompleteddownloadhandling")?.as_deref(),
                Some("True")
            );
            assert_eq!(config(conn, "missing")?, None);
            assert_eq!(all_config(conn)?.len(), 1);
            set_meta(conn, "source", "sonarr")?;
            assert_eq!(meta(conn, "source")?.as_deref(), Some("sonarr"));
            Ok(())
        })
        .unwrap();
    }
}

/// `EpisodeTitleRequiredType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EpisodeTitleRequired {
    #[default]
    Always,
    BulkSeasonReleases,
    Never,
}

/// `FileDateType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FileDateType {
    #[default]
    None,
    LocalAirDate,
    UtcAirDate,
    /// Radarr's `Cinemas`.
    Cinemas,
    /// Radarr's `Release`.
    Release,
}

/// `RescanAfterRefreshType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RescanAfterRefresh {
    #[default]
    Always,
    AfterManual,
    Never,
}

/// The media management settings the import reads (`ConfigService`), with
/// the originals' defaults.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaManagementConfig {
    pub recycle_bin: String,
    pub recycle_bin_cleanup_days: i64,
    pub create_empty_series_folders: bool,
    pub delete_empty_folders: bool,
    pub file_date: FileDateType,
    pub rescan_after_refresh: RescanAfterRefresh,
    pub set_permissions_linux: bool,
    pub chmod_folder: String,
    pub chown_group: String,
    pub episode_title_required: EpisodeTitleRequired,
    pub skip_free_space_check_when_importing: bool,
    pub minimum_free_space_when_importing_mb: i64,
    pub copy_using_hardlinks: bool,
    pub import_extra_files: bool,
    pub extra_file_extensions: String,
    pub enable_media_info: bool,
    pub auto_unmonitor_previously_downloaded_episodes: bool,
    pub enable_completed_download_handling: bool,
    /// `DownloadClientWorkingFolders`, `|`-separated prefixes.
    pub download_client_working_folders: String,
}

impl Default for MediaManagementConfig {
    fn default() -> Self {
        MediaManagementConfig {
            recycle_bin: String::new(),
            recycle_bin_cleanup_days: 7,
            create_empty_series_folders: false,
            delete_empty_folders: false,
            file_date: FileDateType::None,
            rescan_after_refresh: RescanAfterRefresh::Always,
            set_permissions_linux: false,
            chmod_folder: "755".into(),
            chown_group: String::new(),
            episode_title_required: EpisodeTitleRequired::Always,
            skip_free_space_check_when_importing: false,
            minimum_free_space_when_importing_mb: 100,
            copy_using_hardlinks: true,
            import_extra_files: false,
            extra_file_extensions: "srt".into(),
            enable_media_info: true,
            auto_unmonitor_previously_downloaded_episodes: false,
            enable_completed_download_handling: true,
            download_client_working_folders: "_UNPACK_|_FAILED_".into(),
        }
    }
}

impl MediaManagementConfig {
    pub fn load(conn: &Connection) -> anyhow::Result<MediaManagementConfig> {
        let d = MediaManagementConfig::default();
        let text = |key: &str| config(conn, key);
        let flag = |key: &str, default: bool| -> anyhow::Result<bool> {
            Ok(text(key)?
                .map(|v| v.eq_ignore_ascii_case("true"))
                .unwrap_or(default))
        };
        let string = |key: &str, default: &str| -> anyhow::Result<String> {
            Ok(text(key)?.unwrap_or_else(|| default.to_string()))
        };
        let int = |key: &str, default: i64| -> anyhow::Result<i64> {
            Ok(text(key)?.and_then(|v| v.trim().parse().ok()).unwrap_or(default))
        };
        Ok(MediaManagementConfig {
            recycle_bin: string("RecycleBin", &d.recycle_bin)?,
            recycle_bin_cleanup_days: int("RecycleBinCleanupDays", d.recycle_bin_cleanup_days)?,
            create_empty_series_folders: flag("CreateEmptySeriesFolders", d.create_empty_series_folders)?,
            delete_empty_folders: flag("DeleteEmptyFolders", d.delete_empty_folders)?,
            file_date: match text("FileDate")?
                .as_deref()
                .map(str::to_ascii_lowercase)
                .as_deref()
            {
                Some("localairdate") => FileDateType::LocalAirDate,
                Some("utcairdate") => FileDateType::UtcAirDate,
                Some("cinemas") => FileDateType::Cinemas,
                Some("release") => FileDateType::Release,
                _ => FileDateType::None,
            },
            rescan_after_refresh: match text("RescanAfterRefresh")?
                .as_deref()
                .map(str::to_ascii_lowercase)
                .as_deref()
            {
                Some("aftermanual") => RescanAfterRefresh::AfterManual,
                Some("never") => RescanAfterRefresh::Never,
                _ => RescanAfterRefresh::Always,
            },
            set_permissions_linux: flag("SetPermissionsLinux", d.set_permissions_linux)?,
            chmod_folder: string("ChmodFolder", &d.chmod_folder)?,
            chown_group: string("ChownGroup", &d.chown_group)?,
            episode_title_required: match text("EpisodeTitleRequired")?
                .as_deref()
                .map(str::to_ascii_lowercase)
                .as_deref()
            {
                Some("bulkseasonreleases") => EpisodeTitleRequired::BulkSeasonReleases,
                Some("never") => EpisodeTitleRequired::Never,
                _ => EpisodeTitleRequired::Always,
            },
            skip_free_space_check_when_importing: flag(
                "SkipFreeSpaceCheckWhenImporting",
                d.skip_free_space_check_when_importing,
            )?,
            minimum_free_space_when_importing_mb: int(
                "MinimumFreeSpaceWhenImporting",
                d.minimum_free_space_when_importing_mb,
            )?,
            copy_using_hardlinks: flag("CopyUsingHardlinks", d.copy_using_hardlinks)?,
            import_extra_files: flag("ImportExtraFiles", d.import_extra_files)?,
            extra_file_extensions: string("ExtraFileExtensions", &d.extra_file_extensions)?,
            enable_media_info: flag("EnableMediaInfo", d.enable_media_info)?,
            auto_unmonitor_previously_downloaded_episodes: flag(
                "AutoUnmonitorPreviouslyDownloadedEpisodes",
                d.auto_unmonitor_previously_downloaded_episodes,
            )?,
            enable_completed_download_handling: flag(
                "EnableCompletedDownloadHandling",
                d.enable_completed_download_handling,
            )?,
            download_client_working_folders: string(
                "DownloadClientWorkingFolders",
                &d.download_client_working_folders,
            )?,
        })
    }
}
