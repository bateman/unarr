//! `HousekeepingService`: the daily clean-up of rows nothing refers to any
//! more, then a `VACUUM`. One housekeeper failing does not stop the others.
//!
//! Ported from `NzbDrone.Core/Housekeeping/Housekeepers`, those that have a
//! table here: the users beyond the first, the orphaned blocklist, episode
//! files, episodes, history and pending releases, the subtitles and other
//! extras, the provider statuses of providers that went, the pending
//! releases a client or a fallback left for two weeks, status times and
//! scheduled tasks' last runs in the future, and the tags nothing uses.
//! Not ported: the metadata file cleaners (no such files),
//! the update's
//! temporary files, the log database (the log is in memory), the repair of
//! profiles' format items and of clean titles (both are written whole by
//! the code that owns them), and the removal of bad covers.

use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, Connection};

use crate::indexers::status::PERIODS;
use crate::time;

/// What one run removed or repaired, by housekeeper.
pub type Report = Vec<(&'static str, usize)>;

type Housekeeper = fn(&Connection, DateTime<Utc>) -> anyhow::Result<usize>;

const HOUSEKEEPERS: [(&str, Housekeeper); 12] = [
    ("CleanupAdditionalUsers", additional_users),
    ("CleanupOrphanedBlocklist", orphaned_blocklist),
    ("CleanupOrphanedEpisodeFiles", orphaned_episode_files),
    ("CleanupOrphanedEpisodes", orphaned_episodes),
    ("CleanupOrphanedExtraFiles", orphaned_extra_files),
    ("CleanupOrphanedHistoryItems", orphaned_history),
    ("CleanupOrphanedPendingReleases", orphaned_pending_releases),
    (
        "CleanupDownloadClientUnavailablePendingReleases",
        stale_pending_releases,
    ),
    ("CleanupOrphanedProviderStatus", orphaned_provider_status),
    ("FixFutureProviderStatusTimes", future_status_times),
    ("FixFutureRunScheduledTasks", future_task_runs),
    ("CleanupUnusedTags", unused_tags),
];

/// Run every housekeeper, then compact the database.
pub fn run(conn: &Connection, now: DateTime<Utc>) -> Report {
    tracing::info!("Running housecleaning tasks");
    let mut report = Report::new();
    for (name, clean) in HOUSEKEEPERS {
        tracing::debug!("Starting {name}");
        match clean(conn, now) {
            Ok(rows) => {
                tracing::debug!(rows, "Completed {name}");
                report.push((name, rows));
            }
            Err(e) => tracing::error!(error = %e, "Error running housekeeping task: {name}"),
        }
    }
    tracing::debug!("Compressing main database after housekeeping");
    if let Err(e) = conn.execute_batch("VACUUM") {
        tracing::error!(error = %e, "Could not compress the database");
    }
    report
}

fn orphaned_blocklist(conn: &Connection, _: DateTime<Utc>) -> anyhow::Result<usize> {
    Ok(conn.execute(
        "DELETE FROM blocklist WHERE series_id NOT IN (SELECT id FROM series)",
        [],
    )?)
}

/// Files no episode points to.
fn orphaned_episode_files(conn: &Connection, _: DateTime<Utc>) -> anyhow::Result<usize> {
    Ok(conn.execute(
        "DELETE FROM episode_files
         WHERE id NOT IN (SELECT episode_file_id FROM episodes WHERE episode_file_id IS NOT NULL)",
        [],
    )?)
}

fn orphaned_episodes(conn: &Connection, _: DateTime<Utc>) -> anyhow::Result<usize> {
    Ok(conn.execute(
        "DELETE FROM episodes WHERE series_id NOT IN (SELECT id FROM series)",
        [],
    )?)
}

/// History of a series, or of an episode, that is gone.
fn orphaned_history(conn: &Connection, _: DateTime<Utc>) -> anyhow::Result<usize> {
    let by_series = conn.execute(
        "DELETE FROM history WHERE series_id NOT IN (SELECT id FROM series)",
        [],
    )?;
    let by_episode = conn.execute(
        "DELETE FROM history WHERE episode_id NOT IN (SELECT id FROM episodes)",
        [],
    )?;
    Ok(by_series + by_episode)
}

/// Subtitles and other extras of a series, or of an episode file, that is
/// gone (`CleanupOrphanedExtraFiles`, `CleanupOrphanedSubtitleFiles`).
fn orphaned_extra_files(conn: &Connection, _: DateTime<Utc>) -> anyhow::Result<usize> {
    let by_series = conn.execute(
        "DELETE FROM extra_files WHERE series_id NOT IN (SELECT id FROM series)",
        [],
    )?;
    let by_file = conn.execute(
        "DELETE FROM extra_files WHERE episode_file_id > 0
            AND episode_file_id NOT IN (SELECT id FROM episode_files)",
        [],
    )?;
    Ok(by_series + by_file)
}

fn orphaned_pending_releases(conn: &Connection, _: DateTime<Utc>) -> anyhow::Result<usize> {
    Ok(conn.execute(
        "DELETE FROM pending_releases WHERE series_id NOT IN (SELECT id FROM series)",
        [],
    )?)
}

/// Releases held back because no client could take them, or as a
/// fallback, and still there two weeks later.
pub(crate) fn stale_pending_releases(conn: &Connection, now: DateTime<Utc>) -> anyhow::Result<usize> {
    use crate::downloads::pending::PendingReleaseReason;
    Ok(conn.execute(
        "DELETE FROM pending_releases WHERE added < ?1 AND reason IN (?2, ?3)",
        params![
            time::format_utc(now - Duration::days(14)),
            PendingReleaseReason::DownloadClientUnavailable.as_i64(),
            PendingReleaseReason::Fallback.as_i64(),
        ],
    )?)
}

/// The back-off records of indexers, download clients, notifications and
/// import lists that no longer exist.
pub(crate) fn orphaned_provider_status(conn: &Connection, _: DateTime<Utc>) -> anyhow::Result<usize> {
    let statuses = conn.execute(
        "DELETE FROM provider_status
         WHERE NOT EXISTS (SELECT 1 FROM providers p
                           WHERE p.kind = provider_status.kind AND p.id = provider_status.provider_id)",
        [],
    )?;
    let lists = conn.execute(
        "DELETE FROM import_list_status
         WHERE provider_id NOT IN (SELECT id FROM providers WHERE kind = 'importList')",
        [],
    )?;
    Ok(statuses + lists)
}

/// A clock that went back leaves failures in the future and providers
/// disabled for longer than their back-off allows: both are brought back.
/// The originals add the period, which is in seconds, as minutes; so does
/// this.
pub(crate) fn future_status_times(conn: &Connection, now: DateTime<Utc>) -> anyhow::Result<usize> {
    let now_text = time::format_utc(now);
    let mut fixed = 0;
    for (level, period) in PERIODS.iter().enumerate() {
        fixed += conn.execute(
            "UPDATE provider_status SET disabled_till = ?1
             WHERE escalation_level = ?2 AND disabled_till > ?1",
            params![time::format_utc(now + Duration::minutes(*period)), level as i64],
        )?;
    }
    fixed += conn.execute(
        "UPDATE provider_status SET initial_failure = ?1 WHERE initial_failure > ?1",
        params![now_text],
    )?;
    fixed += conn.execute(
        "UPDATE provider_status SET most_recent_failure = ?1 WHERE most_recent_failure > ?1",
        params![now_text],
    )?;
    Ok(fixed)
}

/// `CleanupAdditionalUsers`: one user. The original keeps "the first"
/// the table gives, with no order asked for; SQLite answers that from the
/// smallest index that covers it, the unique index on the user name, so
/// the user kept is the one whose name sorts first (recorded on both
/// originals, `golden-housekeeping`).
pub(crate) fn additional_users(conn: &Connection, _: DateTime<Utc>) -> anyhow::Result<usize> {
    Ok(conn.execute(
        "DELETE FROM users WHERE id NOT IN (SELECT id FROM users ORDER BY username LIMIT 1)",
        [],
    )?)
}

/// `FixFutureRunScheduledTasks`: a clock that went back leaves tasks whose
/// last run is still to come, and which the scheduler would not run
/// before then; their last run becomes now. The originals' scheduler
/// reads its tasks from a cache filled at start, so there it takes effect
/// at the next start; Unarr's reads the table, and the next tick runs
/// what is due.
pub(crate) fn future_task_runs(conn: &Connection, now: DateTime<Utc>) -> anyhow::Result<usize> {
    let mut fixed = 0;
    for task in crate::store::providers::list_scheduled_tasks(conn)? {
        if task.last_execution.is_some_and(|last| last > now) {
            crate::store::providers::set_scheduled_task_last_execution(conn, task.id, now)?;
            fixed += 1;
        }
    }
    Ok(fixed)
}

/// Tags no series, provider, delay or release profile or auto tagging
/// rule carries, and no rule names in a condition.
fn unused_tags(conn: &Connection, _: DateTime<Utc>) -> anyhow::Result<usize> {
    Ok(conn.execute(
        "DELETE FROM tags WHERE id NOT IN (
            SELECT tag_id FROM series_tags
            UNION SELECT value FROM providers, json_each(providers.tags)
            UNION SELECT value FROM delay_profiles, json_each(delay_profiles.tags)
            UNION SELECT value FROM release_profiles, json_each(release_profiles.tags)
            UNION SELECT value FROM auto_tagging, json_each(auto_tagging.tags)
            UNION SELECT json_extract(spec.value, '$.body.value')
                  FROM auto_tagging, json_each(auto_tagging.specifications) AS spec
                  WHERE json_extract(spec.value, '$.type') = 'TagSpecification'
                    AND json_extract(spec.value, '$.body.value') IS NOT NULL
         )",
        [],
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn count(conn: &Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn orphans_go_and_what_is_referred_to_stays() {
        let db = crate::db::Db::open_in_memory().unwrap();
        let now = Utc.with_ymd_and_hms(2026, 11, 10, 12, 0, 0).unwrap();
        db.call_blocking(move |conn| {
            use crate::model::Tag;
            use crate::store::catalog;
            for (id, label) in [(1, "kept-by-series"), (2, "kept-by-rule"), (3, "unused"), (4, "kept-by-profile")] {
                catalog::insert_tag(conn, &Tag { id, label: label.into() })?;
            }
            // A series with its tag; a quality profile is its only other need.
            conn.execute_batch(
                "INSERT INTO quality_profiles (id, name, cutoff, items) VALUES (1, 'Any', 1, '[]');",
            )?;
            let mut series = crate::store::series::tests::sample(1);
            series.tags = vec![1];
            crate::store::series::insert(conn, &series)?;
            conn.execute_batch(
                "PRAGMA foreign_keys = OFF;
                 INSERT INTO blocklist (id, series_id, source_title, date) VALUES
                    (1, 1, 'kept', '2026-11-01T00:00:00Z'), (2, 99, 'orphan', '2026-11-01T00:00:00Z');
                 INSERT INTO pending_releases (id, series_id, added, reason, title, release) VALUES
                    (1, 1, '2026-11-09T00:00:00Z', 1, 'recent', '{}'),
                    (2, 1, '2026-10-01T00:00:00Z', 1, 'stale, no client', '{}'),
                    (3, 1, '2026-10-01T00:00:00Z', 0, 'stale, delayed', '{}'),
                    (4, 99, '2026-11-09T00:00:00Z', 0, 'orphan', '{}');
                 INSERT INTO provider_status (kind, provider_id, escalation_level, disabled_till, initial_failure, most_recent_failure) VALUES
                    ('indexer', 77, 1, '2026-11-10T12:30:00Z', '2026-11-10T11:00:00Z', '2026-11-10T11:00:00Z');
                 INSERT INTO delay_profiles (id, sort_order, tags) VALUES (9, 1, '[4]');
                 INSERT INTO auto_tagging (id, name, tags, specifications) VALUES
                    (1, 'rule', '[]', '[{\"type\":\"TagSpecification\",\"body\":{\"value\":2}}]');
                 PRAGMA foreign_keys = ON;",
            )?;
            let report = run(conn, now);
            let removed = |name: &str| report.iter().find(|(n, _)| *n == name).map(|(_, rows)| *rows);
            assert_eq!(removed("CleanupOrphanedBlocklist"), Some(1));
            assert_eq!(removed("CleanupOrphanedPendingReleases"), Some(1));
            assert_eq!(removed("CleanupDownloadClientUnavailablePendingReleases"), Some(1));
            assert_eq!(removed("CleanupOrphanedProviderStatus"), Some(1));
            assert_eq!(removed("CleanupUnusedTags"), Some(1));
            assert_eq!(count(conn, "SELECT count(*) FROM blocklist"), 1);
            assert_eq!(
                conn.prepare("SELECT title FROM pending_releases ORDER BY id")?
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?,
                ["recent", "stale, delayed"]
            );
            assert_eq!(
                conn.prepare("SELECT label FROM tags ORDER BY id")?
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?,
                ["kept-by-series", "kept-by-rule", "kept-by-profile"]
            );
            // A second run finds nothing.
            assert!(run(conn, now).iter().all(|(_, rows)| *rows == 0));
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn one_user_is_kept() {
        let db = crate::db::Db::open_in_memory().unwrap();
        let now = Utc.with_ymd_and_hms(2026, 11, 10, 12, 0, 0).unwrap();
        db.call_blocking(move |conn| {
            conn.execute_batch(
                "INSERT INTO users (id, identifier, username, password) VALUES
                    (2, 'c', 'unarr', 'x'), (3, 'e', 'second', 'y'), (9, 'i', 'third', 'z');",
            )?;
            let report = run(conn, now);
            assert_eq!(
                report.iter().find(|(n, _)| *n == "CleanupAdditionalUsers"),
                Some(&("CleanupAdditionalUsers", 2))
            );
            let users: Vec<String> = crate::store::providers::list_users(conn)?
                .into_iter()
                .map(|u| u.username)
                .collect();
            // As the original's covering index on the name orders them.
            assert_eq!(users, ["second"]);
            assert_eq!(additional_users(conn, now)?, 0);
            Ok(())
        })
        .unwrap();
    }

    /// The clock went back a day: the tasks that ran "tomorrow" ran now,
    /// and are due again after their interval, not after tomorrow's run.
    #[test]
    fn task_runs_in_the_future_come_back() {
        use crate::commands::scheduler;
        use crate::store::providers;
        let db = crate::db::Db::open_in_memory().unwrap();
        let now = Utc.with_ymd_and_hms(2026, 11, 10, 12, 0, 0).unwrap();
        let tomorrow = now + Duration::days(1);
        let an_hour_ago = now - Duration::hours(1);
        db.call_blocking(move |conn| {
            scheduler::ensure_defaults(conn, now)?;
            let tasks = providers::list_scheduled_tasks(conn)?;
            let (ahead, behind) = (&tasks[0], &tasks[1]);
            providers::set_scheduled_task_execution(conn, ahead.id, tomorrow, tomorrow)?;
            providers::set_scheduled_task_execution(conn, behind.id, an_hour_ago, an_hour_ago)?;
            let due_later = now + Duration::minutes(ahead.interval_minutes) + Duration::seconds(1);
            assert!(
                !scheduler::pending_tasks(conn, due_later)?
                    .iter()
                    .any(|t| t.id == ahead.id),
                "a run in the future keeps the task waiting"
            );
            assert_eq!(future_task_runs(conn, now)?, 1);
            let after = providers::list_scheduled_tasks(conn)?;
            let find = |id: i64| after.iter().find(|t| t.id == id).unwrap().clone();
            assert_eq!(find(ahead.id).last_execution, Some(now));
            // The start of the run is left as it was, as the original does.
            assert_eq!(find(ahead.id).last_start_time, Some(tomorrow));
            assert_eq!(find(behind.id).last_execution, Some(an_hour_ago));
            assert!(scheduler::pending_tasks(conn, due_later)?
                .iter()
                .any(|t| t.id == ahead.id));
            assert_eq!(future_task_runs(conn, now)?, 0);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn status_times_in_the_future_come_back() {
        let db = crate::db::Db::open_in_memory().unwrap();
        let now = Utc.with_ymd_and_hms(2026, 11, 10, 12, 0, 0).unwrap();
        db.call_blocking(move |conn| {
            conn.execute_batch(
                "INSERT INTO providers (kind, id, name, implementation, config_contract) VALUES
                    ('indexer', 1, 'a', 'Newznab', 'NewznabSettings'), ('indexer', 2, 'b', 'Newznab', 'NewznabSettings');
                 INSERT INTO provider_status (kind, provider_id, escalation_level, disabled_till, initial_failure, most_recent_failure) VALUES
                    ('indexer', 1, 1, '2027-01-01T00:00:00Z', '2027-01-01T00:00:00Z', '2026-11-10T11:00:00Z'),
                    ('indexer', 2, 1, '2026-11-10T12:30:00Z', '2026-11-10T11:00:00Z', '2026-11-10T11:00:00Z');",
            )?;
            assert_eq!(future_status_times(conn, now)?, 2);
            let row = |id: i64| -> (String, String) {
                conn.query_row(
                    "SELECT disabled_till, initial_failure FROM provider_status WHERE provider_id = ?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap()
            };
            // Level 1 is 60: an hour from now, as the originals count it.
            assert_eq!(row(1), ("2026-11-10T13:00:00Z".into(), "2026-11-10T12:00:00Z".into()));
            assert_eq!(row(2), ("2026-11-10T12:30:00Z".into(), "2026-11-10T11:00:00Z".into()));
            Ok(())
        })
        .unwrap();
    }
}
