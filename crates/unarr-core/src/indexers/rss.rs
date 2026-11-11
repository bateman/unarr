//! Newznab and Torznab result feeds into [`ReleaseInfo`], after the
//! originals' `RssParser`, `NewznabRssParser` and `TorznabRssParser`.

use chrono::{DateTime, Utc};
use roxmltree::{Document, Node};

use crate::indexers::{IndexerError, IndexerKind};
use crate::model::Protocol;

pub const NEWZNAB_NS: &str = "http://www.newznab.com/DTD/2010/feeds/attributes/";
pub const TORZNAB_NS: &str = "http://torznab.com/schemas/2015/feed";
pub const NZB_MIME: &str = "application/x-nzb";
pub const TORRENT_MIME: &str = "application/x-bittorrent";
pub const MAGNET_MIME: &str = "application/x-bittorrent;x-scheme-handler/magnet";

/// Indexer flags, bit values as the originals' `IndexerFlags`.
pub mod flags {
    pub const FREELEECH: i64 = 1;
    pub const HALFLEECH: i64 = 2;
    pub const DOUBLE_UPLOAD: i64 = 4;
    pub const INTERNAL: i64 = 8;
    pub const SCENE: i64 = 16;
    pub const FREELEECH75: i64 = 32;
    pub const FREELEECH25: i64 = 64;
    pub const NUKED: i64 = 128;

    /// `Enum.TryParse<IndexerFlags>(names, ignoreCase)`: a number, or flag
    /// names separated by commas.
    pub fn from_names(text: &str) -> i64 {
        if let Ok(n) = text.trim().parse::<i64>() {
            return n;
        }
        text.split(',')
            .map(|name| match name.trim().to_ascii_lowercase().as_str() {
                "freeleech" | "g_freeleech" => FREELEECH,
                "halfleech" | "g_halfleech" => HALFLEECH,
                "doubleupload" | "g_doubleupload" => DOUBLE_UPLOAD,
                "internal" | "g_internal" => INTERNAL,
                "scene" | "g_scene" => SCENE,
                "freeleech75" | "g_freeleech75" => FREELEECH75,
                "freeleech25" | "g_freeleech25" => FREELEECH25,
                "nuked" => NUKED,
                _ => 0,
            })
            .fold(0, |acc, f| acc | f)
    }
}

/// One release as an indexer lists it (`ReleaseInfo` plus the `TorrentInfo`
/// fields, which are `None` for usenet). Serialised as JSON for the
/// pending releases table.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ReleaseInfo {
    pub guid: String,
    pub title: String,
    pub size: i64,
    pub download_url: Option<String>,
    pub info_url: Option<String>,
    pub comment_url: Option<String>,
    pub indexer_id: i64,
    pub indexer: String,
    pub indexer_priority: i64,
    pub season_search_maximum_single_episode_age: i64,
    pub protocol: Protocol,
    pub tvdb_id: i64,
    pub tv_rage_id: i64,
    pub imdb_id: Option<String>,
    /// Radarr reads it; Sonarr's releases leave it at 0.
    pub tmdb_id: i64,
    pub publish_date: DateTime<Utc>,
    pub languages: Vec<i64>,
    pub indexer_flags: i64,
    pub magnet_url: Option<String>,
    pub info_hash: Option<String>,
    pub seeders: Option<i64>,
    pub peers: Option<i64>,
    /// The `container` attribute some indexers list (`vob`, `iso`, `m2ts`).
    pub container: Option<String>,
    /// Why the release was held back, when it comes from the pending
    /// releases rather than an indexer.
    pub pending_release_reason: Option<crate::downloads::pending::PendingReleaseReason>,
}

impl Default for ReleaseInfo {
    fn default() -> Self {
        ReleaseInfo {
            guid: String::new(),
            title: String::new(),
            size: 0,
            download_url: None,
            info_url: None,
            comment_url: None,
            indexer_id: 0,
            indexer: String::new(),
            indexer_priority: 25,
            season_search_maximum_single_episode_age: 0,
            protocol: Protocol::Unknown,
            tvdb_id: 0,
            tv_rage_id: 0,
            imdb_id: None,
            tmdb_id: 0,
            publish_date: DateTime::<Utc>::UNIX_EPOCH,
            languages: Vec::new(),
            indexer_flags: 0,
            magnet_url: None,
            info_hash: None,
            seeders: None,
            peers: None,
            container: None,
            pending_release_reason: None,
        }
    }
}

