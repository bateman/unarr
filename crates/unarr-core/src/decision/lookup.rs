//! What the engine reads: a catalog loaded once per batch (profiles,
//! definitions, formats, delay and release profiles, indexers, blocked
//! indexers, settings) and a lookup for the per-release queries (series,
//! episodes, files, history, blocklist, scene mappings).

use std::cell::RefCell;
use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rusqlite::Connection;

use crate::customformat::FormatSet;
use crate::indexers::settings::NewznabSettings;
use crate::model::{
    BlocklistEntry, DelayProfile, Episode, EpisodeFile, HistoryEntry, Provider, ProviderKind,
    QualityDefinition, QualityProfile, ReleaseProfile, SceneMapping, Series,
};
use crate::store;
use crate::store::config::IndexerConfig;
use crate::store::provider_status::ProviderStatus;

/// `ProperDownloadTypes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProperDownloadTypes {
    #[default]
    PreferAndUpgrade,
    DoNotUpgrade,
    DoNotPrefer,
}

impl ProperDownloadTypes {
    pub fn parse(s: &str) -> ProperDownloadTypes {
        match s.trim().to_ascii_lowercase().as_str() {
            "donotupgrade" => ProperDownloadTypes::DoNotUpgrade,
            "donotprefer" => ProperDownloadTypes::DoNotPrefer,
            _ => ProperDownloadTypes::PreferAndUpgrade,
        }
    }
}

/// The settings the specifications consult, with the originals' defaults.
#[derive(Debug, Clone, PartialEq)]
pub struct EngineConfig {
    pub download_propers_and_repacks: ProperDownloadTypes,
    pub enable_completed_download_handling: bool,
    pub auto_unmonitor_previously_downloaded_episodes: bool,
    pub skip_free_space_check_when_importing: bool,
    pub minimum_free_space_when_importing_mb: i64,
    pub indexer: IndexerConfig,
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig {
            download_propers_and_repacks: ProperDownloadTypes::PreferAndUpgrade,
            enable_completed_download_handling: true,
            auto_unmonitor_previously_downloaded_episodes: false,
            skip_free_space_check_when_importing: false,
            minimum_free_space_when_importing_mb: 100,
            indexer: IndexerConfig::default(),
        }
    }
}

impl EngineConfig {
    pub fn load(conn: &Connection) -> anyhow::Result<EngineConfig> {
        let defaults = EngineConfig::default();
        let text = |key: &str| store::config::config(conn, key);
        let flag = |key: &str, default: bool| -> anyhow::Result<bool> {
            Ok(text(key)?
                .map(|v| v.eq_ignore_ascii_case("true"))
                .unwrap_or(default))
        };
        Ok(EngineConfig {
            download_propers_and_repacks: text("DownloadPropersAndRepacks")?
                .map(|v| ProperDownloadTypes::parse(&v))
                .unwrap_or(defaults.download_propers_and_repacks),
            enable_completed_download_handling: flag(
                "EnableCompletedDownloadHandling",
                defaults.enable_completed_download_handling,
            )?,
            auto_unmonitor_previously_downloaded_episodes: flag(
                "AutoUnmonitorPreviouslyDownloadedEpisodes",
                defaults.auto_unmonitor_previously_downloaded_episodes,
            )?,
            skip_free_space_check_when_importing: flag(
                "SkipFreeSpaceCheckWhenImporting",
                defaults.skip_free_space_check_when_importing,
            )?,
            minimum_free_space_when_importing_mb: text("MinimumFreeSpaceWhenImporting")?
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(defaults.minimum_free_space_when_importing_mb),
            indexer: store::config::indexer_config(conn)?,
        })
    }
}

/// What the specifications need to know about an indexer, whatever its
/// implementation.
#[derive(Debug, Clone, PartialEq)]
pub struct IndexerInfo {
    pub id: i64,
    pub name: String,
    pub tags: Vec<i64>,
    /// Torrent indexers only.
    pub minimum_seeders: Option<i64>,
    pub multi_languages: Vec<i64>,
    pub seed_ratio: Option<f64>,
    pub seed_time: Option<i64>,
    pub season_pack_seed_time: Option<i64>,
}

impl IndexerInfo {
    pub fn from_provider(p: &Provider) -> IndexerInfo {
        let torrent = matches!(
            p.implementation.as_str(),
            "Torznab"
                | "TorrentRssIndexer"
                | "BroadcastheNet"
                | "FileList"
                | "HDBits"
                | "IPTorrents"
                | "Nyaa"
                | "TorrentPotato"
        );
        let settings = NewznabSettings::from_value(&p.settings);
        IndexerInfo {
            id: p.id,
            name: p.name.clone(),
            tags: p.tags.clone(),
            minimum_seeders: torrent.then_some(settings.minimum_seeders),
            multi_languages: settings.multi_languages,
            seed_ratio: settings.seed_criteria.seed_ratio,
            seed_time: settings.seed_criteria.seed_time,
            season_pack_seed_time: settings.seed_criteria.season_pack_seed_time,
        }
    }
}

