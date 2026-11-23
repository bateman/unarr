//! Talking to an indexer: capabilities with their cache, feed pages, the
//! connection test with the originals' validation messages, RSS and
//! searches with their paging rules.

use crate::sync::Recover;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};

use crate::indexers::caps::{parse_caps, Capabilities};
use crate::indexers::request::{caps_url, RequestChain, RequestGenerator, SearchCriteria, MAX_PAGES};
use crate::indexers::rss::{parse_feed, ReleaseInfo};
use crate::indexers::settings::NewznabSettings;
use crate::indexers::{IndexerDefinition, IndexerError, IndexerKind};
use crate::validation::ValidationFailure;
use crate::Personality;

/// Capabilities are kept for a week, keyed by the settings, as the originals do.
const CAPS_TTL: Duration = Duration::from_secs(7 * 24 * 3600);
/// The originals stop a query after this many releases.
const MAX_RESULTS_PER_QUERY: usize = 1000;

pub struct IndexerClient {
    http: reqwest::Client,
    caps: Mutex<HashMap<String, (Capabilities, Instant)>>,
    /// Whose indexers these are: Radarr asks for movies and words its
    /// test failures its own way.
    personality: Personality,
}

/// The process-wide client, on the shared HTTP client.
pub fn shared() -> &'static IndexerClient {
    static CLIENT: OnceLock<IndexerClient> = OnceLock::new();
    CLIENT.get_or_init(|| IndexerClient::new(crate::http::shared().clone()))
}

/// What the last RSS sync of an indexer saw, to know when a sync is complete.
#[derive(Debug, Clone, PartialEq)]
pub struct LastRssRelease {
    pub publish_date: DateTime<Utc>,
    pub download_url: Option<String>,
}

/// An RSS fetch: the releases and whether the pages reached what the last
/// sync had seen.
#[derive(Debug, Clone)]
pub struct RecentFetch {
    pub releases: Vec<ReleaseInfo>,
    pub fully_updated: bool,
}

impl IndexerClient {
    /// Sonarr's client.
    pub fn new(http: reqwest::Client) -> IndexerClient {
        IndexerClient::for_personality(http, Personality::Sonarr)
    }

    /// A personality's client, on that personality's HTTP client.
    pub fn for_personality(http: reqwest::Client, personality: Personality) -> IndexerClient {
        IndexerClient {
            http,
            caps: Mutex::new(HashMap::new()),
            personality,
        }
    }

    pub fn personality(&self) -> Personality {
        self.personality
    }

    fn movies(&self) -> bool {
        self.personality == Personality::Radarr
    }

    /// `GetRecentRequests`, the personality's.
    fn recent_requests(&self, generator: &RequestGenerator<'_>) -> RequestChain {
        if self.movies() {
            crate::movies::request::recent_requests(generator)
        } else {
            generator.recent_requests()
        }
    }

    /// `GET` a URL as the originals' indexer requests do (`Accept: rss`).
    async fn get(&self, url: &str) -> Result<(reqwest::StatusCode, Option<String>, String), IndexerError> {
        let response = self
            .http
            .get(url)
            .header(
                reqwest::header::ACCEPT,
                "application/rss+xml, text/rss+xml, application/xml, text/xml",
            )
            .send()
            .await
            .map_err(|e| IndexerError::from_reqwest(&e, url))?;
        let status = response.status();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let body = crate::http::read_text(response, crate::http::MAX_RESPONSE_BYTES)
            .await
            .map_err(|e| match e {
                crate::http::BodyError::Transport(e) => IndexerError::from_reqwest(&e, url),
                too_large => IndexerError::Feed(format!("{too_large} ({})", crate::http::redact(url))),
            })?;
        Ok((status, content_type, body))
    }

    /// `NewznabCapabilitiesProvider.GetCapabilities`.
    pub async fn capabilities(&self, settings: &NewznabSettings) -> Result<Capabilities, IndexerError> {
        let key = settings.to_value().to_string();
        if let Some((caps, at)) = self.caps.lock().recover().get(&key) {
            if at.elapsed() < CAPS_TTL {
                return Ok(caps.clone());
            }
        }
        let url = caps_url(settings);
        let (status, _, body) = self.get(&url).await?;
        if !status.is_success() {
            return Err(IndexerError::Http {
                status: status.as_u16(),
                url: url.clone(),
            });
        }
        let caps = match parse_caps(&body, &url) {
            Ok(caps) => caps,
            // An API key or limit error is a real error; anything else keeps
            // the defaults until the process restarts, as the originals do.
            Err(e @ (IndexerError::ApiKey(_) | IndexerError::RequestLimit(_) | IndexerError::Xml(_))) => {
                return Err(e)
            }
            Err(e) => {
                tracing::error!(url, error = %e, "failed to determine capabilities, using the defaults");
                Capabilities::default()
            }
        };
        self.caps
            .lock()
            .recover()
            .insert(key, (caps.clone(), Instant::now()));
        Ok(caps)
    }

    /// Forget cached capabilities (tests, settings changes).
    pub fn clear_capabilities(&self) {
        self.caps.lock().recover().clear();
    }

