//! The originals' `DownloadHistory`: what happened to each download, by the
//! id the client knows it by.

use chrono::{DateTime, Utc};
use rusqlite::{named_params, Connection, Row};
use serde_json::Value;

use super::{from_json, parse_enum, parse_ts_required, to_json, ts};
use crate::model::Protocol;

/// `DownloadHistoryEventType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadHistoryEvent {
    DownloadGrabbed,
    DownloadImported,
    DownloadFailed,
    DownloadIgnored,
    FileImported,
}

impl DownloadHistoryEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            DownloadHistoryEvent::DownloadGrabbed => "downloadGrabbed",
            DownloadHistoryEvent::DownloadImported => "downloadImported",
            DownloadHistoryEvent::DownloadFailed => "downloadFailed",
            DownloadHistoryEvent::DownloadIgnored => "downloadIgnored",
            DownloadHistoryEvent::FileImported => "fileImported",
        }
    }

    pub fn parse(s: &str) -> Option<DownloadHistoryEvent> {
        [
            DownloadHistoryEvent::DownloadGrabbed,
            DownloadHistoryEvent::DownloadImported,
            DownloadHistoryEvent::DownloadFailed,
            DownloadHistoryEvent::DownloadIgnored,
            DownloadHistoryEvent::FileImported,
        ]
        .into_iter()
        .find(|e| e.as_str().eq_ignore_ascii_case(s))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DownloadHistoryEntry {
    pub id: i64,
    pub event_type: DownloadHistoryEvent,
    pub series_id: i64,
    pub download_id: String,
    pub source_title: String,
    pub date: DateTime<Utc>,
    pub protocol: Protocol,
    pub indexer_id: i64,
    pub download_client_id: i64,
    /// The release as grabbed, when the row came from a grab.
    pub release: Option<Value>,
    pub data: Value,
}

pub fn insert(conn: &Connection, h: &DownloadHistoryEntry) -> anyhow::Result<i64> {
    let max: Option<i64> = conn.query_row("SELECT max(id) FROM download_history", [], |r| r.get(0))?;
    let id = if h.id > 0 { h.id } else { max.unwrap_or(0) + 1 };
    conn.execute(
        "INSERT INTO download_history (id, event_type, series_id, download_id, source_title, date, protocol,
            indexer_id, download_client_id, release, data)
         VALUES (:id, :event_type, :series_id, :download_id, :source_title, :date, :protocol,
            :indexer_id, :download_client_id, :release, :data)",
        named_params! {
            ":id": id, ":event_type": h.event_type.as_str(), ":series_id": h.series_id,
            ":download_id": h.download_id, ":source_title": h.source_title, ":date": ts(Some(h.date)),
            ":protocol": h.protocol.as_str(), ":indexer_id": h.indexer_id,
            ":download_client_id": h.download_client_id,
            ":release": h.release.as_ref().map(to_json).transpose()?, ":data": to_json(&h.data)?,
        },
    )?;
    Ok(id)
}

fn from_row(row: &Row) -> anyhow::Result<DownloadHistoryEntry> {
    let release: Option<String> = row.get("release")?;
    Ok(DownloadHistoryEntry {
        id: row.get("id")?,
        event_type: parse_enum!(DownloadHistoryEvent, row.get("event_type")?)?,
        series_id: row.get("series_id")?,
        download_id: row.get("download_id")?,
        source_title: row.get("source_title")?,
        date: parse_ts_required(row.get("date")?)?,
        protocol: parse_enum!(Protocol, row.get("protocol")?)?,
        indexer_id: row.get("indexer_id")?,
        download_client_id: row.get("download_client_id")?,
        release: release.map(|r| from_json::<Value>(Some(r))).transpose()?,
        data: from_json(row.get("data")?)?,
    })
}

/// `GetLatestDownloadHistoryItem`: the newest row of a download.
pub fn latest_for_download_id(
    conn: &Connection,
    download_id: &str,
) -> anyhow::Result<Option<DownloadHistoryEntry>> {
    let mut stmt = conn.prepare(
        "SELECT * FROM download_history WHERE download_id = ?1 ORDER BY date DESC, id DESC LIMIT 1",
    )?;
    let mut rows = stmt.query([download_id])?;
    match rows.next()? {
        Some(row) => Ok(Some(from_row(row)?)),
        None => Ok(None),
    }
}

/// `GetLatestGrab`: the newest grab row of a download.
pub fn latest_grab(conn: &Connection, download_id: &str) -> anyhow::Result<Option<DownloadHistoryEntry>> {
    let mut stmt = conn.prepare(
        "SELECT * FROM download_history WHERE download_id = ?1 AND event_type = 'downloadGrabbed'
         ORDER BY date DESC, id DESC LIMIT 1",
    )?;
    let mut rows = stmt.query([download_id])?;
    match rows.next()? {
        Some(row) => Ok(Some(from_row(row)?)),
        None => Ok(None),
    }
}

/// Every row, oldest first.
pub fn list(conn: &Connection) -> anyhow::Result<Vec<DownloadHistoryEntry>> {
    let mut stmt = conn.prepare("SELECT * FROM download_history ORDER BY id")?;
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(from_row(row)?);
    }
    Ok(out)
}
