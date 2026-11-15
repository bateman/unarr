//! `/api/v3/release`: interactive search, the RSS feed as a list, grabbing a
//! listed release and pushing one (`ReleaseController`,
//! `ReleasePushController`).

use axum::extract::State;
use axum::http::Uri;
use axum::Json;
use chrono::{DateTime, Utc};
use serde_json::{json, Map, Value};
use unarr_core::decision::{self, Decision, DecisionInput, RemoteEpisode};
use unarr_core::downloads::GrabError;
use unarr_core::indexers::rss::ReleaseInfo;
use unarr_core::model::{Protocol, ProviderKind};
use unarr_core::search::{SearchError, SearchService, Trigger};
use unarr_core::store;
use unarr_core::validation::ValidationFailure;

use crate::error::{ApiError, ApiResult};
use crate::query::Params;
use crate::resources::{languages, AlternateTitleResource, QualityModelResource};
use crate::state::AppState;

/// `GET /release?episodeId=` | `?seriesId=&seasonNumber=` | nothing (RSS).
pub async fn list(State(state): State<AppState>, uri: Uri) -> ApiResult<Json<Vec<Value>>> {
    let params = Params::from_uri(&uri);
    let now = state.now();
    let trigger = Trigger {
        user_invoked: true,
        interactive: true,
    };
    let search = SearchService::new(state.db().clone()).with_queue(state.downloads().queue_source());
    let decisions = if let Some(episode_id) = params.i64("episodeId") {
        search
            .episode_search(episode_id, trigger, now)
            .await
            .map_err(|e| search_error(e, "Episode search failed"))?
    } else if let (Some(series_id), Some(season)) = (params.i64("seriesId"), params.i64("seasonNumber")) {
        search
            .season_search(series_id, season, false, false, trigger, now)
            .await
            .map_err(|e| search_error(e, "Season search failed"))?
    } else {
        let releases = unarr_core::indexers::IndexerService::new(state.db().clone())
            .fetch_recent(now)
            .await?;
        let queue = state.downloads().queued_releases();
        state
            .db()
            .call(move |conn| {
                let catalog = decision::Catalog::load(conn, now)?.with_queue(queue);
                let lookup = decision::StoreLookup::new(conn);
                Ok(decision::decide(
                    &catalog,
                    &lookup,
                    now,
                    releases,
                    DecisionInput::Rss,
                ))
            })
            .await?
    };
    let prioritized = state
        .db()
        .call(move |conn| {
            let catalog = decision::Catalog::load(conn, now)?;
            Ok(decision::prioritize(&catalog, now, decisions))
        })
        .await?;
    Ok(Json(map_decisions(&state, &prioritized, now)))
}

fn search_error(e: SearchError, context: &str) -> ApiError {
    match e {
        SearchError::EpisodeNotFound(id) => {
            ApiError::ServerMessage(format!("Episode with ID {id} does not exist"))
        }
        SearchError::SeriesNotFound(id) => {
            ApiError::ServerMessage(format!("Series with ID {id} does not exist"))
        }
        SearchError::Failed(message) => ApiError::BadRequest(message),
        SearchError::Other(e) => {
            tracing::error!(error = %e, "{context}");
            ApiError::ServerMessage(e.to_string())
        }
    }
}

/// `POST /release`: grab a release a search listed. The download itself
/// arrives with the download clients; until then a cached release answers
/// as the originals do when no client can take it.
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
    let key = format!("{indexer_id}_{guid}");
    let Some(remote) = state.cached_release(&key) else {
        return Err(ApiError::NotFoundMessage(
            "Couldn't find requested release in cache, try searching again".into(),
        ));
    };
    let remote = resolve_grab_target(&state, remote, &body).await?;
    let download_client_id = body["downloadClientId"].as_i64();
    match state
        .downloads()
        .download_report(&remote, download_client_id, state.now())
        .await
    {
        Ok(_) => {}
        Err(GrabError::ReleaseDownload(message)) | Err(GrabError::ReleaseUnavailable(message)) => {
            tracing::error!(error = %message, "getting the release from the indexer failed");
            return Err(ApiError::ConflictMessage(
                "Getting release from indexer failed".into(),
            ));
        }
        Err(GrabError::NoClient(message)) => {
            return Err(ApiError::Internal(anyhow::anyhow!("{message}")));
        }
        Err(GrabError::Client(e)) => {
            return Err(ApiError::Internal(e.into_anyhow()));
        }
        Err(GrabError::Other(e)) => return Err(ApiError::Internal(e)),
    }
    Ok(Json(release_echo(&body)))
}

