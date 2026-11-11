//! Indexers: Newznab and Torznab, the two implementations Prowlarr pushes.
//! Settings, capabilities, request building, feed parsing, the connection
//! test and the fetch logic are ports of the originals; the test messages
//! are their localised strings, since the API and the UI show them.

pub mod caps;
pub mod client;
pub mod request;
pub mod rss;
pub mod service;
pub mod settings;
pub mod status;

pub use client::{shared, IndexerClient, LastRssRelease, RecentFetch};
pub use rss::ReleaseInfo;
pub use service::{IndexerService, Purpose, SearchResult};
pub use settings::NewznabSettings;

use crate::model::{Protocol, Provider};
use crate::validation::ValidationFailure;
use crate::Personality;

/// The indexer implementations this phase talks to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexerKind {
    Newznab,
    Torznab,
}

impl IndexerKind {
    /// By the `implementation` name a provider carries.
    pub fn from_implementation(name: &str) -> Option<IndexerKind> {
        match name {
            "Newznab" => Some(IndexerKind::Newznab),
            "Torznab" => Some(IndexerKind::Torznab),
            _ => None,
        }
    }

    pub fn protocol(self) -> Protocol {
        match self {
            IndexerKind::Newznab => Protocol::Usenet,
            IndexerKind::Torznab => Protocol::Torrent,
        }
    }
}

/// An indexer as the fetch logic needs it: the provider's flags and its
/// decoded settings.
#[derive(Debug, Clone, PartialEq)]
pub struct IndexerDefinition {
    pub id: i64,
    pub name: String,
    pub kind: IndexerKind,
    pub settings: NewznabSettings,
    pub enable_rss: bool,
    pub enable_automatic_search: bool,
    pub enable_interactive_search: bool,
    pub priority: i64,
    pub download_client_id: i64,
    pub season_search_maximum_single_episode_age: i64,
    pub tags: Vec<i64>,
}

impl IndexerDefinition {
    /// From a stored provider; `None` for implementations this phase does
    /// not drive (their settings keep round-tripping untouched).
    pub fn from_provider(p: &Provider) -> Option<IndexerDefinition> {
        let kind = IndexerKind::from_implementation(&p.implementation)?;
        let a = &p.attributes;
        Some(IndexerDefinition {
            id: p.id,
            name: p.name.clone(),
            kind,
            settings: NewznabSettings::from_value(&p.settings),
            enable_rss: a["enableRss"].as_bool().unwrap_or(false),
            enable_automatic_search: a["enableAutomaticSearch"].as_bool().unwrap_or(false),
            enable_interactive_search: a["enableInteractiveSearch"].as_bool().unwrap_or(false),
            priority: a["priority"].as_i64().unwrap_or(25),
            download_client_id: a["downloadClientId"].as_i64().unwrap_or(0),
            season_search_maximum_single_episode_age: a["seasonSearchMaximumSingleEpisodeAge"]
                .as_i64()
                .unwrap_or(0),
            tags: p.tags.clone(),
        })
    }

    /// `IndexerDefinition.Enable`.
    pub fn enabled(&self) -> bool {
        self.enable_rss || self.enable_automatic_search || self.enable_interactive_search
    }
}

/// What can go wrong talking to an indexer, named after the originals'
/// exceptions so the test messages can follow their branches.
#[derive(Debug, Clone, PartialEq)]
pub enum IndexerError {
    /// `ApiKeyException`: error code 100-199, or a key the indexer wants.
    ApiKey(String),
    /// `RequestLimitReachedException`.
    RequestLimit(String),
    /// `IndexerException` / `NewznabException`: an `<error>` or a feed
    /// problem, with the indexer's message.
    Feed(String),
    /// `UnsupportedFeedException`.
    UnsupportedFeed(String),
    /// `XmlException`.
    Xml(String),
    /// `HttpException`: a non-success status.
    Http { status: u16, url: String },
    /// `HttpRequestException`: the connection failed (`Connection refused
    /// (host:port)`, `Name or service not known (host:port)`...).
    Connect(String),
    /// `TaskCanceledException`: the request timed out.
    Timeout(String),
}

impl std::fmt::Display for IndexerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.exception_message())
    }
}

impl std::error::Error for IndexerError {}

impl IndexerError {
    /// `ex.Message`, as the originals interpolate it into their texts.
    pub fn exception_message(&self) -> String {
        match self {
            IndexerError::ApiKey(m) | IndexerError::RequestLimit(m) | IndexerError::Feed(m) => m.clone(),
            IndexerError::UnsupportedFeed(m)
            | IndexerError::Xml(m)
            | IndexerError::Connect(m)
            | IndexerError::Timeout(m) => m.clone(),
            IndexerError::Http { status, url } => format!(
                "HTTP request failed: [{status}:{}] [GET] at [{}]",
                reason_phrase(*status),
                crate::http::redact(url)
            ),
        }
    }

