//! Timestamp conventions. The database and the compatible API both use UTC
//! with second precision and a `Z` suffix, e.g. `2002-09-20T04:00:00Z`.

use chrono::{DateTime, NaiveDate, NaiveDateTime, Timelike, Utc};

/// Format a timestamp as the API and the database spell it.
/// When this process started, as far as the services know: the first call
/// pins it. The daemon and the command runtime call it at start.
pub fn process_start() -> DateTime<Utc> {
    static START: std::sync::OnceLock<DateTime<Utc>> = std::sync::OnceLock::new();
    *START.get_or_init(Utc::now)
}

pub fn format_utc(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Parse a timestamp in any of the spellings met in the wild: ours
/// (`2002-09-20T04:00:00Z`), the originals' database (`2002-09-20 04:00:00Z`,
/// `2026-11-06 23:23:16.6186009Z`), or RFC 3339 with an offset. Fractional
/// seconds are dropped: the API and the schema work at second precision.
pub fn parse_utc(s: &str) -> Option<DateTime<Utc>> {
    parse_utc_exact(s).map(|t| t.with_nanosecond(0).unwrap_or(t))
}

fn parse_utc_exact(s: &str) -> Option<DateTime<Utc>> {
    let s = s.trim();
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Some(t.with_timezone(&Utc));
    }
    let normalized = s.replacen(' ', "T", 1);
    if let Ok(t) = DateTime::parse_from_rfc3339(&normalized) {
        return Some(t.with_timezone(&Utc));
    }
    // No zone suffix: the originals store UTC.
    let bare = normalized.trim_end_matches('Z');
    for fmt in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S"] {
        if let Ok(t) = NaiveDateTime::parse_from_str(bare, fmt) {
            return Some(t.and_utc());
        }
    }
    None
}

/// Parse a calendar date, `2002-09-20`.
pub fn parse_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_accepts_legacy_spellings() {
        let t = parse_utc("2002-09-20 04:00:00Z").unwrap();
        assert_eq!(format_utc(t), "2002-09-20T04:00:00Z");
        let t = parse_utc("2026-11-06 23:23:16.6186009Z").unwrap();
        assert_eq!(format_utc(t), "2026-11-06T23:23:16Z");
        let t = parse_utc("2026-11-06T23:23:16Z").unwrap();
        assert_eq!(format_utc(t), "2026-11-06T23:23:16Z");
        let t = parse_utc("0001-01-01 00:00:00Z").unwrap();
        assert_eq!(format_utc(t), "0001-01-01T00:00:00Z");
        assert!(parse_utc("yesterday").is_none());
        assert_eq!(parse_date("2002-09-20").unwrap().to_string(), "2002-09-20");
    }
}
