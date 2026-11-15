//! Radarr's `/api/v3/release`: an interactive search for a movie, the RSS
//! feed as a list, the grab of a listed release (`ReleaseController`) and
//! a release pushed by another application (`ReleasePushController`).

use axum::extract::State;
use axum::http::Uri;
use axum::Json;
use chrono::{DateTime, Utc};
use serde_json::{json, Map, Value};
use unarr_core::downloads::GrabError;
use unarr_core::indexers::rss::ReleaseInfo;
use unarr_core::model::{Protocol, ProviderKind, QualityModel};
use unarr_core::movies::decision::{self, MovieCatalog, MovieDecision, StoreMovieLookup};
use unarr_core::movies::quality::{indexer_flag_names, MovieQuality};
use unarr_core::movies::search::MovieSearchError;
use unarr_core::validation::ValidationFailure;

use super::resources::{formats_resource, languages_resource, quality_resource};
use crate::error::{ApiError, ApiResult};
use crate::query::Params;
use crate::state::AppState;

/// `GET /release?movieId=` | nothing (RSS).
pub async fn list(State(state): State<AppState>, uri: Uri) -> ApiResult<Json<Vec<Value>>> {
    let params = Params::from_uri(&uri);
    let now = state.now();
    let search = state.movie_search();
    let decisions = match params.i64("movieId") {
        Some(movie_id) => search
            .movie_search(movie_id, true, true, now)
            .await
            .map_err(|e| match e {
                MovieSearchError::MovieNotFound(id) => {
                    ApiError::ServerMessage(format!("Movie with ID {id} does not exist"))
                }
                MovieSearchError::Other(e) => {
                    tracing::error!(error = %e, "Movie search failed");
                    ApiError::ServerMessage(e.to_string())
                }
            })?,
        None => search.rss_decisions(now).await?,
    };
    let prioritized = search.prioritize(decisions, now).await?;
    Ok(Json(map_decisions(&state, &prioritized, now)))
}

/// `MapDecisions`: resources in order, cached for a later grab.
pub fn map_decisions(state: &AppState, decisions: &[MovieDecision], now: DateTime<Utc>) -> Vec<Value> {
    decisions
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let key = format!("{}_{}", d.remote.release.indexer_id, d.remote.release.guid);
            state.cache_movie_release(key, d.remote.clone());
            release_resource(d, i as i64, now)
        })
        .collect()
}