impl ReleaseInfo {
    pub fn age_days(&self, now: DateTime<Utc>) -> i64 {
        (now - self.publish_date).num_days()
    }
}

/// `<error code=.. description=..>` anywhere in the document.
pub fn check_error(doc: &Document, request_url: &str) -> Result<(), IndexerError> {
    let Some(error) = doc.descendants().find(|n| n.has_tag_name("error")) else {
        return Ok(());
    };
    let code: i64 = error.attribute("code").and_then(|c| c.parse().ok()).unwrap_or(0);
    let message = error.attribute("description").unwrap_or("").to_string();
    if (100..=199).contains(&code) {
        return Err(IndexerError::ApiKey(message));
    }
    if !request_url.contains("apikey=") && (message == "Missing parameter" || message.contains("apikey")) {
        return Err(IndexerError::ApiKey("Indexer requires an API key".into()));
    }
    if message == "Request limit reached" {
        return Err(IndexerError::RequestLimit("API limit reached".into()));
    }
    Err(IndexerError::Feed(message))
}

fn child_text<'a>(item: Node<'a, 'a>, name: &str) -> Option<&'a str> {
    item.children()
        .find(|n| n.has_tag_name(name))
        .and_then(|n| n.text())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// `newznab:attr` / `torznab:attr` values by name (case-insensitive), in order.
fn attrs<'a>(item: Node<'a, 'a>, ns: &str, name: &str) -> Vec<&'a str> {
    item.children()
        .filter(|n| n.tag_name().name() == "attr" && n.tag_name().namespace() == Some(ns))
        .filter(|n| n.attribute("name").is_some_and(|v| v.eq_ignore_ascii_case(name)))
        .filter_map(|n| n.attribute("value"))
        .collect()
}

fn attr<'a>(item: Node<'a, 'a>, ns: &str, name: &str) -> Option<&'a str> {
    attrs(item, ns, name).into_iter().next()
}

struct Enclosure<'a> {
    url: Option<&'a str>,
    mime: Option<&'a str>,
    length: i64,
}

fn enclosures<'a>(item: Node<'a, 'a>) -> Vec<Enclosure<'a>> {
    item.children()
        .filter(|n| n.has_tag_name("enclosure"))
        .map(|n| Enclosure {
            url: n.attribute("url"),
            mime: n.attribute("type"),
            length: n.attribute("length").and_then(|l| l.parse().ok()).unwrap_or(0),
        })
        .collect()
}

/// `RssParser.GetEnclosure` with the preferred mime types enforced.
fn preferred_enclosure<'a>(item: Node<'a, 'a>, preferred: &[&str]) -> Option<Enclosure<'a>> {
    let all = enclosures(item);
    if all.is_empty() {
        return None;
    }
    for mime in preferred {
        if let Some(pos) = all.iter().position(|e| e.mime == Some(*mime)) {
            return all.into_iter().nth(pos);
        }
    }
    None
}

/// `XElementExtensions.ParseDate`: RFC 2822 as feeds write it, else ISO
/// 8601, else the date with its time zone name stripped. Years outside
/// 1..=9999 are refused, as .NET's `DateTime` has none: a date arithmetic
/// further on could not take them.
pub fn parse_date(text: &str) -> Option<DateTime<Utc>> {
    use chrono::Datelike;
    parse_date_raw(text).filter(|d| (1..=9999).contains(&d.year()))
}

fn parse_date_raw(text: &str) -> Option<DateTime<Utc>> {
    let text = text.trim();
    if let Ok(d) = DateTime::parse_from_rfc2822(text) {
        return Some(d.with_timezone(&Utc));
    }
    if let Ok(d) = DateTime::parse_from_rfc3339(text) {
        return Some(d.with_timezone(&Utc));
    }
    if let Ok(d) = chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S") {
        return Some(d.and_utc());
    }
    if let Ok(d) = chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S") {
        return Some(d.and_utc());
    }
    // "Fri, 10 Jan 2025 03:12:00 GMT" style with a zone name.
    let stripped = text.rsplit_once(' ').map(|(d, _)| d).unwrap_or(text);
    if let Ok(d) = chrono::NaiveDateTime::parse_from_str(stripped, "%a, %d %b %Y %H:%M:%S") {
        return Some(d.and_utc());
    }
    None
}

