//! `manualimport` and `parse`, after Radarr's `ManualImportController` and
//! `ParseController`.

use std::collections::HashMap;

use axum::extract::State;
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Map, Value};
use unarr_core::commands::{language_ids_from_json, quality_model_from_json};
use unarr_core::customformat::{self, Input, MovieFacts};
use unarr_core::downloads::queue::hash_int31;
use unarr_core::model::{QualityModel, ReleaseType};
use unarr_core::movies::decision::engine::aggregate_languages;
use unarr_core::movies::decision::{mapping, RemoteMovie, StoreMovieLookup};
use unarr_core::movies::import::decide::MovieEnvironment;
use unarr_core::movies::import::manual::{
    self, ManualImportError, ManualImportItem, MovieSource, ReprocessRequest,
};
use unarr_core::movies::model::{AlternativeTitle, Movie, MovieFile};
use unarr_core::movies::parser::{parse_movie_title, ParsedMovieInfo};
use unarr_core::movies::quality::{modifier_number, source_number, MovieQuality};
use unarr_core::movies::store as movies;

use super::resources::{
    formats_resource, languages_resource, movie_resource, quality_resource, Covers, FileShown, MovieContext,
    MovieParts,
};
use crate::blocking;
use crate::error::{ApiError, ApiResult};
use crate::query::Params;
use crate::state::AppState;

async fn environment(state: &AppState) -> ApiResult<MovieEnvironment> {
    let now = state.now();
    Ok(state
        .db()
        .call(move |conn| MovieEnvironment::load(conn, now))
        .await?)
}

fn manual_error(e: ManualImportError) -> ApiError {
    match e {
        ManualImportError::MovieNotFound(_) => ApiError::ModelNotFound(e.to_string()),
        ManualImportError::Augmenting(message) => ApiError::ServerException {
            message,
            exception: "NzbDrone.Core.MediaFiles.MovieImport.Aggregation.AugmentingFailedException".into(),
        },
        ManualImportError::Other(e) => ApiError::Internal(e),
    }
}

/// `AddQualityWeight`: the definition's weight plus the revision.
fn quality_weight(q: &QualityModel) -> i64 {
    MovieQuality::weight(q.quality_id) + q.real * 10 + q.version
}

fn rejections_json(item: &ManualImportItem) -> Value {
    Value::Array(
        item.rejections
            .iter()
            .map(|r| json!({"reason": r.message, "type": "permanent"}))
            .collect(),
    )
}

/// What the movies of the items of one answer show beyond themselves:
/// their alternative titles, by metadata id, and their files, by id.
struct Shown {
    ctx: MovieContext,
    titles: HashMap<i64, Vec<AlternativeTitle>>,
    files: HashMap<i64, MovieFile>,
}

impl Shown {
    /// `availability_delay` is the one `ToResource` is given: the
    /// setting's for a parsed release, none for a manual import.
    async fn load(state: &AppState, shown: Vec<Movie>, availability_delay: Option<i64>) -> ApiResult<Shown> {
        let mut ctx = MovieContext::load(state).await?;
        if let Some(delay) = availability_delay {
            ctx.availability_delay = delay;
        }
        let (titles, files) = state
            .db()
            .call(move |conn| {
                let mut titles = HashMap::new();
                for movie in &shown {
                    // Two hundred files of one movie are one movie.
                    if titles.contains_key(&movie.metadata.id) {
                        continue;
                    }
                    titles.insert(
                        movie.metadata.id,
                        movies::alternative_titles_of(conn, movie.metadata.id)?,
                    );
                }
                let file_ids: Vec<i64> = shown
                    .iter()
                    .map(|m| m.movie_file_id)
                    .filter(|id| *id > 0)
                    .collect();
                let files = movies::movie_files_by_ids(conn, &file_ids)?
                    .into_iter()
                    .map(|f| (f.id, f))
                    .collect();
                Ok((titles, files))
            })
            .await?;
        Ok(Shown { ctx, titles, files })
    }

