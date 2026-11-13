//! Radarr's `config/*` pages saved: the UI's settings, media management,
//! naming, the indexer and download client options, the lists' sync level
//! and the certification country. Each validates as its controller in the
//! original does and answers with the page as it reads afterwards.

use std::sync::LazyLock;

use axum::extract::State;
use axum::response::Response;
use axum::Json;
use fancy_regex::Regex;
use serde_json::{json, Value};
use unarr_core::movies::store as movies;
use unarr_core::store;
use unarr_core::validation::ValidationFailure;

use super::config;
use crate::error::{ApiError, ApiResult};
use crate::routes::settings_writes::{
    accepted, at_least, check, host_overrides, int, object, recycle_bin_failures, rule, save_config,
    save_host_overrides, text, Kind,
};
use crate::state::AppState;

/// `GreaterThanOrEqualTo(1)` with the message the controller gives it.
fn language_at_least_one(property: &str, label: &str, value: i64) -> Option<ValidationFailure> {
    (value < 1).then(|| {
        let mut f = rule(
            property,
            "GreaterThanOrEqualValidator",
            &format!("The {label} value cannot be less than 1"),
            json!(value),
        );
        f.placeholders = vec![
            ("comparisonProperty".into(), json!("")),
            ("comparisonValue".into(), json!(1)),
        ];
        f
    })
}

/// `Language.All.Any(o => o.Id == value)`.
fn language_exists(id: i64) -> bool {
    unarr_core::movies::naming::iso_two_letter_code(id).is_some()
        || super::catalog::language_resource(id)["name"]
            .as_str()
            .is_some_and(|name| !name.is_empty() && name != "Unknown")
        || (-2..=0).contains(&id)
}

/// `PUT /config/ui/{id}`: the theme lives with the host settings.
pub async fn ui(State(state): State<AppState>, Json(body): Json<Value>) -> ApiResult<Response> {
    object(&body)?;
    let mut failures = Vec::new();
    for (property, label, key, invalid) in [
        (
            "MovieInfoLanguage",
            "Movie Info Language",
            "movieInfoLanguage",
            "Invalid Movie Info Language ID",
        ),
        (
            "UILanguage",
            "UI Language",
            "uiLanguage",
            "Invalid UI Language ID",
        ),
    ] {
        let value = int(&body, key);
        match language_at_least_one(property, label, value) {
            Some(failure) => failures.push(failure),
            None if !language_exists(value) => {
                failures.push(rule(property, "PredicateValidator", invalid, json!(value)))
            }
            None => {}
        }
    }
    check(failures)?;
    use Kind::*;
    save_config(
        &state,
        &body,
        &[
            ("firstDayOfWeek", Int),
            ("calendarWeekColumnHeader", Str),
            ("movieRuntimeFormat", Enum),
            ("shortDateFormat", Str),
            ("longDateFormat", Str),
            ("timeFormat", Str),
            ("showRelativeDates", Bool),
            ("enableColorImpairedMode", Bool),
            ("movieInfoLanguage", Int),
        ],
    )
    .await?;
    if let Some(theme) = body.get("theme").and_then(Value::as_str) {
        let mut overrides = host_overrides(&state).await?;
        overrides.insert("theme".into(), json!(theme));
        save_host_overrides(&state, overrides).await?;
    }
    // `UILanguage` is stored under that name.
    if let Some(v) = body.get("uiLanguage").and_then(Value::as_i64) {
        state
            .db()
            .call(move |conn| store::config::set_config(conn, "uilanguage", &v.to_string()))
            .await?;
    }
    let Json(out) = config::ui(State(state)).await?;
    Ok(accepted(out))
}

/// `IsValidFolderPermissionMask`: an octal of access permissions only,
/// with everything for the owner at least.
fn valid_folder_mask(mask: &str) -> bool {
    !mask.is_empty()
        && mask.chars().all(|c| ('0'..='7').contains(&c))
        && u32::from_str_radix(mask, 8).is_ok_and(|bits| bits & !0o777 == 0 && bits & 0o700 == 0o700)
}

