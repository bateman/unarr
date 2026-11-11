//! Radarr's `config/*` resources where they are not the ones both
//! personalities share: the UI's movie settings, media management, naming,
//! the indexer options, the import lists' sync level and the metadata
//! certification country.

use axum::extract::State;
use axum::http::Uri;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};
use unarr_core::model::QualityModel;
use unarr_core::movies::model::{MovieMetadata, MovieNamingConfig, MovieStatus};
use unarr_core::movies::naming::{build_file_name, movie_folder, NamingMovie, NamingMovieFile};
use unarr_core::movies::store as movies;
use unarr_core::naming::{NamingCustomFormat, NamingMediaInfo};
use unarr_core::store;

use crate::error::ApiResult;
use crate::query::Params;
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

/// The naming form as a query carries it, read the way model binding
/// reads a `NamingConfigResource`: what is missing is the type's default
/// (0, off, delete, no format), and what cannot be read is said per field.
fn naming_form(p: &Params) -> Result<(i64, MovieNamingConfig), Value> {
    let mut errors = serde_json::Map::new();
    let mut refuse = |field: &str, property: &str, value: &str, known: bool| {
        let message = if known {
            format!("The value '{value}' is invalid.")
        } else {
            format!("The value '{value}' is not valid for {property}.")
        };
        errors.insert(field.to_string(), json!([message]));
    };
    let id = match p.get("id").map(str::trim) {
        None => 0,
        Some(v) => v.parse::<i32>().map(i64::from).unwrap_or_else(|_| {
            refuse("id", "Id", v, v.is_empty());
            0
        }),
    };
    let mut flag = |field: &str, property: &str| match p.get(field).map(str::trim) {
        None => false,
        Some(v) if v.eq_ignore_ascii_case("true") => true,
        Some(v) if v.eq_ignore_ascii_case("false") => false,
        Some(v) => {
            refuse(field, property, v, v.is_empty());
            false
        }
    };
    let rename_movies = flag("renameMovies", "RenameMovies");
    let replace_illegal_characters = flag("replaceIllegalCharacters", "ReplaceIllegalCharacters");
    // An enum: by its name, any case, or by a number that is one of its values.
    let colon_replacement_format = match p.get("colonReplacementFormat").map(str::trim) {
        None => 0,
        Some(v) => match (v.to_ascii_lowercase().as_str(), v.parse::<i64>()) {
            ("delete", _) => 0,
            ("dash", _) => 1,
            ("spacedash", _) => 2,
            ("spacedashspace", _) => 3,
            ("smart", _) => 4,
            (_, Ok(n)) if (0..=4).contains(&n) => n,
            (_, number) => {
                refuse(
                    "colonReplacementFormat",
                    "ColonReplacementFormat",
                    v,
                    v.is_empty() || number.is_ok(),
                );
                0
            }
        },
    };
    if !errors.is_empty() {
        return Err(Value::Object(errors));
    }
    Ok((
        id,
        MovieNamingConfig {
            rename_movies,
            replace_illegal_characters,
            colon_replacement_format,
            standard_movie_format: p.get("standardMovieFormat").unwrap_or("").to_string(),
            movie_folder_format: p.get("movieFolderFormat").unwrap_or("").to_string(),
        },
    ))
}

/// The movie the originals render their naming examples with
/// (`FileNameSampleService`).
fn sample_movie() -> MovieMetadata {
    MovieMetadata {
        id: 1,
        tmdb_id: 345691,
        imdb_id: Some("tt0066921".into()),
        title: "The Movie: Title".into(),
        sort_title: None,
        clean_title: None,
        original_title: Some("The Original Movie Title".into()),
        clean_original_title: None,
        original_language_id: 1,
        status: MovieStatus::Released,
        overview: None,
        images: Vec::new(),
        genres: Vec::new(),
        keywords: Vec::new(),
        ratings: json!({}),
        recommendations: Vec::new(),
        runtime: 0,
        in_cinemas: None,
        physical_release: None,
        digital_release: None,
        year: 2010,
        secondary_year: None,
        certification: Some("R".into()),
        youtube_trailer_id: None,
        studio: None,
        website: None,
        popularity: 0.0,
        collection_tmdb_id: 123654,
        collection_title: Some("The Movie Collection".into()),
        last_info_sync: None,
    }
}

/// `GET /config/naming/examples`: what the formats make of the sample
/// movie. The form as it is being edited, when it comes with the
/// configuration's id (`GetExamples([FromQuery] NamingConfigResource)`);
/// else the stored configuration.
pub async fn naming_examples(State(state): State<AppState>, uri: Uri) -> ApiResult<Response> {
    let (id, form) = match naming_form(&Params::from_uri(&uri)) {
        Ok(form) => form,
        Err(errors) => {
            return Ok(crate::error::validation_problem(
                crate::error::BAD_REQUEST_TYPE_RFC9110,
                "application/problem+json; charset=utf-8",
                errors,
            ))
        }
    };
    let (stored, definitions, translations) = state
        .db()
        .call(|c| {
            Ok((
                movies::naming(c)?,
                store::catalog::list_quality_definitions(c)?,
                // The sample says its metadata is number 1 and brings no
                // translations, so the original reads those of whatever
                // movie of the library has that number: `{Movie Title:FR}`
                // shows that movie's French title.
                movies::translation_titles(c, 1)?,
            ))
        })
        .await?;
    let config = if id != 0 { form } else { stored };
    // The sample file is Bluray-1080p; its title is whatever the user calls it.
    let quality_title = definitions
        .iter()
        .find(|d| d.quality_id == 7)
        .map(|d| d.title.clone())
        .unwrap_or_else(|| "Bluray-1080p".to_string());
    // The sample carries the old MediaInfo names ("AVC", "DTS"), which the
    // formatter passes through, and nothing it reads a dynamic range from.
    let media_info = NamingMediaInfo {
        video_codec: "AVC".into(),
        video_bit_depth: 10,
        audio_codec: "DTS".into(),
        audio_channels: 5.1,
        audio_languages: vec!["ger".into()],
        subtitles: vec!["eng".into(), "ger".into()],
        video_dynamic_range: String::new(),
        video_dynamic_range_type: String::new(),
    };
    let file = NamingMovieFile {
        quality: QualityModel {
            quality_id: 7,
            version: 2,
            real: 0,
            is_repack: false,
        },
        quality_title: &quality_title,
        relative_path: "The.Movie.Title.2010.1080p.BluRay.DTS.x264-EVOLVE.mkv",
        scene_name: Some("The.Movie.Title.2010.1080p.BluRay.DTS.x264-EVOLVE"),
        release_group: Some("EVOLVE"),
        edition: Some("Ultimate extended edition"),
        media_info: Some(&media_info),
        three_d: true,
    };
    let formats = [
        NamingCustomFormat {
            name: "Surround Sound",
            include_when_renaming: true,
        },
        NamingCustomFormat {
            name: "x264",
            include_when_renaming: true,
        },
    ];
    let metadata = sample_movie();
    let movie = NamingMovie {
        metadata: &metadata,
        translations: &translations,
    };
    // A format left blank has no example.
    let mut out = serde_json::Map::new();
    if !config.standard_movie_format.trim().is_empty() {
        out.insert(
            "movieExample".into(),
            json!(build_file_name(&movie, &file, &config, &formats).unwrap_or_default()),
        );
    }
    if !config.movie_folder_format.trim().is_empty() {
        out.insert("movieFolderExample".into(), json!(movie_folder(&movie, &config)));
    }
    Ok(Json(Value::Object(out)).into_response())
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
