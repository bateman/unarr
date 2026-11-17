//! The indexers as a set: which ones take part in an RSS sync or a search,
//! fetching from all of them at once, and the failure bookkeeping around
//! each fetch (`IndexerFactory`, `FetchAndParseRssService`,
//! `ReleaseSearchService.Dispatch`, `HttpIndexerBase.FetchReleases`).

use chrono::{DateTime, Duration, Utc};
use serde_json::json;

use crate::db::Db;
use crate::indexers::client::{IndexerClient, LastRssRelease};
use crate::indexers::request::SearchCriteria;
use crate::indexers::rss::ReleaseInfo;
use crate::indexers::{status, IndexerDefinition, IndexerError};
use crate::model::ProviderKind;
use crate::store;
use crate::store::provider_status::ProviderStatus;

const KIND: ProviderKind = ProviderKind::Indexer;

/// Why indexers are being asked.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Purpose {
    Rss,
    AutomaticSearch,
    InteractiveSearch,
}

/// What one indexer is asked.
enum Query<'a> {
    Rss,
    Series(&'a SearchCriteria),
    Movie(&'a crate::movies::request::MovieSearchCriteria),
}

/// What a search dispatch found, and from how many indexers.
#[derive(Debug, Clone, Default)]
pub struct SearchResult {
    pub releases: Vec<ReleaseInfo>,
    pub indexers_searched: usize,
}

#[derive(Clone)]
pub struct IndexerService {
    db: Db,
    client: &'static IndexerClient,
}

/// The indexers' answers, fetched at once, put back in the indexers' order.
/// The originals concatenate them as they arrive, so two releases alike in
/// every sort key come out in the order the servers answered; here that
/// order is the configured one, and the same on every run.
async fn in_indexer_order(
    tasks: &mut tokio::task::JoinSet<(usize, Vec<ReleaseInfo>)>,
) -> anyhow::Result<Vec<ReleaseInfo>> {
    let mut answers = Vec::new();
    while let Some(result) = tasks.join_next().await {
        match result {
            Ok(answer) => answers.push(answer),
            // Reading one indexer's answer panicked: that indexer brings
            // nothing this time, as when it cannot be reached, and the
            // others' releases are still decided on. Failing the whole
            // sync would stop every grab for as long as the release that
            // cannot be read stays in that one feed.
            Err(e) if e.is_panic() => {
                tracing::error!(error = %e, "An indexer's answer could not be read; its releases are left out");
            }
            Err(e) => return Err(e.into()),
        }
    }
    answers.sort_by_key(|(position, _)| *position);
    Ok(answers.into_iter().flat_map(|(_, releases)| releases).collect())
}

impl IndexerService {
    pub fn new(db: Db) -> IndexerService {
        IndexerService {
            db,
            client: crate::indexers::client::shared(),
        }
    }

    pub fn with_client(db: Db, client: &'static IndexerClient) -> IndexerService {
        IndexerService { db, client }
    }

    /// Every stored indexer this phase can drive.
    pub async fn definitions(&self) -> anyhow::Result<Vec<IndexerDefinition>> {
        Ok(self
            .db
            .call(|conn| store::providers::list_providers(conn, KIND))
            .await?
            .iter()
            .filter_map(IndexerDefinition::from_provider)
            .collect())
    }

    /// `IndexerFactory.RssEnabled` / `AutomaticSearchEnabled` /
    /// `InteractiveSearchEnabled` with blocked indexers filtered out: the
    /// ones to ask for `purpose` now.
    pub async fn enabled_for(
        &self,
        purpose: Purpose,
        now: DateTime<Utc>,
    ) -> anyhow::Result<Vec<IndexerDefinition>> {
        let definitions = self.definitions().await?;
        let statuses = self
            .db
            .call(|conn| store::provider_status::provider_statuses(conn, KIND))
            .await?;
        Ok(definitions
            .into_iter()
            .filter(|d| match purpose {
                Purpose::Rss => d.enable_rss,
                Purpose::AutomaticSearch => d.enable_automatic_search,
                Purpose::InteractiveSearch => d.enable_interactive_search,
            })
            .filter(|d| {
                let blocked = statuses
                    .iter()
                    .find(|(id, _)| *id == d.id)
                    .filter(|(_, s)| s.is_disabled(now));
                if let Some((_, s)) = blocked {
                    tracing::debug!(
                        indexer = d.name,
                        till = %s.disabled_till.map(crate::time::format_utc).unwrap_or_default(),
                        "temporarily ignoring indexer due to recent failures"
                    );
                }
                blocked.is_none()
            })
            .collect())
    }

    /// `FetchAndParseRssService.Fetch`: the RSS feeds of every RSS-enabled
    /// indexer, fetched at once, newest first.
    pub async fn fetch_recent(&self, now: DateTime<Utc>) -> anyhow::Result<Vec<ReleaseInfo>> {
        let definitions = self.enabled_for(Purpose::Rss, now).await?;
        if definitions.is_empty() {
            tracing::warn!("no available indexers. check your configuration");
            return Ok(Vec::new());
        }
        tracing::debug!(count = definitions.len(), "available indexers for the RSS sync");
        let mut tasks = tokio::task::JoinSet::new();
        for (position, definition) in definitions.into_iter().enumerate() {
            let service = self.clone();
            tasks.spawn(async move { (position, service.fetch_one(&definition, None, now).await) });
        }
        let mut releases = in_indexer_order(&mut tasks).await?;
        // Sonarr lists the newest first; Radarr keeps the feeds one after
        // the other, in the indexers' order.
        if self.client.personality() != crate::Personality::Radarr {
            releases.sort_by_key(|r| std::cmp::Reverse(r.publish_date));
        }
        tracing::debug!(count = releases.len(), "reports from the RSS sync");
        Ok(releases)
    }

    /// `ReleaseSearchService.Dispatch`: `criteria` sent to every indexer
    /// enabled for the search whose tags fit the series (an untagged indexer
    /// fits every series), the answers merged.
    pub async fn search(
        &self,
        criteria: &SearchCriteria,
        interactive: bool,
        series_tags: &[i64],
        now: DateTime<Utc>,
    ) -> anyhow::Result<SearchResult> {
        let purpose = if interactive {
            Purpose::InteractiveSearch
        } else {
            Purpose::AutomaticSearch
        };
        let definitions: Vec<IndexerDefinition> = self
            .enabled_for(purpose, now)
            .await?
            .into_iter()
            .filter(|d| d.tags.is_empty() || d.tags.iter().any(|t| series_tags.contains(t)))
            .collect();
        tracing::info!(count = definitions.len(), interactive, "searching indexers");
        let indexers_searched = definitions.len();
        let mut tasks = tokio::task::JoinSet::new();
        for (position, definition) in definitions.into_iter().enumerate() {
            let service = self.clone();
            let criteria = criteria.clone();
            tasks.spawn(async move {
                (
                    position,
                    service.fetch_one(&definition, Some(&criteria), now).await,
                )
            });
        }
        let releases = in_indexer_order(&mut tasks).await?;
        Ok(SearchResult {
            releases,
            indexers_searched,
        })
    }

    /// Radarr's `ReleaseSearchService.Dispatch`: a movie search sent to every
    /// indexer enabled for it whose tags fit the movie, the answers merged.
    pub async fn search_movie(
        &self,
        criteria: &crate::movies::request::MovieSearchCriteria,
        interactive: bool,
        movie_tags: &[i64],
        now: DateTime<Utc>,
    ) -> anyhow::Result<SearchResult> {
        let purpose = if interactive {
            Purpose::InteractiveSearch
        } else {
            Purpose::AutomaticSearch
        };
        let definitions: Vec<IndexerDefinition> = self
            .enabled_for(purpose, now)
            .await?
            .into_iter()
            .filter(|d| d.tags.is_empty() || d.tags.iter().any(|t| movie_tags.contains(t)))
            .collect();
        tracing::info!(count = definitions.len(), interactive, "searching indexers");
        let indexers_searched = definitions.len();
        let mut tasks = tokio::task::JoinSet::new();
        for (position, definition) in definitions.into_iter().enumerate() {
            let service = self.clone();
            let criteria = criteria.clone();
            tasks.spawn(async move {
                (
                    position,
                    service
                        .fetch_with(&definition, Query::Movie(&criteria), now)
                        .await,
                )
            });
        }
        let releases = in_indexer_order(&mut tasks).await?;
        Ok(SearchResult {
            releases,
            indexers_searched,
        })
    }

    /// One indexer's RSS feed (`criteria` `None`) or search, with the status
    /// bookkeeping of `HttpIndexerBase.FetchReleases`: success steps the
    /// back-off down, a failure steps it up (a request limit by an hour at
    /// least), and the RSS sync remembers the newest release it saw.
    pub async fn fetch_one(
        &self,
        definition: &IndexerDefinition,
        criteria: Option<&SearchCriteria>,
        now: DateTime<Utc>,
    ) -> Vec<ReleaseInfo> {
        let query = match criteria {
            Some(criteria) => Query::Series(criteria),
            None => Query::Rss,
        };
        self.fetch_with(definition, query, now).await
    }

    async fn fetch_with(
        &self,
        definition: &IndexerDefinition,
        query: Query<'_>,
        now: DateTime<Utc>,
    ) -> Vec<ReleaseInfo> {
        let id = definition.id;
        let result = match query {
            Query::Series(criteria) => self.client.search(definition, criteria).await.map(|r| (r, None)),
            Query::Movie(criteria) => self
                .client
                .search_movie(definition, criteria)
                .await
                .map(|r| (r, None)),
            Query::Rss => {
                let last = self.last_rss_release(id).await;
                self.client
                    .fetch_recent(definition, last.as_ref())
                    .await
                    .map(|fetch| {
                        if !fetch.fully_updated && last.is_some() && !fetch.releases.is_empty() {
                            let oldest = fetch.releases.iter().map(|r| r.publish_date).min();
                            tracing::warn!(
                                indexer = definition.name,
                                from = %last.as_ref().map(|l| crate::time::format_utc(l.publish_date)).unwrap_or_default(),
                                to = %oldest.map(crate::time::format_utc).unwrap_or_default(),
                                "RSS sync didn't cover the period; a search may be required"
                            );
                        }
                        let newest = fetch.releases.iter().max_by_key(|r| r.publish_date).cloned();
                        (fetch.releases, newest)
                    })
            }
        };
        match result {
            Ok((releases, newest)) => {
                tracing::debug!(
                    indexer = definition.name,
                    count = releases.len(),
                    "releases fetched"
                );
                self.record_success(id, newest.as_ref()).await;
                releases
            }
            Err(e) => {
                let minimum = match &e {
                    IndexerError::RequestLimit(_) => Duration::hours(1),
                    IndexerError::Http { status: 429, .. } => Duration::hours(1),
                    _ => Duration::zero(),
                };
                match &e {
                    IndexerError::ApiKey(_) => {
                        tracing::warn!(indexer = definition.name, "invalid API key for the indexer")
                    }
                    IndexerError::RequestLimit(_) | IndexerError::Http { status: 429, .. } => {
                        tracing::warn!(indexer = definition.name, "API request limit reached")
                    }
                    IndexerError::Http { status: 503, .. } => {
                        tracing::warn!(indexer = definition.name, "indexer unavailable")
                    }
                    _ => {
                        tracing::warn!(indexer = definition.name, error = %e.exception_message(), "indexer request failed")
                    }
                }
                self.record_failure(id, minimum, now).await;
                Vec::new()
            }
        }
    }

    /// `GetLastRssSyncReleaseInfo`.
    pub async fn last_rss_release(&self, id: i64) -> Option<LastRssRelease> {
        let status = self.status(id).await?;
        let json: serde_json::Value =
            serde_json::from_str(status.last_rss_sync_release_info.as_deref()?).ok()?;
        let publish_date = crate::time::parse_utc(json["publishDate"].as_str()?)?;
        Some(LastRssRelease {
            publish_date,
            download_url: json["downloadUrl"].as_str().map(str::to_string),
        })
    }

    async fn status(&self, id: i64) -> Option<ProviderStatus> {
        self.db
            .call(move |conn| store::provider_status::provider_status(conn, KIND, id))
            .await
            .ok()
    }

    async fn store_status(&self, id: i64, status: ProviderStatus) {
        if let Err(e) = self
            .db
            .call(move |conn| store::provider_status::upsert_provider_status(conn, KIND, id, &status))
            .await
        {
            tracing::error!(indexer = id, error = %e, "failed to store the indexer status");
        }
    }

    /// `RecordSuccess`, with `UpdateRssSyncStatus` when the RSS sync saw
    /// something.
    pub async fn record_success(&self, id: i64, newest: Option<&ReleaseInfo>) {
        let Some(mut status) = self.status(id).await else {
            return;
        };
        let mut changed = status::record_success(&mut status);
        if let Some(r) = newest {
            // The originals store the whole release; the fields the next sync
            // reads come first, under their JSON names.
            status.last_rss_sync_release_info = Some(
                json!({
                    "guid": r.guid,
                    "title": r.title,
                    "publishDate": crate::time::format_utc(r.publish_date),
                    "downloadUrl": r.download_url,
                    "indexerId": r.indexer_id,
                    "indexer": r.indexer,
                    "size": r.size,
                })
                .to_string(),
            );
            changed = true;
        }
        if changed {
            self.store_status(id, status).await;
        }
    }

    /// `RecordFailure`, escalating; `minimum` is the back-off a request
    /// limit demands.
    pub async fn record_failure(&self, id: i64, minimum: Duration, now: DateTime<Utc>) {
        let Some(mut status) = self.status(id).await else {
            return;
        };
        status::record_failure_with(
            &mut status,
            now,
            minimum,
            true,
            &status::Policy::indexer(Some(crate::time::process_start())),
        );
        self.store_status(id, status).await;
    }
}

#[cfg(test)]
mod tests {
    /// One indexer whose answer panics while it is read brings nothing;
    /// the others' releases come out, in the indexers' order.
    #[tokio::test]
    async fn an_indexer_whose_answer_panics_is_left_out() {
        crate::panics::install_hook();
        let release = |title: &str| super::ReleaseInfo {
            title: title.to_owned(),
            ..Default::default()
        };
        let mut tasks = tokio::task::JoinSet::new();
        let (second, first) = (release("second"), release("first"));
        tasks.spawn(async move { (2, vec![second]) });
        tasks.spawn(async {
            let nothing: Vec<super::ReleaseInfo> = Vec::new();
            (1, vec![nothing[0].clone()])
        });
        tasks.spawn(async move { (0, vec![first]) });
        let releases = super::in_indexer_order(&mut tasks).await.unwrap();
        let titles: Vec<&str> = releases.iter().map(|r| r.title.as_str()).collect();
        assert_eq!(titles, ["first", "second"]);
    }

    use super::*;
    use crate::indexers::settings::NewznabSettings;
    use crate::model::Provider;
    use chrono::TimeZone;

    fn indexer(id: i64, name: &str, port: u16, enable_rss: bool) -> Provider {
        Provider {
            kind: KIND,
            id,
            name: name.into(),
            implementation: "Newznab".into(),
            config_contract: "NewznabSettings".into(),
            settings: NewznabSettings {
                base_url: format!("http://127.0.0.1:{port}/newznab"),
                api_key: unarr_mock_key(),
                ..Default::default()
            }
            .to_value(),
            tags: vec![],
            attributes: json!({"enableRss": enable_rss, "enableAutomaticSearch": true, "enableInteractiveSearch": false, "priority": 25}),
        }
    }

    fn unarr_mock_key() -> String {
        "0123456789abcdef0123456789abcdef".into()
    }

    #[test]
    fn blocked_indexers_sit_out() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let db = Db::open_in_memory().unwrap();
            db.call(|conn| {
                store::providers::insert_provider(conn, &indexer(1, "A", 18989, true))?;
                store::providers::insert_provider(conn, &indexer(2, "B", 18989, true))?;
                store::providers::insert_provider(conn, &indexer(3, "C", 18989, false))?;
                Ok(())
            })
            .await
            .unwrap();
            let service = IndexerService::new(db.clone());
            let now = Utc.with_ymd_and_hms(2026, 11, 7, 12, 0, 0).unwrap();
            let names = |defs: Vec<IndexerDefinition>| defs.into_iter().map(|d| d.name).collect::<Vec<_>>();
            assert_eq!(
                names(service.enabled_for(Purpose::Rss, now).await.unwrap()),
                ["A", "B"]
            );
            assert_eq!(
                names(service.enabled_for(Purpose::AutomaticSearch, now).await.unwrap()),
                ["A", "B", "C"]
            );
            assert!(service
                .enabled_for(Purpose::InteractiveSearch, now)
                .await
                .unwrap()
                .is_empty());

            service.record_failure(2, Duration::zero(), Utc::now()).await;
            assert_eq!(
                names(service.enabled_for(Purpose::Rss, now).await.unwrap()),
                ["A"]
            );
            // The block lifts with time, and a success clears it.
            let later = Utc::now() + Duration::minutes(2);
            assert_eq!(
                names(service.enabled_for(Purpose::Rss, later).await.unwrap()),
                ["A", "B"]
            );
            service.record_success(2, None).await;
            assert_eq!(
                names(service.enabled_for(Purpose::Rss, now).await.unwrap()),
                ["A", "B"]
            );
            assert!(service.last_rss_release(2).await.is_none());

            // Deleting the provider drops its status.
            service.record_failure(2, Duration::hours(1), Utc::now()).await;
            let status = service.status(2).await.unwrap();
            assert_eq!(status.escalation_level, 5);
            db.call(|conn| store::providers::delete_provider(conn, KIND, 2))
                .await
                .unwrap();
            assert_eq!(service.status(2).await.unwrap(), ProviderStatus::default());
        });
    }
}
