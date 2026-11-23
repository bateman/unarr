//! The generic webhook for movies, after Radarr's `WebhookBase` and its
//! payload classes (`WebhookMovie`, `WebhookRemoteMovie`,
//! `WebhookMovieFile`, `WebhookRelease`, `WebhookGrabbedRelease`...), sent
//! through the series side's `WebhookProxy` with the same settings.

use serde_json::{json, Value};

use super::{
    convert_to_local_urls, quality_name, tag_labels, DownloadMessage, GrabMessage, ManualInteractionMessage,
    MovieDeleteMessage, MovieFileDeleteMessage, MovieNotificationDefinition,
};
use crate::health::HealthCheck;
use crate::mediafiles::format;
use crate::mediafiles::local::DeleteReason;
use crate::movies::import::events::MovieFileRenamed;
use crate::movies::import::local::GrabbedMovieRelease;
use crate::movies::model::{Movie, MovieFile};
use crate::movies::quality::{indexer_flag_names, indexer_flags_text, MovieLanguage};
use crate::notifications::webhook::{send, send_error_message, Object, Settings};
use crate::notifications::{timestamp, ApplicationUpdateMessage, Context, Identity, SendError, TestOutcome};
use crate::validation::ValidationFailure;

fn base(event_type: &str, ctx: &Context<'_>) -> Object {
    let mut o = Object::new();
    o.set("eventType", json!(event_type));
    o.set("instanceName", json!(ctx.identity.instance_name));
    o.set("applicationUrl", json!(ctx.application_url));
    o
}

/// A `Language` as the payloads carry it.
fn language_json(id: i64) -> Value {
    json!({"id": id, "name": MovieLanguage::name_of(id)})
}

fn languages_json(ids: &[i64]) -> Value {
    Value::Array(ids.iter().map(|id| language_json(*id)).collect())
}

/// `MovieMetadata.PhysicalReleaseDate().ToString("yyyy-MM-dd")`: the
/// physical release, else ninety days after the cinemas, else the end of
/// time.
fn release_date(movie: &Movie) -> String {
    let md = &movie.metadata;
    md.physical_release
        .or_else(|| md.in_cinemas.map(|d| d + chrono::Duration::days(90)))
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "9999-12-31".to_string())
}

/// `GetMovie`: `WebhookMovie`, after the covers were given their local
/// addresses (`ConvertToLocalUrls`).
fn movie_json(movie: &Movie, ctx: &Context<'_>) -> Value {
    let md = &movie.metadata;
    let mut images = md.images.clone();
    let root = ctx.identity.media_cover_dir.as_deref();
    convert_to_local_urls(&ctx.identity.url_base, movie.id, &mut images, &|cover_type| {
        super::cover_exists(root, movie.id, cover_type, movie.added, ctx.now)
    });
    let images: Vec<Value> = images
        .iter()
        .map(|i| {
            let mut o = Object::new();
            o.set("coverType", json!(i.cover_type));
            o.opt("url", i.url.as_ref());
            o.opt("remoteUrl", i.remote_url.as_ref());
            o.done()
        })
        .collect();
    let mut o = Object::new();
    o.set("id", json!(movie.id));
    o.set("title", json!(md.title));
    o.set("year", json!(md.year));
    o.set("releaseDate", json!(release_date(movie)));
    o.set("folderPath", json!(movie.path));
    o.set("tmdbId", json!(md.tmdb_id));
    o.opt("imdbId", md.imdb_id.as_ref());
    o.opt("overview", md.overview.as_ref());
    o.set("genres", json!(md.genres));
    o.set("images", Value::Array(images));
    o.set("tags", json!(tag_labels(ctx.tags, movie)));
    o.set("originalLanguage", language_json(md.original_language_id));
    o.done()
}

/// `WebhookRemoteMovie`.
fn remote_movie_json(movie: &Movie) -> Value {
    let md = &movie.metadata;
    let mut o = Object::new();
    o.set("tmdbId", json!(md.tmdb_id));
    o.opt("imdbId", md.imdb_id.as_ref());
    o.set("title", json!(md.title));
    o.set("year", json!(md.year));
    o.done()
}

