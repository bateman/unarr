//! Movies looked up, added, edited, moved and deleted: Radarr's
//! `MovieLookupController` and the writes of its `MovieController`.

use axum::extract::{Path, State};
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};
use unarr_core::movies::library::{self, ApplyTags, BulkEdit, Found, MovieEdit, NewMovie, WriteError};
use unarr_core::movies::metadata::{MovieInfo, MovieMetadataError};
use unarr_core::movies::model::{Movie, MovieStatus, MovieTranslation};
use unarr_core::movies::naming as movie_naming;
use unarr_core::movies::store as movies;
use unarr_core::movies::{refresh, store};

use super::resources::{movie_file_resource, movie_resource, Covers, FileShown, MovieContext, MovieParts};
use crate::error::{ApiError, ApiResult};
use crate::query::Params;
use crate::routes::series_writes::register_proxy;
use crate::state::AppState;

/// A movie the library does not have, as the model the original builds
/// around the provider's metadata: `new Movie { MovieMetadata = ... }`.
fn unadded(info: &MovieInfo) -> Movie {
    Movie {
        id: 0,
        metadata: info.metadata.clone(),
        path: String::new(),
        monitored: false,
        quality_profile_id: 0,
        minimum_availability: MovieStatus::Tba,
        movie_file_id: 0,
        tags: Vec::new(),
        added: None,
        add_options: None,
        last_search_time: None,
    }
}

/// The translation the resource shows the title and overview of: the
/// provider's own, in the language Settings > UI asks for.
fn translation_of(info: &MovieInfo, language_id: i64) -> Option<&MovieTranslation> {
    info.translations.iter().find(|t| t.language_id == language_id)
}

/// `ToResource` of a movie that is not in the library: no id of its own
/// or of its titles, no path, added at the beginning of time.
fn unadded_resource(ctx: &MovieContext, info: &MovieInfo, language_id: i64) -> Value {
    let movie = unadded(info);
    let mut resource = movie_resource(
        ctx,
        &movie,
        &MovieParts {
            alternative_titles: &info.alternative_titles,
            file: None,
            covers: Covers::Remote,
            root_folder: false,
            statistics: false,
            movie_file: FileShown::No,
        },
    );
    let translation = translation_of(info, language_id);
    if let Some(fields) = resource.as_object_mut() {
        fields.remove("id");
        fields.remove("path");
        fields.insert("folderName".into(), json!(""));
        fields.insert("added".into(), json!("0001-01-01T00:00:00Z"));
        // `movieTranslation?.Title ?? model.Title`.
        if let Some(title) = translation.and_then(|t| t.title.clone()) {
            fields.insert("title".into(), json!(title));
        }
        if let Some(overview) = translation.and_then(|t| t.overview.clone()) {
            fields.insert("overview".into(), json!(overview));
        }
        if let Some(titles) = fields.get_mut("alternateTitles").and_then(Value::as_array_mut) {
            for title in titles {
                if let Some(title) = title.as_object_mut() {
                    title.remove("id");
                }
            }
        }
    }
    resource
}

/// What `MovieLookupController.MapToResource` adds to each result: the
/// covers under an address of this server, the remote poster, the folder
/// the naming would give, and whether the lists exclude the movie.
fn decorate(
    resource: &mut Value,
    movie_id: i64,
    metadata: &unarr_core::movies::model::MovieMetadata,
    translations: &[(i64, String)],
    naming: &unarr_core::movies::model::MovieNamingConfig,
    url_base: &str,
    excluded: bool,
) {
    let Some(fields) = resource.as_object_mut() else {
        return;
    };
    if movie_id == 0 {
        if let Some(images) = fields.get_mut("images").and_then(Value::as_array_mut) {
            for image in images {
                let remote = image["remoteUrl"].as_str().map(str::to_string);
                if let (Some(remote), Some(image)) = (remote, image.as_object_mut()) {
                    image.insert("url".into(), json!(register_proxy(url_base, &remote)));
                }
            }
        }
    }
    if let Some(poster) = metadata
        .images
        .iter()
        .find(|i| i.cover_type == "poster")
        .and_then(|i| i.remote_url.clone())
    {
        fields.insert("remotePoster".into(), json!(poster));
    }
    fields.insert(
        "folder".into(),
        json!(movie_naming::movie_folder(
            &movie_naming::NamingMovie {
                metadata,
                translations,
            },
            naming,
        )),
    );
    if excluded {
        fields.insert("isExcluded".into(), json!(true));
    }
}

