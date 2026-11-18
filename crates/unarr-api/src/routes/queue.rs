//! `/api/v3/queue`, after the originals' `QueueController`,
//! `QueueStatusController`, `QueueDetailsController` and
//! `QueueActionController`: the downloads in progress, one item per
//! episode, with the removal options the UI offers.

use std::cmp::Ordering;

use axum::extract::{Path, State};
use axum::http::Uri;
use axum::Json;
use chrono::Duration;
use serde_json::{json, Map, Value};
use unarr_core::downloads::{GrabError, QueueItem, QueueStatus, RemoveError, RemoveOptions};
use unarr_core::model::Protocol;
use unarr_core::quality::default_profile_index;
use unarr_core::store::paging::Paging;

use crate::error::{ApiError, ApiResult};
use crate::query::Params;
use crate::resources::{
    languages, series_resource_plain, CustomFormatRef, PagingResource, QualityModelResource,
};
use crate::routes::episodes::{episode_resource, Catalog, Include};
use crate::state::AppState;

const SORT_KEYS: &[&str] = &[
    "added",
    "downloadClient",
    "episode",
    "episode.airDateUtc",
    "episode.title",
    "episodes.airDateUtc",
    "episodes.title",
    "estimatedCompletionTime",
    "indexer",
    "language",
    "languages",
    "progress",
    "protocol",
    "quality",
    "series.sortTitle",
    "size",
    "status",
    "timeleft",
    "title",
];

/// The queue as the clients last reported it, refreshed when a grab or a
/// removal left it stale; a change is announced on SignalR.
async fn fresh_queue(state: &AppState) -> ApiResult<Vec<QueueItem>> {
    let downloads = state.downloads();
    if downloads.refresh_if_dirty(state.now()).await? {
        broadcast_queue(state);
    }
    let mut queue = downloads.queue();
    queue.extend(downloads.pending_queue(state.now()).await?);
    Ok(queue)
}

/// `QueueUpdatedEvent` as the three controllers relay it. The status is
/// what `GET /queue/status` would answer: `QueueStatusController` counts
/// the queue after its refresh, with the pending releases, so it follows
/// in a task of its own once the queue is fresh.
pub fn broadcast_queue(state: &AppState) {
    let hub = state.hub();
    hub.broadcast("queue", json!({"action": "sync"}));
    hub.broadcast("queue/details", json!({"action": "sync"}));
    let state = state.clone();
    // Lost if it panics: the next change of the queue sends the status.
    unarr_core::panics::spawn_logged("queue status broadcast", async move {
        let downloads = state.downloads();
        let now = state.now();
        // A grab or a removal only marked the queue dirty. A refresh that
        // fails leaves it so; the status then says what is known.
        if let Err(e) = downloads.refresh_if_dirty(now).await {
            tracing::warn!(error = %e, "queue refresh before the status broadcast failed");
        }
        let mut queue = downloads.queue();
        match downloads.pending_queue(now).await {
            Ok(pending) => queue.extend(pending),
            Err(e) => tracing::warn!(error = %e, "pending releases left out of the queue status"),
        }
        state.hub().broadcast(
            "queue/status",
            json!({"action": "updated", "resource": status_resource(&queue)}),
        );
    });
}

/// `TimeSpan.ToString()`: `[d.]hh:mm:ss[.fffffff]`.
pub fn timespan(d: Duration) -> String {
    let total = d.num_seconds().max(0);
    let days = total / 86400;
    let h = total % 86400 / 3600;
    let m = total % 3600 / 60;
    let s = total % 60;
    let fraction = d.num_milliseconds().max(0) % 1000;
    let mut out = if days > 0 {
        format!("{days}.{h:02}:{m:02}:{s:02}")
    } else {
        format!("{h:02}:{m:02}:{s:02}")
    };
    if fraction != 0 {
        out.push_str(&format!(".{fraction:03}0000"));
    }
    out
}

