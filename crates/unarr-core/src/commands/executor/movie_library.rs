//! Radarr's commands about the library itself: `RefreshMovie` and what
//! follows a movie's refresh (its covers, its rescan). The series side of
//! the same ground is in the parent module; the two share the cover code
//! and nothing else.

use chrono::{DateTime, Utc};

use super::{CollectionAction, CommandBody, CommandPriority, CommandTrigger, Event, Runtime};
use crate::health::CheckSet;
use crate::movies::commands::MovieSide;
use crate::movies::import::events::MovieImportEvent;
use crate::movies::metadata::{RadarrApi, SharedMovieProvider};
use crate::movies::model::Movie;
use crate::movies::refresh::{self, RefreshError, Refreshed};
use crate::movies::store;

/// How long a movie's refresh lets the refresh of the collection it just
/// brought run before it goes on.
const NEW_COLLECTION_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// A collection a movie's refresh brought: `CollectionAddedEvent`, whose
/// handler queues the collection's own refresh, and the wait for that
/// refresh before the movie's metadata row is written (see
/// `refresh::refresh_movie_info`).
struct NewCollection<'a> {
    runtime: &'a Runtime,
    /// The command that refreshes it.
    command: std::sync::Mutex<Option<i64>>,
}

impl refresh::CollectionEvents for NewCollection<'_> {
    fn added(&self, collection: crate::movies::model::Collection) -> crate::metadata::BoxFuture<'_, ()> {
        Box::pin(async move {
            let command = self.runtime.collection_created(collection.id).await;
            *self.command.lock().unwrap() = command;
        })
    }

    fn settled(&self) -> crate::metadata::BoxFuture<'_, ()> {
        Box::pin(async move {
            let command = *self.command.lock().unwrap();
            if let Some(command_id) = command {
                // The wait holds a worker. When every other worker is
                // already waiting like this one (a library import of
                // movies that each bring a collection), nobody is left
                // to run what they wait for: this one goes on, as the
                // original's two commands would have raced anyway.
                let Ok(_permit) = self.runtime.waiting_on_commands.try_acquire() else {
                    return;
                };
                // The refresh was queued behind whatever else waits (the
                // other movies of a bulk add): it goes first, or the
                // wait would only run out.
                self.runtime.queue.expedite(command_id);
                self.runtime
                    .wait_for_command(command_id, NEW_COLLECTION_WAIT)
                    .await;
            }
        })
    }
}

impl Runtime {
    /// Radarr's metadata server, asked through the application's own HTTP
    /// client and the upstreams the runtime was given.
    pub fn movie_provider(&self) -> SharedMovieProvider {
        let augment = self.augment();
        std::sync::Arc::new(RadarrApi::new(
            augment.client().clone(),
            augment.upstreams().clone(),
        ))
    }

    /// `RefreshMovieService.Execute`: the movies asked for, or every one
    /// that is due (all of them when the refresh is manual); each followed
    /// by its rescan.
    pub(super) async fn refresh_movies(
        &self,
        side: &MovieSide,
        id: i64,
        movie_ids: &[i64],
        is_new: bool,
        trigger: CommandTrigger,
        last_start_time: Option<DateTime<Utc>>,
    ) -> anyhow::Result<()> {
        let outcome = self
            .refresh_movies_inner(side, id, movie_ids, is_new, trigger, last_start_time)
            .await;
        // `MovieRefreshCompleteEvent`.
        self.check_health_later_with(CheckSet::MoviesRefreshed, None);
        outcome
    }

