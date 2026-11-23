//! Discord for movies, after Radarr's `Discord`: one embed per event, with
//! the fields the settings list, posted through the series side's
//! `DiscordProxy`. The settings are the same class; the field numbers are
//! Radarr's, which has a `Tags` field Sonarr's lists lack.

use chrono::{DateTime, Utc};
use serde_json::{json, Map, Value};

use super::{
    quality_name, tag_labels, DownloadMessage, GrabMessage, ManualInteractionMessage, MovieDeleteMessage,
    MovieFileDeleteMessage, MovieNotificationDefinition,
};
use crate::health::{HealthCheck, HealthType};
use crate::mediafiles::format;
use crate::movies::import::events::MovieFileRenamed;
use crate::movies::model::Movie;
use crate::notifications::discord::{
    overview, payload, send, send_error_message, version_fields, Settings, APPLICATION_UPDATE_TITLE,
    COLOR_DANGER, COLOR_STANDARD, COLOR_SUCCESS, COLOR_UPGRADE, COLOR_WARNING,
};
use crate::notifications::{
    bytes_to_string, format_number, timestamp, ApplicationUpdateMessage, Context, Identity, SendError,
    TestOutcome,
};
use crate::validation::ValidationFailure;

const ICON_URL: &str = "https://raw.githubusercontent.com/Radarr/Radarr/develop/Logo/256.png";

/// Radarr's `DiscordGrabFieldType`.
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
    pub const TAGS: i64 = 13;
}

/// Radarr's `DiscordImportFieldType`.
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
    pub const TAGS: i64 = 13;
    pub const CUSTOM_FORMATS: i64 = 14;
    pub const CUSTOM_FORMAT_SCORE: i64 = 15;
}

/// Radarr's `DiscordManualInteractionFieldType`.
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
    pub const TAGS: i64 = 10;
}

/// `Settings.Author`, or `_configFileProvider.InstanceName` without one.
fn author(settings: &Settings, identity: &Identity) -> Value {
    let name = if settings.author.trim().is_empty() {
        identity.instance_name.clone()
    } else {
        settings.author.clone()
    };
    json!({"name": name, "icon_url": ICON_URL})
}

/// An embed under construction: the original serialises it with nulls
/// left out.
struct Embed {
    map: Map<String, Value>,
    fields: Vec<Value>,
}

impl Embed {
    fn new(settings: &Settings, identity: &Identity) -> Embed {
        let mut map = Map::new();
        map.insert("author".into(), author(settings, identity));
        Embed {
            map,
            fields: Vec::new(),
        }
    }

    fn set(&mut self, key: &str, value: Value) {
        self.map.insert(key.into(), value);
    }

    fn timestamp(&mut self, now: DateTime<Utc>) {
        self.set("timestamp", json!(timestamp(now)));
    }

    fn movie_url(&mut self, movie: &Movie) {
        self.set(
            "url",
            json!(format!(
                "https://www.themoviedb.org/movie/{}",
                movie.metadata.tmdb_id
            )),
        );
    }

    /// A field joins the embed when its value is not blank.
    fn field(&mut self, name: &str, value: Option<String>, inline: bool) {
        if let Some(v) = value.filter(|v| !v.trim().is_empty()) {
            self.fields
                .push(json!({"name": name, "value": v, "inline": inline}));
        }
    }

    fn finish(mut self, with_fields: bool) -> Value {
        if with_fields {
            self.map.insert("fields".into(), Value::Array(self.fields));
        }
        Value::Object(self.map)
    }
}

/// `DiscordImage { Url = Images.FirstOrDefault(cover)?.RemoteUrl }`.
fn image(movie: Option<&Movie>, cover_type: &str) -> Value {
    let url = movie.and_then(|m| {
        m.metadata
            .images
            .iter()
            .find(|i| i.cover_type.eq_ignore_ascii_case(cover_type))
            .and_then(|i| i.remote_url.clone())
    });
    match url {
        Some(u) => json!({"url": u}),
        None => json!({}),
    }
}