    /// Classify a `reqwest` error the way .NET's exceptions come out.
    pub fn from_reqwest(e: &reqwest::Error, url: &str) -> IndexerError {
        let host_port = reqwest::Url::parse(url)
            .ok()
            .map(|u| {
                format!(
                    "{}:{}",
                    u.host_str().unwrap_or(""),
                    u.port_or_known_default().unwrap_or(80)
                )
            })
            .unwrap_or_default();
        if e.is_timeout() {
            return IndexerError::Timeout(
                "The request was canceled due to the configured HttpClient.Timeout of 100 seconds elapsing."
                    .into(),
            );
        }
        if e.is_connect() {
            let text = format!("{e:?}").to_lowercase();
            let message = if text.contains("refused") {
                format!("Connection refused ({host_port})")
            } else if text.contains("dns") || text.contains("resolve") || text.contains("name") {
                format!("Name or service not known ({host_port})")
            } else if text.contains("unreachable") {
                format!("Network is unreachable ({host_port})")
            } else {
                format!("{} ({host_port})", root_cause(e))
            };
            return IndexerError::Connect(message);
        }
        if let Some(status) = e.status() {
            return IndexerError::Http {
                status: status.as_u16(),
                url: url.to_string(),
            };
        }
        IndexerError::Connect(format!("{} ({host_port})", root_cause(e)))
    }

    /// What `HttpIndexerBase.TestConnection` turns an exception into, in
    /// the personality's wording.
    pub fn into_test_failure(self, personality: Personality) -> ValidationFailure {
        if personality == Personality::Radarr {
            self.into_radarr_test_failure()
        } else {
            self.into_sonarr_test_failure()
        }
    }

    /// Radarr's `HttpIndexerBase.TestConnection`: the same cases, the
    /// exception's message appended as it is.
    fn into_radarr_test_failure(self) -> ValidationFailure {
        let m = self.exception_message();
        let message = match self {
            IndexerError::ApiKey(_) => return ValidationFailure::error("ApiKey", "Invalid API Key"),
            IndexerError::RequestLimit(_) => format!("Request limit reached: {m}"),
            IndexerError::UnsupportedFeed(_) => format!("Indexer feed is not supported: {m}"),
            IndexerError::Feed(_) | IndexerError::Xml(_) => format!("Unable to connect to indexer. {m}"),
            IndexerError::Http { status, .. } if status >= 500 => {
                format!("Unable to connect to indexer, indexer's server is unavailable. Try again later. {m}")
            }
            IndexerError::Http { status, .. } if status == 401 || status == 403 => {
                format!("Unable to connect to indexer, invalid credentials. {m}")
            }
            IndexerError::Http { .. } => format!(
                "Unable to connect to indexer, check the log above the ValidationFailure for more details. {m}"
            ),
            IndexerError::Connect(_) => format!(
                "Unable to connect to indexer, please check your DNS settings and ensure IPv6 is working or disabled. {m}"
            ),
            IndexerError::Timeout(_) => format!(
                "Unable to connect to indexer, possibly due to a timeout. Try again or check your network settings. {m}"
            ),
        };
        ValidationFailure::error("", &message)
    }

    /// Sonarr's.
    fn into_sonarr_test_failure(self) -> ValidationFailure {
        let m = self.exception_message();
        match self {
            IndexerError::ApiKey(_) => ValidationFailure::error("ApiKey", "Invalid API Key"),
            IndexerError::RequestLimit(_) => ValidationFailure::error("", &format!("Request limit reached: {m}")),
            IndexerError::UnsupportedFeed(_) => {
                ValidationFailure::error("", &format!("Indexer feed is not supported: {m}"))
            }
            IndexerError::Feed(_) | IndexerError::Xml(_) => ValidationFailure::error(
                "",
                &format!("Unable to connect to indexer: {m}. Check the log surrounding this error for details"),
            ),
            IndexerError::Http { status, .. } => {
                if status >= 500 {
                    ValidationFailure::error(
                        "",
                        &format!("Unable to connect to indexer, indexer's server is unavailable. Try again later. {m}."),
                    )
                } else if status == 401 || status == 403 {
                    ValidationFailure::error(
                        "",
                        &format!("Unable to connect to indexer, invalid credentials. {m}."),
                    )
                } else {
                    ValidationFailure::error(
                        "",
                        &format!("Unable to connect to indexer: {m}. Check the log surrounding this error for details"),
                    )
                }
            }
            IndexerError::Connect(_) => ValidationFailure::error(
                "",
                &format!("Unable to connect to indexer, please check your DNS settings and ensure that IPv6 is working or disabled. {m}."),
            ),
            IndexerError::Timeout(_) => ValidationFailure::error(
                "",
                &format!("Unable to connect to indexer, possibly due to a timeout. Try again or check your network settings. {m}."),
            ),
        }
    }
}

fn root_cause(e: &reqwest::Error) -> String {
    let mut source: &dyn std::error::Error = e;
    while let Some(next) = source.source() {
        source = next;
    }
    source.to_string()
}

