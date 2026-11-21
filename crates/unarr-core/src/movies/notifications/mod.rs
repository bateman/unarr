//! Radarr's notifications, after its `NotificationService`,
//! `NotificationFactory` and `NotificationDefinition`: what
//! `crate::notifications` is to Sonarr, for movies. The stored connections,
//! their status and back-off, and the transports (Discord's webhook, the
//! generic webhook, the Plex proxy) are the series side's; the event flags,
//! the messages, the embeds, the payloads and the library update are
//! Radarr's and live here.
//!
//! The runtime hands the events over as they happen: a grab
//! (`MovieGrabbedEvent`), the import pipeline's (`MovieFileImportedEvent`,
//! `MovieFileDeletedEvent`, `MovieRenamedEvent`,
//! `ManualInteractionRequiredEvent`), the library's (`MovieAddedEvent`,
//! `MoviesImportedEvent`, `MoviesDeletedEvent`) and the health verdicts.
//! A Plex update is queued per server and performed on `ProcessQueue`,
//! which the original runs on `DownloadsProcessedEvent`,
//! `DeleteCompletedEvent`, `RenameCompletedEvent` and
//! `HealthCheckCompleteEvent`.
//!
//! `UpdateInstalledEvent` has no publisher in Unarr yet: its flag is stored
//! and shown, and nothing is sent for it.

pub mod discord;
pub mod plex;
pub mod webhook;

use crate::sync::Recover;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::downloadclients::{DownloadClientDefinition, ItemClientInfo};
use crate::health::{HealthCheck, HealthType};
use crate::mediafiles::local::DeleteReason;
use crate::model::{CustomFormat, Image, Provider, QualityModel, Tag};
use crate::movies::decision::RemoteMovie;
use crate::movies::downloads::TrackedMovieDownload;
use crate::movies::import::events::{MovieFileRenamed, MovieImportEvent};
use crate::movies::import::local::{DeletedMovieFile, GrabbedMovieRelease};
use crate::movies::model::{Movie, MovieFile};
use crate::notifications::{
    discord as base_discord, plex as base_plex, webhook as base_webhook, Context, Kind, Notifier, TestOutcome,
};
use crate::validation::ValidationFailure;

/// Radarr's `NotificationDefinition`: a stored connection with its event
/// flags.
#[derive(Debug, Clone, PartialEq)]
pub struct MovieNotificationDefinition {
    pub id: i64,
    pub name: String,
    pub implementation: String,
    pub settings: Value,
    pub tags: Vec<i64>,
    pub on_grab: bool,
    pub on_download: bool,
    pub on_upgrade: bool,
    pub on_rename: bool,
    pub on_movie_added: bool,
    pub on_movie_delete: bool,
    pub on_movie_file_delete: bool,
    pub on_movie_file_delete_for_upgrade: bool,
    pub on_health_issue: bool,
    pub on_health_restored: bool,
    pub on_application_update: bool,
    pub on_manual_interaction_required: bool,
    pub include_health_warnings: bool,
}

/// The `on*` attributes Radarr's notification resource carries, in the
/// original's order; a missing one is false.
pub const FLAGS: &[&str] = &[
    "onGrab",
    "onDownload",
    "onUpgrade",
    "onRename",
    "onMovieAdded",
    "onMovieDelete",
    "onMovieFileDelete",
    "onMovieFileDeleteForUpgrade",
    "onHealthIssue",
    "onHealthRestored",
    "onApplicationUpdate",
    "onManualInteractionRequired",
    "includeHealthWarnings",
];

/// The `SupportsOn*` of an implementation: which of `NotificationBase`'s
/// handlers it overrides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Supports {
    pub on_grab: bool,
    pub on_download: bool,
    pub on_upgrade: bool,
    pub on_rename: bool,
    pub on_movie_added: bool,
    pub on_movie_delete: bool,
    pub on_movie_file_delete: bool,
    pub on_movie_file_delete_for_upgrade: bool,
    pub on_health_issue: bool,
    pub on_health_restored: bool,
    pub on_application_update: bool,
    pub on_manual_interaction_required: bool,
}

