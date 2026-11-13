//! Indexers, download clients, notifications, import lists, metadata, and
//! the small settings lists (delay/release profiles, remote path mappings,
//! custom filters, import list exclusions, auto tagging). Read-only.

use axum::extract::{Path, State};
use axum::Json;
use serde_json::{json, Map, Value};
use unarr_core::model::{Provider, ProviderKind};
use unarr_core::store;

use crate::error::{found, ApiResult};
use crate::schemas::{self, SchemaKind};
use crate::state::AppState;

/// A provider as the API shows it: the implementation's schema entry (its
/// capabilities and field metadata), the stored values filled into `fields`,
/// and the per-kind flags.
pub fn provider_resource(personality: unarr_core::Personality, p: &Provider) -> Value {
    let kind = SchemaKind::Provider(p.kind);
    let mut out: Map<String, Value> = match schemas::lookup(personality, kind, &p.implementation) {
        Some(schema) => {
            let mut m = schema.as_object().cloned().unwrap_or_default();
            m.remove("presets");
            m.insert(
                "fields".into(),
                Value::Array(schemas::render_fields(schema, &p.settings)),
            );
            m
        }
        None => {
            // Unknown implementation (newer original): without its schema
            // nothing says which fields are secrets, so every text value is
            // masked. Too much hidden beats a password in clear.
            let fields: Vec<Value> = p
                .settings
                .as_object()
                .map(|s| {
                    s.iter()
                        .enumerate()
                        .map(|(i, (k, v))| {
                            let value = match v {
                                Value::String(text) if !text.is_empty() => json!("********"),
                                other => other.clone(),
                            };
                            json!({"order": i, "name": k, "label": k, "value": value, "type": "textbox", "advanced": false, "privacy": "normal", "isFloat": false})
                        })
                        .collect()
                })
                .unwrap_or_default();
            let mut m = Map::new();
            m.insert("implementation".into(), json!(p.implementation));
            m.insert("implementationName".into(), json!(p.implementation));
            m.insert("configContract".into(), json!(p.config_contract));
            m.insert(
                "infoLink".into(),
                json!(format!(
                    "https://wiki.servarr.com/sonarr/supported#{}",
                    p.implementation.to_lowercase()
                )),
            );
            m.insert("fields".into(), Value::Array(fields));
            m
        }
    };
    out.insert("id".into(), json!(p.id));
    out.insert("name".into(), json!(p.name));
    out.insert("tags".into(), json!(p.tags));
    if let Some(attrs) = p.attributes.as_object() {
        for (k, v) in attrs {
            if !v.is_null() {
                out.insert(k.clone(), v.clone());
            }
        }
    }
    Value::Object(out)
}

async fn list_kind(state: &AppState, kind: ProviderKind) -> anyhow::Result<Vec<Value>> {
    let mut providers = state
        .db()
        .call(move |conn| store::providers::list_providers(conn, kind))
        .await?;
    // The originals list providers by name.
    providers.sort_by(|a, b| unarr_core::collation::compare(&a.name, &b.name));
    Ok(providers
        .iter()
        .map(|p| provider_resource(state.personality(), p))
        .collect())
}

async fn get_kind(state: &AppState, kind: ProviderKind, id: i64) -> ApiResult<Json<Value>> {
    let p = state
        .db()
        .call(move |conn| store::providers::get_provider(conn, kind, id))
        .await?;
    Ok(Json(provider_resource(state.personality(), &found(p)?)))
}

macro_rules! provider_routes {
    ($list:ident, $get:ident, $schema:ident, $kind:expr) => {
        pub async fn $list(State(state): State<AppState>) -> ApiResult<Json<Vec<Value>>> {
            Ok(Json(list_kind(&state, $kind).await?))
        }
        pub async fn $get(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
            get_kind(&state, $kind, id).await
        }
        /// The implementations available, with their field schemas.
        pub async fn $schema(State(state): State<AppState>) -> Json<Vec<Value>> {
            Json(schemas::all(state.personality(), SchemaKind::Provider($kind)).to_vec())
        }
    };
}

