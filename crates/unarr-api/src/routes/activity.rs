//! `history`, `queue`, `blocklist`: what the Activity pages show. Unarr does
//! not download yet, so the queue is empty with the right shape.
//!
//! Rows are read on the database thread and turned into resources on a
//! blocking worker: matching every row against the custom formats is CPU
//! work, and doing it on the database thread stalled every other request
//! while a client paged through a long history.

use std::collections::HashMap;

use axum::extract::{Path, State};
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use unarr_core::customformat::{self, Input};
use unarr_core::model::{BlocklistEntry, Episode, HistoryEntry, HistoryEvent, ReleaseType};
use unarr_core::profile;
use unarr_core::store;
use unarr_core::store::history::HistoryFilter;
use unarr_core::store::paging::Paging;

use crate::error::{ApiError, ApiResult};
use crate::query::Params;
use crate::resources::{languages, series_resource_plain, CustomFormatRef, HistoryResource, PagingResource};
use crate::routes::episodes::{episode_resource, Catalog, Include};
use crate::state::AppState;

/// The API accepts an event type by number or by name.
fn event_param(v: &str) -> Option<HistoryEvent> {
    v.parse::<i64>()
        .ok()
        .and_then(HistoryEvent::from_number)
        .or_else(|| HistoryEvent::parse(v))
        .or_else(|| {
            HistoryEvent::ALL
                .iter()
                .copied()
                .find(|e| e.as_str().eq_ignore_ascii_case(v))
        })
}

/// Custom formats of a release title with the quality, languages and extras
/// a history or blocklist row carries, and their score for the series.
fn release_formats(
    catalog: &Catalog,
    series: &unarr_core::model::Series,
    title: &str,
    input: Input<'_>,
) -> (Vec<CustomFormatRef>, i64) {
    let group = customformat::parse_release_group(title);
    let release_title = customformat::strip_media_extension(title);
    let input = Input {
        release_title: &release_title,
        release_group: group.as_deref(),
        ..input
    };
    let matched = customformat::evaluate(&catalog.formats, &input);
    let ids: Vec<i64> = matched.iter().map(|f| f.id).collect();
    let score = catalog
        .profile(series.quality_profile_id)
        .map(|p| profile::custom_format_score(p, &ids))
        .unwrap_or(0);
    (
        matched
            .iter()
            .map(|f| CustomFormatRef {
                id: f.id,
                name: f.name.clone(),
            })
            .collect(),
        score,
    )
}

fn history_resource(
    catalog: &Catalog,
    h: &HistoryEntry,
    episodes: &HashMap<i64, Episode>,
    include_series: bool,
    include_episode: bool,
) -> HistoryResource {
    let series = catalog.series.get(&h.series_id);
    let (custom_formats, custom_format_score, cutoff_not_met) = match series {
        Some(s) => {
            let title = h.source_title.as_deref().unwrap_or("");
            let release_type = h.data["releaseType"]
                .as_str()
                .and_then(|v| {
                    ReleaseType::ALL
                        .iter()
                        .copied()
                        .find(|r| r.as_str().eq_ignore_ascii_case(v))
                })
                .unwrap_or(ReleaseType::Unknown);
            let input = Input {
                quality: h.quality,
                languages: &h.languages,
                size: h.data["size"].as_str().and_then(|v| v.parse().ok()).unwrap_or(0),
                indexer_flags: h.data["indexerFlags"]
                    .as_str()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0),
                release_type,
                series_original_language: s.original_language_id,
                movie: None,
                ..Input::default()
            };
            let (formats, score) = release_formats(catalog, s, title, input);
            let cutoff_not_met = catalog
                .profile(s.quality_profile_id)
                .map(|p| profile::quality_cutoff_not_met(p, h.quality.quality_id))
                .unwrap_or(false);
            (formats, score, cutoff_not_met)
        }
        None => (Vec::new(), 0, false),
    };
    HistoryResource {
        id: h.id,
        episode_id: h.episode_id,
        series_id: h.series_id,
        source_title: h.source_title.clone(),
        languages: languages(&h.languages),
        quality: h.quality.into(),
        custom_formats,
        custom_format_score,
        quality_cutoff_not_met: cutoff_not_met,
        date: h.date,
        download_id: h.download_id.clone(),
        event_type: h.event_type.as_str(),
        data: h.data.clone(),
        episode: if include_episode {
            episodes
                .get(&h.episode_id)
                .map(|e| episode_resource(catalog, e, None, Include::default()))
        } else {
            None
        },
        series: if include_series {
            series.map(series_resource_plain)
        } else {
            None
        },
    }
}

