//! Writing indexers: `POST`, `PUT`, `DELETE`, `test`, `testall`, `action`
//! and `bulk` on `/api/v3/indexer`, after the originals'
//! `ProviderControllerBase`. This is what Prowlarr drives and what the
//! settings page saves through. Validation runs in the originals' order
//! (resource rules, then settings, then the connection test) and fails in
//! their shapes, since the UI and Prowlarr read the failures.

use axum::extract::{Path, State};
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Map, Value};
use unarr_core::indexers::{self, IndexerDefinition, IndexerKind, NewznabSettings};
use unarr_core::model::{Provider, ProviderKind};
use unarr_core::store;
use unarr_core::validation::{self, ValidationFailure};

use crate::error::{ApiError, ApiResult};
use crate::query::Params;
use crate::routes::providers::provider_resource;
use crate::schemas::{self, SchemaKind};
use crate::state::AppState;

const KIND: ProviderKind = ProviderKind::Indexer;

/// Per-kind flags the resource carries next to `fields`, with the value a
/// missing one takes.
const ATTRIBUTES: &[(&str, Value)] = &[
    ("enableRss", Value::Bool(false)),
    ("enableAutomaticSearch", Value::Bool(false)),
    ("enableInteractiveSearch", Value::Bool(false)),
    ("priority", Value::Null),
    ("downloadClientId", Value::Null),
    ("seasonSearchMaximumSingleEpisodeAge", Value::Null),
];

/// What `*/bulk` may change.
const BULK_ATTRIBUTES: &[&str] = &[
    "enableRss",
    "enableAutomaticSearch",
    "enableInteractiveSearch",
    "priority",
];

async fn list(state: &AppState) -> anyhow::Result<Vec<Provider>> {
    state
        .db()
        .call(|conn| store::providers::list_providers(conn, KIND))
        .await
}

async fn find(state: &AppState, id: i64) -> anyhow::Result<Option<Provider>> {
    state
        .db()
        .call(move |conn| store::providers::get_provider(conn, KIND, id))
        .await
}

/// `RestController.ValidateResource` plus the shared rules of
/// `ProviderControllerBase` and `IndexerController`.
async fn validate_resource(
    state: &AppState,
    body: &Value,
    id: i64,
    method: &str,
    test: bool,
) -> ApiResult<()> {
    let mut failures = Vec::new();
    let name = body["name"].as_str().unwrap_or("");
    if name.trim().is_empty() {
        failures.push(ValidationFailure::not_empty("Name", body["name"].clone()));
    }
    let taken = list(state)
        .await?
        .iter()
        .any(|p| p.name.eq_ignore_ascii_case(name) && p.id != id);
    if taken {
        failures.push(ValidationFailure::rule(
            "Name",
            "PredicateValidator",
            "Should be unique",
            body["name"].clone(),
        ));
    }
    if body["implementation"].as_str().unwrap_or("").trim().is_empty() {
        failures.push(ValidationFailure::not_empty(
            "Implementation",
            body["implementation"].clone(),
        ));
    }
    if body["configContract"].as_str().unwrap_or("").trim().is_empty() {
        failures.push(ValidationFailure::not_empty(
            "ConfigContract",
            body["configContract"].clone(),
        ));
    }
    let download_client_id = body["downloadClientId"].as_i64().unwrap_or(0);
    if download_client_id > 0 {
        let exists = state
            .db()
            .call(move |conn| {
                store::providers::get_provider(conn, ProviderKind::DownloadClient, download_client_id)
            })
            .await?
            .is_some();
        if !exists {
            failures.push(ValidationFailure::rule(
                "DownloadClientId",
                "DownloadClientExistsValidator",
                "Download Client does not exist",
                body["downloadClientId"].clone(),
            ));
        }
    }
    if method == "POST" && !test && !body["fields"].is_array() {
        failures.push(ValidationFailure::not_null("Fields"));
    }
    if method == "PUT" && id <= 0 {
        failures.push(ValidationFailure::greater_than("Id", json!(id), 0));
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(ApiError::Validation(failures))
    }
}