/// `DownloadRelease`'s filling in of the series and episodes: an override
/// from the UI, or the episode or series the request names when the cached
/// release matched none.
async fn resolve_grab_target(
    state: &AppState,
    mut remote: RemoteEpisode,
    body: &Value,
) -> ApiResult<RemoteEpisode> {
    let ints = |key: &str| -> Vec<i64> {
        body[key]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_i64).collect())
            .unwrap_or_default()
    };
    if body["shouldOverride"].as_bool().unwrap_or(false) {
        let series_id = body["seriesId"].as_i64();
        let episode_ids = ints("episodeIds");
        if series_id.is_none()
            || episode_ids.is_empty()
            || body["quality"].is_null()
            || !body["languages"].is_array()
        {
            return Err(ApiError::Internal(anyhow::anyhow!(
                "seriesId, episodeIds, quality and languages must be provided to override"
            )));
        }
        let series_id = series_id.unwrap_or(0);
        let (series, episodes) = state
            .db()
            .call(move |conn| {
                Ok((
                    store::series::get(conn, series_id)?,
                    store::episodes::list_episodes_by_ids(conn, &episode_ids)?,
                ))
            })
            .await?;
        remote.series = series;
        remote.episodes = episodes;
        let quality = &body["quality"];
        let mut parsed = remote.parsed.clone().unwrap_or_default();
        parsed.quality.quality = unarr_core::model::QualityModel {
            quality_id: quality["quality"]["id"].as_i64().unwrap_or(0),
            version: quality["revision"]["version"].as_i64().unwrap_or(1),
            real: quality["revision"]["real"].as_i64().unwrap_or(0),
            is_repack: quality["revision"]["isRepack"].as_bool().unwrap_or(false),
        };
        remote.parsed = Some(parsed);
        remote.languages = body["languages"]
            .as_array()
            .map(|a| a.iter().filter_map(|l| l["id"].as_i64()).collect())
            .unwrap_or_default();
    }
    let episode_id = body["episodeId"].as_i64();
    let series_id = body["seriesId"].as_i64();
    if remote.series.is_none() {
        if let Some(id) = episode_id {
            let (episode, series) = state
                .db()
                .call(move |conn| {
                    let episode = store::episodes::get_episode(conn, id)?;
                    let series = match &episode {
                        Some(e) => store::series::get(conn, e.series_id)?,
                        None => None,
                    };
                    Ok((episode, series))
                })
                .await?;
            let Some(episode) = episode else {
                return Err(ApiError::NotFound);
            };
            remote.series = series;
            remote.episodes = vec![episode];
        } else if let Some(id) = series_id {
            let parsed = remote.parsed.clone().unwrap_or_default();
            let (series, episodes) = state
                .db()
                .call(move |conn| {
                    let Some(series) = store::series::get(conn, id)? else {
                        return Ok((None, Vec::new()));
                    };
                    let lookup = unarr_core::decision::lookup::StoreLookup::new(conn);
                    let episodes = decision::mapping::get_episodes(&lookup, &parsed, &series, true, None)
                        .unwrap_or_default();
                    Ok((Some(series), episodes))
                })
                .await?;
            if series.is_none() {
                return Err(ApiError::NotFound);
            }
            if episodes.is_empty() {
                return Err(ApiError::NotFoundMessage(
                    "Unable to parse episodes in the release, will need to be manually provided".into(),
                ));
            }
            remote.series = series;
            remote.episodes = episodes;
        } else {
            return Err(ApiError::NotFoundMessage(
                "Unable to find matching series and episodes, will need to be manually provided".into(),
            ));
        }
    } else if remote.episodes.is_empty() {
        let parsed = remote.parsed.clone().unwrap_or_default();
        let series = remote.series.clone().expect("checked above");
        let mut episodes = state
            .db()
            .call(move |conn| {
                let lookup = unarr_core::decision::lookup::StoreLookup::new(conn);
                Ok(
                    decision::mapping::get_episodes(&lookup, &parsed, &series, true, None)
                        .unwrap_or_default(),
                )
            })
            .await?;
        if episodes.is_empty() {
            if let Some(id) = episode_id {
                episodes = state
                    .db()
                    .call(move |conn| store::episodes::get_episode(conn, id))
                    .await?
                    .into_iter()
                    .collect();
            }
        }
        remote.episodes = episodes;
    }
    if remote.episodes.is_empty() {
        return Err(ApiError::NotFoundMessage(
            "Unable to parse episodes in the release, will need to be manually provided".into(),
        ));
    }
    Ok(remote)
}