/// History rows with everything their resources refer to.
struct HistoryLoad {
    entries: Vec<HistoryEntry>,
    total: i64,
    catalog: Catalog,
    episodes: HashMap<i64, Episode>,
}

impl HistoryLoad {
    fn read(
        conn: &rusqlite::Connection,
        entries: Vec<HistoryEntry>,
        total: i64,
        include_episode: bool,
    ) -> anyhow::Result<HistoryLoad> {
        let series_ids: Vec<i64> = entries.iter().map(|h| h.series_id).collect();
        let catalog = Catalog::load_for(conn, &series_ids)?;
        let episodes = if include_episode {
            let ids: Vec<i64> = entries.iter().map(|h| h.episode_id).collect();
            store::episodes::list_episodes_by_ids(conn, &ids)?
                .into_iter()
                .map(|e| (e.id, e))
                .collect()
        } else {
            HashMap::new()
        };
        Ok(HistoryLoad {
            entries,
            total,
            catalog,
            episodes,
        })
    }

    fn render(&self, include_series: bool, include_episode: bool) -> Vec<HistoryResource> {
        self.entries
            .iter()
            .map(|h| history_resource(&self.catalog, h, &self.episodes, include_series, include_episode))
            .collect()
    }
}

pub async fn history(
    State(state): State<AppState>,
    uri: Uri,
) -> ApiResult<Json<PagingResource<HistoryResource>>> {
    let p = Params::from_uri(&uri);
    let paging = Paging::resolve(
        p.i64("page"),
        p.i64("pageSize"),
        p.get("sortKey"),
        p.descending(),
        &["date", "series.sortTitle"],
        "date",
    );
    let filter = HistoryFilter {
        event_types: p.all("eventType").iter().filter_map(|v| event_param(v)).collect(),
        episode_id: p.i64("episodeId"),
        download_id: p
            .get("downloadId")
            .map(str::to_string)
            .filter(|s| !s.trim().is_empty()),
        series_ids: p.ints("seriesIds"),
        languages: p.ints("languages"),
        qualities: p.ints("quality"),
    };
    let include_series = p.bool("includeSeries", false);
    let include_episode = p.bool("includeEpisode", false);
    let paging_for_db = paging.clone();
    let load = state
        .db()
        .call(move |conn| {
            let (entries, total) = store::history::paged(conn, &paging_for_db, &filter)?;
            HistoryLoad::read(conn, entries, total, include_episode)
        })
        .await?;
    let out = crate::blocking::run(move || {
        let records = load.render(include_series, include_episode);
        PagingResource::new(&paging, load.total, records)
    })
    .await?;
    Ok(Json(out))
}

/// Rows of `history/since` rendered and serialised per page, so the
/// response does not have to fit in memory at once.
const SINCE_PAGE: i64 = 1000;

/// `GET /history/since?date=`: every row since the date, oldest first,
/// streamed as one JSON array. The originals build the whole list; on a
/// large library that is tens of megabytes held until the client is done.
pub async fn history_since(State(state): State<AppState>, uri: Uri) -> ApiResult<Response> {
    let p = Params::from_uri(&uri);
    let Some(date) = p.time("date") else {
        return Err(ApiError::BadRequest("date is required".into()));
    };
    let event = p.get("eventType").and_then(event_param);
    let include_series = p.bool("includeSeries", false);
    let include_episode = p.bool("includeEpisode", false);
    let db = state.db().clone();
    // One page rendered to JSON text, with the cursor of its last row.
    let page = move |after: Option<(DateTime<Utc>, i64)>, first: bool| {
        let db = db.clone();
        async move {
            db.call(move |conn| {
                let entries = store::history::since_page(conn, date, event, after, SINCE_PAGE)?;
                let last = entries.last().map(|h| (h.date, h.id));
                let total = entries.len() as i64;
                let load = HistoryLoad::read(conn, entries, total, include_episode)?;
                let mut text = String::new();
                for (i, r) in load.render(include_series, include_episode).iter().enumerate() {
                    if !(first && i == 0) {
                        text.push(',');
                    }
                    text.push_str(&serde_json::to_string(r)?);
                }
                Ok((text, last))
            })
            .await
        }
    };
    // The first page before answering, so an error is still a status code.
    let (text, mut cursor) = page(None, true).await?;
    // No cursor means no rows at all: the array closes at once, since the
    // stream below ends where the cursor ends.
    let head = if cursor.is_some() {
        format!("[{text}")
    } else {
        format!("[{text}]")
    };
    let stream = futures_util::stream::unfold(
        (Some(head), cursor.take(), page),
        |(pending, cursor, page)| async move {
            if let Some(text) = pending {
                return Some((
                    Ok::<_, std::io::Error>(axum::body::Bytes::from(text)),
                    (None, cursor, page),
                ));
            }
            let after = cursor?;
            match page(Some(after), false).await {
                Ok((text, next)) if next.is_some() => {
                    Some((Ok(axum::body::Bytes::from(text)), (None, next, page)))
                }
                Ok((text, _)) => Some((
                    Ok(axum::body::Bytes::from(format!("{text}]"))),
                    (None, None, page),
                )),
                Err(e) => {
                    tracing::error!(error = %e, "history/since stopped part way");
                    Some((Err(std::io::Error::other(e.to_string())), (None, None, page)))
                }
            }
        },
    );
    Ok((
        [(
            axum::http::header::CONTENT_TYPE,
            "application/json; charset=utf-8",
        )],
        axum::body::Body::from_stream(stream),
    )
        .into_response())
}