/// `GetTitle`: the title with its year, backticks escaped, cut at 256
/// characters.
pub fn title(movie: &Movie) -> String {
    let md = &movie.metadata;
    let title = if md.year > 0 {
        format!("{} ({})", md.title, md.year)
    } else {
        md.title.clone()
    }
    .replace('`', "\\`");
    if title.chars().count() > 256 {
        let cut: String = title.chars().take(253).collect();
        format!("{}...", cut.trim_end_matches('\\'))
    } else {
        title
    }
}

/// `GetLinksString`.
pub fn links(movie: &Movie) -> String {
    let md = &movie.metadata;
    let filled = |v: &Option<String>| v.clone().filter(|s| !s.trim().is_empty());
    let mut links = vec![format!("[TMDb](https://themoviedb.org/movie/{})", md.tmdb_id)];
    if let Some(imdb) = filled(&md.imdb_id) {
        links.push(format!("[Trakt](https://trakt.tv/movies/{imdb})"));
        links.push(format!("[IMDb](https://imdb.com/title/{imdb}/)"));
    }
    if let Some(trailer) = filled(&md.youtube_trailer_id) {
        links.push(format!("[YouTube](https://www.youtube.com/watch?v={trailer})"));
    }
    if let Some(website) = filled(&md.website) {
        links.push(format!("[Website]({website})"));
    }
    links.join(" / ")
}

