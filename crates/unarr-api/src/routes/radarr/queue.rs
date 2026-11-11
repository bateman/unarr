//! Radarr's `/api/v3/queue`, after its `QueueController`,
//! `QueueStatusController`, `QueueDetailsController` and
//! `QueueActionController`: the downloads in progress, one item each, with
//! the removal options the UI offers.

use std::cmp::Ordering;

use axum::extract::{Path, State};
use axum::http::Uri;
use axum::Json;
use chrono::{DateTime, Utc};
use serde_json::{json, Map, Value};
use unarr_core::downloads::{QueueStatus, TrackedDownloadStatus};
use unarr_core::model::Protocol;
use unarr_core::movies::downloads::MovieQueueItem;
use unarr_core::movies::model::AlternativeTitle;
use unarr_core::movies::quality::{MovieLanguage, MovieQuality};
use unarr_core::store::paging::Paging;

use super::resources::{
    formats_resource, languages_resource, movie_resource, quality_resource, Covers, FileShown, MovieContext,
    MovieParts,
};
use crate::error::{ApiError, ApiResult};
use crate::query::Params;
use crate::resources::PagingResource;
use crate::routes::queue::{culture_cmp, options, protocol_rank, remove_error, timespan};
use crate::state::AppState;

const SORT_KEYS: &[&str] = &[
    "added",
    "downloadClient",
    "estimatedCompletionTime",
    "indexer",
    "languages",
    "movies.sortTitle",
    "progress",
    "protocol",
    "quality",
    "size",
    "status",
    "timeleft",
    "title",
    "year",
];

/// The queue as the clients last reported it, refreshed when a grab or a
/// removal left it stale; a change is announced on SignalR.
async fn fresh_queue(state: &AppState) -> ApiResult<Vec<MovieQueueItem>> {
    let downloads = state.movie_downloads();
    if downloads.refresh_if_dirty(state.now()).await? {
        broadcast_queue(state);
    }
    let mut queue = downloads.queue();
    queue.extend(downloads.pending_queue(state.now()).await?);
    Ok(queue)
}

/// `QueueUpdatedEvent` as the three controllers relay it: the lists told
/// to read themselves again, the status sent as it now stands, with what
/// a grab just added and the releases that wait.
pub fn broadcast_queue(state: &AppState) {
    let hub = state.hub();
    hub.broadcast("queue", json!({"action": "sync"}));
    hub.broadcast("queue/details", json!({"action": "sync"}));
    let state = state.clone();
    tokio::spawn(async move {
        let downloads = state.movie_downloads();
        let now = state.now();
        if let Err(e) = downloads.refresh_if_dirty(now).await {
            tracing::warn!(error = %e, "unable to refresh the queue for its status");
        }
        let mut queue = downloads.queue();
        match downloads.pending_queue(now).await {
            Ok(pending) => queue.extend(pending),
            Err(e) => tracing::warn!(error = %e, "unable to read the pending releases"),
        }
        state.hub().broadcast(
            "queue/status",
            json!({"action": "updated", "resource": status_resource(&queue)}),
        );
    });
}

/// What the movies embedded in the items of one answer are built from.
struct Embedding {
    ctx: MovieContext,
    alternative_titles: Vec<AlternativeTitle>,
}

impl Embedding {
    async fn load(state: &AppState, items: &[&MovieQueueItem], include_movie: bool) -> ApiResult<Embedding> {
        let ctx = MovieContext::load(state).await?;
        // Only the embedded movie shows them.
        let mut metadata_ids: Vec<i64> = items
            .iter()
            .filter(|q| include_movie && q.movie_by_id)
            .filter_map(|q| q.movie.as_ref().map(|m| m.metadata.id))
            .collect();
        metadata_ids.sort_unstable();
        metadata_ids.dedup();
        if metadata_ids.is_empty() {
            return Ok(Embedding {
                ctx,
                alternative_titles: Vec::new(),
            });
        }
        let alternative_titles = state
            .db()
            .call(move |conn| {
                let mut out = Vec::new();
                for id in metadata_ids {
                    out.extend(unarr_core::movies::store::alternative_titles_of(conn, id)?);
                }
                Ok(out)
            })
            .await?;
        Ok(Embedding {
            ctx,
            alternative_titles,
        })
    }
}

