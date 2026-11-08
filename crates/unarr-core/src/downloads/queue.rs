//! `Queue` items, after the originals' `QueueService`: one per episode of a
//! tracked download, with the id the UI addresses it by.

use chrono::{DateTime, Duration, Utc};
use ring::digest::{digest, SHA1_FOR_LEGACY_USE_ONLY};

use super::tracked::{StatusMessage, TrackedDownload, TrackedDownloadState, TrackedDownloadStatus};
use crate::decision::lookup::QueuedRelease;
use crate::decision::RemoteEpisode;
use crate::downloadclients::DownloadItemStatus;
use crate::model::{CustomFormat, Episode, Protocol, QualityModel, Series};
use crate::parser::remove_file_extension;

/// `QueueStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueStatus {
    Unknown,
    Queued,
    Paused,
    Downloading,
    Completed,
    Failed,
    Warning,
    Delay,
    DownloadClientUnavailable,
    Fallback,
}

impl QueueStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            QueueStatus::Unknown => "unknown",
            QueueStatus::Queued => "queued",
            QueueStatus::Paused => "paused",
            QueueStatus::Downloading => "downloading",
            QueueStatus::Completed => "completed",
            QueueStatus::Failed => "failed",
            QueueStatus::Warning => "warning",
            QueueStatus::Delay => "delay",
            QueueStatus::DownloadClientUnavailable => "downloadClientUnavailable",
            QueueStatus::Fallback => "fallback",
        }
    }

    pub fn parse(s: &str) -> Option<QueueStatus> {
        [
            QueueStatus::Unknown,
            QueueStatus::Queued,
            QueueStatus::Paused,
            QueueStatus::Downloading,
            QueueStatus::Completed,
            QueueStatus::Failed,
            QueueStatus::Warning,
            QueueStatus::Delay,
            QueueStatus::DownloadClientUnavailable,
            QueueStatus::Fallback,
        ]
        .into_iter()
        .find(|q| q.as_str().eq_ignore_ascii_case(s))
    }

    fn from_item(status: DownloadItemStatus) -> QueueStatus {
        match status {
            DownloadItemStatus::Queued => QueueStatus::Queued,
            DownloadItemStatus::Paused => QueueStatus::Paused,
            DownloadItemStatus::Downloading => QueueStatus::Downloading,
            DownloadItemStatus::Completed => QueueStatus::Completed,
            DownloadItemStatus::Failed => QueueStatus::Failed,
            DownloadItemStatus::Warning => QueueStatus::Warning,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct QueueItem {
    pub id: i64,
    pub series: Option<Series>,
    pub episode: Option<Episode>,
    pub languages: Vec<i64>,
    pub quality: QualityModel,
    pub custom_formats: Vec<CustomFormat>,
    pub custom_format_score: i64,
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
    pub remote: Option<RemoteEpisode>,
    pub protocol: Protocol,
    pub download_client: Option<String>,
    pub download_client_has_post_import_category: bool,
    pub indexer: Option<String>,
    pub output_path: Option<String>,
}

impl QueueItem {
    /// `100 - sizeLeft / size * 100`, the progress the originals sort by.
    pub fn progress(&self) -> f64 {
        if self.size == 0 {
            return 0.0;
        }
        100.0 - (self.size_left as f64 / self.size as f64 * 100.0)
    }
}

/// `HashConverter.GetHashInt31`: the first four bytes of the SHA-1 of the
/// text, little-endian, without the sign bit.
pub fn hash_int31(text: &str) -> i64 {
    let hash = digest(&SHA1_FOR_LEGACY_USE_ONLY, text.as_bytes());
    let b = hash.as_ref();
    let n = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    (n & 0x7fff_ffff) as i64
}

/// `TimeleftComparer`: unknown times sort last.
pub fn compare_time_left(x: Option<Duration>, y: Option<Duration>) -> std::cmp::Ordering {
    match (x, y) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (Some(_), None) => std::cmp::Ordering::Less,
        (Some(a), Some(b)) => a.cmp(&b),
    }
}

/// `MapQueue` over the trackable downloads, by remaining time.
pub fn build(tracked: &[&TrackedDownload], now: DateTime<Utc>) -> Vec<QueueItem> {
    let mut list: Vec<&TrackedDownload> = tracked.iter().copied().filter(|t| t.is_trackable).collect();
    list.sort_by(|a, b| compare_time_left(a.item.remaining_time, b.item.remaining_time));
    let mut out = Vec::new();
    for t in list {
        let episodes: Vec<Option<&Episode>> = match &t.remote {
            Some(r) if !r.episodes.is_empty() => r.episodes.iter().map(Some).collect(),
            _ => vec![None],
        };
        for episode in episodes {
            out.push(map_item(t, episode, now));
        }
    }
    out
}

/// `MapQueueItem`.
fn map_item(t: &TrackedDownload, episode: Option<&Episode>, now: DateTime<Utc>) -> QueueItem {
    let remote = t.remote.as_ref();
    let parsed = remote.and_then(|r| r.parsed.as_ref());
    let time_left = t.item.remaining_time;
    let id = hash_int31(&format!(
        "trackedDownload-{}-{}-ep{}",
        t.download_client,
        t.item.download_id,
        episode.map(|e| e.id).unwrap_or(0)
    ));
    QueueItem {
        id,
        series: remote.and_then(|r| r.series.clone()),
        episode: episode.cloned(),
        languages: remote
            .map(|r| r.languages.clone())
            .filter(|l| !l.is_empty())
            .unwrap_or_else(|| vec![0]),
        quality: parsed.map(|p| p.quality.quality).unwrap_or_default(),
        custom_formats: remote.map(|r| r.custom_formats.clone()).unwrap_or_default(),
        custom_format_score: remote.map(|r| r.custom_format_score).unwrap_or(0),
        size: t.item.total_size,
        title: remove_file_extension(&t.item.title),
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

/// What the decision engine's `QueueSpecification` sees of the queue: one
/// entry per download (the originals' `QueueService` list).
pub fn queued_releases(items: &[QueueItem]) -> Vec<QueuedRelease> {
    let mut by_download: Vec<QueuedRelease> = Vec::new();
    for item in items {
        let Some(remote) = &item.remote else {
            continue;
        };
        let Some(series) = &remote.series else {
            continue;
        };
        if by_download
            .iter()
            .any(|q| q.title == remote.release.title && q.series_id == series.id)
        {
            continue;
        }
        let parsed = remote.parsed.as_ref();
        by_download.push(QueuedRelease {
            series_id: series.id,
            episode_ids: remote.episodes.iter().map(|e| e.id).collect(),
            title: remote.release.title.clone(),
            quality: parsed.map(|p| p.quality.quality).unwrap_or_default(),
            languages: remote.languages.clone(),
            size: item.size,
            indexer_flags: remote.release.indexer_flags,
            release_type: parsed
                .map(|p| p.release_type())
                .unwrap_or(crate::model::ReleaseType::Unknown),
            failed_pending: item.tracked_download_state == Some(TrackedDownloadState::FailedPending),
        });
    }
    by_download
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_as_the_originals_do() {
        // SHA-1("trackedDownload-4-SABnzbd_nzo_mock1-ep1") = 2e 5e 33 1d ...
        assert_eq!(hash_int31("trackedDownload-4-SABnzbd_nzo_mock1-ep1"), 489905582);
        assert_eq!(remove_file_extension("The.Pitt.S01E01.mkv"), "The.Pitt.S01E01");
        assert_eq!(remove_file_extension("The.Pitt.S01E01.nzb"), "The.Pitt.S01E01");
        assert_eq!(
            remove_file_extension("The.Pitt.S01E01-FLUX"),
            "The.Pitt.S01E01-FLUX"
        );
        assert_eq!(remove_file_extension("Show.2024"), "Show.2024");
    }
}
