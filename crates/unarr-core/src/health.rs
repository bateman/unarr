//! Health checks, after the originals' `HealthCheckService` and the checks
//! in `HealthCheck/Checks` that concern acquisition: indexers and download
//! clients configured, reachable and not in back-off, sorting and remote
//! paths on the clients, the root folders of the library and the import
//! lists. Each check answers with the originals' source name, severity,
//! message and wiki link, so the UI shows what it would show them.

use chrono::{DateTime, Duration, Utc};

use crate::downloadclients::remote_path;
use crate::downloadclients::{
    Client, ClientError, ClientKind, DownloadClientDefinition, DownloadClientService, APP_NAME,
};
use crate::indexers::{IndexerDefinition, IndexerService};
use crate::model::ProviderKind;
use crate::store;
use crate::store::provider_status::ProviderStatus;

/// `HealthCheckResult`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthType {
    Ok,
    Notice,
    Warning,
    Error,
}

impl HealthType {
    pub fn as_str(self) -> &'static str {
        match self {
            HealthType::Ok => "ok",
            HealthType::Notice => "notice",
            HealthType::Warning => "warning",
            HealthType::Error => "error",
        }
    }
}

/// `HealthCheck`: one check's verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthCheck {
    /// The originals' class name (`IndexerRssCheck`), which the UI keys on.
    pub source: &'static str,
    pub kind: HealthType,
    pub message: String,
    pub wiki_url: String,
}

impl HealthCheck {
    fn ok(source: &'static str) -> HealthCheck {
        HealthCheck {
            source,
            kind: HealthType::Ok,
            message: String::new(),
            wiki_url: String::new(),
        }
    }

    fn new(source: &'static str, kind: HealthType, message: String, fragment: &str) -> HealthCheck {
        HealthCheck {
            source,
            kind,
            message,
            wiki_url: format!("https://wiki.servarr.com/sonarr/system{fragment}"),
        }
    }

    pub fn is_ok(&self) -> bool {
        self.kind == HealthType::Ok
    }
}

/// Which checks to run: all of them (startup, schedule, a manual
/// `CheckHealth`), those that follow a provider change, those that
/// follow a provider's status change (cheap, plus the clients' reach), or
/// those the originals' `CheckOn` attributes tie to an import that failed
/// or succeeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckSet {
    All,
    Providers,
    Statuses,
    /// `EpisodeImportFailedEvent`: the checks that look for the cause.
    ImportFailed,
    /// `EpisodeImportedEvent`: the checks whose failure an import clears.
    ImportSucceeded,
}

/// `CheckOnCondition`: whether a check tied to an event runs only while
/// it is passing (to catch a new failure) or only while failing (to see
/// it clear).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Condition {
    Always,
    SuccessfulOnly,
    FailedOnly,
}

/// What `EpisodeImportFailedEvent` carries for `RemotePathMappingCheck`.
#[derive(Debug, Clone, Default)]
pub struct ImportFailure {
    /// The file the import worked on, when it got that far.
    pub path: Option<String>,
    pub download_client: Option<String>,
    pub download_id: Option<String>,
    /// Where the client says the download is, as this process sees it.
    pub output_path: Option<String>,
}

/// What the checks read and ask.
pub struct Context<'a> {
    pub db: &'a crate::db::Db,
    pub indexers: &'a IndexerService,
    pub clients: &'a DownloadClientService,
    pub now: DateTime<Utc>,
    pub is_docker: bool,
    pub import_failure: Option<&'a ImportFailure>,
}

type Check = for<'a> fn(
    &'a Context<'a>,
) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = anyhow::Result<HealthCheck>> + Send + 'a>,
>;

macro_rules! check {
    ($f:ident) => {
        (|ctx| Box::pin($f(ctx))) as Check
    };
}