    async fn refresh_movies_inner(
        &self,
        side: &MovieSide,
        id: i64,
        movie_ids: &[i64],
        is_new: bool,
        trigger: CommandTrigger,
        last_start_time: Option<DateTime<Utc>>,
    ) -> anyhow::Result<()> {
        if !movie_ids.is_empty() {
            for &movie_id in movie_ids {
                match self.refresh_one_movie(id, movie_id).await {
                    Ok(_) => {
                        self.rescan_after_movie_refresh(side, id, movie_id, is_new, trigger)
                            .await
                    }
                    Err(e @ RefreshError::NotFound { .. }) => tracing::error!("{e}"),
                    Err(RefreshError::NoSuchMovie(_)) => {
                        anyhow::bail!("Movie with ID {movie_id} does not exist")
                    }
                    Err(RefreshError::Other(e)) => {
                        tracing::error!(error = %e, "Couldn't refresh info for movie {movie_id}");
                        self.rescan_after_movie_refresh(side, id, movie_id, is_new, trigger)
                            .await;
                        return Err(e);
                    }
                }
            }
            return Ok(());
        }

        let now = self.now();
        let manual = trigger == CommandTrigger::Manual;
        let movies = self.db.call(|conn| store::list_movies(conn)).await?;
        // The movies TMDB changed since the last scheduled run, when that
        // was within two weeks; an empty answer falls back to each movie's
        // own due date.
        let mut changed = std::collections::HashSet::new();
        if let Some(since) = last_start_time.filter(|at| *at + chrono::Duration::days(14) > now) {
            match self.movie_provider().changed_movies(since).await {
                Ok(ids) => changed = ids,
                Err(e) => anyhow::bail!("{e}"),
            }
        }
        for movie in movies {
            let due = (changed.is_empty() && refresh::should_refresh(&movie.metadata, now))
                || changed.contains(&movie.metadata.tmdb_id)
                || manual;
            if due {
                match self.refresh_one_movie(id, movie.id).await {
                    Ok(_) => {}
                    Err(e @ RefreshError::NotFound { .. }) => {
                        tracing::error!("{e}");
                        continue;
                    }
                    Err(e) => tracing::error!("Couldn't refresh info for {}: {e}", movie.metadata.title),
                }
            } else {
                tracing::debug!("Skipping refresh of movie: {}", movie.metadata.title);
            }
            self.rescan_after_movie_refresh(side, id, movie.id, false, trigger)
                .await;
        }
        Ok(())
    }

    /// `RefreshMovieInfo` and the handlers of what it publishes.
    async fn refresh_one_movie(&self, id: i64, movie_id: i64) -> Result<Refreshed, RefreshError> {
        let title = self
            .db
            .call(move |conn| Ok(store::get_movie(conn, movie_id)?.map(|m| m.metadata.title)))
            .await?;
        if let Some(title) = title {
            self.progress(id, format!("Updating info for {title}"));
        }
        let provider = self.movie_provider();
        let events = NewCollection {
            runtime: self,
            command: std::sync::Mutex::new(None),
        };
        match refresh::refresh_movie_info(&self.db, provider.as_ref(), movie_id, self.now(), &events).await {
            Ok(refreshed) => {
                self.movie_updated(&refreshed.movie).await;
                Ok(refreshed)
            }
            Err(RefreshError::NotFound {
                movie,
                marked_deleted,
            }) => {
                if marked_deleted {
                    self.movie_updated(&movie).await;
                }
                Err(RefreshError::NotFound {
                    movie,
                    marked_deleted,
                })
            }
            Err(e) => Err(e),
        }
    }

    /// `MovieCollectionAddedHandler`: a collection that just came in is
    /// refreshed, which brings the movies it names. Returns the command's
    /// id.
    async fn collection_added(&self, collection_id: i64) -> Option<i64> {
        match self
            .queue
            .push(
                CommandBody::RefreshCollections {
                    collection_ids: vec![collection_id],
                },
                CommandPriority::Normal,
                CommandTrigger::Unspecified,
                self.now(),
            )
            .await
        {
            Ok(command) => Some(command.id),
            Err(e) => {
                tracing::warn!(error = %e, "failed to queue the refresh of a new collection");
                None
            }
        }
    }

    /// Wait for a command to end, no longer than `limit`: every worker
    /// may be busy with a command that waits like this one.
    async fn wait_for_command(&self, command_id: i64, limit: std::time::Duration) {
        let ended = async {
            loop {
                match self.queue.get(command_id).await {
                    Ok(Some(command))
                        if matches!(
                            command.status,
                            super::CommandStatus::Queued | super::CommandStatus::Started
                        ) =>
                    {
                        tokio::time::sleep(std::time::Duration::from_millis(20)).await
                    }
                    _ => break,
                }
            }
        };
        let _ = tokio::time::timeout(limit, ended).await;
    }