/// `ReleaseResource` as `ToResource` and `MapDecision` fill it. Nulls are
/// left out, as the original's serializer does.
pub fn release_resource(decision: &MovieDecision, release_weight: i64, now: DateTime<Utc>) -> Value {
    let remote = &decision.remote;
    let release = &remote.release;
    let mut m = Map::new();
    let mut set = |key: &str, value: Value| {
        if !value.is_null() {
            m.insert(key.to_string(), value);
        }
    };
    let age = now - release.publish_date;
    set("guid", json!(release.guid));
    if let Some(parsed) = &remote.parsed {
        let quality = &parsed.quality.quality;
        set("quality", quality_resource(quality));
        // The weight of the quality in a profile of every quality, the
        // revision breaking ties.
        let index = (MovieQuality::weight(quality.quality_id) - 1).max(0);
        set(
            "qualityWeight",
            json!(index * 100 + quality.real * 10 + quality.version),
        );
        set("releaseGroup", json!(parsed.release_group));
        set("releaseHash", json!(parsed.release_hash));
        set("movieTitles", json!(parsed.movie_titles));
        set("edition", json!(parsed.edition));
    } else {
        set("qualityWeight", json!(0));
    }
    set("customFormats", json!(formats_resource(&remote.custom_formats)));
    set("customFormatScore", json!(remote.custom_format_score));
    set("age", json!(age.num_days()));
    set("ageHours", json!(age.num_milliseconds() as f64 / 3_600_000.0));
    set("ageMinutes", json!(age.num_milliseconds() as f64 / 60_000.0));
    set("size", json!(release.size));
    set("indexerId", json!(release.indexer_id));
    set("indexer", json!(release.indexer));
    set("title", json!(release.title));
    set("sceneSource", json!(false));
    set("languages", json!(languages_resource(&remote.languages)));
    set("mappedMovieId", json!(remote.movie.as_ref().map(|m| m.id)));
    set("approved", json!(decision.approved()));
    set("temporarilyRejected", json!(decision.temporarily_rejected()));
    set("rejected", json!(decision.rejected()));
    set("tmdbId", json!(release.tmdb_id));
    // The feed's number, as Radarr keeps it.
    set(
        "imdbId",
        json!(release
            .imdb_id
            .as_deref()
            .and_then(|id| id.trim_start_matches("tt").parse::<i64>().ok())
            .unwrap_or(0)),
    );
    set(
        "rejections",
        json!(decision
            .rejections
            .iter()
            .map(|r| r.message.as_str())
            .collect::<Vec<_>>()),
    );
    set(
        "publishDate",
        json!(unarr_core::time::format_utc(release.publish_date)),
    );
    set("commentUrl", json!(release.comment_url));
    set("downloadUrl", json!(release.download_url));
    set("infoUrl", json!(release.info_url));
    set("movieRequested", json!(remote.movie_requested));
    set("downloadAllowed", json!(remote.download_allowed));
    if release.protocol == Protocol::Torrent {
        set("magnetUrl", json!(release.magnet_url.clone().unwrap_or_default()));
        set("infoHash", json!(release.info_hash));
        set("seeders", json!(release.seeders));
        set(
            "leechers",
            json!(match (release.peers, release.seeders) {
                (Some(peers), Some(seeders)) => Some(peers - seeders),
                _ => None,
            }),
        );
    }
    set("protocol", json!(release.protocol.as_str()));
    set("indexerFlags", json!(indexer_flag_names(release.indexer_flags)));
    set("releaseWeight", json!(release_weight));
    Value::Object(m)
}

/// What a grab can fail with, as the API answers it.
pub(crate) fn grab_error(e: GrabError) -> ApiError {
    match e {
        GrabError::ReleaseDownload(message) | GrabError::ReleaseUnavailable(message) => {
            tracing::error!(error = %message, "getting the release from the indexer failed");
            ApiError::ConflictMessage("Getting release from indexer failed".into())
        }
        GrabError::NoClient(message) => ApiError::Internal(anyhow::anyhow!("{message}")),
        GrabError::Client(e) => ApiError::Internal(e.into_anyhow()),
        GrabError::Other(e) => ApiError::Internal(e),
    }
}

/// `POST /release`: a release a search listed, sent to a download client.
pub async fn grab(State(state): State<AppState>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let mut failures = Vec::new();
    let indexer_id = body["indexerId"].as_i64().unwrap_or(0);
    if indexer_id <= 0 {
        failures.push(ValidationFailure::greater_than("IndexerId", json!(indexer_id), 0));
    }
    let guid = body["guid"].as_str().unwrap_or("");
    if guid.trim().is_empty() {
        failures.push(ValidationFailure::not_empty("Guid", body["guid"].clone()));
    }
    if !failures.is_empty() {
        return Err(ApiError::Validation(failures));
    }
    let Some(mut remote) = state.cached_movie_release(&format!("{indexer_id}_{guid}")) else {
        return Err(ApiError::NotFoundMessage(
            "Couldn't find requested release in cache, try searching again".into(),
        ));
    };
    let movie_id = body["movieId"].as_i64();
    let get_movie = |id: i64| {
        let state = state.clone();
        async move {
            state
                .db()
                .call(move |conn| unarr_core::movies::store::get_movie(conn, id))
                .await?
                .ok_or_else(|| ApiError::ModelNotFound(format!("Movie with ID {id} does not exist")))
        }
    };
    if body["shouldOverride"].as_bool().unwrap_or(false) {
        // The release as the user corrected it: the movie, its quality and
        // its languages.
        // `Ensure.That(...).IsNotNull()`, one after the other.
        let missing = |name: &str| {
            ApiError::Internal(anyhow::anyhow!(
                "Value can not be null. (Parameter 'release.{name}')"
            ))
        };
        let movie_id = movie_id.ok_or_else(|| missing("MovieId"))?;
        if body["quality"].is_null() {
            return Err(missing("Quality"));
        }
        if !body["languages"].is_array() {
            return Err(missing("Languages"));
        }
        remote.movie = Some(get_movie(movie_id).await?);
        let quality = &body["quality"];
        if let Some(parsed) = &mut remote.parsed {
            parsed.quality.quality = QualityModel {
                quality_id: quality["quality"]["id"].as_i64().unwrap_or(0),
                version: quality["revision"]["version"].as_i64().unwrap_or(1),
                real: quality["revision"]["real"].as_i64().unwrap_or(0),
                is_repack: quality["revision"]["isRepack"].as_bool().unwrap_or(false),
            };
        }
        remote.languages = body["languages"]
            .as_array()
            .map(|a| a.iter().filter_map(|l| l["id"].as_i64()).collect())
            .unwrap_or_default();
    }
    if remote.movie.is_none() {
        match movie_id {
            Some(id) => remote.movie = Some(get_movie(id).await?),
            None => {
                return Err(ApiError::NotFoundMessage(
                    "Unable to find matching movie, will need to be manually provided".into(),
                ))
            }
        }
    }
    state
        .movie_downloads()
        .download_report(&remote, body["downloadClientId"].as_i64(), state.now())
        .await
        .map_err(grab_error)?;
    super::queue::broadcast_queue(&state);
    Ok(Json(release_echo(&body)))
}

