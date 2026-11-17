//! Movies, their files and extras, collections, credits and alternative
//! titles: Radarr's library, read.

use std::collections::{HashMap, HashSet};

use axum::extract::{Path, State};
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};
use unarr_core::movies::model::{
    AlternativeTitle, Collection, Credit, ExtraKind, Movie, MovieExtraFile, MovieFile,
};
use unarr_core::movies::naming as movie_naming;
use unarr_core::movies::store as movies;

use super::resources::{
    alternative_title_resource, images_resource, movie_file_resource, movie_resource, Covers, FileShown,
    MovieContext, MovieParts,
};
use crate::error::{found, ApiError, ApiResult};
use crate::query::Params;
use crate::state::AppState;

// --- movies

/// Movies with what their resources show: titles by metadata id, files by id.
pub(super) struct Library {
    pub(super) movies: Vec<Movie>,
    pub(super) titles: HashMap<i64, Vec<AlternativeTitle>>,
    pub(super) files: HashMap<i64, MovieFile>,
}

async fn load_library(state: &AppState, tmdb_id: Option<i64>, id: Option<i64>) -> anyhow::Result<Library> {
    state
        .db()
        .call(move |conn| {
            let all = match (id, tmdb_id) {
                (Some(id), _) => movies::get_movie(conn, id)?.into_iter().collect(),
                (None, Some(tmdb_id)) => movies::movies_by_tmdb_id(conn, tmdb_id)?,
                (None, None) => movies::list_movies(conn)?,
            };
            library_of(conn, all, id.is_none() && tmdb_id.is_none())
        })
        .await
}

/// These movies with their alternative titles and files. The whole library
/// reads every title at once; a part of it, only its own.
pub(super) fn library_of(
    conn: &rusqlite::Connection,
    all: Vec<Movie>,
    whole_library: bool,
) -> anyhow::Result<Library> {
    let mut titles: HashMap<i64, Vec<AlternativeTitle>> = HashMap::new();
    let wanted: HashSet<i64> = all.iter().map(|m| m.metadata.id).collect();
    let found = if all.len() == 1 {
        movies::alternative_titles_of(conn, all[0].metadata.id)?
    } else if whole_library {
        movies::list_alternative_titles(conn)?
    } else {
        let ids: Vec<i64> = wanted.iter().copied().collect();
        movies::alternative_titles_for(conn, &ids)?
    };
    for t in found {
        if wanted.contains(&t.movie_metadata_id) {
            titles.entry(t.movie_metadata_id).or_default().push(t);
        }
    }
    let file_ids: Vec<i64> = all.iter().map(|m| m.movie_file_id).filter(|id| *id > 0).collect();
    let files = movies::movie_files_by_ids(conn, &file_ids)?
        .into_iter()
        .map(|f| (f.id, f))
        .collect();
    Ok(Library {
        movies: all,
        titles,
        files,
    })
}

fn library_resources(ctx: &MovieContext, library: &Library, covers: Covers) -> Vec<Value> {
    library
        .movies
        .iter()
        .map(|m| {
            movie_resource(
                ctx,
                m,
                &MovieParts {
                    alternative_titles: library
                        .titles
                        .get(&m.metadata.id)
                        .map(Vec::as_slice)
                        .unwrap_or(&[]),
                    file: library.files.get(&m.movie_file_id),
                    covers,
                    root_folder: true,
                    statistics: true,
                    movie_file: FileShown::Plain,
                },
            )
        })
        .collect()
}

/// `GET /movie`: the library in id order; `tmdbId` narrows it to one movie,
/// `excludeLocalCovers` leaves the covers as the metadata server names them.
pub async fn list(State(state): State<AppState>, uri: Uri) -> ApiResult<Json<Vec<Value>>> {
    let p = Params::from_uri(&uri);
    let covers = if p.bool("excludeLocalCovers", false) {
        Covers::Remote
    } else {
        Covers::Local
    };
    let tmdb_id = p.i64("tmdbId").filter(|id| *id > 0);
    let ctx = MovieContext::load(&state).await?;
    let library = load_library(&state, tmdb_id, None).await?;
    // A whole library is a megabyte of JSON: built off the async workers.
    Ok(Json(
        crate::blocking::run(move || library_resources(&ctx, &library, covers)).await?,
    ))
}