/// `PUT /config/mediamanagement/{id}`.
pub async fn media_management(State(state): State<AppState>, Json(body): Json<Value>) -> ApiResult<Response> {
    object(&body)?;
    let mut failures = Vec::new();
    failures.extend(at_least(
        "RecycleBinCleanupDays",
        int(&body, "recycleBinCleanupDays"),
        0,
    ));
    let chmod = text(&body, "chmodFolder");
    if !chmod.is_empty() && !valid_folder_mask(&chmod) {
        failures.push(rule(
            "ChmodFolder",
            "FolderChmodValidator",
            "Must contain a valid Unix permissions octal",
            json!(chmod),
        ));
    }
    // The original's path validator throws on a relative recycling bin
    // before it can report it.
    let bin = text(&body, "recycleBin");
    if !bin.trim().is_empty() && !bin.starts_with('/') {
        return Err(ApiError::ServerException {
            message: format!(
                "value [{bin}]  is not a valid *nix path. paths must start with / (Parameter 'path')"
            ),
            exception: "System.ArgumentException".into(),
        });
    }
    if !bin.trim().is_empty() {
        let own_id = body.get("id").and_then(Value::as_i64).unwrap_or(0);
        let (roots, paths) = state
            .db()
            .call(move |conn| {
                Ok((
                    store::catalog::list_root_folders(conn)?,
                    movies::all_movie_paths(conn)?,
                ))
            })
            .await?;
        let roots: Vec<String> = roots.into_iter().map(|r| r.path).collect();
        let startup = state.runtime().startup_path.to_string_lossy().into_owned();
        let mut bin_failures = recycle_bin_failures(&bin, &roots, &paths, own_id, &startup);
        // The same rule in Radarr's words.
        for failure in &mut bin_failures {
            if failure.error_code.as_deref() == Some("SeriesPathValidator") {
                failure.error_code = Some("MoviePathValidator".into());
                failure.error_message = format!("Path '{bin}' is already configured for an existing movie");
            }
        }
        failures.extend(bin_failures);
    }
    if body["useScriptImport"].as_bool().unwrap_or(false) {
        let script = text(&body, "scriptImportPath");
        if !script.starts_with('/') {
            let mut f = rule(
                "ScriptImportPath",
                "PathValidator",
                &format!("Invalid Path: '{script}'"),
                json!(script),
            );
            f.placeholders = vec![("path".into(), json!(script))];
            failures.push(f);
        }
    }
    failures.extend(at_least(
        "MinimumFreeSpaceWhenImporting",
        int(&body, "minimumFreeSpaceWhenImporting"),
        100,
    ));
    check(failures)?;
    use Kind::*;
    save_config(
        &state,
        &body,
        &[
            ("autoUnmonitorPreviouslyDownloadedMovies", Bool),
            ("recycleBin", Str),
            ("recycleBinCleanupDays", Int),
            ("downloadPropersAndRepacks", Enum),
            ("createEmptyMovieFolders", Bool),
            ("deleteEmptyFolders", Bool),
            ("fileDate", Enum),
            ("rescanAfterRefresh", Enum),
            ("autoRenameFolders", Bool),
            ("pathsDefaultStatic", Bool),
            ("setPermissionsLinux", Bool),
            ("chmodFolder", Str),
            ("chownGroup", Str),
            ("skipFreeSpaceCheckWhenImporting", Bool),
            ("minimumFreeSpaceWhenImporting", Int),
            ("copyUsingHardlinks", Bool),
            ("useScriptImport", Bool),
            ("scriptImportPath", Str),
            ("importExtraFiles", Bool),
            ("extraFileExtensions", Str),
            ("enableMediaInfo", Bool),
        ],
    )
    .await?;
    let Json(out) = config::media_management(State(state)).await?;
    Ok(accepted(out))
}

/// `FileNameBuilder.MovieTitleRegex`.
static MOVIE_TITLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(\{(?:Movie)([- ._])(?:Clean)?(?:OriginalTitle|Title(?:The)?)(?::([a-z0-9|-]+))?\})")
        .unwrap()
});
/// `FileNameBuilder.ReleaseYearRegex`.
static RELEASE_YEAR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\{[-{ ._\[(]*Release[- ._]Year[-} ._)\]]*\}").unwrap());
/// `FileNameValidation.OriginalTokenRegex`.
static ORIGINAL_TOKEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(\{Original[- ._](?:Title|Filename)\})").unwrap());
/// `FileNameValidation.DeprecatedMovieFolderTokensRegex`.
static DEPRECATED_FOLDER_TOKENS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(\{[- ._\[\(]?(?:Original[- ._](?:Title|Filename)|Release[- ._]Group|Edition[- ._]Tags|Quality[- ._](?:Full|Title|Proper|Real)|MediaInfo[- ._](?:Video|VideoCodec|VideoBitDepth|Audio|AudioCodec|AudioChannels|AudioLanguages|AudioLanguagesAll|SubtitleLanguages|SubtitleLanguagesAll|3D|Simple|Full|VideoDynamicRange|VideoDynamicRangeType))[- ._\]\)]?\})").unwrap()
});

