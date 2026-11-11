//! Radarr's `PendingReleaseService`: releases the decision engine approved
//! but that cannot be grabbed yet. A delay profile holds a release back
//! for a while (`Delay`); a download client that could not be reached
//! holds it back until the next sync (`DownloadClientUnavailable`); a
//! release for a movie another pending release already covers is kept as
//! a fallback (`Fallback`). Pending releases show in the queue with an
//! estimated completion time, and go back through the decision engine
//! with every RSS sync until they are grabbed or rejected.

use chrono::{DateTime, Duration, Utc};
use rusqlite::Connection;
use serde_json::json;

use super::queue::MovieQueueItem;
use super::MovieDownloads;
use crate::decision::ReleaseSource;
use crate::downloads::pending::PendingReleaseReason;
use crate::downloads::queue::hash_int31;
use crate::indexers::rss::ReleaseInfo;
use crate::model::{Protocol, ProviderKind};
use crate::movies::decision::engine::{aggregate_languages, remote_formats};
use crate::movies::decision::{MovieCatalog, MovieDecision, MovieMatchType, RemoteMovie};
use crate::movies::model::MovieBlocklistEntry;
use crate::movies::parser::language::parse_languages;
use crate::movies::parser::parse_movie_title;
use crate::movies::store::{self, PendingRow};

/// A stored pending release.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingMovieRelease {
    pub id: i64,
    pub movie_id: i64,
    pub title: String,
    pub added: DateTime<Utc>,
    pub release: ReleaseInfo,
    pub reason: PendingReleaseReason,
    pub match_type: MovieMatchType,
    pub release_source: ReleaseSource,
}

/// A pending release with the movie it is for (`IncludeRemoteMovies`).
#[derive(Debug, Clone)]
pub struct PendingMovie {
    pub pending: PendingMovieRelease,
    pub remote: RemoteMovie,
}

/// A row as the service reads it; one whose release cannot be read (a
/// shape the original stored and this port does not know) is skipped.
fn from_row(row: PendingRow) -> Option<PendingMovieRelease> {
    let release: ReleaseInfo = match serde_json::from_str(&row.release) {
        Ok(release) => release,
        Err(e) => {
            tracing::warn!(title = row.title, error = %e, "skipping a pending release that cannot be read");
            return None;
        }
    };
    let info: serde_json::Value = row
        .additional_info
        .as_deref()
        .and_then(|text| serde_json::from_str(text).ok())
        .unwrap_or_default();
    Some(PendingMovieRelease {
        id: row.id,
        movie_id: row.movie_id,
        title: row.title,
        added: row.added,
        release,
        reason: PendingReleaseReason::from_i64(row.reason),
        match_type: info["movieMatchType"]
            .as_str()
            .and_then(MovieMatchType::parse)
            .unwrap_or(MovieMatchType::Unknown),
        release_source: info["releaseSource"]
            .as_str()
            .and_then(ReleaseSource::parse)
            .unwrap_or_default(),
    })
}

fn read(rows: Vec<PendingRow>) -> Vec<PendingMovieRelease> {
    rows.into_iter().filter_map(from_row).collect()
}

/// `IncludeRemoteMovies`: each row with its movie, its languages and its
/// custom formats. Rows of a removed movie are dropped.
fn include_remote_movies(
    conn: &Connection,
    catalog: &MovieCatalog,
    rows: Vec<PendingMovieRelease>,
) -> anyhow::Result<Vec<PendingMovie>> {
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let Some(movie) = store::get_movie(conn, row.movie_id)? else {
            continue;
        };
        let mut remote = RemoteMovie::new(row.release.clone());
        remote.match_type = row.match_type;
        remote.release_source = row.release_source;
        remote.parsed = parse_movie_title(&row.title, false).map(|mut parsed| {
            if parsed.languages.is_empty() {
                parsed.languages = parse_languages(&row.title);
            }
            parsed
        });
        remote.languages = aggregate_languages(catalog, &remote, &movie);
        remote.movie = Some(movie);
        remote.custom_formats = remote_formats(catalog, &remote, row.release.size);
        out.push(PendingMovie { pending: row, remote });
    }
    Ok(out)
}

/// `MatchingReleasePredicate`.
fn matching(p: &PendingMovieRelease, release: &ReleaseInfo) -> bool {
    p.title == release.title
        && p.release.publish_date == release.publish_date
        && p.release.indexer == release.indexer
}