/// .NET's `HttpStatusCode` names, for the messages that quote them.
pub fn reason_phrase(status: u16) -> &'static str {
    match status {
        400 => "BadRequest",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "NotFound",
        429 => "TooManyRequests",
        500 => "InternalServerError",
        502 => "BadGateway",
        503 => "ServiceUnavailable",
        504 => "GatewayTimeout",
        _ => "Unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn definitions_come_from_providers() {
        let p = Provider {
            kind: crate::model::ProviderKind::Indexer,
            id: 7,
            name: "Torznab 7".into(),
            implementation: "Torznab".into(),
            config_contract: "TorznabSettings".into(),
            settings: json!({"baseUrl": "http://torznab.invalid/", "apiPath": "/downloads/api", "apiKey": "redacted", "categories": [5000, 5050], "minimumSeeders": 1}),
            tags: vec![],
            attributes: json!({"enableRss": true, "enableAutomaticSearch": false, "enableInteractiveSearch": true, "priority": 25}),
        };
        let d = IndexerDefinition::from_provider(&p).unwrap();
        assert_eq!(d.kind, IndexerKind::Torznab);
        assert!(d.enabled());
        assert_eq!(d.settings.categories, vec![5000, 5050]);
        assert!(IndexerDefinition::from_provider(&Provider {
            implementation: "Fanzub".into(),
            ..p
        })
        .is_none());
        let f = IndexerError::Connect("Connection refused (127.0.0.1:18999)".into())
            .into_test_failure(Personality::Sonarr);
        assert_eq!(
            f.error_message,
            "Unable to connect to indexer, please check your DNS settings and ensure that IPv6 is working or disabled. Connection refused (127.0.0.1:18999)."
        );
        let f = IndexerError::Connect("Connection refused (127.0.0.1:18999)".into())
            .into_test_failure(Personality::Radarr);
        assert_eq!(
            f.error_message,
            "Unable to connect to indexer, please check your DNS settings and ensure IPv6 is working or disabled. Connection refused (127.0.0.1:18999)"
        );
    }

    #[tokio::test]
    async fn tests_and_fetches_against_the_mock() {
        let fixtures = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
        // A minimal in-process stand-in: the unarr-mock crate cannot be a
        // dependency of unarr-core (it depends on it), so bind a copy here.
        let app = axum_mock(fixtures);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = IndexerClient::new(crate::http::build(std::time::Duration::from_secs(5)));
        let good = NewznabSettings::from_value(&json!({
            "baseUrl": format!("http://{addr}/newznab"), "apiKey": "0123456789abcdef0123456789abcdef", "categories": [5040]
        }));
        assert!(client.test(IndexerKind::Newznab, &good).await.is_empty());
        let wrong = NewznabSettings {
            api_key: "wrong".into(),
            ..good.clone()
        };
        let failures = client.test(IndexerKind::Newznab, &wrong).await;
        assert_eq!(failures[0].property_name, "ApiKey");
        let refused = NewznabSettings {
            base_url: "http://127.0.0.1:1/newznab".into(),
            ..good.clone()
        };
        let failures = client.test(IndexerKind::Newznab, &refused).await;
        assert!(
            failures[0]
                .error_message
                .contains("Connection refused (127.0.0.1:1)"),
            "{failures:?}"
        );

        let definition = IndexerDefinition {
            id: 20,
            name: "Mock".into(),
            kind: IndexerKind::Newznab,
            settings: good.clone(),
            enable_rss: true,
            enable_automatic_search: true,
            enable_interactive_search: true,
            priority: 25,
            download_client_id: 0,
            season_search_maximum_single_episode_age: 0,
            tags: vec![],
        };
        let recent = client.fetch_recent(&definition, None).await.unwrap();
        assert_eq!(recent.releases.len(), 9);
        assert!(recent.fully_updated);
        assert_eq!(recent.releases[0].indexer, "Mock");
        assert_eq!(recent.releases[0].indexer_id, 20);
    }

    /// Serves the fixture files with the routing of `scripts/mock-servarr.py`.
    fn axum_mock(fixtures: std::path::PathBuf) -> axum::Router {
        use axum::extract::{Query, State};
        use axum::http::Uri;
        use std::collections::BTreeMap;
        async fn handle(
            State(fixtures): State<std::path::PathBuf>,
            uri: Uri,
            Query(q): Query<BTreeMap<String, String>>,
        ) -> axum::response::Response {
            use axum::response::IntoResponse;
            let path = uri.path();
            let kind = if path.starts_with("/torznab") {
                "torznab"
            } else {
                "newznab"
            };
            if q.get("apikey").map(String::as_str) != Some("0123456789abcdef0123456789abcdef") {
                return "<?xml version=\"1.0\"?><error code=\"100\" description=\"Incorrect user credentials\"/>".into_response();
            }
            let name = if q.get("t").map(String::as_str) == Some("caps") {
                "caps.xml"
            } else {
                "rss.xml"
            };
            let body = std::fs::read_to_string(fixtures.join("indexers").join(kind).join(name)).unwrap();
            ([(axum::http::header::CONTENT_TYPE, "application/rss+xml")], body).into_response()
        }
        axum::Router::new().fallback(handle).with_state(fixtures)
    }
}
