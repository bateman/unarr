//! Radarr's catalogues and profiles: where its shapes differ from the
//! handlers both personalities share (its qualities carry a modifier, its
//! profiles a language, its tags name movies).

use axum::extract::{Path, State};
use axum::http::Uri;
use axum::Json;
use serde_json::{json, Value};
use unarr_core::model::{CustomFormat, ProfileItem, ProviderKind, QualityProfile};
use unarr_core::movies::quality::{
    MovieLanguage, MovieQuality, RADARR_INDEXER_FLAGS, RADARR_LANGUAGES, RADARR_QUALITIES,
};
use unarr_core::movies::store as movies;
use unarr_core::store;
use unarr_core::store::paging::Paging;

use crate::error::{found, ApiResult};
use crate::query::Params;
use crate::resources::PagingResource;
use crate::state::AppState;

// --- tags

pub async fn tag_details(State(state): State<AppState>) -> ApiResult<Json<Vec<Value>>> {
    Ok(Json(tag_details_all(&state).await?))
}

pub async fn tag_detail(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let all = tag_details_all(&state).await?;
    Ok(Json(found(all.into_iter().find(|t| t["id"] == id))?))
}

async fn tag_details_all(state: &AppState) -> anyhow::Result<Vec<Value>> {
    state
        .db()
        .call(|conn| {
            let mut tags = store::catalog::list_tags(conn)?;
            tags.sort_by(|a, b| a.label.cmp(&b.label));
            let all_movies = movies::list_movies(conn)?;
            let delay = store::providers::list_delay_profiles(conn)?;
            let release = store::providers::list_release_profiles(conn)?;
            let auto = store::providers::list_auto_tagging(conn)?;
            let providers = |kind| store::providers::list_providers(conn, kind);
            let indexers = providers(ProviderKind::Indexer)?;
            let clients = providers(ProviderKind::DownloadClient)?;
            let notifications = providers(ProviderKind::Notification)?;
            let lists = providers(ProviderKind::ImportList)?;
            let with = |ids: Vec<(i64, &Vec<i64>)>, tag: i64| -> Vec<i64> {
                ids.into_iter()
                    .filter(|(_, tags)| tags.contains(&tag))
                    .map(|(id, _)| id)
                    .collect()
            };
            Ok(tags
                .iter()
                .map(|t| {
                    json!({
                        "id": t.id,
                        "label": t.label,
                        "delayProfileIds": with(delay.iter().map(|d| (d.id, &d.tags)).collect(), t.id),
                        "importListIds": with(lists.iter().map(|p| (p.id, &p.tags)).collect(), t.id),
                        "notificationIds": with(notifications.iter().map(|p| (p.id, &p.tags)).collect(), t.id),
                        "releaseProfileIds": with(release.iter().map(|r| (r.id, &r.tags)).collect(), t.id),
                        "indexerIds": with(indexers.iter().map(|p| (p.id, &p.tags)).collect(), t.id),
                        "downloadClientIds": with(clients.iter().map(|p| (p.id, &p.tags)).collect(), t.id),
                        "autoTagIds": with(auto.iter().map(|a| (a.id, &a.tags)).collect(), t.id),
                        "movieIds": with(all_movies.iter().map(|m| (m.id, &m.tags)).collect(), t.id),
                    })
                })
                .collect())
        })
        .await
}

// --- languages and indexer flags

pub fn language_resource(id: i64) -> Value {
    json!({"id": id, "name": MovieLanguage::name_of(id)})
}

pub async fn languages() -> Json<Vec<Value>> {
    Json(
        RADARR_LANGUAGES
            .iter()
            .map(|l| json!({"id": l.id, "name": l.name, "nameLower": l.name.to_lowercase()}))
            .collect(),
    )
}

pub async fn language(Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let l = found(MovieLanguage::by_id(id))?;
    Ok(Json(
        json!({"id": l.id, "name": l.name, "nameLower": l.name.to_lowercase()}),
    ))
}

pub async fn indexer_flags() -> Json<Vec<Value>> {
    Json(
        RADARR_INDEXER_FLAGS
            .iter()
            .map(|(id, name)| json!({"id": id, "name": name, "nameLower": name.to_lowercase()}))
            .collect(),
    )
}