/// `WebhookMovieFileMediaInfo`.
fn media_info_json(file: &MovieFile) -> Option<Value> {
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
    o.opt("audioCodec", format::primary_audio(raw).codec());
    o.set("audioLanguages", json!(distinct(format::audio_languages(raw))));
    o.set("height", json!(format::int(raw, "height")));
    o.set("width", json!(format::int(raw, "width")));
    o.set("subtitles", json!(distinct(format::subtitle_languages(raw))));
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

/// `WebhookMovieFile`.
fn file_object(movie: &Movie, file: &MovieFile) -> Object {
    let mut o = Object::new();
    o.set("id", json!(file.id));
    o.set("relativePath", json!(file.relative_path));
    o.set(
        "path",
        json!(crate::mediafiles::join(&movie.path, &file.relative_path)),
    );
    o.set("quality", json!(quality_name(&file.quality)));
    o.set("qualityVersion", json!(file.quality.version));
    o.opt("releaseGroup", file.release_group.as_ref());
    o.opt("sceneName", file.scene_name.as_ref());
    o.set("indexerFlags", json!(indexer_flags_text(file.indexer_flags)));
    o.set("size", json!(file.size));
    o.set("dateAdded", json!(timestamp(file.date_added)));
    o.set("languages", languages_json(&file.languages));
    o.opt("mediaInfo", media_info_json(file));
    o
}

/// `WebhookGrabbedRelease(release)`: nothing but the size's default
/// without a release.
fn grabbed_release_json(release: Option<&GrabbedMovieRelease>) -> Value {
    let mut o = Object::new();
    match release {
        Some(r) => {
            o.set("releaseTitle", json!(r.title));
            o.opt("indexer", r.indexer.as_ref());
            o.set("size", json!(r.size));
            o.set("indexerFlags", json!(indexer_flag_names(r.indexer_flags)));
        }
        None => {
            o.set("size", json!(0));
        }
    }
    o.done()
}

/// `WebhookGrabbedRelease(release, indexerFlags)`: the file's flags stand
/// in without a release.
fn grabbed_release_or_flags_json(release: Option<&GrabbedMovieRelease>, file_flags: i64) -> Value {
    match release {
        Some(_) => grabbed_release_json(release),
        None => json!({"size": 0, "indexerFlags": indexer_flag_names(file_flags)}),
    }
}

/// `WebhookCustomFormatInfo`.
fn custom_format_info(formats: &[crate::model::CustomFormat], score: i64) -> Value {
    json!({
        "customFormats": formats.iter().map(|f| json!({"id": f.id, "name": f.name})).collect::<Vec<_>>(),
        "customFormatScore": score,
    })
}

/// `DeleteMediaFileReason` as the original's camel-case enum.
fn delete_reason(reason: DeleteReason) -> &'static str {
    match reason {
        DeleteReason::MissingFromDisk => "missingFromDisk",
        DeleteReason::Manual => "manual",
        DeleteReason::Upgrade => "upgrade",
        DeleteReason::NoLinkedEpisodes => "noLinkedEpisodes",
        DeleteReason::ManualOverride => "manualOverride",
    }
}

async fn post(http: &reqwest::Client, settings: &Settings, payload: &Value) -> Result<(), String> {
    send(http, settings, payload).await.map_err(send_error_message)
}

/// `BuildOnGrabPayload`.
pub(crate) fn grab_payload(m: &GrabMessage<'_>, ctx: &Context<'_>) -> Value {
    let parsed = m.remote.parsed.as_ref();
    let release_info = &m.remote.release;
    let mut release = Object::new();
    release.set("quality", json!(quality_name(&m.quality)));
    release.set("qualityVersion", json!(m.quality.version));
    release.opt("releaseGroup", parsed.and_then(|p| p.release_group.as_ref()));
    release.set("releaseTitle", json!(release_info.title));
    release.set("indexer", json!(release_info.indexer));
    release.set("size", json!(release_info.size));
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
    release.set("languages", languages_json(&m.remote.languages));
    release.set(
        "indexerFlags",
        json!(indexer_flag_names(release_info.indexer_flags)),
    );
    let mut o = base("Grab", ctx);
    o.set("movie", movie_json(m.movie, ctx));
    o.set("remoteMovie", remote_movie_json(m.movie));
    o.set("release", release.done());
    o.set("downloadClient", json!(m.download_client_name));
    o.set("downloadClientType", json!(m.download_client_type));
    o.opt("downloadId", m.download_id);
    o.set(
        "customFormatInfo",
        custom_format_info(&m.remote.custom_formats, m.remote.custom_format_score),
    );
    o.done()
}