/// `QueueResource`, with nulls left out as the originals' serializer does.
fn queue_resource(
    q: &QueueItem,
    catalog: Option<&Catalog>,
    include_series: bool,
    include_episode: bool,
) -> Value {
    let mut m = Map::new();
    m.insert("id".into(), json!(q.id));
    if let Some(s) = &q.series {
        m.insert("seriesId".into(), json!(s.id));
    }
    if let Some(e) = &q.episode {
        m.insert("episodeId".into(), json!(e.id));
        m.insert("seasonNumber".into(), json!(e.season_number));
    }
    if include_series {
        if let Some(s) = &q.series {
            m.insert(
                "series".into(),
                serde_json::to_value(series_resource_plain(s)).unwrap_or(Value::Null),
            );
        }
    }
    if include_episode {
        // The catalog is there whenever an item of the answer has an episode.
        if let (Some(e), Some(catalog)) = (&q.episode, catalog) {
            m.insert(
                "episode".into(),
                serde_json::to_value(episode_resource(catalog, e, None, Include::default()))
                    .unwrap_or(Value::Null),
            );
        }
    }
    m.insert("languages".into(), json!(languages(&q.languages)));
    let quality: QualityModelResource = q.quality.into();
    m.insert("quality".into(), json!(quality));
    let formats: Vec<CustomFormatRef> = q
        .custom_formats
        .iter()
        .map(|f| CustomFormatRef {
            id: f.id,
            name: f.name.clone(),
        })
        .collect();
    m.insert("customFormats".into(), json!(formats));
    m.insert("customFormatScore".into(), json!(q.custom_format_score));
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
    m.insert(
        "episodeHasFile".into(),
        json!(q.episode.as_ref().is_some_and(|e| e.episode_file_id > 0)),
    );
    m.insert("sizeleft".into(), json!(q.size_left));
    if let Some(t) = q.time_left {
        m.insert("timeleft".into(), json!(timespan(t)));
    }
    Value::Object(m)
}

fn status_resource(queue: &[QueueItem]) -> Value {
    use unarr_core::downloads::TrackedDownloadStatus;
    let known = |q: &&QueueItem| q.series.is_some();
    let unknown = |q: &&QueueItem| q.series.is_none();
    json!({
        "totalCount": queue.len(),
        "count": queue.iter().filter(known).count(),
        "unknownCount": queue.iter().filter(unknown).count(),
        "errors": queue.iter().filter(known).any(|q| q.tracked_download_status == Some(TrackedDownloadStatus::Error)),
        "warnings": queue.iter().filter(known).any(|q| q.tracked_download_status == Some(TrackedDownloadStatus::Warning)),
        "unknownErrors": queue.iter().filter(unknown).any(|q| q.tracked_download_status == Some(TrackedDownloadStatus::Error)),
        "unknownWarnings": queue.iter().filter(unknown).any(|q| q.tracked_download_status == Some(TrackedDownloadStatus::Warning)),
    })
}