    /// The handlers of `MovieUpdatedEvent`: the movie resource the UI
    /// holds, and the covers (the resource again when one was downloaded,
    /// `MediaCoversUpdatedEvent`).
    pub(super) async fn movie_updated(&self, movie: &Movie) {
        self.emit(Event::MovieUpdated { movie_id: movie.id });
        let root = self.media_cover_dir.read().unwrap().clone();
        if let Some(root) = root {
            let augment = self.augment();
            if crate::tv::covers::ensure_covers_of(
                &root,
                &augment,
                movie.id,
                &movie.metadata.title,
                &movie.metadata.images,
            )
            .await
            {
                self.emit(Event::MovieCoversUpdated { movie_id: movie.id });
            }
        }
    }

    /// `RescanMovie` after a refresh, as `RescanAfterRefresh` says; a new
    /// movie is always scanned.
    async fn rescan_after_movie_refresh(
        &self,
        side: &MovieSide,
        id: i64,
        movie_id: i64,
        is_new: bool,
        trigger: CommandTrigger,
    ) {
        use crate::store::config::RescanAfterRefresh;
        if !is_new {
            let setting = self
                .db
                .call(
                    |conn| Ok(crate::store::config::MediaManagementConfig::load(conn)?.rescan_after_refresh),
                )
                .await
                .unwrap_or_default();
            let skip = match setting {
                RescanAfterRefresh::Never => true,
                RescanAfterRefresh::AfterManual => trigger != CommandTrigger::Manual,
                RescanAfterRefresh::Always => false,
            };
            if skip {
                // `MovieScanSkippedEvent`.
                self.emit_movie_import_events(side, vec![MovieImportEvent::ScanSkipped { movie_id }])
                    .await;
                return;
            }
        }
        if let Err(e) = self.rescan_movie(side, id, Some(movie_id)).await {
            tracing::error!(error = %e, "Couldn't rescan movie {movie_id}");
        }
    }

    /// `MovieAddedHandler`: a movie that just came in is refreshed, as a
    /// new one, which scans its folder whatever the settings say.
    pub async fn movies_added(&self, movie_ids: &[i64]) {
        for &movie_id in movie_ids {
            if let Err(e) = self
                .queue
                .push(
                    CommandBody::RefreshMovie {
                        movie_ids: vec![movie_id],
                        is_new_movie: true,
                    },
                    CommandPriority::Normal,
                    CommandTrigger::Unspecified,
                    self.now(),
                )
                .await
            {
                tracing::warn!(error = %e, "failed to queue the refresh of a new movie");
            }
        }
    }

    /// `MovieScannedHandler`, for a scan and for a skipped one: what the
    /// options a movie was added with ask for, once.
    pub(super) async fn after_movie_scan(&self, movie_id: i64) {
        let acted = self
            .db
            .call(move |conn| {
                let Some(movie) = store::get_movie(conn, movie_id)? else {
                    return Ok(None);
                };
                let Some(options) = movie.add_options.clone() else {
                    return Ok(None);
                };
                tracing::info!(
                    "[{}] was recently added, performing post-add actions",
                    movie.metadata.title
                );
                let mut monitored_collection = None;
                if options["monitor"] == "movieAndCollection" && movie.metadata.collection_tmdb_id > 0 {
                    if let Some(mut collection) =
                        store::collections_by_tmdb_id(conn, movie.metadata.collection_tmdb_id)?.pop()
                    {
                        collection.monitored = true;
                        store::update_collection(conn, &collection)?;
                        monitored_collection = Some(collection.id);
                    }
                }
                store::clear_add_options(conn, movie_id)?;
                Ok(Some((
                    options["searchForMovie"].as_bool().unwrap_or(false),
                    monitored_collection,
                )))
            })
            .await;
        let (search, monitored_collection) = match acted {
            Ok(Some(acted)) => acted,
            Ok(None) => return,
            Err(e) => {
                tracing::warn!(error = %e, "the post-add actions of movie {movie_id} failed");
                return;
            }
        };
        if search {
            if let Err(e) = self
                .queue
                .push(
                    CommandBody::MoviesSearch {
                        movie_ids: vec![movie_id],
                    },
                    CommandPriority::Normal,
                    CommandTrigger::Unspecified,
                    self.now(),
                )
                .await
            {
                tracing::warn!(error = %e, "failed to queue the search of a new movie");
            }
        }
        if let Some(collection_id) = monitored_collection {
            self.collection_edited(collection_id);
        }
    }

