//! `TaskManager` and `Scheduler`: the recurring tasks, kept in the
//! `scheduled_tasks` table with the originals' intervals, and the tick
//! that pushes the ones that are due.

use chrono::{DateTime, Duration, Utc};
use rusqlite::Connection;

use super::{CommandBody, CommandPriority};
use crate::model::ScheduledTask;
use crate::store;

/// How often the scheduler looks for due tasks.
pub const TICK: std::time::Duration = std::time::Duration::from_secs(30);

/// `(command, interval in minutes, priority)` of every default task; the
/// intervals that follow the settings are resolved against the config.
fn default_tasks(conn: &Connection) -> anyhow::Result<Vec<(CommandBody, i64, CommandPriority)>> {
    let indexer = store::config::indexer_config(conn)?;
    let backup_days = store::config::config(conn, "BackupInterval")?
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(7)
        .clamp(1, 7);
    let rss = match indexer.rss_sync_interval {
        i if i > 0 && i < 10 => 10,
        i if i < 0 => 0,
        i => i,
    };
    Ok(vec![
        (CommandBody::RefreshMonitoredDownloads, 1, CommandPriority::High),
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
    let defaults = default_tasks(conn)?;
    let current = store::providers::list_scheduled_tasks(conn)?;
    for job in &current {
        if !defaults.iter().any(|(b, _, _)| b.type_name() == job.type_name) {
            tracing::trace!(task = job.type_name, "removing job from database");
            store::providers::delete_scheduled_task(conn, job.id)?;
        }
    }
    for (body, interval, _) in defaults {
        let type_name = body.type_name();
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
    let defaults = default_tasks(conn)?;
    for task in store::providers::list_scheduled_tasks(conn)? {
        if let Some((_, interval, _)) = defaults.iter().find(|(b, _, _)| b.type_name() == task.type_name) {
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
    match store::providers::list_scheduled_tasks(conn)?
        .into_iter()
        .find(|t| t.type_name == type_name)
    {
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
}
