//! `Queue` items, after Radarr's `QueueService`: one per tracked download,
//! with the id the UI addresses it by.

use chrono::{DateTime, Duration, Utc};

use super::tracked::TrackedMovieDownload;
use crate::downloads::queue::{compare_time_left, hash_int31};
use crate::downloads::{QueueStatus, StatusMessage, TrackedDownloadState, TrackedDownloadStatus};
use crate::model::{CustomFormat, Protocol, QualityModel};
use crate::movies::decision::{QueuedMovie, RemoteMovie};
use crate::movies::model::Movie;
use crate::movies::parser::language::lang;

#[derive(Debug, Clone, PartialEq)]
pub struct MovieQueueItem {
    pub id: i64,
    pub movie: Option<Movie>,
    /// Whether the movie shows its alternative titles (see
    /// `TrackedMovieDownload::movie_by_id`).
    pub movie_by_id: bool,
    pub languages: Vec<i64>,
    pub quality: QualityModel,
    pub custom_formats: Vec<CustomFormat>,
    pub size: i64,
    pub title: String,
    pub size_left: i64,
    pub time_left: Option<Duration>,
    pub estimated_completion_time: Option<DateTime<Utc>>,
    pub added: Option<DateTime<Utc>>,
    pub status: QueueStatus,
    pub tracked_download_status: Option<TrackedDownloadStatus>,
    pub tracked_download_state: Option<TrackedDownloadState>,
    /// `None` for a pending release, which has no tracked download.
    pub status_messages: Option<Vec<StatusMessage>>,
    pub error_message: Option<String>,
    pub download_id: Option<String>,
    pub remote: Option<RemoteMovie>,
    pub protocol: Protocol,
    pub download_client: Option<String>,
    pub download_client_has_post_import_category: bool,
    pub indexer: Option<String>,
    pub output_path: Option<String>,
}

impl MovieQueueItem {
    /// `100 - sizeLeft / size * 100`, the progress the original sorts by.
    pub fn progress(&self) -> f64 {
        if self.size == 0 {
            return 0.0;
        }
        100.0 - (self.size_left as f64 / self.size as f64 * 100.0)
    }
}

/// `MapQueue` over the trackable downloads, by remaining time.
pub fn build(tracked: &[&TrackedMovieDownload], now: DateTime<Utc>) -> Vec<MovieQueueItem> {
    let mut list: Vec<&TrackedMovieDownload> = tracked.iter().copied().filter(|t| t.is_trackable).collect();
    list.sort_by(|a, b| compare_time_left(a.item.remaining_time, b.item.remaining_time));
    list.into_iter().map(|t| map_item(t, now)).collect()
}

/// `MapMovie`.
fn map_item(t: &TrackedMovieDownload, now: DateTime<Utc>) -> MovieQueueItem {
    let remote = t.remote.as_ref();
    let time_left = t.item.remaining_time;
    MovieQueueItem {
        id: hash_int31(&format!(
            "trackedDownload-{}-{}",
            t.download_client, t.item.download_id
        )),
        movie: remote.and_then(|r| r.movie.clone()),
        movie_by_id: t.movie_by_id,
        languages: remote
            .map(|r| r.languages.clone())
            .unwrap_or_else(|| vec![lang::UNKNOWN]),
        quality: remote
            .and_then(|r| r.parsed.as_ref())
            .map(|p| p.quality.quality)
            .unwrap_or_default(),
        custom_formats: remote.map(|r| r.custom_formats.clone()).unwrap_or_default(),
        size: t.item.total_size,
        title: t.item.title.clone(),
        size_left: t.item.remaining_size,
        time_left,
        estimated_completion_time: time_left.and_then(|d| now.checked_add_signed(d)),
        added: t.added,
        status: QueueStatus::from_item(t.item.status),
        tracked_download_status: Some(t.status),
        tracked_download_state: Some(t.state),
        status_messages: Some(t.status_messages.clone()),
        error_message: t.item.message.clone(),
        download_id: Some(t.item.download_id.clone()),
        remote: t.remote.clone(),
        protocol: t.protocol,
        download_client: Some(t.item.client.name.clone()),
        download_client_has_post_import_category: t.item.client.has_post_import_category,
        indexer: t.indexer.clone(),
        output_path: t.item.output_path.clone(),
    }
}

/// What the decision engine's `QueueSpecification` sees of the queue.
pub fn queued_movies(items: &[MovieQueueItem]) -> Vec<QueuedMovie> {
    items
        .iter()
        .filter_map(|item| {
            let remote = item.remote.clone()?;
            remote.movie.as_ref()?;
            Some(QueuedMovie {
                remote,
                size: item.size,
                failed_pending: item.tracked_download_state == Some(TrackedDownloadState::FailedPending),
            })
        })
        .collect()
}
