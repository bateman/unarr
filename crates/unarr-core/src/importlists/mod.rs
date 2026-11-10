//! Import lists: the series a list names are added to the library, and
//! the library is checked against the lists (`ImportListSyncService`,
//! `FetchAndParseImportListService`, `ImportListStatusService`).
//!
//! Only Simkl's user watchlist is fetched. The other implementations keep
//! their settings and are skipped by the sync.

pub mod simkl;

use std::collections::HashSet;

use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};

use crate::db::Db;
use crate::indexers::status;
use crate::metadata::{MetadataProvider, Upstreams};
use crate::model::{NewItemMonitor, Provider, ProviderKind, SeriesType};
use crate::store;
use crate::store::import_lists::ListItem;
use crate::tv::series::{NewSeries, WriteError};
use crate::validation::ValidationFailure;

const KIND: ProviderKind = ProviderKind::ImportList;

/// `ImportListDefinition`: what a list does with the series it names.
#[derive(Debug, Clone, PartialEq)]
pub struct Definition {
    pub id: i64,
    pub name: String,
    pub implementation: String,
    pub enable_automatic_add: bool,
    pub search_for_missing_episodes: bool,
    /// A `MonitorTypes` name, as the API spells it.
    pub should_monitor: String,
    pub monitor_new_items: NewItemMonitor,
    pub root_folder_path: String,
    pub quality_profile_id: i64,
    pub series_type: SeriesType,
    pub season_folder: bool,
    pub tags: Vec<i64>,
    pub settings: Value,
}

impl Definition {
    pub fn from_provider(p: &Provider) -> Definition {
        let a = &p.attributes;
        let flag = |key: &str| a[key].as_bool().unwrap_or(false);
        let text = |key: &str| a[key].as_str().unwrap_or("");
        Definition {
            id: p.id,
            name: p.name.clone(),
            implementation: p.implementation.clone(),
            enable_automatic_add: flag("enableAutomaticAdd"),
            search_for_missing_episodes: flag("searchForMissingEpisodes"),
            should_monitor: text("shouldMonitor").to_string(),
            monitor_new_items: NewItemMonitor::parse(text("monitorNewItems")).unwrap_or(NewItemMonitor::All),
            root_folder_path: text("rootFolderPath").to_string(),
            quality_profile_id: a["qualityProfileId"].as_i64().unwrap_or(0),
            series_type: SeriesType::parse(text("seriesType")).unwrap_or(SeriesType::Standard),
            season_folder: flag("seasonFolder"),
            tags: p.tags.clone(),
            settings: p.settings.clone(),
        }
    }

    /// Whether this is a list the sync knows how to fetch.
    pub fn supported(&self) -> bool {
        self.implementation == simkl::IMPLEMENTATION
    }
}

/// `ListSyncLevelType`: what happens to a series no list names any more.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncLevel {
    Disabled,
    LogOnly,
    KeepAndUnmonitor,
    KeepAndTag,
}

impl SyncLevel {
    /// From the stored setting: the enum's name in any case, or its number.
    pub fn parse(s: &str) -> SyncLevel {
        match s.trim().to_ascii_lowercase().as_str() {
            "logonly" | "1" => SyncLevel::LogOnly,
            "keepandunmonitor" | "2" => SyncLevel::KeepAndUnmonitor,
            "keepandtag" | "3" => SyncLevel::KeepAndTag,
            _ => SyncLevel::Disabled,
        }
    }
}

/// What a sync changed in the library.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Outcome {
    /// The series added, in the order they were.
    pub added: Vec<i64>,
    /// The series the library clean unmonitored or tagged.
    pub updated: Vec<i64>,
}

/// The import lists' service: the database, and the way out to the lists.
#[derive(Debug, Clone)]
pub struct ImportLists {
    db: Db,
    client: reqwest::Client,
    upstreams: Upstreams,
}

impl ImportLists {
    pub fn new(db: Db, client: reqwest::Client, upstreams: Upstreams) -> ImportLists {
        ImportLists {
            db,
            client,
            upstreams,
        }
    }

    /// The settings' own rules, before anything is asked of the list.
    pub fn validate_settings(provider: &Provider) -> Vec<ValidationFailure> {
        if provider.implementation == simkl::IMPLEMENTATION {
            simkl::Settings::from_json(&provider.settings).validate()
        } else {
            Vec::new()
        }
    }