/// The posted `ReleaseResource` as the originals hand it back: every
/// value-typed property at its default, the posted ones over them, nulls
/// left out.
fn release_echo(body: &Value) -> Value {
    let mut m = Map::new();
    for (key, default) in [
        ("age", json!(0)),
        ("ageHours", json!(0)),
        ("ageMinutes", json!(0)),
        ("approved", json!(false)),
        ("customFormatScore", json!(0)),
        ("downloadAllowed", json!(false)),
        ("episodeRequested", json!(false)),
        ("fullSeason", json!(false)),
        ("indexerFlags", json!(0)),
        ("indexerId", json!(0)),
        ("isAbsoluteNumbering", json!(false)),
        ("isDaily", json!(false)),
        ("isPossibleSpecialEpisode", json!(false)),
        ("languageWeight", json!(0)),
        ("protocol", json!("unknown")),
        ("publishDate", json!("0001-01-01T00:00:00Z")),
        ("qualityWeight", json!(0)),
        ("rejected", json!(false)),
        ("releaseWeight", json!(0)),
        ("sceneSource", json!(false)),
        ("seasonNumber", json!(0)),
        ("size", json!(0)),
        ("special", json!(false)),
        ("temporarilyRejected", json!(false)),
        ("tvRageId", json!(0)),
        ("tvdbId", json!(0)),
    ] {
        m.insert(key.into(), default);
    }
    if let Some(posted) = body.as_object() {
        for (k, v) in posted {
            if !v.is_null() {
                m.insert(k.clone(), v.clone());
            }
        }
    }
    Value::Object(m)
}

/// `POST /release/push`: a release handed in by an indexer or Prowlarr,
/// decided as an RSS item would be.
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
        .filter(|d| *d != DateTime::<Utc>::UNIX_EPOCH && d.timestamp() > 0);
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
        download_url: download_url.clone(),
        info_url: text("infoUrl"),
        comment_url: text("commentUrl"),
        indexer_id: body["indexerId"].as_i64().unwrap_or(0),
        indexer: text("indexer").unwrap_or_default(),
        protocol,
        tvdb_id: body["tvdbId"].as_i64().unwrap_or(0),
        tv_rage_id: body["tvRageId"].as_i64().unwrap_or(0),
        imdb_id: text("imdbId"),
        publish_date: publish_date.unwrap(),
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
        info.indexer_flags = body["indexerFlags"].as_i64().unwrap_or(0);
    }
    // `ResolveIndexer`: a name finds the id and the other way round.
    let indexers = state
        .db()
        .call(|conn| store::providers::list_providers(conn, ProviderKind::Indexer))
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
    // Decided and processed under the originals' `PushLock`.
    let _one_push_at_a_time = state.push_lock().lock().await;
    let download_client_id = body["downloadClientId"].as_i64().filter(|id| *id > 0);
    let queue = state.downloads().queued_releases();
    let decisions = state
        .db()
        .call(move |conn| {
            let catalog = decision::Catalog::load(conn, now)?.with_queue(queue);
            let lookup = decision::StoreLookup::new(conn);
            Ok(decision::decide(
                &catalog,
                &lookup,
                now,
                vec![info],
                DecisionInput::Push,
            ))
        })
        .await?;
    let Some(decision) = decisions.into_iter().next() else {
        return Err(ApiError::Validation(vec![ValidationFailure::rule(
            "Title",
            "Unable to parse",
            "Unable to parse",
            body["title"].clone(),
        )]));
    };
    if decision.remote.parsed.is_none() {
        return Err(ApiError::Validation(vec![ValidationFailure::rule(
            "Title",
            "Unable to parse",
            "Unable to parse",
            body["title"].clone(),
        )]));
    }
    // `ProcessDecision`: an approved push goes to the client (or waits as a
    // pending release); the answer reports the decision either way.
    unarr_core::downloads::process::process_decision(state.downloads(), &decision, download_client_id, now)
        .await?;
    Ok(Json(map_decisions(&state, &[decision], now)))
}

/// `MapDecisions`: resources in order, cached for a later grab.
fn map_decisions(state: &AppState, decisions: &[Decision], now: DateTime<Utc>) -> Vec<Value> {
    decisions
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let resource = release_resource(d, i as i64, now);
            let key = format!("{}_{}", d.remote.release.indexer_id, d.remote.release.guid);
            state.cache_release(key, d.remote.clone());
            resource
        })
        .collect()
}

