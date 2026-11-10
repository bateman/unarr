//! Discord, after the originals' `Discord`, `DiscordSettings` and
//! `DiscordProxy`: one embed per event, with the fields the settings list,
//! posted to the channel's webhook URL.

use chrono::{DateTime, Utc};
use serde_json::{json, Map, Value};

use super::{
    bytes_to_string, format_number, quality_name, send_json, timestamp, title, Context, DeleteMessage,
    DownloadMessage, GrabMessage, Identity, ImportCompleteMessage, ManualInteractionMessage,
    NotificationDefinition, SendError, TestOutcome,
};
use crate::health::{HealthCheck, HealthType};
use crate::mediafiles::format;
use crate::model::Series;
use crate::validation::ValidationFailure;

const ICON_URL: &str = "https://raw.githubusercontent.com/Sonarr/Sonarr/develop/Logo/256.png";

/// `DiscordColors`.
const COLOR_DANGER: i64 = 15749200;
const COLOR_SUCCESS: i64 = 2605644;
const COLOR_WARNING: i64 = 16753920;
const COLOR_STANDARD: i64 = 16761392;
const COLOR_UPGRADE: i64 = 4089856;

/// `DiscordGrabFieldType`.
mod grab_field {
    pub const OVERVIEW: i64 = 0;
    pub const RATING: i64 = 1;
    pub const GENRES: i64 = 2;
    pub const QUALITY: i64 = 3;
    pub const GROUP: i64 = 4;
    pub const SIZE: i64 = 5;
    pub const LINKS: i64 = 6;
    pub const RELEASE: i64 = 7;
    pub const POSTER: i64 = 8;
    pub const FANART: i64 = 9;
    pub const INDEXER: i64 = 10;
    pub const CUSTOM_FORMATS: i64 = 11;
    pub const CUSTOM_FORMAT_SCORE: i64 = 12;
}

/// `DiscordImportFieldType`.
mod import_field {
    pub const OVERVIEW: i64 = 0;
    pub const RATING: i64 = 1;
    pub const GENRES: i64 = 2;
    pub const QUALITY: i64 = 3;
    pub const CODECS: i64 = 4;
    pub const GROUP: i64 = 5;
    pub const SIZE: i64 = 6;
    pub const LANGUAGES: i64 = 7;
    pub const SUBTITLES: i64 = 8;
    pub const LINKS: i64 = 9;
    pub const RELEASE: i64 = 10;
    pub const POSTER: i64 = 11;
    pub const FANART: i64 = 12;
    pub const CUSTOM_FORMATS: i64 = 13;
    pub const CUSTOM_FORMAT_SCORE: i64 = 14;
}

/// `DiscordManualInteractionFieldType`.
mod manual_field {
    pub const OVERVIEW: i64 = 0;
    pub const RATING: i64 = 1;
    pub const GENRES: i64 = 2;
    pub const QUALITY: i64 = 3;
    pub const GROUP: i64 = 4;
    pub const SIZE: i64 = 5;
    pub const LINKS: i64 = 6;
    pub const DOWNLOAD_TITLE: i64 = 7;
    pub const POSTER: i64 = 8;
    pub const FANART: i64 = 9;
}

/// `DiscordSettings`.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub web_hook_url: String,
    pub username: String,
    pub avatar: String,
    pub author: String,
    pub grab_fields: Vec<i64>,
    pub import_fields: Vec<i64>,
    pub manual_interaction_fields: Vec<i64>,
}

fn text(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or("").to_string()
}

fn ints(v: &Value, key: &str) -> Vec<i64> {
    v[key]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default()
}

impl Settings {
    pub fn from_value(v: &Value) -> Settings {
        Settings {
            web_hook_url: text(v, "webHookUrl"),
            username: text(v, "username"),
            avatar: text(v, "avatar"),
            author: text(v, "author"),
            grab_fields: ints(v, "grabFields"),
            import_fields: ints(v, "importFields"),
            manual_interaction_fields: ints(v, "manualInteractionFields"),
        }
    }

    /// `DiscordSettingsValidator`: the webhook URL must be valid.
    pub fn validate(&self) -> Vec<ValidationFailure> {
        super::valid_url("WebHookUrl", &self.web_hook_url)
            .into_iter()
            .collect()
    }
}

