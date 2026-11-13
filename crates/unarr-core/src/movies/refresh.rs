//! `RefreshMovieService`: a movie's metadata asked of the provider and
//! stored, with its alternative titles, translations, credits and the
//! collection it belongs to. What follows a refresh (the covers, the
//! rescan, the tags) is the command executor's, which calls in here.

use chrono::{DateTime, Duration, Utc};

use crate::db::Db;
use crate::movies::metadata::{MapContext, MovieInfo, MovieMetadataError, MovieMetadataProvider};
use crate::movies::model::{Collection, Movie, MovieMetadata, MovieStatus};
use crate::movies::store;

/// A movie as the refresh left it.
#[derive(Debug, Clone)]
pub struct Refreshed {
    pub movie: Movie,
    /// `CollectionAddedEvent`: the collection this refresh added, whose
    /// own refresh the original queues.
    pub collection_added: Option<Collection>,
}

#[derive(Debug)]
pub enum RefreshError {
    /// `MovieNotFoundException`: TMDB no longer has the movie. It was
    /// marked as deleted, unless it already was.
    NotFound {
        movie: Box<Movie>,
        marked_deleted: bool,
    },
    /// The movie is not in the library (`ModelNotFoundException`).
    NoSuchMovie(i64),
    Other(anyhow::Error),
}

impl From<anyhow::Error> for RefreshError {
    fn from(e: anyhow::Error) -> Self {
        RefreshError::Other(e)
    }
}

impl std::fmt::Display for RefreshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RefreshError::NotFound { movie, .. } => write!(
                f,
                "Movie '{}' (TMDb {}) was not found, it may have been removed from The Movie Database.",
                movie.metadata.title, movie.metadata.tmdb_id
            ),
            RefreshError::NoSuchMovie(id) => write!(f, "Movie with ID {id} does not exist"),
            RefreshError::Other(e) => write!(f, "{e:#}"),
        }
    }
}

/// `CertificationCountry` of the metadata settings, as the mapping wants
/// it: the country code in upper case, `US` unless saved otherwise.
pub fn certification_country(conn: &rusqlite::Connection) -> anyhow::Result<String> {
    Ok(crate::store::config::config(conn, "certificationcountry")?
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "US".into())
        .to_uppercase())
}

/// The context every answer of the provider is mapped in.
pub async fn map_context(db: &Db, now: DateTime<Utc>) -> anyhow::Result<MapContext> {
    let country = db.call(|conn| certification_country(conn)).await?;
    Ok(MapContext::new(now, &country))
}

/// What `RefreshMovieInfo` copies from the provider's answer onto the
/// stored metadata: everything but the ids, the collection (decided
/// apart) and what only the library knows.
pub fn apply_info(metadata: &mut MovieMetadata, info: &MovieMetadata, now: DateTime<Utc>) {
    if metadata.tmdb_id != info.tmdb_id {
        tracing::warn!(
            "Movie '{}' (TMDb: {}) was replaced with '{}' (TMDb: {}), because the original was a duplicate.",
            metadata.title,
            metadata.tmdb_id,
            info.title,
            info.tmdb_id
        );
        metadata.tmdb_id = info.tmdb_id;
    }
    metadata.title = info.title.clone();
    metadata.imdb_id = info.imdb_id.clone();
    metadata.overview = info.overview.clone();
    metadata.status = info.status;
    metadata.images = info.images.clone();
    metadata.clean_title = info.clean_title.clone();
    metadata.sort_title = info.sort_title.clone();
    metadata.last_info_sync = Some(now);
    metadata.runtime = info.runtime;
    metadata.ratings = info.ratings.clone();
    metadata.genres = info.genres.clone();
    metadata.keywords = info.keywords.clone();
    metadata.certification = info.certification.clone();
    metadata.in_cinemas = info.in_cinemas;
    metadata.website = info.website.clone();
    metadata.year = info.year;
    metadata.secondary_year = info.secondary_year;
    metadata.physical_release = info.physical_release;
    metadata.digital_release = info.digital_release;
    metadata.youtube_trailer_id = info.youtube_trailer_id.clone();
    metadata.studio = info.studio.clone();
    metadata.original_title = info.original_title.clone();
    metadata.clean_original_title = info.clean_original_title.clone();
    metadata.original_language_id = info.original_language_id;
    metadata.recommendations = info.recommendations.clone();
    metadata.popularity = info.popularity;
}

