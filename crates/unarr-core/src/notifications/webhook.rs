//! The generic webhook, after the originals' `Webhook`, `WebhookBase` and
//! `WebhookProxy`: a JSON payload per event, `POST` or `PUT` as configured,
//! with basic credentials and extra headers when set.

use serde_json::{json, Map, Value};

use super::{
    quality_name, send_json, Context, DeleteMessage, DownloadMessage, GrabMessage, Identity,
    ImportCompleteMessage, ManualInteractionMessage, NotificationDefinition, SendError, TestOutcome,
};
use crate::health::HealthCheck;
use crate::mediafiles::events::EpisodeFileRenamed;
use crate::mediafiles::format;
use crate::mediafiles::local::{DeleteReason, GrabbedRelease};
use crate::model::{Episode, EpisodeFile, ReleaseType, Series};
use crate::validation::ValidationFailure;

/// `WebhookMethod`.
const METHOD_POST: i64 = 1;
const METHOD_PUT: i64 = 2;

/// `WebhookSettings`.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub url: String,
    pub method: i64,
    pub username: String,
    pub password: String,
    pub headers: Vec<(String, String)>,
}

impl Settings {
    pub fn from_value(v: &Value) -> Settings {
        let text = |key: &str| v[key].as_str().unwrap_or("").to_string();
        Settings {
            url: text("url"),
            method: v["method"].as_i64().unwrap_or(METHOD_POST),
            username: text("username"),
            password: text("password"),
            headers: v["headers"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|h| {
                            (
                                h["key"].as_str().unwrap_or("").to_string(),
                                h["value"].as_str().unwrap_or("").to_string(),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    /// `WebhookSettingsValidator`: the URL must be valid.
    pub fn validate(&self) -> Vec<ValidationFailure> {
        super::valid_url("Url", &self.url).into_iter().collect()
    }
}

/// A payload object: the originals serialise with nulls left out, so a
/// value only joins when it is there.
struct Object(Map<String, Value>);

impl Object {
    fn new() -> Object {
        Object(Map::new())
    }

    fn set(&mut self, key: &str, value: Value) -> &mut Self {
        if !value.is_null() {
            self.0.insert(key.into(), value);
        }
        self
    }

    fn opt<T: serde::Serialize>(&mut self, key: &str, value: Option<T>) -> &mut Self {
        if let Some(v) = value {
            self.0.insert(key.into(), json!(v));
        }
        self
    }

    fn done(self) -> Value {
        Value::Object(self.0)
    }
}

fn base(event_type: &str, ctx: &Context<'_>) -> Object {
    let mut o = Object::new();
    o.set("eventType", json!(event_type));
    o.set("instanceName", json!(ctx.identity.instance_name));
    o.set("applicationUrl", json!(ctx.application_url));
    o
}

/// `WebhookSeries`, with the covers' local URLs.
fn series_json(series: &Series, ctx: &Context<'_>) -> Value {
    let images: Vec<Value> = series
        .images
        .iter()
        .map(|i| {
            let ext = if i.cover_type == "clearlogo" { "png" } else { "jpg" };
            let mut o = Object::new();
            o.set("coverType", json!(i.cover_type));
            o.set(
                "url",
                json!(format!(
                    "{}/MediaCover/{}/{}.{ext}",
                    ctx.identity.url_base, series.id, i.cover_type
                )),
            );
            o.opt("remoteUrl", i.remote_url.as_ref());
            o.done()
        })
        .collect();
    let language = crate::quality::Language::by_id(series.original_language_id);
    let mut o = Object::new();
    o.set("id", json!(series.id));
    o.set("title", json!(series.title));
    o.set("titleSlug", json!(series.title_slug));
    o.set("path", json!(series.path));
    o.set("tvdbId", json!(series.tvdb_id));
    o.set("tvMazeId", json!(series.tv_maze_id));
    o.set("tmdbId", json!(series.tmdb_id));
    o.opt("imdbId", series.imdb_id.as_ref());
    o.set("type", json!(series.series_type.as_str()));
    o.set("year", json!(series.year));
    o.set("genres", json!(series.genres));
    o.set("images", Value::Array(images));
    o.set("tags", json!(ctx.tag_labels(series)));
    o.set(
        "originalLanguage",
        json!({"id": language.id, "name": language.name}),
    );
    o.done()
}

/// `WebhookEpisode`.
fn episode_json(e: &Episode) -> Value {
    let mut o = Object::new();
    o.set("id", json!(e.id));
    o.set("episodeNumber", json!(e.episode_number));
    o.set("seasonNumber", json!(e.season_number));
    o.opt("title", e.title.as_ref());
    o.opt("overview", e.overview.as_ref());
    o.opt("airDate", e.air_date.map(|d| d.to_string()));
    o.opt("airDateUtc", e.air_date_utc.map(crate::time::format_utc));
    o.set("seriesId", json!(e.series_id));
    o.set("tvdbId", json!(e.tvdb_id));
    o.done()
}

fn episodes_json(episodes: &[Episode]) -> Value {
    Value::Array(episodes.iter().map(episode_json).collect())
}

/// `WebhookEpisodeFileMediaInfo`.
fn media_info_json(file: &EpisodeFile) -> Option<Value> {
    let raw = file.media_info.as_ref()?;
    let scene = file.scene_name.as_deref();
    let distinct = |items: Vec<String>| {
        let mut out: Vec<String> = Vec::new();
        for i in items {
            if !out.contains(&i) {
                out.push(i);
            }
        }
        out
    };
    let hdr = format::str(raw, "videoHdrFormat").unwrap_or("none");
    let mut o = Object::new();
    o.set(
        "audioChannels",
        Value::Number(serde_json::Number::from_f64(format::audio_channels(raw)).unwrap_or_else(|| 0.into())),
    );
    o.opt(
        "audioCodec",
        format::audio_codec(
            format::str(raw, "audioFormat"),
            format::str(raw, "audioCodecID").unwrap_or(""),
            format::str(raw, "audioProfile").unwrap_or(""),
        ),
    );
    o.set(
        "audioLanguages",
        json!(distinct(format::list(raw, "audioLanguages"))),
    );
    o.set("height", json!(format::int(raw, "height")));
    o.set("width", json!(format::int(raw, "width")));
    o.set("subtitles", json!(distinct(format::list(raw, "subtitles"))));
    o.opt(
        "videoCodec",
        format::video_codec(
            format::str(raw, "videoFormat"),
            format::str(raw, "videoCodecID").unwrap_or(""),
            scene,
        ),
    );
    o.set("videoDynamicRange", json!(format::dynamic_range(hdr)));
    o.set("videoDynamicRangeType", json!(format::dynamic_range_type(hdr)));
    Some(o.done())
}

/// `WebhookEpisodeFile`.
fn file_object(series: &Series, file: &EpisodeFile) -> Object {
    let mut o = Object::new();
    o.set("id", json!(file.id));
    o.set("relativePath", json!(file.relative_path));
    o.set(
        "path",
        json!(crate::mediafiles::join(&series.path, &file.relative_path)),
    );
    o.set("quality", json!(quality_name(&file.quality)));
    o.set("qualityVersion", json!(file.quality.version));
    o.opt("releaseGroup", file.release_group.as_ref());
    o.opt("sceneName", file.scene_name.as_ref());
    o.set("size", json!(file.size));
    o.set("dateAdded", json!(super::timestamp(file.date_added)));
    o.set("languages", crate::commands::languages_json(&file.languages));
    o.opt("mediaInfo", media_info_json(file));
    o
}

fn file_json(series: &Series, file: &EpisodeFile) -> Value {
    file_object(series, file).done()
}

/// `WebhookGrabbedRelease`: nothing but the release type without a release.
fn grabbed_release_json(release: Option<&GrabbedRelease>, fallback_type: ReleaseType) -> Value {
    let mut o = Object::new();
    match release {
        Some(r) => {
            o.set("releaseTitle", json!(r.title));
            o.opt("indexer", r.indexer.as_ref());
            o.set("size", json!(r.size));
            o.set("releaseType", json!(r.release_type.as_str()));
        }
        None => {
            o.set("releaseType", json!(fallback_type.as_str()));
        }
    }
    o.done()
}

/// `WebhookCustomFormatInfo`.
fn custom_format_info(formats: &[crate::model::CustomFormat], score: i64) -> Value {
    json!({
        "customFormats": formats.iter().map(|f| json!({"id": f.id, "name": f.name})).collect::<Vec<_>>(),
        "customFormatScore": score,
    })
}

fn client_fields(
    o: &mut Object,
    client: Option<&crate::downloadclients::ItemClientInfo>,
    download_id: Option<&str>,
) {
    o.opt("downloadClient", client.map(|c| c.name.clone()));
    o.opt("downloadClientType", client.map(|c| c.kind.display_name()));
    o.opt("downloadId", download_id);
}

/// `DeleteMediaFileReason` as the originals' camel-case enum.
fn delete_reason(reason: DeleteReason) -> &'static str {
    match reason {
        DeleteReason::MissingFromDisk => "missingFromDisk",
        DeleteReason::Manual => "manual",
        DeleteReason::Upgrade => "upgrade",
        DeleteReason::NoLinkedEpisodes => "noLinkedEpisodes",
        DeleteReason::ManualOverride => "manualOverride",
    }
}

/// `WebhookProxy.SendWebhook`: a failed HTTP call is a
/// `WebhookException("Unable to post to webhook: {0}")`; a call that never
/// got an answer escapes as it is.
async fn send(http: &reqwest::Client, settings: &Settings, payload: &Value) -> Result<(), SendError> {
    let mut request = match settings.method {
        METHOD_PUT => http.put(&settings.url),
        _ => http.post(&settings.url),
    };
    request = request
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(payload.to_string());
    if !settings.username.trim().is_empty() || !settings.password.trim().is_empty() {
        request = request.basic_auth(&settings.username, Some(&settings.password));
    }
    for (k, v) in &settings.headers {
        if !k.trim().is_empty() {
            request = request.header(k.as_str(), v.as_str());
        }
    }
    match send_json(http, request).await {
        Ok(_) => Ok(()),
        Err(SendError::Status(code, message)) => Err(SendError::Status(
            code,
            format!("Unable to post to webhook: {message}"),
        )),
        Err(e) => Err(e),
    }
}

fn send_error_message(e: SendError) -> String {
    match e {
        SendError::Status(_, m) | SendError::Transport(m) => m,
    }
}

pub async fn on_grab(
    http: &reqwest::Client,
    def: &NotificationDefinition,
    m: &GrabMessage<'_>,
    ctx: &Context<'_>,
) -> Result<(), String> {
    let settings = Settings::from_value(&def.settings);
    let parsed = m.remote.parsed.as_ref();
    let mut release = Object::new();
    release.set("quality", json!(quality_name(&m.quality)));
    release.set("qualityVersion", json!(m.quality.version));
    release.opt("releaseGroup", parsed.and_then(|p| p.release_group.as_ref()));
    release.set("releaseTitle", json!(m.remote.release.title));
    release.set("indexer", json!(m.remote.release.indexer));
    release.set("size", json!(m.remote.release.size));
    release.set("customFormatScore", json!(m.remote.custom_format_score));
    release.set(
        "customFormats",
        json!(m
            .remote
            .custom_formats
            .iter()
            .map(|f| f.name.clone())
            .collect::<Vec<_>>()),
    );
    release.set("languages", crate::commands::languages_json(&m.remote.languages));
    let mut o = base("Grab", ctx);
    o.set("series", series_json(m.series, ctx));
    o.set("episodes", episodes_json(m.episodes));
    o.set("release", release.done());
    o.set("downloadClient", json!(m.download_client_name));
    o.set("downloadClientType", json!(m.download_client_type));
    o.opt("downloadId", m.download_id);
    o.set(
        "customFormatInfo",
        custom_format_info(&m.remote.custom_formats, m.remote.custom_format_score),
    );
    send(http, &settings, &o.done()).await.map_err(send_error_message)
}

pub async fn on_download(
    http: &reqwest::Client,
    def: &NotificationDefinition,
    m: &DownloadMessage<'_>,
    ctx: &Context<'_>,
) -> Result<(), String> {
    let settings = Settings::from_value(&def.settings);
    let mut file = file_object(m.series, m.file);
    file.set("sourcePath", json!(m.source_path));
    let mut o = base("Download", ctx);
    o.set("series", series_json(m.series, ctx));
    o.set("episodes", episodes_json(m.episodes));
    o.set("episodeFile", file.done());
    o.set("release", grabbed_release_json(m.release, ReleaseType::Unknown));
    o.set("isUpgrade", json!(!m.old_files.is_empty()));
    client_fields(&mut o, m.client, m.download_id);
    o.set(
        "customFormatInfo",
        custom_format_info(m.custom_formats, m.custom_format_score),
    );
    if !m.old_files.is_empty() {
        let deleted: Vec<Value> = m
            .old_files
            .iter()
            .map(|d| {
                let mut f = file_object(m.series, &d.file);
                f.opt("recycleBinPath", d.recycle_bin_path.as_ref());
                f.done()
            })
            .collect();
        o.set("deletedFiles", Value::Array(deleted));
    }
    send(http, &settings, &o.done()).await.map_err(send_error_message)
}

pub async fn on_import_complete(
    http: &reqwest::Client,
    def: &NotificationDefinition,
    m: &ImportCompleteMessage<'_>,
    ctx: &Context<'_>,
) -> Result<(), String> {
    let settings = Settings::from_value(&def.settings);
    let fallback_type = m.files.first().map(|f| f.release_type).unwrap_or_default();
    let mut o = base("Download", ctx);
    o.set("series", series_json(m.series, ctx));
    o.set("episodes", episodes_json(m.episodes));
    o.set(
        "episodeFiles",
        Value::Array(m.files.iter().map(|f| file_json(m.series, f)).collect()),
    );
    o.set("release", grabbed_release_json(m.release, fallback_type));
    client_fields(&mut o, m.client, m.download_id);
    o.set("fileCount", json!(m.files.len()));
    o.set("sourcePath", json!(m.source_path));
    o.set("destinationPath", json!(m.destination_path));
    send(http, &settings, &o.done()).await.map_err(send_error_message)
}

pub async fn on_episode_file_delete(
    http: &reqwest::Client,
    def: &NotificationDefinition,
    m: &DeleteMessage<'_>,
    ctx: &Context<'_>,
) -> Result<(), String> {
    let settings = Settings::from_value(&def.settings);
    let mut o = base("EpisodeFileDelete", ctx);
    o.set("series", series_json(m.series, ctx));
    o.set("episodes", episodes_json(m.episodes));
    o.set("episodeFile", file_json(m.series, m.file));
    o.set("deleteReason", json!(delete_reason(m.reason)));
    send(http, &settings, &o.done()).await.map_err(send_error_message)
}

pub async fn on_rename(
    http: &reqwest::Client,
    def: &NotificationDefinition,
    series: &Series,
    renamed: &[EpisodeFileRenamed],
    ctx: &Context<'_>,
) -> Result<(), String> {
    let settings = Settings::from_value(&def.settings);
    let files: Vec<Value> = renamed
        .iter()
        .map(|r| {
            let mut f = file_object(series, &r.file);
            f.set(
                "previousRelativePath",
                json!(crate::mediafiles::relative_path(&series.path, &r.original_path)),
            );
            f.set("previousPath", json!(r.original_path));
            f.done()
        })
        .collect();
    let mut o = base("Rename", ctx);
    o.set("series", series_json(series, ctx));
    o.set("renamedEpisodeFiles", Value::Array(files));
    send(http, &settings, &o.done()).await.map_err(send_error_message)
}

pub async fn on_manual_interaction_required(
    http: &reqwest::Client,
    def: &NotificationDefinition,
    m: &ManualInteractionMessage<'_>,
    ctx: &Context<'_>,
) -> Result<(), String> {
    let settings = Settings::from_value(&def.settings);
    let item = &m.tracked.item;
    let mut info = Object::new();
    if let Some(q) = &m.quality {
        info.set("quality", json!(quality_name(q)));
        info.set("qualityVersion", json!(q.version));
    }
    info.set("title", json!(item.title));
    info.set("size", json!(item.total_size));
    let messages: Vec<Value> = m
        .tracked
        .status_messages
        .iter()
        .map(|s| json!({"title": s.title, "messages": s.messages}))
        .collect();
    let (formats, score) = m
        .remote
        .map(|r| (r.custom_formats.as_slice(), r.custom_format_score))
        .unwrap_or((&[], 0));
    let mut o = base("ManualInteractionRequired", ctx);
    o.opt("series", m.series.map(|s| series_json(s, ctx)));
    o.set("episodes", episodes_json(m.episodes));
    o.set("downloadInfo", info.done());
    client_fields(&mut o, Some(&item.client), Some(&item.download_id));
    o.set("downloadStatus", json!(format!("{:?}", m.tracked.status)));
    o.set("downloadStatusMessages", Value::Array(messages));
    o.set("customFormatInfo", custom_format_info(formats, score));
    o.set("release", grabbed_release_json(m.release, ReleaseType::Unknown));
    send(http, &settings, &o.done()).await.map_err(send_error_message)
}

/// `BuildHealthPayload` / `BuildHealthRestoredPayload`.
pub async fn on_health(
    http: &reqwest::Client,
    def: &NotificationDefinition,
    check: &HealthCheck,
    identity: &Identity,
    restored: bool,
) -> Result<(), String> {
    let settings = Settings::from_value(&def.settings);
    let mut o = Object::new();
    o.set(
        "eventType",
        json!(if restored { "HealthRestored" } else { "Health" }),
    );
    o.set("instanceName", json!(identity.instance_name));
    o.set("level", json!(check.kind.as_str()));
    o.set("message", json!(check.message));
    o.set("type", json!(check.source));
    o.set("wikiUrl", json!(check.wiki_url));
    send(http, &settings, &o.done()).await.map_err(send_error_message)
}

/// `Webhook.Test`: the fixed test payload; a failed call names the URL.
pub async fn test(http: &reqwest::Client, def: &NotificationDefinition, ctx: &Context<'_>) -> TestOutcome {
    let settings = Settings::from_value(&def.settings);
    let mut o = base("Test", ctx);
    o.set(
        "series",
        json!({
            "id": 1,
            "title": "Test Title",
            "path": "C:\\testpath",
            "tvdbId": 1234,
            "tvMazeId": 0,
            "tmdbId": 0,
            "type": "standard",
            "year": 0,
            "tags": ["test-tag"],
        }),
    );
    o.set(
        "episodes",
        json!([{
            "id": 123,
            "episodeNumber": 1,
            "seasonNumber": 1,
            "title": "Test title",
            "seriesId": 0,
            "tvdbId": 0,
        }]),
    );
    match send(http, &settings, &o.done()).await {
        Ok(()) => TestOutcome::Failures(Vec::new()),
        Err(SendError::Status(_, m)) => TestOutcome::Failures(vec![ValidationFailure::error(
            "Url",
            &format!("Unable to send test message: {m}"),
        )]),
        Err(SendError::Transport(m)) => TestOutcome::Error(m),
    }
}