/// `RssParser.ParseUrl`: resolve against the request URL, as `HttpUri +` does.
fn parse_url(value: Option<&str>, request_url: &str) -> Option<String> {
    let value = value?.trim();
    if value.is_empty() {
        return None;
    }
    if value.starts_with("magnet:") {
        return Some(value.to_string());
    }
    match reqwest::Url::parse(value) {
        Ok(u) => Some(u.to_string()),
        Err(_) => reqwest::Url::parse(request_url)
            .ok()
            .and_then(|base| base.join(value).ok())
            .map(|u| u.to_string()),
    }
}

/// `RssParser.ParseSize`: "1.3 GB", "1,234 MiB", plain bytes.
pub fn parse_size(text: &str, binary_prefix_default: bool) -> i64 {
    let text = text.trim();
    if text.is_empty() {
        return 0;
    }
    if text.chars().all(|c| c.is_ascii_digit()) {
        return text.parse().unwrap_or(0);
    }
    static SIZE_RE: crate::parser::netregex::Lazy =
        crate::net_regex!(r"(?<value>(?<!\.\d*)(?:\d+,)*\d+(?:\.\d{1,3})?)\W?(?<unit>[KMG]i?B)(?![\w/])");
    let Some(m) = SIZE_RE.captures(text) else {
        return 0;
    };
    let value: f64 = m
        .value("value")
        .unwrap_or("0")
        .replace(',', "")
        .parse()
        .unwrap_or(0.0);
    let unit = m.value("unit").unwrap_or("").to_lowercase();
    let (power, binary) = match unit.as_str() {
        "kb" => (1, binary_prefix_default),
        "mb" => (2, binary_prefix_default),
        "gb" => (3, binary_prefix_default),
        "kib" => (1, true),
        "mib" => (2, true),
        "gib" => (3, true),
        _ => return value as i64,
    };
    let prefix: f64 = if binary { 1024.0 } else { 1000.0 };
    (value * prefix.powi(power)).round() as i64
}

/// Parse a result feed. Items that fail individually are skipped, as the
/// originals log and skip them; an `<error>` or a feed of the other
/// protocol is an error for the whole page.
pub fn parse_feed(xml: &str, kind: IndexerKind, request_url: &str) -> Result<Vec<ReleaseInfo>, IndexerError> {
    let cleaned = clean_xml(xml);
    let doc = Document::parse(&cleaned).map_err(|e| IndexerError::Xml(e.to_string()))?;
    check_error(&doc, request_url)?;
    let root = doc.root_element();
    let Some(channel) = root.children().find(|n| n.has_tag_name("channel")) else {
        return Ok(Vec::new());
    };
    let items: Vec<Node> = channel.children().filter(|n| n.has_tag_name("item")).collect();
    let (ns, preferred): (&str, &[&str]) = match kind {
        IndexerKind::Newznab => (NEWZNAB_NS, &[NZB_MIME]),
        IndexerKind::Torznab => (TORZNAB_NS, &[TORRENT_MIME, MAGNET_MIME]),
    };

    // PostProcess: a feed whose enclosures are all of the other protocol
    // is a misconfigured indexer and yields nothing.
    let mut types: Vec<&str> = items
        .iter()
        .flat_map(|i| enclosures(*i).into_iter().filter_map(|e| e.mime))
        .collect();
    types.sort_unstable();
    types.dedup();
    if !types.is_empty() && !types.iter().any(|t| preferred.contains(t)) {
        let other: &[&str] = match kind {
            IndexerKind::Newznab => &[TORRENT_MIME, MAGNET_MIME],
            IndexerKind::Torznab => &[NZB_MIME],
        };
        if types.iter().any(|t| other.contains(t)) {
            tracing::warn!(
                url = request_url,
                found = types[0],
                "feed carries the other protocol's enclosures"
            );
            return Ok(Vec::new());
        }
        tracing::warn!(
            url = request_url,
            found = types[0],
            "feed carries unexpected enclosure types"
        );
    }

    let mut releases = Vec::new();
    for item in items {
        match parse_item(item, kind, ns, preferred, request_url) {
            Ok(r) => releases.push(r),
            Err(IndexerError::UnsupportedFeed(m)) => return Err(IndexerError::UnsupportedFeed(m)),
            Err(e) => tracing::warn!(url = request_url, error = %e, "skipping feed item"),
        }
    }
    Ok(releases)
}

