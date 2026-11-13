//! Radarr's collections: `RefreshCollectionService`. A collection is what
//! TMDB groups movies by; one that is monitored brings the movies it names
//! into the library.

use chrono::{DateTime, Utc};
use serde_json::json;

use crate::db::Db;
use crate::movies::library::NewMovie;
use crate::movies::metadata::{MapContext, MovieMetadataError, MovieMetadataProvider};
use crate::movies::model::{Collection, MovieStatus};
use crate::movies::store;

#[derive(Debug)]
pub enum CollectionError {
    /// `MovieNotFoundException`: TMDB no longer has the collection, which
    /// was removed from the library.
    NotFound(Box<Collection>),
    /// No such collection in the library.
    NoSuchCollection(i64),
    Other(anyhow::Error),
}

impl From<anyhow::Error> for CollectionError {
    fn from(e: anyhow::Error) -> Self {
        CollectionError::Other(e)
    }
}

/// `RefreshCollectionInfo`: the collection's title, overview and images
/// as the provider has them now, and the metadata of the movies it names
/// stored, in the library or not.
pub async fn refresh_collection_info(
    db: &Db,
    provider: &dyn MovieMetadataProvider,
    ctx: &MapContext,
    collection_id: i64,
) -> Result<Collection, CollectionError> {
    let collection = db
        .call(move |conn| store::get_collection(conn, collection_id))
        .await?
        .ok_or(CollectionError::NoSuchCollection(collection_id))?;
    let info = match provider.collection_info(collection.tmdb_id, ctx).await {
        Ok(info) => info,
        Err(MovieMetadataError::MovieNotFound(_)) => {
            let id = collection.id;
            db.call(move |conn| store::delete_collection(conn, id)).await?;
            tracing::debug!("Removing collection not present on TMDb for {}", collection.title);
            return Err(CollectionError::NotFound(Box::new(collection)));
        }
        Err(e) => return Err(CollectionError::Other(anyhow::anyhow!("{e}"))),
    };
    let now = ctx.now;
    Ok(db
        .call(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let mut collection = collection;
            collection.title = info.title;
            collection.overview = info.overview;
            collection.clean_title = Some(info.clean_title);
            collection.sort_title = Some(info.sort_title);
            collection.last_info_sync = Some(now);
            collection.images = info.images;
            let mut before = store::metadata_by_collection(&tx, collection.tmdb_id)?;
            for part in info.movies {
                let mut metadata = part.metadata;
                metadata.collection_tmdb_id = collection.tmdb_id;
                before.retain(|m| m.tmdb_id != metadata.tmdb_id);
                // `UpsertMany`: a row the library already has is replaced by
                // the part as the provider gives it, when the two differ
                // in anything, the time of its last refresh included.
                match store::metadata_by_tmdb_id(&tx, metadata.tmdb_id)? {
                    Some(existing) => {
                        metadata.id = existing.id;
                        if metadata != existing {
                            store::update_metadata(&tx, &metadata)?;
                        }
                    }
                    None => {
                        store::upsert_metadata(&tx, &mut metadata)?;
                    }
                }
            }
            // What the collection no longer names goes, unless a movie of
            // the library is that row: the original deletes it all the
            // same and leaves the movie without its metadata.
            for gone in before {
                if store::movie_of_metadata(&tx, gone.id)?.is_none() {
                    store::delete_metadata(&tx, gone.id)?;
                }
            }
            store::update_collection(&tx, &collection)?;
            tx.commit()?;
            tracing::debug!("Finished collection refresh for {}", collection.title);
            Ok(collection)
        })
        .await?)
}

/// `SyncCollectionMovies`: the movies of a monitored collection that are
/// out, that the library does not have and that no list exclusion names,
/// as the collection's settings would add them.
pub fn movies_to_add(conn: &rusqlite::Connection, collection: &Collection) -> anyhow::Result<Vec<NewMovie>> {
    if !collection.monitored {
        return Ok(Vec::new());
    }
    let existing = store::all_movie_tmdb_ids(conn)?;
    let excluded: std::collections::HashSet<i64> = store::list_import_exclusions(conn)?
        .into_iter()
        .map(|e| e.tmdb_id)
        .collect();
    Ok(store::metadata_by_collection(conn, collection.tmdb_id)?
        .into_iter()
        .filter(|m| matches!(m.status, MovieStatus::InCinemas | MovieStatus::Released))
        .filter(|m| !existing.contains(&m.tmdb_id) && !excluded.contains(&m.tmdb_id))
        .map(|m| NewMovie {
            tmdb_id: m.tmdb_id,
            title: Some(m.title),
            path: None,
            root_folder_path: collection.root_folder_path.clone(),
            quality_profile_id: collection.quality_profile_id,
            monitored: true,
            minimum_availability: collection.minimum_availability,
            tags: collection.tags.clone(),
            add_options: Some(crate::movies::library::add_options(
                Some(&json!({"searchForMovie": collection.search_on_add})),
                "collection",
            )),
        })
        .collect())
}

/// The collections a scheduled or manual refresh of all of them covers,
/// by sort title, each with whether it is due.
pub fn all_by_sort_title(
    conn: &rusqlite::Connection,
    now: DateTime<Utc>,
    manual: bool,
) -> anyhow::Result<Vec<(Collection, bool)>> {
    let mut all = store::list_collections(conn)?;
    all.sort_by(|a, b| a.sort_title.cmp(&b.sort_title));
    Ok(all
        .into_iter()
        .map(|c| {
            let due = manual || crate::movies::refresh::collection_should_refresh(&c, now);
            (c, due)
        })
        .collect())
}
