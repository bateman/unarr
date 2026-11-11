//! Handing a release to a client, after the originals' `UsenetClientBase`
//! and `TorrentClientBase`: fetch the NZB or the torrent (or take the
//! magnet), check it, give it to the client, return the id the client
//! knows it by.

use std::fmt;

use crate::decision::{ReleaseSource, RemoteEpisode};
use crate::indexers::IndexerDefinition;
use crate::model::{NamingConfig, Protocol};
use crate::naming::clean_file_name;
use crate::torrent;

use super::{Client, ClientError, DownloadClientItem, GrabRequest};

/// The originals' `ReleaseDownloadException` family plus the client's own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadError {
    /// `ReleaseUnavailableException`: the indexer no longer has it (404).
    ReleaseUnavailable(String),
    /// `ReleaseDownloadException`: fetching or parsing it failed.
    ReleaseDownload(String),
    /// `ReleaseBlockedException`: its hash is on the blocklist.
    ReleaseBlocked(String),
    /// `InvalidNzbException`.
    InvalidNzb(String),
    /// The client refused or could not be reached.
    Client(ClientError),
}

impl DownloadError {
    pub fn message(&self) -> &str {
        match self {
            DownloadError::ReleaseUnavailable(m)
            | DownloadError::ReleaseDownload(m)
            | DownloadError::ReleaseBlocked(m)
            | DownloadError::InvalidNzb(m) => m,
            DownloadError::Client(e) => e.message(),
        }
    }
}

impl fmt::Display for DownloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for DownloadError {}

impl From<ClientError> for DownloadError {
    fn from(e: ClientError) -> Self {
        DownloadError::Client(e)
    }
}

/// `Download(remoteEpisode, indexer)`: the download id. `blocklisted` holds
/// the torrent hashes the series' blocklist names, for the grab-time check
/// torrent indexers can ask for.
pub async fn download(
    client: &Client<'_>,
    remote: &RemoteEpisode,
    indexer: Option<&IndexerDefinition>,
    blocklisted: &[String],
) -> Result<String, DownloadError> {
    let request = GrabRequest::for_episode(remote, client.context.now);
    download_release(client, &request, indexer, blocklisted).await
}

/// `Download` for whatever the release is of: the download id.
pub async fn download_release(
    client: &Client<'_>,
    request: &GrabRequest<'_>,
    indexer: Option<&IndexerDefinition>,
    blocklisted: &[String],
) -> Result<String, DownloadError> {
    match client.protocol() {
        Protocol::Usenet => download_nzb(client, request).await,
        Protocol::Torrent => download_torrent(client, request, indexer, blocklisted).await,
        Protocol::Unknown => Err(DownloadError::Client(ClientError::NotSupported(
            "Unknown download protocol".into(),
        ))),
    }
}

/// `FileNameBuilder.CleanFileName(title)` with the default naming settings.
fn clean_title(request: &GrabRequest<'_>) -> String {
    clean_file_name(&request.release.title, &NamingConfig::default())
}

async fn fetch(
    http: &reqwest::Client,
    url: &str,
    accept: Option<&str>,
    what: &str,
    title: &str,
) -> Result<reqwest::Response, DownloadError> {
    let mut attempt = 0;
    loop {
        let mut request = http.get(url);
        if let Some(a) = accept {
            request = request.header(reqwest::header::ACCEPT, a);
        }
        let result = request.send().await;
        match result {
            Ok(response) => {
                let status = response.status();
                // `RetryStrategy`: twice more on server errors and timeouts.
                if (status.is_server_error() || status == reqwest::StatusCode::REQUEST_TIMEOUT) && attempt < 2
                {
                    attempt += 1;
                    tokio::time::sleep(std::time::Duration::from_secs(3 * attempt)).await;
                    continue;
                }
                if status == reqwest::StatusCode::NOT_FOUND {
                    tracing::error!(title, url = %crate::http::redact(url), "downloading {what} failed since it no longer exists");
                    return Err(DownloadError::ReleaseUnavailable(format!(
                        "Downloading {what} failed"
                    )));
                }
                if status.as_u16() == 429 {
                    tracing::error!(url = %crate::http::redact(url), "API grab limit reached");
                    return Err(DownloadError::ReleaseDownload(format!(
                        "Downloading {what} failed"
                    )));
                }
                if !status.is_success() && !status.is_redirection() {
                    tracing::error!(title, url = %crate::http::redact(url), %status, "downloading {what} failed");
                    return Err(DownloadError::ReleaseDownload(format!(
                        "Downloading {what} failed"
                    )));
                }
                return Ok(response);
            }
            Err(e) => {
                tracing::error!(title, url = %crate::http::redact(url), error = %e.without_url(), "downloading {what} failed");
                return Err(DownloadError::ReleaseDownload(format!(
                    "Downloading {what} failed"
                )));
            }
        }
    }
}