provider_routes!(indexers, indexer, indexer_schema, ProviderKind::Indexer);
provider_routes!(
    download_clients,
    download_client,
    download_client_schema,
    ProviderKind::DownloadClient
);
provider_routes!(
    notifications,
    notification,
    notification_schema,
    ProviderKind::Notification
);
provider_routes!(
    import_lists,
    import_list,
    import_list_schema,
    ProviderKind::ImportList
);
provider_routes!(metadata_list, metadata, metadata_schema, ProviderKind::Metadata);

// --- settings lists

fn delay_profile_resource(d: &unarr_core::model::DelayProfile) -> Value {
    json!({
        "id": d.id,
        "enableUsenet": d.enable_usenet,
        "enableTorrent": d.enable_torrent,
        "preferredProtocol": d.preferred_protocol.as_str(),
        "usenetDelay": d.usenet_delay,
        "torrentDelay": d.torrent_delay,
        "bypassIfHighestQuality": d.bypass_if_highest_quality,
        "bypassIfAboveCustomFormatScore": d.bypass_if_above_custom_format_score,
        "minimumCustomFormatScore": d.minimum_custom_format_score,
        "order": d.order,
        "tags": d.tags,
    })
}

pub async fn delay_profiles(State(state): State<AppState>) -> ApiResult<Json<Vec<Value>>> {
    let all = state
        .db()
        .call(|c| store::providers::list_delay_profiles(c))
        .await?;
    Ok(Json(all.iter().map(delay_profile_resource).collect()))
}

pub async fn delay_profile(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let all = state
        .db()
        .call(|c| store::providers::list_delay_profiles(c))
        .await?;
    Ok(Json(found(
        all.iter().find(|d| d.id == id).map(delay_profile_resource),
    )?))
}

fn release_profile_resource(r: &unarr_core::model::ReleaseProfile) -> Value {
    json!({
        "id": r.id,
        "name": r.name,
        "enabled": r.enabled,
        "required": r.required,
        "ignored": r.ignored,
        "indexerId": r.indexer_id,
        "tags": r.tags,
    })
}

pub async fn release_profiles(State(state): State<AppState>) -> ApiResult<Json<Vec<Value>>> {
    let all = state
        .db()
        .call(|c| store::providers::list_release_profiles(c))
        .await?;
    Ok(Json(all.iter().map(release_profile_resource).collect()))
}

pub async fn release_profile(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let all = state
        .db()
        .call(|c| store::providers::list_release_profiles(c))
        .await?;
    Ok(Json(found(
        all.iter().find(|r| r.id == id).map(release_profile_resource),
    )?))
}

fn mapping_resource(m: &unarr_core::model::RemotePathMapping) -> Value {
    json!({"id": m.id, "host": m.host, "remotePath": m.remote_path, "localPath": m.local_path})
}

pub async fn remote_path_mappings(State(state): State<AppState>) -> ApiResult<Json<Vec<Value>>> {
    let all = state
        .db()
        .call(|c| store::providers::list_remote_path_mappings(c))
        .await?;
    Ok(Json(all.iter().map(mapping_resource).collect()))
}

pub async fn remote_path_mapping(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    let all = state
        .db()
        .call(|c| store::providers::list_remote_path_mappings(c))
        .await?;
    Ok(Json(found(
        all.iter().find(|m| m.id == id).map(mapping_resource),
    )?))
}

fn custom_filter_resource(f: &unarr_core::model::CustomFilter) -> Value {
    json!({"id": f.id, "type": f.filter_type, "label": f.label, "filters": f.filters})
}

pub async fn custom_filters(State(state): State<AppState>) -> ApiResult<Json<Vec<Value>>> {
    let all = state
        .db()
        .call(|c| store::providers::list_custom_filters(c))
        .await?;
    Ok(Json(all.iter().map(custom_filter_resource).collect()))
}

pub async fn custom_filter(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let all = state
        .db()
        .call(|c| store::providers::list_custom_filters(c))
        .await?;
    Ok(Json(found(
        all.iter().find(|f| f.id == id).map(custom_filter_resource),
    )?))
}

pub async fn import_list_exclusions(State(state): State<AppState>) -> ApiResult<Json<Vec<Value>>> {
    let all = state
        .db()
        .call(|c| store::providers::list_import_list_exclusions(c))
        .await?;
    Ok(Json(
        all.iter()
            .map(|e| json!({"id": e.id, "tvdbId": e.tvdb_id, "title": e.title}))
            .collect(),
    ))
}