fn matches(pattern: &Regex, text: &str) -> bool {
    pattern.is_match(text).unwrap_or(false)
}

/// `ValidMovieFormat` and `ValidMovieFolderFormat`, every rule that fails.
fn naming_failures(body: &Value) -> Vec<ValidationFailure> {
    let mut failures = Vec::new();
    let standard = text(body, "standardMovieFormat");
    if standard.is_empty() {
        failures.push(ValidationFailure::not_empty(
            "StandardMovieFormat",
            json!(standard),
        ));
    }
    let original = matches(&ORIGINAL_TOKEN, &standard);
    if !(original || (matches(&MOVIE_TITLE, &standard) && matches(&RELEASE_YEAR, &standard))) {
        failures.push(rule(
            "StandardMovieFormat",
            "ValidMovieFormatValidator",
            "Must contain either movie title and release year OR Original Title/Filename",
            json!(standard),
        ));
    }
    let folder = text(body, "movieFolderFormat");
    if folder.is_empty() {
        failures.push(ValidationFailure::not_empty("MovieFolderFormat", json!(folder)));
    }
    let deprecated: Vec<String> = DEPRECATED_FOLDER_TOKENS
        .find_iter(&folder)
        .filter_map(Result::ok)
        .map(|m| m.as_str().to_string())
        .collect();
    if !deprecated.is_empty() {
        let tokens = deprecated.join(", ");
        let mut f = rule(
            "MovieFolderFormat",
            "IllegalMovieFolderTokensValidator",
            &format!("Must not contain deprecated tokens derived from file properties: {tokens}"),
            json!(folder),
        );
        f.placeholders = vec![("tokens".into(), json!(tokens))];
        failures.push(f);
    }
    if !matches(&MOVIE_TITLE, &folder) {
        failures.push(rule(
            "MovieFolderFormat",
            "ValidMovieFolderFormatValidator",
            "Must contain movie title",
            json!(folder),
        ));
    }
    failures
}

/// `ColonReplacementFormat` by its name in any case, or its number.
fn colon_replacement(v: &Value) -> Option<i64> {
    match v {
        Value::String(name) => match name.to_ascii_lowercase().as_str() {
            "delete" => Some(0),
            "dash" => Some(1),
            "spacedash" => Some(2),
            "spacedashspace" => Some(3),
            "smart" => Some(4),
            _ => None,
        },
        Value::Number(n) => n.as_i64().filter(|n| (0..=4).contains(n)),
        _ => None,
    }
}

/// `PUT /config/naming/{id}`.
pub async fn naming(State(state): State<AppState>, Json(body): Json<Value>) -> ApiResult<Response> {
    object(&body)?;
    check(naming_failures(&body))?;
    let b = body.clone();
    state
        .db()
        .call(move |conn| {
            let mut n = movies::naming(conn)?;
            if let Some(v) = b["renameMovies"].as_bool() {
                n.rename_movies = v;
            }
            if let Some(v) = b["replaceIllegalCharacters"].as_bool() {
                n.replace_illegal_characters = v;
            }
            if let Some(v) = colon_replacement(&b["colonReplacementFormat"]) {
                n.colon_replacement_format = v;
            }
            if let Some(v) = b["standardMovieFormat"].as_str() {
                n.standard_movie_format = v.to_string();
            }
            if let Some(v) = b["movieFolderFormat"].as_str() {
                n.movie_folder_format = v.to_string();
            }
            movies::set_naming(conn, &n)
        })
        .await?;
    let Json(out) = config::naming(State(state)).await?;
    Ok(accepted(out))
}

