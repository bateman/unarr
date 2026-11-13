//! Radarr's import lists: the movies a list names are added to the
//! library, and the library is checked against the lists
//! (`ImportListSyncService`, `FetchAndParseImportListService`,
//! `ImportListStatusService`).
//!
//! Only the IMDb lists are fetched (`IMDbListImport`, the watchlist the
//! maintainer syncs). The other implementations keep their settings and
//! are skipped by the sync.

use std::collections::HashSet;

use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};

use crate::db::Db;
use crate::indexers::status;
use crate::metadata::BoxFuture;
use crate::model::{Provider, ProviderKind};
use crate::movies::library::{self, NewMovie, WriteError};
use crate::movies::metadata::{
    CollectionInfo, MapContext, MovieInfo, MovieMetadataError, MovieMetadataProvider,
};
use crate::movies::model::MovieStatus;
use crate::movies::store as movies;
use crate::store;
use crate::validation::ValidationFailure;

const KIND: ProviderKind = ProviderKind::ImportList;

/// The one implementation that is fetched.
pub const IMPLEMENTATION: &str = "IMDbListImport";

/// `IMDbListImport.MinRefreshInterval`.
pub const MIN_REFRESH_INTERVAL: Duration = Duration::hours(12);

/// `ImportListDefinition` as Radarr's resource carries it.
#[derive(Debug, Clone, PartialEq)]
pub struct Definition {
    pub id: i64,
    pub name: String,
    pub implementation: String,
    /// Whether the list is fetched at all (`Enable`).
    pub enabled: bool,
    /// Whether what it names is added to the library (`EnableAuto`).
    pub enable_auto: bool,
    /// A `MonitorTypes` name, as the API spells it.
    pub monitor: String,
    pub search_on_add: bool,
    pub root_folder_path: String,
    pub quality_profile_id: i64,
    pub minimum_availability: MovieStatus,
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
            enabled: flag("enabled"),
            enable_auto: flag("enableAuto"),
            monitor: text("monitor").to_string(),
            search_on_add: flag("searchOnAdd"),
            root_folder_path: text("rootFolderPath").to_string(),
            quality_profile_id: a["qualityProfileId"].as_i64().unwrap_or(0),
            minimum_availability: MovieStatus::parse(text("minimumAvailability"))
                .unwrap_or(MovieStatus::Released),
            tags: p.tags.clone(),
            settings: p.settings.clone(),
        }
    }

    /// Whether this is a list the sync knows how to fetch.
    pub fn supported(&self) -> bool {
        self.implementation == IMPLEMENTATION
    }
}

/// `IMDbListSettings`.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub list_id: String,
}

impl Settings {
    pub fn from_json(settings: &Value) -> Settings {
        Settings {
            list_id: settings["listId"].as_str().unwrap_or("").trim().to_string(),
        }
    }

    /// `IMDbSettingsValidator`: a list id of one of the three shapes, the
    /// first failure only (the rules cascade).
    pub fn validate(&self) -> Vec<ValidationFailure> {
        if self.list_id.is_empty() {
            return vec![ValidationFailure::setting_not_empty(
                "ListId",
                json!(self.list_id),
            )];
        }
        let user_list = self
            .list_id
            .strip_prefix("ur")
            .or_else(|| self.list_id.strip_prefix("UR"))
            .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()));
        if !user_list && !["top250", "popular"].contains(&self.list_id.to_lowercase().as_str()) {
            return vec![ValidationFailure::setting(
                "ListId",
                "List ID must be 'top250', 'popular' or an IMDb user watchlist of the form 'ur12345678'",
                json!(self.list_id),
            )];
        }
        Vec::new()
    }
}

/// What a sync changed in the library.
#[derive(Debug, Clone, Default)]
pub struct Outcome {
    /// The movies added, in the order they were.
    pub added: Vec<i64>,
    /// The movies the library clean unmonitored.
    pub updated: Vec<i64>,
    /// The movies it removed with the collections they left empty, and
    /// whether their files are to go with them.
    pub removed: Option<(library::Deleted, bool)>,
}

/// `ListSyncLevelType`: what happens to a movie no list names any more.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncLevel {
    Disabled,
    LogOnly,
    KeepAndUnmonitor,
    RemoveAndKeep,
    RemoveAndDelete,
}