/// The checks of a set with their source names and conditions, in the
/// originals' registration order.
fn checks(set: CheckSet) -> Vec<(&'static str, Condition, Check)> {
    use Condition::*;
    match set {
        CheckSet::All | CheckSet::Providers | CheckSet::Statuses => {
            let mut out = vec![
                ("IndexerRssCheck", Always, check!(indexer_rss)),
                ("IndexerSearchCheck", Always, check!(indexer_search)),
                ("IndexerStatusCheck", Always, check!(indexer_status)),
                (
                    "IndexerLongTermStatusCheck",
                    Always,
                    check!(indexer_long_term_status),
                ),
                ("DownloadClientCheck", Always, check!(download_client_reach)),
                (
                    "DownloadClientStatusCheck",
                    Always,
                    check!(download_client_status),
                ),
            ];
            if set != CheckSet::Statuses {
                out.extend([
                    (
                        "IndexerDownloadClientCheck",
                        Always,
                        check!(indexer_download_client),
                    ),
                    (
                        "DownloadClientSortingCheck",
                        Always,
                        check!(download_client_sorting),
                    ),
                    ("RemotePathMappingCheck", Always, check!(remote_path_mapping)),
                    (
                        "DownloadClientRootFolderCheck",
                        Always,
                        check!(download_client_root_folder),
                    ),
                    (
                        "ImportListRootFolderCheck",
                        Always,
                        check!(import_list_root_folder),
                    ),
                    ("ImportMechanismCheck", Always, check!(import_mechanism)),
                    (
                        "DownloadClientRemovesCompletedDownloadsCheck",
                        Always,
                        check!(download_client_removes_completed_downloads),
                    ),
                ]);
            }
            if set == CheckSet::All {
                out.extend([
                    ("RootFolderCheck", Always, check!(root_folder)),
                    ("RecyclingBinCheck", Always, check!(recycling_bin)),
                    ("MountCheck", Always, check!(mount)),
                ]);
            }
            out
        }
        CheckSet::ImportFailed => vec![
            ("RootFolderCheck", SuccessfulOnly, check!(root_folder)),
            (
                "ImportListRootFolderCheck",
                SuccessfulOnly,
                check!(import_list_root_folder),
            ),
            ("RecyclingBinCheck", SuccessfulOnly, check!(recycling_bin)),
            (
                "RemotePathMappingCheck",
                SuccessfulOnly,
                check!(remote_path_mapping_after_failure),
            ),
        ],
        CheckSet::ImportSucceeded => vec![
            ("RootFolderCheck", FailedOnly, check!(root_folder)),
            (
                "ImportListRootFolderCheck",
                FailedOnly,
                check!(import_list_root_folder),
            ),
            ("RecyclingBinCheck", FailedOnly, check!(recycling_bin)),
        ],
    }
}

/// Run the checks of some sets, each once, those with a condition only
/// when the standing verdicts allow; every verdict, ok or not, in order.
pub async fn run(ctx: &Context<'_>, sets: &[CheckSet], standing: &[HealthCheck]) -> Vec<HealthCheck> {
    let mut seen: Vec<&'static str> = Vec::new();
    let mut out = Vec::new();
    for set in sets {
        for (source, condition, check) in checks(*set) {
            if seen.contains(&source) {
                continue;
            }
            let failing = standing.iter().any(|s| s.source == source);
            let runs = match condition {
                Condition::Always => true,
                Condition::SuccessfulOnly => !failing,
                Condition::FailedOnly => failing,
            };
            if !runs {
                continue;
            }
            seen.push(source);
            match check(ctx).await {
                Ok(result) => out.push(result),
                Err(e) => tracing::error!(source, error = %e, "health check failed to run"),
            }
        }
    }
    out
}

/// The verdicts that stand after a run: failures replace or join the
/// earlier ones by source, in their first order; passes drop them.
pub fn merge(results: &mut Vec<HealthCheck>, run: Vec<HealthCheck>) -> bool {
    let mut changed = false;
    for r in run {
        let at = results.iter().position(|x| x.source == r.source);
        match (at, r.is_ok()) {
            (Some(i), true) => {
                results.remove(i);
                changed = true;
            }
            (Some(i), false) => {
                if results[i] != r {
                    results[i] = r;
                    changed = true;
                }
            }
            (None, true) => {}
            (None, false) => {
                results.push(r);
                changed = true;
            }
        }
    }
    changed
}

fn names<'a>(it: impl Iterator<Item = &'a str>) -> String {
    it.collect::<Vec<_>>().join(", ")
}

async fn indexer_definitions(ctx: &Context<'_>) -> anyhow::Result<Vec<IndexerDefinition>> {
    ctx.indexers.definitions().await
}

async fn indexer_statuses(ctx: &Context<'_>) -> anyhow::Result<Vec<(i64, ProviderStatus)>> {
    ctx.db
        .call(|conn| store::provider_status::provider_statuses(conn, ProviderKind::Indexer))
        .await
}

fn enabled(d: &IndexerDefinition) -> bool {
    d.enable_rss || d.enable_automatic_search || d.enable_interactive_search
}

/// `IndexerRssCheck`.
async fn indexer_rss(ctx: &Context<'_>) -> anyhow::Result<HealthCheck> {
    const SOURCE: &str = "IndexerRssCheck";
    let all = indexer_definitions(ctx).await?;
    let rss: Vec<&IndexerDefinition> = all.iter().filter(|d| d.enable_rss).collect();
    if rss.is_empty() {
        return Ok(HealthCheck::new(
            SOURCE,
            HealthType::Error,
            format!("No indexers available with RSS sync enabled, {APP_NAME} will not grab new releases automatically"),
            "#no-indexers-available-with-rss-sync-enabled-sonarr-will-not-grab-new-releases-automatically",
        ));
    }
    let statuses = indexer_statuses(ctx).await?;
    let blocked = |d: &IndexerDefinition| {
        statuses
            .iter()
            .any(|(id, s)| *id == d.id && s.is_disabled(ctx.now))
    };
    if rss.iter().all(|d| blocked(d)) {
        return Ok(HealthCheck::new(
            SOURCE,
            HealthType::Warning,
            "All rss-capable indexers are temporarily unavailable due to recent indexer errors".into(),
            "#indexers-are-unavailable-due-to-failures",
        ));
    }
    Ok(HealthCheck::ok(SOURCE))
}

