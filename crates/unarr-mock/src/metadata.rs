//! The metadata upstreams: Skyhook, the Servarr services, XEM, TheTVDB's
//! artwork host and Simkl for Sonarr, api.radarr.video, the Servarr
//! services and TMDB's image host for Radarr, served from `tests/fixtures/metadata/` under
//! `/<host><path>`, with the rules `scripts/mock-servarr.py` documents. The
//! Python mock reaches the same handler through the HTTPS tunnels the
//! recording Sonarr opens; Unarr's tests ask for `/<host><path>` directly.

use std::path::Path;

use axum::http::StatusCode;

use crate::clients::{percent_decode, Reply};

/// The hosts the originals fetch metadata and artwork from.
pub const HOSTS: &[&str] = &[
    "skyhook.sonarr.tv",
    "services.sonarr.tv",
    "thexem.info",
    "artworks.thetvdb.com",
    "api.simkl.com",
    // Radarr's: its metadata server, the Servarr services, TMDB's images.
    "api.radarr.video",
    "radarr.servarr.com",
    "image.tmdb.org",
];

/// The hosts that serve images, answered with a stand-in.
const ARTWORK_HOSTS: &[&str] = &["artworks.thetvdb.com", "image.tmdb.org"];
/// The Servarr services, whose `v1/time` is the clock and whose other
/// answers are named without the query.
const SERVICES_HOSTS: &[&str] = &["services.sonarr.tv", "radarr.servarr.com"];

/// Which stand-in image an artwork path gets, by the folder its kind
/// names; the first match wins.
const ARTWORK_KINDS: &[(&str, &str)] = &[
    ("clearlogo", "clearlogo.png"),
    ("clearart", "clearlogo.png"),
    ("poster", "poster.jpg"),
    ("background", "fanart.jpg"),
    ("fanart", "fanart.jpg"),
    ("banner", "banner.jpg"),
    ("graphical", "banner.jpg"),
    ("screencap", "screenshot.jpg"),
    ("episode", "screenshot.jpg"),
    ("photo", "headshot.jpg"),
    ("actor", "headshot.jpg"),
];

/// The fixture name, relative to `metadata/<host>/`, for a path and its raw
/// query string.
pub fn fixture_name(path: &str, raw_query: &str) -> String {
    let mut name = path.trim_matches('/').to_string();
    if name.is_empty() {
        name = "index".into();
    }
    // The movies changed since a moment: one recording for every moment.
    if name == "v1/movie/changed" {
        return name;
    }
    if !raw_query.is_empty() {
        let slugged = crate::slug(&percent_decode(raw_query));
        name.push('/');
        name.push_str(if slugged.is_empty() { "query" } else { &slugged });
    }
    name
}