impl SyncLevel {
    pub fn parse(s: &str) -> SyncLevel {
        match s.trim().to_ascii_lowercase().as_str() {
            "logonly" => SyncLevel::LogOnly,
            "keepandunmonitor" => SyncLevel::KeepAndUnmonitor,
            "removeandkeep" => SyncLevel::RemoveAndKeep,
            "removeanddelete" => SyncLevel::RemoveAndDelete,
            _ => SyncLevel::Disabled,
        }
    }
}

/// A provider that remembers the movies it was asked for during one
/// sync. The original asks twice for every movie a list adds, once to map
/// the list's item and once to add it; the second answer is the first.
struct Remembering<'p> {
    inner: &'p dyn MovieMetadataProvider,
    movies: std::sync::Mutex<std::collections::HashMap<i64, MovieInfo>>,
}

impl<'p> Remembering<'p> {
    fn new(inner: &'p dyn MovieMetadataProvider) -> Remembering<'p> {
        Remembering {
            inner,
            movies: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }
}

impl MovieMetadataProvider for Remembering<'_> {
    fn movie_info<'a>(
        &'a self,
        tmdb_id: i64,
        ctx: &'a MapContext,
    ) -> BoxFuture<'a, Result<MovieInfo, MovieMetadataError>> {
        Box::pin(async move {
            if let Some(known) = self.movies.lock().unwrap().get(&tmdb_id).cloned() {
                return Ok(known);
            }
            let info = self.inner.movie_info(tmdb_id, ctx).await?;
            self.movies.lock().unwrap().insert(tmdb_id, info.clone());
            Ok(info)
        })
    }

    fn movie_by_imdb_id<'a>(
        &'a self,
        imdb_id: &'a str,
        ctx: &'a MapContext,
    ) -> BoxFuture<'a, Result<Option<MovieInfo>, MovieMetadataError>> {
        self.inner.movie_by_imdb_id(imdb_id, ctx)
    }

    fn collection_info<'a>(
        &'a self,
        tmdb_id: i64,
        ctx: &'a MapContext,
    ) -> BoxFuture<'a, Result<CollectionInfo, MovieMetadataError>> {
        self.inner.collection_info(tmdb_id, ctx)
    }

    fn search<'a>(
        &'a self,
        q: &'a str,
        year: &'a str,
        ctx: &'a MapContext,
    ) -> BoxFuture<'a, Result<Vec<MovieInfo>, MovieMetadataError>> {
        self.inner.search(q, year, ctx)
    }

    fn changed_movies(
        &self,
        since: DateTime<Utc>,
    ) -> BoxFuture<'_, Result<std::collections::HashSet<i64>, MovieMetadataError>> {
        self.inner.changed_movies(since)
    }

    fn list<'a>(
        &'a self,
        route: &'a str,
        ctx: &'a MapContext,
    ) -> BoxFuture<'a, Result<Vec<MovieInfo>, MovieMetadataError>> {
        self.inner.list(route, ctx)
    }
}

/// Why a list could not be read.
pub struct FetchFailure {
    pub message: String,
    /// A transport error, which the originals do not escalate.
    pub connection: bool,
}

/// The import lists' service for movies: the database, and the way out.
#[derive(Debug, Clone)]
pub struct MovieImportLists {
    db: Db,
}

impl MovieImportLists {
    pub fn new(db: Db) -> MovieImportLists {
        MovieImportLists { db }
    }

    /// The settings' own rules, before anything is asked of the list.
    pub fn validate_settings(provider: &Provider) -> Vec<ValidationFailure> {
        if provider.implementation == IMPLEMENTATION {
            Settings::from_json(&provider.settings).validate()
        } else {
            Vec::new()
        }
    }