/// Where a search reads the queue from, without depending on the downloads
/// service itself.
pub type QueueSource = std::sync::Arc<dyn Fn() -> Vec<QueuedRelease> + Send + Sync>;

/// A release in the download queue, as `QueueSpecification` sees it; the
/// queue arrives with the download clients.
#[derive(Debug, Clone, PartialEq)]
pub struct QueuedRelease {
    pub series_id: i64,
    pub episode_ids: Vec<i64>,
    pub title: String,
    pub quality: crate::model::QualityModel,
    pub languages: Vec<i64>,
    pub size: i64,
    pub indexer_flags: i64,
    pub release_type: crate::model::ReleaseType,
    /// `TrackedDownloadState.FailedPending`: skipped by the queue check.
    pub failed_pending: bool,
}

/// Loaded once per batch.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    pub config: EngineConfig,
    pub now: DateTime<Utc>,
    pub profiles: Vec<QualityProfile>,
    pub definitions: Vec<QualityDefinition>,
    pub formats: FormatSet,
    pub delay_profiles: Vec<DelayProfile>,
    pub release_profiles: Vec<ReleaseProfile>,
    pub indexers: Vec<IndexerInfo>,
    /// Indexers in back-off, by id.
    pub blocked_indexers: HashMap<i64, ProviderStatus>,
    pub queue: Vec<QueuedRelease>,
}

impl Catalog {
    pub fn load(conn: &Connection, now: DateTime<Utc>) -> anyhow::Result<Catalog> {
        let blocked = store::provider_status::provider_statuses(conn, ProviderKind::Indexer)?
            .into_iter()
            .filter(|(_, s)| s.is_disabled(now))
            .collect();
        Ok(Catalog {
            config: EngineConfig::load(conn)?,
            now,
            profiles: store::catalog::list_quality_profiles(conn)?,
            definitions: store::catalog::list_quality_definitions(conn)?,
            formats: store::catalog::list_custom_formats(conn)?.into(),
            delay_profiles: store::providers::list_delay_profiles(conn)?,
            release_profiles: store::providers::list_release_profiles(conn)?,
            indexers: store::providers::list_providers(conn, ProviderKind::Indexer)?
                .iter()
                .map(IndexerInfo::from_provider)
                .collect(),
            blocked_indexers: blocked,
            queue: Vec::new(),
        })
    }

    pub fn profile(&self, id: i64) -> Option<&QualityProfile> {
        self.profiles.iter().find(|p| p.id == id)
    }

    /// `IQualityDefinitionService.Get(quality)`: the stored definition, or the
    /// defaults when none was stored.
    pub fn definition(&self, quality_id: i64) -> QualityDefinition {
        self.definitions
            .iter()
            .find(|d| d.quality_id == quality_id)
            .cloned()
            .unwrap_or_else(|| {
                let d = crate::quality::SONARR_QUALITIES
                    .iter()
                    .find(|d| d.quality.id == quality_id);
                QualityDefinition {
                    id: 0,
                    quality_id,
                    title: d.map(|d| d.quality.name.to_string()).unwrap_or_default(),
                    min_size: d.map(|d| d.min_size),
                    max_size: d.and_then(|d| d.max_size),
                    preferred_size: d.map(|d| d.preferred_size),
                }
            })
    }

    /// With the queue the `QueueSpecification` reads: the caller's downloads
    /// service holds it, the catalog is loaded from a connection.
    pub fn with_queue(mut self, queue: Vec<QueuedRelease>) -> Catalog {
        self.queue = queue;
        self
    }

    /// `DelayProfileService.BestForTags`: the first profile, in order, that
    /// shares a tag with the series or has none.
    pub fn delay_profile_for(&self, tags: &[i64]) -> Option<&DelayProfile> {
        let mut candidates: Vec<&DelayProfile> = self
            .delay_profiles
            .iter()
            .filter(|d| d.tags.is_empty() || d.tags.iter().any(|t| tags.contains(t)))
            .collect();
        candidates.sort_by_key(|d| d.order);
        candidates.first().copied()
    }

    /// `ReleaseProfileService.EnabledForTags`.
    pub fn release_profiles_for(&self, tags: &[i64], indexer_id: i64) -> Vec<&ReleaseProfile> {
        self.release_profiles
            .iter()
            .filter(|r| r.tags.is_empty() || r.tags.iter().any(|t| tags.contains(t)))
            .filter(|r| r.enabled)
            .filter(|r| r.indexer_id == indexer_id || r.indexer_id == 0)
            .collect()
    }