    /// `movie.ToResource(0)`: the movie as the service holds it, with its
    /// file. One read by its id carries its alternative titles, one a
    /// title found does not.
    fn movie(&self, movie: &Movie, with_titles: bool) -> Value {
        let titles = self
            .titles
            .get(&movie.metadata.id)
            .filter(|_| with_titles)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        movie_resource(
            &self.ctx,
            movie,
            &MovieParts {
                alternative_titles: titles,
                file: self.files.get(&movie.movie_file_id),
                covers: Covers::Remote,
                root_folder: false,
                statistics: false,
                movie_file: FileShown::Plain,
            },
        )
    }
}

fn with_titles(source: MovieSource) -> bool {
    match source {
        MovieSource::Id => true,
        MovieSource::Title => false,
        MovieSource::Download { by_id } => by_id,
    }
}

/// `ManualImportResource`, nulls left out.
fn manual_import_resource(env: &MovieEnvironment, shown: &Shown, item: &ManualImportItem) -> Value {
    let mut m = Map::new();
    m.insert("id".into(), json!(hash_int31(&item.path)));
    m.insert("path".into(), json!(item.path));
    m.insert("relativePath".into(), json!(item.relative_path));
    if let Some(folder) = &item.folder_name {
        m.insert("folderName".into(), json!(folder));
    }
    if let Some(name) = &item.name {
        m.insert("name".into(), json!(name));
    }
    m.insert("size".into(), json!(item.size));
    if let Some(movie) = &item.movie {
        m.insert("movie".into(), shown.movie(movie, with_titles(item.movie_source)));
    }
    if let Some(id) = item.movie_file_id {
        m.insert("movieFileId".into(), json!(id));
    }
    if let Some(group) = &item.release_group {
        m.insert("releaseGroup".into(), json!(group));
    }
    if let Some(q) = &item.quality {
        m.insert("quality".into(), quality_resource(q));
    }
    if let Some(languages) = &item.languages {
        m.insert("languages".into(), json!(languages_resource(languages)));
    }
    m.insert(
        "qualityWeight".into(),
        json!(item.quality.as_ref().map(quality_weight).unwrap_or(0)),
    );
    if let Some(id) = &item.download_id {
        m.insert("downloadId".into(), json!(id));
    }
    m.insert(
        "customFormats".into(),
        json!(formats_resource(&item.custom_formats)),
    );
    m.insert(
        "customFormatScore".into(),
        json!(item.custom_format_score(&env.catalog)),
    );
    m.insert("indexerFlags".into(), json!(item.indexer_flags));
    m.insert("rejections".into(), rejections_json(item));
    Value::Object(m)
}

/// `GET /manualimport`: the files of a movie, of a download or of a folder.
pub async fn list(State(state): State<AppState>, uri: Uri) -> ApiResult<Json<Value>> {
    let p = Params::from_uri(&uri);
    let env = environment(&state).await?;
    let movie_id = p.i64("movieId");
    let folder = p.get("folder").map(str::to_string);
    let download_id = p.get("downloadId").map(str::to_string);
    let filter_existing = p.bool("filterExistingFiles", true);
    let (db, downloads, env2) = (state.db().clone(), state.movie_downloads().clone(), env.clone());
    let items = blocking::run(move || match movie_id {
        Some(id) if download_id.as_deref().is_none_or(|d| d.trim().is_empty()) => {
            manual::get_media_files_for_movie(&db, &env2, id)
        }
        _ => manual::get_media_files(
            &db,
            &env2,
            &downloads,
            folder.as_deref(),
            download_id.as_deref(),
            movie_id,
            filter_existing,
        ),
    })
    .await?
    .map_err(manual_error)?;
    let shown = Shown::load(
        &state,
        items.iter().filter_map(|i| i.movie.clone()).collect(),
        Some(0),
    )
    .await?;
    Ok(Json(Value::Array(
        items
            .iter()
            .map(|i| manual_import_resource(&env, &shown, i))
            .collect(),
    )))
}