    /// `HttpImportListBase.TestConnection`: the list answers, with
    /// something. An implementation that is not fetched passes untested.
    pub async fn test(
        &self,
        provider: &dyn MovieMetadataProvider,
        ctx: &MapContext,
        definition: &Provider,
    ) -> Vec<ValidationFailure> {
        if definition.implementation != IMPLEMENTATION {
            tracing::warn!(
                "Import list {} ({}) is not supported and was not tested",
                definition.name,
                definition.implementation
            );
            return Vec::new();
        }
        let settings = Settings::from_json(&definition.settings);
        match fetch_items(provider, ctx, &settings).await {
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
            self.record_failure(id, now, true).await
        }
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

    async fn record_failure(&self, id: i64, now: DateTime<Utc>, escalate: bool) -> anyhow::Result<()> {
        self.db
            .call(move |conn| {
                let mut s = store::provider_status::provider_status(conn, KIND, id)?;
                let policy = status::Policy::indexer(Some(crate::time::process_start()));
                status::record_failure_with(&mut s, now, Duration::zero(), escalate, &policy);
                store::provider_status::upsert_provider_status(conn, KIND, id, &s)
            })
            .await
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

    /// One list fetched and its stored movies brought in line
    /// (`SyncMoviesForList`). Returns the metadata rows it named.
    async fn fetch_and_store(
        &self,
        provider: &dyn MovieMetadataProvider,
        ctx: &MapContext,
        list: &Definition,
        now: DateTime<Utc>,
    ) -> anyhow::Result<Option<Vec<i64>>> {
        if !list.supported() {
            tracing::warn!(
                "Import list {} ({}) is not supported and was not synced",
                list.name,
                list.implementation
            );
            return Ok(None);
        }
        let settings = Settings::from_json(&list.settings);
        let tmdb_ids = match fetch_items(provider, ctx, &settings).await {
            Ok(items) => {
                self.record_success(list.id).await?;
                items
            }
            Err(failure) => {
                tracing::warn!(
                    "Unable to connect to import list {}: {}",
                    list.name,
                    failure.message
                );
                self.record_failure(list.id, now, !failure.connection).await?;
                return Ok(None);
            }
        };
        tracing::debug!(
            "Found {} from Import List {} ({})",
            tmdb_ids.len(),
            IMPLEMENTATION,
            list.name
        );
        // `MapMovieReports`: the metadata row the library already has, or
        // the one the provider is asked for and that is written down.
        let mut metadata_ids = Vec::new();
        for tmdb_id in tmdb_ids {
            let known = self
                .db
                .call(move |conn| movies::metadata_by_tmdb_id(conn, tmdb_id))
                .await?;
            let id = match known {
                Some(row) => row.id,
                None => match provider.movie_info(tmdb_id, ctx).await {
                    Ok(info) => {
                        let mut metadata = info.metadata;
                        self.db
                            .call(move |conn| movies::upsert_metadata(conn, &mut metadata))
                            .await?
                    }
                    Err(MovieMetadataError::MovieNotFound(_)) => continue,
                    Err(e) => {
                        tracing::warn!("Error during Import List Sync of {}: {e}", list.name);
                        continue;
                    }
                },
            };
            if !metadata_ids.contains(&id) {
                metadata_ids.push(id);
            }
        }
        let (id, rows) = (list.id, metadata_ids.clone());
        self.db
            .call(move |conn| movies::sync_movies_for_list(conn, id, &rows))
            .await?;
        self.db
            .call(move |conn| store::import_lists::record_sync(conn, id, now, false))
            .await?;
        Ok(Some(metadata_ids))
    }

    /// `ImportListSyncService.Execute`: the list named whatever its
    /// schedule, or every enabled list that is due; then the movies they
    /// name that the library lacks. A clean of the library comes first,
    /// as the original runs it before it processes the items.
    pub async fn sync(
        &self,
        provider: &dyn MovieMetadataProvider,
        definition_id: Option<i64>,
        now: DateTime<Utc>,
        progress: &(dyn Fn(String) + Send + Sync),
    ) -> anyhow::Result<Outcome> {
        let ctx = crate::movies::refresh::map_context(&self.db, now).await?;
        let remembering = Remembering::new(provider);
        let provider: &dyn MovieMetadataProvider = &remembering;
        let lists = self.definitions().await?;
        let mut named: Vec<(i64, Vec<i64>)> = Vec::new();
        match definition_id {
            Some(id) => {
                let list = lists
                    .iter()
                    .find(|l| l.id == id)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "NzbDrone.Core.Datastore.ModelNotFoundException: ImportListDefinition with ID {id} does not exist"
                        )
                    })?;
                progress(format!("Starting Import List Refresh for List {}", list.name));
                if !list.enabled {
                    tracing::debug!(
                        "Import List {} ({}) is not enabled, skipping.",
                        IMPLEMENTATION,
                        list.name
                    );
                    return Ok(Outcome::default());
                }
                if let Some(ids) = self.fetch_and_store(provider, &ctx, list, now).await? {
                    named.push((list.id, ids));
                }
            }
            None => {
                let enabled: Vec<&Definition> = lists.iter().filter(|l| l.enabled).collect();
                if enabled.is_empty() {
                    tracing::debug!("No enabled import lists, skipping sync and cleaning");
                    return Ok(Outcome::default());
                }
                let statuses = self
                    .db
                    .call(|conn| store::provider_status::provider_statuses(conn, KIND))
                    .await?;
                let mut synced = 0;
                let mut any_failure = false;
                for list in enabled {
                    let id = list.id;
                    let last = self
                        .db
                        .call(move |conn| store::import_lists::sync_status(conn, id))
                        .await?
                        .last_info_sync;
                    if last.is_some_and(|l| now < l + MIN_REFRESH_INTERVAL) {
                        tracing::trace!(
                            "Skipping refresh of Import List {} ({}) due to minimum refresh interval",
                            IMPLEMENTATION,
                            list.name
                        );
                        continue;
                    }
                    if statuses.iter().any(|(sid, s)| *sid == id && s.is_disabled(now)) {
                        tracing::debug!(
                            "Temporarily ignoring Import List {} ({}) due to recent failures.",
                            IMPLEMENTATION,
                            list.name
                        );
                        any_failure = true;
                        continue;
                    }
                    progress(format!(
                        "Syncing Movies for Import List {IMPLEMENTATION} ({})",
                        list.name
                    ));
                    match self.fetch_and_store(provider, &ctx, list, now).await? {
                        Some(ids) => named.push((id, ids)),
                        None => any_failure = true,
                    }
                    synced += 1;
                }
                if synced == 0 {
                    return Ok(Outcome::default());
                }
                if !any_failure {
                    let cleaned = self.clean_library().await?;
                    let mut outcome = cleaned;
                    outcome.added = self.process(provider, &ctx, &lists, named, now, progress).await?;
                    return Ok(outcome);
                }
            }
        }
        Ok(Outcome {
            added: self.process(provider, &ctx, &lists, named, now, progress).await?,
            ..Outcome::default()
        })
    }

    /// `ProcessListItems`: the movies the lists named, less the excluded
    /// and the ones the library has, added the way their list asks.
    async fn process(
        &self,
        provider: &dyn MovieMetadataProvider,
        ctx: &MapContext,
        lists: &[Definition],
        named: Vec<(i64, Vec<i64>)>,
        now: DateTime<Utc>,
        progress: &(dyn Fn(String) + Send + Sync),
    ) -> anyhow::Result<Vec<i64>> {
        let (excluded, existing) = self
            .db
            .call(|conn| {
                let excluded: HashSet<i64> = movies::list_import_exclusions(conn)?
                    .iter()
                    .map(|e| e.tmdb_id)
                    .collect();
                Ok((excluded, movies::all_movie_tmdb_ids(conn)?))
            })
            .await?;
        let mut to_add: Vec<NewMovie> = Vec::new();
        for (list_id, metadata_ids) in named {
            let Some(list) = lists.iter().find(|l| l.id == list_id) else {
                continue;
            };
            if !list.enable_auto {
                continue;
            }
            for metadata_id in metadata_ids {
                let Some(row) = self
                    .db
                    .call(move |conn| movies::get_metadata(conn, metadata_id))
                    .await?
                else {
                    continue;
                };
                let (tmdb_id, title) = (row.tmdb_id, row.title.clone());
                if existing.contains(&tmdb_id) {
                    tracing::debug!("{tmdb_id} [{title}] Rejected, Movie Exists in DB");
                    continue;
                }
                if excluded.contains(&tmdb_id) {
                    tracing::debug!("{tmdb_id} [{title}] Rejected due to list exclusion");
                    continue;
                }
                if to_add.iter().any(|m| m.tmdb_id == tmdb_id) {
                    continue;
                }
                let monitored = !list.monitor.eq_ignore_ascii_case("none");
                to_add.push(NewMovie {
                    tmdb_id,
                    title: Some(title),
                    path: None,
                    root_folder_path: Some(list.root_folder_path.clone()),
                    quality_profile_id: list.quality_profile_id,
                    monitored,
                    minimum_availability: list.minimum_availability,
                    tags: list.tags.clone(),
                    add_options: Some(json!({
                        "searchForMovie": monitored && list.search_on_add,
                        "monitor": list.monitor,
                        "addMethod": "list",
                    })),
                });
            }
        }
        if to_add.is_empty() {
            return Ok(Vec::new());
        }
        progress(format!(
            "Adding {} movies from your auto enabled lists to library",
            to_add.len()
        ));
        // `AddMovieService.AddMovies(movies, ignoreErrors: true)`.
        let added = match library::add_many(&self.db, provider, ctx, to_add, true).await {
            Ok(added) => added,
            Err(WriteError::Other(e)) => return Err(e),
            Err(_) => Vec::new(),
        };
        let _ = now;
        Ok(added.iter().map(|a| a.movie.id).collect())
    }

    /// `CleanLibrary`: the movies no enabled list names are logged,
    /// unmonitored or removed, as the setting says.
    pub async fn clean_library(&self) -> anyhow::Result<Outcome> {
        let enabled: Vec<i64> = self
            .definitions()
            .await?
            .iter()
            .filter(|l| l.enabled)
            .map(|l| l.id)
            .collect();
        self.db
            .call(move |conn| {
                let level =
                    SyncLevel::parse(&store::config::config(conn, "listsynclevel")?.unwrap_or_default());
                if level == SyncLevel::Disabled {
                    return Ok(Outcome::default());
                }
                let listed = movies::list_movies_for(conn, &enabled)?;
                let mut outcome = Outcome::default();
                let mut to_remove = Vec::new();
                for mut movie in movies::list_movies(conn)? {
                    let named = listed.iter().any(|l| {
                        let m = &l.metadata;
                        m.tmdb_id == movie.metadata.tmdb_id
                            || match (&m.imdb_id, &movie.metadata.imdb_id) {
                                (Some(a), Some(b)) => !a.trim().is_empty() && a == b,
                                _ => false,
                            }
                    });
                    if named {
                        continue;
                    }
                    let title = movie.metadata.title.clone();
                    match level {
                        SyncLevel::LogOnly => tracing::info!(
                            "{title} was in your library, but not found in your lists --> You might want to unmonitor or remove it"
                        ),
                        SyncLevel::KeepAndUnmonitor if movie.monitored => {
                            tracing::info!(
                                "{title} was in your library, but not found in your lists --> Keeping in library but Unmonitoring it"
                            );
                            movie.monitored = false;
                            movies::update_movie(conn, &movie)?;
                            outcome.updated.push(movie.id);
                        }
                        SyncLevel::RemoveAndKeep | SyncLevel::RemoveAndDelete => {
                            let files = level == SyncLevel::RemoveAndDelete;
                            tracing::info!(
                                "{title} was in your library, but not found in your lists --> Removing from library{}",
                                if files { " and deleting files" } else { " (keeping files)" }
                            );
                            to_remove.push(movie.id);
                        }
                        _ => {}
                    }
                }
                if !to_remove.is_empty() {
                    // `MovieService.DeleteMovie`, as the API's own delete
                    // does it: the rows, and the collections left empty.
                    if let Ok(deleted) = library::delete(conn, &to_remove, false)? {
                        outcome.removed = Some((deleted, level == SyncLevel::RemoveAndDelete));
                    }
                }
                Ok(outcome)
            })
            .await
    }
}

