//! Writing notifications: `POST`, `PUT`, `DELETE`, `test`, `testall`,
//! `action` and `bulk` on `/api/v3/notification`, after the originals'
//! `ProviderControllerBase` and `NotificationController`. The settings
//! page saves through these; validation runs in the originals' order
//! (resource rules, then settings, then the connection test) and fails in
//! their shapes. A test whose request never got an answer escapes as a
//! 500, as the originals' does.
//!
//! Both personalities are served from here: the controller is the same
//! class in the two originals, and what differs (the event flags of the
//! definition, which of them enable it, what a test sends) is asked of
//! the personality's own notifications. Radarr's controller does not
//! route the bulk actions (`[NonAction]`), so its router leaves them out.

use axum::extract::{Path, State};
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Map, Value};
use unarr_core::model::{Provider, ProviderKind};
use unarr_core::movies::notifications::{self as movie_notifications, MovieNotificationDefinition};
use unarr_core::notifications::{self, NotificationDefinition, TestOutcome};
use unarr_core::store;
use unarr_core::validation::{self, ValidationFailure};
use unarr_core::Personality;

use crate::error::{ApiError, ApiResult};
use crate::query::Params;
use crate::routes::providers::provider_resource;
use crate::schemas::{self, SchemaKind};
use crate::state::AppState;

const KIND: ProviderKind = ProviderKind::Notification;

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

/// `NotificationResourceMapper.ToModel`: the stored provider a resource
/// describes, every event flag false unless sent.
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
    let flags = match personality {
        Personality::Radarr => movie_notifications::FLAGS,
        _ => notifications::FLAGS,
    };
    for name in flags {
        let value = body.get(*name).and_then(Value::as_bool).unwrap_or(false);
        attributes.insert((*name).to_string(), Value::Bool(value));
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

/// What the handlers ask of a definition, whichever application's it is.
struct Definition {
    /// `NotificationDefinition.Enable`.
    enabled: bool,
    /// `definition.Settings.Validate()`.
    settings_failures: Vec<ValidationFailure>,
}

fn definition(personality: Personality, p: &Provider) -> Definition {
    match personality {
        Personality::Radarr => {
            let def = MovieNotificationDefinition::from_provider(p);
            Definition {
                enabled: def.enabled(),
                settings_failures: def.validate_settings(),
            }
        }
        _ => {
            let def = NotificationDefinition::from_provider(p);
            Definition {
                enabled: def.enabled(),
                settings_failures: def.validate_settings(),
            }
        }
    }
}

/// `_notificationFactory.Test(definition)`.
async fn test_provider(state: &AppState, p: &Provider) -> ApiResult<Vec<ValidationFailure>> {
    let runtime = state.commands()?;
    let outcome = match runtime.movie_notifier() {
        Some(movies) => {
            movies
                .test(&MovieNotificationDefinition::from_provider(p), state.now())
                .await
        }
        None => {
            runtime
                .notifier()
                .test(&NotificationDefinition::from_provider(p), state.now())
                .await
        }
    };
    match outcome {
        TestOutcome::Failures(f) => Ok(f),
        TestOutcome::Error(message) => Err(ApiError::Internal(anyhow::anyhow!(message))),
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
        .broadcast("notification", json!({"action": action, "resource": resource}));
}

/// `POST /notification[?forceSave=true]`.
pub async fn create(State(state): State<AppState>, uri: Uri, Json(body): Json<Value>) -> ApiResult<Response> {
    let force_save = Params::from_uri(&uri).bool("forceSave", false);
    if !body.is_object() {
        return Err(ApiError::BadRequest(
            "The request body must be a JSON object".into(),
        ));
    }
    validate_resource(&state, &body, 0, "POST", false).await?;
    let mut provider = to_model(state.personality(), &body, 0, None)?;
    let def = definition(state.personality(), &provider);
    if def.enabled {
        verify(def.settings_failures, !force_save)?;
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
    let location = format!("{}/api/v3/notification/{}", state.settings().url_base, stored.id);
    Ok((
        StatusCode::CREATED,
        [(header::LOCATION, location)],
        Json(resource),
    )
        .into_response())
}

/// `PUT /notification/{id}[?forceSave=true]`.
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
    let def = definition(state.personality(), &provider);
    if def.enabled {
        verify(def.settings_failures, !force_save)?;
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
    }
    Ok((StatusCode::ACCEPTED, Json(resource)).into_response())
}

/// `DELETE /notification/{id}`: `{}` whether or not it existed.
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

/// `POST /notification/test[?forceTest=true]`: `{}` when the target answers.
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
    verify(
        definition(state.personality(), &provider).settings_failures,
        !force_test,
    )?;
    verify(test_provider(&state, &provider).await?, true)?;
    Ok(Json(json!({})))
}

/// `POST /notification/testall`: every enabled notification with valid
/// settings, in id order; 400 when any fails, and a test that raises
/// (a host that does not resolve) ends the run with its error.
pub async fn test_all(State(state): State<AppState>) -> ApiResult<Response> {
    let mut results = Vec::new();
    let mut any_invalid = false;
    let mut providers = list(&state).await?;
    providers.sort_by_key(|p| p.id);
    for p in providers {
        let def = definition(state.personality(), &p);
        let settings_failures = def.settings_failures;
        if validation::has_errors(&settings_failures) || !def.enabled {
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

/// `POST /notification/action/{name}`: the Plex sign-in and server list
/// live on plex.tv, which Unarr does not call; the answer is the
/// originals' default, an empty object.
pub async fn action(
    State(state): State<AppState>,
    Path(_name): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let id = body["id"].as_i64().unwrap_or(0);
    let existing = if id > 0 { find(&state, id).await? } else { None };
    to_model(state.personality(), &body, id, existing.as_ref())?;
    Ok(Json(json!({})))
}

/// `PUT /notification/bulk`: tags only, as `NotificationBulkResource`.
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
        let to_store = p.clone();
        state
            .db()
            .call(move |conn| store::providers::update_provider(conn, &to_store))
            .await?;
        let resource = provider_resource(state.personality(), &p);
        broadcast(&state, "updated", resource.clone());
        updated.push(resource);
    }
    Ok((StatusCode::ACCEPTED, Json(Value::Array(updated))).into_response())
}

/// `DELETE /notification/bulk`.
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