/// `POST /manualimport`: `ReprocessItems`. Each item posted comes back
/// with what evaluating it again found; what the user chose stays.
pub async fn reprocess(State(state): State<AppState>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let items = body.as_array().cloned().unwrap_or_default();
    if body.is_array() && items.is_empty() {
        return Err(ApiError::BadRequest("items must be provided".into()));
    }
    let env = environment(&state).await?;
    let mut out = Vec::new();
    for item in items {
        let text = |key: &str| item[key].as_str().map(str::to_string);
        let given_quality = item["quality"]
            .is_object()
            .then(|| quality_model_from_json(&item["quality"]));
        let given_languages = item["languages"]
            .is_array()
            .then(|| language_ids_from_json(&item["languages"]));
        let req = ReprocessRequest {
            path: text("path").unwrap_or_default(),
            download_id: text("downloadId"),
            movie_id: item["movieId"].as_i64().unwrap_or(0),
            release_group: text("releaseGroup"),
            quality: given_quality,
            languages: given_languages.clone(),
            indexer_flags: item["indexerFlags"].as_i64().unwrap_or(0),
        };
        let (db, downloads, env2, req2) = (
            state.db().clone(),
            state.movie_downloads().clone(),
            env.clone(),
            req.clone(),
        );
        let processed = blocking::run(move || manual::reprocess_item(&db, &env2, &downloads, &req2))
            .await?
            .map_err(manual_error)?;
        let shown = Shown::load(&state, processed.movie.iter().cloned().collect(), Some(0)).await?;
        let mut m = Map::new();
        if let Some(id) = item["id"].as_i64().filter(|id| *id != 0) {
            m.insert("id".into(), json!(id));
        }
        m.insert("path".into(), json!(req.path));
        m.insert("movieId".into(), json!(req.movie_id));
        if let Some(movie) = &processed.movie {
            m.insert(
                "movie".into(),
                shown.movie(movie, with_titles(processed.movie_source)),
            );
        }
        // The quality and the languages are taken from the evaluation only
        // where the request left them unknown.
        let quality = match given_quality {
            Some(q) if q.quality_id == 0 => processed.quality,
            given => given,
        };
        if let Some(q) = &quality {
            m.insert("quality".into(), quality_resource(q));
        }
        let processed_languages = processed.languages.clone().unwrap_or_default();
        let languages = match given_languages {
            Some(given)
                if given.len() <= 1
                    && given.first().copied().unwrap_or(0) == 0
                    && !processed_languages.is_empty() =>
            {
                Some(processed_languages)
            }
            given => given,
        };
        if let Some(languages) = &languages {
            m.insert("languages".into(), json!(languages_resource(languages)));
        }
        let release_group = match req.release_group.as_deref() {
            Some(group) if !group.trim().is_empty() => Some(group.to_string()),
            _ => processed.release_group.clone(),
        };
        if let Some(group) = release_group {
            m.insert("releaseGroup".into(), json!(group));
        }
        if let Some(id) = &req.download_id {
            m.insert("downloadId".into(), json!(id));
        }
        m.insert(
            "customFormats".into(),
            json!(formats_resource(&processed.custom_formats)),
        );
        m.insert(
            "customFormatScore".into(),
            json!(processed.custom_format_score(&env.catalog)),
        );
        m.insert("indexerFlags".into(), json!(processed.indexer_flags));
        m.insert("rejections".into(), rejections_json(&processed));
        out.push(Value::Object(m));
    }
    Ok(Json(Value::Array(out)))
}

// --- parse

/// `ParsedMovieInfo` as the API shows it, nulls left out.
fn parsed_info_json(parsed: &ParsedMovieInfo) -> Value {
    let mut m = Map::new();
    m.insert("movieTitles".into(), json!(parsed.movie_titles));
    m.insert("originalTitle".into(), json!(parsed.original_title));
    m.insert("releaseTitle".into(), json!(parsed.release_title));
    m.insert("simpleReleaseTitle".into(), json!(parsed.simple_release_title));
    m.insert("quality".into(), quality_resource(&parsed.quality.quality));
    m.insert("languages".into(), json!(languages_resource(&parsed.languages)));
    if let Some(group) = &parsed.release_group {
        m.insert("releaseGroup".into(), json!(group));
    }
    m.insert("releaseHash".into(), json!(parsed.release_hash));
    m.insert("edition".into(), json!(parsed.edition));
    m.insert("year".into(), json!(parsed.year));
    m.insert("imdbId".into(), json!(parsed.imdb_id));
    m.insert("tmdbId".into(), json!(parsed.tmdb_id));
    if let Some(subs) = &parsed.hardcoded_subs {
        m.insert("hardcodedSubs".into(), json!(subs));
    }
    if let Some(title) = parsed.movie_titles.first() {
        m.insert("movieTitle".into(), json!(title));
        m.insert("primaryMovieTitle".into(), json!(title));
    }
    Value::Object(m)
}