    /// `CollectionEditedEvent`: the UI's collection, and the check of the
    /// collections' root folders.
    pub fn collection_edited(&self, collection_id: i64) {
        self.emit(Event::CollectionChanged {
            collection_id,
            action: CollectionAction::Updated,
        });
        self.check_health_later_with(CheckSet::Collections, None);
    }

    /// `CollectionAddedEvent`: the UI's list, and the refresh that brings
    /// the collection's movies.
    pub async fn collection_created(&self, collection_id: i64) -> Option<i64> {
        self.emit(Event::CollectionChanged {
            collection_id,
            action: CollectionAction::Created,
        });
        self.collection_added(collection_id).await
    }

    /// `CollectionDeletedEvent`.
    pub fn collection_deleted(&self, collection_id: i64) {
        self.emit(Event::CollectionChanged {
            collection_id,
            action: CollectionAction::Deleted,
        });
        self.check_health_later_with(CheckSet::Collections, None);
    }

    /// `RefreshCollectionService.Execute`: the collections asked for, or
    /// every one (those due, all of them when the refresh is manual), each
    /// followed by the movies a monitored collection brings.
    pub(super) async fn refresh_collections(
        &self,
        id: i64,
        collection_ids: &[i64],
        trigger: CommandTrigger,
    ) -> anyhow::Result<()> {
        use crate::movies::collections::{self, CollectionError};
        let now = self.now();
        let provider = self.movie_provider();
        let ctx = refresh::map_context(&self.db, now).await?;
        let targets: Vec<(i64, String, bool)> = if collection_ids.is_empty() {
            let manual = trigger == CommandTrigger::Manual;
            self.db
                .call(move |conn| collections::all_by_sort_title(conn, now, manual))
                .await?
                .into_iter()
                .map(|(c, due)| (c.id, c.title, due))
                .collect()
        } else {
            let ids = collection_ids.to_vec();
            self.db
                .call(move |conn| {
                    let mut out = Vec::new();
                    for id in ids {
                        let collection = store::get_collection(conn, id)?
                            .ok_or_else(|| anyhow::anyhow!("MovieCollection with ID {id} does not exist"))?;
                        out.push((collection.id, collection.title, true));
                    }
                    Ok(out)
                })
                .await?
        };
        let named = !collection_ids.is_empty();
        let mut outcome = Ok(());
        for (collection_id, title, due) in targets {
            if due {
                self.progress(id, format!("Updating info for {title}"));
                match collections::refresh_collection_info(&self.db, provider.as_ref(), &ctx, collection_id)
                    .await
                {
                    Ok(_) => self.emit(Event::CollectionChanged {
                        collection_id,
                        action: CollectionAction::Updated,
                    }),
                    Err(CollectionError::NotFound(collection)) => {
                        self.emit(Event::CollectionChanged {
                            collection_id,
                            action: CollectionAction::Deleted,
                        });
                        tracing::error!(
                            "Collection '{}' (TMDb {}) was not found, it may have been removed from The Movie Database.",
                            collection.title,
                            collection.tmdb_id
                        );
                        if named {
                            // A collection asked for by id lets the
                            // exception end the command.
                            outcome = Err(anyhow::anyhow!(
                                "Movie with tmdbId {} was not found, it may have been removed from TMDb.",
                                collection.tmdb_id
                            ));
                            break;
                        }
                        continue;
                    }
                    Err(CollectionError::NoSuchCollection(_)) => continue,
                    Err(CollectionError::Other(e)) => {
                        outcome = Err(e);
                        break;
                    }
                }
            }
            // `SyncCollectionMovies`.
            let to_add = self
                .db
                .call(move |conn| match store::get_collection(conn, collection_id)? {
                    Some(collection) => collections::movies_to_add(conn, &collection),
                    None => Ok(Vec::new()),
                })
                .await?;
            if !to_add.is_empty() {
                match crate::movies::library::add_many(&self.db, provider.as_ref(), &ctx, to_add, true, true)
                    .await
                {
                    Ok(added) => {
                        let ids: Vec<i64> = added.iter().map(|a| a.movie.id).collect();
                        self.movies_added(&ids).await;
                    }
                    Err(e) => tracing::error!("Adding the movies of {title} failed: {e:?}"),
                }
            }
        }
        // `CollectionRefreshCompleteEvent`.
        self.check_health_later_with(CheckSet::Collections, None);
        outcome
    }