/// `GET /importlistexclusion/paged`: what the settings page lists.
pub async fn import_list_exclusions_paged(
    State(state): State<AppState>,
    uri: axum::http::Uri,
) -> ApiResult<Json<crate::resources::PagingResource<Value>>> {
    let p = crate::query::Params::from_uri(&uri);
    let mut paging = unarr_core::store::paging::Paging::resolve(
        p.i64("page"),
        p.i64("pageSize"),
        p.get("sortKey"),
        p.descending(),
        &["id", "title", "tvdbId"],
        "id",
    );
    // A page size of zero is taken at its word: no records.
    if p.i64("pageSize") == Some(0) {
        paging.page_size = 0;
    }
    let mut all = state
        .db()
        .call(|c| store::providers::list_import_list_exclusions(c))
        .await?;
    match paging.sort_key.as_str() {
        // The database's binary collation: capitals before lower case.
        "title" => all.sort_by(|a, b| a.title.as_bytes().cmp(b.title.as_bytes())),
        "tvdbId" => all.sort_by_key(|e| e.tvdb_id),
        _ => all.sort_by_key(|e| e.id),
    }
    if paging.descending {
        all.reverse();
    }
    let records = paging
        .slice(&all)
        .iter()
        .map(|e| json!({"id": e.id, "tvdbId": e.tvdb_id, "title": e.title}))
        .collect();
    Ok(Json(crate::resources::PagingResource::new(
        &paging,
        all.len() as i64,
        records,
    )))
}

/// Auto-tagging rules; specifications are rendered like custom format ones.
fn auto_tagging_resource(personality: unarr_core::Personality, a: &unarr_core::model::AutoTagging) -> Value {
    let specs: Vec<Value> = a
        .specifications
        .as_array()
        .map(|list| {
            list.iter()
                .map(|spec| {
                    let implementation = spec["type"].as_str().unwrap_or("");
                    let body = &spec["body"];
                    let mut out = json!({
                        "name": body["name"],
                        "implementation": implementation,
                        "implementationName": body["implementationName"],
                        "negate": body["negate"].as_bool().unwrap_or(false),
                        "required": body["required"].as_bool().unwrap_or(false),
                        "fields": [],
                    });
                    if let Some(schema) =
                        schemas::lookup(personality, SchemaKind::AutoTaggingSpecification, implementation)
                    {
                        out["implementationName"] = schema["implementationName"].clone();
                        out["fields"] = Value::Array(schemas::render_fields(schema, body));
                    }
                    out
                })
                .collect()
        })
        .unwrap_or_default();
    json!({
        "id": a.id,
        "name": a.name,
        "removeTagsAutomatically": a.remove_tags_automatically,
        "tags": a.tags,
        "specifications": specs,
    })
}

pub async fn auto_tagging(State(state): State<AppState>) -> ApiResult<Json<Vec<Value>>> {
    let all = state
        .db()
        .call(|c| store::providers::list_auto_tagging(c))
        .await?;
    Ok(Json(
        all.iter()
            .map(|a| auto_tagging_resource(state.personality(), a))
            .collect(),
    ))
}

pub async fn auto_tagging_schema(State(state): State<AppState>) -> Json<Vec<Value>> {
    Json(schemas::all(state.personality(), SchemaKind::AutoTaggingSpecification).to_vec())
}

pub async fn custom_format_schema(State(state): State<AppState>) -> Json<Vec<Value>> {
    Json(schemas::all(state.personality(), SchemaKind::CustomFormatSpecification).to_vec())
}

