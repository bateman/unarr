//! Settings > Metadata, the writes Sonarr's `MetadataController` takes
//! besides the save of a consumer (`providers::metadata_update`): add,
//! test, action, delete. A consumer is stored and shown; nothing drives it,
//! so no metadata file is written and a test has nothing to reach.
//! Radarr's `/metadata/test` is this one too: its controller shares the
//! rules, and the schemas are looked up by the running personality.
//!
//! The bulk routes the base controller declares are hidden by Sonarr's
//! (`[NonAction]`): `/metadata/bulk` matches only `{id:int}`, which it is
//! not, so it is the empty 404 any method gets there.

use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Map, Value};
use unarr_core::model::{Provider, ProviderKind};
use unarr_core::store;
use unarr_core::validation::ValidationFailure;

use crate::error::{ApiError, ApiResult};
use crate::routes::providers::provider_resource;
use crate::schemas::{self, SchemaKind};
use crate::state::AppState;

const KIND: ProviderKind = ProviderKind::Metadata;

/// `RestController.ValidateResource` plus the shared rules of
/// `ProviderControllerBase`, as a create or a test runs them.
async fn validate_resource(state: &AppState, body: &Value, id: i64, test: bool) -> ApiResult<()> {
    if !body.is_object() {
        return Err(ApiError::BadRequest(
            "The request body must be a JSON object".into(),
        ));
    }
    let all = state
        .db()
        .call(|conn| store::providers::list_providers(conn, KIND))
        .await?;
    let mut failures = Vec::new();
    let name = body["name"].as_str().unwrap_or("");
    if name.trim().is_empty() {
        failures.push(ValidationFailure::not_empty("Name", body["name"].clone()));
    }
    if all
        .iter()
        .any(|p| p.name.eq_ignore_ascii_case(name) && p.id != id)
    {
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
    if !test && !body["fields"].is_array() {
        failures.push(ValidationFailure::not_null("Fields"));
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(ApiError::Validation(failures))
    }
}

/// `MetadataResourceMapper.ToModel`. An implementation nobody knows has no
/// settings type to read the fields into: the original's schema builder
/// throws, and the exception escapes as a 500.
fn to_model(state: &AppState, body: &Value, id: i64, existing: Option<&Provider>) -> ApiResult<Provider> {
    let implementation = body["implementation"].as_str().unwrap_or("").to_string();
    let Some(schema) = schemas::lookup(state.personality(), SchemaKind::Provider(KIND), &implementation)
    else {
        return Err(ApiError::ServerException {
            message: "Value can not be null. (Parameter 'targetType')".into(),
            exception: "System.ArgumentNullException".into(),
        });
    };
    let settings = schemas::read_fields(schema, &body["fields"], existing.map(|e| &e.settings));
    let mut attributes = Map::new();
    attributes.insert("enable".into(), json!(body["enable"].as_bool().unwrap_or(false)));
    Ok(Provider {
        kind: KIND,
        id,
        name: body["name"].as_str().unwrap_or("").to_string(),
        implementation,
        config_contract: body["configContract"].as_str().unwrap_or("").to_string(),
        settings,
        tags: body["tags"]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_i64).collect())
            .unwrap_or_default(),
        attributes: Value::Object(attributes),
    })
}

async fn find(state: &AppState, id: i64) -> anyhow::Result<Option<Provider>> {
    state
        .db()
        .call(move |conn| store::providers::get_provider(conn, KIND, id))
        .await
}

fn broadcast(state: &AppState, action: &str, resource: Value) {
    state
        .hub()
        .broadcast("metadata", json!({"action": action, "resource": resource}));
}

/// `POST /metadata`: a consumer added, 201 with what was stored. The test
/// an enabled one goes through always passes.
pub async fn create(State(state): State<AppState>, Json(body): Json<Value>) -> ApiResult<Response> {
    validate_resource(&state, &body, 0, false).await?;
    let mut provider = to_model(&state, &body, 0, None)?;
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
    let location = format!("{}/api/v3/metadata/{}", state.settings().url_base, stored.id);
    Ok((
        StatusCode::CREATED,
        [(header::LOCATION, location)],
        Json(resource),
    )
        .into_response())
}