/// An exception of the provider the lookup by id lets escape: a 500.
fn lookup_error(e: MovieMetadataError) -> ApiError {
    let exception = match e {
        MovieMetadataError::MovieNotFound(_) | MovieMetadataError::ImdbNotFound(_) => {
            "NzbDrone.Core.Exceptions.MovieNotFoundException"
        }
        _ => "NzbDrone.Common.Http.HttpException",
    };
    ApiError::ServerException {
        message: e.to_string(),
        exception: exception.into(),
    }
}

/// The id of the language the titles and overviews are shown in
/// (`MovieInfoLanguage`).
async fn info_language(state: &AppState) -> anyhow::Result<i64> {
    state
        .db()
        .call(|conn| {
            Ok(unarr_core::store::config::config(conn, "movieinfolanguage")?
                .and_then(|v| v.parse().ok())
                .unwrap_or(1))
        })
        .await
}

/// `GET /movie/lookup?term=`.
pub async fn lookup(State(state): State<AppState>, uri: Uri) -> ApiResult<Json<Vec<Value>>> {
    let params = Params::from_uri(&uri);
    let provider = state.commands()?.movie_provider();
    let map = refresh::map_context(state.db(), state.now()).await?;
    let found = library::lookup(state.db(), provider.as_ref(), &map, params.get("term"))
        .await
        .map_err(|e| ApiError::Unavailable {
            message: e.0,
            exception: "NzbDrone.Core.MetadataSource.SkyHook.SkyHookException".into(),
        })?;
    let ctx = MovieContext::load(&state).await?;
    let language_id = info_language(&state).await?;
    let ids: Vec<(i64, i64)> = found
        .iter()
        .filter_map(|f| match f {
            Found::Library(movie) => Some((movie.metadata.id, movie.movie_file_id)),
            Found::New(_) => None,
        })
        .collect();
    // What the library's own movies show beside the row itself, the
    // naming, and the list exclusions.
    let (naming, excluded, parts) = state
        .db()
        .call(move |conn| {
            let mut parts = std::collections::HashMap::new();
            for (metadata_id, file_id) in ids {
                parts.insert(
                    metadata_id,
                    (
                        movies::alternative_titles_of(conn, metadata_id)?,
                        movies::translation_titles(conn, metadata_id)?,
                        movies::get_movie_file(conn, file_id)?,
                    ),
                );
            }
            let excluded: std::collections::HashSet<i64> = store::list_import_exclusions(conn)?
                .into_iter()
                .map(|e| e.tmdb_id)
                .collect();
            Ok((movies::naming(conn)?, excluded, parts))
        })
        .await?;
    let url_base = state.settings().url_base.clone();
    let mut out = Vec::with_capacity(found.len());
    for item in &found {
        let is_excluded = excluded.contains(&item.tmdb_id());
        match item {
            Found::Library(movie) => {
                let empty = (Vec::new(), Vec::new(), None);
                let (titles, translations, file) = parts.get(&movie.metadata.id).unwrap_or(&empty);
                let mut resource = movie_resource(
                    &ctx,
                    movie,
                    &MovieParts {
                        alternative_titles: titles,
                        file: None,
                        covers: Covers::Local,
                        root_folder: false,
                        statistics: false,
                        movie_file: FileShown::No,
                    },
                );
                if let (Some(file), Some(fields)) = (file, resource.as_object_mut()) {
                    fields.insert("movieFile".into(), movie_file_resource(&ctx, file, movie, false));
                }
                decorate(
                    &mut resource,
                    movie.id,
                    &movie.metadata,
                    translations,
                    &naming,
                    &url_base,
                    is_excluded,
                );
                out.push(resource);
            }
            Found::New(info) => {
                let mut resource = unadded_resource(&ctx, info, language_id);
                let translations: Vec<(i64, String)> = info
                    .translations
                    .iter()
                    .filter_map(|t| Some((t.language_id, t.title.clone()?)))
                    .collect();
                decorate(
                    &mut resource,
                    0,
                    &info.metadata,
                    &translations,
                    &naming,
                    &url_base,
                    is_excluded,
                );
                out.push(resource);
            }
        }
    }
    Ok(Json(out))
}