    /// `HttpImportListBase.TestConnection`: the list answers, with
    /// something. An implementation that is not fetched passes untested.
    pub async fn test(&self, provider: &Provider) -> Vec<ValidationFailure> {
        if provider.implementation != simkl::IMPLEMENTATION {
            tracing::warn!(
                "Import list {} ({}) is not supported and was not tested",
                provider.name,
                provider.implementation
            );
            return Vec::new();
        }
        let settings = simkl::Settings::from_json(&provider.settings);
        match simkl::fetch_items(&self.client, &self.upstreams, &settings).await {
            Ok(items) if items.is_empty() => vec![ValidationFailure::warning(
                "",
                "No results were returned from your import list, please check your settings and the log for details.",
            )],
            Ok(_) => Vec::new(),
            Err(failure) => {
                tracing::warn!("Unable to connect to import list: {}", failure.message);
                vec![ValidationFailure::error(
                    "",
                    &format!(
                        "Unable to connect to import list: {}. Check the log surrounding this error for details.",
                        failure.message
                    ),
                )]
            }
        }
    }

    /// `ImportListFactory.Test`: a stored list remembers how its test went.
    pub async fn record_test(&self, id: i64, valid: bool, now: DateTime<Utc>) -> anyhow::Result<()> {
        if id == 0 {
            return Ok(());
        }
        if valid {
            self.record_success(id).await
        } else {
            self.record_failure(id, now, Duration::zero(), true).await
        }
    }

    /// `RequestAction`: the sign-in's steps; anything else is `{}`.
    pub async fn action(
        &self,
        provider: &Provider,
        name: &str,
        query: &[(String, String)],
        now: DateTime<Utc>,
    ) -> Result<Value, String> {
        if provider.implementation != simkl::IMPLEMENTATION {
            return Ok(json!({}));
        }
        let settings = simkl::Settings::from_json(&provider.settings);
        simkl::action(&self.client, &self.upstreams, &settings, name, query, now)
            .await
            .map_err(|f| f.message)
    }

    async fn record_success(&self, id: i64) -> anyhow::Result<()> {
        self.db
            .call(move |conn| {
                let mut s = store::provider_status::provider_status(conn, KIND, id)?;
                if status::record_success(&mut s) {
                    store::provider_status::upsert_provider_status(conn, KIND, id, &s)?;
                }
                Ok(())
            })
            .await
    }

    async fn record_failure(
        &self,
        id: i64,
        now: DateTime<Utc>,
        minimum_back_off: Duration,
        escalate: bool,
    ) -> anyhow::Result<()> {
        self.db
            .call(move |conn| {
                let mut s = store::provider_status::provider_status(conn, KIND, id)?;
                // `ImportListStatusService` keeps the base policy: the
                // whole table, blocked from the first failure.
                let policy = status::Policy::indexer(Some(crate::time::process_start()));
                status::record_failure_with(&mut s, now, minimum_back_off, escalate, &policy);
                store::provider_status::upsert_provider_status(conn, KIND, id, &s)
            })
            .await
    }

    /// `SimklImportBase.Fetch`: the list's items, or `None` when it could
    /// not be asked (or had nothing new to say since the last sync).
    async fn fetch(&self, list: &Definition, now: DateTime<Utc>) -> anyhow::Result<Option<Vec<ListItem>>> {
        let mut settings = simkl::Settings::from_json(&list.settings);
        // Simkl's tokens do not expire today; the originals are ready for
        // the day they do.
        let expiring = settings.expires.is_none_or(|e| e < now + Duration::minutes(5));
        if !settings.refresh_token.trim().is_empty() && expiring {
            if let Some(renewed) = simkl::refresh_token(&self.client, &self.upstreams, &settings, now).await {
                settings = renewed;
                if list.id > 0 {
                    let (id, s) = (list.id, settings.clone());
                    self.db
                        .call(move |conn| {
                            if let Some(mut p) = store::providers::get_provider(conn, KIND, id)? {
                                p.settings["accessToken"] = json!(s.access_token);
                                p.settings["refreshToken"] = json!(s.refresh_token);
                                p.settings["expires"] = json!(s.expires.map(crate::time::format_utc));
                                store::providers::update_provider(conn, &p)?;
                            }
                            Ok(())
                        })
                        .await?;
                }
            }
        }
        let id = list.id;
        let last_fetch = self
            .db
            .call(move |conn| store::import_lists::sync_status(conn, id))
            .await?
            .last_info_sync;
        let last_activity = simkl::last_activity(&self.client, &self.upstreams, &settings, now).await;
        if last_fetch.is_some_and(|f| last_activity < f - Duration::hours(2)) {
            return Ok(None);
        }
        match simkl::fetch_items(&self.client, &self.upstreams, &settings).await {
            Ok(items) => {
                self.record_success(id).await?;
                // `CleanupListItems`.
                let mut seen = HashSet::new();
                Ok(Some(
                    items
                        .into_iter()
                        .filter(|i| seen.insert((i.title.clone(), i.tvdb_id, i.imdb_id.clone())))
                        .map(|i| ListItem {
                            import_list_id: id,
                            ..i
                        })
                        .collect(),
                ))
            }
            Err(failure) => {
                tracing::warn!(
                    "Unable to connect to import list {}: {}",
                    list.name,
                    failure.message
                );
                self.record_failure(id, now, failure.minimum_back_off, !failure.connection)
                    .await?;
                Ok(None)
            }
        }
    }