/// `IndexerSearchCheck`.
async fn indexer_search(ctx: &Context<'_>) -> anyhow::Result<HealthCheck> {
    const SOURCE: &str = "IndexerSearchCheck";
    let all = indexer_definitions(ctx).await?;
    if !all.iter().any(|d| d.enable_automatic_search) {
        return Ok(HealthCheck::new(
            SOURCE,
            HealthType::Warning,
            format!("No indexers available with Automatic Search enabled, {APP_NAME} will not provide any automatic search results"),
            "#no-indexers-available-with-automatic-search-enabled-sonarr-will-not-provide-any-automatic-search-results",
        ));
    }
    if !all.iter().any(|d| d.enable_interactive_search) {
        return Ok(HealthCheck::new(
            SOURCE,
            HealthType::Warning,
            format!("No indexers available with Interactive Search enabled, {APP_NAME} will not provide any interactive search results"),
            "#no-indexers-available-with-interactive-search-enabled",
        ));
    }
    let statuses = indexer_statuses(ctx).await?;
    let active = all.iter().filter(|d| d.enable_automatic_search).any(|d| {
        !statuses
            .iter()
            .any(|(id, s)| *id == d.id && s.is_disabled(ctx.now))
    });
    if !active {
        return Ok(HealthCheck::new(
            SOURCE,
            HealthType::Warning,
            "All search-capable indexers are temporarily unavailable due to recent indexer errors".into(),
            "#indexers-are-unavailable-due-to-failures",
        ));
    }
    Ok(HealthCheck::ok(SOURCE))
}

/// `IndexerStatusCheck` (failures of the last six hours) and
/// `IndexerLongTermStatusCheck` (older ones).
async fn indexer_back_off(
    ctx: &Context<'_>,
    source: &'static str,
    recent: bool,
) -> anyhow::Result<HealthCheck> {
    let all = indexer_definitions(ctx).await?;
    let enabled: Vec<&IndexerDefinition> = all.iter().filter(|d| enabled(d)).collect();
    let statuses = indexer_statuses(ctx).await?;
    let six_hours_ago = ctx.now - Duration::hours(6);
    let backed_off: Vec<&IndexerDefinition> = enabled
        .iter()
        .copied()
        .filter(|d| {
            statuses.iter().any(|(id, s)| {
                *id == d.id
                    && s.is_disabled(ctx.now)
                    && s.initial_failure.is_some_and(|f| {
                        if recent {
                            f > six_hours_ago
                        } else {
                            f < six_hours_ago
                        }
                    })
            })
        })
        .collect();
    if backed_off.is_empty() {
        return Ok(HealthCheck::ok(source));
    }
    let (all_text, some_text) = if recent {
        (
            "All indexers are unavailable due to failures",
            "Indexers unavailable due to failures: ",
        )
    } else {
        (
            "All indexers are unavailable due to failures for more than 6 hours",
            "Indexers unavailable due to failures for more than 6 hours: ",
        )
    };
    if backed_off.len() == enabled.len() {
        return Ok(HealthCheck::new(
            source,
            HealthType::Error,
            all_text.into(),
            "#indexers-are-unavailable-due-to-failures",
        ));
    }
    Ok(HealthCheck::new(
        source,
        HealthType::Warning,
        format!("{some_text}{}", names(backed_off.iter().map(|d| d.name.as_str()))),
        "#indexers-are-unavailable-due-to-failures",
    ))
}

async fn indexer_status(ctx: &Context<'_>) -> anyhow::Result<HealthCheck> {
    indexer_back_off(ctx, "IndexerStatusCheck", true).await
}

async fn indexer_long_term_status(ctx: &Context<'_>) -> anyhow::Result<HealthCheck> {
    indexer_back_off(ctx, "IndexerLongTermStatusCheck", false).await
}

/// `IndexerDownloadClientCheck`: indexers naming a client that is gone or
/// disabled.
async fn indexer_download_client(ctx: &Context<'_>) -> anyhow::Result<HealthCheck> {
    const SOURCE: &str = "IndexerDownloadClientCheck";
    let clients: Vec<i64> = ctx.clients.enabled().await?.iter().map(|c| c.id).collect();
    let invalid: Vec<String> = indexer_definitions(ctx)
        .await?
        .into_iter()
        .filter(|d| enabled(d) && d.download_client_id > 0 && !clients.contains(&d.download_client_id))
        .map(|d| d.name)
        .collect();
    if invalid.is_empty() {
        return Ok(HealthCheck::ok(SOURCE));
    }
    Ok(HealthCheck::new(
        SOURCE,
        HealthType::Warning,
        format!("Indexers with invalid download clients: {}.", invalid.join(", ")),
        "#invalid-indexer-download-client-setting",
    ))
}

