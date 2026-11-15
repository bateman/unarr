//! Radarr's `ProcessDownloadDecisions`: what to do with the decisions a
//! search or an RSS sync made. Approved releases are grabbed best first,
//! one per movie; releases held back by a delay, or whose client could not
//! be reached, become pending.

use chrono::{DateTime, Utc};

use super::pending;
use super::MovieDownloads;
use crate::downloadclients::ClientError;
use crate::downloads::pending::PendingReleaseReason;
use crate::downloads::{GrabError, ProcessedResult};
use crate::model::Protocol;
use crate::movies::decision::{self, MovieCatalog, MovieDecision};

/// `ProcessedDecisions`.
#[derive(Debug, Default)]
pub struct ProcessedMovies {
    pub grabbed: Vec<MovieDecision>,
    pub pending: Vec<MovieDecision>,
    pub rejected: Vec<MovieDecision>,
}

/// `IsQualifiedReport`: approved or only temporarily rejected, for a movie
/// of the library.
fn qualified(d: &MovieDecision) -> bool {
    (d.approved() || d.temporarily_rejected()) && d.remote.movie.is_some()
}

/// `IsMovieProcessed`.
fn movie_processed(done: &[MovieDecision], d: &MovieDecision) -> bool {
    let id = d.remote.movie.as_ref().map(|m| m.id);
    done.iter().any(|r| r.remote.movie.as_ref().map(|m| m.id) == id)
}

/// `ProcessDecisions`.
pub async fn process_decisions(
    downloads: &MovieDownloads,
    decisions: Vec<MovieDecision>,
    now: DateTime<Utc>,
) -> anyhow::Result<ProcessedMovies> {
    let qualified_reports: Vec<MovieDecision> = decisions.iter().filter(|d| qualified(d)).cloned().collect();
    let prioritized = downloads
        .db()
        .call(move |conn| {
            let catalog = MovieCatalog::load(conn, now)?;
            Ok(decision::prioritize(&catalog, qualified_reports))
        })
        .await?;
    let mut processed = ProcessedMovies {
        rejected: decisions.iter().filter(|d| d.rejected()).cloned().collect(),
        ..Default::default()
    };
    let mut to_add: Vec<(MovieDecision, PendingReleaseReason)> = Vec::new();
    let (mut usenet_failed, mut torrent_failed) = (false, false);
    for report in prioritized {
        let protocol = report.remote.release.protocol;
        if movie_processed(&processed.grabbed, &report) {
            continue;
        }
        if report.temporarily_rejected() {
            prepare_pending(&mut to_add, &mut processed, report, PendingReleaseReason::Delay);
            continue;
        }
        if (protocol == Protocol::Usenet && usenet_failed)
            || (protocol == Protocol::Torrent && torrent_failed)
        {
            prepare_pending(
                &mut to_add,
                &mut processed,
                report,
                PendingReleaseReason::DownloadClientUnavailable,
            );
            continue;
        }
        match process_internal(downloads, &report, None, now).await? {
            ProcessedResult::Grabbed => processed.grabbed.push(report),
            ProcessedResult::Pending => {
                prepare_pending(&mut to_add, &mut processed, report, PendingReleaseReason::Delay)
            }
            ProcessedResult::Rejected => processed.rejected.push(report),
            ProcessedResult::Failed => {
                match protocol {
                    Protocol::Usenet => usenet_failed = true,
                    Protocol::Torrent => torrent_failed = true,
                    Protocol::Unknown => {}
                }
                prepare_pending(
                    &mut to_add,
                    &mut processed,
                    report,
                    PendingReleaseReason::DownloadClientUnavailable,
                );
            }
            ProcessedResult::Skipped => {}
        }
    }
    if !to_add.is_empty() {
        add_pending(downloads, to_add, now).await?;
    }
    Ok(processed)
}

/// [`pending::add_many`] in one transaction: its rows, reason changes and
/// duplicate removals land together or not at all.
async fn add_pending(
    downloads: &MovieDownloads,
    items: Vec<(MovieDecision, PendingReleaseReason)>,
    now: DateTime<Utc>,
) -> anyhow::Result<bool> {
    downloads
        .db()
        .call(move |conn| {
            let tx = conn.transaction()?;
            let changed = pending::add_many(&tx, now, &items)?;
            tx.commit()?;
            Ok(changed)
        })
        .await
}