/// `QueueResource`, with nulls left out as the original's serializer does.
fn queue_resource(q: &MovieQueueItem, embedding: &Embedding, include_movie: bool) -> Value {
    let ctx = &embedding.ctx;
    let mut m = Map::new();
    m.insert("id".into(), json!(q.id));
    if let Some(movie) = &q.movie {
        m.insert("movieId".into(), json!(movie.id));
        if include_movie {
            // The movie as the queue holds it, without its file or local
            // covers; with its alternative titles when it was read by id.
            let titles: Vec<AlternativeTitle> = embedding
                .alternative_titles
                .iter()
                .filter(|t| q.movie_by_id && t.movie_metadata_id == movie.metadata.id)
                .cloned()
                .collect();
            m.insert(
                "movie".into(),
                movie_resource(
                    ctx,
                    movie,
                    &MovieParts {
                        alternative_titles: &titles,
                        file: None,
                        covers: Covers::Remote,
                        root_folder: false,
                        statistics: false,
                        movie_file: FileShown::No,
                    },
                ),
            );
        }
    }
    m.insert("languages".into(), json!(languages_resource(&q.languages)));
    m.insert("quality".into(), quality_resource(&q.quality));
    m.insert("customFormats".into(), json!(formats_resource(&q.custom_formats)));
    let score = q
        .movie
        .as_ref()
        .and_then(|movie| ctx.profile(movie.quality_profile_id))
        .map(|p| unarr_core::decision::upgradable::format_score(p, &q.custom_formats))
        .unwrap_or(0);
    m.insert("customFormatScore".into(), json!(score));
    m.insert("size".into(), json!(q.size));
    m.insert("title".into(), json!(q.title));
    if let Some(t) = q.estimated_completion_time {
        m.insert(
            "estimatedCompletionTime".into(),
            json!(unarr_core::time::format_utc(t)),
        );
    }
    if let Some(t) = q.added {
        m.insert("added".into(), json!(unarr_core::time::format_utc(t)));
    }
    m.insert("status".into(), json!(q.status.as_str()));
    if let Some(s) = q.tracked_download_status {
        m.insert("trackedDownloadStatus".into(), json!(s.as_str()));
    }
    if let Some(s) = q.tracked_download_state {
        m.insert("trackedDownloadState".into(), json!(s.as_str()));
    }
    if let Some(messages) = &q.status_messages {
        m.insert(
            "statusMessages".into(),
            json!(messages
                .iter()
                .map(|s| json!({"title": s.title, "messages": s.messages}))
                .collect::<Vec<_>>()),
        );
    }
    if let Some(e) = &q.error_message {
        m.insert("errorMessage".into(), json!(e));
    }
    if let Some(id) = &q.download_id {
        m.insert("downloadId".into(), json!(id));
    }
    m.insert("protocol".into(), json!(q.protocol.as_str()));
    if let Some(c) = &q.download_client {
        m.insert("downloadClient".into(), json!(c));
    }
    m.insert(
        "downloadClientHasPostImportCategory".into(),
        json!(q.download_client_has_post_import_category),
    );
    if let Some(i) = &q.indexer {
        m.insert("indexer".into(), json!(i));
    }
    if let Some(p) = &q.output_path {
        m.insert("outputPath".into(), json!(p));
    }
    m.insert("sizeleft".into(), json!(q.size_left));
    if let Some(t) = q.time_left {
        m.insert("timeleft".into(), json!(timespan(t)));
    }
    Value::Object(m)
}

