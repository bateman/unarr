//! `TaskManager` and `Scheduler`: the recurring tasks, kept in the
//! `scheduled_tasks` table with the originals' intervals, and the tick
//! that pushes the ones that are due.

use chrono::{DateTime, Duration, Utc};
use rusqlite::Connection;

use super::{CommandBody, CommandPriority};
use crate::model::ScheduledTask;
use crate::store;

/// How long the scheduler waits, after a tick ends, before it looks for
/// due tasks again.
pub const TICK: std::time::Duration = std::time::Duration::from_secs(30);

/// `(command, interval in minutes, priority)` of every default task; the
/// intervals that follow the settings are resolved against the config.
fn default_tasks(conn: &Connection) -> anyhow::Result<Vec<(CommandBody, i64, CommandPriority)>> {
    let indexer = store::config::indexer_config(conn)?;
    let backup_days = store::config::config(conn, "BackupInterval")?
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(7)
        .clamp(1, 7);
    // `GetRefreshMonitoredInterval`: the setting, at least a minute.
    let refresh_downloads = store::config::config(conn, "CheckForFinishedDownloadInterval")?
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(1)
        .max(1);
    let rss = match indexer.rss_sync_interval {
        i if i > 0 && i < 10 => 10,
        i if i < 0 => 0,
        i => i,
    };
    Ok(vec![
        (
            CommandBody::RefreshMonitoredDownloads,
            refresh_downloads,
            CommandPriority::High,
        ),
        (CommandBody::MessagingCleanup, 5, CommandPriority::Low),
        (
            CommandBody::ApplicationUpdateCheck {
                install_major_update: false,
            },
            6 * 60,
            CommandPriority::Low,
        ),
        (CommandBody::UpdateSceneMapping, 3 * 60, CommandPriority::Low),
        (CommandBody::CheckHealth, 6 * 60, CommandPriority::Low),
        (
            CommandBody::RefreshSeries {
                series_ids: vec![],
                is_new_series: false,
            },
            12 * 60,
            CommandPriority::Low,
        ),
        (CommandBody::Housekeeping, 24 * 60, CommandPriority::Low),
        (CommandBody::CleanUpRecycleBin, 24 * 60, CommandPriority::Low),
        (
            CommandBody::ImportListSync { definition_id: None },
            5,
            CommandPriority::Low,
        ),
        (
            CommandBody::Backup {
                kind: super::BackupType::Scheduled,
            },
            backup_days * 60 * 24,
            CommandPriority::Low,
        ),
        (CommandBody::RssSync, rss, CommandPriority::Low),
    ])
}

/// Radarr's default tasks, `(type name, interval in minutes)`: its own
/// `TaskManager` list. The ones whose commands a later milestone brings
/// are listed and not run yet.
fn radarr_tasks(conn: &Connection) -> anyhow::Result<Vec<(String, i64)>> {
    let sonarr = default_tasks(conn)?;
    let interval_of = |body: CommandBody| {
        sonarr
            .iter()
            .find(|(b, _, _)| b.type_name() == body.type_name())
            .map(|(_, interval, _)| *interval)
            .unwrap_or(0)
    };
    let backup = interval_of(CommandBody::Backup {
        kind: super::BackupType::Scheduled,
    });
    Ok([
        ("NzbDrone.Core.Messaging.Commands.MessagingCleanupCommand", 5),
        // Radarr 7 drops the expired entries of its in-memory caches every
        // five minutes; listed, and not run: Unarr has no such caches.
        ("NzbDrone.Core.Caching.ClearExpiredCacheCommand", 5),
        (super::RADARR_UPDATE_CHECK_TYPE, 6 * 60),
        ("NzbDrone.Core.HealthCheck.CheckHealthCommand", 6 * 60),
        ("NzbDrone.Core.Movies.Commands.RefreshMovieCommand", 24 * 60),
        ("NzbDrone.Core.Housekeeping.HousekeepingCommand", 24 * 60),
        (
            "NzbDrone.Core.MediaFiles.Commands.CleanUpRecycleBinCommand",
            24 * 60,
        ),
        ("NzbDrone.Core.Movies.Commands.RefreshCollectionsCommand", 24 * 60),
        ("NzbDrone.Core.Backup.BackupCommand", backup),
        (
            "NzbDrone.Core.Indexers.RssSyncCommand",
            interval_of(CommandBody::RssSync),
        ),
        ("NzbDrone.Core.ImportLists.ImportListSyncCommand", 5),
        (
            "NzbDrone.Core.Download.RefreshMonitoredDownloadsCommand",
            interval_of(CommandBody::RefreshMonitoredDownloads),
        ),
    ]
    .into_iter()
    .map(|(name, interval)| (name.to_string(), interval))
    .collect())
}