impl Supports {
    /// What Radarr's `Discord`, `Webhook` and `PlexServer` declare:
    /// everything for the first two; for Plex the events that change the
    /// library's files (`OnDownload`, `OnMovieRename`, `OnMovieFileDelete`,
    /// `OnMovieDelete`), with `SupportsOnUpgrade` and
    /// `SupportsOnMovieFileDeleteForUpgrade` following `OnDownload` and
    /// `OnMovieFileDelete` as `NotificationBase` has them.
    pub fn of(kind: Kind) -> Supports {
        match kind {
            Kind::Discord | Kind::Webhook => Supports {
                on_grab: true,
                on_download: true,
                on_upgrade: true,
                on_rename: true,
                on_movie_added: true,
                on_movie_delete: true,
                on_movie_file_delete: true,
                on_movie_file_delete_for_upgrade: true,
                on_health_issue: true,
                on_health_restored: true,
                on_application_update: true,
                on_manual_interaction_required: true,
            },
            Kind::PlexServer => Supports {
                on_download: true,
                on_upgrade: true,
                on_rename: true,
                on_movie_delete: true,
                on_movie_file_delete: true,
                on_movie_file_delete_for_upgrade: true,
                ..Supports::default()
            },
        }
    }

    /// The value of one `supportsOn*` attribute, by the name the resource
    /// gives it; `None` for a name that is not one.
    pub fn get(&self, name: &str) -> Option<bool> {
        Some(match name {
            "supportsOnGrab" => self.on_grab,
            "supportsOnDownload" => self.on_download,
            "supportsOnUpgrade" => self.on_upgrade,
            "supportsOnRename" => self.on_rename,
            "supportsOnMovieAdded" => self.on_movie_added,
            "supportsOnMovieDelete" => self.on_movie_delete,
            "supportsOnMovieFileDelete" => self.on_movie_file_delete,
            "supportsOnMovieFileDeleteForUpgrade" => self.on_movie_file_delete_for_upgrade,
            "supportsOnHealthIssue" => self.on_health_issue,
            "supportsOnHealthRestored" => self.on_health_restored,
            "supportsOnApplicationUpdate" => self.on_application_update,
            "supportsOnManualInteractionRequired" => self.on_manual_interaction_required,
            _ => return None,
        })
    }
}

impl MovieNotificationDefinition {
    pub fn from_provider(p: &Provider) -> MovieNotificationDefinition {
        let flag = |name: &str| p.attributes[name].as_bool().unwrap_or(false);
        MovieNotificationDefinition {
            id: p.id,
            name: p.name.clone(),
            implementation: p.implementation.clone(),
            settings: p.settings.clone(),
            tags: p.tags.clone(),
            on_grab: flag("onGrab"),
            on_download: flag("onDownload"),
            on_upgrade: flag("onUpgrade"),
            on_rename: flag("onRename"),
            on_movie_added: flag("onMovieAdded"),
            on_movie_delete: flag("onMovieDelete"),
            on_movie_file_delete: flag("onMovieFileDelete"),
            on_movie_file_delete_for_upgrade: flag("onMovieFileDeleteForUpgrade"),
            on_health_issue: flag("onHealthIssue"),
            on_health_restored: flag("onHealthRestored"),
            on_application_update: flag("onApplicationUpdate"),
            on_manual_interaction_required: flag("onManualInteractionRequired"),
            include_health_warnings: flag("includeHealthWarnings"),
        }
    }

    pub fn kind(&self) -> Option<Kind> {
        Kind::from_implementation(&self.implementation)
    }

    /// The `SupportsOn*` of the implementation; nothing for one Unarr does
    /// not drive.
    pub fn supports(&self) -> Supports {
        self.kind().map(Supports::of).unwrap_or_default()
    }

    /// `NotificationDefinition.Enable`: any event flag set. `OnUpgrade`
    /// and `OnMovieFileDeleteForUpgrade` count only beside the flag they
    /// refine, which already counts.
    pub fn enabled(&self) -> bool {
        self.on_grab
            || self.on_download
            || self.on_rename
            || self.on_movie_added
            || self.on_movie_delete
            || self.on_movie_file_delete
            || self.on_health_issue
            || self.on_health_restored
            || self.on_application_update
            || self.on_manual_interaction_required
    }