/// An embed under construction, in the originals' field order.
struct Embed {
    map: Map<String, Value>,
    fields: Vec<Value>,
}

impl Embed {
    fn new(settings: &Settings, identity: &Identity, now: DateTime<Utc>) -> Embed {
        let author = if settings.author.trim().is_empty() {
            identity.machine_name.clone()
        } else {
            settings.author.clone()
        };
        let mut map = Map::new();
        map.insert("author".into(), json!({"name": author, "icon_url": ICON_URL}));
        map.insert("timestamp".into(), json!(timestamp(now)));
        Embed {
            map,
            fields: Vec::new(),
        }
    }

    fn set(&mut self, key: &str, value: Value) {
        self.map.insert(key.into(), value);
    }

    fn series_url(&mut self, series: &Series) {
        self.set(
            "url",
            json!(format!("http://thetvdb.com/?tab=series&id={}", series.tvdb_id)),
        );
    }

    /// A field joins the embed when its name and value are not blank.
    fn field(&mut self, name: &str, value: Option<String>, inline: bool) {
        if let Some(v) = value.filter(|v| !v.trim().is_empty()) {
            self.fields
                .push(json!({"name": name, "value": v, "inline": inline}));
        }
    }

    fn thumbnail(&mut self, series: Option<&Series>) {
        self.set("thumbnail", image(series, "poster"));
    }

    fn image(&mut self, series: Option<&Series>) {
        self.set("image", image(series, "fanart"));
    }

    fn finish(mut self, with_fields: bool) -> Value {
        if with_fields {
            self.map.insert("fields".into(), Value::Array(self.fields));
        }
        Value::Object(self.map)
    }
}

/// `DiscordImage { Url = images.FirstOrDefault(cover)?.RemoteUrl }`: the
/// originals serialise it with a null URL left out.
fn image(series: Option<&Series>, cover_type: &str) -> Value {
    let url = series.and_then(|s| {
        s.images
            .iter()
            .find(|i| i.cover_type == cover_type)
            .and_then(|i| i.remote_url.clone())
    });
    match url {
        Some(u) => json!({"url": u}),
        None => json!({}),
    }
}

/// `Overview`, cut at 300 characters.
fn overview(text: Option<&str>) -> String {
    let text = text.unwrap_or("");
    if text.chars().count() <= 300 {
        text.to_string()
    } else {
        format!("{}...", text.chars().take(300).collect::<String>())
    }
}

/// `GetLinksString`.
fn links(series: &Series) -> String {
    let mut links = vec![
        format!(
            "[The TVDB](https://thetvdb.com/?tab=series&id={})",
            series.tvdb_id
        ),
        format!(
            "[Trakt](https://trakt.tv/search/tvdb/{}?id_type=show)",
            series.tvdb_id
        ),
    ];
    if let Some(imdb) = series.imdb_id.as_deref().filter(|s| !s.trim().is_empty()) {
        links.push(format!("[IMDB](https://imdb.com/title/{imdb}/)"));
    }
    links.join(" / ")
}

fn rating(series: &Series) -> String {
    format_number(series.ratings.value)
}

fn genres(series: &Series) -> String {
    series
        .genres
        .iter()
        .take(5)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ")
}

/// `CreatePayload`.
fn payload(settings: &Settings, content: Option<&str>, embeds: Option<Vec<Value>>) -> Value {
    let mut m = Map::new();
    if let Some(c) = content {
        m.insert("content".into(), json!(c));
    }
    if !settings.username.trim().is_empty() {
        m.insert("username".into(), json!(settings.username));
    }
    if !settings.avatar.trim().is_empty() {
        m.insert("avatar_url".into(), json!(settings.avatar));
    }
    if let Some(e) = embeds {
        m.insert("embeds".into(), Value::Array(e));
    }
    Value::Object(m)
}

