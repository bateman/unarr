//! The `pending_releases` table.

use rusqlite::{params, Connection, Row};

use super::{parse_ts_required, ts};
use crate::downloads::pending::{PendingRelease, PendingReleaseReason};

const COLUMNS: &str = "id, series_id, title, added, release, reason, series_match_type, release_source";

fn from_row(row: &Row) -> anyhow::Result<PendingRelease> {
    let release_text: String = row.get(4)?;
    let match_type: String = row.get(6)?;
    let source: String = row.get(7)?;
    Ok(PendingRelease {
        id: row.get(0)?,
        series_id: row.get(1)?,
        title: row.get(2)?,
        added: parse_ts_required(row.get(3)?)?,
        release: serde_json::from_str(&release_text)?,
        reason: PendingReleaseReason::from_i64(row.get(5)?),
        series_match_type: crate::decision::SeriesMatchType::parse(&match_type).unwrap_or_default(),
        release_source: crate::decision::ReleaseSource::parse(&source).unwrap_or_default(),
    })
}

fn query(conn: &Connection, sql: &str, args: &[&dyn rusqlite::ToSql]) -> anyhow::Result<Vec<PendingRelease>> {
    let mut stmt = conn.prepare(sql)?;
    let mut rows = stmt.query(args)?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(from_row(row)?);
    }
    Ok(out)
}

pub fn all(conn: &Connection) -> anyhow::Result<Vec<PendingRelease>> {
    query(
        conn,
        &format!("SELECT {COLUMNS} FROM pending_releases ORDER BY id"),
        &[],
    )
}

pub fn all_by_series(conn: &Connection, series_id: i64) -> anyhow::Result<Vec<PendingRelease>> {
    query(
        conn,
        &format!("SELECT {COLUMNS} FROM pending_releases WHERE series_id = ?1 ORDER BY id"),
        &[&series_id],
    )
}

/// `WithoutFallback`: joined with the series, so a deleted series' rows
/// stay out.
pub fn without_fallback(conn: &Connection) -> anyhow::Result<Vec<PendingRelease>> {
    query(
        conn,
        &format!(
            "SELECT {} FROM pending_releases p JOIN series s ON s.id = p.series_id
             WHERE p.reason != ?1 ORDER BY p.id",
            COLUMNS
                .split(", ")
                .map(|c| format!("p.{c}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        &[&PendingReleaseReason::Fallback.as_i64()],
    )
}

/// Insert a row; an id of zero takes the next free one.
pub fn insert(conn: &Connection, p: &PendingRelease) -> anyhow::Result<i64> {
    let id = if p.id > 0 {
        p.id
    } else {
        let max: Option<i64> = conn.query_row("SELECT max(id) FROM pending_releases", [], |r| r.get(0))?;
        max.unwrap_or(0) + 1
    };
    conn.execute(
        "INSERT INTO pending_releases (id, series_id, title, added, release, reason, series_match_type, release_source)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            id,
            p.series_id,
            p.title,
            ts(Some(p.added)),
            serde_json::to_string(&p.release)?,
            p.reason.as_i64(),
            p.series_match_type.as_str(),
            p.release_source.as_str(),
        ],
    )?;
    Ok(id)
}

pub fn set_reason(conn: &Connection, id: i64, reason: PendingReleaseReason) -> anyhow::Result<()> {
    conn.execute(
        "UPDATE pending_releases SET reason = ?2 WHERE id = ?1",
        params![id, reason.as_i64()],
    )?;
    Ok(())
}

pub fn delete(conn: &Connection, id: i64) -> anyhow::Result<()> {
    conn.execute("DELETE FROM pending_releases WHERE id = ?1", params![id])?;
    Ok(())
}

pub fn count(conn: &Connection) -> anyhow::Result<i64> {
    Ok(conn.query_row("SELECT count(*) FROM pending_releases", [], |r| r.get(0))?)
}