fn status_resource(queue: &[MovieQueueItem]) -> Value {
    let known = |q: &&MovieQueueItem| q.movie.is_some();
    let unknown = |q: &&MovieQueueItem| q.movie.is_none();
    let any = |pick: &dyn Fn(&&MovieQueueItem) -> bool, status: TrackedDownloadStatus| {
        queue
            .iter()
            .filter(|q| pick(q))
            .any(|q| q.tracked_download_status == Some(status))
    };
    json!({
        "totalCount": queue.len(),
        "count": queue.iter().filter(known).count(),
        "unknownCount": queue.iter().filter(unknown).count(),
        "errors": any(&known, TrackedDownloadStatus::Error),
        "warnings": any(&known, TrackedDownloadStatus::Warning),
        "unknownErrors": any(&unknown, TrackedDownloadStatus::Error),
        "unknownWarnings": any(&unknown, TrackedDownloadStatus::Warning),
    })
}

/// `QueueController.GetQueue`: filtered, sorted, paged.
pub async fn list(State(state): State<AppState>, uri: Uri) -> ApiResult<Json<PagingResource<Value>>> {
    let p = Params::from_uri(&uri);
    let mut paging = Paging::resolve(
        p.i64("page"),
        p.i64("pageSize"),
        p.get("sortKey"),
        p.descending(),
        SORT_KEYS,
        "timeleft",
    );
    let include_unknown = p.bool("includeUnknownMovieItems", false);
    let include_movie = p.bool("includeMovie", false);
    let movie_ids = p.ints("movieIds");
    let protocol = p.get("protocol").and_then(Protocol::parse);
    let language_ids = p.ints("languages");
    let quality_ids = p.ints("quality");
    let statuses: Vec<QueueStatus> = p
        .all("status")
        .iter()
        .filter_map(|s| QueueStatus::parse(s))
        .collect();
    let queue = fresh_queue(&state).await?;
    let mut full: Vec<MovieQueueItem> = queue
        .into_iter()
        .filter(|q| include_unknown || q.movie.is_some())
        .filter(|q| {
            let mut include = true;
            if !movie_ids.is_empty() {
                include &= q.movie.as_ref().is_some_and(|m| movie_ids.contains(&m.id));
            }
            if include {
                if let Some(protocol) = protocol {
                    include &= q.protocol == protocol;
                }
            }
            if include && !language_ids.is_empty() {
                include &= q.languages.iter().any(|l| language_ids.contains(l));
            }
            if include && !quality_ids.is_empty() {
                include &= quality_ids.contains(&q.quality.quality_id);
            }
            if include && !statuses.is_empty() {
                include &= statuses.contains(&q.status);
            }
            include
        })
        .collect();
    let total = full.len() as i64;
    sort(&mut full, &paging.sort_key, paging.descending);
    let mut records = paging.slice(&full);
    if records.is_empty() && paging.page > 1 {
        // The original falls back to the last page (integer division, at
        // least one).
        paging.page = (total / paging.page_size).max(1);
        records = paging.slice(&full);
    }
    let embedding = Embedding::load(&state, &records.iter().collect::<Vec<_>>(), include_movie).await?;
    let out: Vec<Value> = records
        .iter()
        .map(|q| queue_resource(q, &embedding, include_movie))
        .collect();
    Ok(Json(PagingResource::new(&paging, total, out)))
}

/// How two queue items compare under one sort key.
type Comparer = Box<dyn Fn(&MovieQueueItem, &MovieQueueItem) -> Ordering>;

/// `DatetimeComparer`: unknown dates sort last.
fn compare_dates(x: Option<DateTime<Utc>>, y: Option<DateTime<Utc>>) -> Ordering {
    match (x, y) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(a), Some(b)) => a.cmp(&b),
    }
}