    pub fn indexer(&self, id: i64) -> Option<&IndexerInfo> {
        self.indexers.iter().find(|i| i.id == id)
    }

    pub fn indexer_by_name(&self, name: &str) -> Option<&IndexerInfo> {
        self.indexers.iter().find(|i| i.name.eq_ignore_ascii_case(name))
    }
}

/// The per-release queries.
pub trait Lookup {
    fn series_by_id(&self, id: i64) -> Option<Series>;
    /// `SeriesRepository.FindByTitle`: every series with the clean title.
    fn series_by_clean_title(&self, clean_title: &str) -> Vec<Series>;
    fn series_by_clean_title_and_year(&self, clean_title: &str, year: i64) -> Vec<Series>;
    /// `FindByTitleInexact`: series whose clean title the given one contains.
    fn series_inexact(&self, clean_title: &str) -> Vec<Series>;
    fn series_by_tvdb_id(&self, tvdb_id: i64) -> Option<Series>;
    fn series_by_tv_rage_id(&self, tv_rage_id: i64) -> Option<Series>;
    fn series_by_imdb_id(&self, imdb_id: &str) -> Option<Series>;
    fn episodes_by_season(&self, series_id: i64, season: i64) -> Vec<Episode>;
    fn episodes_by_scene_season(&self, series_id: i64, scene_season: i64) -> Vec<Episode>;
    fn episode_by_number(&self, series_id: i64, season: i64, number: i64) -> Option<Episode>;
    fn episode_by_absolute(&self, series_id: i64, absolute: i64) -> Option<Episode>;
    fn episodes_by_scene_numbering(
        &self,
        series_id: i64,
        scene_season: i64,
        scene_episode: i64,
    ) -> Vec<Episode>;
    fn episodes_by_scene_absolute(&self, series_id: i64, scene_absolute: i64) -> Vec<Episode>;
    fn episodes_by_air_date(&self, series_id: i64, air_date: &str) -> Vec<Episode>;
    fn episodes_by_file(&self, file_id: i64) -> Vec<Episode>;
    fn episode_file(&self, id: i64) -> Option<EpisodeFile>;
    fn scene_mappings_by_parse_term(&self, parse_term: &str) -> Vec<SceneMapping>;
    /// Newest first.
    fn history_for_episode(&self, episode_id: i64) -> Vec<HistoryEntry>;
    fn blocklist_for_series(&self, series_id: i64) -> Vec<BlocklistEntry>;
    /// `PendingReleaseService.OldestPendingRelease`: the age in minutes of the
    /// oldest pending release for these episodes. Pending releases arrive
    /// with the grab pipeline.
    fn oldest_pending_release_age_minutes(
        &self,
        _series_id: i64,
        _episode_ids: &[i64],
        _now: DateTime<Utc>,
    ) -> Option<f64> {
        None
    }
    /// Free bytes at a path, `None` when the path is unknown.
    fn free_space(&self, path: &str) -> Option<i64>;
    fn file_exists(&self, path: &str) -> bool;
}

/// The lookup over a database connection. Series are cached for the batch:
/// a release does not change what a series is.
pub struct StoreLookup<'c> {
    conn: &'c Connection,
    series_cache: RefCell<HashMap<i64, Option<Series>>>,
}

impl<'c> StoreLookup<'c> {
    pub fn new(conn: &'c Connection) -> StoreLookup<'c> {
        StoreLookup {
            conn,
            series_cache: RefCell::new(HashMap::new()),
        }
    }

    fn remember(&self, series: Vec<Series>) -> Vec<Series> {
        let mut cache = self.series_cache.borrow_mut();
        for s in &series {
            cache.insert(s.id, Some(s.clone()));
        }
        series
    }

    fn one(&self, result: anyhow::Result<Option<Series>>) -> Option<Series> {
        match result {
            Ok(s) => s.map(|s| self.remember(vec![s]).pop().unwrap()),
            Err(e) => {
                tracing::error!(error = %e, "series lookup failed");
                None
            }
        }
    }

    fn many(&self, result: anyhow::Result<Vec<Series>>) -> Vec<Series> {
        match result {
            Ok(s) => self.remember(s),
            Err(e) => {
                tracing::error!(error = %e, "series lookup failed");
                Vec::new()
            }
        }
    }
}

fn logged<T: Default>(what: &str, result: anyhow::Result<T>) -> T {
    match result {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, "{what} lookup failed");
            T::default()
        }
    }
}