    /// `definition.Settings.Validate()` for the implementations driven
    /// here, whose settings and rules are the series side's; the others'
    /// settings are stored as given.
    pub fn validate_settings(&self) -> Vec<ValidationFailure> {
        match self.kind() {
            Some(Kind::Discord) => base_discord::Settings::from_value(&self.settings).validate(),
            Some(Kind::Webhook) => base_webhook::Settings::from_value(&self.settings).validate(),
            Some(Kind::PlexServer) => base_plex::Settings::from_value(&self.settings).validate(),
            None => Vec::new(),
        }
    }

    /// `ShouldHandleMovie`: no tags, or one in common with the movie.
    pub fn handles(&self, movie: &Movie) -> bool {
        self.tags.is_empty() || self.tags.iter().any(|t| movie.tags.contains(t))
    }

    /// `ShouldHandleHealthFailure`.
    fn handles_health(&self, check: &HealthCheck) -> bool {
        check.kind == HealthType::Error || (check.kind == HealthType::Warning && self.include_health_warnings)
    }
}

/// Radarr's `GrabMessage`.
pub struct GrabMessage<'a> {
    pub movie: &'a Movie,
    pub remote: &'a RemoteMovie,
    pub quality: QualityModel,
    pub download_client_name: &'a str,
    pub download_client_type: &'a str,
    pub download_id: Option<&'a str>,
}

/// Radarr's `DownloadMessage`.
pub struct DownloadMessage<'a> {
    pub movie: &'a Movie,
    pub file: &'a MovieFile,
    pub old_files: &'a [DeletedMovieFile],
    pub source_path: &'a str,
    pub client: Option<&'a ItemClientInfo>,
    pub download_id: Option<&'a str>,
    pub release: Option<&'a GrabbedMovieRelease>,
    pub custom_formats: &'a [CustomFormat],
    pub custom_format_score: i64,
}

/// `MovieFileDeleteMessage`.
pub struct MovieFileDeleteMessage<'a> {
    pub movie: &'a Movie,
    pub file: &'a MovieFile,
    pub reason: DeleteReason,
}

/// Radarr's `MovieDeleteMessage`, with the size of the file the movie had,
/// which the original reads off the movie (`Movie.MovieFile`, joined when
/// the movie was loaded for its deletion).
pub struct MovieDeleteMessage<'a> {
    pub movie: &'a Movie,
    pub deleted_files: bool,
    pub file_size: Option<i64>,
}

impl MovieDeleteMessage<'_> {
    /// `DeletedFilesMessage`.
    pub fn deleted_files_message(&self) -> &'static str {
        if self.deleted_files {
            "Movie removed and all files were deleted"
        } else {
            "Movie removed, files were not deleted"
        }
    }
}

/// Radarr's `ManualInteractionRequiredMessage`.
pub struct ManualInteractionMessage<'a> {
    pub movie: Option<&'a Movie>,
    pub remote: &'a RemoteMovie,
    pub quality: QualityModel,
    pub tracked: &'a TrackedMovieDownload,
    pub release: Option<&'a GrabbedMovieRelease>,
}

/// `GetTagLabels`: the labels of a movie's tags, sorted.
pub(crate) fn tag_labels(tags: &[Tag], movie: &Movie) -> Vec<String> {
    let mut labels: Vec<String> = tags
        .iter()
        .filter(|t| movie.tags.contains(&t.id) && !t.label.trim().is_empty())
        .map(|t| t.label.clone())
        .collect();
    labels.sort();
    labels
}

/// `Quality.Name` of a movie's quality model.
pub(crate) fn quality_name(q: &QualityModel) -> &'static str {
    crate::movies::quality::MovieQuality::by_id(q.quality_id).name
}

/// `MediaCoverService.ConvertToLocalUrls(movieId, covers)`: every cover
/// gets the address it is served at, with the first twenty hex digits of
/// its remote URL's SHA-256 when it has one. The original writes this into
/// the movie object it is handed, which is why a movie a tracked download
/// holds shows the addresses afterwards.
pub fn convert_to_local_urls(url_base: &str, movie_id: i64, images: &mut [Image]) {
    for image in images {
        if image.cover_type.eq_ignore_ascii_case("unknown") {
            continue;
        }
        let cover_type = image.cover_type.to_lowercase();
        let extension = if cover_type == "clearlogo" { ".png" } else { ".jpg" };
        let mut url = format!("{url_base}/MediaCover/{movie_id}/{cover_type}{extension}");
        if let Some(remote) = image.remote_url.as_deref().filter(|r| !r.trim().is_empty()) {
            let digest = ring::digest::digest(&ring::digest::SHA256, remote.as_bytes());
            let hash: String = digest
                .as_ref()
                .iter()
                .take(10)
                .map(|b| format!("{b:02x}"))
                .collect();
            url.push_str("?h=");
            url.push_str(&hash);
        }
        image.url = Some(url);
    }
}