/// `AddMany`: store each decision as pending unless an equal release is
/// pending for its movie, in which case the reason may change (a release
/// once held back for want of a client keeps that reason).
pub fn add_many(
    conn: &Connection,
    now: DateTime<Utc>,
    decisions: &[(MovieDecision, PendingReleaseReason)],
) -> anyhow::Result<bool> {
    let mut changed = false;
    let mut by_movie: Vec<(i64, Vec<&(MovieDecision, PendingReleaseReason)>)> = Vec::new();
    for pair in decisions {
        let Some(movie) = &pair.0.remote.movie else {
            continue;
        };
        match by_movie.iter_mut().find(|(id, _)| *id == movie.id) {
            Some((_, group)) => group.push(pair),
            None => by_movie.push((movie.id, vec![pair])),
        }
    }
    for (movie_id, group) in by_movie {
        let mut already = read(store::pending_of_movie(conn, movie_id)?);
        for (decision, reason) in group {
            let reason = *reason;
            let release = &decision.remote.release;
            let existing: Vec<usize> = already
                .iter()
                .enumerate()
                .filter(|(_, p)| matching(p, release))
                .map(|(i, _)| i)
                .collect();
            if let Some(&first) = existing.first() {
                let current = already[first].reason;
                if current != reason {
                    if current == PendingReleaseReason::DownloadClientUnavailable {
                        tracing::debug!(
                            title = release.title,
                            "release already pending for want of a client, keeping that reason"
                        );
                    } else {
                        tracing::debug!(
                            title = release.title,
                            ?current,
                            ?reason,
                            "pending release's reason changed"
                        );
                        store::pending_set_reason(conn, already[first].id, reason.as_i64())?;
                        already[first].reason = reason;
                        changed = true;
                    }
                } else {
                    tracing::debug!(
                        title = release.title,
                        ?reason,
                        "release already pending, not adding again"
                    );
                }
                for &i in existing.iter().skip(1).rev() {
                    tracing::debug!(title = release.title, "removing a duplicate pending release");
                    store::pending_delete(conn, already[i].id)?;
                    already.remove(i);
                    changed = true;
                }
                continue;
            }
            tracing::debug!(
                title = release.title,
                ?reason,
                "adding release to pending releases"
            );
            let mut row = PendingMovieRelease {
                id: 0,
                movie_id,
                title: release.title.clone(),
                added: now,
                release: release.clone(),
                reason,
                match_type: decision.remote.match_type,
                release_source: decision.remote.release_source,
            };
            row.id = store::pending_insert(
                conn,
                &PendingRow {
                    id: 0,
                    movie_id,
                    title: row.title.clone(),
                    added: now,
                    release: serde_json::to_string(&row.release)?,
                    reason: reason.as_i64(),
                    additional_info: Some(
                        json!({
                            "movieMatchType": row.match_type.as_str(),
                            "releaseSource": row.release_source.as_str(),
                        })
                        .to_string(),
                    ),
                },
            )?;
            already.push(row);
            changed = true;
        }
    }
    Ok(changed)
}