/// `GET /movie/lookup/tmdb?tmdbId=`: the provider's movie, whether or not
/// the library has it, without what the search adds to its results.
pub async fn lookup_tmdb(State(state): State<AppState>, uri: Uri) -> ApiResult<Json<Value>> {
    let tmdb_id = Params::from_uri(&uri).i64("tmdbId").unwrap_or(0);
    let provider = state.commands()?.movie_provider();
    let map = refresh::map_context(state.db(), state.now()).await?;
    let info = provider.movie_info(tmdb_id, &map).await.map_err(lookup_error)?;
    let ctx = MovieContext::load(&state).await?;
    let language_id = info_language(&state).await?;
    Ok(Json(unadded_resource(&ctx, &info, language_id)))
}

/// `GET /movie/lookup/imdb?imdbId=`.
pub async fn lookup_imdb(State(state): State<AppState>, uri: Uri) -> ApiResult<Json<Value>> {
    let params = Params::from_uri(&uri);
    let imdb_id = unarr_core::parser::title::normalize_imdb_id(params.get("imdbId").unwrap_or(""));
    let provider = state.commands()?.movie_provider();
    let map = refresh::map_context(state.db(), state.now()).await?;
    let info = match imdb_id {
        Some(imdb_id) => provider
            .movie_by_imdb_id(&imdb_id, &map)
            .await
            .map_err(lookup_error)?,
        None => None,
    };
    // A movie the provider does not list is a null the original
    // dereferences: a 500.
    let info = info.ok_or_else(|| ApiError::ServerException {
        message: "Object reference not set to an instance of an object.".into(),
        exception: "System.NullReferenceException".into(),
    })?;
    let ctx = MovieContext::load(&state).await?;
    let language_id = info_language(&state).await?;
    Ok(Json(unadded_resource(&ctx, &info, language_id)))
}

// --- adding a movie

fn write_error(e: WriteError) -> ApiError {
    match e {
        WriteError::Invalid(f) => ApiError::Validation(f),
        WriteError::InvalidPlain(f) => ApiError::ValidationPlain(f),
        WriteError::NullReference => ApiError::ServerException {
            message: "Object reference not set to an instance of an object.".into(),
            exception: "System.NullReferenceException".into(),
        },
        WriteError::Other(e) => ApiError::Internal(e),
    }
}

fn text(body: &Value, key: &str) -> Option<String> {
    body[key].as_str().map(str::to_string)
}