    /// Fetch one list and bring its stored items in line.
    async fn fetch_and_store(
        &self,
        list: &Definition,
        now: DateTime<Utc>,
        into: &mut Vec<ListItem>,
    ) -> anyhow::Result<()> {
        if !list.supported() {
            tracing::warn!(
                "Import list {} ({}) is not supported and was not synced",
                list.name,
                list.implementation
            );
            return Ok(());
        }
        let Some(items) = self.fetch(list, now).await? else {
            return Ok(());
        };
        let (id, stored) = (list.id, items.clone());
        self.db
            .call(move |conn| {
                let removed = store::import_lists::sync_items(conn, id, &stored)?;
                store::import_lists::record_sync(conn, id, now, removed > 0)
            })
            .await?;
        into.extend(items);
        Ok(())
    }

    async fn definitions(&self) -> anyhow::Result<Vec<Definition>> {
        self.db
            .call(|conn| {
                Ok(store::providers::list_providers(conn, KIND)?
                    .iter()
                    .map(Definition::from_provider)
                    .collect())
            })
            .await
    }

    /// `ImportListSyncService.Execute`: every list due, or the one named
    /// whatever its schedule; then the series they name that the library
    /// lacks, then the library against the lists.
    pub async fn sync(
        &self,
        provider: &dyn MetadataProvider,
        definition_id: Option<i64>,
        now: DateTime<Utc>,
        progress: &(dyn Fn(String) + Send + Sync),
    ) -> anyhow::Result<Outcome> {
        let lists = self.definitions().await?;
        let mut items = Vec::new();
        match definition_id {
            Some(id) => {
                let list = lists
                    .iter()
                    .find(|l| l.id == id)
                    .ok_or_else(|| anyhow::anyhow!("import list {id} does not exist"))?;
                progress(format!("Starting Import List Refresh for List {}", list.name));
                if list.enable_automatic_add {
                    self.fetch_and_store(list, now, &mut items).await?;
                }
            }
            None => {
                let statuses = self
                    .db
                    .call(|conn| store::provider_status::provider_statuses(conn, KIND))
                    .await?;
                let enabled: Vec<&Definition> = lists
                    .iter()
                    .filter(|l| l.enable_automatic_add)
                    .filter(|l| !statuses.iter().any(|(id, s)| *id == l.id && s.is_disabled(now)))
                    .collect();
                if enabled.is_empty() {
                    tracing::debug!("No import lists with automatic add enabled");
                    return Ok(Outcome::default());
                }
                progress("Starting Import List Sync".to_string());
                for list in enabled {
                    let id = list.id;
                    let last = self
                        .db
                        .call(move |conn| store::import_lists::sync_status(conn, id))
                        .await?
                        .last_info_sync;
                    if last.is_some_and(|l| now < l + simkl::MIN_REFRESH_INTERVAL) {
                        tracing::trace!(
                            "Skipping refresh of Import List {} due to minimum refresh interval",
                            list.name
                        );
                        continue;
                    }
                    self.fetch_and_store(list, now, &mut items).await?;
                }
            }
        }
        let mut seen = HashSet::new();
        items.retain(|i| seen.insert((i.tvdb_id, i.imdb_id.clone(), i.title.clone())));

        let mut outcome = Outcome {
            added: self.process(provider, &lists, items, now, progress).await?,
            updated: Vec::new(),
        };
        outcome.updated = self.try_clean_library().await?;
        Ok(outcome)
    }

    /// The first series the provider finds for `term`.
    async fn map(provider: &dyn MetadataProvider, term: String) -> anyhow::Result<Option<(i64, String)>> {
        let found = provider.search(&term).await.map_err(anyhow::Error::from)?;
        Ok(found.into_iter().next().map(|s| (s.tvdb_id, s.title)))
    }