/// A download whose movie had its covers given their local addresses by
/// a webhook payload, and the event the payload was for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConvertedCovers {
    pub download_id: String,
    pub by: ConvertedBy,
}

/// Which event's payload converted the covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConvertedBy {
    /// `ManualInteractionRequiredEvent`: the message's movie is always the
    /// tracked download's.
    ManualInteraction,
    /// `MovieFileImportedEvent`: the message's movie is the one the import
    /// was handed, the tracked download's when the download itself was
    /// imported and another object when a manual import chose the movie.
    Import,
}

/// Radarr's `NotificationService`, over the series side's connections,
/// statuses and transports.
pub struct MovieNotifier {
    base: Arc<Notifier>,
    /// `MediaServerUpdateQueue<PlexServer, bool>`: the movies whose Plex
    /// section awaits a refresh, per host, in the order they came.
    plex_pending: Mutex<HashMap<String, Vec<Movie>>>,
    /// One `ProcessQueue` at a time (`UpdateQueue.Refreshing`).
    flushing: tokio::sync::Mutex<()>,
}

impl MovieNotifier {
    pub fn new(base: Arc<Notifier>) -> MovieNotifier {
        MovieNotifier {
            base,
            plex_pending: Mutex::new(HashMap::new()),
            flushing: tokio::sync::Mutex::new(()),
        }
    }

    /// The series side's notifier this one sends through.
    pub fn base(&self) -> &Arc<Notifier> {
        &self.base
    }

    /// Every stored notification, by id.
    pub async fn definitions(&self) -> anyhow::Result<Vec<MovieNotificationDefinition>> {
        Ok(self
            .base
            .providers()
            .await?
            .iter()
            .map(MovieNotificationDefinition::from_provider)
            .collect())
    }

    /// `GetAvailableProviders` through `FilterBlockedNotifications`: the
    /// enabled ones, without those in back-off when `filter_blocked`.
    async fn available(&self, now: DateTime<Utc>, filter_blocked: bool) -> Vec<MovieNotificationDefinition> {
        let defs = match self.definitions().await {
            Ok(defs) => defs,
            Err(e) => {
                tracing::error!(error = %e, "failed to load the notifications");
                return Vec::new();
            }
        };
        let blocked = if filter_blocked {
            self.base.blocked(now).await
        } else {
            Vec::new()
        };
        defs.into_iter()
            .filter(|d| d.enabled())
            .filter(|d| {
                if blocked.contains(&d.id) {
                    tracing::debug!(notification = %d.name, "temporarily ignoring notification due to recent failures");
                    false
                } else {
                    true
                }
            })
            .collect()
    }

    async fn deliver(
        &self,
        def: &MovieNotificationDefinition,
        what: &str,
        now: DateTime<Utc>,
        result: Result<(), String>,
    ) {
        self.base.deliver_to(def.id, &def.name, what, now, result).await;
    }

    /// `Handle(MovieGrabbedEvent)`.
    pub async fn on_grab(
        &self,
        remote: &RemoteMovie,
        client: &DownloadClientDefinition,
        download_id: Option<&str>,
        now: DateTime<Utc>,
    ) {
        // The original reads the movie and the parsed quality off the
        // release before anything else; a grab always has both.
        let (Some(movie), Some(parsed)) = (remote.movie.as_ref(), remote.parsed.as_ref()) else {
            return;
        };
        let message = GrabMessage {
            movie,
            remote,
            quality: parsed.quality.quality,
            download_client_name: &client.name,
            download_client_type: client.kind.display_name(),
            download_id,
        };
        let (application_url, tags) = self.base.context_parts().await;
        let ctx = Context {
            identity: self.base.identity(),
            application_url: &application_url,
            tags: &tags,
            now,
        };
        for def in self.available(now, true).await {
            if !def.on_grab || !def.handles(movie) {
                continue;
            }
            let result = match def.kind() {
                Some(Kind::Discord) => discord::on_grab(self.base.http(), &def, &message, &ctx).await,
                Some(Kind::Webhook) => webhook::on_grab(self.base.http(), &def, &message, &ctx).await,
                _ => Ok(()),
            };
            self.deliver(&def, "OnGrab", now, result).await;
        }
    }