/// `IMDbListRequestGenerator` and `IMDbListParser`: the TMDB ids the list
/// names. The `ls12345678` lists the original dropped raise its message.
pub async fn fetch_items(
    provider: &dyn MovieMetadataProvider,
    ctx: &MapContext,
    settings: &Settings,
) -> Result<Vec<i64>, FetchFailure> {
    if settings.list_id.to_lowercase().starts_with("ls") {
        return Err(FetchFailure {
            message: "IMDb lists of the form 'ls12345678' are no longer supported. Feel free to remove this list after you review your Clean Library Level.".into(),
            connection: false,
        });
    }
    tracing::info!("IMDb List {}: Importing movies", settings.list_id);
    let route = format!("list/imdb/{}", settings.list_id);
    match provider.list(&route, ctx).await {
        Ok(items) => Ok(items.into_iter().map(|i| i.metadata.tmdb_id).collect()),
        // `HttpImportListBase.FetchReleases`: only a host that does not
        // resolve or connect is a connection failure; an error status, a
        // timeout or an answer that does not parse is the list's own.
        Err(MovieMetadataError::Unreachable(message)) => Err(FetchFailure {
            message,
            connection: true,
        }),
        Err(e) => Err(FetchFailure {
            message: e.to_string(),
            connection: false,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A provider that fails a list request the way it is told to and
    /// counts the movies it is asked for.
    struct Failing {
        error: fn() -> MovieMetadataError,
        asked: std::sync::atomic::AtomicUsize,
    }

    impl MovieMetadataProvider for Failing {
        fn movie_info<'a>(
            &'a self,
            tmdb_id: i64,
            _: &'a MapContext,
        ) -> BoxFuture<'a, Result<MovieInfo, MovieMetadataError>> {
            self.asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Box::pin(async move { Err(MovieMetadataError::MovieNotFound(tmdb_id.to_string())) })
        }
        fn movie_by_imdb_id<'a>(
            &'a self,
            _: &'a str,
            _: &'a MapContext,
        ) -> BoxFuture<'a, Result<Option<MovieInfo>, MovieMetadataError>> {
            Box::pin(async { Ok(None) })
        }
        fn collection_info<'a>(
            &'a self,
            tmdb_id: i64,
            _: &'a MapContext,
        ) -> BoxFuture<'a, Result<CollectionInfo, MovieMetadataError>> {
            Box::pin(async move { Err(MovieMetadataError::MovieNotFound(tmdb_id.to_string())) })
        }
        fn search<'a>(
            &'a self,
            _: &'a str,
            _: &'a str,
            _: &'a MapContext,
        ) -> BoxFuture<'a, Result<Vec<MovieInfo>, MovieMetadataError>> {
            Box::pin(async { Ok(Vec::new()) })
        }
        fn changed_movies(
            &self,
            _: DateTime<Utc>,
        ) -> BoxFuture<'_, Result<std::collections::HashSet<i64>, MovieMetadataError>> {
            Box::pin(async { Ok(std::collections::HashSet::new()) })
        }
        fn list<'a>(
            &'a self,
            _: &'a str,
            _: &'a MapContext,
        ) -> BoxFuture<'a, Result<Vec<MovieInfo>, MovieMetadataError>> {
            let error = (self.error)();
            Box::pin(async move { Err(error) })
        }
    }

    fn failing(error: fn() -> MovieMetadataError) -> Failing {
        Failing {
            error,
            asked: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    #[tokio::test]
    async fn only_a_host_that_cannot_be_reached_is_a_connection_failure() {
        let ctx = MapContext::new(Utc::now(), "US");
        let settings = Settings {
            list_id: "ur0000001".into(),
        };
        // `RecordConnectionFailure`: the back-off does not escalate.
        let unreachable = failing(|| MovieMetadataError::Unreachable("no such host".into()));
        let failure = fetch_items(&unreachable, &ctx, &settings).await.err().unwrap();
        assert!(failure.connection);
        // `RecordFailure`: an error status and an answer that does not
        // parse are the list's own.
        for error in [
            (|| MovieMetadataError::Unavailable("[NotFound]".into())) as fn() -> MovieMetadataError,
            || MovieMetadataError::Invalid("not JSON".into()),
        ] {
            let failure = fetch_items(&failing(error), &ctx, &settings).await.err().unwrap();
            assert!(!failure.connection, "{}", failure.message);
        }
    }

    #[tokio::test]
    async fn an_answer_that_failed_is_asked_for_again() {
        let ctx = MapContext::new(Utc::now(), "US");
        let inner = failing(|| MovieMetadataError::Invalid(String::new()));
        let remembering = Remembering::new(&inner);
        // A failure is not remembered: the movie is asked for again.
        assert!(remembering.movie_info(603, &ctx).await.is_err());
        assert!(remembering.movie_info(603, &ctx).await.is_err());
        assert_eq!(inner.asked.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[test]
    fn the_list_id_is_one_of_three_shapes() {
        let valid = |id: &str| Settings { list_id: id.into() }.validate().is_empty();
        assert!(valid("ur12345678") && valid("top250") && valid("popular"));
        assert!(!valid("") && !valid("ls12345678") && !valid("ur") && !valid("nope"));
    }
}
