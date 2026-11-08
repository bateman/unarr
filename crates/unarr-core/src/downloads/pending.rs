//! `PendingReleaseService`: releases the decision engine approved but
//! that cannot be grabbed yet. A delay profile holds a release back for a
//! while (`Delay`); a download client that could not be reached holds it
//! back until the next sync (`DownloadClientUnavailable`); a release for
//! episodes another pending release already covers is kept as a fallback
//! (`Fallback`). Pending releases show in the queue with an estimated
//! completion time, and go back through the decision engine with every
//! RSS sync until they are grabbed or rejected.

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use rusqlite::Connection;

use super::queue::{hash_int31, QueueItem, QueueStatus};
use super::Downloads;
use crate::decision::lookup::{Catalog, Lookup, StoreLookup};
use crate::decision::{engine, mapping, Decision, ReleaseSource, RemoteEpisode, SeriesMatchType};
use crate::indexers::rss::ReleaseInfo;
use crate::model::{Episode, Protocol, ProviderKind, Series};
use crate::parser;
use crate::store;

/// `PendingReleaseReason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PendingReleaseReason {
    Delay,
    DownloadClientUnavailable,
    Fallback,
}

impl PendingReleaseReason {
    pub fn as_i64(self) -> i64 {
        match self {
            PendingReleaseReason::Delay => 0,
            PendingReleaseReason::DownloadClientUnavailable => 1,
            PendingReleaseReason::Fallback => 2,
        }
    }

    pub fn from_i64(n: i64) -> PendingReleaseReason {
        match n {
            1 => PendingReleaseReason::DownloadClientUnavailable,
            2 => PendingReleaseReason::Fallback,
            _ => PendingReleaseReason::Delay,
        }
    }

    /// The queue status of the same name.
    pub fn queue_status(self) -> QueueStatus {
        match self {
            PendingReleaseReason::Delay => QueueStatus::Delay,
            PendingReleaseReason::DownloadClientUnavailable => QueueStatus::DownloadClientUnavailable,
            PendingReleaseReason::Fallback => QueueStatus::Fallback,
        }
    }
}

/// A stored pending release.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingRelease {
    pub id: i64,
    pub series_id: i64,
    pub title: String,
    pub added: DateTime<Utc>,
    pub release: ReleaseInfo,
    pub reason: PendingReleaseReason,
    pub series_match_type: SeriesMatchType,
    pub release_source: ReleaseSource,
}

/// A pending release with the series and episodes it maps to
/// (`IncludeRemoteEpisodes`).
#[derive(Debug, Clone)]
pub struct PendingRemote {
    pub pending: PendingRelease,
    pub remote: RemoteEpisode,
}

/// `IncludeRemoteEpisodes`: map each row back to its series and episodes;
/// `known` carries the remote episodes of releases decided in this batch,
/// whose mapping is reused. Rows of a deleted series are dropped.
pub fn include_remote_episodes(
    catalog: &Catalog,
    lookup: &dyn Lookup,
    now: DateTime<Utc>,
    rows: Vec<PendingRelease>,
    known: &HashMap<String, RemoteEpisode>,
) -> Vec<PendingRemote> {
    let mut out = Vec::with_capacity(rows.len());
    let mut series_cache: HashMap<i64, Option<Series>> = HashMap::new();
    for row in rows {
        let series = series_cache
            .entry(row.series_id)
            .or_insert_with(|| {
                known
                    .values()
                    .filter_map(|r| r.series.clone())
                    .find(|s| s.id == row.series_id)
                    .or_else(|| lookup.series_by_id(row.series_id))
            })
            .clone();
        let Some(series) = series else {
            continue;
        };
        let mut parsed = parser::parse_title(&row.title).unwrap_or_default();
        if parsed.languages.is_empty() {
            parsed.languages = parser::language::parse_languages(&row.title);
        }
        let mut remote = RemoteEpisode::new(row.release.clone());
        remote.series_match_type = row.series_match_type;
        remote.release_source = row.release_source;
        remote.parsed = Some(parsed.clone());
        if let Some(k) = known.get(&row.release.title) {
            remote.mapped_season_number = k.mapped_season_number;
            remote.episodes = k.episodes.clone();
            remote.series = Some(series);
        } else if mapping::validate_for_series_type(&parsed, &series) {
            match mapping::map_to_series(lookup, row.release.clone(), parsed.clone(), series.clone()) {
                Ok(mapped) => {
                    remote.mapped_season_number = mapped.mapped_season_number;
                    remote.episodes = mapped.episodes;
                    remote.series = mapped.series;
                }
                Err(e) => {
                    tracing::debug!(title = row.title, error = %e, "pending release maps to no episodes");
                    remote.mapped_season_number = parsed.season_number;
                    remote.series = Some(series);
                }
            }
        } else {
            remote.mapped_season_number = parsed.season_number;
            remote.series = Some(series);
        }
        let size = row.release.size;
        engine::augment_tracked(catalog, lookup, now, &mut remote, size);
        out.push(PendingRemote { pending: row, remote });
    }
    out
}

