//! Radarr's writes of its catalogues where they differ from the ones both
//! personalities share: quality definitions (validated), quality profiles
//! (a language, and a name clash left to the database), the lists'
//! exclusions (movies) and the collections.

use axum::extract::{Path, State};
use axum::response::Response;
use axum::Json;
use serde_json::{json, Value};
use unarr_core::commands::{CommandBody, CommandPriority, CommandTrigger};
use unarr_core::model::ProviderKind;
use unarr_core::movies::model::ImportExclusion;
use unarr_core::movies::store as movies;
use unarr_core::store;
use unarr_core::validation::{display_name, ValidationFailure};

use crate::error::{ApiError, ApiResult};
use crate::routes::settings_writes::{
    accepted, at_least, check, created, delete_from, insert_new, int, ok_empty, quality_definition_row,
    quality_profile_row, replace, rule, text,
};
use crate::state::AppState;

// --- quality definitions

/// `QualityDefinitionLimits`: megabytes per minute.
const SIZE_MIN: f64 = 0.0;
const SIZE_MAX: f64 = 2000.0;

/// A size as FluentValidation prints it and as the JSON carries it: a
/// whole number without its fraction.
fn number(v: f64) -> Value {
    if v.fract() == 0.0 {
        json!(v as i64)
    } else {
        json!(v)
    }
}

fn compared(property: &str, value: f64, bound: f64, at_least: bool) -> Option<ValidationFailure> {
    let failed = if at_least { value < bound } else { value > bound };
    failed.then(|| {
        let (code, words) = if at_least {
            ("GreaterThanOrEqualTo", "greater than or equal to")
        } else {
            ("LessThanOrEqualTo", "less than or equal to")
        };
        let mut f = rule(
            property,
            code,
            &format!(
                "'{}' must be {words} '{}'.",
                display_name(property),
                number(bound)
            ),
            number(value),
        );
        f.placeholders = vec![
            ("comparisonProperty".into(), json!("")),
            ("comparisonValue".into(), number(bound)),
        ];
        f
    })
}

/// `QualityDefinitionResourceValidator`: each size between its
/// neighbours, a size that is not sent between the limits.
fn quality_definition_failures(body: &Value) -> Vec<ValidationFailure> {
    let size = |key: &str| body.get(key).and_then(Value::as_f64);
    let (min, preferred, max) = (size("minSize"), size("preferredSize"), size("maxSize"));
    let mut failures = Vec::new();
    if let Some(v) = min {
        failures.extend(compared("MinSize", v, SIZE_MIN, true));
        failures.extend(compared("MinSize", v, preferred.unwrap_or(SIZE_MAX), false));
    }
    if let Some(v) = preferred {
        failures.extend(compared("PreferredSize", v, min.unwrap_or(SIZE_MIN), true));
        failures.extend(compared("PreferredSize", v, max.unwrap_or(SIZE_MAX), false));
    }
    if let Some(v) = max {
        failures.extend(compared("MaxSize", v, preferred.unwrap_or(SIZE_MIN), true));
        failures.extend(compared("MaxSize", v, SIZE_MAX, false));
    }
    failures
}

pub async fn quality_definition_update(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    check(quality_definition_failures(&body))?;
    let id = body.get("id").and_then(Value::as_i64).unwrap_or(id);
    if !replace(
        &state,
        "quality_definitions",
        id,
        quality_definition_row(&body, id),
        store::catalog::insert_quality_definition,
    )
    .await?
    {
        return Err(ApiError::NotFound);
    }
    let Json(out) = super::catalog::quality_definition(State(state), Path(id)).await?;
    Ok(accepted(out))
}

/// `PUT /qualitydefinition/update`: every definition sent, each under the
/// same rules. The original answers from a five-second cache, often with
/// what was there before; Unarr answers with what it wrote.
pub async fn quality_definition_update_many(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    let sent = body.as_array().map(Vec::as_slice).unwrap_or(&[]);
    check(sent.iter().flat_map(quality_definition_failures).collect())?;
    let rows: Vec<_> = sent
        .iter()
        .filter_map(|d| d["id"].as_i64().map(|id| quality_definition_row(d, id)))
        .collect();
    state
        .db()
        .call(move |conn| {
            let tx = conn.unchecked_transaction()?;
            for r in &rows {
                if store::catalog::delete_row(&tx, "quality_definitions", r.id)? {
                    store::catalog::insert_quality_definition(&tx, r)?;
                }
            }
            tx.commit()?;
            Ok(())
        })
        .await?;
    let Json(out) = super::catalog::quality_definitions(State(state)).await?;
    Ok(accepted(Value::Array(out)))
}

// --- quality profiles