// --- quality definitions and profiles

fn quality_definition_resource(d: &unarr_core::model::QualityDefinition) -> Value {
    let mut out = json!({
        "id": d.id,
        "quality": MovieQuality::by_id(d.quality_id),
        "title": d.title,
        "weight": MovieQuality::weight(d.quality_id),
    });
    for (key, v) in [
        ("minSize", d.min_size),
        ("maxSize", d.max_size),
        ("preferredSize", d.preferred_size),
    ] {
        if let Some(v) = v {
            out[key] = json!(v);
        }
    }
    out
}

/// The definitions in weight order, as the originals list them; equal
/// weights (WEBDL and WEBRip) keep their place in the table.
pub async fn quality_definitions(State(state): State<AppState>) -> ApiResult<Json<Vec<Value>>> {
    let mut defs = state
        .db()
        .call(|c| store::catalog::list_quality_definitions(c))
        .await?;
    let place = |id: i64| {
        RADARR_QUALITIES
            .iter()
            .position(|q| q.quality.id == id)
            .unwrap_or(usize::MAX)
    };
    defs.sort_by_key(|d| place(d.quality_id));
    Ok(Json(defs.iter().map(quality_definition_resource).collect()))
}

pub async fn quality_definition(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    let defs = state
        .db()
        .call(|c| store::catalog::list_quality_definitions(c))
        .await?;
    Ok(Json(found(
        defs.iter().find(|d| d.id == id).map(quality_definition_resource),
    )?))
}

/// `QualityDefinitionLimits`: megabytes per minute.
pub async fn quality_definition_limits() -> Json<Value> {
    Json(json!({"min": 0, "max": 2000}))
}

fn profile_item_resource(item: &ProfileItem) -> Value {
    let mut out = json!({
        "items": item.items.iter().map(profile_item_resource).collect::<Vec<_>>(),
        "allowed": item.allowed,
    });
    if let Some(q) = item.quality {
        out["quality"] = json!(MovieQuality::by_id(q));
    }
    if let Some(id) = item.id {
        out["id"] = json!(id);
    }
    if let Some(name) = &item.name {
        out["name"] = json!(name);
    }
    out
}

fn quality_profile_resource(p: &QualityProfile, language: i64, formats: &[CustomFormat]) -> Value {
    json!({
        "id": p.id,
        "name": p.name,
        "upgradeAllowed": p.upgrade_allowed,
        "cutoff": p.cutoff,
        "items": p.items.iter().map(profile_item_resource).collect::<Vec<_>>(),
        "minFormatScore": p.min_format_score,
        "cutoffFormatScore": p.cutoff_format_score,
        "minUpgradeFormatScore": p.min_upgrade_format_score,
        "formatItems": p.format_items.iter().map(|fi| json!({
            "format": fi.format,
            "name": formats.iter().find(|f| f.id == fi.format).map(|f| f.name.clone()),
            "score": fi.score,
        })).collect::<Vec<_>>(),
        "language": language_resource(language),
    })
}

async fn profile_resources(state: &AppState) -> anyhow::Result<Vec<Value>> {
    state
        .db()
        .call(|conn| {
            let profiles = store::catalog::list_quality_profiles(conn)?;
            let formats = store::catalog::list_custom_formats(conn)?;
            let languages = movies::profile_languages(conn)?;
            Ok(profiles
                .iter()
                .map(|p| quality_profile_resource(p, languages.get(&p.id).copied().unwrap_or(-1), &formats))
                .collect())
        })
        .await
}

pub async fn quality_profiles(State(state): State<AppState>) -> ApiResult<Json<Vec<Value>>> {
    Ok(Json(profile_resources(&state).await?))
}

pub async fn quality_profile(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let all = profile_resources(&state).await?;
    Ok(Json(found(all.into_iter().find(|p| p["id"] == id))?))
}