/// `XmlCleaner`: entities feeds get wrong, and characters XML forbids.
fn clean_xml(xml: &str) -> String {
    let mut out = String::with_capacity(xml.len());
    for c in xml.chars() {
        let keep = matches!(c, '\t' | '\n' | '\r')
            || ('\u{20}'..='\u{D7FF}').contains(&c)
            || ('\u{E000}'..='\u{FFFD}').contains(&c)
            || c > '\u{FFFF}';
        if keep {
            out.push(c);
        }
    }
    // Named HTML entities other than the five XML ones are not XML.
    static ENTITY_RE: crate::parser::netregex::Lazy = crate::net_regex!(r"&[a-z]+;");
    ENTITY_RE.replace_all_with(&out, |m| match m.whole() {
        "&amp;" | "&lt;" | "&gt;" | "&quot;" | "&apos;" => m.whole().to_string(),
        // An unknown name is not XML either: escape the ampersand so the
        // text survives as written rather than failing the whole page.
        other => html_entity(other)
            .map(|c| format!("&#{};", c as u32))
            .unwrap_or_else(|| format!("&amp;{}", &other[1..])),
    })
}

fn html_entity(name: &str) -> Option<char> {
    Some(match name {
        "&nbsp;" => '\u{a0}',
        "&copy;" => '©',
        "&reg;" => '®',
        "&trade;" => '™',
        "&hellip;" => '…',
        "&mdash;" => '—',
        "&ndash;" => '–',
        "&lsquo;" => '‘',
        "&rsquo;" => '’',
        "&ldquo;" => '“',
        "&rdquo;" => '”',
        "&eacute;" => 'é',
        "&egrave;" => 'è',
        "&agrave;" => 'à',
        "&uuml;" => 'ü',
        "&ouml;" => 'ö',
        "&auml;" => 'ä',
        "&szlig;" => 'ß',
        "&ntilde;" => 'ñ',
        "&ccedil;" => 'ç',
        _ => return None,
    })
}