    /// One feed page.
    pub async fn fetch_page(&self, url: &str, kind: IndexerKind) -> Result<Vec<ReleaseInfo>, IndexerError> {
        let (status, content_type, body) = self.get(url).await?;
        if !status.is_success() {
            if status.is_server_error() {
                return Err(IndexerError::Http {
                    status: status.as_u16(),
                    url: url.to_string(),
                });
            }
            let is_xml = content_type.as_deref().is_some_and(|c| c.contains("xml"));
            if !is_xml {
                return Err(IndexerError::Http {
                    status: status.as_u16(),
                    url: url.to_string(),
                });
            }
        }
        if content_type.as_deref().is_some_and(|c| c.contains("text/html")) {
            return Err(IndexerError::Feed(
                "Indexer responded with html content. Site is likely blocked or unavailable.".into(),
            ));
        }
        parse_feed(&body, kind, url)
    }

    fn generator<'a>(&self, settings: &'a NewznabSettings, caps: &'a Capabilities) -> RequestGenerator<'a> {
        RequestGenerator {
            settings,
            caps,
            page_size: caps.page_size_for(self.personality),
            max_pages: MAX_PAGES,
        }
    }

    /// The connection test the originals run before saving: fetch the first
    /// RSS page, then check the capabilities support searching.
    pub async fn test(&self, kind: IndexerKind, settings: &NewznabSettings) -> Vec<ValidationFailure> {
        let mut failures = Vec::new();
        if let Some(f) = self.test_connection(kind, settings).await {
            failures.push(f);
        }
        if crate::validation::has_errors(&failures) {
            return failures;
        }
        if kind == IndexerKind::Torznab {
            if let Some(f) = jackett_all(settings) {
                failures.push(f);
            }
        }
        if let Some(f) = self.test_capabilities(settings).await {
            failures.push(f);
        }
        failures
    }

    async fn test_connection(
        &self,
        kind: IndexerKind,
        settings: &NewznabSettings,
    ) -> Option<ValidationFailure> {
        let caps = match self.capabilities(settings).await {
            Ok(caps) => caps,
            Err(e) => return Some(e.into_test_failure(self.personality)),
        };
        let generator = self.generator(settings, &caps);
        let chain = self.recent_requests(&generator);
        let Some(first) = chain.iter().flatten().next().and_then(|pages| pages.first()) else {
            return Some(ValidationFailure::error(
                "",
                if self.movies() {
                    "No rss feed query available. This may be an issue with the indexer or your indexer category settings."
                } else {
                    "No RSS feed query available. This may be an issue with the indexer or your indexer category settings."
                },
            ));
        };
        match self.fetch_page(first, kind).await {
            Ok(releases) if releases.is_empty() => Some(ValidationFailure::error(
                "",
                "Query successful, but no results in the configured categories were returned from your indexer. This may be an issue with the indexer or your indexer category settings.",
            )),
            Ok(_) => None,
            Err(e) => Some(e.into_test_failure(self.personality)),
        }
    }

    async fn test_capabilities(&self, settings: &NewznabSettings) -> Option<ValidationFailure> {
        match self.capabilities(settings).await {
            Ok(caps) if self.movies() => {
                // Radarr's `Newznab.TestCapabilities`.
                let supported: Vec<i64> = caps
                    .categories
                    .iter()
                    .flat_map(|c| std::iter::once(c.id).chain(c.subcategories.iter().map(|s| s.id)))
                    .collect();
                if !settings.categories.is_empty()
                    && settings.categories.iter().all(|c| !supported.contains(c))
                {
                    return Some(ValidationFailure::error(
                        "",
                        "This indexer does not support any of the selected categories! (You may need to turn on advanced settings to see them)",
                    ));
                }
                if caps.supports_search_param("q")
                    || ["q", "tmdbid", "imdbid"]
                        .iter()
                        .any(|p| caps.supports_movie_param(p))
                {
                    return None;
                }
                Some(ValidationFailure::error(
                    "",
                    "This indexer does not support searching for movies :(. Tell your indexer staff to enable this or force add the indexer by disabling search, adding the indexer and then enabling it again.",
                ))
            }
            Ok(caps) => {
                if caps.supports_search_param("q") {
                    return None;
                }
                if caps.supported_tv_search_parameters.is_some()
                    && ["q", "tvdbid", "rid"].iter().any(|p| caps.supports_tv_param(p))
                    && ["season", "ep"].iter().all(|p| caps.supports_tv_param(p))
                {
                    return None;
                }
                Some(ValidationFailure::error(
                    "",
                    "Indexer does not support required search parameters",
                ))
            }
            Err(e) => Some(ValidationFailure::error(
                "",
                &format!(
                    "Unable to connect to indexer: {}. Check the log surrounding this error for details",
                    e.exception_message()
                ),
            )),
        }
    }

