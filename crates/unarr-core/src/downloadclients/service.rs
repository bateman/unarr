//! The stored download clients as a service, after the originals'
//! `DownloadClientFactory`, `DownloadClientProvider` and
//! `DownloadClientStatusService`: which client takes a release, which are
//! healthy, and the back-off a failing one earns.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::Mutex;

use chrono::{DateTime, Duration, Utc};

use super::{Client, ClientContext, DownloadClientDefinition};
use crate::db::Db;
use crate::indexers::{status, IndexerDefinition};
use crate::model::{Protocol, ProviderKind};
use crate::store;
use crate::store::provider_status::ProviderStatus;
use crate::validation::ValidationFailure;

const KIND: ProviderKind = ProviderKind::DownloadClient;

/// `DownloadClientStatusService.MaximumEscalationLevel`: an hour at most.
pub const MAXIMUM_ESCALATION_LEVEL: i64 = 5;
/// `MinimumTimeSinceInitialFailure` before the health check reports it.
pub const MINIMUM_TIME_SINCE_INITIAL_FAILURE: Duration = Duration::minutes(5);

/// `DownloadClientUnavailableException`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadClientUnavailable(pub String);

impl fmt::Display for DownloadClientUnavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DownloadClientUnavailable {}

pub struct DownloadClientService {
    db: Db,
    http: reqwest::Client,
    /// `_lastUsedDownloadClient`: round-robin position per protocol.
    last_used: Mutex<HashMap<Protocol, i64>>,
}

impl DownloadClientService {
    pub fn new(db: Db) -> DownloadClientService {
        Self::with_client(db, crate::http::shared().clone())
    }