/// The answer for `/<host><path>`, or `None` when the host is not one of
/// [`HOSTS`].
pub fn handle(fixtures: &Path, full_path: &str, raw_query: &str) -> Option<Reply> {
    let rest = full_path.strip_prefix('/')?;
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    if !HOSTS.contains(&host) {
        return None;
    }
    let path = format!("/{path}");
    if ARTWORK_HOSTS.contains(&host) {
        let lowered = path.to_ascii_lowercase();
        let name = ARTWORK_KINDS
            .iter()
            .find(|(marker, _)| lowered.contains(marker))
            .map_or("poster.jpg", |(_, name)| name);
        let content_type = if name.ends_with(".png") {
            "image/png"
        } else {
            "image/jpeg"
        };
        let body = std::fs::read(fixtures.join("metadata/artwork").join(name)).unwrap_or_default();
        return Some((StatusCode::OK, body, content_type, format!("artwork/{name}")).into());
    }
    if SERVICES_HOSTS.contains(&host) && path == "/v1/time" {
        let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.7fZ");
        let body = format!("{{\"dateTimeUtc\":\"{now}\"}}").into_bytes();
        return Some(
            (
                StatusCode::OK,
                body,
                "application/json",
                format!("{host}/v1/time (now)"),
            )
                .into(),
        );
    }
    // The Servarr services are asked with the original's version, OS and
    // runtime in the query: one recording answers every version.
    let query = if SERVICES_HOSTS.contains(&host) {
        ""
    } else {
        raw_query
    };
    let name = fixture_name(&path, query);
    let stored = fixtures.join("metadata").join(host).join(format!("{name}.json"));
    let Ok(body) = std::fs::read(&stored) else {
        let body = format!("{{\"message\":\"NotFound\",\"description\":\"no fixture {host}/{name}.json\"}}");
        return Some(
            (
                StatusCode::NOT_FOUND,
                body.into_bytes(),
                "application/json",
                format!("{host}/{name} (missing)"),
            )
                .into(),
        );
    };
    let status = std::fs::read_to_string(stored.with_extension("status"))
        .ok()
        .and_then(|s| s.trim().parse::<u16>().ok())
        .and_then(|s| StatusCode::from_u16(s).ok())
        .unwrap_or(StatusCode::OK);
    Some(
        (
            status,
            body,
            "application/json; charset=utf-8",
            format!("{host}/{name}"),
        )
            .into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_the_python_mock() {
        assert_eq!(
            fixture_name("/v1/tvdb/shows/en/448176", ""),
            "v1/tvdb/shows/en/448176"
        );
        assert_eq!(
            fixture_name("/v1/tvdb/search/en/", "term=the%20pitt"),
            "v1/tvdb/search/en/term-the-pitt"
        );
        assert_eq!(
            fixture_name("/v1/tvdb/search/en/", "term=imdb%3Att11280740"),
            "v1/tvdb/search/en/term-imdb-tt11280740"
        );
        assert_eq!(
            fixture_name("/map/all", "id=424536&origin=tvdb"),
            "map/all/id-424536-origin-tvdb"
        );
    }

    #[test]
    fn serves_recordings_statuses_and_artwork() {
        let fixtures = crate::fixtures_dir();
        let show = handle(&fixtures, "/skyhook.sonarr.tv/v1/tvdb/shows/en/448176", "").unwrap();
        assert_eq!(show.status, StatusCode::OK);
        let missing = handle(&fixtures, "/skyhook.sonarr.tv/v1/tvdb/shows/en/999999999", "").unwrap();
        assert_eq!(missing.status, StatusCode::NOT_FOUND);
        let poster = handle(
            &fixtures,
            "/artworks.thetvdb.com/banners/v4/series/448176/posters/6941c7b719bbb.jpg",
            "",
        )
        .unwrap();
        assert_eq!(poster.content_type, "image/jpeg");
        assert!(poster.body.starts_with(&[0xFF, 0xD8]));
        assert!(handle(&fixtures, "/newznab/api", "").is_none());
    }

    #[test]
    fn the_servarr_services_answer_whatever_version_asks() {
        let fixtures = crate::fixtures_dir();
        for (path, name) in [
            (
                "/radarr.servarr.com/v1/update/master",
                "radarr.servarr.com/v1/update/master",
            ),
            (
                "/services.sonarr.tv/v1/notification",
                "services.sonarr.tv/v1/notification",
            ),
        ] {
            for query in [
                "version=1.0.0.1&os=linux",
                "version=99.0.0.1&os=linux&runtimeVer=10.0.0",
            ] {
                let reply = handle(&fixtures, path, query).unwrap();
                assert_eq!(reply.status, StatusCode::OK, "{path}?{query}");
                assert_eq!(reply.served, name, "{path}?{query}");
            }
        }
        // Elsewhere the query still names the recording.
        let search = handle(
            &fixtures,
            "/skyhook.sonarr.tv/v1/tvdb/search/en/",
            "term=the%20pitt",
        )
        .unwrap();
        assert_eq!(search.served, "skyhook.sonarr.tv/v1/tvdb/search/en/term-the-pitt");
    }
}