/// `PUT /metadata/{id}`: Settings > Metadata saves a consumer's switch and
/// options. They are kept, so the page works and a later export finds
/// them; no metadata file is written (the consumers are not driven).
pub async fn metadata_update(
    State(state): State<AppState>,
    Path(route_id): Path<i64>,
    Json(mut body): Json<Value>,
) -> ApiResult<axum::response::Response> {
    use axum::response::IntoResponse;
    if !body.is_object() {
        return Err(crate::error::ApiError::BadRequest(
            "The request body must be a JSON object".into(),
        ));
    }
    if body["id"].as_i64().unwrap_or(0) == 0 {
        body["id"] = json!(route_id);
    }
    let id = body["id"].as_i64().unwrap_or(0);
    // `ProviderControllerBase`'s shared rules run before the provider is
    // read, so a name another consumer has is a 400, not a 404.
    let all = state
        .db()
        .call(|conn| store::providers::list_providers(conn, ProviderKind::Metadata))
        .await?;
    let name = body["name"].as_str().unwrap_or("");
    let mut failures = Vec::new();
    if name.trim().is_empty() {
        failures.push(unarr_core::validation::ValidationFailure::not_empty(
            "Name",
            body["name"].clone(),
        ));
    }
    if all
        .iter()
        .any(|p| p.name.eq_ignore_ascii_case(name) && p.id != id)
    {
        failures.push(unarr_core::validation::ValidationFailure::rule(
            "Name",
            "PredicateValidator",
            "Should be unique",
            body["name"].clone(),
        ));
    }
    if !failures.is_empty() {
        return Err(crate::error::ApiError::Validation(failures));
    }
    let existing = found(all.into_iter().find(|p| p.id == id))?;
    let mut updated = existing.clone();
    if let Some(name) = body["name"].as_str() {
        updated.name = name.to_string();
    }
    if let Some(schema) = schemas::lookup(
        state.personality(),
        SchemaKind::Provider(ProviderKind::Metadata),
        &existing.implementation,
    ) {
        updated.settings = schemas::read_fields(schema, &body["fields"], Some(&existing.settings));
    }
    if let Some(tags) = body["tags"].as_array() {
        updated.tags = tags.iter().filter_map(Value::as_i64).collect();
    }
    if let (Some(enable), Some(attributes)) = (body["enable"].as_bool(), updated.attributes.as_object_mut()) {
        attributes.insert("enable".into(), json!(enable));
    }
    if updated != existing {
        let to_store = updated.clone();
        state
            .db()
            .call(move |conn| store::providers::update_provider(conn, &to_store))
            .await?;
        state.hub().broadcast(
            "metadata",
            json!({"action": "updated", "resource": provider_resource(state.personality(), &updated)}),
        );
    }
    Ok((
        axum::http::StatusCode::ACCEPTED,
        Json(provider_resource(state.personality(), &updated)),
    )
        .into_response())
}

/// `POST /metadata/test`: a metadata consumer has nothing to connect to,
/// so the test passes on anything the resource rules accept.
pub async fn metadata_test(Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    if !body.is_object() {
        return Err(crate::error::ApiError::BadRequest(
            "The request body must be a JSON object".into(),
        ));
    }
    Ok(Json(json!({})))
}

/// `POST /metadata/testall`: every enabled consumer, each valid.
pub async fn metadata_test_all(State(state): State<AppState>) -> ApiResult<Json<Vec<Value>>> {
    let mut providers = state
        .db()
        .call(|conn| store::providers::list_providers(conn, ProviderKind::Metadata))
        .await?;
    providers.sort_by_key(|p| p.id);
    Ok(Json(
        providers
            .iter()
            .filter(|p| p.attributes["enable"].as_bool().unwrap_or(false))
            .map(|p| json!({"id": p.id, "isValid": true, "validationFailures": []}))
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_a_known_implementation_from_its_schema() {
        let p = Provider {
            kind: ProviderKind::Indexer,
            id: 1,
            name: "Fixture Newznab".into(),
            implementation: "Newznab".into(),
            config_contract: "NewznabSettings".into(),
            settings: json!({"baseUrl": "http://indexer.invalid", "apiKey": "k", "categories": [5030]}),
            tags: vec![],
            attributes: json!({"enableRss": true, "priority": 25}),
        };
        let r = provider_resource(unarr_core::Personality::Sonarr, &p);
        assert_eq!(r["supportsRss"], true);
        assert_eq!(r["protocol"], "usenet");
        assert_eq!(r["priority"], 25);
        assert_eq!(r["implementationName"], "Newznab");
        assert!(r.get("presets").is_none());
        let fields = r["fields"].as_array().unwrap();
        assert_eq!(
            fields.iter().find(|f| f["name"] == "apiKey").unwrap()["value"],
            "********"
        );

        let unknown = Provider {
            implementation: "FutureIndexer".into(),
            ..p
        };
        let r = provider_resource(unarr_core::Personality::Sonarr, &unknown);
        assert_eq!(r["fields"].as_array().unwrap().len(), 3);
    }
}
