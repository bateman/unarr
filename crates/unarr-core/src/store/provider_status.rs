//! Per-provider failure state: the originals' `IndexerStatus` and
//! `DownloadClientStatus` rows, one per provider, created on first use.

use chrono::{DateTime, Utc};
use rusqlite::{named_params, params, Connection, OptionalExtension, Row};

use super::{parse_ts, ts};
use crate::model::ProviderKind;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProviderStatus {
    pub initial_failure: Option<DateTime<Utc>>,
    pub most_recent_failure: Option<DateTime<Utc>>,
    pub escalation_level: i64,
    pub disabled_till: Option<DateTime<Utc>>,
    /// The newest release the last RSS sync saw, as JSON (indexers only).
    pub last_rss_sync_release_info: Option<String>,
}

impl ProviderStatus {
    /// `DisabledTill` in the future: the provider is left alone.
    pub fn is_disabled(&self, now: DateTime<Utc>) -> bool {
        self.disabled_till.is_some_and(|t| t > now)
    }
}

/// The status of a provider; a fresh one when none was recorded.
pub fn provider_status(conn: &Connection, kind: ProviderKind, id: i64) -> anyhow::Result<ProviderStatus> {
    let found = conn
        .query_row(
            "SELECT * FROM provider_status WHERE kind = ?1 AND provider_id = ?2",
            params![kind.as_str(), id],
            status_from_row,
        )
        .optional()?;
    Ok(found.unwrap_or_default())
}

/// Every recorded status of a kind, by provider id.
pub fn provider_statuses(
    conn: &Connection,
    kind: ProviderKind,
) -> anyhow::Result<Vec<(i64, ProviderStatus)>> {
    let mut stmt = conn.prepare("SELECT * FROM provider_status WHERE kind = ?1 ORDER BY provider_id")?;
    let rows = stmt.query_map(params![kind.as_str()], |r| {
        Ok((r.get::<_, i64>("provider_id")?, status_from_row(r)?))
    })?;
    rows.map(|r| r.map_err(Into::into)).collect()
}

pub fn upsert_provider_status(
    conn: &Connection,
    kind: ProviderKind,
    id: i64,
    status: &ProviderStatus,
) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO provider_status (kind, provider_id, initial_failure, most_recent_failure,
            escalation_level, disabled_till, last_rss_sync_release_info)
         VALUES (:kind, :id, :initial, :recent, :level, :till, :last)
         ON CONFLICT (kind, provider_id) DO UPDATE SET initial_failure = :initial,
            most_recent_failure = :recent, escalation_level = :level, disabled_till = :till,
            last_rss_sync_release_info = :last",
        named_params! {
            ":kind": kind.as_str(), ":id": id,
            ":initial": ts(status.initial_failure), ":recent": ts(status.most_recent_failure),
            ":level": status.escalation_level, ":till": ts(status.disabled_till),
            ":last": status.last_rss_sync_release_info,
        },
    )?;
    Ok(())
}

pub fn delete_provider_status(conn: &Connection, kind: ProviderKind, id: i64) -> anyhow::Result<()> {
    conn.execute(
        "DELETE FROM provider_status WHERE kind = ?1 AND provider_id = ?2",
        params![kind.as_str(), id],
    )?;
    Ok(())
}

fn status_from_row(r: &Row) -> rusqlite::Result<ProviderStatus> {
    let bad = |e: anyhow::Error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, e.into())
    };
    Ok(ProviderStatus {
        initial_failure: parse_ts(r.get("initial_failure")?).map_err(bad)?,
        most_recent_failure: parse_ts(r.get("most_recent_failure")?).map_err(bad)?,
        escalation_level: r.get("escalation_level")?,
        disabled_till: parse_ts(r.get("disabled_till")?).map_err(bad)?,
        last_rss_sync_release_info: r.get("last_rss_sync_release_info")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use chrono::TimeZone;

    #[test]
    fn round_trips_and_goes_with_the_provider() {
        let db = Db::open_in_memory().unwrap();
        db.call_blocking(|conn| {
            let kind = ProviderKind::Indexer;
            assert_eq!(provider_status(conn, kind, 7)?, ProviderStatus::default());
            let now = Utc.with_ymd_and_hms(2026, 11, 7, 12, 0, 0).unwrap();
            let status = ProviderStatus {
                initial_failure: Some(now),
                most_recent_failure: Some(now),
                escalation_level: 2,
                disabled_till: Some(now + chrono::Duration::minutes(5)),
                last_rss_sync_release_info: Some(r#"{"title":"x"}"#.into()),
            };
            upsert_provider_status(conn, kind, 7, &status)?;
            assert_eq!(provider_status(conn, kind, 7)?, status);
            assert!(status.is_disabled(now));
            assert!(!status.is_disabled(now + chrono::Duration::minutes(5)));
            let cleared = ProviderStatus {
                escalation_level: 0,
                disabled_till: None,
                ..status.clone()
            };
            upsert_provider_status(conn, kind, 7, &cleared)?;
            assert_eq!(provider_status(conn, kind, 7)?, cleared);
            assert_eq!(provider_statuses(conn, kind)?.len(), 1);
            assert!(provider_statuses(conn, ProviderKind::DownloadClient)?.is_empty());
            delete_provider_status(conn, kind, 7)?;
            assert_eq!(provider_status(conn, kind, 7)?, ProviderStatus::default());
            Ok(())
        })
        .unwrap();
    }
}
