//! Movies, their files and extras, collections, credits and alternative
//! titles: Radarr's library, read.

use std::collections::{HashMap, HashSet};

use axum::extract::{Path, State};
use axum::http::Uri;
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
struct Library {
    movies: Vec<Movie>,
    titles: HashMap<i64, Vec<AlternativeTitle>>,
    files: HashMap<i64, MovieFile>,
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
            let mut titles: HashMap<i64, Vec<AlternativeTitle>> = HashMap::new();
            let wanted: HashSet<i64> = all.iter().map(|m| m.metadata.id).collect();
            let found = if all.len() == 1 {
                movies::alternative_titles_of(conn, all[0].metadata.id)?
            } else {
                movies::list_alternative_titles(conn)?
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
        })
        .await
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
    let ctx = MovieContext::load(&state).await?;
    let library = load_library(&state, None, Some(id)).await?;
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
    let ctx = MovieContext::load(&state).await?;
    let files = files_with_movies(&state, movie_ids, file_ids).await?;
    Ok(Json(file_resources(&ctx, &files)))
}

pub async fn file(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let ctx = MovieContext::load(&state).await?;
    let files = files_with_movies(&state, Vec::new(), vec![id]).await?;
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
    folder_format: String,
}

async fn load_collections(state: &AppState) -> anyhow::Result<Collections> {
    state
        .db()
        .call(|conn| {
            Ok(Collections {
                collections: movies::list_collections(conn)?,
                members: movies::metadata_in_collections(conn)?,
                in_library: movies::list_movies(conn)?
                    .into_iter()
                    .map(|m| m.metadata.tmdb_id)
                    .collect(),
                excluded: movies::list_import_exclusions(conn)?
                    .into_iter()
                    .map(|e| e.tmdb_id)
                    .collect(),
                folder_format: movies::naming(conn)?.movie_folder_format,
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
        .filter(|m| !all.in_library.contains(&m.tmdb_id))
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
                "folder": movie_naming::movie_folder(&all.folder_format, m),
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

pub async fn collection(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let all = load_collections(&state).await?;
    Ok(Json(found(
        all.collections
            .iter()
            .find(|c| c.id == id)
            .map(|c| collection_resource(&all, c)),
    )?))
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