/// `ProcessDecision`: one decision, to the client asked for.
pub async fn process_decision(
    downloads: &MovieDownloads,
    decision: &MovieDecision,
    download_client_id: Option<i64>,
    now: DateTime<Utc>,
) -> anyhow::Result<ProcessedResult> {
    if !qualified(decision) {
        return Ok(ProcessedResult::Rejected);
    }
    if decision.temporarily_rejected() {
        add_pending(
            downloads,
            vec![(decision.clone(), PendingReleaseReason::Delay)],
            now,
        )
        .await?;
        return Ok(ProcessedResult::Pending);
    }
    let result = process_internal(downloads, decision, download_client_id, now).await?;
    if result == ProcessedResult::Failed {
        add_pending(
            downloads,
            vec![(decision.clone(), PendingReleaseReason::DownloadClientUnavailable)],
            now,
        )
        .await?;
    }
    Ok(result)
}

/// `PreparePending`: a release for a movie already grabbed or pending is
/// kept as a fallback.
fn prepare_pending(
    to_add: &mut Vec<(MovieDecision, PendingReleaseReason)>,
    processed: &mut ProcessedMovies,
    report: MovieDecision,
    reason: PendingReleaseReason,
) {
    let reason =
        if movie_processed(&processed.grabbed, &report) || movie_processed(&processed.pending, &report) {
            PendingReleaseReason::Fallback
        } else {
            reason
        };
    to_add.push((report.clone(), reason));
    processed.pending.push(report);
}

/// `ProcessDecisionInternal`: the grab, and what its failure means.
async fn process_internal(
    downloads: &MovieDownloads,
    decision: &MovieDecision,
    download_client_id: Option<i64>,
    now: DateTime<Utc>,
) -> anyhow::Result<ProcessedResult> {
    let remote = &decision.remote;
    tracing::trace!(
        title = remote.release.title,
        indexer = remote.release.indexer,
        priority = remote.release.indexer_priority,
        "grabbing release"
    );
    match downloads.download_report(remote, download_client_id, now).await {
        Ok(_) => Ok(ProcessedResult::Grabbed),
        Err(GrabError::ReleaseUnavailable(_)) => {
            tracing::warn!(
                title = remote.release.title,
                indexer = remote.release.indexer,
                "failed to download release: not available"
            );
            Ok(ProcessedResult::Rejected)
        }
        // A client named and not there is the original's `Single()` that
        // finds nothing: any other exception, so the release is skipped.
        // Only the want of a client for the protocol keeps it for later.
        Err(GrabError::NoClient(m)) if download_client_id.is_some() => {
            tracing::warn!(title = remote.release.title, indexer = remote.release.indexer, error = %m, "couldn't add release to download queue");
            Ok(ProcessedResult::Skipped)
        }
        Err(GrabError::NoClient(m)) => {
            tracing::debug!(title = remote.release.title, indexer = remote.release.indexer, error = %m, "failed to send release to download client, storing until later");
            Ok(ProcessedResult::Failed)
        }
        Err(GrabError::Client(ClientError::Unavailable(m) | ClientError::Authentication(m))) => {
            tracing::debug!(title = remote.release.title, indexer = remote.release.indexer, error = %m, "failed to send release to download client, storing until later");
            Ok(ProcessedResult::Failed)
        }
        // The original catches whatever the grab throws and goes on to
        // the next release: one that failed past the client (its history
        // could not be written) must not take the rest of the batch along.
        Err(GrabError::Other(e)) => {
            tracing::error!(title = remote.release.title, indexer = remote.release.indexer, error = %format!("{e:#}"), "couldn't add release to download queue");
            Ok(ProcessedResult::Skipped)
        }
        Err(e) => {
            tracing::warn!(title = remote.release.title, indexer = remote.release.indexer, error = %e.exception(), "couldn't add release to download queue");
            Ok(ProcessedResult::Skipped)
        }
    }
}