/// The default tasks of an application, `(type name, interval)`.
fn tasks_for(conn: &Connection, personality: crate::Personality) -> anyhow::Result<Vec<(String, i64)>> {
    match personality {
        crate::Personality::Radarr => radarr_tasks(conn),
        _ => Ok(default_tasks(conn)?
            .into_iter()
            .map(|(body, interval, _)| (body.type_name().to_string(), interval))
            .collect()),
    }
}

/// `ScheduledTask.Priority` for a stored task.
pub fn priority_of(type_name: &str) -> CommandPriority {
    if type_name == CommandBody::RefreshMonitoredDownloads.type_name() {
        CommandPriority::High
    } else {
        CommandPriority::Low
    }
}

/// `Handle(ApplicationStartedEvent)`: the table brought in line with the
/// default tasks. A task not stored yet counts as just run, so it waits a
/// whole interval; a stored task keeps its last run and takes the
/// current interval.
pub fn ensure_defaults(conn: &Connection, now: DateTime<Utc>) -> anyhow::Result<()> {
    ensure_defaults_for(conn, crate::Personality::Sonarr, now)
}

/// [`ensure_defaults`] with the default tasks of one application.
pub fn ensure_defaults_for(
    conn: &Connection,
    personality: crate::Personality,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    let defaults = tasks_for(conn, personality)?;
    let current = store::providers::list_scheduled_tasks(conn)?;
    for job in &current {
        if !defaults.iter().any(|(name, _)| *name == job.type_name) {
            tracing::trace!(task = job.type_name, "removing job from database");
            store::providers::delete_scheduled_task(conn, job.id)?;
        }
    }
    for (type_name, interval) in defaults {
        let type_name = type_name.as_str();
        match current.iter().find(|t| t.type_name == type_name) {
            Some(existing) => store::providers::set_scheduled_task_interval(conn, existing.id, interval)?,
            None => store::providers::insert_scheduled_task(
                conn,
                &ScheduledTask {
                    id: 0,
                    type_name: type_name.to_string(),
                    interval_minutes: interval,
                    last_execution: Some(now),
                    last_start_time: Some(now),
                },
            )?,
        }
    }
    Ok(())
}

/// `HandleAsync(ConfigSavedEvent)`: the intervals that follow the settings.
pub fn refresh_intervals(conn: &Connection) -> anyhow::Result<()> {
    refresh_intervals_for(conn, crate::Personality::Sonarr)
}

/// [`refresh_intervals`] with the default tasks of one application.
pub fn refresh_intervals_for(conn: &Connection, personality: crate::Personality) -> anyhow::Result<()> {
    let defaults = tasks_for(conn, personality)?;
    for task in store::providers::list_scheduled_tasks(conn)? {
        if let Some((_, interval)) = defaults.iter().find(|(name, _)| *name == task.type_name) {
            if *interval != task.interval_minutes {
                store::providers::set_scheduled_task_interval(conn, task.id, *interval)?;
            }
        }
    }
    Ok(())
}

/// `GetPending`: tasks whose interval has passed since their last run.
pub fn pending_tasks(conn: &Connection, now: DateTime<Utc>) -> anyhow::Result<Vec<ScheduledTask>> {
    Ok(store::providers::list_scheduled_tasks(conn)?
        .into_iter()
        .filter(|t| t.interval_minutes > 0)
        .filter(|t| {
            t.last_execution
                .map(|last| last + Duration::minutes(t.interval_minutes) < now)
                .unwrap_or(true)
        })
        .collect())
}

/// `GetNextExecution`: when the task next runs, by its type name.
pub fn next_execution(conn: &Connection, type_name: &str) -> anyhow::Result<Option<DateTime<Utc>>> {
    Ok(store::providers::list_scheduled_tasks(conn)?
        .into_iter()
        .find(|t| t.type_name == type_name)
        .map(|t| t.last_execution.unwrap_or_default() + Duration::minutes(t.interval_minutes)))
}

