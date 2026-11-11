//! Writing import lists: `POST`, `PUT`, `DELETE`, `test`, `testall`,
//! `action` and `bulk` on `/api/v3/importlist`, after the originals'
//! `ProviderControllerBase` and `ImportListController`. Validation runs in
//! the originals' order (resource rules, then settings, then the list's
//! own test). A list that changed is synced at once and a list that went
//! has the library checked against those that remain, as the originals'
//! `ImportListUpdatedHandler` and `ImportListSyncService` do.

use axum::extract::{Path, State};
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Map, Value};
use unarr_core::commands::{CommandBody, CommandPriority, CommandTrigger};
use unarr_core::importlists::ImportLists;
use unarr_core::model::{Provider, ProviderKind};
use unarr_core::store;
use unarr_core::validation::{self, ValidationFailure};

use crate::error::{ApiError, ApiResult};
use crate::query::Params;
use crate::routes::providers::provider_resource;
use crate::schemas::{self, SchemaKind};
use crate::state::AppState;

const KIND: ProviderKind = ProviderKind::ImportList;

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
/// `ProviderControllerBase`.
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
    let root = body["rootFolderPath"].as_str().unwrap_or("").to_string();
    let profile = body["qualityProfileId"].as_i64().unwrap_or(0);
    let (root_folders, profiles) = state
        .db()
        .call(|conn| {
            Ok((
                store::catalog::list_root_folders(conn)?,
                store::catalog::list_quality_profiles(conn)?,
            ))
        })
        .await?;
    // `IsValidPath`, then `RootFolderExistsValidator` (the cascade stops).
    if !root.starts_with('/') || root.contains('\0') {
        let mut f = ValidationFailure::rule(
            "RootFolderPath",
            "PathValidator",
            &format!("Invalid Path: '{root}'"),
            body["rootFolderPath"].clone(),
        );
        f.placeholders = vec![("path".into(), json!(root))];
        failures.push(f);
    } else if !root_folders
        .iter()
        .any(|r| unarr_core::mediafiles::path_equals(&r.path, &root))
    {
        let mut f = ValidationFailure::rule(
            "RootFolderPath",
            "RootFolderExistsValidator",
            &format!("Root folder '{root}' does not exist"),
            json!(root),
        );
        f.placeholders = vec![("path".into(), json!(root))];
        failures.push(f);
    }
    // `ValidId`, then `QualityProfileExistsValidator`.
    if profile <= 0 {
        failures.push(ValidationFailure::greater_than(
            "QualityProfileId",
            json!(profile),
            0,
        ));
    } else if !profiles.iter().any(|p| p.id == profile) {
        failures.push(ValidationFailure::rule(
            "QualityProfileId",
            "QualityProfileExistsValidator",
            "Quality Profile does not exist",
            json!(profile),
        ));
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

/// What `ImportListResource` carries beyond the provider's own: a flag or
/// a name, with the value an absent one takes.
const ATTRIBUTES: &[(&str, Value)] = &[
    ("enableAutomaticAdd", Value::Bool(false)),
    ("searchForMissingEpisodes", Value::Bool(false)),
    ("seasonFolder", Value::Bool(false)),
];

/// `ImportListResourceMapper.ToModel`.
fn to_model(
    personality: unarr_core::Personality,
    body: &Value,
    id: i64,
    existing: Option<&Provider>,
) -> ApiResult<Provider> {
    let implementation = body["implementation"].as_str().unwrap_or("").to_string();
    let Some(schema) = schemas::lookup(personality, SchemaKind::Provider(KIND), &implementation) else {
        return Err(ApiError::BadRequest(format!(
            "Unknown implementation {implementation:?}"
        )));
    };
    let settings = schemas::read_fields(schema, &body["fields"], existing.map(|e| &e.settings));
    let mut attributes = Map::new();
    for (name, default) in ATTRIBUTES {
        let value = body.get(*name).filter(|v| v.is_boolean()).unwrap_or(default);
        attributes.insert((*name).to_string(), value.clone());
    }
    let text = |name: &str, default: &str| {
        let s = body[name].as_str().unwrap_or(default);
        json!(lower_first(s))
    };
    attributes.insert("shouldMonitor".into(), text("shouldMonitor", "unknown"));
    attributes.insert("monitorNewItems".into(), text("monitorNewItems", "all"));
    attributes.insert("seriesType".into(), text("seriesType", "standard"));
    attributes.insert(
        "rootFolderPath".into(),
        body.get("rootFolderPath").cloned().unwrap_or(Value::Null),
    );
    attributes.insert(
        "qualityProfileId".into(),
        json!(body["qualityProfileId"].as_i64().unwrap_or(0)),
    );
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

/// The API spells enum names with a lower first letter and reads any case.
fn lower_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// `ImportListDefinition.Enable`.
fn enabled(p: &Provider) -> bool {
    p.attributes["enableAutomaticAdd"].as_bool().unwrap_or(false)
}

/// `_importListFactory.Test(definition)`: a stored list remembers how its
/// test went.
async fn test_provider(state: &AppState, p: &Provider) -> ApiResult<Vec<ValidationFailure>> {
    let lists = state.commands()?.import_lists();
    let failures = lists.test(p).await;
    lists
        .record_test(p.id, !validation::has_errors(&failures), state.now())
        .await?;
    Ok(failures)
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
        .broadcast("importlist", json!({"action": action, "resource": resource}));
}

/// `ImportListUpdatedHandler`: a list that changed is synced now.
async fn queue_sync(state: &AppState, id: i64) -> ApiResult<()> {
    state
        .commands()?
        .queue()
        .push(
            CommandBody::ImportListSync {
                definition_id: Some(id),
            },
            CommandPriority::Normal,
            CommandTrigger::Unspecified,
            state.now(),
        )
        .await?;
    Ok(())
}

/// `POST /importlist[?forceSave=true]`.
pub async fn create(State(state): State<AppState>, uri: Uri, Json(body): Json<Value>) -> ApiResult<Response> {
    let force_save = Params::from_uri(&uri).bool("forceSave", false);
    if !body.is_object() {
        return Err(ApiError::BadRequest(
            "The request body must be a JSON object".into(),
        ));
    }
    validate_resource(&state, &body, 0, "POST", false).await?;
    let mut provider = to_model(state.personality(), &body, 0, None)?;
    if enabled(&provider) {
        verify(ImportLists::validate_settings(&provider), !force_save)?;
        verify(test_provider(&state, &provider).await?, !force_save)?;
    }
    let stored = state
        .db()
        .call(move |conn| {
            provider.id = store::providers::next_provider_id(conn, KIND)?;
            store::providers::insert_provider(conn, &provider)?;
            Ok(provider)
        })
        .await?;
    let resource = provider_resource(state.personality(), &stored);
    broadcast(&state, "created", resource.clone());
    let location = format!("{}/api/v3/importlist/{}", state.settings().url_base, stored.id);
    Ok((
        StatusCode::CREATED,
        [(header::LOCATION, location)],
        Json(resource),
    )
        .into_response())
}

/// `PUT /importlist/{id}[?forceSave=true]`.
pub async fn update(
    State(state): State<AppState>,
    Path(route_id): Path<i64>,
    uri: Uri,
    Json(mut body): Json<Value>,
) -> ApiResult<Response> {
    let force_save = Params::from_uri(&uri).bool("forceSave", false);
    if !body.is_object() {
        return Err(ApiError::BadRequest(
            "The request body must be a JSON object".into(),
        ));
    }
    if body["id"].as_i64().unwrap_or(0) == 0 {
        body["id"] = json!(route_id);
    }
    let body_id = body["id"].as_i64().unwrap_or(0);
    validate_resource(&state, &body, body_id, "PUT", false).await?;
    let existing = match find(&state, route_id).await? {
        Some(p) => p,
        None => find(&state, body_id).await?.ok_or(ApiError::NotFound)?,
    };
    let provider = to_model(state.personality(), &body, existing.id, Some(&existing))?;
    let changed = provider != existing;
    if enabled(&provider) {
        verify(ImportLists::validate_settings(&provider), !force_save)?;
        if !force_save && changed {
            verify(test_provider(&state, &provider).await?, true)?;
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
    let resource = provider_resource(state.personality(), &stored);
    if changed {
        broadcast(&state, "updated", resource.clone());
        queue_sync(&state, stored.id).await?;
    }
    Ok((StatusCode::ACCEPTED, Json(resource)).into_response())
}

/// `DELETE /importlist/{id}`: `{}` whether or not it existed.
pub async fn delete(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let removed = state
        .db()
        .call(move |conn| store::providers::delete_provider(conn, KIND, id))
        .await?;
    if removed {
        broadcast(&state, "deleted", json!({"id": id}));
        state.commands()?.import_list_deleted().await;
    }
    Ok(Json(json!({})))
}

/// `POST /importlist/test[?forceTest=true]`: `{}` when the target answers.
pub async fn test(
    State(state): State<AppState>,
    uri: Uri,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let force_test = Params::from_uri(&uri).bool("forceTest", false);
    let id = body["id"].as_i64().unwrap_or(0);
    validate_resource(&state, &body, id, "POST", true).await?;
    let existing = if id > 0 { find(&state, id).await? } else { None };
    let provider = to_model(state.personality(), &body, id, existing.as_ref())?;
    verify(ImportLists::validate_settings(&provider), !force_test)?;
    verify(test_provider(&state, &provider).await?, true)?;
    Ok(Json(json!({})))
}

/// `POST /importlist/testall`: every enabled list with valid
/// settings, in id order; 400 when any fails, and a test that raises
/// (a host that does not resolve) ends the run with its error.
pub async fn test_all(State(state): State<AppState>) -> ApiResult<Response> {
    let mut results = Vec::new();
    let mut any_invalid = false;
    let mut providers = list(&state).await?;
    providers.sort_by_key(|p| p.id);
    for p in providers {
        let settings_failures = ImportLists::validate_settings(&p);
        if validation::has_errors(&settings_failures) || !enabled(&p) {
            continue;
        }
        let mut failures: Vec<ValidationFailure> =
            settings_failures.into_iter().filter(|f| !f.is_warning).collect();
        failures.extend(
            test_provider(&state, &p)
                .await?
                .into_iter()
                .filter(|f| !f.is_warning),
        );
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

/// `POST /importlist/action/{name}`: the sign-in's steps, given the query
/// string as the originals' `RequestAction` is.
pub async fn action(
    State(state): State<AppState>,
    Path(name): Path<String>,
    uri: Uri,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let id = body["id"].as_i64().unwrap_or(0);
    let existing = if id > 0 { find(&state, id).await? } else { None };
    let provider = to_model(state.personality(), &body, id, existing.as_ref())?;
    let query = Params::from_uri(&uri);
    let answer = state
        .commands()?
        .import_lists()
        .action(&provider, &name, query.pairs(), state.now())
        .await
        .map_err(|message| ApiError::Internal(anyhow::anyhow!(message)))?;
    Ok(Json(answer))
}

/// `PUT /importlist/bulk`: the tags, and what `ImportListBulkResource`
/// adds to them; each list is then synced, as after its own `PUT`.
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
        for name in ["enableAutomaticAdd", "rootFolderPath", "qualityProfileId"] {
            if !body[name].is_null() {
                p.attributes[name] = body[name].clone();
            }
        }
        let to_store = p.clone();
        state
            .db()
            .call(move |conn| store::providers::update_provider(conn, &to_store))
            .await?;
        let resource = provider_resource(state.personality(), &p);
        broadcast(&state, "updated", resource.clone());
        queue_sync(&state, p.id).await?;
        updated.push(resource);
    }
    Ok((StatusCode::ACCEPTED, Json(Value::Array(updated))).into_response())
}

/// `DELETE /importlist/bulk`.
pub async fn bulk_delete(State(state): State<AppState>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let ids: Vec<i64> = body["ids"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default();
    let mut any = false;
    for id in ids {
        let removed = state
            .db()
            .call(move |conn| store::providers::delete_provider(conn, KIND, id))
            .await?;
        if removed {
            broadcast(&state, "deleted", json!({"id": id}));
            any = true;
        }
    }
    if any {
        state.commands()?.import_list_deleted().await;
    }
    Ok(Json(json!({})))
}