/// The collection a movie brings with it, as `AddMovieCollectionService`
/// builds it: the provider's title, overview and images, the movie's own
/// profile, availability, root folder and tags. `None` when TMDB does not
/// have the collection.
async fn new_collection(
    db: &Db,
    provider: &dyn MovieMetadataProvider,
    ctx: &MapContext,
    movie: &Movie,
    info: &MovieMetadata,
) -> anyhow::Result<Option<Collection>> {
    let fetched = match provider.collection_info(info.collection_tmdb_id, ctx).await {
        Ok(fetched) => fetched,
        Err(MovieMetadataError::MovieNotFound(_)) => {
            tracing::error!(
                "TmdbId {} was not found, it may have been removed from TMDb.",
                info.collection_tmdb_id
            );
            return Ok(None);
        }
        Err(e) => anyhow::bail!("{e}"),
    };
    let path = movie.path.clone();
    let root = db
        .call(move |conn| {
            let roots: Vec<String> = crate::store::catalog::list_root_folders(conn)?
                .into_iter()
                .map(|r| r.path)
                .collect();
            Ok(crate::health::best_root_folder(&path, &roots))
        })
        .await?;
    let options = movie.add_options.as_ref();
    Ok(Some(Collection {
        id: 0,
        tmdb_id: info.collection_tmdb_id,
        clean_title: Some(crate::movies::parser::clean_movie_title(&fetched.title)),
        sort_title: Some(crate::movies::metadata::sort_title(
            &fetched.title,
            info.collection_tmdb_id,
        )),
        title: fetched.title,
        overview: fetched.overview,
        images: fetched.images,
        monitored: options.and_then(|o| o["monitor"].as_str()) == Some("movieAndCollection"),
        quality_profile_id: movie.quality_profile_id,
        root_folder_path: Some(root.trim_end_matches('/').to_string()).filter(|r| !r.is_empty()),
        minimum_availability: movie.minimum_availability,
        search_on_add: options
            .and_then(|o| o["searchForMovie"].as_bool())
            .unwrap_or(false),
        tags: movie.tags.clone(),
        added: Some(ctx.now),
        last_info_sync: None,
    }))
}

/// What a refresh tells its caller about a collection it brought.
pub trait CollectionEvents: Sync {
    /// `CollectionAddedEvent`: the row is there; the original's handler
    /// queues the collection's own refresh.
    fn added(&self, collection: Collection) -> crate::metadata::BoxFuture<'_, ()>;

    /// Called before the movie's own metadata row is written: where the
    /// refresh queued by [`CollectionEvents::added`] is given the time to
    /// end.
    fn settled(&self) -> crate::metadata::BoxFuture<'_, ()>;
}

/// For callers with nothing to do about a new collection.
pub struct NoCollectionEvents;

impl CollectionEvents for NoCollectionEvents {
    fn added(&self, _collection: Collection) -> crate::metadata::BoxFuture<'_, ()> {
        Box::pin(async {})
    }

    fn settled(&self) -> crate::metadata::BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}

/// `RefreshMovieInfo`.
pub async fn refresh_movie_info(
    db: &Db,
    provider: &dyn MovieMetadataProvider,
    movie_id: i64,
    now: DateTime<Utc>,
    events: &dyn CollectionEvents,
) -> Result<Refreshed, RefreshError> {
    let movie = db
        .call(move |conn| store::get_movie(conn, movie_id))
        .await?
        .ok_or(RefreshError::NoSuchMovie(movie_id))?;
    let ctx = map_context(db, now).await?;
    let info: MovieInfo = match provider.movie_info(movie.metadata.tmdb_id, &ctx).await {
        Ok(info) => info,
        Err(MovieMetadataError::MovieNotFound(_)) => {
            let marked_deleted = movie.metadata.status != MovieStatus::Deleted;
            let mut movie = movie;
            if marked_deleted {
                movie.metadata.status = MovieStatus::Deleted;
                let metadata = movie.metadata.clone();
                db.call(move |conn| store::update_metadata(conn, &metadata))
                    .await?;
                tracing::debug!("Movie marked as deleted on TMDb for {}", movie.metadata.title);
            }
            return Err(RefreshError::NotFound {
                movie: Box::new(movie),
                marked_deleted,
            });
        }
        Err(e) => return Err(RefreshError::Other(anyhow::anyhow!("{e}"))),
    };
    // The collection, when the library does not have it yet, is asked of
    // the provider before anything is written.
    let collection_tmdb_id = info.metadata.collection_tmdb_id;
    let existing = if collection_tmdb_id > 0 {
        db.call(move |conn| Ok(store::collections_by_tmdb_id(conn, collection_tmdb_id)?.pop()))
            .await?
    } else {
        None
    };
    let brought = if collection_tmdb_id > 0 && existing.is_none() {
        new_collection(db, provider, &ctx, &movie, &info.metadata).await?
    } else {
        None
    };
    // `AddMovieCollection`: the collection's row first, on its own. The
    // original raises `CollectionAddedEvent` here, whose handler queues
    // the collection's refresh, and goes on to the movie's titles,
    // translations and credits. That refresh is one request, and writes
    // the metadata of the collection's movies, this one's among them,
    // before the movie's own row is written last: the movie has the last
    // word on its row. The same order is kept here, with the caller
    // giving the queued refresh the time to end before the last write.
    let mut collection_added = None;
    let collection = match (existing, brought) {
        (Some(existing), _) => Some(existing),
        (None, Some(new)) => {
            let (collection, added) = db
                .call(move |conn| {
                    // Another refresh may have added it meanwhile.
                    Ok(match store::collections_by_tmdb_id(conn, new.tmdb_id)?.pop() {
                        Some(existing) => (existing, false),
                        None => {
                            let mut new = new;
                            tracing::info!("Adding Collection {}[{}]", new.title, new.tmdb_id);
                            store::add_collection(conn, &mut new)?;
                            (new, true)
                        }
                    })
                })
                .await?;
            if added {
                events.added(collection.clone()).await;
                collection_added = Some(collection.clone());
            }
            Some(collection)
        }
        (None, None) => None,
    };
    let mut movie = movie;
    apply_info(&mut movie.metadata, &info.metadata, now);
    if collection_tmdb_id > 0 {
        // A collection TMDB does not have leaves the movie's as it was.
        if let Some(collection) = collection {
            movie.metadata.collection_tmdb_id = collection.tmdb_id;
            movie.metadata.collection_title = Some(collection.title);
        }
    } else {
        movie.metadata.collection_tmdb_id = 0;
        movie.metadata.collection_title = None;
    }
    let metadata = movie.metadata.clone();
    db.call(move |conn| {
        let tx = conn.unchecked_transaction()?;
        store::update_alternative_titles(&tx, info.alternative_titles, &metadata)?;
        store::update_translations(&tx, info.translations, metadata.id)?;
        store::update_credits(&tx, info.credits, metadata.id)?;
        tx.commit()?;
        Ok(())
    })
    .await?;
    if collection_added.is_some() {
        events.settled().await;
    }
    let metadata = movie.metadata.clone();
    db.call(move |conn| store::update_metadata(conn, &metadata))
        .await?;
    tracing::debug!("Finished movie metadata refresh for {}", movie.metadata.title);
    Ok(Refreshed {
        movie,
        collection_added,
    })
}