/// `Handle(CommandExecutedEvent)`: the task's last run recorded when the
/// command counts as one.
pub fn record_execution(
    conn: &Connection,
    body: &CommandBody,
    last_execution: DateTime<Utc>,
    started_at: DateTime<Utc>,
) -> anyhow::Result<bool> {
    if !body.updates_scheduled_task() {
        return Ok(false);
    }
    let type_name = body.type_name();
    // The task is found by the command it names, not by the spelling of
    // its type: Radarr's update check is the same command under another.
    match store::providers::list_scheduled_tasks(conn)?
        .into_iter()
        .find(|t| {
            t.type_name == type_name
                || CommandBody::from_type_name(&t.type_name).is_some_and(|b| b.name() == body.name())
        }) {
        Some(task) => {
            tracing::trace!(task = type_name, "updating last run time");
            store::providers::set_scheduled_task_execution(conn, task.id, last_execution, started_at)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use chrono::TimeZone;

    #[test]
    fn defaults_follow_the_settings_and_keep_stored_runs() {
        let db = Db::open_in_memory().unwrap();
        let now = Utc.with_ymd_and_hms(2026, 11, 7, 12, 0, 0).unwrap();
        db.call_blocking(move |conn| {
            store::config::set_config(conn, "RssSyncInterval", "5")?;
            store::config::set_config(conn, "BackupInterval", "30")?;
            store::providers::insert_scheduled_task(
                conn,
                &ScheduledTask {
                    id: 1,
                    type_name: "NzbDrone.Core.Indexers.RssSyncCommand".into(),
                    interval_minutes: 60,
                    last_execution: Some(now - Duration::hours(3)),
                    last_start_time: Some(now - Duration::hours(3)),
                },
            )?;
            store::providers::insert_scheduled_task(
                conn,
                &ScheduledTask {
                    id: 2,
                    type_name: "NzbDrone.Core.Gone.OldCommand".into(),
                    interval_minutes: 60,
                    last_execution: None,
                    last_start_time: None,
                },
            )?;
            ensure_defaults(conn, now)?;
            let tasks = store::providers::list_scheduled_tasks(conn)?;
            assert_eq!(tasks.len(), 11);
            assert!(tasks
                .iter()
                .all(|t| t.type_name != "NzbDrone.Core.Gone.OldCommand"));
            let rss = tasks
                .iter()
                .find(|t| t.type_name.ends_with("RssSyncCommand"))
                .unwrap();
            assert_eq!(
                rss.interval_minutes, 10,
                "an interval under ten minutes is raised to ten"
            );
            assert_eq!(
                rss.last_execution,
                Some(now - Duration::hours(3)),
                "a stored task keeps its last run"
            );
            let backup = tasks
                .iter()
                .find(|t| t.type_name.ends_with("BackupCommand"))
                .unwrap();
            assert_eq!(
                backup.interval_minutes,
                7 * 24 * 60,
                "backup days are clamped to a week"
            );
            // Only the RSS sync is due: every new task counts as just run.
            let due = pending_tasks(conn, now)?;
            assert_eq!(
                due.iter().map(|t| t.type_name.as_str()).collect::<Vec<_>>(),
                vec!["NzbDrone.Core.Indexers.RssSyncCommand"]
            );
            assert!(record_execution(
                conn,
                &CommandBody::RssSync,
                now,
                now - Duration::seconds(5)
            )?);
            assert!(pending_tasks(conn, now)?.is_empty());
            assert_eq!(
                next_execution(conn, "NzbDrone.Core.Indexers.RssSyncCommand")?,
                Some(now + Duration::minutes(10))
            );
            // A command that does not count as the task's run leaves it alone.
            assert!(!record_execution(
                conn,
                &CommandBody::RefreshSeries {
                    series_ids: vec![1],
                    is_new_series: false
                },
                now,
                now
            )?);
            assert_eq!(
                priority_of(CommandBody::RefreshMonitoredDownloads.type_name()),
                CommandPriority::High
            );
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn a_task_ended_after_its_tick_waits_for_the_next_one() {
        // The originals keep a task's last run to the tick: a one-minute
        // task that ended a few milliseconds after the tick that pushed it
        // is not due on the tick a minute later, only on the one after.
        let db = Db::open_in_memory().unwrap();
        let tick = Utc.with_ymd_and_hms(2026, 11, 15, 7, 24, 34).unwrap() + Duration::milliseconds(580);
        db.call_blocking(move |conn| {
            ensure_defaults(conn, tick - Duration::hours(1))?;
            let ended = tick + Duration::milliseconds(40);
            assert!(record_execution(
                conn,
                &CommandBody::RefreshMonitoredDownloads,
                ended,
                tick + Duration::milliseconds(2)
            )?);
            let monitor = |due: Vec<ScheduledTask>| {
                due.iter()
                    .any(|t| t.type_name == CommandBody::RefreshMonitoredDownloads.type_name())
            };
            let stored = store::providers::list_scheduled_tasks(conn)?
                .into_iter()
                .find(|t| t.type_name == CommandBody::RefreshMonitoredDownloads.type_name())
                .unwrap();
            assert_eq!(stored.last_execution, Some(ended), "kept to the microsecond");
            assert!(!monitor(pending_tasks(conn, tick + Duration::seconds(60))?));
            assert!(monitor(pending_tasks(conn, tick + Duration::seconds(90))?));
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn radarr_keeps_its_own_tasks_and_commands() {
        use crate::Personality;
        let db = Db::open_in_memory_for(Personality::Radarr).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 11, 11, 12, 0, 0).unwrap();
        db.call_blocking(move |conn| {
            ensure_defaults_for(conn, Personality::Radarr, now)?;
            let tasks = store::providers::list_scheduled_tasks(conn)?;
            assert_eq!(tasks.len(), 12);
            // Radarr 7's cache clean-up, every five minutes.
            let clear = tasks
                .iter()
                .find(|t| t.type_name == "NzbDrone.Core.Caching.ClearExpiredCacheCommand")
                .expect("ClearExpiredCache");
            assert_eq!(clear.interval_minutes, 5);
            for name in [
                "NzbDrone.Core.Movies.Commands.RefreshMovieCommand",
                "NzbDrone.Core.Movies.Commands.RefreshCollectionsCommand",
                "NzbDrone.Core.Update.Commands.ApplicationCheckUpdateCommand",
            ] {
                assert!(tasks.iter().any(|t| t.type_name == name), "{name}");
            }
            assert!(tasks.iter().all(|t| !t.type_name.contains("Series")));
            // Radarr's update check is found under its own type name,
            // to be run and to have its run recorded.
            let check = CommandBody::from_type_name(crate::commands::RADARR_UPDATE_CHECK_TYPE)
                .expect("the update check");
            assert_eq!(check.name_in(Personality::Radarr), "ApplicationCheckUpdate");
            let later = now + chrono::Duration::hours(7);
            assert!(record_execution(conn, &check, later, later)?);
            let task = store::providers::list_scheduled_tasks(conn)?
                .into_iter()
                .find(|t| t.type_name == crate::commands::RADARR_UPDATE_CHECK_TYPE)
                .unwrap();
            assert_eq!(task.last_execution, Some(later));
            // Running it again changes nothing.
            ensure_defaults_for(conn, Personality::Radarr, now)?;
            assert_eq!(store::providers::list_scheduled_tasks(conn)?.len(), 12);
            Ok(())
        })
        .unwrap();
        // Each application answers to its own names only.
        let body = serde_json::json!({"movieIds": [116], "seriesId": 1});
        assert_eq!(
            CommandBody::parse_for(Personality::Radarr, "moviessearch", &body),
            Some(CommandBody::MoviesSearch { movie_ids: vec![116] })
        );
        assert_eq!(
            CommandBody::parse_for(Personality::Sonarr, "MoviesSearch", &body),
            None
        );
        assert_eq!(
            CommandBody::parse_for(Personality::Radarr, "SeriesSearch", &body),
            None
        );
        assert!(CommandBody::parse_for(Personality::Sonarr, "SeriesSearch", &body).is_some());
        assert!(CommandBody::parse_for(Personality::Radarr, "RssSync", &body).is_some());
        // The tasks with no media in them run in both; the library's
        // commands under each application's own names.
        assert!(CommandBody::Housekeeping.runs_in(Personality::Radarr));
        assert!(CommandBody::Housekeeping.runs_in(Personality::Sonarr));
        assert!(CommandBody::parse_for(Personality::Radarr, "RescanMovie", &body).is_some());
        assert!(CommandBody::parse_for(Personality::Sonarr, "RescanMovie", &body).is_none());
        assert!(CommandBody::parse_for(Personality::Radarr, "RescanSeries", &body).is_none());
        // The import lists' sync runs in both, and names what each
        // application's lists name.
        assert!(CommandBody::ImportListSync { definition_id: None }.runs_in(Personality::Radarr));
        // Whatever the body says, the trigger makes the backup's type, in
        // both originals: posted, it is a manual one; the scheduler's run, a
        // scheduled one.
        for personality in [Personality::Sonarr, Personality::Radarr] {
            for body in [
                serde_json::json!({}),
                serde_json::json!({"type": "scheduled"}),
                serde_json::json!({"type": "update"}),
            ] {
                let posted = CommandBody::parse_for(personality, "Backup", &body).unwrap();
                assert_eq!(
                    posted.clone().for_trigger(super::super::CommandTrigger::Manual),
                    CommandBody::Backup {
                        kind: super::super::BackupType::Manual
                    },
                    "{personality:?} {body}"
                );
                assert_eq!(
                    posted.for_trigger(super::super::CommandTrigger::Scheduled),
                    CommandBody::Backup {
                        kind: super::super::BackupType::Scheduled
                    },
                    "{personality:?} {body}"
                );
            }
        }
    }
}
