//! Persistence of the domain model. Plain functions over a `rusqlite`
//! connection; callers obtain the connection through [`crate::db::Db`].

pub mod catalog;
pub mod commands;
pub mod config;
pub mod download_history;
pub mod episodes;
pub mod history;
pub mod paging;
pub mod pending;
pub mod provider_status;
pub mod providers;
pub mod scene;
pub mod series;
pub mod stats;

use chrono::{DateTime, NaiveDate, Utc};
use rusqlite::Row;
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::model::QualityModel;
use crate::time;

/// Serialize a value for a JSON text column.
pub(crate) fn to_json<T: Serialize>(v: &T) -> anyhow::Result<String> {
    Ok(serde_json::to_string(v)?)
}

/// Parse a JSON text column; `NULL` and empty text become `T::default()`.
pub(crate) fn from_json<T: DeserializeOwned + Default>(s: Option<String>) -> anyhow::Result<T> {
    match s {
        Some(s) if !s.trim().is_empty() => Ok(serde_json::from_str(&s)?),
        _ => Ok(T::default()),
    }
}

pub(crate) fn ts(t: Option<DateTime<Utc>>) -> Option<String> {
    t.map(time::format_utc)
}

pub(crate) fn parse_ts(s: Option<String>) -> anyhow::Result<Option<DateTime<Utc>>> {
    match s {
        None => Ok(None),
        Some(s) => time::parse_utc(&s)
            .map(Some)
            .ok_or_else(|| anyhow::anyhow!("bad timestamp {s:?}")),
    }
}

pub(crate) fn parse_ts_required(s: String) -> anyhow::Result<DateTime<Utc>> {
    time::parse_utc(&s).ok_or_else(|| anyhow::anyhow!("bad timestamp {s:?}"))
}

pub(crate) fn parse_date(s: Option<String>) -> anyhow::Result<Option<NaiveDate>> {
    match s {
        None => Ok(None),
        Some(s) => time::parse_date(&s)
            .map(Some)
            .ok_or_else(|| anyhow::anyhow!("bad date {s:?}")),
    }
}

/// Read the four quality columns every quality-bearing table shares.
pub(crate) fn quality_from_row(row: &Row) -> rusqlite::Result<QualityModel> {
    Ok(QualityModel {
        quality_id: row.get("quality_id")?,
        version: row.get("revision_version")?,
        real: row.get("revision_real")?,
        is_repack: row.get("revision_is_repack")?,
    })
}

/// Parse a string-backed enumeration column.
macro_rules! parse_enum {
    ($ty:ty, $value:expr) => {{
        let s: String = $value;
        <$ty>::parse(&s).ok_or_else(|| anyhow::anyhow!(concat!("bad ", stringify!($ty), " {:?}"), s))
    }};
}
pub(crate) use parse_enum;
