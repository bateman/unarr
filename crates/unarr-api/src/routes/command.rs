//! `/api/v3/command`, after the originals' `CommandController`: start a
//! command, list the ones still around, read one back, cancel a queued one.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Map, Value};
use unarr_core::commands::{split_camel_case, CommandBody, CommandModel, CommandPriority, CommandTrigger};
use unarr_core::validation::ValidationFailure;

use crate::error::{ApiError, ApiResult};
use crate::routes::queue::timespan;
use crate::state::AppState;

/// `CommandResource`, nulls left out.
pub fn command_resource(personality: unarr_core::Personality, c: &CommandModel) -> Value {
    let mut m = Map::new();
    let time = unarr_core::time::format_utc;
    m.insert("id".into(), json!(c.id));
    let name = c.body.name_in(personality);
    m.insert("name".into(), json!(name));
    m.insert("commandName".into(), json!(split_camel_case(name)));
    if let Some(message) = &c.message {
        m.insert("message".into(), json!(message));
    }
    m.insert("body".into(), c.body_json(personality));
    m.insert("priority".into(), json!(c.priority.as_str()));
    m.insert("status".into(), json!(c.status.as_str()));
    m.insert("result".into(), json!(c.result.as_str()));
    m.insert("queued".into(), json!(time(c.queued_at)));
    if let Some(t) = c.started_at {
        m.insert("started".into(), json!(time(t)));
    }
    if let Some(t) = c.ended_at {
        m.insert("ended".into(), json!(time(t)));
    }
    if let Some(d) = c.duration {
        m.insert("duration".into(), json!(timespan_precise(d)));
    }
    if let Some(e) = &c.exception {
        m.insert("exception".into(), json!(e));
    }
    m.insert("trigger".into(), json!(c.trigger.as_str()));
    // The originals read the wrong header name, so the agent is never set.
    if let Some(t) = c.state_change_time() {
        m.insert("stateChangeTime".into(), json!(time(t)));
    }
    m.insert("sendUpdatesToClient".into(), json!(c.send_updates_to_client));
    m.insert(
        "updateScheduledTask".into(),
        json!(c.body.updates_scheduled_task()),
    );
    if let Some(t) = c.last_execution_time {
        m.insert("lastExecutionTime".into(), json!(time(t)));
    }
    Value::Object(m)
}

/// A `TimeSpan` with its seven fraction digits, as a duration serialises.
fn timespan_precise(d: chrono::Duration) -> String {
    let whole = timespan(chrono::Duration::seconds(d.num_seconds().max(0)));
    let ticks = (d.num_nanoseconds().unwrap_or(0).max(0) % 1_000_000_000) / 100;
    format!("{whole}.{ticks:07}")
}

/// `POST /command`: `{"name": "RssSync", ...}`.
pub async fn start(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Response> {
    let name = body["name"].as_str().unwrap_or("");
    if name.trim().is_empty() {
        // `NotBlank` is a NotNull and a NotEmpty rule; both fail on nothing.
        let attempted = body["name"].clone();
        return Err(ApiError::Validation(vec![
            ValidationFailure::rule(
                "Name",
                "NotNullValidator",
                "'Name' must not be empty.",
                attempted.clone(),
            ),
            ValidationFailure::rule(
                "Name",
                "NotEmptyValidator",
                "'Name' must not be empty.",
                attempted,
            ),
        ]));
    }
    let command = CommandBody::parse_for(state.personality(), name, &body)
        .ok_or_else(|| ApiError::Internal(anyhow::anyhow!("Sequence contains no matching element")))?
        .for_trigger(CommandTrigger::Manual);
    let runtime = state.commands()?;
    let sends = body["sendUpdatesToClient"].as_bool().unwrap_or(false) || command.sends_updates();
    // The API turns the updates on for every command but Radarr's
    // `DownloadedMoviesScan`, whose class answers with its own
    // `SendUpdates`, false unless the request sets it.
    let forced_updates = !matches!(command, CommandBody::DownloadedMoviesScan { .. })
        || body["sendUpdates"].as_bool().unwrap_or(false);
    // A command whose class declares the property read-only keeps what it
    // declares, whatever the controller assigns.
    let fixed_updates = command.client_updates_are_fixed();
    // The originals push manual imports ahead of the rest.
    let priority = if matches!(
        command,
        CommandBody::ManualImport { .. } | CommandBody::ManualImportMovies { .. }
    ) {
        CommandPriority::High
    } else {
        CommandPriority::Normal
    };
    // The originals read `UserAgent`, a header no client sends.
    let agent = headers
        .get("UserAgent")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let model = runtime
        .queue()
        .push_with(command, priority, CommandTrigger::Manual, state.now(), |m| {
            m.suppress_messages = !sends;
            if !fixed_updates {
                m.send_updates_to_client = forced_updates;
            }
            m.client_user_agent = agent;
        })
        .await?;
    let current = runtime.queue().get(model.id).await?.unwrap_or(model);
    Ok((
        StatusCode::CREATED,
        Json(command_resource(state.personality(), &current)),
    )
        .into_response())
}

/// `GET /command`: started first, then by status, the higher priority
/// first.
pub async fn list(State(state): State<AppState>) -> ApiResult<Json<Vec<Value>>> {
    let mut all = state.commands()?.queue().all();
    all.sort_by(|a, b| {
        a.status
            .list_rank()
            .cmp(&b.status.list_rank())
            .then_with(|| b.priority.cmp(&a.priority))
    });
    Ok(Json(
        all.iter()
            .map(|c| command_resource(state.personality(), c))
            .collect(),
    ))
}

pub async fn get(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    match state.commands()?.queue().get(id).await? {
        Some(c) => Ok(Json(command_resource(state.personality(), &c))),
        None => Err(ApiError::NotFound),
    }
}

/// `DELETE /command/{id}`: only a queued command can be cancelled.
pub async fn cancel(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    if state.commands()?.queue().cancel(id) {
        Ok(Json(Value::Null))
    } else {
        Err(ApiError::ClientMessage(
            axum::http::StatusCode::CONFLICT,
            "Unable to cancel task".into(),
        ))
    }
}