pub async fn on_grab(
    http: &reqwest::Client,
    def: &MovieNotificationDefinition,
    m: &GrabMessage<'_>,
    ctx: &Context<'_>,
) -> Result<(), String> {
    let settings = Settings::from_value(&def.settings);
    post(http, &settings, &grab_payload(m, ctx)).await
}

/// `BuildOnDownloadPayload`.
pub(crate) fn download_payload(m: &DownloadMessage<'_>, ctx: &Context<'_>) -> Value {
    let mut file = file_object(m.movie, m.file);
    file.set("sourcePath", json!(m.source_path));
    let mut o = base("Download", ctx);
    o.set("movie", movie_json(m.movie, ctx));
    o.set("remoteMovie", remote_movie_json(m.movie));
    o.set("movieFile", file.done());
    o.set(
        "release",
        grabbed_release_or_flags_json(m.release, m.file.indexer_flags),
    );
    o.set("isUpgrade", json!(!m.old_files.is_empty()));
    o.opt("downloadClient", m.client.map(|c| c.name.clone()));
    o.opt("downloadClientType", m.client.map(|c| c.kind.display_name()));
    o.opt("downloadId", m.download_id);
    o.set(
        "customFormatInfo",
        custom_format_info(m.custom_formats, m.custom_format_score),
    );
    if !m.old_files.is_empty() {
        let deleted: Vec<Value> = m
            .old_files
            .iter()
            .map(|d| {
                let mut f = file_object(m.movie, &d.file);
                f.opt("recycleBinPath", d.recycle_bin_path.as_ref());
                f.done()
            })
            .collect();
        o.set("deletedFiles", Value::Array(deleted));
    }
    o.done()
}

pub async fn on_download(
    http: &reqwest::Client,
    def: &MovieNotificationDefinition,
    m: &DownloadMessage<'_>,
    ctx: &Context<'_>,
) -> Result<(), String> {
    let settings = Settings::from_value(&def.settings);
    post(http, &settings, &download_payload(m, ctx)).await
}

/// `AddMovieMethod` as the original's camel-case enum, whatever the case
/// the client sent it in.
fn add_method(sent: &str) -> String {
    match sent.to_ascii_lowercase().as_str() {
        "manual" => "manual".into(),
        "list" => "list".into(),
        "collection" => "collection".into(),
        _ => sent.to_string(),
    }
}

/// `BuildOnMovieAdded`. `movie.AddOptions.AddMethod`: a movie without
/// options throws, which the handler records as a failure.
pub(crate) fn movie_added_payload(movie: &Movie, ctx: &Context<'_>) -> Result<Value, String> {
    let method = movie
        .add_options
        .as_ref()
        .filter(|o| o.is_object())
        .ok_or_else(|| "Object reference not set to an instance of an object.".to_string())?
        .get("addMethod")
        .and_then(Value::as_str)
        .map(add_method)
        .unwrap_or_else(|| "manual".to_string());
    let mut o = base("MovieAdded", ctx);
    o.set("movie", movie_json(movie, ctx));
    o.set("addMethod", json!(method));
    Ok(o.done())
}

pub async fn on_movie_added(
    http: &reqwest::Client,
    def: &MovieNotificationDefinition,
    movie: &Movie,
    ctx: &Context<'_>,
) -> Result<(), String> {
    let settings = Settings::from_value(&def.settings);
    let payload = movie_added_payload(movie, ctx)?;
    post(http, &settings, &payload).await
}

/// `BuildOnMovieDelete`: the folder's size is the size of the movie's
/// file, when the files went and the movie had one.
pub(crate) fn movie_delete_payload(m: &MovieDeleteMessage<'_>, ctx: &Context<'_>) -> Value {
    let size = if m.deleted_files {
        m.file_size.unwrap_or(0)
    } else {
        0
    };
    let mut o = base("MovieDelete", ctx);
    o.set("movie", movie_json(m.movie, ctx));
    o.set("deletedFiles", json!(m.deleted_files));
    o.set("movieFolderSize", json!(size));
    o.done()
}

