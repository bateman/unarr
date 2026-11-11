//! What Radarr's commands do, after its `RssSyncService` and
//! `MoviesSearchService`. The command queue, its workers and the scheduler
//! are `crate::commands`, shared with Sonarr; they call in here for the
//! commands whose work is about movies.

use std::sync::Arc;

use chrono::{DateTime, Utc};

use crate::db::Db;
use crate::indexers::IndexerService;
use crate::movies::availability::is_available;
use crate::movies::downloads::{pending, process_decisions, MovieDownloads, ProcessedMovies};
use crate::movies::model::Movie;
use crate::movies::search::MovieSearchService;
use crate::movies::store;

/// What the runtime needs of Radarr: its downloads, its searches and its
/// indexers.
#[derive(Clone)]
pub struct MovieSide {
    pub downloads: Arc<MovieDownloads>,
    pub search: MovieSearchService,
    pub indexers: IndexerService,
}

/// What a command did that its caller acts on: the queue to refresh after
/// a grab, the UI to tell about a pending release.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    pub grabbed: usize,
    pub pending_changed: bool,
}

impl Outcome {
    fn add(&mut self, processed: &ProcessedMovies) {
        self.grabbed += processed.grabbed.len();
        self.pending_changed |= !processed.pending.is_empty();
    }
}

/// `RssSyncService.Sync`: what the indexers list as new and what is
/// pending, decided on and processed.
pub async fn rss_sync(
    side: &MovieSide,
    db: &Db,
    now: DateTime<Utc>,
    progress: &(dyn Fn(String) + Sync),
) -> anyhow::Result<Outcome> {
    progress("Starting RSS Sync".into());
    let mut reports = side.search.fetch_recent(now).await?;
    reports.extend(db.call(move |conn| pending::get_pending(conn, now)).await?);
    let count = reports.len();
    let decisions = side.search.decide_rss(reports, now).await?;
    let processed = process_decisions(&side.downloads, decisions, now).await?;
    let mut message = format!(
        "RSS Sync Completed. Reports found: {}, Reports grabbed: {}",
        count,
        processed.grabbed.len()
    );
    if !processed.pending.is_empty() {
        message.push_str(&format!(", Reports pending: {}", processed.pending.len()));
    }
    progress(message);
    let mut outcome = Outcome::default();
    outcome.add(&processed);
    // `RssSyncCompleteEvent`: the pending releases drop what was rejected.
    let rejected = processed.rejected;
    outcome.pending_changed |= db
        .call(move |conn| pending::remove_rejected(conn, &rejected))
        .await?;
    Ok(outcome)
}

/// `SearchForBulkMovies`: the movies searched one after the other, the one
/// searched longest ago first.
async fn search_bulk(
    side: &MovieSide,
    mut movies: Vec<Movie>,
    user_invoked: bool,
    now: DateTime<Utc>,
    progress: &(dyn Fn(String) + Sync),
) -> anyhow::Result<Outcome> {
    progress(format!("Performing search for {} movies", movies.len()));
    movies.sort_by_key(|m| m.last_search_time);
    let mut outcome = Outcome::default();
    for movie in &movies {
        let decisions = match side.search.movie_search(movie.id, user_invoked, false, now).await {
            Ok(decisions) => decisions,
            Err(e) => {
                tracing::error!(movie = movie.id, error = ?e, "unable to search for movie");
                continue;
            }
        };
        outcome.add(&process_decisions(&side.downloads, decisions, now).await?);
    }
    progress(format!(
        "Completed search for {} movies. {} reports downloaded.",
        movies.len(),
        outcome.grabbed
    ));
    Ok(outcome)
}

/// `Execute(MoviesSearchCommand)`: the movies named; on a schedule only
/// those that are monitored and available.
pub async fn movies_search(
    side: &MovieSide,
    db: &Db,
    movie_ids: &[i64],
    user_invoked: bool,
    now: DateTime<Utc>,
    progress: &(dyn Fn(String) + Sync),
) -> anyhow::Result<Outcome> {
    let ids = movie_ids.to_vec();
    let (movies, delay) = db
        .call(move |conn| Ok((store::movies_by_ids(conn, &ids)?, availability_delay(conn)?)))
        .await?;
    let movies = movies
        .into_iter()
        .filter(|m| user_invoked || (m.monitored && is_available(m, now, delay)))
        .collect();
    search_bulk(side, movies, user_invoked, now, progress).await
}

/// `AvailabilityDelay` of the indexer settings, in days.
fn availability_delay(conn: &rusqlite::Connection) -> anyhow::Result<i64> {
    Ok(crate::store::config::config(conn, "availabilitydelay")?
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0))
}

/// The movies a bulk search covers: those `wanted` picks among the
/// monitored ones, without the ones the queue already holds.
async fn wanted_movies<F>(side: &MovieSide, db: &Db, wanted: F) -> anyhow::Result<Vec<Movie>>
where
    F: Fn(&Movie) -> bool + Send + 'static,
{
    let queued: Vec<i64> = side
        .downloads
        .queue()
        .iter()
        .filter_map(|q| q.movie.as_ref().map(|m| m.id))
        .collect();
    db.call(move |conn| {
        let mut out = Vec::new();
        for movie in store::list_movies(conn)? {
            if movie.monitored && !queued.contains(&movie.id) && wanted(&movie) {
                out.push(movie);
            }
        }
        Ok(out)
    })
    .await
}

/// `Execute(MissingMoviesSearchCommand)`: every monitored movie without a
/// file that is not in the queue.
pub async fn missing_movies_search(
    side: &MovieSide,
    db: &Db,
    user_invoked: bool,
    now: DateTime<Utc>,
    progress: &(dyn Fn(String) + Sync),
) -> anyhow::Result<Outcome> {
    let movies = wanted_movies(side, db, |movie| movie.movie_file_id == 0).await?;
    search_bulk(side, movies, user_invoked, now, progress).await
}

/// `Execute(CutoffUnmetMoviesSearchCommand)`: every monitored movie whose
/// file is below the cutoff of its profile and that is not in the queue.
pub async fn cutoff_unmet_movies_search(
    side: &MovieSide,
    db: &Db,
    user_invoked: bool,
    now: DateTime<Utc>,
    progress: &(dyn Fn(String) + Sync),
) -> anyhow::Result<Outcome> {
    let profiles = db
        .call(|conn| crate::store::catalog::list_quality_profiles(conn))
        .await?;
    // The files in one read, not one per movie.
    let files: std::collections::HashMap<i64, i64> = db
        .call(|conn| {
            Ok(store::list_movie_files(conn)?
                .into_iter()
                .map(|f| (f.id, f.quality.quality_id))
                .collect())
        })
        .await?;
    let movies = wanted_movies(side, db, move |movie| {
        let (Some(quality_id), Some(profile)) = (
            files.get(&movie.movie_file_id),
            profiles.iter().find(|p| p.id == movie.quality_profile_id),
        ) else {
            return false;
        };
        crate::profile::quality_cutoff_not_met(profile, *quality_id)
    })
    .await?;
    search_bulk(side, movies, user_invoked, now, progress).await
}