/// `Ratings.Tmdb?.Value.ToString()`: the number as the metadata holds it.
fn rating(movie: &Movie) -> String {
    match &movie.metadata.ratings["tmdb"]["value"] {
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

fn genres(movie: &Movie) -> String {
    movie
        .metadata
        .genres
        .iter()
        .take(5)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ")
}

/// `GetTagLabels(movie)?.Join(", ")`: the first five labels.
fn tags(movie: &Movie, ctx: &Context<'_>) -> String {
    tag_labels(ctx.tags, movie)
        .into_iter()
        .take(5)
        .collect::<Vec<_>>()
        .join(", ")
}

async fn post(http: &reqwest::Client, settings: &Settings, payload: &Value) -> Result<(), String> {
    send(http, settings, payload).await.map_err(send_error_message)
}

/// `OnGrab`.
pub async fn on_grab(
    http: &reqwest::Client,
    def: &MovieNotificationDefinition,
    m: &GrabMessage<'_>,
    ctx: &Context<'_>,
) -> Result<(), String> {
    let settings = Settings::from_value(&def.settings);
    post(http, &settings, &grab_payload(&settings, m, ctx)).await
}

pub(crate) fn grab_payload(settings: &Settings, m: &GrabMessage<'_>, ctx: &Context<'_>) -> Value {
    let movie = m.movie;
    let mut embed = Embed::new(settings, ctx.identity);
    embed.movie_url(movie);
    embed.set("description", json!("Movie Grabbed"));
    embed.set("title", json!(title(movie)));
    embed.set("color", json!(COLOR_STANDARD));
    embed.timestamp(ctx.now);
    if settings.grab_fields.contains(&grab_field::POSTER) {
        embed.set("thumbnail", image(Some(movie), "poster"));
    }
    if settings.grab_fields.contains(&grab_field::FANART) {
        embed.set("image", image(Some(movie), "fanart"));
    }
    let release = &m.remote.release;
    for field in &settings.grab_fields {
        match *field {
            grab_field::OVERVIEW => embed.field(
                "Overview",
                Some(overview(movie.metadata.overview.as_deref())),
                false,
            ),
            grab_field::RATING => embed.field("Rating", Some(rating(movie)), false),
            grab_field::GENRES => embed.field("Genres", Some(genres(movie)), false),
            grab_field::QUALITY => embed.field("Quality", Some(quality_name(&m.quality).to_string()), true),
            grab_field::GROUP => embed.field(
                "Group",
                m.remote.parsed.as_ref().and_then(|p| p.release_group.clone()),
                false,
            ),
            grab_field::SIZE => embed.field("Size", Some(bytes_to_string(release.size)), true),
            grab_field::RELEASE => embed.field("Release", Some(format!("```{}```", release.title)), false),
            grab_field::LINKS => embed.field("Links", Some(links(movie)), false),
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
            grab_field::INDEXER => embed.field("Indexer", Some(release.indexer.clone()), false),
            grab_field::TAGS => embed.field("Tags", Some(tags(movie, ctx)), false),
            _ => {}
        }
    }
    payload(settings, None, Some(vec![embed.finish(true)]))
}

/// `OnDownload`.
pub async fn on_download(
    http: &reqwest::Client,
    def: &MovieNotificationDefinition,
    m: &DownloadMessage<'_>,
    ctx: &Context<'_>,
) -> Result<(), String> {
    let settings = Settings::from_value(&def.settings);
    post(http, &settings, &download_payload(&settings, m, ctx)).await
}

pub(crate) fn download_payload(settings: &Settings, m: &DownloadMessage<'_>, ctx: &Context<'_>) -> Value {
    let movie = m.movie;
    let is_upgrade = !m.old_files.is_empty();
    let mut embed = Embed::new(settings, ctx.identity);
    embed.movie_url(movie);
    embed.set(
        "description",
        json!(if is_upgrade {
            "Movie Upgraded"
        } else {
            "Movie Imported"
        }),
    );
    embed.set("title", json!(title(movie)));
    embed.set(
        "color",
        json!(if is_upgrade { COLOR_UPGRADE } else { COLOR_SUCCESS }),
    );
    embed.timestamp(ctx.now);
    if settings.import_fields.contains(&import_field::POSTER) {
        embed.set("thumbnail", image(Some(movie), "poster"));
    }
    if settings.import_fields.contains(&import_field::FANART) {
        embed.set("image", image(Some(movie), "fanart"));
    }
    let media = m.file.media_info.as_ref();
    for field in &settings.import_fields {
        match *field {
            import_field::OVERVIEW => embed.field(
                "Overview",
                Some(overview(movie.metadata.overview.as_deref())),
                false,
            ),
            import_field::RATING => embed.field("Rating", Some(rating(movie)), false),
            import_field::GENRES => embed.field("Genres", Some(genres(movie)), false),
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
                let audio =
                    media.and_then(|raw| format::primary_audio(raw).codec_for(crate::Personality::Radarr));
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
                media.map(|raw| format::audio_languages(raw).join("/")),
                false,
            ),
            import_field::SUBTITLES => embed.field(
                "Subtitles",
                media.map(|raw| format::subtitle_languages(raw).join("/")),
                false,
            ),
            import_field::RELEASE => embed.field(
                "Release",
                Some(format!("```{}```", m.file.scene_name.clone().unwrap_or_default())),
                false,
            ),
            import_field::LINKS => embed.field("Links", Some(links(movie)), false),
            import_field::TAGS => embed.field("Tags", Some(tags(movie, ctx)), false),
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
    payload(settings, None, Some(vec![embed.finish(true)]))
}

/// `OnMovieRename`: one embed per renamed file, with neither author nor
/// timestamp; an embed's colour is an integer and serialises as 0.
pub async fn on_rename(
    http: &reqwest::Client,
    def: &MovieNotificationDefinition,
    movie: &Movie,
    renamed: &[MovieFileRenamed],
) -> Result<(), String> {
    let settings = Settings::from_value(&def.settings);
    post(http, &settings, &rename_payload(&settings, movie, renamed)).await
}

pub(crate) fn rename_payload(settings: &Settings, movie: &Movie, renamed: &[MovieFileRenamed]) -> Value {
    let embeds: Vec<Value> = renamed
        .iter()
        .map(|r| {
            json!({
                "title": movie.metadata.title,
                "description": format!(
                    "{} renamed to {}",
                    crate::mediafiles::relative_path(&r.movie.path, &r.original_path),
                    r.file.relative_path
                ),
                "color": 0,
            })
        })
        .collect();
    payload(settings, Some("Renamed"), Some(embeds))
}

/// The embed `OnMovieAdded` and `OnMovieDelete` share: the movie's title
/// as it is (no year, no escaping), its links as the one field, and the
/// poster and fanart when the import fields ask for them; no timestamp.
fn library_embed(
    settings: &Settings,
    identity: &Identity,
    movie: &Movie,
    description: &str,
    color: i64,
) -> Value {
    let mut embed = Embed::new(settings, identity);
    embed.movie_url(movie);
    embed.set("title", json!(movie.metadata.title));
    embed.set("description", json!(description));
    embed.set("color", json!(color));
    // Added as it is: the original sends it even blank.
    embed
        .fields
        .push(json!({"name": "Links", "value": links(movie), "inline": false}));
    if settings.import_fields.contains(&import_field::POSTER) {
        embed.set("thumbnail", image(Some(movie), "poster"));
    }
    if settings.import_fields.contains(&import_field::FANART) {
        embed.set("image", image(Some(movie), "fanart"));
    }
    embed.finish(true)
}

/// `OnMovieAdded`.
pub async fn on_movie_added(
    http: &reqwest::Client,
    def: &MovieNotificationDefinition,
    movie: &Movie,
    ctx: &Context<'_>,
) -> Result<(), String> {
    let settings = Settings::from_value(&def.settings);
    post(http, &settings, &movie_added_payload(&settings, movie, ctx)).await
}

pub(crate) fn movie_added_payload(settings: &Settings, movie: &Movie, ctx: &Context<'_>) -> Value {
    let embed = library_embed(settings, ctx.identity, movie, "Movie Added", COLOR_SUCCESS);
    payload(settings, None, Some(vec![embed]))
}

/// `OnMovieDelete`.
pub async fn on_movie_delete(
    http: &reqwest::Client,
    def: &MovieNotificationDefinition,
    m: &MovieDeleteMessage<'_>,
    ctx: &Context<'_>,
) -> Result<(), String> {
    let settings = Settings::from_value(&def.settings);
    post(http, &settings, &movie_delete_payload(&settings, m, ctx)).await
}

pub(crate) fn movie_delete_payload(
    settings: &Settings,
    m: &MovieDeleteMessage<'_>,
    ctx: &Context<'_>,
) -> Value {
    let embed = library_embed(
        settings,
        ctx.identity,
        m.movie,
        m.deleted_files_message(),
        COLOR_DANGER,
    );
    payload(settings, None, Some(vec![embed]))
}

/// `OnMovieFileDelete`.
pub async fn on_movie_file_delete(
    http: &reqwest::Client,
    def: &MovieNotificationDefinition,
    m: &MovieFileDeleteMessage<'_>,
    ctx: &Context<'_>,
) -> Result<(), String> {
    let settings = Settings::from_value(&def.settings);
    post(http, &settings, &file_delete_payload(&settings, m, ctx)).await
}

pub(crate) fn file_delete_payload(
    settings: &Settings,
    m: &MovieFileDeleteMessage<'_>,
    ctx: &Context<'_>,
) -> Value {
    let mut embed = Embed::new(settings, ctx.identity);
    embed.movie_url(m.movie);
    embed.set("title", json!(title(m.movie)));
    embed.set("description", json!("Movie File Deleted"));
    embed.set("color", json!(COLOR_DANGER));
    embed.timestamp(ctx.now);
    // These two are added as they are: a blank one would still be sent.
    embed.fields.push(json!({
        "name": "Reason", "value": m.reason.as_str(), "inline": false,
    }));
    embed.fields.push(json!({
        "name": "File name",
        "value": format!("```{}```", crate::mediafiles::join(&m.movie.path, &m.file.relative_path)),
        "inline": false,
    }));
    payload(settings, None, Some(vec![embed.finish(true)]))
}

/// `OnManualInteractionRequired`.
pub async fn on_manual_interaction_required(
    http: &reqwest::Client,
    def: &MovieNotificationDefinition,
    m: &ManualInteractionMessage<'_>,
    ctx: &Context<'_>,
) -> Result<(), String> {
    let settings = Settings::from_value(&def.settings);
    post(http, &settings, &manual_interaction_payload(&settings, m, ctx)).await
}

pub(crate) fn manual_interaction_payload(
    settings: &Settings,
    m: &ManualInteractionMessage<'_>,
    ctx: &Context<'_>,
) -> Value {
    let movie = m.movie;
    let mut embed = Embed::new(settings, ctx.identity);
    if let Some(known) = movie.filter(|k| k.metadata.tmdb_id > 0) {
        embed.movie_url(known);
    }
    embed.set("description", json!("Manual interaction needed"));
    if let Some(known) = movie {
        embed.set("title", json!(title(known)));
    }
    embed.set("color", json!(COLOR_STANDARD));
    embed.timestamp(ctx.now);
    if settings.manual_interaction_fields.contains(&manual_field::POSTER) {
        embed.set("thumbnail", image(movie, "poster"));
    }
    if settings.manual_interaction_fields.contains(&manual_field::FANART) {
        embed.set("image", image(movie, "fanart"));
    }
    let item = &m.tracked.item;
    for field in &settings.manual_interaction_fields {
        match *field {
            manual_field::OVERVIEW => embed.field(
                "Overview",
                Some(overview(movie.and_then(|k| k.metadata.overview.as_deref()))),
                false,
            ),
            manual_field::RATING => embed.field("Rating", movie.map(rating), false),
            manual_field::GENRES => embed.field("Genres", movie.map(genres), false),
            // `message.Quality?.Quality?.Name`: none without a release.
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
            manual_field::SIZE => embed.field("Size", Some(bytes_to_string(item.total_size)), true),
            manual_field::DOWNLOAD_TITLE => {
                embed.field("Download", Some(format!("```{}```", item.title)), false)
            }
            manual_field::LINKS => embed.field("Links", movie.map(links), false),
            manual_field::TAGS => embed.field("Tags", movie.map(|k| tags(k, ctx)), false),
            _ => {}
        }
    }
    payload(settings, None, Some(vec![embed.finish(true)]))
}

/// `OnApplicationUpdate`: the series side's embed, signed as Radarr signs.
pub async fn on_application_update(
    http: &reqwest::Client,
    def: &MovieNotificationDefinition,
    update: &ApplicationUpdateMessage,
    identity: &Identity,
    now: DateTime<Utc>,
) -> Result<(), String> {
    let settings = Settings::from_value(&def.settings);
    post(
        http,
        &settings,
        &application_update_payload(&settings, update, identity, now),
    )
    .await
}

pub(crate) fn application_update_payload(
    settings: &Settings,
    update: &ApplicationUpdateMessage,
    identity: &Identity,
    now: DateTime<Utc>,
) -> Value {
    let mut embed = Embed::new(settings, identity);
    embed.set("title", json!(APPLICATION_UPDATE_TITLE));
    embed.timestamp(now);
    embed.set("color", json!(COLOR_STANDARD));
    embed.fields.extend(version_fields(update));
    payload(settings, None, Some(vec![embed.finish(true)]))
}

/// `OnHealthIssue`.
pub async fn on_health_issue(
    http: &reqwest::Client,
    def: &MovieNotificationDefinition,
    check: &HealthCheck,
    identity: &Identity,
    now: DateTime<Utc>,
) -> Result<(), String> {
    let settings = Settings::from_value(&def.settings);
    let mut embed = Embed::new(&settings, identity);
    embed.set("title", json!(check.source));
    embed.set("description", json!(check.message));
    embed.timestamp(now);
    embed.set(
        "color",
        json!(if check.kind == HealthType::Warning {
            COLOR_WARNING
        } else {
            COLOR_DANGER
        }),
    );
    let payload = payload(&settings, None, Some(vec![embed.finish(false)]));
    post(http, &settings, &payload).await
}

/// `OnHealthRestored`.
pub async fn on_health_restored(
    http: &reqwest::Client,
    def: &MovieNotificationDefinition,
    previous: &HealthCheck,
    identity: &Identity,
    now: DateTime<Utc>,
) -> Result<(), String> {
    let settings = Settings::from_value(&def.settings);
    let mut embed = Embed::new(&settings, identity);
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
    embed.timestamp(now);
    embed.set("color", json!(COLOR_SUCCESS));
    let payload = payload(&settings, None, Some(vec![embed.finish(false)]));
    post(http, &settings, &payload).await
}

/// `Discord.Test`: `Test message from Radarr posted at {DateTime.Now}`,
/// the local time in the invariant culture's `MM/dd/yyyy HH:mm:ss`.
pub async fn test(
    http: &reqwest::Client,
    def: &MovieNotificationDefinition,
    now: DateTime<Utc>,
) -> TestOutcome {
    let settings = Settings::from_value(&def.settings);
    let local = now.with_timezone(&chrono::Local);
    let message = format!(
        "Test message from Radarr posted at {}",
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