/// `MovieMetadata.PhysicalReleaseDate()`: the earlier of the physical and
/// the digital release, else ninety days after the cinemas.
fn physical_release_date(m: &MovieMetadata) -> Option<DateTime<Utc>> {
    match (m.physical_release, m.digital_release) {
        (Some(p), Some(d)) => Some(p.min(d)),
        (Some(p), None) => Some(p),
        (None, Some(d)) => Some(d),
        (None, None) => m.in_cinemas.map(|c| c + Duration::days(90)),
    }
}

/// `CheckIfMovieShouldBeRefreshed.ShouldRefresh`.
pub fn should_refresh(m: &MovieMetadata, now: DateTime<Utc>) -> bool {
    let synced = m.last_info_sync;
    if synced.is_none_or(|at| at < now - Duration::days(180)) {
        return true;
    }
    if synced.is_some_and(|at| at >= now - Duration::hours(12)) {
        return false;
    }
    if matches!(m.status, MovieStatus::Announced | MovieStatus::InCinemas) {
        return true;
    }
    m.status == MovieStatus::Released
        && physical_release_date(m).is_some_and(|at| at >= now - Duration::days(30))
}

/// `RefreshCollectionService.ShouldRefresh`.
pub fn collection_should_refresh(c: &Collection, now: DateTime<Utc>) -> bool {
    c.last_info_sync.is_none_or(|at| at < now - Duration::days(15))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata(status: MovieStatus, synced_hours_ago: i64, now: DateTime<Utc>) -> MovieMetadata {
        MovieMetadata {
            id: 1,
            tmdb_id: 1,
            imdb_id: None,
            title: "A".into(),
            sort_title: None,
            clean_title: None,
            original_title: None,
            clean_original_title: None,
            original_language_id: 1,
            status,
            overview: None,
            images: vec![],
            genres: vec![],
            keywords: vec![],
            ratings: serde_json::json!({}),
            recommendations: vec![],
            runtime: 0,
            in_cinemas: None,
            physical_release: None,
            digital_release: None,
            year: 2020,
            secondary_year: None,
            certification: None,
            youtube_trailer_id: None,
            studio: None,
            website: None,
            popularity: 0.0,
            collection_tmdb_id: 0,
            collection_title: None,
            last_info_sync: Some(now - Duration::hours(synced_hours_ago)),
        }
    }

    #[test]
    fn a_movie_is_refreshed_as_the_original_decides() {
        let now: DateTime<Utc> = "2026-11-13T12:00:00Z".parse().unwrap();
        // Synced within twelve hours: never.
        assert!(!should_refresh(&metadata(MovieStatus::Announced, 2, now), now));
        // Not out yet: every time past the twelve hours.
        assert!(should_refresh(&metadata(MovieStatus::Announced, 13, now), now));
        assert!(should_refresh(&metadata(MovieStatus::InCinemas, 13, now), now));
        // Out long ago: only after half a year.
        let mut old = metadata(MovieStatus::Released, 13, now);
        old.physical_release = Some(now - Duration::days(400));
        assert!(!should_refresh(&old, now));
        old.last_info_sync = Some(now - Duration::days(181));
        assert!(should_refresh(&old, now));
        // Out within the month.
        let mut recent = metadata(MovieStatus::Released, 13, now);
        recent.digital_release = Some(now - Duration::days(10));
        recent.physical_release = Some(now + Duration::days(40));
        assert!(should_refresh(&recent, now));
        // Never synced.
        let mut never = metadata(MovieStatus::Released, 0, now);
        never.last_info_sync = None;
        assert!(should_refresh(&never, now));
    }
}