/// `LanguagesComparer`: none last, one before several, fewer before more,
/// two single languages by name.
fn compare_languages(x: &[i64], y: &[i64]) -> Ordering {
    match (x.len(), y.len()) {
        (0, 0) => Ordering::Equal,
        (0, _) => Ordering::Greater,
        (_, 0) => Ordering::Less,
        (1, 1) => MovieLanguage::name_of(x[0]).cmp(MovieLanguage::name_of(y[0])),
        (a, b) => a.cmp(&b),
    }
}

/// The original's order: the sort key, then progress descending.
fn sort(items: &mut [MovieQueueItem], key: &str, descending: bool) {
    let text = |a: Option<&str>, b: Option<&str>| {
        a.unwrap_or("")
            .to_lowercase()
            .cmp(&b.unwrap_or("").to_lowercase())
    };
    let quality_weight = |q: &MovieQueueItem| {
        (
            MovieQuality::weight(q.quality.quality_id),
            q.quality.real,
            q.quality.version,
        )
    };
    let by: Comparer = match key {
        "estimatedCompletionTime" => {
            Box::new(|a, b| compare_dates(a.estimated_completion_time, b.estimated_completion_time))
        }
        "added" => Box::new(|a, b| compare_dates(a.added, b.added)),
        "protocol" => Box::new(|a, b| protocol_rank(a.protocol).cmp(&protocol_rank(b.protocol))),
        "indexer" => Box::new(move |a, b| text(a.indexer.as_deref(), b.indexer.as_deref())),
        "downloadClient" => {
            Box::new(move |a, b| text(a.download_client.as_deref(), b.download_client.as_deref()))
        }
        "quality" => Box::new(move |a, b| quality_weight(a).cmp(&quality_weight(b))),
        "languages" => Box::new(|a, b| compare_languages(&a.languages, &b.languages)),
        "status" => Box::new(|a, b| a.status.as_str().cmp(b.status.as_str())),
        "movies.sortTitle" => Box::new(|a, b| {
            let key = |q: &MovieQueueItem| {
                q.movie
                    .as_ref()
                    .and_then(|m| m.metadata.sort_title.clone())
                    .unwrap_or_else(|| q.title.clone())
            };
            culture_cmp(&key(a), &key(b))
        }),
        "title" => Box::new(|a, b| culture_cmp(&a.title, &b.title)),
        "year" => Box::new(|a, b| {
            let key = |q: &MovieQueueItem| q.movie.as_ref().map(|m| m.metadata.year).unwrap_or(0);
            key(a).cmp(&key(b))
        }),
        "size" => Box::new(|a, b| a.size.cmp(&b.size)),
        "progress" => Box::new(|a, b| {
            // `100 - sizeLeft / max(size * 100, 1)`, as the original wrote it.
            let key = |q: &MovieQueueItem| 100.0 - q.size_left as f64 / (q.size as f64 * 100.0).max(1.0);
            key(a).partial_cmp(&key(b)).unwrap_or(Ordering::Equal)
        }),
        _ => Box::new(|a, b| unarr_core::downloads::queue::compare_time_left(a.time_left, b.time_left)),
    };
    items.sort_by(|a, b| {
        let primary = if descending { by(b, a) } else { by(a, b) };
        primary.then_with(|| b.progress().partial_cmp(&a.progress()).unwrap_or(Ordering::Equal))
    });
}

/// `GET /queue/status`.
pub async fn status(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let queue = fresh_queue(&state).await?;
    Ok(Json(status_resource(&queue)))
}

/// `GET /queue/details?movieId&includeMovie`.
pub async fn details(State(state): State<AppState>, uri: Uri) -> ApiResult<Json<Vec<Value>>> {
    let p = Params::from_uri(&uri);
    let movie_id = p.i64("movieId");
    let include_movie = p.bool("includeMovie", false);
    let queue = fresh_queue(&state).await?;
    let selected: Vec<&MovieQueueItem> = queue
        .iter()
        .filter(|q| match movie_id {
            Some(id) => q.movie.as_ref().is_some_and(|m| m.id == id),
            None => true,
        })
        .collect();
    let embedding = Embedding::load(&state, &selected, include_movie).await?;
    Ok(Json(
        selected
            .into_iter()
            .map(|q| queue_resource(q, &embedding, include_movie))
            .collect(),
    ))
}