    /// The import pipeline's events, in order, through the original's
    /// synchronous handlers. Returns the downloads whose movie a webhook
    /// payload was built for: the original rewrote the covers of the movie
    /// object it was handed (`WebhookBase.GetMovie`), and where that is
    /// the object a tracked download holds, whoever shows the download's
    /// movie shows the local addresses from then on.
    pub async fn on_import_events(
        &self,
        events: &[MovieImportEvent],
        now: DateTime<Utc>,
    ) -> Vec<ConvertedCovers> {
        let mut converted = Vec::new();
        for e in events {
            match e {
                MovieImportEvent::Imported(ev) => {
                    if !ev.new_download {
                        continue;
                    }
                    let Some(movie) = ev.local.movie.as_ref() else {
                        continue;
                    };
                    let message = DownloadMessage {
                        movie,
                        file: &ev.file,
                        old_files: &ev.old_files,
                        source_path: &ev.local.path,
                        client: ev.client.as_ref(),
                        download_id: ev.download_id.as_deref(),
                        release: ev.local.release.as_ref(),
                        custom_formats: &ev.local.custom_formats,
                        custom_format_score: ev.local.custom_format_score,
                    };
                    if self.on_download(&message, now).await {
                        converted.extend(ev.download_id.clone().map(|download_id| ConvertedCovers {
                            download_id,
                            by: ConvertedBy::Import,
                        }));
                    }
                }
                MovieImportEvent::FileDeleted(ev) => {
                    let message = MovieFileDeleteMessage {
                        movie: &ev.movie,
                        file: &ev.file,
                        reason: ev.reason,
                    };
                    self.on_movie_file_delete(&message, now).await;
                }
                MovieImportEvent::MovieRenamed { renamed, .. } => {
                    if let Some(first) = renamed.first() {
                        self.on_rename(&first.movie, renamed, now).await;
                    }
                }
                MovieImportEvent::ManualInteractionRequired(ev) => {
                    if self
                        .on_manual_interaction_required(&ev.tracked, ev.release.as_ref(), now)
                        .await
                    {
                        converted.push(ConvertedCovers {
                            download_id: ev.tracked.download_id().to_string(),
                            by: ConvertedBy::ManualInteraction,
                        });
                    }
                }
                // `DownloadCompletedEvent` has no handler in Radarr's
                // `NotificationService`; the rest never had one.
                MovieImportEvent::DownloadCompleted(_)
                | MovieImportEvent::ImportFailed(_)
                | MovieImportEvent::FileRenamed(_)
                | MovieImportEvent::Scanned { .. }
                | MovieImportEvent::ScanSkipped { .. }
                | MovieImportEvent::RescanRequested { .. } => {}
            }
        }
        converted
    }

    /// `Handle(MovieFileImportedEvent)`; `true` when a webhook payload was
    /// built for the movie.
    async fn on_download(&self, message: &DownloadMessage<'_>, now: DateTime<Utc>) -> bool {
        let (application_url, tags) = self.base.context_parts().await;
        let ctx = Context {
            identity: self.base.identity(),
            application_url: &application_url,
            tags: &tags,
            now,
        };
        let mut webhook_built = false;
        for def in self.available(now, true).await {
            if !def.on_download || !def.handles(message.movie) {
                continue;
            }
            if !message.old_files.is_empty() && !def.on_upgrade {
                continue;
            }
            let result = match def.kind() {
                Some(Kind::Discord) => discord::on_download(self.base.http(), &def, message, &ctx).await,
                Some(Kind::Webhook) => {
                    webhook_built = true;
                    webhook::on_download(self.base.http(), &def, message, &ctx).await
                }
                Some(Kind::PlexServer) => {
                    self.plex_update_if_enabled(&def, message.movie).await;
                    Ok(())
                }
                None => Ok(()),
            };
            self.deliver(&def, "OnDownload", now, result).await;
        }
        webhook_built
    }

