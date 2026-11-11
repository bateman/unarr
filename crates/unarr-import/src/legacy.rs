//! Reading an original application's SQLite database: what the importers
//! of every personality share.
//!
//! The legacy schema is read table by table through `SELECT *` and columns are
//! fetched by name, so a column added by a later migration is simply absent
//! (and defaulted) on older databases.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Context;
use chrono::{DateTime, NaiveDate, Utc};
use rusqlite::types::{FromSql, Value as SqlValue};
use rusqlite::{Connection, OpenFlags, Row, Statement};
use serde::de::DeserializeOwned;
use serde_json::Value;
use unarr_core::indexers::rss::ReleaseInfo;
use unarr_core::model::*;
use unarr_core::store::provider_status::ProviderStatus;
use unarr_core::time;

/// What an import did.
#[derive(Debug, Default)]
pub struct ImportReport {
    pub migration_version: i64,
    /// Rows imported per legacy table.
    pub counts: BTreeMap<&'static str, usize>,
    /// Human-readable notes about anything skipped or defaulted.
    pub notes: Vec<String>,
    /// Legacy tables with rows the importer does not carry over, with
    /// their row counts.
    pub skipped: BTreeMap<String, i64>,
}

/// Tables with rows that nothing reads, by name: those not in `handled`.
pub(crate) fn skipped_tables(legacy: &Connection, handled: &[&str]) -> anyhow::Result<BTreeMap<String, i64>> {
    let mut stmt = legacy.prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")?;
    let names: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<_, _>>()?;
    let mut out = BTreeMap::new();
    for name in names {
        if handled.contains(&name.as_str()) {
            continue;
        }
        let rows: i64 = legacy.query_row(&format!("SELECT count(*) FROM \"{name}\""), [], |r| r.get(0))?;
        if rows > 0 {
            out.insert(name, rows);
        }
    }
    Ok(out)
}

/// A release as the originals store it (`ReleaseInfo` serialised by their
/// JSON settings: `downloadProtocol`, languages as objects), mapped to
/// Unarr's.
pub(crate) fn legacy_release(v: &Value) -> ReleaseInfo {
    let text = |k: &str| v[k].as_str().map(String::from);
    let int = |k: &str| v[k].as_i64().unwrap_or(0);
    ReleaseInfo {
        guid: text("guid").unwrap_or_default(),
        title: text("title").unwrap_or_default(),
        size: int("size"),
        download_url: text("downloadUrl"),
        info_url: text("infoUrl"),
        comment_url: text("commentUrl"),
        indexer_id: int("indexerId"),
        indexer: text("indexer").unwrap_or_default(),
        indexer_priority: v["indexerPriority"].as_i64().unwrap_or(25),
        season_search_maximum_single_episode_age: int("seasonSearchMaximumSingleEpisodeAge"),
        protocol: match v["downloadProtocol"]
            .as_str()
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("usenet") => Protocol::Usenet,
            Some("torrent") => Protocol::Torrent,
            _ => protocol(int("downloadProtocol")),
        },
        tvdb_id: int("tvdbId"),
        tv_rage_id: int("tvRageId"),
        imdb_id: text("imdbId"),
        publish_date: text("publishDate")
            .and_then(|s| time::parse_utc(&s))
            .unwrap_or(DateTime::<Utc>::UNIX_EPOCH),
        languages: v["languages"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|l| l["id"].as_i64().or_else(|| l.as_i64()))
                    .collect()
            })
            .unwrap_or_default(),
        indexer_flags: int("indexerFlags"),
        magnet_url: text("magnetUrl"),
        info_hash: text("infoHash"),
        seeders: v["seeders"].as_i64(),
        peers: v["peers"].as_i64(),
        container: text("container"),
        pending_release_reason: None,
    }
}

/// The status rows of a provider kind.
pub(crate) fn read_statuses(
    legacy: &Connection,
    table: &'static str,
) -> anyhow::Result<Vec<(i64, ProviderStatus)>> {
    read_table(legacy, table, |r| {
        Ok((
            r.req("ProviderId")?,
            ProviderStatus {
                initial_failure: r.timestamp("InitialFailure")?,
                most_recent_failure: r.timestamp("MostRecentFailure")?,
                escalation_level: r.or("EscalationLevel", 0)?,
                disabled_till: r.timestamp("DisabledTill")?,
                last_rss_sync_release_info: r
                    .json_opt("LastRssSyncReleaseInfo")?
                    .map(|v| serde_json::to_string(&legacy_release(&v)).unwrap_or_default()),
            },
        ))
    })
}

/// Open the legacy database read-only, and only read-only: the original files
/// are never modified, as the README promises. A leftover `-wal` is replayed
/// by SQLite as long as the `-shm` can be created, which read-only mode allows
/// when the directory is writable; a leftover rollback journal cannot be
/// applied without writing, so that import stops with a message.
pub(crate) fn open_legacy(path: &Path) -> anyhow::Result<Connection> {
    let ro = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(path, ro)
        .with_context(|| format!("opening {} read-only", path.display()))?;
    // Force a read so journal recovery problems surface here.
    conn.query_row("SELECT count(*) FROM VersionInfo", [], |r| r.get::<_, i64>(0))
        .with_context(|| {
            format!(
                "reading {} read-only; if the original application left a journal behind, \
                 stop it or copy its files (the database with any -wal and -shm) and import the copy",
                path.display()
            )
        })?;
    Ok(conn)
}

/// Row with column lookup by name that tolerates absent columns.
pub(crate) struct LegacyRow<'a> {
    row: &'a Row<'a>,
    names: &'a [String],
    table: &'static str,
}