pub async fn history_series(
    State(state): State<AppState>,
    uri: Uri,
) -> ApiResult<Json<Vec<HistoryResource>>> {
    let p = Params::from_uri(&uri);
    let Some(series_id) = p.i64("seriesId") else {
        return Err(ApiError::BadRequest("seriesId is required".into()));
    };
    let season = p.i64("seasonNumber");
    let event = p.get("eventType").and_then(event_param);
    let include_series = p.bool("includeSeries", false);
    // The originals never embed the episode here: their series query does not
    // load it, so `includeEpisode` is accepted and ignored.
    let include_episode = false;
    let load = state
        .db()
        .call(move |conn| {
            if store::series::get(conn, series_id)?.is_none() {
                return Ok(None);
            }
            let entries = store::history::for_series(conn, series_id, season, event)?;
            let total = entries.len() as i64;
            Ok(Some(HistoryLoad::read(conn, entries, total, include_episode)?))
        })
        .await?
        .ok_or(ApiError::NotFound)?;
    let out = crate::blocking::run(move || load.render(include_series, include_episode)).await?;
    Ok(Json(out))
}

// --- queue (nothing downloads yet)

// --- blocklist

fn blocklist_resource(catalog: &Catalog, b: &BlocklistEntry) -> Value {
    let quality: crate::resources::QualityModelResource = b.quality.into();
    let custom_formats: Vec<CustomFormatRef> = match catalog.series.get(&b.series_id) {
        Some(s) => {
            let input = Input {
                quality: b.quality,
                languages: &b.languages,
                size: b.size.unwrap_or(0),
                indexer_flags: b.indexer_flags,
                release_type: b.release_type,
                series_original_language: s.original_language_id,
                movie: None,
                ..Input::default()
            };
            release_formats(catalog, s, b.source_title.as_deref().unwrap_or(""), input).0
        }
        None => Vec::new(),
    };
    json!({
        "id": b.id,
        "seriesId": b.series_id,
        "episodeIds": b.episode_ids,
        "sourceTitle": b.source_title,
        "languages": languages(&b.languages),
        "quality": quality,
        "customFormats": custom_formats,
        "date": unarr_core::time::format_utc(b.date),
        "protocol": b.protocol.as_str(),
        "indexer": b.indexer,
        "message": b.message,
        "series": catalog.series.get(&b.series_id).map(series_resource_plain),
    })
}

/// `protocols` as the framework binds a `DownloadProtocol[]`: repeated,
/// each a name in any case or a number; a list in one value is read as
/// flags, which no protocol is. `Err` is what it says of a value it does
/// not take, as recorded.
pub(crate) fn protocols_filter(p: &Params) -> Result<Option<Vec<unarr_core::model::Protocol>>, String> {
    use unarr_core::model::Protocol;
    let values = p.all("protocols");
    if values.is_empty() {
        return Ok(None);
    }
    let mut out = Vec::new();
    for value in values {
        let mut flags = 0i64;
        for part in value.split(',') {
            let part = part.trim();
            flags |= match part.to_ascii_lowercase().as_str() {
                "unknown" => 0,
                "usenet" => 1,
                "torrent" => 2,
                _ => match part.parse::<i64>() {
                    Ok(n) if !part.is_empty() => n,
                    // Blank, spaces included, is a value missing, not one
                    // that does not parse: ASP.NET's binder says it otherwise.
                    _ if value.trim().is_empty() => return Err(format!("The value '{value}' is invalid.")),
                    _ => return Err(format!("The value '{value}' is not valid.")),
                },
            };
        }
        out.push(match flags {
            0 => Protocol::Unknown,
            1 => Protocol::Usenet,
            2 => Protocol::Torrent,
            _ => return Err(format!("The value '{value}' is invalid.")),
        });
    }
    Ok(Some(out))
}