pub async fn get(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let library = load_library(&state, None, Some(id)).await?;
    let ctx = MovieContext::load_for(&state, &library.movies).await?;
    Ok(Json(found(
        library_resources(&ctx, &library, Covers::Local)
            .into_iter()
            .next(),
    )?))
}

// --- movie files

async fn files_with_movies(
    state: &AppState,
    movie_ids: Vec<i64>,
    file_ids: Vec<i64>,
) -> anyhow::Result<Vec<(MovieFile, Option<Movie>)>> {
    state
        .db()
        .call(move |conn| {
            let files = if !movie_ids.is_empty() {
                movies::movie_files_by_movies(conn, &movie_ids)?
            } else {
                movies::movie_files_by_ids(conn, &file_ids)?
            };
            let owners: Vec<i64> = files.iter().map(|f| f.movie_id).collect();
            let by_id: HashMap<i64, Movie> = movies::movies_by_ids(conn, &owners)?
                .into_iter()
                .map(|m| (m.id, m))
                .collect();
            Ok(files
                .into_iter()
                .map(|f| {
                    let movie = by_id.get(&f.movie_id).cloned();
                    (f, movie)
                })
                .collect())
        })
        .await
}

fn file_resources(ctx: &MovieContext, files: &[(MovieFile, Option<Movie>)]) -> Vec<Value> {
    files
        .iter()
        .filter_map(|(f, m)| m.as_ref().map(|m| movie_file_resource(ctx, f, m, true)))
        .collect()
}

/// `GET /moviefile?movieId=` (repeatable) or `?movieFileIds=`.
pub async fn files(State(state): State<AppState>, uri: Uri) -> ApiResult<Json<Vec<Value>>> {
    let p = Params::from_uri(&uri);
    let movie_ids = p.ints("movieId");
    let file_ids = p.ints("movieFileIds");
    if movie_ids.is_empty() && file_ids.is_empty() {
        return Err(ApiError::BadRequest(
            "movieId or movieFileIds must be provided".into(),
        ));
    }
    let files = files_with_movies(&state, movie_ids, file_ids).await?;
    let ctx = MovieContext::load_for(&state, files.iter().filter_map(|(_, m)| m.as_ref())).await?;
    Ok(Json(file_resources(&ctx, &files)))
}

pub async fn file(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let files = files_with_movies(&state, Vec::new(), vec![id]).await?;
    let ctx = MovieContext::load_for(&state, files.iter().filter_map(|(_, m)| m.as_ref())).await?;
    Ok(Json(found(file_resources(&ctx, &files).into_iter().next())?))
}

// --- extra files

fn extra_file_resource(e: &MovieExtraFile) -> Value {
    let mut out = json!({
        "id": e.id,
        "movieId": e.movie_id,
        "movieFileId": e.movie_file_id.unwrap_or(0),
        "relativePath": e.relative_path,
        "extension": e.extension,
        "type": e.kind.as_str(),
    });
    if e.kind == ExtraKind::Subtitle {
        out["languageTags"] = json!(e.language_tags);
        if let Some(title) = &e.title {
            out["title"] = json!(title);
        }
    }
    out
}

