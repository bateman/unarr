//! Where series and episode metadata come from.
//!
//! The originals call the Servarr team's proxies (Skyhook for Sonarr) and
//! Unarr does the same (`docs/DESIGN.md` section 7, option A), behind
//! [`MetadataProvider`]. The trait's output is the originals' own model:
//! a [`Series`] and its [`Episode`]s as `SkyHookProxy` maps them, so that a
//! provider built on other sources (TVMaze, XEM, TVDB) stays additive.

pub mod augment;
pub mod skyhook;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::model::{Episode, Series};

pub use skyhook::Skyhook;

/// A boxed future, so that the trait stays object-safe.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A series as the provider knows it, with its episodes. Ids are 0: the
/// caller decides whether these become rows or update existing ones.
#[derive(Debug, Clone)]
pub struct SeriesInfo {
    pub series: Series,
    pub episodes: Vec<Episode>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MetadataError {
    /// `SeriesNotFoundException`: the provider answered 404 for this id.
    SeriesNotFound(i64),
    /// The provider could not be reached or answered with an error status
    /// (`HttpException`, `WebException`).
    Unavailable(String),
    /// The provider answered something that does not parse.
    Invalid(String),
}

impl std::fmt::Display for MetadataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MetadataError::SeriesNotFound(id) => write!(
                f,
                "Series with tvdbid {id} was not found, it may have been removed from TheTVDB."
            ),
            MetadataError::Unavailable(m) | MetadataError::Invalid(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for MetadataError {}

/// `IProvideSeriesInfo` and the provider half of `ISearchForNewSeries`.
pub trait MetadataProvider: Send + Sync {
    /// `GetSeriesInfo`: the series and every episode the provider lists.
    fn series_info(&self, tvdb_id: i64) -> BoxFuture<'_, Result<SeriesInfo, MetadataError>>;

    /// The provider's search for a term (`imdb:`, `tmdb:`, `tvmaze:`,
    /// `mal:`, `anilist:` prefixes are the provider's to interpret). The
    /// results are new series; [`crate::series::lookup`] swaps in the ones
    /// already in the library and handles `tvdb:` itself.
    fn search(&self, term: &str) -> BoxFuture<'_, Result<Vec<Series>, MetadataError>>;
}

pub type SharedProvider = Arc<dyn MetadataProvider>;

/// Where the metadata hosts are. Production asks the real hosts; the tests
/// and the golden replay ask a mock that serves every host under
/// `<base>/<host>/`, which is what `rewrite_to` sets.
#[derive(Debug, Clone, Default)]
pub struct Upstreams {
    rewrite_to: Option<String>,
}

impl Upstreams {
    /// The real hosts.
    pub fn real() -> Upstreams {
        Upstreams::default()
    }

    /// Every `https://<host>/<path>` becomes `<base>/<host>/<path>`.
    pub fn mock(base: &str) -> Upstreams {
        Upstreams {
            rewrite_to: Some(base.trim_end_matches('/').to_string()),
        }
    }

    /// The URL to ask for `url`, an absolute `https://` or `http://` one.
    pub fn url(&self, url: &str) -> String {
        let Some(base) = &self.rewrite_to else {
            return url.to_string();
        };
        let rest = url
            .strip_prefix("https://")
            .or_else(|| url.strip_prefix("http://"))
            .unwrap_or(url);
        format!("{base}/{rest}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upstreams_rewrite_only_for_the_mock() {
        let url = "https://skyhook.sonarr.tv/v1/tvdb/shows/en/1";
        assert_eq!(Upstreams::real().url(url), url);
        assert_eq!(
            Upstreams::mock("http://127.0.0.1:5/").url(url),
            "http://127.0.0.1:5/skyhook.sonarr.tv/v1/tvdb/shows/en/1"
        );
    }
}