/// `QualityProfileController`'s rules. A name another profile has is not
/// among them: the database's unique index answers for it.
async fn quality_profile_failures(state: &AppState, body: &Value) -> ApiResult<Vec<ValidationFailure>> {
    let formats = state
        .db()
        .call(|c| store::catalog::list_custom_formats(c))
        .await?;
    let p = quality_profile_row(body, 0);
    let mut failures = Vec::new();
    if p.name.trim().is_empty() {
        failures.push(ValidationFailure::not_empty("Name", json!(p.name)));
    }
    failures.extend(at_least("MinUpgradeFormatScore", p.min_upgrade_format_score, 1));
    let cutoff_ok = p.items.iter().any(|i| {
        i.allowed && (i.quality == Some(p.cutoff) || (i.quality.is_none() && i.id == Some(p.cutoff)))
    });
    if !cutoff_ok {
        failures.push(rule(
            "Cutoff",
            "ValidCutoffValidator`1",
            "Cutoff must be an allowed quality or group",
            json!(p.cutoff),
        ));
    }
    let items = |code: &str, message: &str| rule("Items", code, message, body["items"].clone());
    if !p.items.iter().any(|i| i.allowed) {
        failures.push(items(
            "AllowedValidator`1",
            "Must contain at least one allowed quality",
        ));
    }
    if p.items
        .iter()
        .any(|i| i.name.as_deref().is_some_and(|n| !n.trim().is_empty()) && i.items.len() <= 1)
    {
        failures.push(items(
            "GroupItemValidator`1",
            "Groups must contain multiple qualities",
        ));
    }
    if p.items
        .iter()
        .any(|i| i.name.as_deref().is_some_and(|n| !n.trim().is_empty()) && i.quality.is_some())
    {
        failures.push(items(
            "QualityNameValidator`1",
            "Individual qualities should not be named",
        ));
    }
    if formats
        .iter()
        .any(|f| !p.format_items.iter().any(|i| i.format == f.id))
    {
        failures.push(rule(
            "FormatItems",
            "PredicateValidator",
            "All Custom Formats and no extra ones need to be present inside your Profile! Try refreshing your browser.",
            body["formatItems"].clone(),
        ));
    }
    let positive: i64 = p.format_items.iter().map(|f| f.score).filter(|s| *s > 0).sum();
    let best = p.format_items.iter().map(|f| f.score).max().unwrap_or(0);
    if positive < p.min_format_score && best < p.min_format_score {
        failures.push(ValidationFailure::bare(
            "",
            "Minimum Custom Format Score can never be satisfied",
        ));
    }
    Ok(failures)
}

/// The language a profile is sent with; `Original` when it names none.
fn language(body: &Value) -> i64 {
    body["language"]["id"].as_i64().unwrap_or(-2)
}

/// The unique index on the profiles' names, as the original lets SQLite
/// answer for it.
async fn name_clash(state: &AppState, name: String, id: i64) -> ApiResult<()> {
    let taken = state
        .db()
        .call(move |c| {
            Ok(store::catalog::list_quality_profiles(c)?
                .iter()
                .any(|p| p.name == name && p.id != id))
        })
        .await?;
    if taken {
        return Err(ApiError::ConflictMessage(
            "constraint failed\nUNIQUE constraint failed: QualityProfiles.Name".into(),
        ));
    }
    Ok(())
}

pub async fn quality_profile_create(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    check(quality_profile_failures(&state, &body).await?)?;
    name_clash(&state, text(&body, "name"), 0).await?;
    let language = language(&body);
    let id = insert_new(
        &state,
        "quality_profiles",
        quality_profile_row(&body, 0),
        |r, id| r.id = id,
        move |conn, row| {
            store::catalog::insert_quality_profile(conn, row)?;
            movies::set_profile_language(conn, row.id, language)
        },
    )
    .await?;
    let Json(out) = super::catalog::quality_profile(State(state.clone()), Path(id)).await?;
    Ok(created(&state, "qualityprofile", id, out))
}

pub async fn quality_profile_update(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    let id = body
        .get("id")
        .and_then(Value::as_i64)
        .filter(|i| *i > 0)
        .unwrap_or(id);
    check(quality_profile_failures(&state, &body).await?)?;
    name_clash(&state, text(&body, "name"), id).await?;
    let language = language(&body);
    if !replace(
        &state,
        "quality_profiles",
        id,
        quality_profile_row(&body, id),
        move |conn, row| {
            store::catalog::insert_quality_profile(conn, row)?;
            movies::set_profile_language(conn, row.id, language)
        },
    )
    .await?
    {
        return Err(ApiError::ModelNotFound(format!(
            "QualityProfile with ID {id} does not exist"
        )));
    }
    let Json(out) = super::catalog::quality_profile(State(state), Path(id)).await?;
    Ok(accepted(out))
}

