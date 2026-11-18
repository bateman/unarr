//! Routes that panic on purpose, for the tests of what a panic costs
//! (`make panic-test`). They are compiled only with the `panic-switch`
//! feature, which no published binary is built with: without it there is
//! no such route, and nothing a caller or the environment could set that
//! would bring one back.

use axum::extract::{Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use unarr_core::panics::switch;

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

#[derive(Deserialize)]
pub struct Where {
    r#in: String,
    name: Option<String>,
}

/// `GET /api/v3/system/panic?in=...`: panic in the handler, on the
/// database thread, on the blocking pool or while holding a lock of the
/// state; or arm the next run of the command `name` to panic, in its body
/// (`command`) or in the executor around it (`record`).
pub async fn api(State(state): State<AppState>, Query(q): Query<Where>) -> ApiResult<Json<Value>> {
    let app = state.personality().app_name().to_lowercase();
    match q.r#in.as_str() {
        "handler" => panic!("the panic switch was set for a handler"),
        "database" => {
            state
                .db()
                .call(|_| -> anyhow::Result<()> {
                    panic!("the panic switch was set for the database thread")
                })
                .await?
        }
        "blocking" => {
            crate::blocking::run(|| -> () { panic!("the panic switch was set for the blocking pool") })
                .await?
        }
        "lock" => state.panic_holding_the_settings(),
        place @ ("command" | "record") => {
            let name = q.name.ok_or(ApiError::NotFound)?;
            switch::arm(&format!("{place}:{app}:{name}"));
        }
        _ => return Err(ApiError::NotFound),
    }
    Ok(Json(json!({ "armed": true })))
}

/// `GET /panic`: a panic outside the API's pipeline, where a page or
/// `/ping` is served.
pub async fn page() -> &'static str {
    panic!("the panic switch was set for a page")
}