async fn download_nzb(client: &Client<'_>, request: &GrabRequest<'_>) -> Result<String, DownloadError> {
    let url = request.release.download_url.clone().unwrap_or_default();
    let filename = format!("{}.nzb", clean_title(request));
    let response = fetch(&client.context.http, &url, None, "nzb", &request.release.title).await?;
    let data = crate::http::read_body(response, crate::http::MAX_RESPONSE_BYTES)
        .await
        .map_err(|_| DownloadError::ReleaseDownload("Downloading nzb failed".into()))?;
    validate_nzb(&filename, &data)?;
    Ok(client.add_nzb(request, &filename, data).await?)
}

/// `NzbValidationService.Validate`.
pub fn validate_nzb(filename: &str, data: &[u8]) -> Result<(), DownloadError> {
    let text = String::from_utf8_lossy(data);
    // A DOCTYPE is normal in an NZB; an entity declaration is not, and the
    // parser would expand every use of it before any check runs ("billion
    // laughs"): a 130 KB file can cost a gigabyte.
    if text.contains("<!ENTITY") {
        return Err(DownloadError::InvalidNzb(format!(
            "Invalid NZB: entity declarations are not allowed [{filename}]"
        )));
    }
    let options = roxmltree::ParsingOptions {
        allow_dtd: true,
        ..Default::default()
    };
    let doc = roxmltree::Document::parse_with_options(&text, options)
        .map_err(|e| DownloadError::InvalidNzb(format!("Invalid NZB: {e} [{filename}]")))?;
    let root = doc.root_element();
    let name = root.tag_name().name();
    if name == "error" {
        if let (Some(code), Some(description)) = (root.attribute("code"), root.attribute("description")) {
            // nZEDb reports its errors with a 200 and an XML body.
            return Err(DownloadError::InvalidNzb(format!(
                "Invalid NZB: Contains indexer error: {code} - {description}"
            )));
        }
    }
    if name != "nzb" {
        return Err(DownloadError::InvalidNzb(format!(
            "Invalid NZB: Unexpected root element. Expected 'nzb' found '{name}' [{filename}]"
        )));
    }
    let files = root
        .children()
        .filter(|c| c.is_element() && c.tag_name().name() == "file")
        .count();
    if files == 0 {
        return Err(DownloadError::InvalidNzb(format!(
            "Invalid NZB: No files [{filename}]"
        )));
    }
    Ok(())
}

async fn download_torrent(
    client: &Client<'_>,
    request: &GrabRequest<'_>,
    indexer: Option<&IndexerDefinition>,
    blocklisted: &[String],
) -> Result<String, DownloadError> {
    let release = &request.release;
    let download_url = release.download_url.clone().unwrap_or_default();
    let mut magnet_url: Option<String> = None;
    let mut torrent_url: Option<String> = None;
    if !download_url.trim().is_empty() {
        if download_url.starts_with("magnet:") {
            magnet_url = Some(download_url.clone());
        } else {
            torrent_url = Some(download_url.clone());
        }
    }
    if let Some(m) = release.magnet_url.as_deref().filter(|m| !m.trim().is_empty()) {
        magnet_url = Some(m.to_string());
    }
    // None of the clients driven here prefers the torrent file.
    if let Some(magnet) = &magnet_url {
        match download_from_magnet(client, request, indexer, blocklisted, magnet).await {
            Ok(hash) => return Ok(hash),
            Err(DownloadError::Client(ClientError::NotSupported(message))) => {
                if torrent_url.is_none() {
                    return Err(DownloadError::ReleaseDownload(format!(
                        "Magnet not supported by download client. ({message})"
                    )));
                }
            }
            Err(e) => return Err(e),
        }
    }
    if let Some(url) = &torrent_url {
        return download_from_web_url(client, request, indexer, blocklisted, url, 0).await;
    }
    Err(DownloadError::ReleaseDownload(
        "Release has no download URL".into(),
    ))
}