fn matching(p: &PendingRelease, release: &ReleaseInfo) -> bool {
    p.title == release.title
        && p.release.publish_date == release.publish_date
        && p.release.indexer == release.indexer
}

/// `AddMany`: store each decision as pending unless an equal release is
/// pending for one of its episodes, in which case the reason may change
/// (a release once held back for want of a client keeps that reason).
pub fn add_many(
    conn: &Connection,
    now: DateTime<Utc>,
    decisions: &[(Decision, PendingReleaseReason)],
) -> anyhow::Result<bool> {
    let catalog = Catalog::load(conn, now)?;
    let lookup = StoreLookup::new(conn);
    let mut changed = false;
    let mut by_series: Vec<(i64, Vec<&(Decision, PendingReleaseReason)>)> = Vec::new();
    for pair in decisions {
        let Some(series) = &pair.0.remote.series else {
            continue;
        };
        match by_series.iter_mut().find(|(id, _)| *id == series.id) {
            Some((_, group)) => group.push(pair),
            None => by_series.push((series.id, vec![pair])),
        }
    }
    for (series_id, group) in by_series {
        let known: HashMap<String, RemoteEpisode> = group
            .iter()
            .map(|(d, _)| (d.remote.release.title.clone(), d.remote.clone()))
            .collect();
        let mut already = include_remote_episodes(
            &catalog,
            &lookup,
            now,
            store::pending::all_by_series(conn, series_id)?,
            &known,
        );
        for (decision, reason) in group {
            let reason = *reason;
            let episode_ids: Vec<i64> = decision.remote.episodes.iter().map(|e| e.id).collect();
            let existing: Vec<usize> = already
                .iter()
                .enumerate()
                .filter(|(_, p)| p.remote.episodes.iter().any(|e| episode_ids.contains(&e.id)))
                .filter(|(_, p)| matching(&p.pending, &decision.remote.release))
                .map(|(i, _)| i)
                .collect();
            if let Some(&first) = existing.first() {
                let current = already[first].pending.reason;
                if current != reason {
                    if current == PendingReleaseReason::DownloadClientUnavailable {
                        tracing::debug!(
                            title = decision.remote.release.title,
                            "release already pending for want of a client, keeping that reason"
                        );
                    } else {
                        tracing::debug!(
                            title = decision.remote.release.title,
                            ?current,
                            ?reason,
                            "pending release's reason changed"
                        );
                        store::pending::set_reason(conn, already[first].pending.id, reason)?;
                        already[first].pending.reason = reason;
                        changed = true;
                    }
                } else {
                    tracing::debug!(
                        title = decision.remote.release.title,
                        ?reason,
                        "release already pending, not adding again"
                    );
                }
                if existing.len() > 1 {
                    tracing::debug!(
                        title = decision.remote.release.title,
                        duplicates = existing.len() - 1,
                        "removing duplicate pending releases"
                    );
                    for &i in existing.iter().skip(1).rev() {
                        store::pending::delete(conn, already[i].pending.id)?;
                        already.remove(i);
                        changed = true;
                    }
                }
                continue;
            }
            tracing::debug!(
                title = decision.remote.release.title,
                ?reason,
                "adding release to pending releases"
            );
            let row = PendingRelease {
                id: 0,
                series_id,
                title: decision.remote.release.title.clone(),
                added: now,
                release: decision.remote.release.clone(),
                reason,
                series_match_type: decision.remote.series_match_type,
                release_source: decision.remote.release_source,
            };
            let id = store::pending::insert(conn, &row)?;
            already.push(PendingRemote {
                pending: PendingRelease { id, ..row },
                remote: decision.remote.clone(),
            });
            changed = true;
        }
    }
    Ok(changed)
}

