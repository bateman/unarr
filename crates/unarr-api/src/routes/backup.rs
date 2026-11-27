//! `system/backup` and `log/file`: the backup zips and the log files on
//! disk.
//!
//! A restore never touches the running database: the files are staged
//! under the app data and the next start applies them, as the originals do
//! (their answer is the same `restartRequired`).

use std::path::{Path as FsPath, PathBuf};

use axum::extract::{Multipart, Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use unarr_core::backup::{self, BackupFile};
use unarr_core::time;

use crate::blocking;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

/// The largest zip an upload may hold. The originals leave it to the web
/// server's limits; a library's database stays far below this.
pub const UPLOAD_LIMIT: usize = 2 * 1024 * 1024 * 1024;

async fn backups(state: &AppState) -> anyhow::Result<Vec<BackupFile>> {
    let ctx = state.backup_context();
    let root = backup::configured_root(state.db(), &ctx).await?;
    blocking::run(move || backup::list(&ctx, &root)).await
}

fn resource(file: &BackupFile) -> Value {
    json!({
        "id": file.id,
        "name": file.name,
        "path": format!("/backup/{}/{}", file.kind.as_str(), file.name),
        "size": file.size,
        "time": time::format_utc(file.time),
        "type": file.kind.as_str(),
    })
}

/// `GET /system/backup`: every backup, newest first.
pub async fn list(State(state): State<AppState>) -> ApiResult<Json<Vec<Value>>> {
    Ok(Json(backups(&state).await?.iter().map(resource).collect()))
}

/// A backup by id. None there: Sonarr throws `NotFoundException`
/// (`{"message": "NotFound"}`), Radarr 7 answers `TypedResults.NotFound()`.
async fn find(state: &AppState, id: i64) -> ApiResult<BackupFile> {
    let missing = match state.personality() {
        unarr_core::Personality::Radarr => ApiError::NotFound,
        unarr_core::Personality::Sonarr | unarr_core::Personality::Lidarr => ApiError::NotFoundException,
    };
    backups(state)
        .await?
        .into_iter()
        .find(|b| b.id == id)
        .ok_or(missing)
}

/// `DELETE /system/backup/{id}`.
pub async fn delete(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let file = find(&state, id).await?;
    tokio::fs::remove_file(&file.path)
        .await
        .map_err(|e| anyhow::anyhow!("deleting {}: {e}", file.path.display()))?;
    Ok(Json(json!({})))
}

async fn stage(state: &AppState, zip: PathBuf) -> ApiResult<Json<Value>> {
    let ctx = state.backup_context();
    let source = blocking::run(move || backup::stage_restore(&ctx, &zip)).await??;
    tracing::info!(?source, "backup staged; it is applied at the next start");
    Ok(Json(json!({ "restartRequired": true })))
}

/// `POST /system/backup/restore/{id}`: stage one of the listed backups.
pub async fn restore(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    let file = find(&state, id).await?;
    stage(&state, file.path).await
}

/// `POST /system/backup/restore/upload`: stage the zip the form carries.
pub async fn restore_upload(State(state): State<AppState>, mut form: Multipart) -> Response {
    let bad = |message: &str| ApiError::BadRequest(message.to_string()).into_response();
    let mut field = loop {
        match form.next_field().await {
            Ok(Some(field)) if field.file_name().is_some() => break field,
            Ok(Some(_)) => continue,
            Ok(None) => return bad("file must be provided"),
            Err(e) => return bad(&e.to_string()),
        }
    };
    let is_zip = field
        .file_name()
        .is_some_and(|n| n.to_ascii_lowercase().ends_with(".zip"));
    if !is_zip {
        let error = "Invalid extension, must be one of: .zip";
        // Sonarr throws `UnsupportedMediaTypeException`; Radarr 7 answers
        // a `BackupRestoreErrorResource`.
        return match state.personality() {
            unarr_core::Personality::Radarr => {
                (StatusCode::BAD_REQUEST, Json(json!({ "error": error }))).into_response()
            }
            unarr_core::Personality::Sonarr | unarr_core::Personality::Lidarr => (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                Json(json!({ "message": error })),
            )
                .into_response(),
        };
    }

    // The name is ours: nothing of the upload's reaches the file system.
    let target = state.runtime().app_data.join(".restore-upload.zip");
    let written: anyhow::Result<()> = async {
        use tokio::io::AsyncWriteExt;
        let mut out = tokio::fs::File::create(&target).await?;
        while let Some(chunk) = field.chunk().await? {
            out.write_all(&chunk).await?;
        }
        out.flush().await?;
        Ok(())
    }
    .await;
    let result = match written {
        Ok(()) => stage(&state, target.clone()).await,
        Err(e) => Err(ApiError::Internal(e)),
    };
    let _ = tokio::fs::remove_file(&target).await;
    match result {
        Ok(staged) => staged.into_response(),
        Err(e) => e.into_response(),
    }
}

/// `GET /backup/{type}/{name}`: the zip itself, as the list's `path` names
/// it. Only a listed backup is served, so the name cannot leave the folder.
pub async fn download(State(state): State<AppState>, Path((kind, name)): Path<(String, String)>) -> Response {
    let file = match backups(&state).await {
        Ok(all) => all
            .into_iter()
            .find(|b| b.kind.as_str() == kind.to_ascii_lowercase() && b.name == name),
        Err(e) => return ApiError::Internal(e).into_response(),
    };
    let not_found = || crate::ui::static_not_found(state.personality());
    let Some(file) = file else {
        return not_found();
    };
    // As `BackupFileMapper` serves it, a static file: the type ASP.NET's
    // table gives `.zip`, and no `Content-Disposition` (the browser saves
    // a zip all the same).
    match tokio::fs::read(&file.path).await {
        Ok(bytes) => (
            [(
                header::CONTENT_TYPE,
                crate::http::content_type(state.personality(), &file.name),
            )],
            bytes,
        )
            .into_response(),
        Err(_) => not_found(),
    }
}

/// The folder the log files are written to (`sonarr.txt` and its archives,
/// by the daemon's `LogFiles` layer), where the originals keep theirs.
fn log_dir(state: &AppState) -> PathBuf {
    state.runtime().app_data.join("logs")
}

/// The `.txt` files of the folder, by name, with their last write.
fn log_files_in(dir: &FsPath) -> Vec<(String, DateTime<Utc>)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<(String, DateTime<Utc>)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let meta = entry.metadata().ok()?;
            (meta.is_file() && name.ends_with(".txt")).then(|| {
                let written = meta.modified().map(DateTime::<Utc>::from).unwrap_or_default();
                (name, written)
            })
        })
        .collect();
    files.sort();
    files
}