/// The reference data of the episodes one answer embeds: their series, the
/// quality profiles and the custom formats. Only `episode_resource` reads
/// it, so it is None, and nothing is read, when the answer embeds no
/// episode: without `includeEpisode`, or with no item that has one, such as
/// an empty queue.
async fn catalog(
    state: &AppState,
    items: &[&QueueItem],
    include_episode: bool,
) -> ApiResult<Option<Catalog>> {
    if !include_episode || items.iter().all(|q| q.episode.is_none()) {
        return Ok(None);
    }
    let ids: Vec<i64> = items
        .iter()
        .filter(|q| q.episode.is_some())
        .filter_map(|q| q.series.as_ref().map(|s| s.id))
        .collect();
    Ok(Some(
        state.db().call(move |conn| Catalog::load_for(conn, &ids)).await?,
    ))
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
    let include_unknown = p.bool("includeUnknownSeriesItems", false);
    let include_series = p.bool("includeSeries", false);
    let include_episode = p.bool("includeEpisode", false);
    let series_ids = p.ints("seriesIds");
    let protocol = p.get("protocol").and_then(Protocol::parse);
    let language_ids = p.ints("languages");
    let quality_ids = p.ints("quality");
    let statuses: Vec<QueueStatus> = p
        .all("status")
        .iter()
        .filter_map(|s| QueueStatus::parse(s))
        .collect();
    let queue = fresh_queue(&state).await?;
    let mut full: Vec<QueueItem> = queue
        .into_iter()
        .filter(|q| include_unknown || q.series.is_some())
        .filter(|q| {
            let mut include = true;
            if !series_ids.is_empty() {
                include &= q.series.as_ref().is_some_and(|s| series_ids.contains(&s.id));
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
        // The originals fall back to the last page (integer division, at
        // least one).
        paging.page = (total / paging.page_size).max(1);
        records = paging.slice(&full);
    }
    let catalog = catalog(&state, &records.iter().collect::<Vec<_>>(), include_episode).await?;
    let out: Vec<Value> = records
        .iter()
        .map(|q| queue_resource(q, catalog.as_ref(), include_series, include_episode))
        .collect();
    Ok(Json(PagingResource::new(&paging, total, out)))
}

/// How two queue items compare under one sort key.
type Comparer = Box<dyn Fn(&QueueItem, &QueueItem) -> Ordering>;

/// The originals' order: the sort key, then progress descending.
fn sort(items: &mut [QueueItem], key: &str, descending: bool) {
    let compare_opt_time = |a: &QueueItem, b: &QueueItem| {
        unarr_core::downloads::queue::compare_time_left(a.time_left, b.time_left)
    };
    let compare_opt_date =
        |x: Option<chrono::DateTime<chrono::Utc>>, y: Option<chrono::DateTime<chrono::Utc>>| match (x, y) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(a), Some(b)) => a.cmp(&b),
        };
    let quality_weight = |q: &QueueItem| {
        (
            default_profile_index(q.quality.quality_id),
            q.quality.real,
            q.quality.version,
        )
    };
    let by: Comparer = match key {
        "estimatedCompletionTime" => {
            Box::new(move |a, b| compare_opt_date(a.estimated_completion_time, b.estimated_completion_time))
        }
        "added" => Box::new(move |a, b| compare_opt_date(a.added, b.added)),
        "protocol" => Box::new(|a, b| protocol_rank(a.protocol).cmp(&protocol_rank(b.protocol))),
        "indexer" => Box::new(|a, b| {
            a.indexer
                .as_deref()
                .unwrap_or("")
                .to_lowercase()
                .cmp(&b.indexer.as_deref().unwrap_or("").to_lowercase())
        }),
        "downloadClient" => Box::new(|a, b| {
            a.download_client
                .as_deref()
                .unwrap_or("")
                .to_lowercase()
                .cmp(&b.download_client.as_deref().unwrap_or("").to_lowercase())
        }),
        "quality" => Box::new(move |a, b| quality_weight(a).cmp(&quality_weight(b))),
        "language" | "languages" => Box::new(|a, b| {
            let names = |q: &QueueItem| {
                languages(&q.languages)
                    .iter()
                    .map(|l| l.name.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            names(a).cmp(&names(b))
        }),
        "status" => Box::new(|a, b| a.status.as_str().cmp(b.status.as_str())),
        "series.sortTitle" => Box::new(|a, b| {
            let key = |q: &QueueItem| {
                q.series
                    .as_ref()
                    .and_then(|s| s.sort_title.clone())
                    .unwrap_or_else(|| q.title.clone())
            };
            culture_cmp(&key(a), &key(b))
        }),
        "title" => Box::new(|a, b| culture_cmp(&a.title, &b.title)),
        "episode" => Box::new(|a, b| {
            let key = |q: &QueueItem| q.episode.as_ref().map(|e| (e.season_number, e.episode_number));
            key(a).cmp(&key(b))
        }),
        "episode.airDateUtc" | "episodes.airDateUtc" => Box::new(|a, b| {
            let key = |q: &QueueItem| q.episode.as_ref().and_then(|e| e.air_date_utc);
            key(a).cmp(&key(b))
        }),
        "episode.title" | "episodes.title" => Box::new(|a, b| {
            let key = |q: &QueueItem| {
                q.episode
                    .as_ref()
                    .and_then(|e| e.title.clone())
                    .unwrap_or_default()
            };
            culture_cmp(&key(a), &key(b))
        }),
        "size" => Box::new(|a, b| a.size.cmp(&b.size)),
        "progress" => Box::new(|a, b| {
            // `100 - sizeLeft / max(size * 100, 1)`, as the originals wrote it.
            let key = |q: &QueueItem| 100.0 - q.size_left as f64 / (q.size as f64 * 100.0).max(1.0);
            key(a).partial_cmp(&key(b)).unwrap_or(Ordering::Equal)
        }),
        _ => Box::new(move |a, b| compare_opt_time(a, b)),
    };
    items.sort_by(|a, b| {
        let primary = if descending { by(b, a) } else { by(a, b) };
        primary.then_with(|| b.progress().partial_cmp(&a.progress()).unwrap_or(Ordering::Equal))
    });
}

/// The default string comparer of the originals: letters in alphabetical
/// order whatever their case, the case deciding only among equals.
pub(crate) fn culture_cmp(a: &str, b: &str) -> Ordering {
    a.to_lowercase().cmp(&b.to_lowercase()).then_with(|| a.cmp(b))
}

/// `DownloadProtocol`'s declaration order.
pub(crate) fn protocol_rank(p: Protocol) -> i64 {
    match p {
        Protocol::Unknown => 0,
        Protocol::Usenet => 1,
        Protocol::Torrent => 2,
    }
}

/// `GET /queue/status`.
pub async fn status(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let queue = fresh_queue(&state).await?;
    Ok(Json(status_resource(&queue)))
}

/// `GET /queue/details?seriesId&episodeIds&includeSeries&includeEpisode`.
pub async fn details(State(state): State<AppState>, uri: Uri) -> ApiResult<Json<Vec<Value>>> {
    let p = Params::from_uri(&uri);
    let series_id = p.i64("seriesId");
    let episode_ids = p.ints("episodeIds");
    let include_series = p.bool("includeSeries", false);
    let include_episode = p.bool("includeEpisode", false);
    let queue = fresh_queue(&state).await?;
    let selected: Vec<&QueueItem> = match (series_id, episode_ids.is_empty()) {
        (Some(id), _) => queue
            .iter()
            .filter(|q| q.series.as_ref().is_some_and(|s| s.id == id))
            .collect(),
        (None, false) => queue
            .iter()
            .filter(|q| q.episode.as_ref().is_some_and(|e| episode_ids.contains(&e.id)))
            .collect(),
        _ => queue.iter().collect(),
    };
    // The catalog of the selected items only, not of the whole queue.
    let catalog = catalog(&state, &selected, include_episode).await?;
    Ok(Json(
        selected
            .into_iter()
            .map(|q| queue_resource(q, catalog.as_ref(), include_series, include_episode))
            .collect(),
    ))
}

pub(crate) fn options(p: &Params) -> RemoveOptions {
    RemoveOptions {
        remove_from_client: p.bool("removeFromClient", true),
        blocklist: p.bool("blocklist", false),
        skip_redownload: p.bool("skipRedownload", false),
        change_category: p.bool("changeCategory", false),
    }
}

pub(crate) fn remove_error(e: RemoveError) -> ApiError {
    match e {
        RemoveError::NotFound => ApiError::NotFoundException,
        RemoveError::ClientGone => ApiError::BadRequest(String::new()),
        RemoveError::Other(e) => ApiError::Internal(e),
    }
}

/// `DELETE /queue/{id}`: the originals answer an empty body (`null`).
pub async fn remove(State(state): State<AppState>, Path(id): Path<i64>, uri: Uri) -> ApiResult<Json<Value>> {
    let p = Params::from_uri(&uri);
    fresh_queue(&state).await?;
    let opts = options(&p);
    // A pending release first, as the originals look.
    if let Some(pending) = state.downloads().find_pending_queue_item(id, state.now()).await? {
        state
            .downloads()
            .remove_pending(&pending, opts.blocklist, state.now())
            .await?;
        broadcast_queue(&state);
        return Ok(Json(Value::Null));
    }
    state
        .downloads()
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
    // The originals look every id up and 404 when one names nothing.
    if ids.iter().any(|id| !queue.iter().any(|q| q.id == *id)) {
        return Err(ApiError::NotFoundException);
    }
    let opts = options(&p);
    let (pending, tracked): (Vec<&QueueItem>, Vec<i64>) = {
        let mut pending = Vec::new();
        let mut tracked = Vec::new();
        for id in &ids {
            match queue.iter().find(|q| q.id == *id) {
                Some(q) if q.download_id.is_none() => {
                    if !pending.iter().any(|p: &&QueueItem| p.id == q.id) {
                        pending.push(q);
                    }
                }
                _ => tracked.push(*id),
            }
        }
        (pending, tracked)
    };
    for item in pending {
        state
            .downloads()
            .remove_pending(item, opts.blocklist, state.now())
            .await?;
    }
    state
        .downloads()
        .remove_queue_items(&tracked, opts, state.now())
        .await
        .map_err(remove_error)?;
    broadcast_queue(&state);
    Ok(Json(json!({})))
}

/// `POST /queue/grab/{id}`: a pending release grabbed now.
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
            // A pack grabbed for one of its episodes took the rows of the
            // others with it; the remaining ids are still tried.
            Err(ApiError::NotFoundException) => continue,
            other => other?,
        }
    }
    Ok(Json(json!({})))
}