/// `GetPending`: every pending release as a report for the RSS sync,
/// those of blocked indexers left out.
pub fn get_pending(conn: &Connection, now: DateTime<Utc>) -> anyhow::Result<Vec<ReleaseInfo>> {
    let rows = store::pending::all(conn)?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let blocked: Vec<i64> = store::provider_status::provider_statuses(conn, ProviderKind::Indexer)?
        .into_iter()
        .filter(|(_, s)| s.is_disabled(now))
        .map(|(id, _)| id)
        .collect();
    Ok(rows
        .into_iter()
        .filter(|p| !blocked.contains(&p.release.indexer_id))
        .map(|p| ReleaseInfo {
            pending_release_reason: Some(p.reason),
            ..p.release
        })
        .collect())
}

/// `GetDelay`: the delay profile's delay for the protocol, or the minimum
/// age, whichever is longer, in minutes.
fn delay_minutes(catalog: &Catalog, remote: &RemoteEpisode) -> i64 {
    let tags = remote.series.as_ref().map(|s| s.tags.clone()).unwrap_or_default();
    let delay = catalog
        .delay_profile_for(&tags)
        .map(|p| match remote.release.protocol {
            Protocol::Usenet => p.usenet_delay,
            Protocol::Torrent => p.torrent_delay,
            Protocol::Unknown => 0,
        })
        .unwrap_or(0);
    delay.max(catalog.config.indexer.minimum_age)
}

/// `TaskManager.GetNextExecution(RssSyncCommand)`.
fn next_rss_sync(conn: &Connection) -> anyhow::Result<Option<DateTime<Utc>>> {
    crate::commands::scheduler::next_execution(conn, crate::commands::CommandBody::RssSync.type_name())
}

fn queue_id(pending_id: i64, episode: Option<&Episode>) -> i64 {
    hash_int31(&format!(
        "pending-{}-ep{}",
        pending_id,
        episode.map(|e| e.id).unwrap_or(0)
    ))
}

fn queue_item(
    conn: &Connection,
    catalog: &Catalog,
    p: &PendingRemote,
    next_rss_sync: Option<DateTime<Utc>>,
    rss_sync_interval: i64,
    episode: Option<&Episode>,
    now: DateTime<Utc>,
) -> anyhow::Result<QueueItem> {
    // A delay or an interval too long for a date saturates rather than panics.
    let far = DateTime::<Utc>::MAX_UTC;
    let delay = Duration::try_minutes(delay_minutes(catalog, &p.remote)).unwrap_or(Duration::MAX);
    let mut ect = p
        .remote
        .release
        .publish_date
        .checked_add_signed(delay)
        .unwrap_or(far);
    match next_rss_sync {
        Some(next) if ect < next => ect = next,
        _ => {
            let interval = Duration::try_minutes(rss_sync_interval).unwrap_or(Duration::MAX);
            ect = ect.checked_add_signed(interval).unwrap_or(far);
        }
    }
    let time_left = (ect - now).max(Duration::zero());
    // The client the indexer names, if any.
    let mut download_client = None;
    if p.remote.release.indexer_id > 0 {
        let indexer =
            store::providers::get_provider(conn, ProviderKind::Indexer, p.remote.release.indexer_id)?
                .as_ref()
                .and_then(crate::indexers::IndexerDefinition::from_provider);
        if let Some(i) = indexer.filter(|i| i.download_client_id > 0) {
            download_client =
                store::providers::get_provider(conn, ProviderKind::DownloadClient, i.download_client_id)?
                    .map(|c| c.name);
        }
    }
    let parsed = p.remote.parsed_or_default().into_owned();
    Ok(QueueItem {
        id: queue_id(p.pending.id, episode),
        series: p.remote.series.clone(),
        episode: episode.cloned(),
        languages: p.remote.languages.clone(),
        quality: parsed.quality.quality,
        custom_formats: p.remote.custom_formats.clone(),
        custom_format_score: p.remote.custom_format_score,
        size: p.remote.release.size,
        title: p.pending.title.clone(),
        size_left: p.remote.release.size,
        time_left: Some(time_left),
        estimated_completion_time: Some(ect),
        added: Some(p.pending.added),
        status: p.pending.reason.queue_status(),
        tracked_download_status: None,
        tracked_download_state: None,
        status_messages: None,
        error_message: None,
        download_id: None,
        remote: Some(p.remote.clone()),
        protocol: p.remote.release.protocol,
        download_client,
        download_client_has_post_import_category: false,
        indexer: Some(p.remote.release.indexer.clone()).filter(|i| !i.is_empty()),
        output_path: None,
    })
}