/// `DownloadClientCheck`: at least one client, and every enabled one
/// answers.
async fn download_client_reach(ctx: &Context<'_>) -> anyhow::Result<HealthCheck> {
    const SOURCE: &str = "DownloadClientCheck";
    let clients = ctx.clients.enabled().await?;
    if clients.is_empty() {
        return Ok(HealthCheck::new(
            SOURCE,
            HealthType::Warning,
            "No download client is available".into(),
            "#no-download-client-is-available",
        ));
    }
    let context = ctx.clients.context(ctx.now).await?;
    for definition in &clients {
        let client = Client::new(definition, &context);
        if let Err(e) = client.items().await {
            tracing::debug!(client = definition.name, error = %e, "unable to communicate with the download client");
            return Ok(HealthCheck::new(
                SOURCE,
                HealthType::Error,
                format!("Unable to communicate with {}. {}", definition.name, e.message()),
                "#unable-to-communicate-with-download-client",
            ));
        }
    }
    Ok(HealthCheck::ok(SOURCE))
}

/// `DownloadClientStatusCheck`: enabled clients in back-off.
async fn download_client_status(ctx: &Context<'_>) -> anyhow::Result<HealthCheck> {
    const SOURCE: &str = "DownloadClientStatusCheck";
    let clients = ctx.clients.enabled().await?;
    let blocked = ctx.clients.blocked(ctx.now).await;
    let backed_off: Vec<&DownloadClientDefinition> =
        clients.iter().filter(|c| blocked.contains(&c.id)).collect();
    if backed_off.is_empty() {
        return Ok(HealthCheck::ok(SOURCE));
    }
    if backed_off.len() == clients.len() {
        return Ok(HealthCheck::new(
            SOURCE,
            HealthType::Error,
            "All download clients are unavailable due to failures".into(),
            "#download-clients-are-unavailable-due-to-failures",
        ));
    }
    Ok(HealthCheck::new(
        SOURCE,
        HealthType::Warning,
        format!(
            "Download clients unavailable due to failures: {}",
            names(backed_off.iter().map(|c| c.name.as_str()))
        ),
        "#download-clients-are-unavailable-due-to-failures",
    ))
}

/// The enabled clients that are not in back-off, with their status, for
/// the checks that read where downloads land. A client that cannot be
/// asked is left to `DownloadClientCheck`.
async fn client_statuses(
    ctx: &Context<'_>,
) -> anyhow::Result<
    Vec<(
        DownloadClientDefinition,
        crate::downloadclients::DownloadClientInfo,
    )>,
> {
    let clients = ctx.clients.download_handling_enabled(true, ctx.now).await?;
    let context = ctx.clients.context(ctx.now).await?;
    let mut out = Vec::new();
    for definition in clients {
        let client = Client::new(&definition, &context);
        match client.status().await {
            Ok(status) => out.push((definition, status)),
            Err(
                e @ (ClientError::Unavailable(_)
                | ClientError::Authentication(_)
                | ClientError::Other(_)
                | ClientError::Rejected(_)
                | ClientError::NotSupported(_)),
            ) => {
                tracing::debug!(client = definition.name, error = %e, "unable to communicate with the download client");
            }
        }
    }
    Ok(out)
}

/// `DownloadClientSortingCheck`.
async fn download_client_sorting(ctx: &Context<'_>) -> anyhow::Result<HealthCheck> {
    const SOURCE: &str = "DownloadClientSortingCheck";
    for (definition, status) in client_statuses(ctx).await? {
        if let Some(mode) = status.sorting_mode.filter(|m| !m.trim().is_empty()) {
            return Ok(HealthCheck::new(
                SOURCE,
                HealthType::Warning,
                format!(
                    "Download client {} has {mode} sorting enabled for {APP_NAME}'s category. You should disable sorting in your download client to avoid import issues.",
                    definition.name
                ),
                "#download-folder-and-library-folder-not-different-folders",
            ));
        }
    }
    Ok(HealthCheck::ok(SOURCE))
}