/// `GET /blocklist`: a page, of some series and some protocols when the
/// UI's filter asks (`seriesIds`, `protocols`).
pub async fn blocklist(State(state): State<AppState>, uri: Uri) -> ApiResult<Response> {
    let p = Params::from_uri(&uri);
    let paging = Paging::resolve(
        p.i64("page"),
        p.i64("pageSize"),
        p.get("sortKey"),
        p.descending(),
        &["date", "indexer", "series.sortTitle", "sourceTitle"],
        "date",
    );
    let series_ids = p.ints("seriesIds");
    let protocols = match protocols_filter(&p) {
        Ok(protocols) => protocols,
        Err(message) => {
            return Ok(crate::error::validation_problem(
                crate::error::BAD_REQUEST_TYPE_RFC7231,
                "application/problem+json; charset=utf-8",
                json!({"protocols": [message]}),
            ))
        }
    };
    let paging_for_db = paging.clone();
    let (entries, total, catalog) = state
        .db()
        .call(move |conn| {
            let (entries, total) =
                store::history::blocklist_paged(conn, &paging_for_db, &series_ids, protocols.as_deref())?;
            let ids: Vec<i64> = entries.iter().map(|b| b.series_id).collect();
            let catalog = Catalog::load_for(conn, &ids)?;
            Ok((entries, total, catalog))
        })
        .await?;
    let out = crate::blocking::run(move || {
        let records = entries.iter().map(|b| blocklist_resource(&catalog, b)).collect();
        PagingResource::new(&paging, total, records)
    })
    .await?;
    Ok(Json(out).into_response())
}

/// `DELETE /blocklist/{id}`: one entry removed.
pub async fn blocklist_delete(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    state
        .db()
        .call(move |conn| store::history::delete_blocklist(conn, &[id]))
        .await?;
    Ok(StatusCode::OK)
}

/// `DELETE /blocklist/bulk`: the entries of `ids` removed.
pub async fn blocklist_bulk_delete(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let ids: Vec<i64> = body["ids"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default();
    state
        .db()
        .call(move |conn| store::history::delete_blocklist(conn, &ids))
        .await?;
    Ok(Json(json!({})))
}

/// `POST /history/failed/{id}`: "Mark as failed" on a history row.
pub async fn history_mark_failed(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    if !state.downloads().mark_history_as_failed(id, state.now()).await? {
        return Err(ApiError::ModelNotFound(format!(
            "EpisodeHistory with ID {id} does not exist"
        )));
    }
    crate::routes::queue::broadcast_queue(&state);
    Ok(Json(json!({})))
}

#[cfg(test)]
mod tests {
    use super::*;
    use unarr_core::model::Protocol;

    /// The values Sonarr 4.0.20 took and refused for `protocols`, with its
    /// two wordings.
    #[test]
    fn the_protocol_filter_binds_as_the_original() {
        let filter = |query: &str| {
            let uri: Uri = format!("/blocklist?{query}").parse().unwrap();
            protocols_filter(&Params::from_uri(&uri))
        };
        assert_eq!(filter("page=1"), Ok(None));
        assert_eq!(filter("protocols=torrent"), Ok(Some(vec![Protocol::Torrent])));
        assert_eq!(filter("Protocols=Usenet"), Ok(Some(vec![Protocol::Usenet])));
        assert_eq!(
            filter("protocols=1&protocols=2"),
            Ok(Some(vec![Protocol::Usenet, Protocol::Torrent]))
        );
        assert_eq!(filter("protocols=0"), Ok(Some(vec![Protocol::Unknown])));
        assert_eq!(
            filter("protocols=bogus"),
            Err("The value 'bogus' is not valid.".into())
        );
        for refused in ["1,2", "3", ""] {
            assert_eq!(
                filter(&format!("protocols={refused}")),
                Err(format!("The value '{refused}' is invalid."))
            );
        }
    }
}