async fn download_from_magnet(
    client: &Client<'_>,
    request: &GrabRequest<'_>,
    indexer: Option<&IndexerDefinition>,
    blocklisted: &[String],
    magnet: &str,
) -> Result<String, DownloadError> {
    let hash = torrent::magnet_info_hash(magnet).ok_or_else(|| {
        DownloadError::ReleaseDownload(format!(
            "Failed to parse magnetlink for {} '{}': '{}'",
            request.noun, request.release.title, magnet
        ))
    })?;
    ensure_not_blocklisted(request, indexer, blocklisted, &hash)?;
    let actual = client.add_magnet(request, &hash, magnet).await?;
    warn_on_hash_mismatch(client, request, &hash, &actual);
    Ok(actual)
}

fn download_from_web_url<'a>(
    client: &'a Client<'a>,
    request: &'a GrabRequest<'a>,
    indexer: Option<&'a IndexerDefinition>,
    blocklisted: &'a [String],
    url: &'a str,
    depth: u8,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, DownloadError>> + Send + 'a>> {
    Box::pin(async move {
        // Redirects are followed by hand: a tracker may answer with a magnet.
        let response = fetch(
            &client.context.no_redirect,
            url,
            Some("application/x-bittorrent"),
            "torrent",
            &request.release.title,
        )
        .await?;
        let status = response.status();
        if status == reqwest::StatusCode::MOVED_PERMANENTLY
            || status == reqwest::StatusCode::FOUND
            || status == reqwest::StatusCode::SEE_OTHER
        {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .map(String::from);
            let Some(location) = location else {
                return Err(DownloadError::ReleaseDownload(
                    "Downloading torrent failed: remote website tried to redirect without providing a location.".into(),
                ));
            };
            if location.starts_with("magnet:") {
                return download_from_magnet(client, request, indexer, blocklisted, &location).await;
            }
            if depth >= 5 {
                return Err(DownloadError::ReleaseDownload(
                    "Downloading torrent failed: too many redirects".into(),
                ));
            }
            let next = reqwest::Url::parse(url)
                .and_then(|base| base.join(&location))
                .map(|u| u.to_string())
                .unwrap_or(location);
            return download_from_web_url(client, request, indexer, blocklisted, &next, depth + 1).await;
        }
        if status.is_redirection() {
            return Err(DownloadError::ReleaseDownload(
                "Downloading torrent failed".into(),
            ));
        }
        let data = crate::http::read_body(response, crate::http::MAX_RESPONSE_BYTES)
            .await
            .map_err(|_| DownloadError::ReleaseDownload("Downloading torrent failed".into()))?;
        let filename = format!("{}.torrent", clean_title(request));
        let hash = torrent::info_hash(&data).ok_or_else(|| {
            tracing::trace!(content = %String::from_utf8_lossy(&data), "invalid torrent file contents");
            DownloadError::ReleaseDownload("Downloading torrent failed: the file is not a torrent".into())
        })?;
        ensure_not_blocklisted(request, indexer, blocklisted, &hash)?;
        let actual = client.add_torrent(request, &hash, &filename, data).await?;
        warn_on_hash_mismatch(client, request, &hash, &actual);
        Ok(actual)
    })
}

/// `EnsureReleaseIsNotBlocklisted`: a torrent indexer may ask that a hash
/// first learned at grab time be checked against the blocklist, unless the
/// user picked the release by hand.
fn ensure_not_blocklisted(
    request: &GrabRequest<'_>,
    indexer: Option<&IndexerDefinition>,
    blocklisted: &[String],
    hash: &str,
) -> Result<(), DownloadError> {
    let known = request
        .release
        .info_hash
        .as_deref()
        .is_some_and(|h| !h.trim().is_empty());
    if request.release.protocol != Protocol::Torrent || known {
        return Ok(());
    }
    let reject = indexer.is_some_and(|i| i.settings.reject_blocklisted_torrent_hashes_while_grabbing);
    if request.source != ReleaseSource::InteractiveSearch
        && reject
        && blocklisted.iter().any(|b| b.eq_ignore_ascii_case(hash))
    {
        return Err(DownloadError::ReleaseBlocked(
            "Release previously added to blocklist".into(),
        ));
    }
    Ok(())
}

fn warn_on_hash_mismatch(client: &Client<'_>, request: &GrabRequest<'_>, expected: &str, actual: &str) {
    if !actual.trim().is_empty() && !expected.eq_ignore_ascii_case(actual) {
        tracing::warn!(
            client = client.kind().implementation(),
            url = request.release.download_url.as_deref().unwrap_or(""),
            "the client did not return the expected InfoHash; the download in progress may be lost track of"
        );
    }
}