/// `RemotePathMappingCheck`: the folders the clients download to, as this
/// process sees them.
async fn remote_path_mapping(ctx: &Context<'_>) -> anyhow::Result<HealthCheck> {
    const SOURCE: &str = "RemotePathMappingCheck";
    let handling = ctx
        .db
        .call(|conn| store::config::config(conn, "EnableCompletedDownloadHandling"))
        .await?
        .map(|v| v.trim().eq_ignore_ascii_case("true"))
        .unwrap_or(true);
    if !handling {
        return Ok(HealthCheck::ok(SOURCE));
    }
    let os_name = std::env::consts::OS;
    for (definition, status) in client_statuses(ctx).await? {
        let name = &definition.name;
        for folder in &status.output_root_folders {
            if !remote_path::is_rooted(folder) {
                let (message, fragment) = if !status.is_localhost {
                    (format!("Remote download client {name} places downloads in {folder} but this is not a valid {os_name} path. Review your remote path mappings and download client settings."), "#bad-remote-path-mapping")
                } else if ctx.is_docker {
                    (format!("You are using docker; download client {name} places downloads in {folder} but this is not a valid {os_name} path. Review your remote path mappings and download client settings."), "#docker-bad-remote-path-mapping")
                } else {
                    (format!("Local download client {name} places downloads in {folder} but this is not a valid {os_name} path. Review your download client settings."), "#bad-download-client-settings")
                };
                return Ok(HealthCheck::new(SOURCE, HealthType::Error, message, fragment));
            }
            if !std::path::Path::new(folder).is_dir() {
                let (message, fragment) = if ctx.is_docker {
                    (format!("You are using docker; download client {name} places downloads in {folder} but this directory does not appear to exist inside the container. Review your remote path mappings and container volume settings."), "#docker-bad-remote-path-mapping")
                } else if !status.is_localhost {
                    (format!("Remote download client {name} places downloads in {folder} but this directory does not appear to exist. Likely missing or incorrect remote path mapping."), "#bad-remote-path-mapping")
                } else {
                    (format!("Download client {name} places downloads in {folder} but {APP_NAME} cannot see this directory. You may need to adjust the folder's permissions."), "#permissions-error")
                };
                return Ok(HealthCheck::new(SOURCE, HealthType::Error, message, fragment));
            }
        }
    }
    Ok(HealthCheck::ok(SOURCE))
}

/// `DownloadClientRootFolderCheck`: a client downloading into a root folder.
async fn download_client_root_folder(ctx: &Context<'_>) -> anyhow::Result<HealthCheck> {
    const SOURCE: &str = "DownloadClientRootFolderCheck";
    let root_folders = ctx
        .db
        .call(|conn| store::catalog::list_root_folders(conn))
        .await?;
    for (definition, status) in client_statuses(ctx).await? {
        for folder in &status.output_root_folders {
            if let Some(root) = root_folders.iter().find(|r| paths_equal(&r.path, folder)) {
                return Ok(HealthCheck::new(
                    SOURCE,
                    HealthType::Warning,
                    format!(
                        "Download client {} places downloads in the root folder {}. You should not download to a root folder.",
                        definition.name, root.path
                    ),
                    "#downloads-in-root-folder",
                ));
            }
        }
    }
    Ok(HealthCheck::ok(SOURCE))
}

/// `PathEquals`: the same folder, trailing separators aside.
fn paths_equal(a: &str, b: &str) -> bool {
    let trim = |p: &str| p.trim_end_matches(['/', '\\']).to_string();
    trim(a) == trim(b)
}

/// `RootFolderService.GetBestRootFolderPath`: the longest root folder
/// holding the path, else the path's parent.
fn best_root_folder(path: &str, root_folders: &[String]) -> String {
    root_folders
        .iter()
        .filter(|r| remote_path::contains(r, path))
        .max_by_key(|r| r.len())
        .cloned()
        .or_else(|| remote_path::parent(path))
        .unwrap_or_else(|| path.to_string())
}

/// `RootFolderCheck`: every series' root folder must exist.
async fn root_folder(ctx: &Context<'_>) -> anyhow::Result<HealthCheck> {
    const SOURCE: &str = "RootFolderCheck";
    let (paths, root_folders) = ctx
        .db
        .call(|conn| {
            let paths: Vec<String> = store::series::list(conn)?.into_iter().map(|s| s.path).collect();
            let roots: Vec<String> = store::catalog::list_root_folders(conn)?
                .into_iter()
                .map(|r| r.path)
                .collect();
            Ok((paths, roots))
        })
        .await?;
    let mut missing: Vec<String> = Vec::new();
    for path in paths {
        let root = best_root_folder(&path, &root_folders);
        if (!remote_path::is_rooted(&root) || !std::path::Path::new(&root).is_dir())
            && !missing.contains(&root)
        {
            missing.push(root);
        }
    }
    match missing.len() {
        0 => Ok(HealthCheck::ok(SOURCE)),
        1 => Ok(HealthCheck::new(
            SOURCE,
            HealthType::Error,
            format!("Missing root folder: {}", missing[0]),
            "#missing-root-folder",
        )),
        _ => Ok(HealthCheck::new(
            SOURCE,
            HealthType::Error,
            format!("Multiple root folders are missing: {}", missing.join(" | ")),
            "#missing-root-folder",
        )),
    }
}

