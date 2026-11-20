//! `config/naming` and `config/mediamanagement`.

use axum::extract::State;
use axum::http::Uri;
use axum::Json;
use serde_json::{json, Value};
use unarr_core::model::{QualityModel, SeriesType};
use unarr_core::naming::{
    self, NamingCustomFormat, NamingEpisode, NamingFile, NamingMediaInfo, NamingSeries,
};
use unarr_core::store;

use crate::error::ApiResult;
use crate::query::Params;
use crate::routes::config::Values;
use crate::state::AppState;

pub async fn naming(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let n = state.db().call(|c| store::config::naming(c)).await?;
    Ok(Json(json!({
        "renameEpisodes": n.rename_episodes,
        "replaceIllegalCharacters": n.replace_illegal_characters,
        "colonReplacementFormat": n.colon_replacement_format,
        "customColonReplacementFormat": n.custom_colon_replacement_format,
        "multiEpisodeStyle": n.multi_episode_style,
        "standardEpisodeFormat": n.standard_episode_format,
        "dailyEpisodeFormat": n.daily_episode_format,
        "animeEpisodeFormat": n.anime_episode_format,
        "seriesFolderFormat": n.series_folder_format,
        "seasonFolderFormat": n.season_folder_format,
        "specialsFolderFormat": n.specials_folder_format,
        "id": 1,
    })))
}