    /// `ProcessListItems`: the items given a TVDB id, less the excluded
    /// and the ones the library has, added the way their list asks.
    async fn process(
        &self,
        provider: &dyn MetadataProvider,
        lists: &[Definition],
        items: Vec<ListItem>,
        now: DateTime<Utc>,
        progress: &(dyn Fn(String) + Send + Sync),
    ) -> anyhow::Result<Vec<i64>> {
        if items.is_empty() {
            progress("No list items to process".to_string());
            return Ok(Vec::new());
        }
        progress(format!("Processing {} list items", items.len()));
        let (excluded, existing) = self
            .db
            .call(|conn| {
                let excluded: HashSet<i64> = store::providers::list_import_list_exclusions(conn)?
                    .iter()
                    .map(|e| e.tvdb_id)
                    .collect();
                let existing: HashSet<i64> = store::series::list(conn)?.iter().map(|s| s.tvdb_id).collect();
                Ok((excluded, existing))
            })
            .await?;
        let found = items.len();
        let mut to_add: Vec<NewSeries> = Vec::new();
        for mut item in items {
            let Some(list) = lists.iter().find(|l| l.id == item.import_list_id) else {
                continue;
            };
            if !list.enable_automatic_add {
                continue;
            }
            let imdb = item.imdb_id.clone().unwrap_or_default();
            if item.tvdb_id <= 0 && !imdb.trim().is_empty() {
                if let Some((tvdb_id, title)) = Self::map(provider, format!("imdb:{imdb}")).await? {
                    (item.tvdb_id, item.title) = (tvdb_id, title);
                }
            }
            if item.tvdb_id <= 0 && item.tmdb_id > 0 {
                if let Some((tvdb_id, title)) = Self::map(provider, format!("tmdb:{}", item.tmdb_id)).await? {
                    (item.tvdb_id, item.title) = (tvdb_id, title);
                }
            }
            if item.tvdb_id <= 0 && item.ani_list_id > 0 {
                match Self::map(provider, format!("anilist:{}", item.ani_list_id)).await? {
                    Some((tvdb_id, title)) => (item.tvdb_id, item.title) = (tvdb_id, title),
                    None => {
                        tracing::debug!(
                            "[{}] Rejected, unable to find matching TVDB ID for Anilist ID: {}",
                            item.title,
                            item.ani_list_id
                        );
                        continue;
                    }
                }
            }
            if item.tvdb_id <= 0 && item.mal_id > 0 {
                match Self::map(provider, format!("mal:{}", item.mal_id)).await? {
                    Some((tvdb_id, title)) => (item.tvdb_id, item.title) = (tvdb_id, title),
                    None => {
                        tracing::debug!(
                            "[{}] Rejected, unable to find matching TVDB ID for MAL ID: {}",
                            item.title,
                            item.mal_id
                        );
                        continue;
                    }
                }
            }
            if item.tvdb_id <= 0 {
                tracing::debug!("[{}] Rejected, unable to find TVDB ID", item.title);
                continue;
            }
            if excluded.contains(&item.tvdb_id) {
                tracing::debug!("{} [{}] Rejected due to list exclusion", item.tvdb_id, item.title);
                continue;
            }
            if existing.contains(&item.tvdb_id) {
                tracing::debug!(
                    "{} [{}] Rejected, series exists in database",
                    item.tvdb_id,
                    item.title
                );
                continue;
            }
            if to_add.iter().any(|s| s.tvdb_id == item.tvdb_id) {
                continue;
            }
            let monitored = !list.should_monitor.eq_ignore_ascii_case("none");
            to_add.push(NewSeries {
                tvdb_id: item.tvdb_id,
                title: Some(item.title.clone()),
                path: None,
                root_folder_path: Some(list.root_folder_path.clone()),
                quality_profile_id: list.quality_profile_id,
                season_folder: list.season_folder,
                monitored,
                monitor_new_items: list.monitor_new_items,
                series_type: list.series_type,
                tags: list.tags.clone(),
                // Simkl names no seasons, so the list's preset applies.
                seasons: Vec::new(),
                add_options: Some(json!({
                    "ignoreEpisodesWithFiles": false,
                    "ignoreEpisodesWithoutFiles": false,
                    "monitor": list.should_monitor,
                    "searchForMissingEpisodes": list.search_for_missing_episodes,
                    "searchForCutoffUnmetEpisodes": false,
                })),
            });
        }
        // `AddSeriesService.AddSeries(list, ignoreErrors: true)`. The
        // originals insert the batch at once; here a series the provider
        // fails on mid-way leaves the earlier ones added.
        let candidates = to_add.len();
        let mut added = Vec::new();
        for new in to_add {
            let (tvdb_id, title) = (new.tvdb_id, new.title.clone().unwrap_or_default());
            match crate::tv::series::add(&self.db, provider, new, now).await {
                Ok(series) => added.push(series.id),
                Err(WriteError::Invalid(f) | WriteError::InvalidPlain(f)) => {
                    let why: Vec<&str> = f.iter().map(|f| f.error_message.as_str()).collect();
                    tracing::debug!(
                        "Series {title} with TVDB ID {tvdb_id} was not added due to validation failures: {}",
                        why.join(" ")
                    );
                }
                Err(WriteError::Other(e)) => return Err(e),
            }
        }
        progress(format!(
            "Import List Sync Completed. Items found: {found}, Series added: {candidates}"
        ));
        Ok(added)
    }