    pub fn with_client(db: Db, http: reqwest::Client) -> DownloadClientService {
        DownloadClientService {
            db,
            http,
            last_used: Mutex::new(HashMap::new()),
        }
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    /// Every stored client this phase can drive, enabled or not.
    pub async fn definitions(&self) -> anyhow::Result<Vec<DownloadClientDefinition>> {
        Ok(self
            .db
            .call(|conn| store::providers::list_providers(conn, KIND))
            .await?
            .iter()
            .filter_map(DownloadClientDefinition::from_provider)
            .collect())
    }

    /// `GetAvailableProviders`: the enabled ones.
    pub async fn enabled(&self) -> anyhow::Result<Vec<DownloadClientDefinition>> {
        Ok(self
            .definitions()
            .await?
            .into_iter()
            .filter(|d| d.enable)
            .collect())
    }

    pub async fn find(&self, id: i64) -> anyhow::Result<Option<DownloadClientDefinition>> {
        Ok(self
            .db
            .call(move |conn| store::providers::get_provider(conn, KIND, id))
            .await?
            .as_ref()
            .and_then(DownloadClientDefinition::from_provider))
    }

    /// The context clients run with: the HTTP client, the remote path
    /// mappings and the history limit as configured.
    pub async fn context(&self, now: DateTime<Utc>) -> anyhow::Result<ClientContext> {
        let (mappings, limit, root_folders) = self
            .db
            .call(|conn| {
                let mappings = store::providers::list_remote_path_mappings(conn)?;
                let limit = store::config::config(conn, "DownloadClientHistoryLimit")?
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(60);
                let roots = store::catalog::list_root_folders(conn)?
                    .into_iter()
                    .map(|r| r.path)
                    .collect();
                Ok((mappings, limit, roots))
            })
            .await?;
        Ok(ClientContext {
            http: self.http.clone(),
            mappings,
            history_limit: limit,
            root_folders,
            now,
        })
    }

    /// `GetBlockedProviders`: ids disabled by their back-off right now.
    pub async fn blocked(&self, now: DateTime<Utc>) -> HashSet<i64> {
        self.db
            .call(move |conn| store::provider_status::provider_statuses(conn, KIND))
            .await
            .map(|all| {
                all.into_iter()
                    .filter(|(_, s)| s.is_disabled(now))
                    .map(|(id, _)| id)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// `DownloadHandlingEnabled`: the enabled clients, minus the blocked
    /// ones when `filter_blocked`.
    pub async fn download_handling_enabled(
        &self,
        filter_blocked: bool,
        now: DateTime<Utc>,
    ) -> anyhow::Result<Vec<DownloadClientDefinition>> {
        let enabled = self.enabled().await?;
        if !filter_blocked {
            return Ok(enabled);
        }
        let blocked = self.blocked(now).await;
        Ok(enabled.into_iter().filter(|d| !blocked.contains(&d.id)).collect())
    }

    /// `DownloadClientProvider.GetDownloadClient`: the client for a release
    /// of `protocol` from `indexer_id` (0 for none), honouring the indexer's
    /// pinned client, the series tags, the clients' priorities and a
    /// round-robin among equals. `None` when no client serves the protocol.
    pub async fn get_download_client(
        &self,
        protocol: Protocol,
        indexer_id: i64,
        filter_blocked: bool,
        tags: &[i64],
        now: DateTime<Utc>,
    ) -> anyhow::Result<Result<Option<DownloadClientDefinition>, DownloadClientUnavailable>> {
        let blocked = self.blocked(now).await;
        let mut available: Vec<DownloadClientDefinition> = self
            .enabled()
            .await?
            .into_iter()
            .filter(|d| d.protocol() == protocol)
            .collect();
        if available.is_empty() {
            return Ok(Ok(None));
        }
        // Clients with tags are picked only for series sharing one; untagged
        // clients take everything else.
        let matching: Vec<DownloadClientDefinition> = available
            .iter()
            .filter(|d| d.tags.iter().any(|t| tags.contains(t)))
            .cloned()
            .collect();
        available = if matching.is_empty() {
            available.into_iter().filter(|d| d.tags.is_empty()).collect()
        } else {
            matching
        };
        if available.is_empty() {
            return Ok(Err(DownloadClientUnavailable(
                "No download client was found without tags or a matching series tag. Please check your settings.".into(),
            )));
        }
        if indexer_id > 0 {
            let indexer = self
                .db
                .call(move |conn| store::providers::get_provider(conn, ProviderKind::Indexer, indexer_id))
                .await?
                .as_ref()
                .and_then(IndexerDefinition::from_provider);
            if let Some(indexer) = indexer.filter(|i| i.download_client_id > 0) {
                let Some(client) = available
                    .iter()
                    .find(|d| d.id == indexer.download_client_id)
                    .cloned()
                else {
                    return Ok(Err(DownloadClientUnavailable(format!(
                        "Indexer specified download client does not exist for {}",
                        indexer.name
                    ))));
                };
                if filter_blocked && blocked.contains(&client.id) {
                    return Ok(Err(DownloadClientUnavailable(format!(
                        "Indexer specified download client is not available due to recent failures for {}",
                        indexer.name
                    ))));
                }
                return Ok(Ok(Some(client)));
            }
        }
        if !blocked.is_empty() {
            let healthy: Vec<DownloadClientDefinition> = available
                .iter()
                .filter(|d| !blocked.contains(&d.id))
                .cloned()
                .collect();
            if !healthy.is_empty() {
                available = healthy;
            } else if filter_blocked {
                return Ok(Err(DownloadClientUnavailable(format!(
                    "All download clients for {} are not available",
                    protocol_name(protocol)
                ))));
            } else {
                tracing::trace!("all download clients are blocked, using any");
            }
        }
        // The lowest priority group first, by id within it.
        let best = available.iter().map(|d| d.priority).min().unwrap_or(1);
        let mut group: Vec<DownloadClientDefinition> =
            available.into_iter().filter(|d| d.priority == best).collect();
        group.sort_by_key(|d| d.id);
        let last = self
            .last_used
            .lock()
            .unwrap()
            .get(&protocol)
            .copied()
            .unwrap_or(0);
        let chosen = group
            .iter()
            .find(|d| d.id > last)
            .or_else(|| group.first())
            .cloned()
            .expect("a non-empty group");
        self.last_used.lock().unwrap().insert(protocol, chosen.id);
        Ok(Ok(Some(chosen)))
    }

    // --- status

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
            tracing::error!(client = id, error = %e, "failed to store the download client status");
        }
    }

    pub async fn record_success(&self, id: i64) {
        let Some(mut s) = self.status(id).await else {
            return;
        };
        if status::record_success(&mut s) {
            self.store_status(id, s).await;
        }
    }

    pub async fn record_failure(&self, id: i64, now: DateTime<Utc>) {
        let Some(mut s) = self.status(id).await else {
            return;
        };
        status::record_failure_with(
            &mut s,
            now,
            Duration::zero(),
            true,
            &status::Policy::download_client(Some(crate::time::process_start())),
        );
        self.store_status(id, s).await;
    }

    /// `DownloadClientFactory.Test`: the connection test, recorded as a
    /// success or failure on a stored client.
    pub async fn test(
        &self,
        definition: &DownloadClientDefinition,
        now: DateTime<Utc>,
    ) -> anyhow::Result<Vec<ValidationFailure>> {
        let context = self.context(now).await?;
        let client = Client::new(definition, &context);
        let failures = client.test().await;
        if definition.id > 0 {
            if crate::validation::has_errors(&failures) {
                self.record_failure(definition.id, now).await;
            } else {
                self.record_success(definition.id).await;
            }
        }
        Ok(failures)
    }
}

/// `DownloadProtocol.ToString()`.
fn protocol_name(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::Usenet => "Usenet",
        Protocol::Torrent => "Torrent",
        Protocol::Unknown => "Unknown",
    }
}