/// `ImportListRootFolderCheck`: every import list's root folder must be a
/// known, existing root folder.
async fn import_list_root_folder(ctx: &Context<'_>) -> anyhow::Result<HealthCheck> {
    const SOURCE: &str = "ImportListRootFolderCheck";
    let (lists, root_folders) = ctx
        .db
        .call(|conn| {
            let lists: Vec<(String, String)> =
                store::providers::list_providers(conn, ProviderKind::ImportList)?
                    .into_iter()
                    .map(|p| {
                        let root = p.attributes["rootFolderPath"].as_str().unwrap_or("").to_string();
                        (p.name, root)
                    })
                    .collect();
            let roots: Vec<String> = store::catalog::list_root_folders(conn)?
                .into_iter()
                .map(|r| r.path)
                .collect();
            Ok((lists, roots))
        })
        .await?;
    // Missing root folders in order of first appearance, with their lists.
    let mut missing: Vec<(String, Vec<String>)> = Vec::new();
    for (name, root) in lists {
        if let Some((_, names)) = missing.iter_mut().find(|(r, _)| *r == root) {
            names.push(name);
            continue;
        }
        let bad = root.trim().is_empty()
            || !remote_path::is_rooted(&root)
            || !root_folders.iter().any(|r| paths_equal(r, &root))
            || !std::path::Path::new(&root).is_dir();
        if bad {
            missing.push((root, vec![name]));
        }
    }
    let format = |(root, names): &(String, Vec<String>)| format!("{root} ({})", names.join(", "));
    match missing.len() {
        0 => Ok(HealthCheck::ok(SOURCE)),
        1 => Ok(HealthCheck::new(
            SOURCE,
            HealthType::Error,
            format!("Missing root folder for import list(s): {}", format(&missing[0])),
            "#import-list-missing-root-folder",
        )),
        _ => Ok(HealthCheck::new(
            SOURCE,
            HealthType::Error,
            format!(
                "Multiple root folders are missing for import lists: {}",
                missing.iter().map(format).collect::<Vec<_>>().join(" | ")
            ),
            "#import-list-missing-root-folder",
        )),
    }
}

/// `ImportMechanismCheck`: completed download handling must be on; a
/// client that cannot be asked leaves the verdict for later.
async fn import_mechanism(ctx: &Context<'_>) -> anyhow::Result<HealthCheck> {
    const SOURCE: &str = "ImportMechanismCheck";
    let clients = ctx.clients.enabled().await?;
    let context = ctx.clients.context(ctx.now).await?;
    let mut statuses = Vec::new();
    for definition in &clients {
        match Client::new(definition, &context).status().await {
            Ok(status) => statuses.push((definition, status)),
            // One or more download clients failed: assume the health is
            // okay and verify later.
            Err(_) => return Ok(HealthCheck::ok(SOURCE)),
        }
    }
    let defined = ctx
        .db
        .call(|conn| store::config::config(conn, "EnableCompletedDownloadHandling"))
        .await?;
    let Some(value) = defined else {
        // Migration helper logic.
        let all_local = statuses.iter().all(|(_, s)| s.is_localhost);
        let message = if !all_local {
            "Enable Completed Download Handling if possible (Multi-Computer unsupported)".to_string()
        } else if !clients.is_empty() && clients.iter().all(|d| d.kind == ClientKind::Sabnzbd) {
            "Enable Completed Download Handling if possible (Sabnzbd)".to_string()
        } else if !clients.is_empty() && clients.iter().all(|d| d.kind == ClientKind::Nzbget) {
            "Enable Completed Download Handling if possible (Nzbget)".to_string()
        } else {
            "Enable Completed Download Handling if possible".to_string()
        };
        return Ok(HealthCheck::new(
            SOURCE,
            HealthType::Warning,
            message,
            "#completedfailed-download-handling",
        ));
    };
    if !value.trim().eq_ignore_ascii_case("true") {
        return Ok(HealthCheck::new(
            SOURCE,
            HealthType::Warning,
            "Enable Completed Download Handling".into(),
            "#completed-download-handling-is-disabled",
        ));
    }
    Ok(HealthCheck::ok(SOURCE))
}

/// `DownloadClientRemovesCompletedDownloadsCheck`.
async fn download_client_removes_completed_downloads(ctx: &Context<'_>) -> anyhow::Result<HealthCheck> {
    const SOURCE: &str = "DownloadClientRemovesCompletedDownloadsCheck";
    for (definition, status) in client_statuses(ctx).await? {
        if status.removes_completed_downloads {
            return Ok(HealthCheck::new(
                SOURCE,
                HealthType::Warning,
                format!(
                    "Download client {} is set to remove completed downloads. This can result in downloads being removed from your client before {APP_NAME} can import them.",
                    definition.name
                ),
                "#download-client-removes-completed-downloads",
            ));
        }
    }
    Ok(HealthCheck::ok(SOURCE))
}