    /// `Handle(MovieFileDeletedEvent)`.
    async fn on_movie_file_delete(&self, message: &MovieFileDeleteMessage<'_>, now: DateTime<Utc>) {
        let (application_url, tags) = self.base.context_parts().await;
        let ctx = Context {
            identity: self.base.identity(),
            application_url: &application_url,
            tags: &tags,
            now,
        };
        for def in self.available(now, true).await {
            if !def.on_movie_file_delete {
                continue;
            }
            if message.reason == DeleteReason::Upgrade && !def.on_movie_file_delete_for_upgrade {
                continue;
            }
            if !def.handles(message.movie) {
                continue;
            }
            let result = match def.kind() {
                Some(Kind::Discord) => {
                    discord::on_movie_file_delete(self.base.http(), &def, message, &ctx).await
                }
                Some(Kind::Webhook) => {
                    webhook::on_movie_file_delete(self.base.http(), &def, message, &ctx).await
                }
                Some(Kind::PlexServer) => {
                    self.plex_update_if_enabled(&def, message.movie).await;
                    Ok(())
                }
                None => Ok(()),
            };
            self.deliver(&def, "OnMovieFileDelete", now, result).await;
        }
    }

    /// `Handle(MovieAddedEvent)` and `Handle(MoviesImportedEvent)`: the
    /// first for a movie added alone, the second for a list of them (a
    /// bulk add, a library import, an import list, a collection). The
    /// second wraps a connection's whole list in one `try`, so a
    /// connection stops at its first failure; with one movie the two are
    /// the same. Plex does not override `OnMovieAdded`: the base's no-op
    /// counts as sent.
    pub async fn on_movies_added(&self, movies: &[Movie], now: DateTime<Utc>) {
        if movies.is_empty() {
            return;
        }
        let (application_url, tags) = self.base.context_parts().await;
        let ctx = Context {
            identity: self.base.identity(),
            application_url: &application_url,
            tags: &tags,
            now,
        };
        for def in self.available(now, true).await {
            if !def.on_movie_added {
                continue;
            }
            for movie in movies {
                if !def.handles(movie) {
                    continue;
                }
                let result = match def.kind() {
                    Some(Kind::Discord) => discord::on_movie_added(self.base.http(), &def, movie, &ctx).await,
                    Some(Kind::Webhook) => webhook::on_movie_added(self.base.http(), &def, movie, &ctx).await,
                    _ => Ok(()),
                };
                let failed = result.is_err();
                self.deliver(&def, "OnMovieAdded", now, result).await;
                if failed {
                    break;
                }
            }
        }
    }

    /// `Handle(MoviesDeletedEvent)`: one message per movie; Plex updates
    /// its library when the files went. `file_sizes` holds the size of
    /// each deleted movie's file, by movie id.
    pub async fn on_movies_deleted(
        &self,
        movies: &[Movie],
        file_sizes: &HashMap<i64, i64>,
        deleted_files: bool,
        now: DateTime<Utc>,
    ) {
        if movies.is_empty() {
            return;
        }
        let (application_url, tags) = self.base.context_parts().await;
        let ctx = Context {
            identity: self.base.identity(),
            application_url: &application_url,
            tags: &tags,
            now,
        };
        for movie in movies {
            let message = MovieDeleteMessage {
                movie,
                deleted_files,
                file_size: file_sizes.get(&movie.id).copied(),
            };
            for def in self.available(now, true).await {
                if !def.on_movie_delete || !def.handles(movie) {
                    continue;
                }
                let result = match def.kind() {
                    Some(Kind::Discord) => {
                        discord::on_movie_delete(self.base.http(), &def, &message, &ctx).await
                    }
                    Some(Kind::Webhook) => {
                        webhook::on_movie_delete(self.base.http(), &def, &message, &ctx).await
                    }
                    Some(Kind::PlexServer) => {
                        if deleted_files {
                            self.plex_update_if_enabled(&def, movie).await;
                        }
                        Ok(())
                    }
                    None => Ok(()),
                };
                self.deliver(&def, "OnMovieDelete", now, result).await;
            }
        }
    }

