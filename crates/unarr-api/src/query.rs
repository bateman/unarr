//! Query string access that tolerates the originals' conventions: repeated
//! parameters (`episodeIds=1&episodeIds=2`), case-insensitive names, booleans
//! spelled `true`/`false`.

use axum::http::Uri;
use chrono::{DateTime, Utc};

use crate::auth::percent_decode;

#[derive(Debug, Default, Clone)]
pub struct Params(Vec<(String, String)>);

impl Params {
    pub fn from_uri(uri: &Uri) -> Params {
        let mut out = Vec::new();
        if let Some(q) = uri.query() {
            for pair in q.split('&').filter(|p| !p.is_empty()) {
                let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
                out.push((percent_decode(k), percent_decode(v)));
            }
        }
        Params(out)
    }

    /// Every pair, in the order sent.
    pub fn pairs(&self) -> &[(String, String)] {
        &self.0
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn all(&self, name: &str) -> Vec<&str> {
        self.0
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
            .collect()
    }

    pub fn bool(&self, name: &str, default: bool) -> bool {
        match self.get(name) {
            Some(v) => v.eq_ignore_ascii_case("true") || v == "1",
            None => default,
        }
    }

    pub fn i64(&self, name: &str) -> Option<i64> {
        self.get(name).and_then(|v| v.parse().ok())
    }

    /// Repeated or comma-separated integers.
    pub fn ints(&self, name: &str) -> Vec<i64> {
        self.all(name)
            .iter()
            .flat_map(|v| v.split(','))
            .filter_map(|v| v.trim().parse().ok())
            .collect()
    }

    /// A timestamp, accepting a bare date (`2026-11-06`) as midnight UTC.
    pub fn time(&self, name: &str) -> Option<DateTime<Utc>> {
        let v = self.get(name)?;
        unarr_core::time::parse_utc(v).or_else(|| {
            unarr_core::time::parse_date(v).and_then(|d| d.and_hms_opt(0, 0, 0).map(|t| t.and_utc()))
        })
    }

    /// `sortDirection`: `ascending`/`descending`, else None.
    pub fn descending(&self) -> Option<bool> {
        self.get("sortDirection")
            .and_then(|v| match v.to_ascii_lowercase().as_str() {
                "descending" => Some(true),
                "ascending" => Some(false),
                _ => None,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_repeated_and_typed_parameters() {
        let uri: Uri = "/x?seriesId=2&episodeIds=1&episodeIds=2&includeImages=true&start=2002-09-01T00:00:00Z&sortDirection=descending&eventType=3,5"
            .parse()
            .unwrap();
        let p = Params::from_uri(&uri);
        assert_eq!(p.i64("seriesid"), Some(2));
        assert_eq!(p.ints("episodeIds"), vec![1, 2]);
        assert_eq!(p.ints("eventType"), vec![3, 5]);
        assert!(p.bool("includeImages", false));
        assert!(!p.bool("includeSeries", false));
        assert!(p.time("start").is_some());
        assert_eq!(p.descending(), Some(true));
    }
}