/// `GET /extrafile?movieId=`: the subtitles, the metadata files and the
/// other extras of a movie, each kind by path.
pub async fn extra_files(State(state): State<AppState>, uri: Uri) -> ApiResult<Json<Vec<Value>>> {
    let p = Params::from_uri(&uri);
    let Some(movie_id) = p.i64("movieId") else {
        return Ok(Json(Vec::new()));
    };
    let mut extras = state
        .db()
        .call(move |c| movies::extra_files_of(c, movie_id))
        .await?;
    let rank = |k: ExtraKind| match k {
        ExtraKind::Subtitle => 0,
        ExtraKind::Metadata => 1,
        ExtraKind::Other => 2,
    };
    extras.sort_by(|a, b| {
        rank(a.kind)
            .cmp(&rank(b.kind))
            .then_with(|| a.relative_path.cmp(&b.relative_path))
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(Json(extras.iter().map(extra_file_resource).collect()))
}

// --- collections

/// Collections with the movies each names and what the library and the
/// exclusions say about them.
struct Collections {
    collections: Vec<Collection>,
    members: Vec<unarr_core::movies::model::MovieMetadata>,
    in_library: HashSet<i64>,
    excluded: HashSet<i64>,
    naming: unarr_core::movies::model::MovieNamingConfig,
}

async fn load_collections(state: &AppState) -> anyhow::Result<Collections> {
    state
        .db()
        .call(|conn| {
            Ok(Collections {
                collections: movies::list_collections(conn)?,
                members: movies::metadata_in_collections(conn)?,
                in_library: movies::all_movie_tmdb_ids(conn)?,
                excluded: movies::list_import_exclusions(conn)?
                    .into_iter()
                    .map(|e| e.tmdb_id)
                    .collect(),
                naming: movies::naming(conn)?,
            })
        })
        .await
}

fn collection_resource(all: &Collections, c: &Collection) -> Value {
    let members: Vec<_> = all
        .members
        .iter()
        .filter(|m| m.collection_tmdb_id == c.tmdb_id)
        .collect();
    let missing = members
        .iter()
        // A movie the lists exclude is not one the collection misses.
        .filter(|m| !all.in_library.contains(&m.tmdb_id) && !all.excluded.contains(&m.tmdb_id))
        .count();
    let movies: Vec<Value> = members
        .iter()
        .map(|m| {
            let mut out = json!({
                "tmdbId": m.tmdb_id,
                "title": m.title,
                "status": m.status.as_str(),
                "runtime": m.runtime,
                "images": images_resource(&m.images, 0, "", Covers::Remote),
                "year": m.year,
                "ratings": m.ratings,
                "genres": m.genres,
                "folder": movie_naming::movie_folder(
                    &movie_naming::NamingMovie {
                        metadata: m,
                        translations: &[],
                    },
                    &all.naming,
                ),
                "isExisting": all.in_library.contains(&m.tmdb_id),
                "isExcluded": all.excluded.contains(&m.tmdb_id),
            });
            for (key, value) in [
                ("imdbId", &m.imdb_id),
                ("cleanTitle", &m.clean_title),
                ("sortTitle", &m.sort_title),
                ("overview", &m.overview),
            ] {
                if let Some(v) = value {
                    out[key] = json!(v);
                }
            }
            out
        })
        .collect();
    let mut out = json!({
        "id": c.id,
        "title": c.title,
        "tmdbId": c.tmdb_id,
        "images": images_resource(&c.images, 0, "", Covers::Remote),
        "monitored": c.monitored,
        "qualityProfileId": c.quality_profile_id,
        "minimumAvailability": c.minimum_availability.as_str(),
        "searchOnAdd": c.search_on_add,
        "movies": movies,
        "missingMovies": missing,
        "tags": c.tags,
    });
    for (key, value) in [
        ("sortTitle", &c.sort_title),
        ("overview", &c.overview),
        ("rootFolderPath", &c.root_folder_path),
    ] {
        if let Some(v) = value {
            out[key] = json!(v);
        }
    }
    out
}

/// `GET /collection`, all of them or the one of a TMDB id.
pub async fn collections(State(state): State<AppState>, uri: Uri) -> ApiResult<Json<Vec<Value>>> {
    let tmdb_id = Params::from_uri(&uri).i64("tmdbId");
    let all = load_collections(&state).await?;
    Ok(Json(
        all.collections
            .iter()
            .filter(|c| tmdb_id.is_none_or(|id| c.tmdb_id == id))
            .map(|c| collection_resource(&all, c))
            .collect(),
    ))
}

/// `GET /movie/{id}/folder`: the folder the naming settings would give
/// the movie, which the editor asks for before it moves one.
pub async fn folder(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let movie = state
        .db()
        .call(move |conn| movies::get_movie(conn, id))
        .await?
        .ok_or_else(|| crate::error::ApiError::ModelNotFound(format!("Movie with ID {id} does not exist")))?;
    let metadata_id = movie.metadata.id;
    // Every translated title under its own language, as the add and the
    // move read them: a format may ask for any of them.
    let (naming, translations) = state
        .db()
        .call(move |conn| {
            Ok((
                movies::naming(conn)?,
                movies::translation_titles(conn, metadata_id)?,
            ))
        })
        .await?;
    let folder = movie_naming::movie_folder(
        &movie_naming::NamingMovie {
            metadata: &movie.metadata,
            translations: &translations,
        },
        &naming,
    );
    Ok(Json(json!({"folder": folder})))
}

pub async fn collection(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let all = load_collections(&state).await?;
    Ok(Json(found(
        all.collections
            .iter()
            .find(|c| c.id == id)
            .map(|c| collection_resource(&all, c)),
    )?))
}

/// `CollectionController`'s handlers of the collection events: the
/// collection as it reads now, or the id of one that is gone.
pub async fn broadcast_collection(
    state: &AppState,
    collection_id: i64,
    action: unarr_core::commands::executor::CollectionAction,
) {
    use unarr_core::commands::executor::CollectionAction;
    if action == CollectionAction::Deleted {
        state.hub().broadcast(
            "collection",
            json!({"action": "deleted", "resource": {"id": collection_id}}),
        );
        return;
    }
    let Ok(all) = load_collections(state).await else {
        return;
    };
    if let Some(collection) = all.collections.iter().find(|c| c.id == collection_id) {
        let action = if action == CollectionAction::Created {
            "created"
        } else {
            "updated"
        };
        state.hub().broadcast(
            "collection",
            json!({"action": action, "resource": collection_resource(&all, collection)}),
        );
    }
}

// --- credits and alternative titles

/// `CreditResource`. The credits of one movie show their headshots
/// through the cover proxy (`proxied` holds the URL base); one credit, or
/// all of them, as the metadata server names them.
fn credit_resource(c: &Credit, proxied: Option<&str>) -> Value {
    let mut images = images_resource(&c.images, 0, "", Covers::Remote);
    if let Some(url_base) = proxied {
        for image in &mut images {
            if let Some(remote) = image["remoteUrl"].as_str().map(str::to_string) {
                image["url"] = json!(crate::routes::series_writes::register_proxy(url_base, &remote));
            }
        }
    }
    let mut out = json!({
        "id": c.id,
        "movieMetadataId": c.movie_metadata_id,
        "personTmdbId": c.person_tmdb_id,
        "images": images,
        "order": c.order,
        "type": c.credit_type,
    });
    for (key, value) in [
        ("personName", &c.name),
        ("creditTmdbId", &c.credit_tmdb_id),
        ("character", &c.character),
        ("department", &c.department),
        ("job", &c.job),
    ] {
        if let Some(v) = value {
            out[key] = json!(v);
        }
    }
    out
}

/// The metadata row a request names, by `movieMetadataId` or through its
/// movie; `None` when it names neither.
async fn metadata_of(state: &AppState, p: &Params) -> anyhow::Result<Option<i64>> {
    if let Some(id) = p.i64("movieMetadataId") {
        return Ok(Some(id));
    }
    let Some(movie_id) = p.i64("movieId") else {
        return Ok(None);
    };
    let movie = state.db().call(move |c| movies::get_movie(c, movie_id)).await?;
    // A movie that is not there has no credits and no titles.
    Ok(Some(movie.map(|m| m.metadata.id).unwrap_or(0)))
}

pub async fn credits(State(state): State<AppState>, uri: Uri) -> ApiResult<Json<Vec<Value>>> {
    let p = Params::from_uri(&uri);
    let url_base = state.settings().url_base.clone();
    let (found, proxied) = match metadata_of(&state, &p).await? {
        Some(id) => (
            state.db().call(move |c| movies::credits_of(c, id)).await?,
            Some(url_base.as_str()),
        ),
        None => (state.db().call(|c| movies::list_credits(c)).await?, None),
    };
    Ok(Json(found.iter().map(|c| credit_resource(c, proxied)).collect()))
}

pub async fn credit(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let c = state.db().call(move |c| movies::get_credit(c, id)).await?;
    Ok(Json(credit_resource(&found(c)?, None)))
}

pub async fn alternative_titles(State(state): State<AppState>, uri: Uri) -> ApiResult<Json<Vec<Value>>> {
    let p = Params::from_uri(&uri);
    let found = match metadata_of(&state, &p).await? {
        Some(id) => {
            state
                .db()
                .call(move |c| movies::alternative_titles_of(c, id))
                .await?
        }
        None => state.db().call(|c| movies::list_alternative_titles(c)).await?,
    };
    Ok(Json(found.iter().map(alternative_title_resource).collect()))
}

pub async fn alternative_title(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let t = state
        .db()
        .call(move |c| movies::get_alternative_title(c, id))
        .await?;
    Ok(Json(alternative_title_resource(&found(t)?)))
}

// --- the importer's events on SignalR

/// What the runtime raised about movies, for the hub.
pub enum HubMovieEvent {
    /// `MovieGrabbedEvent`.
    Grabbed(i64),
    /// The importer's events.
    Import(Box<unarr_core::movies::import::events::MovieImportEvent>),
    /// `MovieUpdatedEvent` and `MediaCoversUpdatedEvent`: a refresh, an
    /// edit or a cover changed the movie.
    Updated(i64),
    /// `MoviesDeletedEvent`, for one movie.
    Deleted(i64),
}

/// The movie events on the hub, in the order they were raised, on a task
/// of their own: each one reads the movie it is about, and a scan that
/// drops a hundred files raises a hundred of them, which must not hold up
/// the command and queue messages. The context every resource is built
/// with (profiles, formats, the translations of the whole library) is
/// read once for a run of events, again every two seconds of a long one,
/// and let go when the run ends.
pub async fn relay_movie_events(
    state: AppState,
    mut events: tokio::sync::mpsc::UnboundedReceiver<HubMovieEvent>,
) {
    // The context and when it was read. A run of events that goes on
    // reads it again after a while, so that what changed meanwhile (a
    // translation a refresh brought, a profile renamed) reaches the UI.
    const CONTEXT_LIFETIME: std::time::Duration = std::time::Duration::from_secs(2);
    let mut ctx: Option<(MovieContext, std::time::Instant)> = None;
    while let Some(event) = events.recv().await {
        // A panic while one event is told costs that event, as in the
        // relay this one is fed by.
        let told = unarr_core::panics::catch(relay_movie_event(&state, &mut ctx, event, CONTEXT_LIFETIME));
        if let Err(panic) = told.await {
            tracing::error!("The movie hub relay {panic}; the event is not sent");
            ctx = None;
        }
        if events.is_empty() {
            ctx = None;
        }
    }
}

/// One event of [`relay_movie_events`], with the context it keeps.
async fn relay_movie_event(
    state: &AppState,
    ctx: &mut Option<(MovieContext, std::time::Instant)>,
    event: HubMovieEvent,
    context_lifetime: std::time::Duration,
) {
    // `MovieController`'s own handler names the movie by its id and
    // reads nothing: it is told even when the context cannot be read.
    if let HubMovieEvent::Deleted(movie_id) = event {
        state.hub().broadcast(
            "movie",
            json!({"action": "deleted", "resource": {"id": movie_id}}),
        );
        return;
    }
    if ctx
        .as_ref()
        .is_none_or(|(_, read)| read.elapsed() > context_lifetime)
    {
        *ctx = MovieContext::load(state)
            .await
            .ok()
            .map(|loaded| (loaded, std::time::Instant::now()));
    }
    if let Some((ctx, _)) = ctx.as_ref() {
        match event {
            HubMovieEvent::Grabbed(movie_id) => broadcast_movie_change(state, ctx, movie_id, true).await,
            HubMovieEvent::Import(e) => relay_import(state, ctx, &e).await,
            HubMovieEvent::Deleted(_) => {}
            // `MovieController`'s own handler: the movie as it reads,
            // root folder included, under its own name alone.
            HubMovieEvent::Updated(movie_id) => {
                if let Some(resource) = movie_for_hub(state, ctx, movie_id).await {
                    state
                        .hub()
                        .broadcast("movie", json!({"action": "updated", "resource": resource}));
                }
            }
        }
    }
}

/// The movie as `GET /movie/{id}` answers it, for the hub.
async fn movie_for_hub(state: &AppState, ctx: &MovieContext, movie_id: i64) -> Option<Value> {
    let library = load_library(state, None, Some(movie_id)).await.ok()?;
    library_resources(ctx, &library, Covers::Local).into_iter().next()
}

/// The three names whose controllers share `MovieControllerWithSignalR`,
/// in the order the original's hub sent them. Their resource has no root
/// folder: only `MovieController`'s own mapping adds it.
fn broadcast_under_three_names(state: &AppState, resource: &Value, names: &[&str]) {
    let mut resource = resource.clone();
    if let Some(fields) = resource.as_object_mut() {
        fields.remove("rootFolderPath");
    }
    for name in names {
        state
            .hub()
            .broadcast(name, json!({"action": "updated", "resource": resource.clone()}));
    }
}

/// `MovieControllerWithSignalR.Handle` for a grab and an import: the movie
/// under the three names, `grabbed` set by a grab.
async fn broadcast_movie_change(state: &AppState, ctx: &MovieContext, movie_id: i64, grabbed: bool) {
    let Some(mut resource) = movie_for_hub(state, ctx, movie_id).await else {
        return;
    };
    if grabbed {
        if let Some(fields) = resource.as_object_mut() {
            fields.insert("grabbed".into(), json!(true));
        }
    }
    broadcast_under_three_names(state, &resource, &["wanted/cutoff", "wanted/missing", "movie"]);
}

/// A movie file is gone. The original tells the UI as the deletion
/// happens, when the movie has no file: `MovieController` first (the
/// movie edited, root folder included), then the wanted lists, the file,
/// and the movie again. Unarr relays after the fact, and by then an
/// upgrade has put the new file in: the movie is shown as it was in
/// between, and the import's own messages follow.
async fn broadcast_file_deleted(state: &AppState, ctx: &MovieContext, movie_id: i64, file_id: i64) {
    let resource = movie_for_hub(state, ctx, movie_id).await.map(|mut resource| {
        if let Some(fields) = resource.as_object_mut() {
            if fields.get("movieFileId").and_then(Value::as_i64).unwrap_or(0) != 0 {
                fields.insert("hasFile".into(), json!(false));
                fields.insert("movieFileId".into(), json!(0));
                fields.insert("sizeOnDisk".into(), json!(0));
                fields.insert(
                    "statistics".into(),
                    json!({"movieFileCount": 0, "movieFileQualities": [], "releaseGroups": [], "sizeOnDisk": 0}),
                );
                fields.remove("movieFile");
            }
        }
        resource
    });
    if let Some(resource) = &resource {
        state
            .hub()
            .broadcast("movie", json!({"action": "updated", "resource": resource}));
        broadcast_under_three_names(state, resource, &["wanted/cutoff", "wanted/missing"]);
    }
    state.hub().broadcast(
        "moviefile",
        json!({"action": "deleted", "resource": {"id": file_id}}),
    );
    if let Some(resource) = &resource {
        broadcast_under_three_names(state, resource, &["movie"]);
    }
}

/// What `MovieFileController` and the movie controllers broadcast for an
/// import's events: the movie file added (`updated`) or gone (`deleted`),
/// and the movie either touched.
async fn relay_import(
    state: &AppState,
    ctx: &MovieContext,
    e: &unarr_core::movies::import::events::MovieImportEvent,
) {
    use unarr_core::movies::import::events::MovieImportEvent;
    match e {
        MovieImportEvent::Imported(ev) => {
            if let Ok(files) = files_with_movies(state, Vec::new(), vec![ev.file.id]).await {
                for resource in file_resources(ctx, &files) {
                    state
                        .hub()
                        .broadcast("moviefile", json!({"action": "updated", "resource": resource}));
                }
            }
            broadcast_movie_change(state, ctx, ev.file.movie_id, false).await;
        }
        MovieImportEvent::FileDeleted(ev) => {
            broadcast_file_deleted(state, ctx, ev.movie.id, ev.file.id).await;
        }
        MovieImportEvent::ImportFailed(_)
        | MovieImportEvent::DownloadCompleted(_)
        | MovieImportEvent::ManualInteractionRequired(_)
        | MovieImportEvent::RescanRequested { .. }
        | MovieImportEvent::Scanned { .. }
        | MovieImportEvent::ScanSkipped { .. } => {}
        // `MovieController.Handle(MovieRenamedEvent)`; the files' new
        // paths reach the UI with the movie's page, as in the original.
        MovieImportEvent::MovieRenamed { movie_id, .. } => {
            if let Ok(library) = load_library(state, None, Some(*movie_id)).await {
                for resource in library_resources(ctx, &library, Covers::Local) {
                    state
                        .hub()
                        .broadcast("movie", json!({"action": "updated", "resource": resource}));
                }
            }
        }
        MovieImportEvent::FileRenamed(_) => {}
    }
}

// --- rename

/// `GET /rename?movieId=` (repeatable): the files of those movies the
/// naming settings would call otherwise.
pub async fn rename_preview(State(state): State<AppState>, uri: Uri) -> ApiResult<Json<Vec<Value>>> {
    let movie_ids = Params::from_uri(&uri).ints("movieId");
    if movie_ids.is_empty() {
        return Err(ApiError::BadRequest("movieId must be provided".into()));
    }
    let now = state.now();
    let previews = state
        .db()
        .call(move |conn| {
            let env = unarr_core::movies::import::decide::MovieEnvironment::load(conn, now)?;
            unarr_core::movies::import::rename::previews_for_movies(conn, &env, &movie_ids)
        })
        .await?;
    Ok(Json(
        previews
            .iter()
            .map(|p| {
                json!({
                    "movieId": p.movie_id,
                    "movieFileId": p.movie_file_id,
                    "existingPath": p.existing_path,
                    "newPath": p.new_path,
                })
            })
            .collect(),
    ))
}

// --- movie file writes

fn accepted(body: Value) -> Response {
    (StatusCode::ACCEPTED, Json(body)).into_response()
}

/// A quality model as a resource carries it.
fn quality_from_json(v: &Value) -> unarr_core::model::QualityModel {
    unarr_core::model::QualityModel {
        quality_id: v["quality"]["id"].as_i64().unwrap_or(0),
        version: v["revision"]["version"].as_i64().unwrap_or(1),
        real: v["revision"]["real"].as_i64().unwrap_or(0),
        is_repack: v["revision"]["isRepack"].as_bool().unwrap_or(false),
    }
}

/// The languages of a resource; a file is never set to 'Any' or
/// 'Original' by the editors.
fn languages_from_json(v: &Value, editor: bool) -> Vec<i64> {
    use unarr_core::movies::parser::language::lang;
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|l| l["id"].as_i64())
                .filter(|id| !editor || (*id != lang::ANY && *id != lang::ORIGINAL))
                .collect()
        })
        .unwrap_or_default()
}