fn parse_item(
    item: Node<'_, '_>,
    kind: IndexerKind,
    ns: &str,
    preferred: &[&str],
    request_url: &str,
) -> Result<ReleaseInfo, IndexerError> {
    let guid = child_text(item, "guid")
        .map(str::to_string)
        .unwrap_or_else(|| format!("unarr-{:x}", rand_guid()));
    let title = child_text(item, "title").unwrap_or("Unknown").to_string();
    let publish_date = match kind {
        IndexerKind::Newznab => attr(item, ns, "usenetdate")
            .and_then(parse_date)
            .or_else(|| child_text(item, "pubDate").and_then(parse_date)),
        IndexerKind::Torznab => child_text(item, "pubDate").and_then(parse_date),
    };
    let Some(publish_date) = publish_date else {
        if child_text(item, "pubDate").is_none() {
            return Err(IndexerError::UnsupportedFeed(
                "Each item in the RSS feed must have a pubDate element with a valid publish date.".into(),
            ));
        }
        return Err(IndexerError::Feed(format!(
            "Unable to parse {}",
            child_text(item, "pubDate").unwrap_or("")
        )));
    };
    let enclosure = preferred_enclosure(item, preferred);
    let mut download_url = parse_url(enclosure.as_ref().and_then(|e| e.url), request_url);
    if kind == IndexerKind::Torznab
        && download_url
            .as_deref()
            .is_none_or(|u| reqwest::Url::parse(u).is_err() && !u.starts_with("magnet:"))
    {
        download_url = parse_url(enclosures(item).first().and_then(|e| e.url), request_url);
    }
    let comments = child_text(item, "comments");
    let info_url = parse_url(comments.map(|c| c.trim_end_matches("#comments")), request_url);
    let comment_url = parse_url(comments, request_url);
    let size = attr(item, ns, "size")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or_else(|| enclosure.as_ref().map(|e| e.length).unwrap_or(0));
    let languages = parse_languages(item, ns);
    let tvdb_id = attr(item, ns, "tvdbid").and_then(|v| v.parse().ok()).unwrap_or(0);
    let tv_rage_id = attr(item, ns, "rageid").and_then(|v| v.parse().ok()).unwrap_or(0);
    let imdb_id = attr(item, ns, "imdb")
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|v| *v > 0)
        .map(|v| format!("tt{v:07}"));

    let mut release = ReleaseInfo {
        guid,
        title,
        size,
        download_url,
        info_url,
        comment_url,
        indexer_id: 0,
        indexer: String::new(),
        indexer_priority: 25,
        season_search_maximum_single_episode_age: 0,
        protocol: match kind {
            IndexerKind::Newznab => Protocol::Usenet,
            IndexerKind::Torznab => Protocol::Torrent,
        },
        tvdb_id,
        tv_rage_id,
        imdb_id,
        tmdb_id: attr(item, ns, "tmdbid").and_then(|v| v.parse().ok()).unwrap_or(0),
        publish_date,
        languages,
        indexer_flags: 0,
        magnet_url: None,
        info_hash: None,
        seeders: None,
        peers: None,
        container: None,
        pending_release_reason: None,
    };

    match kind {
        IndexerKind::Newznab => {
            if attr(item, ns, "prematch") == Some("1") || attr(item, ns, "haspretime") == Some("1") {
                release.indexer_flags |= flags::SCENE;
            }
            if attr(item, ns, "nuked") == Some("1") {
                release.indexer_flags |= flags::NUKED;
            }
        }
        IndexerKind::Torznab => {
            // A missing attribute reads as an empty string in the originals
            // (`TryGetTorznabAttribute`), and the resource shows it so.
            release.info_hash = Some(attr(item, ns, "infohash").unwrap_or("").to_string());
            release.magnet_url = Some(attr(item, ns, "magneturl").unwrap_or("").to_string());
            if release.magnet_url.as_deref() == Some("") {
                if let Some(u) = release
                    .download_url
                    .as_deref()
                    .filter(|u| u.starts_with("magnet:"))
                {
                    release.magnet_url = Some(u.to_string());
                }
            }
            if release.info_hash.as_deref() == Some("") {
                if let Some(hash) = release
                    .magnet_url
                    .as_deref()
                    .and_then(crate::torrent::magnet_info_hash)
                {
                    release.info_hash = Some(hash);
                }
            }
            release.seeders = attr(item, ns, "seeders").and_then(|v| v.parse().ok());
            release.peers = attr(item, ns, "peers").and_then(|v| v.parse().ok()).or_else(|| {
                let s: Option<i64> = attr(item, ns, "seeders").and_then(|v| v.parse().ok());
                let l: Option<i64> = attr(item, ns, "leechers").and_then(|v| v.parse().ok());
                Some(s? + l?)
            });
            let factor =
                |name: &str| -> f64 { attr(item, ns, name).and_then(|v| v.parse().ok()).unwrap_or(1.0) };
            let down = factor("downloadvolumefactor");
            let up = factor("uploadvolumefactor");
            if down == 0.5 {
                release.indexer_flags |= flags::HALFLEECH;
            }
            if down == 0.75 {
                release.indexer_flags |= flags::FREELEECH25;
            }
            if down == 0.25 {
                release.indexer_flags |= flags::FREELEECH75;
            }
            if down == 0.0 {
                release.indexer_flags |= flags::FREELEECH;
            }
            if up == 2.0 {
                release.indexer_flags |= flags::DOUBLE_UPLOAD;
            }
            let tags = attrs(item, ns, "tag");
            if tags.iter().any(|t| t.eq_ignore_ascii_case("internal")) {
                release.indexer_flags |= flags::INTERNAL;
            }
            if tags.iter().any(|t| t.eq_ignore_ascii_case("scene")) {
                release.indexer_flags |= flags::SCENE;
            }
        }
    }
    Ok(release)
}