/// `DeleteItemData`: the item's files, when it has a known output path.
///
/// The originals delete whatever path the client reports. A client with
/// sorting on, or a broken one, can report a library folder or `/`; those
/// are refused here, with a warning, since one click would destroy the
/// library.
pub fn delete_item_data(item: &DownloadClientItem, root_folders: &[String]) {
    let Some(path) = item.output_path.as_deref().filter(|p| !p.is_empty()) else {
        return;
    };
    let p = std::path::Path::new(path);
    if let Err(why) = deletable(p, root_folders) {
        tracing::warn!(path, why, "refusing to delete download item data");
        return;
    }
    let result = if p.is_dir() {
        std::fs::remove_dir_all(p)
    } else if p.is_file() {
        std::fs::remove_file(p)
    } else {
        tracing::trace!(path, "download item data not found, nothing to delete");
        return;
    };
    if let Err(e) = result {
        tracing::warn!(path, error = %e, "unable to delete download item data");
    }
}

/// Whether a path is one a download's files can be in: not the root of a
/// drive or a top-level folder, not escaping with `..`, and not a library
/// root folder, inside one, or above one.
fn deletable(path: &std::path::Path, root_folders: &[String]) -> Result<(), &'static str> {
    use std::path::Component;
    let mut depth = 0;
    for c in path.components() {
        match c {
            Component::ParentDir => return Err("the path climbs out of its folder"),
            Component::Normal(_) => depth += 1,
            _ => {}
        }
    }
    if depth < 2 {
        return Err("the path is too close to the root of the filesystem");
    }
    for root in root_folders {
        let root = std::path::Path::new(root.trim_end_matches(['/', '\\']));
        if root.as_os_str().is_empty() {
            continue;
        }
        if path == root || root.starts_with(path) {
            return Err("the path holds a root folder");
        }
        if path.starts_with(root) {
            return Err("the path is inside a root folder");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deletes_only_where_a_download_can_be() {
        let roots = vec!["/tv".to_string(), "/mnt/media/anime/".to_string()];
        let ok = |p: &str| deletable(std::path::Path::new(p), &roots).is_ok();
        assert!(ok("/downloads/complete/Show.S01E01"));
        assert!(ok("/downloads/Show.S01E01.mkv"));
        assert!(!ok("/"));
        assert!(!ok("/downloads"));
        assert!(!ok("/downloads/complete/../../etc"));
        assert!(!ok("/tv"));
        assert!(!ok("/tv/Show/Season 01"));
        assert!(!ok("/mnt/media"));
        assert!(!ok("/mnt/media/anime/Show"));
        assert!(ok("/mnt/media/downloads/Show"));
    }

    #[test]
    fn validates_nzbs() {
        let ok = br#"<?xml version="1.0"?><!DOCTYPE nzb PUBLIC "-//newzBin//DTD NZB 1.1//EN" "http://www.newzbin.com/DTD/nzb/nzb-1.1.dtd"><nzb xmlns="http://www.newzbin.com/DTD/2003/nzb"><file poster="p" date="1" subject="s"><groups><group>g</group></groups><segments><segment bytes="1" number="1">x</segment></segments></file></nzb>"#;
        assert!(validate_nzb("a.nzb", ok).is_ok());
        // A DOCTYPE that defines an entity used many times would expand to
        // gigabytes before the first check.
        let laughs = format!(
            r#"<?xml version="1.0"?><!DOCTYPE nzb [<!ENTITY a "{}">]><nzb xmlns="http://www.newzbin.com/DTD/2003/nzb"><file subject="{}"/></nzb>"#,
            "x".repeat(100),
            "&a;".repeat(100)
        );
        assert!(matches!(
            validate_nzb("b.nzb", laughs.as_bytes()),
            Err(DownloadError::InvalidNzb(m)) if m.contains("entity declarations")
        ));
        let empty = br#"<nzb xmlns="http://www.newzbin.com/DTD/2003/nzb"></nzb>"#;
        assert_eq!(
            validate_nzb("a.nzb", empty).unwrap_err().message(),
            "Invalid NZB: No files [a.nzb]"
        );
        let wrong = br#"<html></html>"#;
        assert_eq!(
            validate_nzb("a.nzb", wrong).unwrap_err().message(),
            "Invalid NZB: Unexpected root element. Expected 'nzb' found 'html' [a.nzb]"
        );
        let error = br#"<error code="100" description="Incorrect user credentials"/>"#;
        assert_eq!(
            validate_nzb("a.nzb", error).unwrap_err().message(),
            "Invalid NZB: Contains indexer error: 100 - Incorrect user credentials"
        );
        assert!(validate_nzb("a.nzb", b"not xml").is_err());
    }
}