/// A scene name is taken only when it reads as one (`SceneChecker`).
fn scene_name_from_json(v: &Value) -> Option<String> {
    v.as_str()
        .filter(|s| unarr_core::parser::title::is_scene_title(s))
        .map(str::to_string)
}

/// What `PUT moviefile/editor` and `PUT moviefile/bulk` set: only what
/// the request names.
fn apply_edits(file: &mut MovieFile, edits: &Value) {
    if edits["languages"].is_array() {
        file.languages = languages_from_json(&edits["languages"], true);
    }
    if edits["quality"].is_object() {
        file.quality = quality_from_json(&edits["quality"]);
    }
    if let Some(flags) = edits["indexerFlags"].as_i64() {
        file.indexer_flags = flags;
    }
    if let Some(edition) = edits["edition"].as_str() {
        file.edition = Some(edition.to_string());
    }
    if let Some(group) = edits["releaseGroup"].as_str() {
        file.release_group = Some(group.to_string());
    }
    if let Some(scene) = scene_name_from_json(&edits["sceneName"]) {
        file.scene_name = Some(scene);
    }
}

async fn file_resources_by_ids(state: &AppState, ids: Vec<i64>) -> ApiResult<Vec<Value>> {
    let files = files_with_movies(state, Vec::new(), ids).await?;
    let ctx = MovieContext::load_for(state, files.iter().filter_map(|(_, m)| m.as_ref())).await?;
    Ok(file_resources(&ctx, &files))
}