/// `GET /log/file`: the log files on disk, the last written first; the
/// ids number them before that sort, as the originals do.
pub async fn log_files(State(state): State<AppState>) -> ApiResult<Json<Vec<Value>>> {
    let dir = log_dir(&state);
    let prefix = state.personality().api_prefix();
    let url_base = state.settings().url_base.clone();
    let files = blocking::run(move || log_files_in(&dir)).await?;
    let mut numbered: Vec<(usize, &(String, DateTime<Utc>))> = files.iter().enumerate().collect();
    numbered.sort_by_key(|(_, (_, written))| std::cmp::Reverse(*written));
    Ok(Json(
        numbered
            .into_iter()
            .map(|(i, (name, written))| {
                json!({
                    "id": i + 1,
                    "filename": name,
                    "lastWriteTime": time::format_utc(*written),
                    "contentsUrl": format!("{url_base}{prefix}/log/file/{name}"),
                    "downloadUrl": format!("{url_base}/logfile/{name}"),
                })
            })
            .collect(),
    ))
}

/// `GET /log/file/update`: the updater's log files. Unarr has no updater.
pub async fn update_log_files() -> Json<Vec<Value>> {
    Json(Vec::new())
}

/// A listed log file's bytes; only a listed file is served, so the name
/// cannot leave the folder.
async fn read_log_file(state: &AppState, filename: &str) -> Option<(String, Vec<u8>)> {
    let dir = log_dir(state);
    let listed = {
        let dir = dir.clone();
        blocking::run(move || log_files_in(&dir))
            .await
            .unwrap_or_default()
    };
    // The API lower-cases its paths: match the name without its case.
    let (name, _) = listed.iter().find(|(n, _)| n.eq_ignore_ascii_case(filename))?;
    let bytes = tokio::fs::read(dir.join(name)).await.ok()?;
    Some((name.clone(), bytes))
}

/// `GET /log/file/{filename}`: one log file's text.
pub async fn log_file(State(state): State<AppState>, Path(filename): Path<String>) -> Response {
    match read_log_file(&state, &filename).await {
        Some((_, bytes)) => ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], bytes).into_response(),
        // `NotFound()` from the controller: the runtime's problem details,
        // not the exception's `{"message": "NotFound"}`.
        None => ApiError::NotFound.into_response(),
    }
}

/// `GET /logfile/{filename}` (`LogFileMapper`): the file the list's
/// `downloadUrl` names, for the UI's Download link.
pub async fn log_file_download(State(state): State<AppState>, Path(filename): Path<String>) -> Response {
    match read_log_file(&state, &filename).await {
        Some((_, bytes)) => ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], bytes).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// `GET /updatelogfile/{filename}`: the updater's logs; there are none.
pub async fn update_log_file_download() -> Response {
    StatusCode::NOT_FOUND.into_response()
}
