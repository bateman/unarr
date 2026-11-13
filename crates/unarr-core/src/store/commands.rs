//! The `commands` table.

use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, Connection, OptionalExtension, Row};

use super::{parse_ts, parse_ts_required, ts};
use crate::commands::{
    CommandBody, CommandModel, CommandPriority, CommandResult, CommandStatus, CommandTrigger,
};

pub fn next_id(conn: &Connection) -> anyhow::Result<i64> {
    let max: Option<i64> = conn.query_row("SELECT max(id) FROM commands", [], |r| r.get(0))?;
    // An imported database continues the original's numbering, which its
    // emptied table no longer shows.
    let seeded: Option<i64> = conn
        .query_row("SELECT value FROM sequences WHERE name = 'commands'", [], |r| {
            r.get(0)
        })
        .optional()?;
    Ok(max.unwrap_or(0).max(seeded.unwrap_or(0)) + 1)
}

pub fn insert(conn: &Connection, personality: crate::Personality, c: &CommandModel) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO commands (id, name, body, priority, status, result, queued_at, started_at, ended_at,
                               duration_ms, exception, trigger, message)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            c.id,
            c.name(),
            c.body_json(personality).to_string(),
            c.priority.as_i64(),
            c.status.as_i64(),
            c.result.as_i64(),
            ts(Some(c.queued_at)),
            ts(c.started_at),
            ts(c.ended_at),
            c.duration.map(|d| d.num_milliseconds()),
            c.exception,
            c.trigger.as_i64(),
            c.message,
        ],
    )?;
    Ok(())
}

/// Write the state fields of a command already inserted.
pub fn update(conn: &Connection, c: &CommandModel) -> anyhow::Result<()> {
    conn.execute(
        "UPDATE commands SET status = ?2, result = ?3, started_at = ?4, ended_at = ?5, duration_ms = ?6,
                             exception = ?7, message = ?8
         WHERE id = ?1",
        params![
            c.id,
            c.status.as_i64(),
            c.result.as_i64(),
            ts(c.started_at),
            ts(c.ended_at),
            c.duration.map(|d| d.num_milliseconds()),
            c.exception,
            c.message,
        ],
    )?;
    Ok(())
}

const COLUMNS: &str = "id, name, body, priority, status, result, queued_at, started_at, ended_at, duration_ms, exception, trigger, message";

/// A stored command, read as a command of `personality`: `RenameFiles` and
/// `ManualImport` are two different commands under one name.
fn from_row(personality: crate::Personality, row: &Row) -> anyhow::Result<CommandModel> {
    let name: String = row.get(1)?;
    let body_text: String = row.get(2)?;
    let body_json: serde_json::Value = serde_json::from_str(&body_text).unwrap_or_default();
    let body = CommandBody::parse_for(personality, &name, &body_json)
        .ok_or_else(|| anyhow::anyhow!("unknown command {name:?} in the commands table"))?;
    let duration_ms: Option<i64> = row.get(9)?;
    Ok(CommandModel {
        id: row.get(0)?,
        send_updates_to_client: body_json["sendUpdatesToClient"]
            .as_bool()
            .unwrap_or(body.sends_updates()),
        suppress_messages: body_json["suppressMessages"].as_bool().unwrap_or(false),
        client_user_agent: body_json["clientUserAgent"].as_str().map(String::from),
        last_execution_time: body_json["lastExecutionTime"]
            .as_str()
            .and_then(crate::time::parse_utc),
        last_start_time: body_json["lastStartTime"]
            .as_str()
            .and_then(crate::time::parse_utc),
        type_exclusive: body_json["isTypeExclusive"].as_bool(),
        body,
        priority: CommandPriority::from_i64(row.get(3)?),
        status: CommandStatus::from_i64(row.get(4)?),
        result: CommandResult::from_i64(row.get(5)?),
        queued_at: parse_ts_required(row.get(6)?)?,
        started_at: parse_ts(row.get(7)?)?,
        ended_at: parse_ts(row.get(8)?)?,
        duration: duration_ms.map(Duration::milliseconds),
        exception: row.get(10)?,
        trigger: CommandTrigger::from_i64(row.get(11)?),
        message: row.get(12)?,
    })
}

pub fn get(
    conn: &Connection,
    personality: crate::Personality,
    id: i64,
) -> anyhow::Result<Option<CommandModel>> {
    let mut stmt = conn.prepare(&format!("SELECT {COLUMNS} FROM commands WHERE id = ?1"))?;
    let mut rows = stmt.query(params![id])?;
    match rows.next()? {
        Some(row) => Ok(Some(from_row(personality, row)?)),
        None => Ok(None),
    }
}

/// `Queued`: commands to run again after a restart.
pub fn queued(conn: &Connection, personality: crate::Personality) -> anyhow::Result<Vec<CommandModel>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM commands WHERE status = ?1 ORDER BY id"
    ))?;
    let mut rows = stmt.query(params![CommandStatus::Queued.as_i64()])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        match from_row(personality, row) {
            Ok(c) => out.push(c),
            Err(e) => tracing::warn!(error = %e, "skipping a stored command"),
        }
    }
    Ok(out)
}

/// `OrphanStarted`: commands a previous run left started.
pub fn orphan_started(conn: &Connection, now: DateTime<Utc>) -> anyhow::Result<()> {
    conn.execute(
        "UPDATE commands SET status = ?1, ended_at = ?2 WHERE status = ?3",
        params![
            CommandStatus::Orphaned.as_i64(),
            ts(Some(now)),
            CommandStatus::Started.as_i64()
        ],
    )?;
    Ok(())
}

/// `Trim`: commands that ended more than a day ago.
pub fn trim(conn: &Connection, now: DateTime<Utc>) -> anyhow::Result<()> {
    conn.execute(
        "DELETE FROM commands WHERE ended_at IS NOT NULL AND ended_at < ?1",
        params![ts(Some(now - Duration::days(1)))],
    )?;
    Ok(())
}