/// `DiskProvider.FolderWritable`: a file written and removed.
fn folder_writable(path: &str) -> bool {
    let test = std::path::Path::new(path).join("unarr_write_test.txt");
    match std::fs::write(&test, "") {
        Ok(()) => {
            let _ = std::fs::remove_file(&test);
            true
        }
        Err(_) => false,
    }
}

/// `RecyclingBinCheck`: a configured recycle bin must be writable.
async fn recycling_bin(ctx: &Context<'_>) -> anyhow::Result<HealthCheck> {
    const SOURCE: &str = "RecyclingBinCheck";
    let recycle_bin = ctx
        .db
        .call(|conn| store::config::config(conn, "RecycleBin"))
        .await?
        .unwrap_or_default();
    if recycle_bin.trim().is_empty() {
        return Ok(HealthCheck::ok(SOURCE));
    }
    if !folder_writable(&recycle_bin) {
        return Ok(HealthCheck::new(
            SOURCE,
            HealthType::Error,
            format!("Unable to write to configured recycling bin folder: {recycle_bin}. Ensure this path exists and is writable by the user running {APP_NAME}"),
            "#cannot-write-recycle-bin",
        ));
    }
    Ok(HealthCheck::ok(SOURCE))
}

/// A mounted file system as `/proc/mounts` lists it.
struct Mount {
    device: String,
    root: String,
    read_only: bool,
}

/// `ProcMountProvider.GetMounts`, from the given table text.
fn parse_mounts(text: &str) -> Vec<Mount> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let device = parts.next()?.to_string();
            let root = parts.next()?.replace("\\040", " ");
            let _fs_type = parts.next()?;
            let options = parts.next().unwrap_or("");
            let read_only = options.split(',').any(|o| o == "ro");
            Some(Mount {
                device,
                root,
                read_only,
            })
        })
        .collect()
}

/// `DiskProvider.GetMount`: the mount with the longest root above the path.
fn mount_of<'m>(mounts: &'m [Mount], path: &str) -> Option<&'m Mount> {
    mounts
        .iter()
        .filter(|m| m.root == "/" || path == m.root || path.starts_with(&format!("{}/", m.root)))
        .max_by_key(|m| m.root.len())
}

/// `MountCheck`: a series on a read-only mount.
async fn mount(ctx: &Context<'_>) -> anyhow::Result<HealthCheck> {
    const SOURCE: &str = "MountCheck";
    let Ok(table) = std::fs::read_to_string("/proc/mounts") else {
        return Ok(HealthCheck::ok(SOURCE));
    };
    let mounts = parse_mounts(&table);
    let paths: Vec<String> = ctx
        .db
        .call(|conn| Ok(store::series::list(conn)?.into_iter().map(|s| s.path).collect()))
        .await?;
    let mut seen_roots: Vec<&str> = Vec::new();
    let mut found: Vec<String> = Vec::new();
    for path in &paths {
        let Some(m) = mount_of(&mounts, path).filter(|m| m.read_only) else {
            continue;
        };
        if seen_roots.contains(&m.root.as_str()) {
            continue;
        }
        seen_roots.push(&m.root);
        found.push(format!("{} ({path})", m.device));
    }
    if found.is_empty() {
        return Ok(HealthCheck::ok(SOURCE));
    }
    Ok(HealthCheck::new(
        SOURCE,
        HealthType::Error,
        format!(
            "Mount containing a series path is mounted read-only: {}",
            found.join(", ")
        ),
        "#series-mount-ro",
    ))
}