    /// `MovieEditedEvent`: the UI's movie. A movie edited one by one is
    /// announced twice, by the event and by the controller itself.
    pub fn movie_edited(&self, movie_id: i64) {
        self.emit(Event::MovieUpdated { movie_id });
    }

    /// Queue the move of a movie's folder after its path was edited.
    pub async fn queue_movie_move(
        &self,
        movie_id: i64,
        source: String,
        destination: String,
    ) -> anyhow::Result<()> {
        self.queue
            .push(
                CommandBody::MoveMovie {
                    movie_id,
                    source_path: source,
                    destination_path: destination,
                },
                CommandPriority::Normal,
                CommandTrigger::Manual,
                self.now(),
            )
            .await?;
        Ok(())
    }

    /// Queue the editor's move of several movies to a root folder.
    pub async fn queue_bulk_movie_move(
        &self,
        root: String,
        movies: Vec<(i64, String)>,
    ) -> anyhow::Result<()> {
        self.queue
            .push(
                CommandBody::BulkMoveMovie {
                    destination_root_folder: root,
                    movies,
                },
                CommandPriority::Normal,
                CommandTrigger::Unspecified,
                self.now(),
            )
            .await?;
        Ok(())
    }

    /// `MoveMovieService.MoveSingleMovie`.
    pub(super) async fn move_movie(
        &self,
        id: i64,
        movie_id: i64,
        source: &str,
        destination: &str,
        position: Option<(usize, usize)>,
    ) -> anyhow::Result<()> {
        let title = self
            .db
            .call(move |conn| Ok(store::get_movie(conn, movie_id)?.map(|m| m.metadata.title)))
            .await?
            .ok_or_else(|| anyhow::anyhow!("Movie with ID {movie_id} does not exist"))?;
        if !source.starts_with('/') {
            tracing::warn!(
                "Folder '{source}' for '{title}' is invalid, unable to move movie. Try moving files manually"
            );
            return Ok(());
        }
        if !std::path::Path::new(source).is_dir() {
            tracing::debug!("Folder '{source}' for '{title}' does not exist, not moving.");
            return Ok(());
        }
        match position {
            Some((i, n)) => self.progress(
                id,
                format!(
                    "Moving {title} from '{source}' to '{destination}' ({}/{n})",
                    i + 1
                ),
            ),
            None => self.progress(id, format!("Moving {title} from '{source}' to '{destination}'")),
        }
        if crate::mediafiles::path_equals(source, destination) {
            self.progress(
                id,
                format!("{title} is already in the specified location '{destination}'."),
            );
            return Ok(());
        }
        let (src, dst) = (
            std::path::PathBuf::from(source),
            std::path::PathBuf::from(destination),
        );
        let moved =
            tokio::task::spawn_blocking(move || crate::mediafiles::transfer::move_folder(&src, &dst)).await?;
        match moved {
            Ok(()) => {
                self.progress(id, format!("{title} moved successfully to {destination}"));
                // `MovieMovedEvent`, and the checks tied to it.
                self.check_health_later_with(CheckSet::MoviesGone, None);
            }
            Err(e) => {
                tracing::error!(error = %e, "Unable to move movie from '{source}' to '{destination}'. Try moving files manually");
                // `RevertPath`, which is an edit of its own.
                let source = source.to_string();
                self.db
                    .call(move |conn| {
                        conn.execute(
                            "UPDATE movies SET path = ?2 WHERE id = ?1",
                            rusqlite::params![movie_id, source],
                        )?;
                        Ok(())
                    })
                    .await?;
                self.movie_edited(movie_id);
            }
        }
        Ok(())
    }