/// `GetPending`: every pending release as a report for the RSS sync,
/// those of blocked indexers left out.
pub fn get_pending(conn: &Connection, now: DateTime<Utc>) -> anyhow::Result<Vec<ReleaseInfo>> {
    let rows = read(store::pending_all(conn)?);
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let blocked: Vec<i64> = crate::store::provider_status::provider_statuses(conn, ProviderKind::Indexer)?
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
fn delay_minutes(catalog: &MovieCatalog, remote: &RemoteMovie) -> i64 {
    let tags = remote.movie.as_ref().map(|m| m.tags.clone()).unwrap_or_default();
    let delay = catalog
        .base
        .delay_profile_for(&tags)
        .map(|p| match remote.release.protocol {
            Protocol::Usenet => p.usenet_delay,
            Protocol::Torrent => p.torrent_delay,
            Protocol::Unknown => 0,
        })
        .unwrap_or(0);
    delay.max(catalog.base.config.indexer.minimum_age)
}

/// `GetQueueId`.
fn queue_id(pending_id: i64, movie_id: Option<i64>) -> i64 {
    hash_int31(&format!("pending-{}-movie{}", pending_id, movie_id.unwrap_or(0)))
}

/// `GetQueueItem`.
fn queue_item(
    conn: &Connection,
    catalog: &MovieCatalog,
    p: &PendingMovie,
    next_rss_sync: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> anyhow::Result<MovieQueueItem> {
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
            let interval =
                Duration::try_minutes(catalog.base.config.indexer.rss_sync_interval).unwrap_or(Duration::MAX);
            ect = ect.checked_add_signed(interval).unwrap_or(far);
        }
    }
    // The client the indexer names, if any.
    let mut download_client = None;
    if p.remote.release.indexer_id > 0 {
        let indexer =
            crate::store::providers::get_provider(conn, ProviderKind::Indexer, p.remote.release.indexer_id)?
                .as_ref()
                .and_then(crate::indexers::IndexerDefinition::from_provider);
        if let Some(i) = indexer.filter(|i| i.download_client_id > 0) {
            download_client = crate::store::providers::get_provider(
                conn,
                ProviderKind::DownloadClient,
                i.download_client_id,
            )?
            .map(|c| c.name);
        }
    }
    let movie = p.remote.movie.clone();
    Ok(MovieQueueItem {
        id: queue_id(p.pending.id, movie.as_ref().map(|m| m.id)),
        movie,
        movie_by_id: true,
        languages: p.remote.languages.clone(),
        quality: p
            .remote
            .parsed
            .as_ref()
            .map(|parsed| parsed.quality.quality)
            .unwrap_or_default(),
        custom_formats: p.remote.custom_formats.clone(),
        size: p.remote.release.size,
        title: p.pending.title.clone(),
        size_left: p.remote.release.size,
        time_left: Some((ect - now).max(Duration::zero())),
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

/// `GetPendingQueue`: one queue item per movie with a pending release that
/// is not a fallback, the best release kept for each.
pub fn pending_queue(conn: &Connection, now: DateTime<Utc>) -> anyhow::Result<Vec<MovieQueueItem>> {
    let rows = read(store::pending_without_fallback(
        conn,
        PendingReleaseReason::Fallback.as_i64(),
    )?);
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let catalog = MovieCatalog::load(conn, now)?;
    let next =
        crate::commands::scheduler::next_execution(conn, crate::commands::CommandBody::RssSync.type_name())?;
    let mut by_movie: Vec<(i64, Vec<MovieQueueItem>)> = Vec::new();
    for p in include_remote_movies(conn, &catalog, rows)? {
        let item = queue_item(conn, &catalog, &p, next, now)?;
        let Some(movie_id) = item.movie.as_ref().map(|m| m.id) else {
            continue;
        };
        match by_movie.iter_mut().find(|(id, _)| *id == movie_id) {
            Some((_, group)) => group.push(item),
            None => by_movie.push((movie_id, vec![item])),
        }
    }
    // The best release for each movie: highest quality, then the preferred
    // protocol.
    let mut out = Vec::new();
    for (_, mut group) in by_movie {
        let movie = group[0].movie.clone();
        let profile = movie
            .as_ref()
            .and_then(|m| catalog.base.profile(m.quality_profile_id));
        let preferred = movie
            .as_ref()
            .and_then(|m| catalog.base.delay_profile_for(&m.tags))
            .map(|d| d.preferred_protocol);
        group.sort_by(|a, b| {
            let by_quality = match profile {
                Some(p) => crate::decision::upgradable::compare_model(p, &b.quality, &a.quality),
                None => std::cmp::Ordering::Equal,
            };
            by_quality.then_with(|| {
                let rank = |q: &MovieQueueItem| if Some(q.protocol) == preferred { 0 } else { 1 };
                rank(a).cmp(&rank(b))
            })
        });
        out.push(group.remove(0));
    }
    Ok(out)
}

/// `RemovePendingQueueItems`: every pending release of the movie whose
/// title reads as the same movie title as the one the queue id names.
pub fn remove_pending_queue_items(conn: &Connection, queue_id_: i64) -> anyhow::Result<bool> {
    let all = read(store::pending_all(conn)?);
    let Some(target) = all.iter().find(|p| queue_id(p.id, Some(p.movie_id)) == queue_id_) else {
        return Ok(false);
    };
    let title_of = |p: &PendingMovieRelease| {
        parse_movie_title(&p.title, false).map(|parsed| parsed.primary_movie_title().to_string())
    };
    let target_title = title_of(target);
    for p in all.iter().filter(|p| p.movie_id == target.movie_id) {
        if title_of(p) == target_title {
            store::pending_delete(conn, p.id)?;
        }
    }
    Ok(true)
}

/// `Handle(MovieGrabbedEvent)`: pending releases of the movie at the
/// grabbed quality or below are dropped; a better one is left to be tried
/// again.
pub fn remove_grabbed(conn: &Connection, remote: &RemoteMovie, now: DateTime<Utc>) -> anyhow::Result<bool> {
    let Some(movie) = &remote.movie else {
        return Ok(false);
    };
    let rows = read(store::pending_of_movie(conn, movie.id)?);
    if rows.is_empty() {
        return Ok(false);
    }
    let catalog = MovieCatalog::load(conn, now)?;
    let profile = catalog.base.profile(movie.quality_profile_id);
    let quality_of = |r: &RemoteMovie| {
        r.parsed
            .as_ref()
            .map(|parsed| parsed.quality.quality)
            .unwrap_or_default()
    };
    let grabbed = quality_of(remote);
    let mut removed = false;
    for p in include_remote_movies(conn, &catalog, rows)? {
        let compare = match profile {
            Some(profile) => {
                crate::decision::upgradable::compare_model(profile, &grabbed, &quality_of(&p.remote))
            }
            None => std::cmp::Ordering::Equal,
        };
        if compare != std::cmp::Ordering::Less {
            tracing::debug!(
                title = p.pending.title,
                "removing previously pending release, as it was grabbed"
            );
            store::pending_delete(conn, p.pending.id)?;
            removed = true;
        }
    }
    Ok(removed)
}

/// `Handle(RssSyncCompleteEvent)`: pending releases the sync rejected are
/// dropped.
pub fn remove_rejected(conn: &Connection, rejected: &[MovieDecision]) -> anyhow::Result<bool> {
    let rows = read(store::pending_all(conn)?);
    let mut removed = false;
    for d in rejected {
        for row in rows.iter().filter(|p| matching(p, &d.remote.release)) {
            tracing::debug!(
                title = row.title,
                "removing previously pending release, as it has now been rejected"
            );
            store::pending_delete(conn, row.id)?;
            removed = true;
        }
    }
    Ok(removed)
}

/// `BlocklistService.Block(remoteMovie, message)`: a release blocklisted
/// before any download.
fn block_release(
    conn: &Connection,
    remote: &RemoteMovie,
    message: &str,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    let Some(movie) = &remote.movie else {
        return Ok(());
    };
    let parsed = remote.parsed.as_ref();
    store::insert_blocklist(
        conn,
        &MovieBlocklistEntry {
            id: store::next_blocklist_id(conn)?,
            movie_id: movie.id,
            source_title: Some(remote.release.title.clone()),
            quality: parsed.map(|p| p.quality.quality).unwrap_or_default(),
            date: now,
            published_date: Some(remote.release.publish_date),
            size: Some(remote.release.size),
            protocol: remote.release.protocol,
            indexer: Some(remote.release.indexer.clone()),
            message: Some(message.to_string()),
            // `if (remoteMovie.Release is TorrentInfo)`: without it the
            // blocklist, which matches a torrent by its hash, would not
            // see the release again.
            torrent_info_hash: (remote.release.protocol == Protocol::Torrent)
                .then(|| remote.release.info_hash.clone())
                .flatten(),
            languages: parsed.map(|p| p.languages.clone()).unwrap_or_default(),
            indexer_flags: 0,
        },
    )
}

impl MovieDownloads {
    /// The pending releases as queue items.
    pub async fn pending_queue(&self, now: DateTime<Utc>) -> anyhow::Result<Vec<MovieQueueItem>> {
        self.db().call(move |conn| pending_queue(conn, now)).await
    }

    pub async fn find_pending_queue_item(
        &self,
        id: i64,
        now: DateTime<Utc>,
    ) -> anyhow::Result<Option<MovieQueueItem>> {
        Ok(self.pending_queue(now).await?.into_iter().find(|q| q.id == id))
    }

    /// `QueueController.Remove(pendingRelease, blocklist)`.
    pub async fn remove_pending(
        &self,
        item: &MovieQueueItem,
        blocklist: bool,
        now: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        let remote = item.remote.clone();
        let id = item.id;
        self.db()
            .call(move |conn| {
                let tx = conn.transaction()?;
                if let (true, Some(remote)) = (blocklist, &remote) {
                    block_release(&tx, remote, "Pending release manually blocklisted", now)?;
                }
                remove_pending_queue_items(&tx, id)?;
                tx.commit()?;
                Ok(())
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::movies::decision::testing;
    use crate::movies::decision::{MovieRejection, MovieRejectionReason};
    use crate::Personality;

    fn decision(movie: &crate::movies::model::Movie, title: &str, quality_id: i64) -> MovieDecision {
        let published = "2026-11-10T08:00:00Z".parse().unwrap();
        let mut remote = testing::remote(
            testing::release(title, published),
            movie,
            testing::quality(quality_id, 1),
        );
        remote.parsed = parse_movie_title(title, false);
        remote.match_type = MovieMatchType::Title;
        remote.release_source = ReleaseSource::ReleasePush;
        MovieDecision {
            remote,
            rejections: vec![MovieRejection::temporary(
                MovieRejectionReason::Unknown,
                "Waiting for better quality release",
            )],
        }
    }

    #[test]
    fn pending_releases_are_kept_once_listed_best_first_and_dropped_on_a_grab() {
        let db = Db::open_in_memory_for(Personality::Radarr).unwrap();
        let now: DateTime<Utc> = "2026-11-11T12:00:00Z".parse().unwrap();
        db.call_blocking(move |conn| {
            let movie = testing::movie(13, "BlacKkKlansman", 2018);
            store::insert_metadata(conn, &movie.metadata)?;
            store::insert_movie(conn, &movie)?;
            let web = decision(&movie, "BlacKkKlansman.2018.1080p.WEB-DL.DD5.1.H264-MeM", 3);
            let bluray = decision(&movie, "BlacKkKlansman.2018.1080p.BluRay.x264-GRP", 7);
            let delay = PendingReleaseReason::Delay;
            assert!(add_many(conn, now, &[(web.clone(), delay)])?);
            // The same release again changes nothing; another reason does,
            // unless the first was the want of a client.
            assert!(!add_many(conn, now, &[(web.clone(), delay)])?);
            assert!(add_many(
                conn,
                now,
                &[(web.clone(), PendingReleaseReason::DownloadClientUnavailable)]
            )?);
            assert!(!add_many(conn, now, &[(web.clone(), delay)])?);
            assert!(add_many(
                conn,
                now,
                &[(bluray.clone(), PendingReleaseReason::Fallback)]
            )?);
            let rows = read(store::pending_all(conn)?);
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].reason, PendingReleaseReason::DownloadClientUnavailable);
            assert_eq!(rows[0].match_type, MovieMatchType::Title);
            assert_eq!(rows[0].release_source, ReleaseSource::ReleasePush);
            // Back to the RSS sync with their reason.
            let reports = get_pending(conn, now)?;
            assert_eq!(reports.len(), 2);
            assert_eq!(
                reports[0].pending_release_reason,
                Some(PendingReleaseReason::DownloadClientUnavailable)
            );
            // The queue shows one item for the movie, and not the fallback.
            let queue = pending_queue(conn, now)?;
            assert_eq!(queue.len(), 1);
            assert_eq!(queue[0].title, web.remote.release.title);
            assert_eq!(queue[0].status.as_str(), "downloadClientUnavailable");
            assert_eq!(queue[0].id, queue_id(rows[0].id, Some(13)));
            assert!(queue[0].download_id.is_none());
            // The decision engine's delay reads the oldest of them.
            assert_eq!(
                store::oldest_pending_release(conn, 13)?,
                Some(web.remote.release.publish_date)
            );
            // Blocklisted from the queue, a torrent keeps the hash the
            // blocklist will know it by; a usenet release has none.
            let mut torrent = bluray.remote.clone();
            torrent.release.protocol = Protocol::Torrent;
            torrent.release.info_hash = Some("D0C1309E36CDB92C5D7B4ABFD794F9BDFF969920".into());
            block_release(conn, &torrent, "Pending release manually blocklisted", now)?;
            block_release(conn, &web.remote, "Pending release manually blocklisted", now)?;
            let blocked = store::blocklist_of_movie(conn, 13)?;
            assert_eq!(blocked.len(), 2);
            let hashes: Vec<Option<&str>> = blocked.iter().map(|b| b.torrent_info_hash.as_deref()).collect();
            assert!(hashes.contains(&Some("D0C1309E36CDB92C5D7B4ABFD794F9BDFF969920")));
            assert!(hashes.contains(&None));
            assert!(blocked.iter().all(|b| b.indexer_flags == 0));
            // What the sync rejects goes.
            assert!(remove_rejected(conn, std::slice::from_ref(&bluray))?);
            let left = read(store::pending_all(conn)?);
            assert_eq!(left.len(), 1);
            assert_eq!(left[0].title, web.remote.release.title);
            // And so does what a grab of the same quality makes useless.
            assert!(remove_grabbed(conn, &web.remote, now)?);
            assert!(store::pending_all(conn)?.is_empty());
            assert!(pending_queue(conn, now)?.is_empty());
            Ok(())
        })
        .unwrap();
    }
}