/// `GetPendingQueue`: one queue item per episode of each pending release
/// that is not a fallback, the best release kept for each episode.
pub fn pending_queue(conn: &Connection, now: DateTime<Utc>) -> anyhow::Result<Vec<QueueItem>> {
    let rows = store::pending::without_fallback(conn)?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let catalog = Catalog::load(conn, now)?;
    let lookup = StoreLookup::new(conn);
    let next = next_rss_sync(conn)?;
    let interval = catalog.config.indexer.rss_sync_interval;
    let mut queued = Vec::new();
    for p in include_remote_episodes(&catalog, &lookup, now, rows, &HashMap::new()) {
        if p.remote.episodes.is_empty() {
            let mut item = queue_item(conn, &catalog, &p, next, interval, None, now)?;
            item.error_message = Some("Unable to find matching episode(s)".into());
            queued.push(item);
            continue;
        }
        for episode in p.remote.episodes.clone() {
            queued.push(queue_item(
                conn,
                &catalog,
                &p,
                next,
                interval,
                Some(&episode),
                now,
            )?);
        }
    }
    // The best release for each episode: highest quality, then the
    // preferred protocol.
    let mut by_episode: Vec<(i64, Vec<QueueItem>)> = Vec::new();
    for item in queued {
        let Some(episode) = &item.episode else {
            continue;
        };
        match by_episode.iter_mut().find(|(id, _)| *id == episode.id) {
            Some((_, group)) => group.push(item),
            None => by_episode.push((episode.id, vec![item])),
        }
    }
    let mut out = Vec::new();
    for (_, mut group) in by_episode {
        let series = group[0].series.clone();
        let profile = series
            .as_ref()
            .and_then(|s| catalog.profile(s.quality_profile_id));
        let preferred = series
            .as_ref()
            .and_then(|s| catalog.delay_profile_for(&s.tags))
            .map(|d| d.preferred_protocol);
        group.sort_by(|a, b| {
            let by_quality = match profile {
                Some(p) => crate::decision::upgradable::compare_model(p, &b.quality, &a.quality),
                None => std::cmp::Ordering::Equal,
            };
            by_quality.then_with(|| {
                let rank = |q: &QueueItem| if Some(q.protocol) == preferred { 0 } else { 1 };
                rank(a).cmp(&rank(b))
            })
        });
        out.push(group.remove(0));
    }
    Ok(out)
}

pub fn find_pending_queue_item(
    conn: &Connection,
    now: DateTime<Utc>,
    id: i64,
) -> anyhow::Result<Option<QueueItem>> {
    Ok(pending_queue(conn, now)?.into_iter().find(|q| q.id == id))
}

/// `RemovePendingQueueItems`: every pending release of the series for the
/// same season and episode numbers as the one the queue id names.
pub fn remove_pending_queue_items(
    conn: &Connection,
    now: DateTime<Utc>,
    queue_id_: i64,
) -> anyhow::Result<bool> {
    let catalog = Catalog::load(conn, now)?;
    let lookup = StoreLookup::new(conn);
    let all = include_remote_episodes(
        &catalog,
        &lookup,
        now,
        store::pending::all(conn)?,
        &HashMap::new(),
    );
    let Some(target) = all.iter().find(|p| {
        p.remote
            .episodes
            .iter()
            .any(|e| queue_id(p.pending.id, Some(e)) == queue_id_)
    }) else {
        return Ok(false);
    };
    let target_parsed = target.remote.parsed_or_default().into_owned();
    let to_remove: Vec<i64> = all
        .iter()
        .filter(|p| p.pending.series_id == target.pending.series_id)
        .filter(|p| {
            let parsed = p.remote.parsed_or_default();
            parsed.season_number == target_parsed.season_number
                && parsed.episode_numbers == target_parsed.episode_numbers
        })
        .map(|p| p.pending.id)
        .collect();
    for id in to_remove {
        store::pending::delete(conn, id)?;
    }
    Ok(true)
}