/// `GET /parse?title=`: what a release name parses to and the movie of
/// the library it is about. Nothing without a title; a `path` alone is
/// not read.
pub async fn parse(State(state): State<AppState>, uri: Uri) -> ApiResult<Response> {
    let p = Params::from_uri(&uri);
    let Some(title) = p
        .get("title")
        .map(str::to_string)
        .filter(|t| !t.trim().is_empty())
    else {
        return Ok(StatusCode::NO_CONTENT.into_response());
    };
    let Some(parsed) = parse_movie_title(&title, false) else {
        return Ok(Json(json!({"title": title, "customFormatScore": 0})).into_response());
    };
    let env = environment(&state).await?;
    // `ParsingService.Map(parsedMovieInfo, "", 0)`.
    let parsed2 = parsed.clone();
    let found = state
        .db()
        .call(move |conn| {
            let lookup = StoreMovieLookup { conn };
            Ok(mapping::find_movie(&lookup, &parsed2, "", 0, None))
        })
        .await?
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("{e}")))?;
    let mut remote = RemoteMovie::new(Default::default());
    remote.languages = parsed.languages.clone();
    remote.parsed = Some(parsed.clone());
    if let Some((movie, match_type)) = found {
        remote.match_type = match_type;
        // `RemoteMovieAggregationService.Augment`; without a movie the
        // parsed languages stand.
        remote.languages = aggregate_languages(&env.catalog, &remote, &movie);
        remote.movie = Some(movie);
    }
    // `ParseCustomFormat(remoteMovie, 0)`: the conditions about a movie
    // read the release's year when it has no movie.
    let quality = parsed.quality.quality;
    let definition = MovieQuality::by_id(quality.quality_id);
    let input = Input {
        release_title: &parsed.simple_release_title,
        filename: None,
        release_group: parsed.release_group.as_deref(),
        quality,
        languages: &remote.languages,
        size: 0,
        indexer_flags: 0,
        release_type: ReleaseType::Unknown,
        series_original_language: remote
            .movie
            .as_ref()
            .map(|m| m.metadata.original_language_id)
            .unwrap_or(0),
        movie: Some(MovieFacts {
            source: source_number(definition.source),
            resolution: definition.resolution,
            modifier: modifier_number(definition.modifier),
            edition: Some(parsed.edition.as_str()),
            year: remote
                .movie
                .as_ref()
                .map(|m| m.metadata.year)
                .unwrap_or(parsed.year),
        }),
    };
    let formats: Vec<_> = customformat::evaluate(&env.catalog.base.formats, &input)
        .into_iter()
        .cloned()
        .collect();
    let score = remote
        .movie
        .as_ref()
        .and_then(|m| env.catalog.base.profile(m.quality_profile_id))
        .map(|p| unarr_core::decision::upgradable::format_score(p, &formats))
        .unwrap_or(0);
    let mut m = Map::new();
    m.insert("title".into(), json!(title));
    m.insert("parsedMovieInfo".into(), parsed_info_json(&parsed));
    if let Some(movie) = &remote.movie {
        // The movie a title found: without its alternative titles.
        let shown = Shown::load(&state, vec![movie.clone()], None).await?;
        m.insert("movie".into(), shown.movie(movie, false));
    }
    m.insert("languages".into(), json!(languages_resource(&remote.languages)));
    m.insert("customFormats".into(), json!(formats_resource(&formats)));
    m.insert("customFormatScore".into(), json!(score));
    Ok(Json(Value::Object(m)).into_response())
}