impl Lookup for StoreLookup<'_> {
    fn series_by_id(&self, id: i64) -> Option<Series> {
        if let Some(cached) = self.series_cache.borrow().get(&id) {
            return cached.clone();
        }
        let found = self.one(store::series::get(self.conn, id));
        self.series_cache.borrow_mut().entry(id).or_insert(found.clone());
        found
    }

    fn series_by_clean_title(&self, clean_title: &str) -> Vec<Series> {
        self.many(store::series::find_by_clean_title(self.conn, clean_title))
    }

    fn series_by_clean_title_and_year(&self, clean_title: &str, year: i64) -> Vec<Series> {
        self.many(store::series::find_by_clean_title_and_year(
            self.conn,
            clean_title,
            year,
        ))
    }

    fn series_inexact(&self, clean_title: &str) -> Vec<Series> {
        self.many(store::series::find_by_clean_title_inexact(self.conn, clean_title))
    }

    fn series_by_tvdb_id(&self, tvdb_id: i64) -> Option<Series> {
        self.one(store::series::find_by_tvdb_id(self.conn, tvdb_id))
    }

    fn series_by_tv_rage_id(&self, tv_rage_id: i64) -> Option<Series> {
        self.one(store::series::find_by_tv_rage_id(self.conn, tv_rage_id))
    }

    fn series_by_imdb_id(&self, imdb_id: &str) -> Option<Series> {
        self.one(store::series::find_by_imdb_id(self.conn, imdb_id))
    }

    fn episodes_by_season(&self, series_id: i64, season: i64) -> Vec<Episode> {
        logged(
            "episodes",
            store::episodes::list_episodes(self.conn, series_id, Some(season)),
        )
    }

    fn episodes_by_scene_season(&self, series_id: i64, scene_season: i64) -> Vec<Episode> {
        logged(
            "episodes",
            store::episodes::list_episodes_by_scene_season(self.conn, series_id, scene_season),
        )
    }

    fn episode_by_number(&self, series_id: i64, season: i64, number: i64) -> Option<Episode> {
        logged(
            "episode",
            store::episodes::find_episode(self.conn, series_id, season, number),
        )
    }

    fn episode_by_absolute(&self, series_id: i64, absolute: i64) -> Option<Episode> {
        logged(
            "episode",
            store::episodes::find_episode_by_absolute(self.conn, series_id, absolute),
        )
    }

    fn episodes_by_scene_numbering(
        &self,
        series_id: i64,
        scene_season: i64,
        scene_episode: i64,
    ) -> Vec<Episode> {
        logged(
            "episodes",
            store::episodes::find_episodes_by_scene_numbering(
                self.conn,
                series_id,
                scene_season,
                scene_episode,
            ),
        )
    }

    fn episodes_by_scene_absolute(&self, series_id: i64, scene_absolute: i64) -> Vec<Episode> {
        logged(
            "episodes",
            store::episodes::find_episodes_by_scene_absolute(self.conn, series_id, scene_absolute),
        )
    }

    fn episodes_by_air_date(&self, series_id: i64, air_date: &str) -> Vec<Episode> {
        logged(
            "episodes",
            store::episodes::list_episodes_by_air_date(self.conn, series_id, air_date),
        )
    }

    fn episodes_by_file(&self, file_id: i64) -> Vec<Episode> {
        logged(
            "episodes",
            store::episodes::list_episodes_by_file(self.conn, file_id),
        )
    }

    fn episode_file(&self, id: i64) -> Option<EpisodeFile> {
        logged("episode file", store::episodes::get_file(self.conn, id))
    }

    fn scene_mappings_by_parse_term(&self, parse_term: &str) -> Vec<SceneMapping> {
        logged(
            "scene mappings",
            store::scene::by_parse_term(self.conn, parse_term),
        )
    }

    fn history_for_episode(&self, episode_id: i64) -> Vec<HistoryEntry> {
        logged("history", store::history::for_episode(self.conn, episode_id))
    }

    fn blocklist_for_series(&self, series_id: i64) -> Vec<BlocklistEntry> {
        logged(
            "blocklist",
            store::history::blocklist_for_series(self.conn, series_id),
        )
    }

    fn oldest_pending_release_age_minutes(
        &self,
        series_id: i64,
        episode_ids: &[i64],
        now: DateTime<Utc>,
    ) -> Option<f64> {
        let rows = logged(
            "pending releases",
            store::pending::all_by_series(self.conn, series_id),
        );
        if rows.is_empty() {
            return None;
        }
        crate::downloads::pending::oldest_pending_release_age_hours(self, rows, episode_ids, now)
            .map(|hours| hours * 60.0)
    }

    fn free_space(&self, path: &str) -> Option<i64> {
        crate::fs::free_space(std::path::Path::new(path))
    }

    fn file_exists(&self, path: &str) -> bool {
        std::path::Path::new(path).exists()
    }
}