/// The posted `ReleaseResource` as the original binds and serializes it:
/// the value-typed members at their defaults, what was posted over them.
fn release_echo(body: &Value) -> Value {
    let mut m = Map::new();
    for (key, default) in [
        ("age", json!(0)),
        ("ageHours", json!(0)),
        ("ageMinutes", json!(0)),
        ("approved", json!(false)),
        ("customFormatScore", json!(0)),
        ("downloadAllowed", json!(false)),
        ("imdbId", json!(0)),
        ("indexerId", json!(0)),
        ("movieRequested", json!(false)),
        ("protocol", json!("unknown")),
        ("publishDate", json!("0001-01-01T00:00:00Z")),
        ("qualityWeight", json!(0)),
        ("rejected", json!(false)),
        ("releaseWeight", json!(0)),
        ("sceneSource", json!(false)),
        ("size", json!(0)),
        ("temporarilyRejected", json!(false)),
        ("tmdbId", json!(0)),
    ] {
        m.insert(key.to_string(), default);
    }
    if let Some(posted) = body.as_object() {
        for (key, value) in posted {
            if !value.is_null() {
                m.insert(key.clone(), value.clone());
            }
        }
    }
    Value::Object(m)
}

/// `POST /release/push`: a release another application found, decided on
/// as the RSS sync would and grabbed when approved.
pub async fn push(State(state): State<AppState>, Json(body): Json<Value>) -> ApiResult<Json<Vec<Value>>> {
    let now = state.now();
    let text = |key: &str| {
        body[key]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let title = text("title");
    let download_url = text("downloadUrl");
    let magnet_url = text("magnetUrl");
    let protocol = Protocol::parse(body["protocol"].as_str().unwrap_or("")).unwrap_or(Protocol::Unknown);
    let publish_date = body["publishDate"]
        .as_str()
        .and_then(unarr_core::time::parse_utc)
        .filter(|d| d.timestamp() > 0);
    tracing::info!(
        title = title.as_deref().unwrap_or(""),
        url = %unarr_core::http::redact(download_url.as_deref().or(magnet_url.as_deref()).unwrap_or("")),
        "release pushed"
    );
    let mut failures = Vec::new();
    if title.is_none() {
        failures.push(ValidationFailure::not_empty("Title", body["title"].clone()));
    }
    if download_url.is_none() && magnet_url.is_none() {
        failures.push(ValidationFailure::not_empty(
            "DownloadUrl",
            body["downloadUrl"].clone(),
        ));
        failures.push(ValidationFailure::not_empty(
            "MagnetUrl",
            body["magnetUrl"].clone(),
        ));
    }
    if protocol == Protocol::Unknown {
        failures.push(ValidationFailure::not_empty("Protocol", json!(protocol.as_str())));
    }
    if publish_date.is_none() {
        failures.push(ValidationFailure::not_empty(
            "PublishDate",
            json!("0001-01-01T00:00:00Z"),
        ));
    }
    if !failures.is_empty() {
        return Err(ApiError::Validation(failures));
    }
    let mut info = ReleaseInfo {
        guid: format!("PUSH-{}", download_url.clone().unwrap_or_default()),
        title: title.unwrap_or_default(),
        size: body["size"].as_i64().unwrap_or(0),
        download_url,
        info_url: text("infoUrl"),
        comment_url: text("commentUrl"),
        indexer_id: body["indexerId"].as_i64().unwrap_or(0),
        indexer: text("indexer").unwrap_or_default(),
        protocol,
        tmdb_id: body["tmdbId"].as_i64().unwrap_or(0),
        imdb_id: body["imdbId"]
            .as_i64()
            .filter(|id| *id > 0)
            .map(|id| id.to_string()),
        publish_date: publish_date.unwrap_or_default(),
        ..Default::default()
    };
    if protocol == Protocol::Torrent {
        info.magnet_url = magnet_url;
        info.info_hash = text("infoHash");
        info.seeders = body["seeders"].as_i64();
        info.peers = match (body["seeders"].as_i64(), body["leechers"].as_i64()) {
            (Some(s), Some(l)) => Some(s + l),
            _ => None,
        };
    }
    // `ResolveIndexer`: a name finds the id and the other way round.
    let (indexers, clients) = state
        .db()
        .call(|conn| {
            Ok((
                unarr_core::store::providers::list_providers(conn, ProviderKind::Indexer)?,
                unarr_core::store::providers::list_providers(conn, ProviderKind::DownloadClient)?,
            ))
        })
        .await?;
    if info.indexer_id == 0 && !info.indexer.is_empty() {
        if let Some(i) = indexers
            .iter()
            .find(|i| i.name.eq_ignore_ascii_case(&info.indexer))
        {
            info.indexer_id = i.id;
        }
    } else if info.indexer_id != 0 && info.indexer.is_empty() {
        match indexers.iter().find(|i| i.id == info.indexer_id) {
            Some(i) => info.indexer = i.name.clone(),
            None => info.indexer_id = 0,
        }
    }
    // `ResolveDownloadClientId`: a client's name stands in for its id.
    let mut download_client_id = body["downloadClientId"].as_i64();
    if download_client_id.unwrap_or(0) == 0 {
        if let Some(name) = text("downloadClient") {
            if let Some(c) = clients.iter().find(|c| c.name.eq_ignore_ascii_case(&name)) {
                download_client_id = Some(c.id);
            }
        }
    }
    // Decided and processed under the original's `PushLock`.
    let _one_push_at_a_time = state.push_lock().lock().await;
    let queue = state.movie_downloads().queued_movies();
    let decisions = state
        .db()
        .call(move |conn| {
            let catalog = MovieCatalog::load(conn, now)?.with_queue(queue);
            let lookup = StoreMovieLookup { conn };
            Ok(decision::decide(&catalog, &lookup, &[info], None, true))
        })
        .await?;
    let unparsed = || {
        ApiError::Validation(vec![ValidationFailure::rule(
            "Title",
            "Unable to parse",
            "Unable to parse",
            body["title"].clone(),
        )])
    };
    let decision = decisions.into_iter().next().ok_or_else(unparsed)?;
    // `ProcessDecision`: an approved push goes to the client, one a delay
    // holds back waits as a pending release; the answer reports the
    // decision either way.
    unarr_core::movies::downloads::process_decision(
        state.movie_downloads(),
        &decision,
        download_client_id.filter(|id| *id > 0),
        now,
    )
    .await?;
    super::queue::broadcast_queue(&state);
    if decision.remote.parsed.is_none() {
        return Err(unparsed());
    }
    Ok(Json(map_decisions(&state, &[decision], now)))
}