/// `ProviderResourceMapper.ToModel`: the stored provider a resource describes.
fn to_model(body: &Value, id: i64, existing: Option<&Provider>) -> ApiResult<Provider> {
    let implementation = body["implementation"].as_str().unwrap_or("").to_string();
    let Some(schema) = schemas::lookup(SchemaKind::Provider(KIND), &implementation) else {
        return Err(ApiError::BadRequest(format!(
            "Unknown implementation {implementation:?}"
        )));
    };
    let settings = schemas::read_fields(schema, &body["fields"], existing.map(|e| &e.settings));
    let mut attributes = Map::new();
    for (name, default) in ATTRIBUTES {
        let value = body.get(*name).cloned().unwrap_or(Value::Null);
        let value = match (value, default) {
            (Value::Null, Value::Null) => json!(0),
            (Value::Null, d) => d.clone(),
            (v, _) => v,
        };
        attributes.insert((*name).to_string(), value);
    }
    let tags: Vec<i64> = body["tags"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default();
    Ok(Provider {
        kind: KIND,
        id,
        name: body["name"].as_str().unwrap_or("").to_string(),
        implementation,
        config_contract: body["configContract"].as_str().unwrap_or("").to_string(),
        settings,
        tags,
        attributes: Value::Object(attributes),
    })
}

fn kind_of(p: &Provider) -> Option<IndexerKind> {
    IndexerKind::from_implementation(&p.implementation)
}

fn enabled(p: &Provider) -> bool {
    ["enableRss", "enableAutomaticSearch", "enableInteractiveSearch"]
        .iter()
        .any(|k| p.attributes[k].as_bool().unwrap_or(false))
}

/// `definition.Settings.Validate()`, for the implementations this phase
/// knows; the others' settings are stored as given.
fn validate_settings(p: &Provider) -> Vec<ValidationFailure> {
    match kind_of(p) {
        Some(kind) => NewznabSettings::from_value(&p.settings).validate(kind == IndexerKind::Torznab),
        None => Vec::new(),
    }
}

/// `_providerFactory.Test(definition)`: the connection test.
async fn test_provider(p: &Provider) -> Vec<ValidationFailure> {
    match kind_of(p) {
        Some(kind) => {
            let settings = NewznabSettings::from_value(&p.settings);
            indexers::shared().test(kind, &settings).await
        }
        None => vec![ValidationFailure::error(
            "",
            &format!(
                "Test was aborted due to an error: Unarr does not drive {} indexers yet",
                p.implementation
            ),
        )],
    }
}

fn verify(failures: Vec<ValidationFailure>, include_warnings: bool) -> ApiResult<()> {
    match validation::blocking(failures, include_warnings) {
        Some(failures) => Err(ApiError::Validation(failures)),
        None => Ok(()),
    }
}

fn broadcast(state: &AppState, action: &str, resource: Value) {
    state
        .hub()
        .broadcast("indexer", json!({"action": action, "resource": resource}));
    state.providers_changed();
}

/// `POST /indexer[?forceSave=true]`.
pub async fn create(State(state): State<AppState>, uri: Uri, Json(body): Json<Value>) -> ApiResult<Response> {
    let force_save = Params::from_uri(&uri).bool("forceSave", false);
    // `body["id"] = ...` below would panic on an array or a scalar.
    if !body.is_object() {
        return Err(ApiError::BadRequest(
            "The request body must be a JSON object".into(),
        ));
    }
    validate_resource(&state, &body, 0, "POST", false).await?;
    let mut provider = to_model(&body, 0, None)?;
    if enabled(&provider) {
        verify(validate_settings(&provider), !force_save)?;
        verify(test_provider(&provider).await, !force_save)?;
    }
    let stored = state
        .db()
        .call(move |conn| {
            provider.id = store::providers::next_provider_id(conn, KIND)?;
            store::providers::insert_provider(conn, &provider)?;
            Ok(provider)
        })
        .await?;
    let resource = provider_resource(&stored);
    broadcast(&state, "created", resource.clone());
    let location = format!("{}/api/v3/indexer/{}", state.settings().url_base, stored.id);
    Ok((
        StatusCode::CREATED,
        [(header::LOCATION, location)],
        Json(resource),
    )
        .into_response())
}

/// `PUT /indexer/{id}[?forceSave=true]`.
pub async fn update(
    State(state): State<AppState>,
    Path(route_id): Path<i64>,
    uri: Uri,
    Json(mut body): Json<Value>,
) -> ApiResult<Response> {
    let force_save = Params::from_uri(&uri).bool("forceSave", false);
    // `body["id"] = ...` below would panic on an array or a scalar.
    if !body.is_object() {
        return Err(ApiError::BadRequest(
            "The request body must be a JSON object".into(),
        ));
    }
    // The route id fills a missing body id, as the originals do before validating.
    if body["id"].as_i64().unwrap_or(0) == 0 {
        body["id"] = json!(route_id);
    }
    let body_id = body["id"].as_i64().unwrap_or(0);
    validate_resource(&state, &body, body_id, "PUT", false).await?;
    let existing = match find(&state, route_id).await? {
        Some(p) => p,
        None => find(&state, body_id).await?.ok_or(ApiError::NotFound)?,
    };
    let provider = to_model(&body, existing.id, Some(&existing))?;
    let changed = provider != existing;
    if enabled(&provider) {
        verify(validate_settings(&provider), !force_save)?;
        if !force_save && changed {
            verify(test_provider(&provider).await, true)?;
        }
    }
    if changed {
        let to_store = provider.clone();
        state
            .db()
            .call(move |conn| store::providers::update_provider(conn, &to_store))
            .await?;
    }
    let stored = find(&state, existing.id).await?.ok_or(ApiError::NotFound)?;
    let resource = provider_resource(&stored);
    if changed {
        broadcast(&state, "updated", resource.clone());
    }
    Ok((StatusCode::ACCEPTED, Json(resource)).into_response())
}

/// `DELETE /indexer/{id}`: `{}` whether or not it existed.
pub async fn delete(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let removed = state
        .db()
        .call(move |conn| store::providers::delete_provider(conn, KIND, id))
        .await?;
    if removed {
        broadcast(&state, "deleted", json!({"id": id}));
    }
    Ok(Json(json!({})))
}

/// `POST /indexer/test[?forceTest=true]`: `{}` when the indexer answers.
pub async fn test(
    State(state): State<AppState>,
    uri: Uri,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let force_test = Params::from_uri(&uri).bool("forceTest", false);
    let id = body["id"].as_i64().unwrap_or(0);
    validate_resource(&state, &body, id, "POST", true).await?;
    let existing = if id > 0 { find(&state, id).await? } else { None };
    let provider = to_model(&body, id, existing.as_ref())?;
    verify(validate_settings(&provider), !force_test)?;
    verify(test_provider(&provider).await, true)?;
    Ok(Json(json!({})))
}

/// `POST /indexer/testall`: every enabled indexer with valid settings,
/// 400 when any fails.
pub async fn test_all(State(state): State<AppState>) -> ApiResult<Response> {
    let mut results = Vec::new();
    let mut any_invalid = false;
    for p in list(&state).await? {
        let settings_failures = validate_settings(&p);
        if validation::has_errors(&settings_failures) || !enabled(&p) {
            continue;
        }
        let mut failures: Vec<ValidationFailure> =
            settings_failures.into_iter().filter(|f| !f.is_warning).collect();
        failures.extend(test_provider(&p).await.into_iter().filter(|f| !f.is_warning));
        any_invalid |= !failures.is_empty();
        results.push(json!({
            "id": p.id,
            "isValid": failures.is_empty(),
            "validationFailures": failures.iter().map(|f| f.to_plain_json()).collect::<Vec<_>>(),
        }));
    }
    let status = if any_invalid {
        StatusCode::BAD_REQUEST
    } else {
        StatusCode::OK
    };
    Ok((status, Json(Value::Array(results))).into_response())
}

/// `POST /indexer/action/{name}`: `newznabCategories` answers the category
/// options of the indexer's `caps`, or the defaults when it cannot be
/// reached; other actions answer `null`.
pub async fn action(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let id = body["id"].as_i64().unwrap_or(0);
    let existing = if id > 0 { find(&state, id).await? } else { None };
    let provider = to_model(&body, id, existing.as_ref())?;
    // Action names are matched like the originals' routes: case-insensitively.
    if !name.eq_ignore_ascii_case("newznabCategories") || kind_of(&provider).is_none() {
        return Ok(Json(Value::Null));
    }
    let settings = NewznabSettings::from_value(&provider.settings);
    let categories = if settings.base_url.trim().is_empty() || settings.api_path.trim().is_empty() {
        None
    } else {
        indexers::shared()
            .capabilities(&settings)
            .await
            .ok()
            .map(|caps| caps.categories)
    };
    Ok(Json(json!({
        "options": indexers::caps::category_options(categories.as_deref())
    })))
}

/// `PUT /indexer/bulk`.
pub async fn bulk_update(State(state): State<AppState>, Json(body): Json<Value>) -> ApiResult<Response> {
    let ids: Vec<i64> = body["ids"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default();
    if ids.is_empty() {
        return Err(ApiError::BadRequest("ids must be provided".into()));
    }
    let apply_tags = body["applyTags"].as_str().unwrap_or("add").to_ascii_lowercase();
    let tags: Option<Vec<i64>> = body["tags"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_i64).collect());
    let mut updated = Vec::new();
    let mut providers: Vec<Provider> = list(&state)
        .await?
        .into_iter()
        .filter(|p| ids.contains(&p.id))
        .collect();
    providers.sort_by_key(|p| p.id);
    for mut p in providers {
        if let Some(new_tags) = &tags {
            match apply_tags.as_str() {
                "remove" => p.tags.retain(|t| !new_tags.contains(t)),
                "replace" => p.tags = new_tags.clone(),
                _ => {
                    for t in new_tags {
                        if !p.tags.contains(t) {
                            p.tags.push(*t);
                        }
                    }
                }
            }
            p.tags.sort_unstable();
            p.tags.dedup();
        }
        for name in BULK_ATTRIBUTES {
            if let Some(v) = body.get(*name).filter(|v| !v.is_null()) {
                p.attributes[*name] = v.clone();
            }
        }
        let to_store = p.clone();
        state
            .db()
            .call(move |conn| store::providers::update_provider(conn, &to_store))
            .await?;
        let resource = provider_resource(&p);
        broadcast(&state, "updated", resource.clone());
        updated.push(resource);
    }
    Ok((StatusCode::ACCEPTED, Json(Value::Array(updated))).into_response())
}

/// `DELETE /indexer/bulk`.
pub async fn bulk_delete(State(state): State<AppState>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let ids: Vec<i64> = body["ids"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default();
    for id in ids {
        let removed = state
            .db()
            .call(move |conn| store::providers::delete_provider(conn, KIND, id))
            .await?;
        if removed {
            broadcast(&state, "deleted", json!({"id": id}));
        }
    }
    Ok(Json(json!({})))
}

/// Every indexer the phase can drive, decoded.
pub async fn definitions(state: &AppState) -> anyhow::Result<Vec<IndexerDefinition>> {
    Ok(list(state)
        .await?
        .iter()
        .filter_map(IndexerDefinition::from_provider)
        .collect())
}