/// `PUT /moviefile/{id}`: the file the body names (not the path's id, as
/// in the original) gets the body's quality, languages, edition and
/// flags outright, its release group and scene name when given.
pub async fn file_update(State(state): State<AppState>, Json(body): Json<Value>) -> ApiResult<Response> {
    let id = body["id"].as_i64().unwrap_or(0);
    let body2 = body.clone();
    let found = state
        .db()
        .call(move |conn| {
            let Some(mut file) = movies::get_movie_file(conn, id)? else {
                return Ok(false);
            };
            file.indexer_flags = body2["indexerFlags"].as_i64().unwrap_or(0);
            file.quality = quality_from_json(&body2["quality"]);
            file.languages = languages_from_json(&body2["languages"], false);
            file.edition = body2["edition"].as_str().map(str::to_string);
            if let Some(group) = body2["releaseGroup"].as_str() {
                file.release_group = Some(group.to_string());
            }
            if let Some(scene) = scene_name_from_json(&body2["sceneName"]) {
                file.scene_name = Some(scene);
            }
            movies::update_movie_file(conn, &file)?;
            Ok(true)
        })
        .await?;
    if !found {
        return Err(ApiError::ModelNotFound(format!(
            "MovieFile with ID {id} does not exist"
        )));
    }
    let mut resources = file_resources_by_ids(&state, vec![id]).await?;
    Ok(accepted(resources.pop().unwrap_or(Value::Null)))
}