pub async fn on_movie_delete(
    http: &reqwest::Client,
    def: &MovieNotificationDefinition,
    m: &MovieDeleteMessage<'_>,
    ctx: &Context<'_>,
) -> Result<(), String> {
    let settings = Settings::from_value(&def.settings);
    post(http, &settings, &movie_delete_payload(m, ctx)).await
}

/// `BuildOnMovieFileDelete`.
pub(crate) fn file_delete_payload(m: &MovieFileDeleteMessage<'_>, ctx: &Context<'_>) -> Value {
    let mut o = base("MovieFileDelete", ctx);
    o.set("movie", movie_json(m.movie, ctx));
    o.set("movieFile", file_object(m.movie, m.file).done());
    o.set("deleteReason", json!(delete_reason(m.reason)));
    o.done()
}

pub async fn on_movie_file_delete(
    http: &reqwest::Client,
    def: &MovieNotificationDefinition,
    m: &MovieFileDeleteMessage<'_>,
    ctx: &Context<'_>,
) -> Result<(), String> {
    let settings = Settings::from_value(&def.settings);
    post(http, &settings, &file_delete_payload(m, ctx)).await
}

/// `BuildOnRenamePayload`.
pub(crate) fn rename_payload(movie: &Movie, renamed: &[MovieFileRenamed], ctx: &Context<'_>) -> Value {
    let files: Vec<Value> = renamed
        .iter()
        .map(|r| {
            let mut f = file_object(movie, &r.file);
            f.set(
                "previousRelativePath",
                json!(crate::mediafiles::relative_path(&r.movie.path, &r.original_path)),
            );
            f.set("previousPath", json!(r.original_path));
            f.done()
        })
        .collect();
    let mut o = base("Rename", ctx);
    o.set("movie", movie_json(movie, ctx));
    o.set("renamedMovieFiles", Value::Array(files));
    o.done()
}

pub async fn on_rename(
    http: &reqwest::Client,
    def: &MovieNotificationDefinition,
    movie: &Movie,
    renamed: &[MovieFileRenamed],
    ctx: &Context<'_>,
) -> Result<(), String> {
    let settings = Settings::from_value(&def.settings);
    post(http, &settings, &rename_payload(movie, renamed, ctx)).await
}

/// `BuildManualInteractionRequiredPayload`.
pub(crate) fn manual_interaction_payload(m: &ManualInteractionMessage<'_>, ctx: &Context<'_>) -> Value {
    let item = &m.tracked.item;
    let mut info = Object::new();
    let quality = m.quality.unwrap_or_default();
    info.set("quality", json!(quality_name(&quality)));
    info.set("qualityVersion", json!(quality.version));
    info.set("title", json!(item.title));
    info.set("size", json!(item.total_size));
    let messages: Vec<Value> = m
        .tracked
        .status_messages
        .iter()
        .map(|s| json!({"title": s.title, "messages": s.messages}))
        .collect();
    let mut o = base("ManualInteractionRequired", ctx);
    o.opt("movie", m.movie.map(|movie| movie_json(movie, ctx)));
    o.set("downloadInfo", info.done());
    o.set("downloadClient", json!(item.client.name));
    o.set("downloadClientType", json!(item.client.kind.display_name()));
    o.set("downloadId", json!(item.download_id));
    o.set("downloadStatus", json!(format!("{:?}", m.tracked.status)));
    o.set("downloadStatusMessages", Value::Array(messages));
    o.set(
        "customFormatInfo",
        // Called with a release only (`on_manual_interaction_required`).
        m.remote
            .map(|r| custom_format_info(&r.custom_formats, r.custom_format_score))
            .unwrap_or(Value::Null),
    );
    o.set("release", grabbed_release_json(m.release));
    o.done()
}

