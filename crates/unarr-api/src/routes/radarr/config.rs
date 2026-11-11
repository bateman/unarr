//! Radarr's `config/*` resources where they are not the ones both
//! personalities share: the UI's movie settings, media management, naming,
//! the indexer options, the import lists' sync level and the metadata
//! certification country.

use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};
use unarr_core::movies::store as movies;

use crate::error::ApiResult;
use crate::routes::config::Values;
use crate::state::AppState;

pub async fn ui(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let v = Values::load(&state).await?;
    Ok(Json(json!({
        "firstDayOfWeek": v.int("FirstDayOfWeek", 0),
        "calendarWeekColumnHeader": v.str("CalendarWeekColumnHeader", "ddd M/D"),
        "movieRuntimeFormat": v.enum_name("MovieRuntimeFormat", "hoursMinutes"),
        "shortDateFormat": v.str("ShortDateFormat", "MMM D YYYY"),
        "longDateFormat": v.str("LongDateFormat", "dddd, MMMM D YYYY"),
        "timeFormat": v.str("TimeFormat", "h(:mm)a"),
        "showRelativeDates": v.bool("ShowRelativeDates", true),
        "enableColorImpairedMode": v.bool("EnableColorImpairedMode", false),
        "movieInfoLanguage": v.int("MovieInfoLanguage", 1),
        "uiLanguage": v.int("UILanguage", 1),
        "theme": state.settings().host.theme,
        "id": 1,
    })))
}

pub async fn media_management(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let v = Values::load(&state).await?;
    Ok(Json(json!({
        "autoUnmonitorPreviouslyDownloadedMovies": v.bool("AutoUnmonitorPreviouslyDownloadedMovies", false),
        "recycleBin": v.str("RecycleBin", ""),
        "recycleBinCleanupDays": v.int("RecycleBinCleanupDays", 7),
        "downloadPropersAndRepacks": v.enum_name("DownloadPropersAndRepacks", "preferAndUpgrade"),
        "createEmptyMovieFolders": v.bool("CreateEmptyMovieFolders", false),
        "deleteEmptyFolders": v.bool("DeleteEmptyFolders", false),
        "fileDate": v.enum_name("FileDate", "none"),
        "rescanAfterRefresh": v.enum_name("RescanAfterRefresh", "always"),
        "autoRenameFolders": v.bool("AutoRenameFolders", false),
        "pathsDefaultStatic": v.bool("PathsDefaultStatic", false),
        "setPermissionsLinux": v.bool("SetPermissionsLinux", false),
        "chmodFolder": v.str("ChmodFolder", "755"),
        "chownGroup": v.str("ChownGroup", ""),
        "skipFreeSpaceCheckWhenImporting": v.bool("SkipFreeSpaceCheckWhenImporting", false),
        "minimumFreeSpaceWhenImporting": v.int("MinimumFreeSpaceWhenImporting", 100),
        "copyUsingHardlinks": v.bool("CopyUsingHardlinks", true),
        "useScriptImport": v.bool("UseScriptImport", false),
        "scriptImportPath": v.str("ScriptImportPath", ""),
        "importExtraFiles": v.bool("ImportExtraFiles", false),
        "extraFileExtensions": v.str("ExtraFileExtensions", "srt"),
        "enableMediaInfo": v.bool("EnableMediaInfo", true),
        "id": 1,
    })))
}

/// `ColonReplacementFormat`, in declaration order.
fn colon_replacement(n: i64) -> &'static str {
    match n {
        0 => "delete",
        1 => "dash",
        2 => "spaceDash",
        3 => "spaceDashSpace",
        _ => "smart",
    }
}

pub async fn naming(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let n = state.db().call(|c| movies::naming(c)).await?;
    Ok(Json(json!({
        "renameMovies": n.rename_movies,
        "replaceIllegalCharacters": n.replace_illegal_characters,
        "colonReplacementFormat": colon_replacement(n.colon_replacement_format),
        "standardMovieFormat": n.standard_movie_format,
        "movieFolderFormat": n.movie_folder_format,
        "id": 1,
    })))
}

pub async fn indexer(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let v = Values::load(&state).await?;
    Ok(Json(json!({
        "minimumAge": v.int("MinimumAge", 0),
        "maximumSize": v.int("MaximumSize", 0),
        "retention": v.int("Retention", 0),
        "rssSyncInterval": v.int("RssSyncInterval", 60),
        "preferIndexerFlags": v.bool("PreferIndexerFlags", false),
        "availabilityDelay": v.int("AvailabilityDelay", 0),
        "allowHardcodedSubs": v.bool("AllowHardcodedSubs", false),
        "whitelistedHardcodedSubs": v.str("WhitelistedHardcodedSubs", ""),
        "id": 1,
    })))
}

pub async fn download_client(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let v = Values::load(&state).await?;
    Ok(Json(json!({
        "downloadClientWorkingFolders": v.str("DownloadClientWorkingFolders", "_UNPACK_|_FAILED_"),
        "enableCompletedDownloadHandling": v.bool("EnableCompletedDownloadHandling", true),
        "checkForFinishedDownloadInterval": v.int("CheckForFinishedDownloadInterval", 1),
        "autoRedownloadFailed": v.bool("AutoRedownloadFailed", true),
        "autoRedownloadFailedFromInteractiveSearch": v.bool("AutoRedownloadFailedFromInteractiveSearch", true),
        "id": 1,
    })))
}

pub async fn import_list(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let v = Values::load(&state).await?;
    Ok(Json(json!({
        "listSyncLevel": v.enum_name("ListSyncLevel", "disabled"),
        "id": 1,
    })))
}

pub async fn metadata(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let v = Values::load(&state).await?;
    Ok(Json(json!({
        "certificationCountry": v.enum_name("CertificationCountry", "us"),
        "id": 1,
    })))
}