/// `PUT /moviefile/editor` (the original's obsolete one).
pub async fn file_editor(State(state): State<AppState>, Json(body): Json<Value>) -> ApiResult<Response> {
    let ids: Vec<i64> = body["movieFileIds"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default();
    let (ids2, body2) = (ids.clone(), body.clone());
    state
        .db()
        .call(move |conn| {
            for mut file in movies::movie_files_by_ids(conn, &ids2)? {
                apply_edits(&mut file, &body2);
                movies::update_movie_file(conn, &file)?;
            }
            Ok(())
        })
        .await?;
    Ok(accepted(Value::Array(file_resources_by_ids(&state, ids).await?)))
}

/// `PUT /moviefile/bulk`: `SetPropertiesBulk`.
pub async fn file_bulk_update(State(state): State<AppState>, Json(body): Json<Value>) -> ApiResult<Response> {
    let items = body.as_array().cloned().unwrap_or_default();
    let ids: Vec<i64> = items.iter().filter_map(|i| i["id"].as_i64()).collect();
    let (ids2, items2) = (ids.clone(), items.clone());
    state
        .db()
        .call(move |conn| {
            for mut file in movies::movie_files_by_ids(conn, &ids2)? {
                if let Some(item) = items2.iter().find(|i| i["id"].as_i64() == Some(file.id)) {
                    apply_edits(&mut file, item);
                    movies::update_movie_file(conn, &file)?;
                }
            }
            Ok(())
        })
        .await?;
    Ok(accepted(Value::Array(file_resources_by_ids(&state, ids).await?)))
}

/// `MediaFileDeletionService.DeleteMovieFile` for one file, with the
/// original's status codes.
async fn delete_one(state: &AppState, id: i64) -> ApiResult<()> {
    use unarr_core::movies::import::{deletion, events::MovieImportEvent};
    let now = state.now();
    let (config, movie, file) = state
        .db()
        .call(move |conn| {
            let Some(file) = movies::get_movie_file(conn, id)? else {
                return Ok(None);
            };
            let movie = movies::get_movie(conn, file.movie_id)?;
            let config = unarr_core::store::config::MediaManagementConfig::load(conn)?;
            Ok(Some((config, movie, file)))
        })
        .await?
        .ok_or_else(|| ApiError::ModelNotFound(format!("MovieFile with ID {id} does not exist")))?;
    let movie = movie
        .ok_or_else(|| ApiError::ModelNotFound(format!("Movie with ID {} does not exist", file.movie_id)))?;
    let db = state.db().clone();
    let result =
        crate::blocking::run(move || deletion::delete_movie_file(&db, &config, &movie, &file, now)).await?;
    match result {
        Ok(deleted) => {
            if let Ok(runtime) = state.commands() {
                runtime
                    .publish_movie_import_events(vec![MovieImportEvent::FileDeleted(Box::new(deleted))])
                    .await;
            }
            Ok(())
        }
        Err(e) => Err(match e.status() {
            409 => ApiError::ConflictMessage(e.to_string()),
            _ => ApiError::ServerMessage(e.to_string()),
        }),
    }
}

/// `DELETE /moviefile/{id}`.
pub async fn file_delete(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Response> {
    delete_one(&state, id).await?;
    Ok(StatusCode::OK.into_response())
}

/// `DELETE /moviefile/bulk`.
pub async fn file_bulk_delete(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let ids: Vec<i64> = body["movieFileIds"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default();
    if ids.is_empty() {
        return Err(ApiError::BadRequest("movieFileIds must be provided".into()));
    }
    let ids2 = ids.clone();
    let existing: Vec<i64> = state
        .db()
        .call(move |conn| {
            Ok(movies::movie_files_by_ids(conn, &ids2)?
                .into_iter()
                .map(|f| f.id)
                .collect())
        })
        .await?;
    for id in existing {
        delete_one(&state, id).await?;
    }
    Ok(Json(json!({})))
}
