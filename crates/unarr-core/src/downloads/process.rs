//! `ProcessDownloadDecisions`: what to do with the decisions a search or
//! an RSS sync made. Approved releases are grabbed best first, one per
//! episode; releases held back by a delay, or whose client could not be
//! reached, become pending.

use chrono::{DateTime, Utc};

use super::grab::GrabError;
use super::pending::{self, PendingReleaseReason};
use super::Downloads;
use crate::decision::{self, Decision};
use crate::downloadclients::ClientError;
use crate::model::Protocol;

/// `ProcessedDecisions`.
#[derive(Debug, Default)]
pub struct Processed {
    pub grabbed: Vec<Decision>,
    pub pending: Vec<Decision>,
    pub rejected: Vec<Decision>,
}

/// `ProcessedDecisionResult`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessedResult {
    Grabbed,
    Pending,
    Rejected,
    Failed,
    Skipped,
}

/// `IsQualifiedReport`: approved or only temporarily rejected, with
/// episodes to grab for.
fn qualified(d: &Decision) -> bool {
    (d.approved() || d.temporarily_rejected()) && !d.remote.episodes.is_empty()
}

fn episode_processed(done: &[Decision], d: &Decision) -> bool {
    let ids: Vec<i64> = d.remote.episodes.iter().map(|e| e.id).collect();
    done.iter()
        .flat_map(|r| r.remote.episodes.iter())
        .any(|e| ids.contains(&e.id))
}

/// `ProcessDecisions`.
pub async fn process_decisions(
    downloads: &Downloads,
    decisions: Vec<Decision>,
    now: DateTime<Utc>,
) -> anyhow::Result<Processed> {
    let qualified_reports: Vec<Decision> = decisions.iter().filter(|d| qualified(d)).cloned().collect();
    let prioritized = downloads
        .db()
        .call(move |conn| {
            let catalog = decision::Catalog::load(conn, now)?;
            Ok(decision::prioritize(&catalog, now, qualified_reports))
        })
        .await?;
    let mut processed = Processed {
        rejected: decisions.iter().filter(|d| d.rejected()).cloned().collect(),
        ..Default::default()
    };
    let mut to_add: Vec<(Decision, PendingReleaseReason)> = Vec::new();
    let (mut usenet_failed, mut torrent_failed) = (false, false);
    for report in prioritized {
        let protocol = report.remote.release.protocol;
        if episode_processed(&processed.grabbed, &report) {
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
        downloads
            .db()
            .call(move |conn| add_pending(conn, now, &to_add))
            .await?;
    }
    Ok(processed)
}

/// [`pending::add_many`] in one transaction: its rows, reason changes and
/// duplicate removals land together or not at all.
fn add_pending(
    conn: &mut rusqlite::Connection,
    now: DateTime<Utc>,
    items: &[(Decision, PendingReleaseReason)],
) -> anyhow::Result<bool> {
    let tx = conn.transaction()?;
    let changed = pending::add_many(&tx, now, items)?;
    tx.commit()?;
    Ok(changed)
}

/// `ProcessDecision`: one decision, to the client asked for.
pub async fn process_decision(
    downloads: &Downloads,
    decision: &Decision,
    download_client_id: Option<i64>,
    now: DateTime<Utc>,
) -> anyhow::Result<ProcessedResult> {
    if !qualified(decision) {
        return Ok(ProcessedResult::Rejected);
    }
    if decision.temporarily_rejected() {
        let d = decision.clone();
        downloads
            .db()
            .call(move |conn| add_pending(conn, now, &[(d, PendingReleaseReason::Delay)]))
            .await?;
        return Ok(ProcessedResult::Pending);
    }
    let result = process_internal(downloads, decision, download_client_id, now).await?;
    if result == ProcessedResult::Failed {
        let d = decision.clone();
        downloads
            .db()
            .call(move |conn| add_pending(conn, now, &[(d, PendingReleaseReason::DownloadClientUnavailable)]))
            .await?;
    }
    Ok(result)
}

/// `PreparePending`: a release for episodes already grabbed or pending is
/// kept as a fallback.
fn prepare_pending(
    to_add: &mut Vec<(Decision, PendingReleaseReason)>,
    processed: &mut Processed,
    report: Decision,
    reason: PendingReleaseReason,
) {
    let reason =
        if episode_processed(&processed.grabbed, &report) || episode_processed(&processed.pending, &report) {
            PendingReleaseReason::Fallback
        } else {
            reason
        };
    to_add.push((report.clone(), reason));
    processed.pending.push(report);
}

/// `ProcessDecisionInternal`: the grab, and what its failure means.
async fn process_internal(
    downloads: &Downloads,
    decision: &Decision,
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
        Err(GrabError::NoClient(m)) => {
            tracing::debug!(title = remote.release.title, indexer = remote.release.indexer, error = %m, "failed to send release to download client, storing until later");
            Ok(ProcessedResult::Failed)
        }
        Err(GrabError::Client(ClientError::Unavailable(m) | ClientError::Authentication(m))) => {
            tracing::debug!(title = remote.release.title, indexer = remote.release.indexer, error = %m, "failed to send release to download client, storing until later");
            Ok(ProcessedResult::Failed)
        }
        Err(GrabError::Other(e)) => Err(e),
        Err(e) => {
            tracing::warn!(title = remote.release.title, indexer = remote.release.indexer, error = %e, "couldn't add release to download queue");
            Ok(ProcessedResult::Skipped)
        }
    }
}