pub async fn media_management(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let v = Values::load(&state).await?;
    Ok(Json(json!({
        "autoUnmonitorPreviouslyDownloadedEpisodes": v.bool("AutoUnmonitorPreviouslyDownloadedEpisodes", false),
        "recycleBin": v.str("RecycleBin", ""),
        "recycleBinCleanupDays": v.int("RecycleBinCleanupDays", 7),
        "downloadPropersAndRepacks": v.enum_name("DownloadPropersAndRepacks", "preferAndUpgrade"),
        "createEmptySeriesFolders": v.bool("CreateEmptySeriesFolders", false),
        "deleteEmptyFolders": v.bool("DeleteEmptyFolders", false),
        "fileDate": v.enum_name("FileDate", "none"),
        "rescanAfterRefresh": v.enum_name("RescanAfterRefresh", "always"),
        "setPermissionsLinux": v.bool("SetPermissionsLinux", false),
        "chmodFolder": v.str("ChmodFolder", "755"),
        "chownGroup": v.str("ChownGroup", ""),
        "episodeTitleRequired": v.enum_name("EpisodeTitleRequired", "always"),
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

pub async fn indexer_config(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let v = Values::load(&state).await?;
    Ok(Json(json!({
        "minimumAge": v.int("MinimumAge", 0),
        "maximumSize": v.int("MaximumSize", 0),
        "retention": v.int("Retention", 0),
        "rssSyncInterval": v.int("RssSyncInterval", 15),
        "id": 1,
    })))
}

// --- naming examples

/// The sample the originals render their naming examples with
/// (`FileNameSampleService`): one fictional series in three flavours, three
/// episodes, a file per flavour and two custom formats that rename.
struct Sample {
    standard: NamingSeries<'static>,
    daily: NamingSeries<'static>,
    anime: NamingSeries<'static>,
    episodes: [NamingEpisode<'static>; 3],
    media_info: NamingMediaInfo,
    media_info_anime: NamingMediaInfo,
}

impl Sample {
    fn new() -> Sample {
        let series = |series_type| NamingSeries {
            title: "The Series Title's!",
            year: 2010,
            series_type,
            imdb_id: Some("tt12345"),
            tvdb_id: 12345,
            tv_maze_id: 54321,
            tmdb_id: 11223,
        };
        let episode = |n, title, air_date| NamingEpisode {
            season_number: 1,
            episode_number: n,
            title,
            air_date,
            absolute_episode_number: Some(n),
        };
        // The originals' sample carries the old MediaInfo field names ("AVC",
        // "DTS"), which their formatter passes through, and no HDR format.
        let media_info = |audio: &str, subs: &[&str]| NamingMediaInfo {
            video_codec: "AVC".into(),
            video_bit_depth: 10,
            audio_codec: "DTS".into(),
            audio_channels: 5.1,
            audio_languages: vec![audio.to_string()],
            subtitles: subs.iter().map(|s| s.to_string()).collect(),
            video_dynamic_range: String::new(),
            video_dynamic_range_type: String::new(),
        };
        Sample {
            standard: series(SeriesType::Standard),
            daily: series(SeriesType::Daily),
            anime: series(SeriesType::Anime),
            episodes: [
                episode(1, "Episode Title (1)", Some("2013-10-30")),
                episode(2, "Episode Title (2)", None),
                episode(3, "Episode Title (3)", None),
            ],
            media_info: media_info("ger", &["eng", "ger"]),
            media_info_anime: media_info("jpn", &["jpn", "eng"]),
        }
    }

    fn file<'a>(&'a self, scene: &'a str, anime: bool, quality_title: &'a str) -> NamingFile<'a> {
        NamingFile {
            quality: QualityModel {
                quality_id: 3,
                version: 2,
                real: 0,
                is_repack: false,
            },
            quality_title,
            relative_path: scene,
            scene_name: Some(scene),
            release_group: Some("RlsGrp"),
            release_hash: None,
            media_info: Some(if anime {
                &self.media_info_anime
            } else {
                &self.media_info
            }),
            is_new: true,
        }
    }
}

pub async fn naming_examples(State(state): State<AppState>, uri: Uri) -> ApiResult<Json<Value>> {
    // `GetExamples([FromQuery] NamingConfigResource)`: the form as it is
    // being edited, when it comes with the configuration's id; else the
    // stored configuration.
    let p = Params::from_uri(&uri);
    let form = (p.i64("id").unwrap_or(0) != 0).then(|| {
        let mut form = serde_json::Map::new();
        for key in ["renameEpisodes", "replaceIllegalCharacters"] {
            if let Some(v) = p.get(key) {
                form.insert(key.into(), json!(v.eq_ignore_ascii_case("true")));
            }
        }
        for key in ["colonReplacementFormat", "multiEpisodeStyle"] {
            if let Some(v) = p.i64(key) {
                form.insert(key.into(), json!(v));
            }
        }
        for key in [
            "customColonReplacementFormat",
            "standardEpisodeFormat",
            "dailyEpisodeFormat",
            "animeEpisodeFormat",
            "seriesFolderFormat",
            "seasonFolderFormat",
            "specialsFolderFormat",
        ] {
            if let Some(v) = p.get(key) {
                form.insert(key.into(), json!(v));
            }
        }
        Value::Object(form)
    });
    let (mut n, definitions) = state
        .db()
        .call(|c| {
            Ok((
                store::config::naming(c)?,
                store::catalog::list_quality_definitions(c)?,
            ))
        })
        .await?;
    if let Some(form) = &form {
        crate::routes::settings_writes::apply_naming(&mut n, form);
    }
    // The sample file is WEBDL-1080p; its title is whatever the user calls it.
    let quality_title = definitions
        .iter()
        .find(|d| d.quality_id == 3)
        .map(|d| d.title.clone())
        .unwrap_or_else(|| "WEBDL-1080p".to_string());
    let sample = Sample::new();
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
    let single = &sample.episodes[..1];
    let multi = &sample.episodes[..];
    let build = |episodes: &[NamingEpisode], series: &NamingSeries, scene: &str, anime: bool| {
        let file = sample.file(scene, anime, &quality_title);
        // An empty format is an error for the originals too: the example is blank.
        naming::build_file_name(episodes, series, &file, "", &n, &formats).unwrap_or_default()
    };
    // `FileNameValidationService`: an example whose name does not parse
    // back to the sample's episodes is left out, and the form shows the
    // format as invalid.
    let parsed = |name: &str| {
        if name.contains('/') {
            unarr_core::parser::parse_path(name)
        } else {
            unarr_core::parser::parse_title(name)
        }
    };
    let numbers_match = |info: &unarr_core::parser::ParsedEpisodeInfo, episodes: &[NamingEpisode]| {
        let mut got = info.episode_numbers.clone();
        got.sort_unstable();
        let mut want: Vec<i64> = episodes.iter().map(|e| e.episode_number).collect();
        want.sort_unstable();
        info.season_number == episodes[0].season_number && got == want
    };
    let standard_ok =
        |name: &str, episodes: &[NamingEpisode]| parsed(name).is_some_and(|i| numbers_match(&i, episodes));
    let daily_ok = |name: &str, episodes: &[NamingEpisode]| {
        parsed(name).is_some_and(|i| match &i.air_date {
            Some(date) => Some(date.as_str()) == episodes[0].air_date,
            None => numbers_match(&i, episodes),
        })
    };
    let anime_ok = |name: &str, episodes: &[NamingEpisode]| {
        parsed(name).is_some_and(|i| match i.absolute_episode_numbers.first() {
            Some(first) => Some(*first) == episodes[0].absolute_episode_number,
            None => numbers_match(&i, episodes),
        })
    };
    let mut out = serde_json::Map::new();
    let mut example = |key: &str, name: String, ok: bool| {
        if ok {
            out.insert(key.into(), json!(name));
        }
    };
    let name = build(
        single,
        &sample.standard,
        "The.Series.Title's!.S01E01.1080p.WEBDL.x264-EVOLVE",
        false,
    );
    let ok = standard_ok(&name, single);
    example("singleEpisodeExample", name, ok);
    let name = build(
        multi,
        &sample.standard,
        "The.Series.Title's!.S01E01-E03.1080p.WEBDL.x264-EVOLVE",
        false,
    );
    let ok = standard_ok(&name, multi);
    example("multiEpisodeExample", name, ok);
    let name = build(
        single,
        &sample.daily,
        "The.Series.Title's!.2013.10.30.1080p.WEBDL.x264-EVOLVE",
        false,
    );
    let ok = daily_ok(&name, single);
    example("dailyEpisodeExample", name, ok);
    let name = build(
        single,
        &sample.anime,
        "[RlsGroup] The Series Title's! - 001 [1080P]",
        true,
    );
    let ok = anime_ok(&name, single);
    example("animeEpisodeExample", name, ok);
    let name = build(
        multi,
        &sample.anime,
        "[RlsGroup] The Series Title's! - 001 - 103 [1080p]",
        true,
    );
    let ok = anime_ok(&name, multi);
    example("animeMultiEpisodeExample", name, ok);
    if !n.series_folder_format.trim().is_empty() {
        out.insert(
            "seriesFolderExample".into(),
            json!(naming::series_folder(&sample.standard, &n)),
        );
    }
    if !n.season_folder_format.trim().is_empty() {
        out.insert(
            "seasonFolderExample".into(),
            json!(naming::season_folder(&sample.standard, 1, &n)),
        );
    }
    if !n.specials_folder_format.trim().is_empty() {
        out.insert(
            "specialsFolderExample".into(),
            json!(naming::season_folder(&sample.standard, 0, &n)),
        );
    }
    Ok(Json(Value::Object(out)))
}

/// The language profile Sonarr v3 had; v4 keeps one fixed "Deprecated"
/// profile for old clients (Bazarr still asks for it).
pub fn language_profile_resource() -> Value {
    json!({
        "id": 1,
        "name": "Deprecated",
        "upgradeAllowed": true,
        "cutoff": {"id": 1, "name": "English"},
        "languages": [{"language": {"id": 1, "name": "English"}, "allowed": true}],
    })
}

pub async fn language_profiles() -> Json<Vec<Value>> {
    Json(vec![language_profile_resource()])
}

/// `GET /languageprofile/{id}` and `/languageprofile/schema`: the one
/// profile, whatever the id.
pub async fn language_profile(axum::extract::Path(_id): axum::extract::Path<i64>) -> Json<Value> {
    Json(language_profile_resource())
}

pub async fn language_profile_schema() -> Json<Value> {
    Json(language_profile_resource())
}

/// What `LanguageProfileController` answers a write with: the resource it
/// read from the body, written back as it serializes one (nothing unknown,
/// nothing null, no id of 0). Nothing is stored.
fn language_profile_echo(body: &Value, id: i64) -> Value {
    fn language(v: &Value) -> Option<Value> {
        let v = v.as_object()?;
        let mut out = serde_json::Map::new();
        for key in ["id", "name"] {
            if let Some(value) = v.get(key).filter(|value| !value.is_null()) {
                out.insert(key.into(), value.clone());
            }
        }
        Some(Value::Object(out))
    }
    let mut out = serde_json::Map::new();
    if let Some(name) = body.get("name").filter(|v| !v.is_null()) {
        out.insert("name".into(), name.clone());
    }
    out.insert(
        "upgradeAllowed".into(),
        json!(body["upgradeAllowed"].as_bool().unwrap_or(false)),
    );
    if let Some(cutoff) = language(&body["cutoff"]) {
        out.insert("cutoff".into(), cutoff);
    }
    if let Some(items) = body["languages"].as_array() {
        let items: Vec<Value> = items
            .iter()
            .map(|item| {
                let mut entry = serde_json::Map::new();
                if let Some(language) = language(&item["language"]) {
                    entry.insert("language".into(), language);
                }
                entry.insert(
                    "allowed".into(),
                    json!(item["allowed"].as_bool().unwrap_or(false)),
                );
                Value::Object(entry)
            })
            .collect();
        out.insert("languages".into(), Value::Array(items));
    }
    if id != 0 {
        out.insert("id".into(), json!(id));
    }
    Value::Object(out)
}

fn object_body(body: &Value) -> ApiResult<()> {
    if body.is_object() {
        Ok(())
    } else {
        Err(crate::error::ApiError::BadRequest(
            "The request body must be a JSON object".into(),
        ))
    }
}

/// `POST /languageprofile`: 202 with the body read back.
pub async fn language_profile_create(
    Json(body): Json<Value>,
) -> ApiResult<(axum::http::StatusCode, Json<Value>)> {
    object_body(&body)?;
    let id = body["id"].as_i64().unwrap_or(0);
    Ok((
        axum::http::StatusCode::ACCEPTED,
        Json(language_profile_echo(&body, id)),
    ))
}

/// `PUT /languageprofile`: the id the body names, which must be one.
pub async fn language_profile_update(
    Json(body): Json<Value>,
) -> ApiResult<(axum::http::StatusCode, Json<Value>)> {
    object_body(&body)?;
    let id = body["id"].as_i64().unwrap_or(0);
    if id <= 0 {
        return Err(crate::error::ApiError::Validation(vec![
            unarr_core::validation::ValidationFailure::greater_than("Id", json!(id), 0),
        ]));
    }
    Ok((
        axum::http::StatusCode::ACCEPTED,
        Json(language_profile_echo(&body, id)),
    ))
}

/// `PUT /languageprofile/{id}`: the body's id, or the route's when the
/// body has none.
pub async fn language_profile_update_id(
    axum::extract::Path(route_id): axum::extract::Path<i64>,
    Json(body): Json<Value>,
) -> ApiResult<(axum::http::StatusCode, Json<Value>)> {
    object_body(&body)?;
    let id = match body["id"].as_i64().unwrap_or(0) {
        0 => route_id,
        id => id,
    };
    Ok((
        axum::http::StatusCode::ACCEPTED,
        Json(language_profile_echo(&body, id)),
    ))
}

/// `DELETE /languageprofile/{id}`: an empty 200, and the profile stays.
pub async fn language_profile_delete(
    axum::extract::Path(_id): axum::extract::Path<i64>,
) -> axum::http::StatusCode {
    axum::http::StatusCode::OK
}