    /// `TryCleanLibrary`: once every list has synced and one of them lost
    /// an item, the series no list names are logged, unmonitored or
    /// tagged, as the setting says.
    /// Also what a deleted list asks for. Returns the series it changed.
    pub async fn try_clean_library(&self) -> anyhow::Result<Vec<i64>> {
        let lists = self.definitions().await?;
        let enabled: Vec<i64> = lists
            .iter()
            .filter(|l| l.enable_automatic_add)
            .map(|l| l.id)
            .collect();
        self.db
            .call(move |conn| {
                let level = SyncLevel::parse(&store::config::config(conn, "listsynclevel")?.unwrap_or_default());
                if level == SyncLevel::Disabled {
                    return Ok(Vec::new());
                }
                // `AllListsSuccessfulWithAPendingClean`.
                let failures = store::provider_status::provider_statuses(conn, KIND)?;
                let mut pending = false;
                for id in &enabled {
                    let sync = store::import_lists::sync_status(conn, *id)?;
                    let blocked = failures.iter().any(|(f, s)| f == id && s.disabled_till.is_some());
                    if blocked || sync.last_info_sync.is_none() {
                        return Ok(Vec::new());
                    }
                    pending |= sync.has_removed_item_since_last_clean;
                }
                if !pending {
                    return Ok(Vec::new());
                }
                let tag = store::config::config(conn, "listsynctag")?
                    .and_then(|t| t.trim().parse::<i64>().ok())
                    .unwrap_or(0);
                let items = store::import_lists::all_items(conn)?;
                let mut updated = Vec::new();
                for listed in store::series::list(conn)? {
                    if items.iter().any(|item| names(item, &listed)) {
                        continue;
                    }
                    // A series read from the list never goes back whole:
                    // the settings are written on a fresh read.
                    let Some(mut series) = store::series::get(conn, listed.id)? else {
                        continue;
                    };
                    match level {
                        SyncLevel::LogOnly => tracing::info!(
                            "{} was in your library, but not found in your lists --> You might want to unmonitor or remove it",
                            series.title
                        ),
                        SyncLevel::KeepAndUnmonitor if series.monitored => {
                            tracing::info!(
                                "{} was in your library, but not found in your lists --> Keeping in library but unmonitoring it",
                                series.title
                            );
                            series.monitored = false;
                            store::series::update_settings(conn, &series)?;
                            updated.push(series.id);
                        }
                        SyncLevel::KeepAndTag if !series.tags.contains(&tag) => {
                            tracing::info!(
                                "{} was in your library, but not found in your lists --> Keeping in library but tagging it",
                                series.title
                            );
                            series.tags.push(tag);
                            store::series::update_settings(conn, &series)?;
                            updated.push(series.id);
                        }
                        _ => {}
                    }
                }
                store::import_lists::mark_cleaned(conn)?;
                Ok(updated)
            })
            .await
    }
}

fn ids(v: &Option<Value>) -> Vec<i64> {
    v.as_ref()
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default()
}

/// Whether a list item names a library series, by any id. The originals
/// compare the ids as they are, so an item without an IMDb or a TMDB id
/// names every series that has none either.
fn names(item: &ListItem, series: &crate::model::Series) -> bool {
    item.tvdb_id == series.tvdb_id
        || item.imdb_id == series.imdb_id
        || item.tmdb_id == series.tmdb_id
        || ids(&series.mal_ids).contains(&item.mal_id)
        || ids(&series.anilist_ids).contains(&item.ani_list_id)
}