    /// Fetch the pages of a request chain as `HttpIndexerBase.FetchReleases`
    /// does: tier by tier until one yields releases, stopping a query on a
    /// short page or at the result cap; for RSS, stopping once the pages
    /// reach the last release seen.
    async fn fetch_chain(
        &self,
        definition: &IndexerDefinition,
        chain: &RequestChain,
        page_size: i64,
        last_rss: Option<&LastRssRelease>,
        is_recent: bool,
    ) -> Result<RecentFetch, IndexerError> {
        let mut releases: Vec<ReleaseInfo> = Vec::new();
        let mut fully_updated = false;
        let now = Utc::now();
        for tier in chain {
            for pageable in tier {
                let mut paged: Vec<ReleaseInfo> = Vec::new();
                for url in pageable {
                    let page = self.fetch_page(url, definition.kind).await?;
                    let page_len = page.len();
                    paged.extend(page.iter().cloned());
                    if is_recent && !page.is_empty() {
                        match last_rss {
                            None => {
                                fully_updated = true;
                                break;
                            }
                            Some(last) => {
                                let oldest = page.iter().map(|r| r.publish_date).min().unwrap();
                                if oldest < last.publish_date
                                    || page.iter().any(|r| {
                                        r.download_url.is_some() && r.download_url == last.download_url
                                    })
                                {
                                    fully_updated = true;
                                    break;
                                }
                                if paged.len() >= MAX_RESULTS_PER_QUERY
                                    && oldest < now - chrono::Duration::hours(24)
                                {
                                    fully_updated = false;
                                    break;
                                }
                            }
                        }
                    } else if paged.len() >= MAX_RESULTS_PER_QUERY {
                        break;
                    }
                    if page_size == 0 || (page_len as i64) < page_size {
                        break;
                    }
                }
                releases.extend(paged.into_iter().filter(|r| {
                    !r.title.trim().is_empty()
                        && r.download_url.as_deref().is_some_and(|u| !u.trim().is_empty())
                }));
            }
            if !releases.is_empty() {
                break;
            }
        }
        Ok(RecentFetch {
            releases: cleanup(definition, releases),
            fully_updated,
        })
    }

    /// `FetchRecent`: the RSS feed.
    pub async fn fetch_recent(
        &self,
        definition: &IndexerDefinition,
        last_rss: Option<&LastRssRelease>,
    ) -> Result<RecentFetch, IndexerError> {
        let caps = self.capabilities(&definition.settings).await?;
        let generator = self.generator(&definition.settings, &caps);
        let chain = self.recent_requests(&generator);
        self.fetch_chain(
            definition,
            &chain,
            caps.page_size_for(self.personality),
            last_rss,
            true,
        )
        .await
    }

    /// `Fetch(MovieSearchCriteria)`: Radarr's search.
    pub async fn search_movie(
        &self,
        definition: &IndexerDefinition,
        criteria: &crate::movies::request::MovieSearchCriteria,
    ) -> Result<Vec<ReleaseInfo>, IndexerError> {
        let caps = self.capabilities(&definition.settings).await?;
        let generator = self.generator(&definition.settings, &caps);
        let chain = crate::movies::request::search_requests(&generator, criteria);
        Ok(self
            .fetch_chain(
                definition,
                &chain,
                caps.page_size_for(self.personality),
                None,
                false,
            )
            .await?
            .releases)
    }

    /// `Fetch(criteria)`: a search.
    pub async fn search(
        &self,
        definition: &IndexerDefinition,
        criteria: &SearchCriteria,
    ) -> Result<Vec<ReleaseInfo>, IndexerError> {
        let caps = self.capabilities(&definition.settings).await?;
        let generator = self.generator(&definition.settings, &caps);
        let chain = generator.search_requests(criteria);
        Ok(self
            .fetch_chain(
                definition,
                &chain,
                caps.page_size_for(self.personality),
                None,
                false,
            )
            .await?
            .releases)
    }
}

/// `IndexerBase.CleanupReleases`: distinct by GUID, indexer details filled
/// in, the indexer's multi languages applied to `MULTi` releases.
fn cleanup(definition: &IndexerDefinition, releases: Vec<ReleaseInfo>) -> Vec<ReleaseInfo> {
    let mut seen = std::collections::HashSet::new();
    releases
        .into_iter()
        .filter(|r| seen.insert(r.guid.clone()))
        .map(|mut r| {
            if r.languages.is_empty()
                && !definition.settings.multi_languages.is_empty()
                && crate::parser::language::has_multiple_languages(&r.title)
            {
                r.languages = definition.settings.multi_languages.clone();
            }
            r.indexer_id = definition.id;
            r.indexer = definition.name.clone();
            r.protocol = definition.kind.protocol();
            r.indexer_priority = definition.priority;
            r.season_search_maximum_single_episode_age = definition.season_search_maximum_single_episode_age;
            r
        })
        .collect()
}

/// `Torznab.JackettAll`.
fn jackett_all(settings: &NewznabSettings) -> Option<ValidationFailure> {
    let all = |s: &str| s.contains("/torznab/all") || s.contains("/api/v2.0/indexers/all/results/torznab");
    if all(&settings.api_path) || all(&settings.base_url) {
        return Some(ValidationFailure::error(
            "ApiPath",
            "Jackett's all endpoint is not supported, please add indexers individually",
        ));
    }
    None
}