    /// `Handle(MovieRenamedEvent)`.
    async fn on_rename(&self, movie: &Movie, renamed: &[MovieFileRenamed], now: DateTime<Utc>) {
        let (application_url, tags) = self.base.context_parts().await;
        let ctx = Context {
            identity: self.base.identity(),
            application_url: &application_url,
            tags: &tags,
            now,
        };
        for def in self.available(now, true).await {
            if !def.on_rename || !def.handles(movie) {
                continue;
            }
            let result = match def.kind() {
                Some(Kind::Discord) => discord::on_rename(self.base.http(), &def, movie, renamed).await,
                Some(Kind::Webhook) => webhook::on_rename(self.base.http(), &def, movie, renamed, &ctx).await,
                Some(Kind::PlexServer) => {
                    self.plex_update_if_enabled(&def, movie).await;
                    Ok(())
                }
                None => Ok(()),
            };
            self.deliver(&def, "OnRename", now, result).await;
        }
    }

    /// `Handle(ManualInteractionRequiredEvent)`; `true` when a webhook
    /// payload was built for the download's movie.
    async fn on_manual_interaction_required(
        &self,
        tracked: &TrackedMovieDownload,
        release: Option<&GrabbedMovieRelease>,
        now: DateTime<Utc>,
    ) -> bool {
        let remote = tracked.remote.as_ref();
        let movie = remote.and_then(|r| r.movie.as_ref());
        // The message is the movie's or, failing that, the download's
        // title; nothing is sent when both are blank. With a release but
        // nothing parsed from it the original dereferences a null before
        // it reaches any connection.
        if movie.is_none() && tracked.item.title.trim().is_empty() {
            return false;
        }
        if remote.is_some_and(|r| r.parsed.is_none()) {
            tracing::error!("Object reference not set to an instance of an object.");
            return false;
        }
        let (application_url, tags) = self.base.context_parts().await;
        let ctx = Context {
            identity: self.base.identity(),
            application_url: &application_url,
            tags: &tags,
            now,
        };
        let mut webhook_built = false;
        for def in self.available(now, true).await {
            if !def.on_manual_interaction_required {
                continue;
            }
            // `ShouldHandleMovie(definition, message.RemoteMovie.Movie)`:
            // without a release, or with tags to compare and no movie, the
            // original throws, which the handler's catch records as a
            // failure of the connection.
            let null = || Err("Object reference not set to an instance of an object.".to_string());
            let Some(remote) = remote else {
                self.deliver(&def, "OnManualInteractionRequired", now, null())
                    .await;
                continue;
            };
            match movie {
                Some(m) if !def.handles(m) => continue,
                None if !def.tags.is_empty() => {
                    self.deliver(&def, "OnManualInteractionRequired", now, null())
                        .await;
                    continue;
                }
                _ => {}
            }
            let message = ManualInteractionMessage {
                movie,
                remote,
                quality: remote
                    .parsed
                    .as_ref()
                    .map(|p| p.quality.quality)
                    .unwrap_or_default(),
                tracked,
                release,
            };
            let result = match def.kind() {
                Some(Kind::Discord) => {
                    discord::on_manual_interaction_required(self.base.http(), &def, &message, &ctx).await
                }
                Some(Kind::Webhook) => {
                    webhook_built |= movie.is_some();
                    webhook::on_manual_interaction_required(self.base.http(), &def, &message, &ctx).await
                }
                _ => Ok(()),
            };
            self.deliver(&def, "OnManualInteractionRequired", now, result)
                .await;
        }
        webhook_built
    }

    /// `Handle(HealthCheckFailedEvent)`, outside the startup grace period.
    pub async fn on_health_issue(&self, check: &HealthCheck, now: DateTime<Utc>) {
        let (application_url, _) = self.base.context_parts().await;
        for def in self.available(now, true).await {
            if !def.on_health_issue || !def.handles_health(check) {
                continue;
            }
            let result = match def.kind() {
                Some(Kind::Discord) => {
                    discord::on_health_issue(self.base.http(), &def, check, self.base.identity(), now).await
                }
                Some(Kind::Webhook) => {
                    webhook::on_health_issue(
                        self.base.http(),
                        &def,
                        check,
                        self.base.identity(),
                        &application_url,
                    )
                    .await
                }
                _ => Ok(()),
            };
            self.deliver(&def, "OnHealthIssue", now, result).await;
        }
    }