/// `GetLanguages`: `attr name="language"` values or `<language>` elements,
/// comma separated, mapped by English name.
fn parse_languages(item: Node<'_, '_>, ns: &str) -> Vec<i64> {
    let mut values: Vec<String> = attrs(item, ns, "language")
        .into_iter()
        .map(str::to_string)
        .collect();
    if values.is_empty() {
        values = item
            .children()
            .filter(|n| n.has_tag_name("language"))
            .filter_map(|n| n.text())
            .map(str::to_string)
            .collect();
    }
    let mut out = Vec::new();
    for v in values {
        for name in v.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            if let Some(l) = crate::quality::LANGUAGES
                .iter()
                .find(|l| l.name.eq_ignore_ascii_case(name))
            {
                out.push(l.id);
            }
        }
    }
    out
}

fn rand_guid() -> u128 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
    h.finish() as u128
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(kind: &str, name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/../../tests/fixtures/indexers/{kind}/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    #[test]
    fn parses_the_newznab_and_torznab_fixtures() {
        let url = "http://127.0.0.1:18989/newznab/api?t=tvsearch&apikey=k";
        let releases = parse_feed(&fixture("newznab", "rss.xml"), IndexerKind::Newznab, url).unwrap();
        assert_eq!(releases.len(), 9);
        let first = &releases[0];
        assert_eq!(first.title, "The.Pitt.S01E01.1080p.AMZN.WEB-DL.DDP5.1.H.264-FLUX");
        assert_eq!(first.size, 2684354560);
        assert_eq!(first.tvdb_id, 448176);
        assert_eq!(first.protocol, Protocol::Usenet);
        assert_eq!(
            first.download_url.as_deref(),
            Some("http://127.0.0.1:18989/newznab/download/a1f3c2d4e5b6a7c8d9e0f1a2b3c4d5e6.nzb")
        );
        assert_eq!(first.publish_date.to_rfc3339(), "2025-01-10T03:12:00+00:00");
        assert_eq!(
            first.info_url.as_deref(),
            Some("http://127.0.0.1:18989/newznab/details/a1f3c2d4e5b6a7c8d9e0f1a2b3c4d5e6")
        );

        let url = "http://127.0.0.1:18989/torznab/api?t=tvsearch&apikey=k";
        let torrents = parse_feed(&fixture("torznab", "rss.xml"), IndexerKind::Torznab, url).unwrap();
        assert_eq!(torrents.len(), 9);
        assert_eq!(torrents[0].seeders, Some(48));
        assert_eq!(torrents[0].peers, Some(52));
        assert_eq!(torrents[0].protocol, Protocol::Torrent);
        assert_eq!(torrents[0].info_hash.as_deref().map(str::len), Some(40));

        // A torrent feed read as Newznab yields nothing, with a warning.
        let crossed = parse_feed(&fixture("torznab", "rss.xml"), IndexerKind::Newznab, url).unwrap();
        assert!(crossed.is_empty());
        assert!(matches!(
            parse_feed(
                "<error code=\"100\" description=\"bad\"/>",
                IndexerKind::Newznab,
                url
            ),
            Err(IndexerError::ApiKey(_))
        ));
        assert!(matches!(
            parse_feed(
                "<error code=\"500\" description=\"Request limit reached\"/>",
                IndexerKind::Newznab,
                url
            ),
            Err(IndexerError::RequestLimit(_))
        ));
    }

    #[test]
    fn sizes_dates_and_magnets() {
        assert_eq!(parse_size("1.5 GB", true), 1610612736);
        assert_eq!(parse_size("1,024 MiB", false), 1073741824);
        assert_eq!(parse_size("12345", true), 12345);
        assert_eq!(parse_size("Size: 700 MB", false), 700_000_000);
        assert!(parse_date("Fri, 10 Jan 2025 03:12:00 +0000").is_some());
        assert!(parse_date("2025-01-10T03:12:00Z").is_some());
        assert!(parse_date("Fri, 10 Jan 2025 03:12:00 GMT").is_some());
        assert!(parse_date("Fri, 10 Jan 99999 03:12:00 +0000").is_none());
        assert_eq!(
            clean_xml("<a>Canci&oacute;n &amp; m&eacute;s</a>"),
            "<a>Canci&amp;oacute;n &amp; m&#233;s</a>"
        );
    }
}