/// `DiscordProxy.SendPayload`: a failed HTTP call is a
/// `DiscordException("Unable to post payload")`; a call that never got an
/// answer escapes as it is.
async fn send(http: &reqwest::Client, settings: &Settings, payload: &Value) -> Result<(), SendError> {
    let request = http
        .post(&settings.web_hook_url)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(payload.to_string());
    match send_json(http, request).await {
        Ok(_) => Ok(()),
        Err(SendError::Status(code, message)) => {
            tracing::error!(status = code, "unable to post payload {payload}");
            Err(SendError::Status(
                code,
                format!("Unable to post payload: {message}"),
            ))
        }
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
    let series = m.series;
    let mut embed = Embed::new(&settings, ctx.identity, ctx.now);
    embed.series_url(series);
    embed.set("description", json!("Episode Grabbed"));
    embed.set("title", json!(title(series, m.episodes)));
    embed.set("color", json!(COLOR_STANDARD));
    if settings.grab_fields.contains(&grab_field::POSTER) {
        embed.thumbnail(Some(series));
    }
    if settings.grab_fields.contains(&grab_field::FANART) {
        embed.image(Some(series));
    }
    let parsed = m.remote.parsed.as_ref();
    for field in &settings.grab_fields {
        match *field {
            grab_field::OVERVIEW => embed.field(
                "Overview",
                Some(overview(m.episodes.first().and_then(|e| e.overview.as_deref()))),
                false,
            ),
            grab_field::RATING => embed.field("Rating", Some(rating(series)), false),
            grab_field::GENRES => embed.field("Genres", Some(genres(series)), false),
            grab_field::QUALITY => embed.field("Quality", Some(quality_name(&m.quality).to_string()), true),
            grab_field::GROUP => embed.field("Group", parsed.and_then(|p| p.release_group.clone()), false),
            grab_field::SIZE => embed.field("Size", Some(bytes_to_string(m.remote.release.size)), true),
            grab_field::RELEASE => embed.field(
                "Release",
                Some(format!("```{}```", m.remote.release.title)),
                false,
            ),
            grab_field::LINKS => embed.field("Links", Some(links(series)), false),
            grab_field::CUSTOM_FORMATS => embed.field(
                "Custom Formats",
                Some(
                    m.remote
                        .custom_formats
                        .iter()
                        .map(|f| f.name.clone())
                        .collect::<Vec<_>>()
                        .join("|"),
                ),
                false,
            ),
            grab_field::CUSTOM_FORMAT_SCORE => embed.field(
                "Custom Format Score",
                Some(m.remote.custom_format_score.to_string()),
                false,
            ),
            grab_field::INDEXER => embed.field("Indexer", Some(m.remote.release.indexer.clone()), false),
            _ => {}
        }
    }
    let payload = payload(&settings, None, Some(vec![embed.finish(true)]));
    send(http, &settings, &payload).await.map_err(send_error_message)
}

pub async fn on_download(
    http: &reqwest::Client,
    def: &NotificationDefinition,
    m: &DownloadMessage<'_>,
    ctx: &Context<'_>,
) -> Result<(), String> {
    let settings = Settings::from_value(&def.settings);
    let series = m.series;
    let is_upgrade = !m.old_files.is_empty();
    let mut embed = Embed::new(&settings, ctx.identity, ctx.now);
    embed.series_url(series);
    embed.set(
        "description",
        json!(if is_upgrade {
            "Episode Upgraded"
        } else {
            "Episode Imported"
        }),
    );
    embed.set("title", json!(title(series, m.episodes)));
    embed.set(
        "color",
        json!(if is_upgrade { COLOR_UPGRADE } else { COLOR_SUCCESS }),
    );
    if settings.import_fields.contains(&import_field::POSTER) {
        embed.thumbnail(Some(series));
    }
    if settings.import_fields.contains(&import_field::FANART) {
        embed.image(Some(series));
    }
    let media = m.file.media_info.as_ref();
    for field in &settings.import_fields {
        match *field {
            import_field::OVERVIEW => embed.field(
                "Overview",
                Some(overview(m.episodes.first().and_then(|e| e.overview.as_deref()))),
                false,
            ),
            import_field::RATING => embed.field("Rating", Some(rating(series)), false),
            import_field::GENRES => embed.field("Genres", Some(genres(series)), false),
            import_field::QUALITY => {
                embed.field("Quality", Some(quality_name(&m.file.quality).to_string()), true)
            }
            import_field::CODECS => {
                // `FormatVideoCodec(mediaInfo, null)`: no scene name here.
                let video = media.and_then(|raw| {
                    format::video_codec(
                        format::str(raw, "videoFormat"),
                        format::str(raw, "videoCodecID").unwrap_or(""),
                        None,
                    )
                });
                let audio = media.and_then(|raw| {
                    format::audio_codec(
                        format::str(raw, "audioFormat"),
                        format::str(raw, "audioCodecID").unwrap_or(""),
                        format::str(raw, "audioProfile").unwrap_or(""),
                    )
                });
                let channels = media.map(format::audio_channels).unwrap_or(0.0);
                embed.field(
                    "Codecs",
                    Some(format!(
                        "{} / {} {}",
                        video.unwrap_or_default(),
                        audio.unwrap_or_default(),
                        format_number(channels)
                    )),
                    true,
                )
            }
            import_field::GROUP => embed.field("Group", m.file.release_group.clone(), false),
            import_field::SIZE => embed.field("Size", Some(bytes_to_string(m.file.size)), true),
            import_field::LANGUAGES => embed.field(
                "Languages",
                media.map(|raw| format::list(raw, "audioLanguages").join("/")),
                false,
            ),
            import_field::SUBTITLES => embed.field(
                "Subtitles",
                media.map(|raw| format::list(raw, "subtitles").join("/")),
                false,
            ),
            import_field::RELEASE => embed.field(
                "Release",
                Some(format!("```{}```", m.file.scene_name.clone().unwrap_or_default())),
                false,
            ),
            import_field::LINKS => embed.field("Links", Some(links(series)), false),
            import_field::CUSTOM_FORMATS => embed.field(
                "Custom Formats",
                Some(
                    m.custom_formats
                        .iter()
                        .map(|f| f.name.clone())
                        .collect::<Vec<_>>()
                        .join("|"),
                ),
                false,
            ),
            import_field::CUSTOM_FORMAT_SCORE => embed.field(
                "Custom Format Score",
                Some(m.custom_format_score.to_string()),
                false,
            ),
            _ => {}
        }
    }
    let payload = payload(&settings, None, Some(vec![embed.finish(true)]));
    send(http, &settings, &payload).await.map_err(send_error_message)
}

pub async fn on_import_complete(
    http: &reqwest::Client,
    def: &NotificationDefinition,
    m: &ImportCompleteMessage<'_>,
    ctx: &Context<'_>,
) -> Result<(), String> {
    let settings = Settings::from_value(&def.settings);
    let series = m.series;
    let mut embed = Embed::new(&settings, ctx.identity, ctx.now);
    embed.series_url(series);
    embed.set("description", json!("Import Complete"));
    embed.set("title", json!(title(series, m.episodes)));
    embed.set("color", json!(COLOR_SUCCESS));
    if settings.import_fields.contains(&import_field::POSTER) {
        embed.thumbnail(Some(series));
    }
    if settings.import_fields.contains(&import_field::FANART) {
        embed.image(Some(series));
    }
    for field in &settings.import_fields {
        match *field {
            import_field::OVERVIEW => embed.field(
                "Overview",
                Some(overview(m.episodes.first().and_then(|e| e.overview.as_deref()))),
                false,
            ),
            import_field::RATING => embed.field("Rating", Some(rating(series)), false),
            import_field::GENRES => embed.field("Genres", Some(genres(series)), false),
            import_field::QUALITY => embed.field(
                "Quality",
                Some(quality_name(&m.release_quality).to_string()),
                true,
            ),
            import_field::GROUP => embed.field("Group", m.release_group.map(str::to_string), false),
            import_field::SIZE => {
                let size = m
                    .release
                    .map(|r| r.size)
                    .unwrap_or_else(|| m.files.iter().map(|f| f.size).sum());
                embed.field("Size", Some(bytes_to_string(size)), true)
            }
            import_field::RELEASE => {
                let name = m
                    .release
                    .map(|r| r.title.clone())
                    .or_else(|| m.source_title.map(str::to_string))
                    .unwrap_or_default();
                embed.field("Release", Some(format!("```{name}```")), false)
            }
            import_field::LINKS => embed.field("Links", Some(links(series)), false),
            _ => {}
        }
    }
    let payload = payload(&settings, None, Some(vec![embed.finish(true)]));
    send(http, &settings, &payload).await.map_err(send_error_message)
}

/// `OnSeriesAdd` and `OnSeriesDelete`: an embed with no timestamp, the
/// links, and the poster and fanart when the import fields ask for them.
fn series_embed(
    settings: &Settings,
    identity: &Identity,
    series: &Series,
    description: &str,
    color: i64,
) -> Value {
    let author = if settings.author.trim().is_empty() {
        identity.machine_name.clone()
    } else {
        settings.author.clone()
    };
    let mut embed = json!({
        "author": {"name": author, "icon_url": ICON_URL},
        "url": format!("http://thetvdb.com/?tab=series&id={}", series.tvdb_id),
        "title": series.title,
        "description": description,
        "color": color,
        "fields": [{"name": "Links", "value": links(series), "inline": false}],
    });
    if settings.import_fields.contains(&import_field::POSTER) {
        embed["thumbnail"] = image(Some(series), "poster");
    }
    if settings.import_fields.contains(&import_field::FANART) {
        embed["image"] = image(Some(series), "fanart");
    }
    embed
}

pub async fn on_series_add(
    http: &reqwest::Client,
    def: &NotificationDefinition,
    series: &Series,
    ctx: &Context<'_>,
) -> Result<(), String> {
    let settings = Settings::from_value(&def.settings);
    let embed = series_embed(&settings, ctx.identity, series, "Series Added", COLOR_SUCCESS);
    let payload = payload(&settings, None, Some(vec![embed]));
    send(http, &settings, &payload).await.map_err(send_error_message)
}

pub async fn on_series_delete(
    http: &reqwest::Client,
    def: &NotificationDefinition,
    series: &Series,
    deleted_files: bool,
    ctx: &Context<'_>,
) -> Result<(), String> {
    let settings = Settings::from_value(&def.settings);
    let description = if deleted_files {
        "Series removed and all files were deleted"
    } else {
        "Series removed, files were not deleted"
    };
    let embed = series_embed(&settings, ctx.identity, series, description, COLOR_DANGER);
    let payload = payload(&settings, None, Some(vec![embed]));
    send(http, &settings, &payload).await.map_err(send_error_message)
}

pub async fn on_rename(
    http: &reqwest::Client,
    def: &NotificationDefinition,
    series: &Series,
) -> Result<(), String> {
    let settings = Settings::from_value(&def.settings);
    let embed = json!({"title": series.title, "color": 0});
    let payload = payload(&settings, Some("Renamed"), Some(vec![embed]));
    send(http, &settings, &payload).await.map_err(send_error_message)
}

pub async fn on_episode_file_delete(
    http: &reqwest::Client,
    def: &NotificationDefinition,
    m: &DeleteMessage<'_>,
    ctx: &Context<'_>,
) -> Result<(), String> {
    let settings = Settings::from_value(&def.settings);
    let series = m.series;
    let mut embed = Embed::new(&settings, ctx.identity, ctx.now);
    embed.series_url(series);
    embed.set("title", json!(title(series, m.episodes)));
    embed.set("description", json!("Episode Deleted"));
    embed.set("color", json!(COLOR_DANGER));
    embed.field("Reason", Some(m.reason.as_str().to_string()), false);
    embed.field(
        "File name",
        Some(format!(
            "```{}```",
            crate::mediafiles::join(&series.path, &m.file.relative_path)
        )),
        false,
    );
    let payload = payload(&settings, None, Some(vec![embed.finish(true)]));
    send(http, &settings, &payload).await.map_err(send_error_message)
}

pub async fn on_manual_interaction_required(
    http: &reqwest::Client,
    def: &NotificationDefinition,
    m: &ManualInteractionMessage<'_>,
    ctx: &Context<'_>,
) -> Result<(), String> {
    let settings = Settings::from_value(&def.settings);
    let series = m.series;
    let mut embed = Embed::new(&settings, ctx.identity, ctx.now);
    if let Some(s) = series.filter(|s| s.tvdb_id > 0) {
        embed.series_url(s);
    }
    embed.set("description", json!("Manual interaction needed"));
    if let Some(s) = series {
        embed.set("title", json!(title(s, m.episodes)));
    }
    embed.set("color", json!(COLOR_STANDARD));
    if settings.manual_interaction_fields.contains(&manual_field::POSTER) {
        embed.thumbnail(series);
    }
    if settings.manual_interaction_fields.contains(&manual_field::FANART) {
        embed.image(series);
    }
    for field in &settings.manual_interaction_fields {
        match *field {
            manual_field::OVERVIEW => embed.field(
                "Overview",
                Some(overview(m.episodes.first().and_then(|e| e.overview.as_deref()))),
                false,
            ),
            manual_field::RATING => embed.field("Rating", series.map(rating), false),
            manual_field::GENRES => embed.field("Genres", series.map(genres), false),
            manual_field::QUALITY => embed.field(
                "Quality",
                m.quality.as_ref().map(|q| quality_name(q).to_string()),
                true,
            ),
            manual_field::GROUP => embed.field(
                "Group",
                m.remote
                    .and_then(|r| r.parsed.as_ref())
                    .and_then(|p| p.release_group.clone()),
                false,
            ),
            manual_field::SIZE => embed.field("Size", Some(bytes_to_string(m.tracked.item.total_size)), true),
            manual_field::DOWNLOAD_TITLE => {
                embed.field("Download", Some(format!("```{}```", m.tracked.item.title)), false)
            }
            manual_field::LINKS => embed.field("Links", series.map(links), false),
            _ => {}
        }
    }
    let payload = payload(&settings, None, Some(vec![embed.finish(true)]));
    send(http, &settings, &payload).await.map_err(send_error_message)
}

pub async fn on_health_issue(
    http: &reqwest::Client,
    def: &NotificationDefinition,
    check: &HealthCheck,
    identity: &Identity,
    now: DateTime<Utc>,
) -> Result<(), String> {
    let settings = Settings::from_value(&def.settings);
    let mut embed = Embed::new(&settings, identity, now);
    embed.set("title", json!(check.source));
    embed.set("description", json!(check.message));
    embed.set(
        "color",
        json!(if check.kind == HealthType::Warning {
            COLOR_WARNING
        } else {
            COLOR_DANGER
        }),
    );
    let payload = payload(&settings, None, Some(vec![embed.finish(false)]));
    send(http, &settings, &payload).await.map_err(send_error_message)
}

pub async fn on_health_restored(
    http: &reqwest::Client,
    def: &NotificationDefinition,
    previous: &HealthCheck,
    identity: &Identity,
    now: DateTime<Utc>,
) -> Result<(), String> {
    let settings = Settings::from_value(&def.settings);
    let mut embed = Embed::new(&settings, identity, now);
    embed.set(
        "title",
        json!(format!("Health Issue Resolved: {}", previous.source)),
    );
    embed.set(
        "description",
        json!(format!(
            "The following issue is now resolved: {}",
            previous.message
        )),
    );
    embed.set("color", json!(COLOR_SUCCESS));
    let payload = payload(&settings, None, Some(vec![embed.finish(false)]));
    send(http, &settings, &payload).await.map_err(send_error_message)
}

/// `Discord.Test`: `Test message from Sonarr posted at {DateTime.Now}`,
/// the local time in the invariant culture's `MM/dd/yyyy HH:mm:ss`.
pub async fn test(
    http: &reqwest::Client,
    def: &NotificationDefinition,
    identity: &Identity,
    now: DateTime<Utc>,
) -> TestOutcome {
    let settings = Settings::from_value(&def.settings);
    let local = now.with_timezone(&chrono::Local);
    let message = format!(
        "Test message from {} posted at {}",
        identity.instance_name.split(' ').next().unwrap_or("Sonarr"),
        local.format("%m/%d/%Y %H:%M:%S")
    );
    let payload = payload(&settings, Some(&message), None);
    match send(http, &settings, &payload).await {
        Ok(()) => TestOutcome::Failures(Vec::new()),
        Err(SendError::Status(_, m)) => TestOutcome::Failures(vec![ValidationFailure::error(
            "",
            &format!("Unable to send test message: {m}"),
        )]),
        Err(SendError::Transport(m)) => TestOutcome::Error(m),
    }
}