    /// `Handle(HealthCheckRestoredEvent)`, outside the startup grace period.
    pub async fn on_health_restored(&self, previous: &HealthCheck, now: DateTime<Utc>) {
        for def in self.available(now, true).await {
            if !def.on_health_restored || !def.handles_health(previous) {
                continue;
            }
            let result = match def.kind() {
                Some(Kind::Discord) => {
                    discord::on_health_restored(self.base.http(), &def, previous, self.base.identity(), now)
                        .await
                }
                Some(Kind::Webhook) => {
                    webhook::on_health_restored(self.base.http(), &def, previous, self.base.identity()).await
                }
                _ => Ok(()),
            };
            self.deliver(&def, "OnHealthRestored", now, result).await;
        }
    }

    /// `PlexServer.UpdateIfEnabled`: plex.tv is pinged with the token, once
    /// a day so that the token does not expire, then the movie joins the
    /// server's queue, once per movie.
    async fn plex_update_if_enabled(&self, def: &MovieNotificationDefinition, movie: &Movie) {
        let settings = base_plex::Settings::from_value(&def.settings);
        base_plex::ping_plex_tv(&self.base, &settings.auth_token).await;
        if !settings.update_library {
            return;
        }
        tracing::debug!(id = movie.id, title = %movie.metadata.title, "scheduling library update for movie");
        let mut pending = self.plex_pending.lock().recover();
        let queue = pending.entry(settings.host.clone()).or_default();
        // `queue.Pending[movie.Id] = item`: a movie already waiting keeps
        // its place and the object first queued for it.
        if !queue.iter().any(|m| m.id == movie.id) {
            queue.push(movie.clone());
        }
    }

    /// The movies waiting for a library update on `host`, in order.
    pub fn plex_pending(&self, host: &str) -> Vec<Movie> {
        self.plex_pending
            .lock()
            .recover()
            .get(host)
            .cloned()
            .unwrap_or_default()
    }

    /// `NotificationService.ProcessQueue`: every enabled connection, in
    /// back-off or not, performs what it queued; only Plex queues
    /// anything. What a failed update had taken off the queue is not put
    /// back, and the failure is logged, not recorded against the
    /// connection.
    pub async fn process_queue(&self, now: DateTime<Utc>) {
        let _one_at_a_time = self.flushing.lock().await;
        for def in self.available(now, false).await {
            if def.kind() != Some(Kind::PlexServer) {
                continue;
            }
            let settings = base_plex::Settings::from_value(&def.settings);
            let items: Vec<Movie> = {
                let mut pending = self.plex_pending.lock().recover();
                pending.remove(&settings.host).unwrap_or_default()
            };
            if items.is_empty() || !settings.update_library {
                continue;
            }
            tracing::debug!("performing library update for {} movies", items.len());
            if let Err(e) = plex::update_library(&self.base, &settings, &items, now).await {
                tracing::warn!(host = %settings.host, error = %e, "failed to update Plex host");
            }
        }
    }

    /// `NotificationFactory.Test(definition)`: the connection test, whose
    /// outcome a stored connection's status records.
    pub async fn test(&self, def: &MovieNotificationDefinition, now: DateTime<Utc>) -> TestOutcome {
        let outcome = match def.kind() {
            Some(Kind::Discord) => discord::test(self.base.http(), def, now).await,
            Some(Kind::Webhook) => {
                let (application_url, tags) = self.base.context_parts().await;
                let ctx = Context {
                    identity: self.base.identity(),
                    application_url: &application_url,
                    tags: &tags,
                    now,
                };
                webhook::test(self.base.http(), def, &ctx).await
            }
            Some(Kind::PlexServer) => {
                plex::test(&self.base, &base_plex::Settings::from_value(&def.settings)).await
            }
            None => TestOutcome::Failures(vec![ValidationFailure::error(
                "",
                &format!(
                    "Test was aborted due to an error: Unarr does not drive {} notifications yet",
                    def.implementation
                ),
            )]),
        };
        if def.id > 0 && def.kind().is_some() {
            match &outcome {
                TestOutcome::Failures(f) if f.is_empty() => self.base.record_success(def.id).await,
                TestOutcome::Failures(_) => self.base.record_failure(def.id, now).await,
                // An exception escapes the factory before it records.
                TestOutcome::Error(_) => {}
            }
        }
        outcome
    }
}

#[cfg(test)]
mod tests;