/// `ReleaseResource` as `ToResource` and `MapDecision` fill it. Nulls are
/// left out, as the originals' serializer does.
pub fn release_resource(decision: &Decision, release_weight: i64, now: DateTime<Utc>) -> Value {
    let remote: &RemoteEpisode = &decision.remote;
    let release = &remote.release;
    let parsed = remote.parsed_or_default();
    let quality = parsed.quality.quality;
    let age_ms = (now - release.publish_date).num_milliseconds() as f64;
    let mut m = Map::new();
    let mut put = |k: &str, v: Value| {
        if !v.is_null() {
            m.insert(k.to_string(), v);
        }
    };
    put("guid", json!(release.guid));
    put(
        "quality",
        serde_json::to_value(QualityModelResource::from(quality)).unwrap(),
    );
    put(
        "qualityWeight",
        json!(
            unarr_core::quality::default_profile_index(quality.quality_id) * 100
                + quality.real * 10
                + quality.version
        ),
    );
    put("age", json!((now - release.publish_date).num_days()));
    put("ageHours", json!(age_ms / 3_600_000.0));
    put("ageMinutes", json!(age_ms / 60_000.0));
    put("size", json!(release.size));
    put("indexerId", json!(release.indexer_id));
    put("indexer", json!(release.indexer));
    put("releaseGroup", json!(parsed.release_group));
    put("releaseHash", json!(parsed.release_hash));
    put("title", json!(release.title));
    put("fullSeason", json!(parsed.full_season));
    put("sceneSource", json!(false));
    put("seasonNumber", json!(parsed.season_number));
    put("languages", json!(languages(&remote.languages)));
    put("languageWeight", json!(0));
    put("airDate", json!(parsed.air_date));
    put("seriesTitle", json!(parsed.series_title));
    put("episodeNumbers", json!(parsed.episode_numbers));
    put("absoluteEpisodeNumbers", json!(parsed.absolute_episode_numbers));
    put(
        "mappedSeasonNumber",
        json!(remote.episodes.first().map(|e| e.season_number)),
    );
    put(
        "mappedEpisodeNumbers",
        json!(remote
            .episodes
            .iter()
            .map(|e| e.episode_number)
            .collect::<Vec<_>>()),
    );
    put(
        "mappedAbsoluteEpisodeNumbers",
        json!(remote
            .episodes
            .iter()
            .filter_map(|e| e.absolute_episode_number)
            .collect::<Vec<_>>()),
    );
    put("mappedSeriesId", json!(remote.series.as_ref().map(|s| s.id)));
    put(
        "mappedEpisodeInfo",
        json!(remote
            .episodes
            .iter()
            .map(|e| {
                let mut info = Map::new();
                info.insert("id".into(), json!(e.id));
                info.insert("seasonNumber".into(), json!(e.season_number));
                info.insert("episodeNumber".into(), json!(e.episode_number));
                if let Some(a) = e.absolute_episode_number {
                    info.insert("absoluteEpisodeNumber".into(), json!(a));
                }
                if let Some(t) = &e.title {
                    info.insert("title".into(), json!(t));
                }
                Value::Object(info)
            })
            .collect::<Vec<_>>()),
    );
    put("approved", json!(decision.approved()));
    put("temporarilyRejected", json!(decision.temporarily_rejected()));
    put("rejected", json!(decision.rejected()));
    put("tvdbId", json!(release.tvdb_id));
    put("tvRageId", json!(release.tv_rage_id));
    put("imdbId", json!(release.imdb_id));
    put(
        "rejections",
        json!(decision
            .rejections
            .iter()
            .map(|r| r.message.clone())
            .collect::<Vec<_>>()),
    );
    put(
        "publishDate",
        json!(unarr_core::time::format_utc(release.publish_date)),
    );
    put("commentUrl", json!(release.comment_url));
    put("downloadUrl", json!(release.download_url));
    put("infoUrl", json!(release.info_url));
    put("episodeRequested", json!(remote.episode_requested));
    put("downloadAllowed", json!(remote.download_allowed));
    put("releaseWeight", json!(release_weight));
    put(
        "customFormats",
        json!(remote
            .custom_formats
            .iter()
            .map(|f| json!({"id": f.id, "name": f.name}))
            .collect::<Vec<_>>()),
    );
    put("customFormatScore", json!(remote.custom_format_score));
    put(
        "sceneMapping",
        remote
            .scene_mapping
            .as_ref()
            .map(|sm| {
                serde_json::to_value(AlternateTitleResource {
                    title: sm.title.clone(),
                    season_number: sm.season_number,
                    scene_season_number: sm.scene_season_number,
                    scene_origin: sm.scene_origin.clone(),
                    comment: match (&sm.comment, &sm.filter_regex) {
                        (Some(c), _) if !c.trim().is_empty() => Some(c.clone()),
                        (_, Some(r)) if !r.trim().is_empty() => Some("Limited matching".into()),
                        _ => None,
                    },
                })
                .unwrap()
            })
            .unwrap_or(Value::Null),
    );
    put("magnetUrl", json!(release.magnet_url));
    put("infoHash", json!(release.info_hash));
    put("seeders", json!(release.seeders));
    put(
        "leechers",
        json!(match (release.peers, release.seeders) {
            (Some(p), Some(s)) => Some(p - s),
            _ => None,
        }),
    );
    put("protocol", json!(release.protocol.as_str()));
    put("indexerFlags", json!(release.indexer_flags));
    put("isDaily", json!(parsed.is_daily()));
    put("isAbsoluteNumbering", json!(parsed.is_absolute_numbering()));
    put(
        "isPossibleSpecialEpisode",
        json!(parsed.is_possible_special_episode()),
    );
    put("special", json!(parsed.special));
    Value::Object(m)
}