/// `DELETE /queue/{id}`: the original answers an empty body (`null`).
pub async fn remove(State(state): State<AppState>, Path(id): Path<i64>, uri: Uri) -> ApiResult<Json<Value>> {
    let p = Params::from_uri(&uri);
    fresh_queue(&state).await?;
    let opts = options(&p);
    let downloads = state.movie_downloads();
    // A pending release first, as the original looks.
    if let Some(pending) = downloads.find_pending_queue_item(id, state.now()).await? {
        downloads
            .remove_pending(&pending, opts.blocklist, state.now())
            .await?;
        broadcast_queue(&state);
        return Ok(Json(Value::Null));
    }
    downloads
        .remove_queue_item(id, opts, state.now())
        .await
        .map_err(remove_error)?;
    broadcast_queue(&state);
    Ok(Json(Value::Null))
}

/// `DELETE /queue/bulk`.
pub async fn remove_many(
    State(state): State<AppState>,
    uri: Uri,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let p = Params::from_uri(&uri);
    let ids: Vec<i64> = body["ids"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default();
    let queue = fresh_queue(&state).await?;
    // The original looks every id up and 404s when one names nothing.
    if ids.iter().any(|id| !queue.iter().any(|q| q.id == *id)) {
        return Err(ApiError::NotFoundException);
    }
    let opts = options(&p);
    let downloads = state.movie_downloads();
    // Each item once, however often the request names it (the original's
    // `DistinctBy`): a second removal would blocklist the release again.
    let mut tracked = Vec::new();
    let mut pending_done: Vec<i64> = Vec::new();
    for id in &ids {
        match queue.iter().find(|q| q.id == *id) {
            Some(pending) if pending.download_id.is_none() => {
                if pending_done.contains(id) {
                    continue;
                }
                pending_done.push(*id);
                downloads
                    .remove_pending(pending, opts.blocklist, state.now())
                    .await?
            }
            _ => tracked.push(*id),
        }
    }
    downloads
        .remove_queue_items(&tracked, opts, state.now())
        .await
        .map_err(remove_error)?;
    broadcast_queue(&state);
    Ok(Json(json!({})))
}

/// `POST /queue/grab/{id}`: a pending release sent to a client now.
pub async fn grab(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    grab_pending(&state, id).await?;
    Ok(Json(json!({})))
}

/// `POST /queue/grab/bulk`.
pub async fn grab_many(State(state): State<AppState>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let ids: Vec<i64> = body["ids"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default();
    for id in ids {
        match grab_pending(&state, id).await {
            // The grab of one release took the lesser ones of its movie
            // with it; the remaining ids are still tried.
            Err(ApiError::NotFoundException) => continue,
            other => other?,
        }
    }
    Ok(Json(json!({})))
}

/// `QueueActionController.Grab`.
async fn grab_pending(state: &AppState, id: i64) -> ApiResult<()> {
    let now = state.now();
    let downloads = state.movie_downloads();
    let item = downloads
        .find_pending_queue_item(id, now)
        .await?
        .ok_or(ApiError::NotFoundException)?;
    let remote = item.remote.ok_or(ApiError::NotFoundException)?;
    downloads
        .download_report(&remote, None, now)
        .await
        .map_err(|e| match e {
            unarr_core::downloads::GrabError::ReleaseDownload(m)
            | unarr_core::downloads::GrabError::ReleaseUnavailable(m) => ApiError::ConflictMessage(m),
            other => super::release::grab_error(other),
        })?;
    broadcast_queue(state);
    Ok(())
}
