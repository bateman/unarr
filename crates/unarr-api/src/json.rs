//! Serialization helpers for the compatible JSON dialect: UTC timestamps at
//! second precision with a `Z` suffix, dates as `YYYY-MM-DD`.
//!
//! Use with `#[serde(with = "json::ts")]`, `#[serde(with = "json::ts_opt")]`
//! and `#[serde(with = "json::date_opt")]`.

pub mod ts {
    use chrono::{DateTime, Utc};
    use serde::Serializer;

    pub fn serialize<S: Serializer>(t: &DateTime<Utc>, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&unarr_core::time::format_utc(*t))
    }
}

pub mod ts_opt {
    use chrono::{DateTime, Utc};
    use serde::Serializer;

    pub fn serialize<S: Serializer>(t: &Option<DateTime<Utc>>, s: S) -> Result<S::Ok, S::Error> {
        match t {
            Some(t) => s.serialize_str(&unarr_core::time::format_utc(*t)),
            None => s.serialize_none(),
        }
    }
}

pub mod date_opt {
    use chrono::NaiveDate;
    use serde::Serializer;

    pub fn serialize<S: Serializer>(d: &Option<NaiveDate>, s: S) -> Result<S::Ok, S::Error> {
        match d {
            Some(d) => s.serialize_str(&d.format("%Y-%m-%d").to_string()),
            None => s.serialize_none(),
        }
    }
}