/// `PUT /config/indexer/{id}`.
pub async fn indexer(State(state): State<AppState>, Json(body): Json<Value>) -> ApiResult<Response> {
    object(&body)?;
    let mut failures = Vec::new();
    failures.extend(at_least("MinimumAge", int(&body, "minimumAge"), 0));
    failures.extend(at_least("MaximumSize", int(&body, "maximumSize"), 0));
    failures.extend(at_least("Retention", int(&body, "retention"), 0));
    let rss = int(&body, "rssSyncInterval");
    if rss != 0 && !(10..=120).contains(&rss) {
        failures.push(rule(
            "RssSyncInterval",
            "RssSyncIntervalValidator",
            "Must be between 10 and 120 or 0 to disable",
            json!(rss),
        ));
    }
    check(failures)?;
    use Kind::*;
    save_config(
        &state,
        &body,
        &[
            ("minimumAge", Int),
            ("maximumSize", Int),
            ("retention", Int),
            ("rssSyncInterval", Int),
            ("preferIndexerFlags", Bool),
            ("availabilityDelay", Int),
            ("allowHardcodedSubs", Bool),
            ("whitelistedHardcodedSubs", Str),
        ],
    )
    .await?;
    let Json(out) = config::indexer(State(state)).await?;
    Ok(accepted(out))
}

/// `PUT /config/downloadclient/{id}`.
pub async fn download_client(State(state): State<AppState>, Json(body): Json<Value>) -> ApiResult<Response> {
    object(&body)?;
    use Kind::*;
    save_config(
        &state,
        &body,
        &[
            ("downloadClientWorkingFolders", Str),
            ("enableCompletedDownloadHandling", Bool),
            ("checkForFinishedDownloadInterval", Int),
            ("autoRedownloadFailed", Bool),
            ("autoRedownloadFailedFromInteractiveSearch", Bool),
        ],
    )
    .await?;
    let Json(out) = config::download_client(State(state)).await?;
    Ok(accepted(out))
}

/// `PUT /config/importlist/{id}`: the level is a string to the original,
/// which stores whatever it is given.
pub async fn import_list(State(state): State<AppState>, Json(body): Json<Value>) -> ApiResult<Response> {
    object(&body)?;
    use Kind::*;
    save_config(&state, &body, &[("listSyncLevel", Str)]).await?;
    let Json(out) = config::import_list(State(state)).await?;
    Ok(accepted(out))
}

/// `TMDbCountryCode`'s names.
const CERTIFICATION_COUNTRIES: &[&str] = &[
    "au", "br", "ca", "fr", "de", "gb", "in", "ie", "it", "nz", "ro", "es", "us",
];

/// Where in a request body the string value of `key` ends, as the JSON
/// reader counts when it refuses that value.
fn end_of_string_value(raw: &str, key: &str) -> usize {
    let Some(at) = raw.find(&format!("\"{key}\"")) else {
        return 0;
    };
    let after_key = at + key.len() + 2;
    let Some(open) = raw[after_key..].find('"') else {
        return 0;
    };
    let start = after_key + open + 1;
    let mut escaped = false;
    for (offset, c) in raw[start..].char_indices() {
        match c {
            '\\' if !escaped => escaped = true,
            '"' if !escaped => return start + offset + 1,
            _ => escaped = false,
        }
    }
    0
}

/// `PUT /config/metadata/{id}`.
pub async fn metadata(State(state): State<AppState>, raw: axum::body::Bytes) -> ApiResult<Response> {
    let text_body = String::from_utf8_lossy(&raw).into_owned();
    let body: Value = serde_json::from_slice(&raw)
        .map_err(|_| ApiError::BadRequest("The request body must be a JSON object".into()))?;
    object(&body)?;
    use Kind::*;
    // An enumeration: a name model binding does not know is its refusal,
    // before the controller is reached.
    if let Some(country) = body["certificationCountry"].as_str() {
        if !CERTIFICATION_COUNTRIES.contains(&country.to_ascii_lowercase().as_str()) {
            let position = end_of_string_value(&text_body, "certificationCountry");
            return Ok(crate::error::validation_problem(
                crate::error::BAD_REQUEST_TYPE_RFC9110,
                "application/problem+json; charset=utf-8",
                json!({"$.certificationCountry": [format!(
                    "The JSON value could not be converted to NzbDrone.Core.MetadataSource.SkyHook.Resource.TMDbCountryCode. Path: $.certificationCountry | LineNumber: 0 | BytePositionInLine: {position}."
                )]}),
            ));
        }
    }
    save_config(&state, &body, &[("certificationCountry", Enum)]).await?;
    let Json(out) = config::metadata(State(state)).await?;
    Ok(accepted(out))
}
