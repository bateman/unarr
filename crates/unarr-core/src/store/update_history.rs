//! The originals' `UpdateHistoryService`: the versions the application was
//! started with, and the update a start reveals.

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};

use super::ts;

/// `UpdateHistoryEventType.Installed`.
const INSTALLED: i64 = 1;

/// `UpdateHistoryRepository.LastInstalled`: the version of the newest
/// `Installed` row.
pub fn last_installed(conn: &Connection) -> anyhow::Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT version FROM update_history WHERE event_type = ?1 ORDER BY id DESC LIMIT 1",
            params![INSTALLED],
            |r| r.get(0),
        )
        .optional()?)
}

/// `UpdateHistoryService.Handle(ApplicationStartedEvent)`: a start with a
/// version other than the last one recorded is recorded, and the version
/// it replaced returned; the very first start has none to return.
pub fn record_start(conn: &Connection, version: &str, now: DateTime<Utc>) -> anyhow::Result<Option<String>> {
    let last = last_installed(conn)?;
    if last.as_deref() == Some(version) {
        return Ok(None);
    }
    conn.execute(
        "INSERT INTO update_history (date, version, event_type) VALUES (?1, ?2, ?3)",
        params![ts(Some(now)), version, INSTALLED],
    )?;
    Ok(last)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::Personality;

    #[test]
    fn a_start_returns_the_version_it_replaced() {
        for personality in [Personality::Sonarr, Personality::Radarr] {
            let db = Db::open_in_memory_for(personality).unwrap();
            let now: DateTime<Utc> = "2026-11-21T10:00:00Z".parse().unwrap();
            db.call_blocking(move |conn| {
                // The first start: recorded, nothing replaced.
                assert_eq!(record_start(conn, "0.1.0", now)?, None);
                // The same version again: nothing recorded.
                assert_eq!(record_start(conn, "0.1.0", now)?, None);
                assert_eq!(record_start(conn, "0.2.0", now)?.as_deref(), Some("0.1.0"));
                assert_eq!(last_installed(conn)?.as_deref(), Some("0.2.0"));
                // Going back is a change too, as the originals compare.
                assert_eq!(record_start(conn, "0.1.0", now)?.as_deref(), Some("0.2.0"));
                let rows: i64 = conn.query_row("SELECT count(*) FROM update_history", [], |r| r.get(0))?;
                assert_eq!(rows, 3);
                Ok(())
            })
            .unwrap();
        }
    }
}