/// `POST /metadata/test`, for Sonarr and Radarr: `{}` for whatever the
/// resource rules accept.
pub async fn test(State(state): State<AppState>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let id = body["id"].as_i64().unwrap_or(0);
    validate_resource(&state, &body, id, true).await?;
    let existing = if id > 0 { find(&state, id).await? } else { None };
    to_model(&state, &body, id, existing.as_ref())?;
    Ok(Json(json!({})))
}

/// `POST /metadata/action/{name}`: no consumer has an action, so the
/// answer is `null` once the resource reads as a consumer.
pub async fn action(
    State(state): State<AppState>,
    Path(_name): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let id = body["id"].as_i64().unwrap_or(0);
    let existing = if id > 0 { find(&state, id).await? } else { None };
    to_model(&state, &body, id, existing.as_ref())?;
    Ok(Json(Value::Null))
}

/// `DELETE /metadata/{id}`: `{}`, whether there was such a consumer or not.
///
/// A built-in consumer deleted here comes back at the next start, disabled
/// and with its default settings, as in the originals
/// ([`insert_missing_consumers`]).
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

/// The order the originals insert their consumers in, which gives the ids
/// (recorded on both started without a database: `golden-blank`). Plex is
/// Sonarr's, Emby (Legacy) Radarr's.
const START_ORDER: &[&str] = &[
    "XbmcMetadata",
    "WdtvMetadata",
    "RoksboxMetadata",
    "PlexMetadata",
    "MediaBrowserMetadata",
    "KometaMetadata",
];

/// `MetadataFactory.InitializeProviders`, at every start: each consumer the
/// application knows and the table lacks is inserted, disabled, named as
/// its implementation and with its default settings. A database started
/// from nothing gets all of them; one where a consumer was deleted gets it
/// back. Returns how many were added.
pub fn insert_missing_consumers(
    conn: &rusqlite::Connection,
    personality: unarr_core::Personality,
) -> anyhow::Result<usize> {
    let existing = store::providers::list_providers(conn, KIND)?;
    let mut known: Vec<&'static Value> = schemas::all(personality, SchemaKind::Provider(KIND))
        .iter()
        .collect();
    let place = |schema: &Value| {
        let implementation = schema["implementation"].as_str().unwrap_or("");
        START_ORDER
            .iter()
            .position(|i| *i == implementation)
            .unwrap_or(START_ORDER.len())
    };
    known.sort_by_key(|schema| place(schema));
    let mut added = 0;
    for schema in known {
        let implementation = schema["implementation"].as_str().unwrap_or("");
        if implementation.is_empty() || existing.iter().any(|p| p.implementation == implementation) {
            continue;
        }
        let provider = Provider {
            kind: KIND,
            id: store::providers::next_provider_id(conn, KIND)?,
            name: schema["implementationName"]
                .as_str()
                .unwrap_or(implementation)
                .to_string(),
            implementation: implementation.to_string(),
            config_contract: schema["configContract"].as_str().unwrap_or("").to_string(),
            settings: schemas::read_fields(schema, &Value::Null, None),
            tags: Vec::new(),
            attributes: json!({"enable": false}),
        };
        store::providers::insert_provider(conn, &provider)?;
        added += 1;
    }
    Ok(added)
}

#[cfg(test)]
mod tests {
    use super::*;
    use unarr_core::{Db, Personality};

    #[test]
    fn a_deleted_consumer_comes_back_disabled_at_the_next_start() {
        let db = Db::open_in_memory().unwrap();
        db.call_blocking(|conn| {
            assert_eq!(insert_missing_consumers(conn, Personality::Sonarr)?, 5);
            assert_eq!(insert_missing_consumers(conn, Personality::Sonarr)?, 0);
            let kodi = store::providers::get_provider(conn, KIND, 1)?.expect("Kodi first");
            assert_eq!(kodi.implementation, "XbmcMetadata");
            // Enabled, changed, then deleted: back as it was at first.
            store::providers::delete_provider(conn, KIND, 1)?;
            assert_eq!(insert_missing_consumers(conn, Personality::Sonarr)?, 1);
            let back = store::providers::list_providers(conn, KIND)?
                .into_iter()
                .find(|p| p.implementation == "XbmcMetadata")
                .expect("Kodi back");
            assert_eq!(back.id, 6);
            assert_eq!(
                (back.name.as_str(), back.attributes["enable"].clone()),
                ("Kodi (XBMC) / Emby", json!(false))
            );
            assert_eq!(back.settings, kodi.settings);
            Ok(())
        })
        .unwrap();
    }
}