/// `OldestPendingRelease`: the age in hours of the oldest pending release
/// for any of the episodes, for the delay specification.
pub fn oldest_pending_release_age_hours(
    lookup: &dyn Lookup,
    rows: Vec<PendingRelease>,
    episode_ids: &[i64],
    now: DateTime<Utc>,
) -> Option<f64> {
    let mut oldest: Option<f64> = None;
    for row in rows {
        let series = lookup.series_by_id(row.series_id)?;
        let parsed = parser::parse_title(&row.title).unwrap_or_default();
        let episodes = if mapping::validate_for_series_type(&parsed, &series) {
            mapping::map_to_series(lookup, row.release.clone(), parsed, series)
                .map(|r| r.episodes)
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        if episodes.iter().any(|e| episode_ids.contains(&e.id)) {
            let hours = (now - row.release.publish_date).num_milliseconds() as f64 / 3_600_000.0;
            oldest = Some(oldest.map_or(hours, |o: f64| o.max(hours)));
        }
    }
    oldest
}

/// `Handle(EpisodeGrabbedEvent)`: pending releases of the same episodes
/// at the grabbed quality or below are dropped.
pub fn remove_grabbed(conn: &Connection, remote: &RemoteEpisode, now: DateTime<Utc>) -> anyhow::Result<bool> {
    let Some(series) = &remote.series else {
        return Ok(false);
    };
    let rows = store::pending::all_by_series(conn, series.id)?;
    if rows.is_empty() {
        return Ok(false);
    }
    let catalog = Catalog::load(conn, now)?;
    let lookup = StoreLookup::new(conn);
    let episode_ids: Vec<i64> = remote.episodes.iter().map(|e| e.id).collect();
    let profile = catalog.profile(series.quality_profile_id);
    let grabbed = remote.parsed_or_default().quality.quality;
    let mut removed = false;
    for p in include_remote_episodes(&catalog, &lookup, now, rows, &HashMap::new()) {
        if !p.remote.episodes.iter().any(|e| episode_ids.contains(&e.id)) {
            continue;
        }
        let existing = p.remote.parsed_or_default().quality.quality;
        let compare = match profile {
            Some(profile) => crate::decision::upgradable::compare_model(profile, &grabbed, &existing),
            None => std::cmp::Ordering::Equal,
        };
        if compare != std::cmp::Ordering::Less {
            tracing::debug!(
                title = p.pending.title,
                "removing previously pending release, as it was grabbed"
            );
            store::pending::delete(conn, p.pending.id)?;
            removed = true;
        }
    }
    Ok(removed)
}

/// `Handle(RssSyncCompleteEvent)`: pending releases the sync rejected are
/// dropped.
pub fn remove_rejected(conn: &Connection, rejected: &[Decision]) -> anyhow::Result<bool> {
    let rows = store::pending::all(conn)?;
    let mut removed = false;
    for d in rejected {
        for row in rows.iter().filter(|p| matching(p, &d.remote.release)) {
            tracing::debug!(
                title = row.title,
                "removing previously pending release, as it has now been rejected"
            );
            store::pending::delete(conn, row.id)?;
            removed = true;
        }
    }
    Ok(removed)
}

impl Downloads {
    /// The pending releases as queue items.
    pub async fn pending_queue(&self, now: DateTime<Utc>) -> anyhow::Result<Vec<QueueItem>> {
        self.db().call(move |conn| pending_queue(conn, now)).await
    }

    pub async fn find_pending_queue_item(
        &self,
        id: i64,
        now: DateTime<Utc>,
    ) -> anyhow::Result<Option<QueueItem>> {
        self.db()
            .call(move |conn| find_pending_queue_item(conn, now, id))
            .await
    }

    /// `QueueController.Remove(pendingRelease, blocklist)`.
    pub async fn remove_pending(
        &self,
        item: &QueueItem,
        blocklist: bool,
        now: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        if blocklist {
            if let Some(remote) = &item.remote {
                super::failed::block_release(self, remote, "Pending release manually blocklisted", now)
                    .await?;
            }
        }
        let id = item.id;
        self.db()
            .call(move |conn| remove_pending_queue_items(conn, now, id))
            .await?;
        Ok(())
    }
}