    /// `Execute(BulkMoveMovieCommand)`: each movie to the folder the
    /// naming gives it under the root folder.
    pub(super) async fn bulk_move_movies(
        &self,
        id: i64,
        root: &str,
        movies: &[(i64, String)],
    ) -> anyhow::Result<()> {
        self.progress(id, format!("Moving {} movies to '{root}'", movies.len()));
        for (index, (movie_id, source)) in movies.iter().enumerate() {
            let movie_id = *movie_id;
            let folder = self
                .db
                .call(move |conn| {
                    let movie = store::get_movie(conn, movie_id)?
                        .ok_or_else(|| anyhow::anyhow!("Movie with ID {movie_id} does not exist"))?;
                    Ok(crate::movies::naming::movie_folder(
                        &crate::movies::naming::NamingMovie {
                            metadata: &movie.metadata,
                            translations: &store::translation_titles(conn, movie.metadata.id)?,
                        },
                        &store::naming(conn)?,
                    ))
                })
                .await?;
            let destination = format!("{}/{folder}", root.trim_end_matches('/'));
            self.move_movie(id, movie_id, source, &destination, Some((index, movies.len())))
                .await?;
        }
        self.progress(id, format!("Finished moving {} movies to '{root}'", movies.len()));
        Ok(())
    }

    /// The handlers of `MoviesDeletedEvent` outside the database: the
    /// downloads that were for the movies, their folders when asked for,
    /// their covers, the UI, the health checks.
    pub async fn movies_deleted(&self, deleted: crate::movies::library::Deleted, delete_files: bool) {
        let now = self.now();
        if let Some(side) = &self.movies {
            side.downloads.mark_dirty();
            match side.downloads.refresh_if_dirty(now).await {
                Ok(true) => self.emit(Event::QueueUpdated),
                Ok(false) => {}
                Err(e) => tracing::error!(error = %e, "Couldn't update the tracked downloads"),
            }
        }
        if delete_files {
            let (db, list) = (self.db.clone(), deleted.movies.clone());
            let _ = tokio::task::spawn_blocking(move || {
                let config =
                    db.call_blocking(|conn| crate::store::config::MediaManagementConfig::load(conn))?;
                let remaining = db.call_blocking(|conn| store::all_movie_paths(conn))?;
                for movie in &list {
                    crate::movies::library::delete_movie_folder(&config, movie, &remaining);
                }
                anyhow::Ok(())
            })
            .await;
        }
        let root = self.media_cover_dir.read().unwrap().clone();
        for movie in &deleted.movies {
            self.emit(Event::MovieDeleted { movie_id: movie.id });
            if let Some(root) = &root {
                crate::tv::covers::delete_covers(root, movie.id).await;
            }
        }
        for collection_id in &deleted.collections {
            self.emit(Event::CollectionChanged {
                collection_id: *collection_id,
                action: CollectionAction::Deleted,
            });
        }
        if delete_files {
            // `DeleteCompletedEvent`.
            self.process_notification_queue(now).await;
        }
        if !deleted.movies.is_empty() {
            self.check_health_later_with(CheckSet::MoviesGone, None);
            if deleted
                .movies
                .iter()
                .any(|m| m.metadata.status == crate::movies::model::MovieStatus::Deleted)
            {
                self.check_health_later_with(CheckSet::MoviesRefreshed, None);
            }
        }
        if !deleted.collections.is_empty() {
            self.check_health_later_with(CheckSet::Collections, None);
        }
    }
}