/// `QueueActionController.Grab`: a pending release sent to a client now.
async fn grab_pending(state: &AppState, id: i64) -> ApiResult<()> {
    let now = state.now();
    let item = state
        .downloads()
        .find_pending_queue_item(id, now)
        .await?
        .ok_or(ApiError::NotFoundException)?;
    let remote = item.remote.ok_or(ApiError::NotFoundException)?;
    match state.downloads().download_report(&remote, None, now).await {
        Ok(_) => {
            broadcast_queue(state);
            Ok(())
        }
        Err(GrabError::ReleaseDownload(m)) | Err(GrabError::ReleaseUnavailable(m)) => {
            Err(ApiError::ConflictMessage(m))
        }
        Err(GrabError::NoClient(m)) => Err(ApiError::Internal(anyhow::anyhow!("{m}"))),
        Err(GrabError::Client(e)) => Err(ApiError::Internal(e.into_anyhow())),
        Err(GrabError::Other(e)) => Err(ApiError::Internal(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_timespans() {
        assert_eq!(timespan(Duration::minutes(10)), "00:10:00");
        assert_eq!(timespan(Duration::seconds(93784)), "1.02:03:04");
        assert_eq!(timespan(Duration::milliseconds(1500)), "00:00:01.5000000");
    }
}