impl LegacyRow<'_> {
    fn index(&self, name: &str) -> Option<usize> {
        self.names.iter().position(|n| n == name)
    }

    /// Required column.
    pub(crate) fn req<T: FromSql>(&self, name: &str) -> anyhow::Result<T> {
        let i = self
            .index(name)
            .ok_or_else(|| anyhow::anyhow!("{}.{name} is missing in this database", self.table))?;
        self.row
            .get::<_, T>(i)
            .with_context(|| format!("{}.{name}", self.table))
    }

    /// Column that may be NULL or absent.
    pub(crate) fn opt<T: FromSql>(&self, name: &str) -> anyhow::Result<Option<T>> {
        match self.index(name) {
            None => Ok(None),
            Some(i) => self
                .row
                .get::<_, Option<T>>(i)
                .with_context(|| format!("{}.{name}", self.table)),
        }
    }

    /// Column with a default when NULL or absent.
    pub(crate) fn or<T: FromSql>(&self, name: &str, default: T) -> anyhow::Result<T> {
        Ok(self.opt(name)?.unwrap_or(default))
    }

    pub(crate) fn text(&self, name: &str) -> anyhow::Result<Option<String>> {
        // Sonarr stores the empty string for "no value" in several text
        // columns; the API omits those, so they become None here.
        Ok(self.opt::<String>(name)?.filter(|s| !s.is_empty()))
    }

    pub(crate) fn flag(&self, name: &str, default: bool) -> anyhow::Result<bool> {
        Ok(self.or::<i64>(name, default as i64)? != 0)
    }

    pub(crate) fn json<T: DeserializeOwned + Default>(&self, name: &str) -> anyhow::Result<T> {
        match self.opt::<String>(name)? {
            Some(s) if !s.trim().is_empty() => {
                serde_json::from_str(&s).with_context(|| format!("{}.{name} JSON", self.table))
            }
            _ => Ok(T::default()),
        }
    }

    pub(crate) fn json_opt(&self, name: &str) -> anyhow::Result<Option<Value>> {
        match self.opt::<String>(name)? {
            Some(s) if !s.trim().is_empty() => Ok(Some(
                serde_json::from_str(&s).with_context(|| format!("{}.{name} JSON", self.table))?,
            )),
            _ => Ok(None),
        }
    }

    pub(crate) fn timestamp(&self, name: &str) -> anyhow::Result<Option<DateTime<Utc>>> {
        match self.opt::<SqlValue>(name)? {
            None | Some(SqlValue::Null) => Ok(None),
            Some(SqlValue::Text(s)) => time::parse_utc(&s)
                .map(Some)
                .ok_or_else(|| anyhow::anyhow!("{}.{name}: bad timestamp {s:?}", self.table)),
            Some(other) => anyhow::bail!("{}.{name}: unexpected {other:?}", self.table),
        }
    }

    pub(crate) fn timestamp_req(&self, name: &str) -> anyhow::Result<DateTime<Utc>> {
        self.timestamp(name)?
            .ok_or_else(|| anyhow::anyhow!("{}.{name} is NULL", self.table))
    }

    pub(crate) fn date(&self, name: &str) -> anyhow::Result<Option<NaiveDate>> {
        match self.text(name)? {
            None => Ok(None),
            Some(s) => time::parse_date(&s)
                .map(Some)
                .ok_or_else(|| anyhow::anyhow!("{}.{name}: bad date {s:?}", self.table)),
        }
    }
}

/// Run `SELECT * FROM table` and map every row.
pub(crate) fn read_table<T>(
    legacy: &Connection,
    table: &'static str,
    mut map: impl FnMut(&LegacyRow) -> anyhow::Result<T>,
) -> anyhow::Result<Vec<T>> {
    let exists: i64 = legacy.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |r| r.get(0),
    )?;
    if exists == 0 {
        tracing::warn!(table, "legacy table missing, skipping");
        return Ok(Vec::new());
    }
    let mut stmt: Statement = legacy.prepare(&format!("SELECT * FROM \"{table}\" ORDER BY Id"))?;
    let names: Vec<String> = stmt.column_names().map(|s| s.to_string()).collect();
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(map(&LegacyRow {
            row,
            names: &names,
            table,
        })?);
    }
    Ok(out)
}

/// `{"quality": 7, "revision": {"version": 1, "real": 0, "isRepack": false}}`
pub(crate) fn quality_model(v: Option<Value>) -> QualityModel {
    let Some(v) = v else {
        return QualityModel::default();
    };
    QualityModel {
        quality_id: v["quality"].as_i64().unwrap_or(0),
        version: v["revision"]["version"].as_i64().unwrap_or(1),
        real: v["revision"]["real"].as_i64().unwrap_or(0),
        is_repack: v["revision"]["isRepack"].as_bool().unwrap_or(false),
    }
}

pub(crate) fn protocol(v: i64) -> Protocol {
    match v {
        1 => Protocol::Usenet,
        2 => Protocol::Torrent,
        _ => Protocol::Unknown,
    }
}

pub(crate) fn history_event(v: i64) -> HistoryEvent {
    HistoryEvent::from_number(v).unwrap_or(HistoryEvent::Unknown)
}

/// A provider row (indexer, download client, notification, import list,
/// metadata consumer): the columns every kind has, with the flags of its
/// kind as `attributes`.
pub(crate) fn provider_row(r: &LegacyRow, kind: ProviderKind, attributes: Value) -> anyhow::Result<Provider> {
    Ok(Provider {
        kind,
        id: r.req("Id")?,
        name: r.req("Name")?,
        implementation: r.req("Implementation")?,
        config_contract: r.or("ConfigContract", String::new())?,
        settings: r.json_opt("Settings")?.unwrap_or(serde_json::json!({})),
        tags: r.json("Tags")?,
        attributes,
    })
}