fn ints(v: &Value) -> Vec<i64> {
    v.as_array()
        .map(|a| a.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default()
}

/// `MinimumAvailability` as a client sends it: the enumeration's name in
/// any case, or its number; `tba` when it says nothing.
pub(crate) fn minimum_availability(v: &Value) -> MovieStatus {
    match v {
        Value::String(name) => [
            MovieStatus::Tba,
            MovieStatus::Announced,
            MovieStatus::InCinemas,
            MovieStatus::Released,
            MovieStatus::Deleted,
        ]
        .into_iter()
        .find(|s| s.as_str().eq_ignore_ascii_case(name))
        .unwrap_or(MovieStatus::Tba),
        Value::Number(n) => n
            .as_i64()
            .and_then(MovieStatus::from_number)
            .unwrap_or(MovieStatus::Tba),
        _ => MovieStatus::Tba,
    }
}

/// `MovieResource.ToModel`, for what an add reads of it.
pub(crate) fn new_movie(body: &Value, method: &str) -> NewMovie {
    NewMovie {
        tmdb_id: body["tmdbId"].as_i64().unwrap_or(0),
        title: text(body, "title"),
        path: text(body, "path"),
        root_folder_path: text(body, "rootFolderPath"),
        quality_profile_id: body["qualityProfileId"].as_i64().unwrap_or(0),
        monitored: body["monitored"].as_bool().unwrap_or(false),
        minimum_availability: minimum_availability(&body["minimumAvailability"]),
        tags: ints(&body["tags"]),
        add_options: body
            .get("addOptions")
            .filter(|o| o.is_object())
            .map(|o| library::add_options(Some(o), method)),
    }
}

/// The movie as `GET /movie/{id}` answers it.
pub(crate) async fn movie_by_id(state: &AppState, id: i64) -> ApiResult<Value> {
    let ctx = MovieContext::load(state).await?;
    let (movie, titles, file) = state
        .db()
        .call(move |conn| {
            let Some(movie) = movies::get_movie(conn, id)? else {
                return Ok(None);
            };
            let titles = movies::alternative_titles_of(conn, movie.metadata.id)?;
            let file = movies::get_movie_file(conn, movie.movie_file_id)?;
            Ok(Some((movie, titles, file)))
        })
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(movie_resource(
        &ctx,
        &movie,
        &MovieParts {
            alternative_titles: &titles,
            file: file.as_ref(),
            covers: Covers::Local,
            root_folder: true,
            statistics: true,
            movie_file: FileShown::Plain,
        },
    ))
}

/// `POST /movie`: the controller's validation, the provider's metadata,
/// the row; the refresh that brings the rest is queued.
pub async fn add(State(state): State<AppState>, Json(body): Json<Value>) -> ApiResult<Response> {
    if !body.is_object() {
        return Err(ApiError::BadRequest(
            "The request body must be a JSON object".into(),
        ));
    }
    let new = new_movie(&body, "manual");
    let (check, year) = (new.clone(), body["year"].as_i64().unwrap_or(0));
    let failures = state
        .db()
        .call(move |conn| Ok(library::validate_post(conn, &check, year)))
        .await?
        .map_err(write_error)?;
    if !failures.is_empty() {
        return Err(ApiError::Validation(failures));
    }
    let runtime = state.commands()?;
    let provider = runtime.movie_provider();
    let map = refresh::map_context(state.db(), state.now()).await?;
    let added = library::add(state.db(), provider.as_ref(), &map, new)
        .await
        .map_err(write_error)?;
    // `MovieAddedEvent`.
    runtime.movies_added(&[added.id]).await;
    let resource = movie_by_id(&state, added.id).await?;
    let location = format!("{}/api/v3/movie/{}", state.settings().url_base, added.id);
    Ok((
        StatusCode::CREATED,
        [(header::LOCATION, location)],
        Json(resource),
    )
        .into_response())
}

// --- editing, moving and deleting

fn accepted(body: Value) -> Response {
    (StatusCode::ACCEPTED, Json(body)).into_response()
}

/// `PUT /movie/{id}[?moveFiles=true]`: the id the body carries is the one
/// that counts. With `moveFiles` the folder follows the new path, in a
/// command of its own.
pub async fn update(
    State(state): State<AppState>,
    Path(_route_id): Path<i64>,
    uri: Uri,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    if !body.is_object() {
        return Err(ApiError::BadRequest(
            "The request body must be a JSON object".into(),
        ));
    }
    let move_files = Params::from_uri(&uri).bool("moveFiles", false);
    let edit = MovieEdit {
        id: body["id"].as_i64().unwrap_or(0),
        path: text(&body, "path"),
        quality_profile_id: body["qualityProfileId"].as_i64().unwrap_or(0),
        monitored: body["monitored"].as_bool().unwrap_or(false),
        minimum_availability: minimum_availability(&body["minimumAvailability"]),
        tags: ints(&body["tags"]),
        add_options: body
            .get("addOptions")
            .filter(|o| o.is_object())
            .map(|o| library::add_options(Some(o), "manual")),
    };
    let check = edit.clone();
    let failures = state
        .db()
        .call(move |conn| library::validate_put(conn, &check))
        .await?;
    if !failures.is_empty() {
        return Err(ApiError::Validation(failures));
    }
    let id = edit.id;
    let (before, after) = state
        .db()
        .call(move |conn| library::update(conn, &edit))
        .await?
        .ok_or_else(|| ApiError::ModelNotFound(format!("Movie with ID {id} does not exist")))?;
    let runtime = state.commands()?;
    if move_files {
        runtime
            .queue_movie_move(id, before.path.clone(), after.path.clone())
            .await?;
    }
    // `MovieEditedEvent`, then the controller's own broadcast.
    runtime.movie_edited(id);
    runtime.movie_edited(id);
    Ok(accepted(movie_by_id(&state, id).await?))
}

/// The answer for a movie that is not there (`ModelNotFoundException`).
fn no_such_movie(id: i64) -> ApiError {
    ApiError::ModelNotFound(format!("Movie with ID {id} does not exist"))
}

async fn delete_many(state: &AppState, ids: Vec<i64>, delete_files: bool, exclude: bool) -> ApiResult<()> {
    let deleted = state
        .db()
        .call(move |conn| library::delete(conn, &ids, exclude))
        .await?
        .map_err(no_such_movie)?;
    state.commands()?.movies_deleted(deleted, delete_files).await;
    Ok(())
}

/// `DELETE /movie/{id}?deleteFiles=&addImportExclusion=`.
pub async fn delete(State(state): State<AppState>, Path(id): Path<i64>, uri: Uri) -> ApiResult<Response> {
    let params = Params::from_uri(&uri);
    delete_many(
        &state,
        vec![id],
        params.bool("deleteFiles", false),
        params.bool("addImportExclusion", false),
    )
    .await?;
    Ok(StatusCode::OK.into_response())
}

/// `ApplyTags` as a client sends it: the name in any case, or the number.
fn apply_tags(v: &Value) -> ApplyTags {
    match v {
        Value::String(name) => match name.to_lowercase().as_str() {
            "remove" => ApplyTags::Remove,
            "replace" => ApplyTags::Replace,
            _ => ApplyTags::Add,
        },
        Value::Number(n) => match n.as_i64() {
            Some(1) => ApplyTags::Remove,
            Some(2) => ApplyTags::Replace,
            _ => ApplyTags::Add,
        },
        _ => ApplyTags::Add,
    }
}

/// `PUT /movie/editor`: the movies of the index changed together.
pub async fn editor_update(State(state): State<AppState>, Json(body): Json<Value>) -> ApiResult<Response> {
    let edit = BulkEdit {
        movie_ids: ints(&body["movieIds"]),
        monitored: body["monitored"].as_bool(),
        quality_profile_id: body["qualityProfileId"].as_i64(),
        minimum_availability: body
            .get("minimumAvailability")
            .filter(|v| !v.is_null())
            .map(minimum_availability),
        root_folder_path: text(&body, "rootFolderPath"),
        tags: body["tags"].is_array().then(|| ints(&body["tags"])),
        apply_tags: apply_tags(&body["applyTags"]),
        move_files: body["moveFiles"].as_bool().unwrap_or(false),
    };
    let root = edit.root_folder_path.clone().filter(|r| !r.trim().is_empty());
    let move_files = edit.move_files;
    let (updated, moves) = state
        .db()
        .call(move |conn| Ok(library::update_many(conn, &edit)))
        .await?
        .map_err(write_error)?;
    let runtime = state.commands()?;
    if let (true, Some(root), false) = (move_files, &root, moves.is_empty()) {
        runtime.queue_bulk_movie_move(root.clone(), moves).await?;
    }
    // `MoviesBulkEditedEvent`.
    for movie in &updated {
        runtime.movie_edited(movie.id);
    }
    let ctx = MovieContext::load(&state).await?;
    let ids: Vec<i64> = updated.iter().map(|m| m.metadata.id).collect();
    let files: Vec<i64> = updated.iter().map(|m| m.movie_file_id).collect();
    let (titles, files) = state
        .db()
        .call(move |conn| {
            let mut titles = std::collections::HashMap::new();
            for id in ids {
                titles.insert(id, movies::alternative_titles_of(conn, id)?);
            }
            let files: std::collections::HashMap<i64, _> = movies::movie_files_by_ids(conn, &files)?
                .into_iter()
                .map(|f| (f.id, f))
                .collect();
            Ok((titles, files))
        })
        .await?;
    let resources: Vec<Value> = updated
        .iter()
        .map(|movie| {
            let mut resource = movie_resource(
                &ctx,
                movie,
                &MovieParts {
                    alternative_titles: titles.get(&movie.metadata.id).map(Vec::as_slice).unwrap_or(&[]),
                    file: None,
                    covers: Covers::Local,
                    root_folder: false,
                    statistics: false,
                    movie_file: FileShown::No,
                },
            );
            if let Some(fields) = resource.as_object_mut() {
                if let Some(file) = files.get(&movie.movie_file_id) {
                    fields.insert("movieFile".into(), movie_file_resource(&ctx, file, movie, false));
                }
                // The root folder the editor set on the model, when it
                // set one.
                if let Some(root) = &root {
                    fields.insert("rootFolderPath".into(), json!(root));
                }
            }
            resource
        })
        .collect();
    Ok(accepted(json!(resources)))
}

/// `DELETE /movie/editor`.
pub async fn editor_delete(State(state): State<AppState>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    delete_many(
        &state,
        ints(&body["movieIds"]),
        body["deleteFiles"].as_bool().unwrap_or(false),
        body["addImportExclusion"].as_bool().unwrap_or(false),
    )
    .await?;
    Ok(Json(json!({})))
}

/// `POST /movie/import`: Library Import. The movies are added together,
/// or none is; what the answer shows of each is the movie alone.
pub async fn import(State(state): State<AppState>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let Some(items) = body.as_array() else {
        return Err(ApiError::BadRequest(
            "The request body must be a JSON array".into(),
        ));
    };
    let new: Vec<NewMovie> = items.iter().map(|item| new_movie(item, "manual")).collect();
    // A movie the library has is the database's own refusal: the
    // original inserts and the unique index says no.
    let wanted: Vec<i64> = new.iter().map(|m| m.tmdb_id).collect();
    let taken = state
        .db()
        .call(move |conn| {
            let existing = store::all_movie_tmdb_ids(conn)?;
            let mut seen = std::collections::HashSet::new();
            Ok(wanted.iter().any(|id| existing.contains(id) || !seen.insert(*id)))
        })
        .await?;
    let runtime = state.commands()?;
    let provider = runtime.movie_provider();
    let map = refresh::map_context(state.db(), state.now()).await?;
    if taken {
        // The provider is asked first, as in the original, so that a
        // movie it does not know is refused before the insert is tried.
        for movie in &new {
            if let Err(MovieMetadataError::MovieNotFound(_)) = provider.movie_info(movie.tmdb_id, &map).await
            {
                return Err(write_error(WriteError::InvalidPlain(vec![
                    unarr_core::validation::ValidationFailure::setting(
                        "TmdbId",
                        &format!(
                            "A movie with this ID was not found. Path: {}",
                            movie.path.as_deref().unwrap_or("")
                        ),
                        json!(movie.tmdb_id),
                    ),
                ])));
            }
        }
        return Err(ApiError::ConflictMessage(
            "constraint failed\nUNIQUE constraint failed: Movies.MovieMetadataId".into(),
        ));
    }
    let added = library::add_many(state.db(), provider.as_ref(), &map, new, false)
        .await
        .map_err(write_error)?;
    // `MoviesImportedEvent`.
    let ids: Vec<i64> = added.iter().map(|a| a.movie.id).collect();
    runtime.movies_added(&ids).await;
    let ctx = MovieContext::load(&state).await?;
    // The model as the service holds it: the provider's alternative
    // titles, which are no rows yet.
    let resources: Vec<Value> = added
        .iter()
        .zip(items.iter())
        .map(|(one, sent)| {
            let mut resource = movie_resource(
                &ctx,
                &one.movie,
                &MovieParts {
                    alternative_titles: &one.alternative_titles,
                    file: None,
                    covers: Covers::Remote,
                    root_folder: false,
                    statistics: false,
                    movie_file: FileShown::No,
                },
            );
            // `MovieResource.RootFolderPath` is the model's own, which
            // the service keeps from the request: the page that sent one
            // reads it back, the one that sent none sees no key.
            if let Some(root) = sent["rootFolderPath"].as_str().filter(|r| !r.trim().is_empty()) {
                resource["rootFolderPath"] = json!(root);
            }
            if let Some(titles) = resource["alternateTitles"].as_array_mut() {
                for title in titles {
                    if let Some(title) = title.as_object_mut() {
                        title.remove("id");
                    }
                }
            }
            resource
        })
        .collect();
    Ok(Json(json!(resources)))
}
