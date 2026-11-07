//! `config/naming` and `config/mediamanagement`.

use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};
use unarr_core::model::{QualityModel, SeriesType};
use unarr_core::naming::{
    self, NamingCustomFormat, NamingEpisode, NamingFile, NamingMediaInfo, NamingSeries,
};
use unarr_core::store;

use crate::error::ApiResult;
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

pub async fn naming_examples(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let (n, definitions) = state
        .db()
        .call(|c| {
            Ok((
                store::config::naming(c)?,
                store::catalog::list_quality_definitions(c)?,
            ))
        })
        .await?;
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
    Ok(Json(json!({
        "singleEpisodeExample": build(single, &sample.standard, "The.Series.Title's!.S01E01.1080p.WEBDL.x264-EVOLVE", false),
        "multiEpisodeExample": build(multi, &sample.standard, "The.Series.Title's!.S01E01-E03.1080p.WEBDL.x264-EVOLVE", false),
        "dailyEpisodeExample": build(single, &sample.daily, "The.Series.Title's!.2013.10.30.1080p.WEBDL.x264-EVOLVE", false),
        "animeEpisodeExample": build(single, &sample.anime, "[RlsGroup] The Series Title's! - 001 [1080P]", true),
        "animeMultiEpisodeExample": build(multi, &sample.anime, "[RlsGroup] The Series Title's! - 001 - 103 [1080p]", true),
        "seriesFolderExample": naming::series_folder(&sample.standard, &n),
        "seasonFolderExample": naming::season_folder(&sample.standard, 1, &n),
        "specialsFolderExample": naming::season_folder(&sample.standard, 0, &n),
    })))
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

pub async fn language_profile(axum::extract::Path(id): axum::extract::Path<i64>) -> ApiResult<Json<Value>> {
    if id == 1 {
        Ok(Json(language_profile_resource()))
    } else {
        Err(crate::error::ApiError::NotFound)
    }
}