/// `RemotePathMappingCheck.Check(EpisodeImportFailedEvent)`: what a failed
/// import says about the paths.
async fn remote_path_mapping_after_failure(ctx: &Context<'_>) -> anyhow::Result<HealthCheck> {
    const SOURCE: &str = "RemotePathMappingCheck";
    let handling = ctx
        .db
        .call(|conn| store::config::config(conn, "EnableCompletedDownloadHandling"))
        .await?
        .map(|v| v.trim().eq_ignore_ascii_case("true"))
        .unwrap_or(true);
    let Some(failure) = ctx.import_failure.filter(|_| handling) else {
        return Ok(HealthCheck::ok(SOURCE));
    };
    if let Some(path) = failure.path.as_deref().filter(|p| !p.is_empty()) {
        // The file is there but the import failed: likely permissions;
        // gone: removed part way through processing.
        if std::path::Path::new(path).is_file() {
            return Ok(HealthCheck::new(
                SOURCE,
                HealthType::Error,
                format!(
                    "{APP_NAME} can see but not access downloaded episode {path}. Likely permissions error."
                ),
                "#permissions-error",
            ));
        }
        return Ok(HealthCheck::new(
            SOURCE,
            HealthType::Error,
            format!("File {path} was removed part way through processing."),
            "#remote-path-file-removed",
        ));
    }
    // The failure came before any file was found: ask the client where
    // the download is.
    let Some(name) = failure.download_client.as_deref() else {
        return Ok(HealthCheck::ok(SOURCE));
    };
    let Some((definition, status)) = client_statuses(ctx)
        .await?
        .into_iter()
        .find(|(d, _)| d.name == name)
    else {
        return Ok(HealthCheck::ok(SOURCE));
    };
    let os_name = std::env::consts::OS;
    let Some(dlpath) = failure.output_path.as_deref().filter(|p| !p.trim().is_empty()) else {
        return Ok(HealthCheck::new(
            SOURCE,
            HealthType::Error,
            format!("{APP_NAME} failed to import (an) episode(s). Check your logs for details."),
            "#remote-path-import-failed",
        ));
    };
    let name = &definition.name;
    if !remote_path::is_rooted(dlpath) {
        let (message, fragment) = if !status.is_localhost {
            (format!("Remote download client {name} reported files in {dlpath} but this is not a valid {os_name} path. Review your remote path mappings and download client settings."), "#bad-remote-path-mapping")
        } else if ctx.is_docker {
            (format!("You are using docker; download client {name} reported files in {dlpath} but this is not a valid {os_name} path. Review your remote path mappings and download client settings."), "#docker-bad-remote-path-mapping")
        } else {
            (format!("Local download client {name} reported files in {dlpath} but this is not a valid {os_name} path. Review your download client settings."), "#bad-download-client-settings")
        };
        return Ok(HealthCheck::new(SOURCE, HealthType::Error, message, fragment));
    }
    if std::path::Path::new(dlpath).is_dir() {
        return Ok(HealthCheck::new(
            SOURCE,
            HealthType::Error,
            format!(
                "{APP_NAME} can see but not access download directory {dlpath}. Likely permissions error."
            ),
            "#permissions-error",
        ));
    }
    let (message, fragment) = if ctx.is_docker {
        (
            format!(
                "{APP_NAME} can see but not access download directory {dlpath}. Likely permissions error."
            ),
            "#docker-bad-remote-path-mapping",
        )
    } else if !status.is_localhost {
        (format!("Remote download client {name} reported files in {dlpath} but this directory does not appear to exist. Likely missing remote path mapping."), "#bad-remote-path-mapping")
    } else {
        (format!("Download client {name} reported files in {dlpath} but {APP_NAME} cannot see this directory. You may need to adjust the folder's permissions."), "#permissions-error")
    };
    Ok(HealthCheck::new(SOURCE, HealthType::Error, message, fragment))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mounts_are_read_and_matched() {
        let table = "/dev/sda1 / ext4 rw,relatime 0 0\n/dev/sdb1 /mnt/media ext4 ro,relatime 0 0\ntmpfs /mnt/media\\040two tmpfs rw 0 0\n";
        let mounts = parse_mounts(table);
        assert_eq!(mounts.len(), 3);
        assert_eq!(mounts[2].root, "/mnt/media two");
        let m = mount_of(&mounts, "/mnt/media/tv/Show").unwrap();
        assert_eq!(m.device, "/dev/sdb1");
        assert!(m.read_only);
        let root = mount_of(&mounts, "/home/user/tv").unwrap();
        assert_eq!(root.root, "/");
        assert!(!root.read_only);
        assert_eq!(mount_of(&mounts, "/mnt/mediax").unwrap().root, "/");
    }

    #[test]
    fn a_missing_recycle_bin_is_not_writable() {
        assert!(!folder_writable("/nonexistent/unarr/recycle"));
        let dir = std::env::temp_dir().join(format!("unarr_health_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(folder_writable(dir.to_str().unwrap()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn merges_verdicts_by_source() {
        let mut results = Vec::new();
        let bad = HealthCheck::new(
            "RootFolderCheck",
            HealthType::Error,
            "Missing root folder: /tv".into(),
            "#missing-root-folder",
        );
        assert!(merge(
            &mut results,
            vec![HealthCheck::ok("IndexerRssCheck"), bad.clone()]
        ));
        assert_eq!(results, vec![bad.clone()]);
        assert_eq!(
            results[0].wiki_url,
            "https://wiki.servarr.com/sonarr/system#missing-root-folder"
        );
        assert!(
            !merge(&mut results, vec![bad.clone()]),
            "the same verdict changes nothing"
        );
        assert!(merge(&mut results, vec![HealthCheck::ok("RootFolderCheck")]));
        assert!(results.is_empty());
    }

    #[test]
    fn picks_the_best_root_folder() {
        let roots = vec!["/tv".to_string(), "/tv/kids".to_string()];
        assert_eq!(best_root_folder("/tv/kids/Bluey", &roots), "/tv/kids");
        assert_eq!(best_root_folder("/tv/The Pitt", &roots), "/tv");
        assert_eq!(best_root_folder("/media/Other", &roots), "/media");
        assert!(paths_equal("/downloads/", "/downloads"));
    }
}