/// `QualityProfileService.Delete`: refused while a movie, an import list
/// or a collection uses the profile; one that is not there is no error.
pub async fn quality_profile_delete(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Response> {
    let in_use = state
        .db()
        .call(move |conn| {
            let by_movie: i64 = conn.query_row(
                "SELECT COUNT(*) FROM movies WHERE quality_profile_id = ?1",
                [id],
                |r| r.get(0),
            )?;
            let by_list = store::providers::list_providers(conn, ProviderKind::ImportList)?
                .iter()
                .any(|l| l.attributes["qualityProfileId"].as_i64() == Some(id));
            let by_collection = movies::list_collections(conn)?
                .iter()
                .any(|c| c.quality_profile_id == id);
            Ok(by_movie > 0 || by_list || by_collection)
        })
        .await?;
    if in_use {
        return Err(ApiError::ServerException {
            message: format!("QualityProfile [{id}] is in use."),
            exception: "NzbDrone.Core.Profiles.Qualities.QualityProfileInUseException".into(),
        });
    }
    delete_from(&state, "quality_profiles", id).await?;
    Ok(ok_empty())
}

// --- the lists' exclusions

fn exclusion_resource(e: &ImportExclusion) -> Value {
    json!({
        "id": e.id,
        "tmdbId": e.tmdb_id,
        "movieTitle": e.movie_title,
        "movieYear": e.movie_year,
    })
}

fn exclusion(body: &Value, id: i64) -> ImportExclusion {
    ImportExclusion {
        id,
        tmdb_id: int(body, "tmdbId"),
        movie_title: body["movieTitle"].as_str().map(str::to_string),
        movie_year: int(body, "movieYear"),
    }
}

/// `ImportListExclusionController`'s rules.
fn exclusion_failures(body: &Value, id: i64, all: &[ImportExclusion]) -> Vec<ValidationFailure> {
    let mut failures = Vec::new();
    let tmdb = int(body, "tmdbId");
    if tmdb == 0 {
        failures.push(ValidationFailure::not_empty("TmdbId", json!(tmdb)));
    } else if all.iter().any(|e| e.tmdb_id == tmdb && e.id != id) {
        failures.push(rule(
            "TmdbId",
            "ImportListExclusionExistsValidator",
            "This exclusion has already been added.",
            json!(tmdb),
        ));
    }
    let title = text(body, "movieTitle");
    if title.trim().is_empty() {
        failures.push(ValidationFailure::not_empty("MovieTitle", json!(title)));
    }
    failures.extend(at_least("MovieYear", int(body, "movieYear"), 0));
    failures
}

async fn all_exclusions(state: &AppState) -> ApiResult<Vec<ImportExclusion>> {
    Ok(state.db().call(|c| movies::list_import_exclusions(c)).await?)
}

pub async fn exclusion_create(State(state): State<AppState>, Json(body): Json<Value>) -> ApiResult<Response> {
    let id = int(&body, "id");
    check(exclusion_failures(&body, id, &all_exclusions(&state).await?))?;
    let mut row = exclusion(&body, 0);
    let row = state
        .db()
        .call(move |conn| {
            movies::add_import_exclusion(conn, &mut row)?;
            Ok(row)
        })
        .await?;
    Ok(created(&state, "exclusions", row.id, exclusion_resource(&row)))
}

pub async fn exclusion_update(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    let id = body
        .get("id")
        .and_then(Value::as_i64)
        .filter(|i| *i > 0)
        .unwrap_or(id);
    check(exclusion_failures(&body, id, &all_exclusions(&state).await?))?;
    let row = exclusion(&body, id);
    let out = exclusion_resource(&row);
    if !replace(
        &state,
        "import_exclusions",
        id,
        row,
        movies::insert_import_exclusion,
    )
    .await?
    {
        return Err(ApiError::ModelNotFound(format!(
            "ImportListExclusion with ID {id} does not exist"
        )));
    }
    Ok(accepted(out))
}

/// `POST /exclusions/bulk`: every exclusion under the rules of one, then
/// all of them added.
pub async fn exclusion_bulk_create(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let sent = body.as_array().map(Vec::as_slice).unwrap_or(&[]);
    let all = all_exclusions(&state).await?;
    check(
        sent.iter()
            .flat_map(|e| exclusion_failures(e, int(e, "id"), &all))
            .collect(),
    )?;
    // `DeDupeExclusions`: a movie named twice in one request is excluded
    // once.
    let mut rows: Vec<ImportExclusion> = Vec::new();
    for row in sent.iter().map(|e| exclusion(e, 0)) {
        if !rows.iter().any(|r| r.tmdb_id == row.tmdb_id) {
            rows.push(row);
        }
    }
    let rows = state
        .db()
        .call(move |conn| {
            let tx = conn.unchecked_transaction()?;
            for row in &mut rows {
                movies::add_import_exclusion(&tx, row)?;
            }
            tx.commit()?;
            Ok(rows)
        })
        .await?;
    Ok(Json(Value::Array(rows.iter().map(exclusion_resource).collect())))
}

pub async fn exclusion_delete(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Response> {
    delete_from(&state, "import_exclusions", id).await?;
    Ok(ok_empty())
}

pub async fn exclusion_bulk_delete(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let ids: Vec<i64> = body["ids"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default();
    state
        .db()
        .call(move |conn| {
            for id in ids {
                store::catalog::delete_row(conn, "import_exclusions", id)?;
            }
            Ok(())
        })
        .await?;
    Ok(Json(json!({})))
}

// --- collections

/// `PUT /collection/{id}`: what the user decides about a collection (the
/// rest is the metadata server's). `CollectionEditedEvent` tells the UI
/// and runs the check of the collections' root folders; the movies a
/// monitored collection brings come with its next refresh.
pub async fn collection_update(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    let id = body
        .get("id")
        .and_then(Value::as_i64)
        .filter(|i| *i > 0)
        .unwrap_or(id);
    let sent = body.clone();
    let found = state
        .db()
        .call(move |conn| {
            let Some(mut c) = movies::get_collection(conn, id)? else {
                return Ok(false);
            };
            c.monitored = sent["monitored"].as_bool().unwrap_or(false);
            c.quality_profile_id = int(&sent, "qualityProfileId");
            c.root_folder_path = sent["rootFolderPath"].as_str().map(str::to_string);
            c.search_on_add = sent["searchOnAdd"].as_bool().unwrap_or(false);
            c.minimum_availability = super::movie_writes::minimum_availability(&sent["minimumAvailability"]);
            c.tags = crate::routes::settings_writes::ints(&sent, "tags");
            movies::update_collection(conn, &c)?;
            Ok(true)
        })
        .await?;
    if !found {
        return Err(ApiError::ModelNotFound(format!(
            "MovieCollection with ID {id} does not exist"
        )));
    }
    state.commands()?.collection_edited(id);
    let Json(out) = super::movies::collection(State(state), Path(id)).await?;
    Ok(accepted(out))
}

/// `PUT /collection`: the same decisions for several collections, and
/// whether their movies in the library are monitored. No event is raised
/// for the collections: the refresh of all of them the original queues
/// tells the UI what it finds.
pub async fn collection_update_many(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    let ids = crate::routes::settings_writes::ints(&body, "collectionIds");
    let sent = body.clone();
    let (updated, movie_ids) = state
        .db()
        .call(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let mut updated = Vec::new();
            let mut movie_ids = Vec::new();
            for mut c in movies::list_collections(&tx)?
                .into_iter()
                .filter(|c| ids.contains(&c.id))
            {
                if let Some(v) = sent["monitored"].as_bool() {
                    c.monitored = v;
                }
                if let Some(v) = sent["qualityProfileId"].as_i64() {
                    c.quality_profile_id = v;
                }
                if !sent["minimumAvailability"].is_null() {
                    c.minimum_availability =
                        super::movie_writes::minimum_availability(&sent["minimumAvailability"]);
                }
                if let Some(v) = sent["rootFolderPath"].as_str().filter(|p| !p.trim().is_empty()) {
                    c.root_folder_path = Some(v.to_string());
                }
                if let Some(v) = sent["searchOnAdd"].as_bool() {
                    c.search_on_add = v;
                }
                if let Some(monitored) = sent["monitorMovies"].as_bool() {
                    for mut m in movies::movies_by_collection(&tx, c.tmdb_id)? {
                        m.monitored = monitored;
                        movies::update_movie(&tx, &m)?;
                        movie_ids.push(m.id);
                    }
                }
                movies::update_collection(&tx, &c)?;
                updated.push(c.id);
            }
            tx.commit()?;
            Ok((updated, movie_ids))
        })
        .await?;
    let runtime = state.commands()?;
    for id in movie_ids {
        runtime.movie_edited(id);
    }
    runtime
        .queue()
        .push(
            CommandBody::RefreshCollections {
                collection_ids: Vec::new(),
            },
            CommandPriority::Normal,
            CommandTrigger::Unspecified,
            state.now(),
        )
        .await?;
    let Json(all) = super::movies::collections(State(state.clone()), "/collection".parse().unwrap()).await?;
    // `UpdateCollections(...).ToResource()`: the collections without the
    // movies `MapToResource` would add.
    let mut out: Vec<Value> = all
        .into_iter()
        .filter(|c| c["id"].as_i64().is_some_and(|id| updated.contains(&id)))
        .map(|mut c| {
            c["movies"] = json!([]);
            c["missingMovies"] = json!(0);
            c
        })
        .collect();
    out.sort_by_key(|c| c["id"].as_i64());
    Ok(accepted(Value::Array(out)))
}