pub async fn on_manual_interaction_required(
    http: &reqwest::Client,
    def: &MovieNotificationDefinition,
    m: &ManualInteractionMessage<'_>,
    ctx: &Context<'_>,
) -> Result<(), String> {
    // The payload reads the release's custom formats: without a release
    // the original throws there, which the handler records as a failure.
    if m.remote.is_none() {
        return Err("Object reference not set to an instance of an object.".to_string());
    }
    let settings = Settings::from_value(&def.settings);
    post(http, &settings, &manual_interaction_payload(m, ctx)).await
}

/// `BuildApplicationUpdatePayload`: Radarr's carries the application URL.
pub(crate) fn application_update_payload(update: &ApplicationUpdateMessage, ctx: &Context<'_>) -> Value {
    let mut o = base("ApplicationUpdate", ctx);
    o.set("message", json!(update.message));
    o.set("previousVersion", json!(update.previous_version));
    o.set("newVersion", json!(update.new_version));
    o.done()
}

pub async fn on_application_update(
    http: &reqwest::Client,
    def: &MovieNotificationDefinition,
    update: &ApplicationUpdateMessage,
    ctx: &Context<'_>,
) -> Result<(), String> {
    let settings = Settings::from_value(&def.settings);
    post(http, &settings, &application_update_payload(update, ctx)).await
}

/// `BuildHealthPayload`: with the application URL.
pub async fn on_health_issue(
    http: &reqwest::Client,
    def: &MovieNotificationDefinition,
    check: &HealthCheck,
    identity: &Identity,
    application_url: &str,
) -> Result<(), String> {
    let settings = Settings::from_value(&def.settings);
    let mut o = Object::new();
    o.set("eventType", json!("Health"));
    o.set("instanceName", json!(identity.instance_name));
    o.set("applicationUrl", json!(application_url));
    o.set("level", json!(check.kind.as_str()));
    o.set("message", json!(check.message));
    o.set("type", json!(check.source));
    o.set("wikiUrl", json!(check.wiki_url));
    post(http, &settings, &o.done()).await
}

/// `BuildHealthRestoredPayload`: the original leaves the application URL
/// out of this one.
pub async fn on_health_restored(
    http: &reqwest::Client,
    def: &MovieNotificationDefinition,
    previous: &HealthCheck,
    identity: &Identity,
) -> Result<(), String> {
    let settings = Settings::from_value(&def.settings);
    let mut o = Object::new();
    o.set("eventType", json!("HealthRestored"));
    o.set("instanceName", json!(identity.instance_name));
    o.set("level", json!(previous.kind.as_str()));
    o.set("message", json!(previous.message));
    o.set("type", json!(previous.source));
    o.set("wikiUrl", json!(previous.wiki_url));
    post(http, &settings, &o.done()).await
}

/// `BuildTestPayload`.
pub(crate) fn test_payload(ctx: &Context<'_>) -> Value {
    let mut o = base("Test", ctx);
    o.set(
        "movie",
        json!({
            "id": 1,
            "title": "Test Title",
            "year": 1970,
            "releaseDate": "1970-01-01",
            "folderPath": "C:\\testpath",
            "tmdbId": 0,
            "tags": ["test-tag"],
        }),
    );
    o.set(
        "remoteMovie",
        json!({
            "tmdbId": 1234,
            "imdbId": "5678",
            "title": "Test title",
            "year": 1970,
        }),
    );
    o.set(
        "release",
        json!({
            "quality": "Test Quality",
            "qualityVersion": 1,
            "releaseGroup": "Test Group",
            "releaseTitle": "Test Title",
            "indexer": "Test Indexer",
            "size": 9999999,
            "customFormatScore": 0,
        }),
    );
    o.done()
}

/// `Webhook.Test`: the fixed test payload; a failed call is a validation
/// failure on the URL, one that got no answer escapes.
pub async fn test(
    http: &reqwest::Client,
    def: &MovieNotificationDefinition,
    ctx: &Context<'_>,
) -> TestOutcome {
    let settings = Settings::from_value(&def.settings);
    match send(http, &settings, &test_payload(ctx)).await {
        Ok(()) => TestOutcome::Failures(Vec::new()),
        Err(SendError::Status(_, m)) => TestOutcome::Failures(vec![ValidationFailure::error(
            "Url",
            &format!("Unable to send test message: {m}"),
        )]),
        Err(SendError::Transport(m)) => TestOutcome::Error(m),
    }
}