/// `GetDefaultProfile`: every quality in weight order, those sharing a
/// weight as a group, nothing allowed, every custom format at zero, the
/// original language.
pub async fn quality_profile_schema(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let mut items = Vec::new();
    let mut group_id = 1000;
    let mut weights: Vec<i64> = Vec::new();
    for d in RADARR_QUALITIES {
        if !weights.contains(&d.weight) {
            weights.push(d.weight);
        }
    }
    for w in weights {
        let group: Vec<_> = RADARR_QUALITIES.iter().filter(|d| d.weight == w).collect();
        let quality = |id: i64| json!({"allowed": false, "items": [], "quality": MovieQuality::by_id(id)});
        if group.len() == 1 {
            items.push(quality(group[0].quality.id));
            continue;
        }
        items.push(json!({
            "allowed": false,
            "id": group_id,
            "items": group.iter().map(|d| quality(d.quality.id)).collect::<Vec<_>>(),
            "name": format!("WEB {}p", group[0].quality.resolution),
        }));
        group_id += 1;
    }
    let formats = state
        .db()
        .call(|c| store::catalog::list_custom_formats(c))
        .await?;
    Ok(Json(json!({
        "cutoff": 0,
        "cutoffFormatScore": 0,
        "formatItems": formats.iter().map(|f| json!({"format": f.id, "name": f.name, "score": 0})).collect::<Vec<_>>(),
        "items": items,
        "language": language_resource(-2),
        "minFormatScore": 0,
        "minUpgradeFormatScore": 1,
        "name": "",
        "upgradeAllowed": false,
    })))
}

// --- import exclusions

fn exclusion_resource(e: &unarr_core::movies::model::ImportExclusion) -> Value {
    json!({
        "id": e.id,
        "tmdbId": e.tmdb_id,
        "movieTitle": e.movie_title,
        "movieYear": e.movie_year,
    })
}

pub async fn exclusions(State(state): State<AppState>) -> ApiResult<Json<Vec<Value>>> {
    let all = state.db().call(|c| movies::list_import_exclusions(c)).await?;
    Ok(Json(all.iter().map(exclusion_resource).collect()))
}

pub async fn exclusion(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let all = state.db().call(|c| movies::list_import_exclusions(c)).await?;
    Ok(Json(found(
        all.iter().find(|e| e.id == id).map(exclusion_resource),
    )?))
}

pub async fn exclusions_paged(
    State(state): State<AppState>,
    uri: Uri,
) -> ApiResult<Json<PagingResource<Value>>> {
    let p = Params::from_uri(&uri);
    let paging = Paging::resolve(
        p.i64("page"),
        p.i64("pageSize"),
        p.get("sortKey"),
        p.descending(),
        &["id", "tmdbId", "movieTitle", "movieYear"],
        "id",
    );
    let mut all = state.db().call(|c| movies::list_import_exclusions(c)).await?;
    // Ties keep id order, whatever the direction.
    match paging.sort_key.as_str() {
        "tmdbId" => all.sort_by_key(|e| e.tmdb_id),
        "movieTitle" => all.sort_by(|a, b| {
            unarr_core::collation::compare(
                a.movie_title.as_deref().unwrap_or(""),
                b.movie_title.as_deref().unwrap_or(""),
            )
        }),
        "movieYear" => all.sort_by_key(|e| e.movie_year),
        _ => {}
    }
    if paging.descending {
        let mut groups: Vec<Vec<_>> = Vec::new();
        for e in all {
            let same = groups.last().and_then(|g: &Vec<_>| g.first()).is_some_and(
                |f: &unarr_core::movies::model::ImportExclusion| match paging.sort_key.as_str() {
                    "tmdbId" => f.tmdb_id == e.tmdb_id,
                    "movieTitle" => f.movie_title == e.movie_title,
                    "movieYear" => f.movie_year == e.movie_year,
                    _ => false,
                },
            );
            if same {
                groups.last_mut().unwrap().push(e);
            } else {
                groups.push(vec![e]);
            }
        }
        groups.reverse();
        all = groups.into_iter().flatten().collect();
    }
    let total = all.len() as i64;
    let records = paging.slice(&all).iter().map(exclusion_resource).collect();
    Ok(Json(PagingResource::new(&paging, total, records)))
}
