//! The ping to plex.tv that keeps a Plex token alive (`PlexTvService.Ping`):
//! its query as the originals build it, and at most one a day per token.

use chrono::{TimeZone, Utc};
use serde_json::json;
use unarr_core::metadata::Upstreams;
use unarr_core::model::{Provider, ProviderKind};
use unarr_core::notifications::{Identity, NotificationDefinition, Notifier};
use unarr_core::Db;
use unarr_mock::clients::PLEX_TOKEN;
use unarr_mock::{fixtures_dir, Mock};

fn plex(port: u16, token: &str) -> NotificationDefinition {
    NotificationDefinition::from_provider(&Provider {
        kind: ProviderKind::Notification,
        id: 1,
        name: "Plex".into(),
        implementation: "PlexServer".into(),
        config_contract: "PlexServerSettings".into(),
        settings: json!({"host": "127.0.0.1", "port": port, "authToken": token, "updateLibrary": true}),
        tags: Vec::new(),
        attributes: json!({}),
    })
}

#[tokio::test]
async fn plex_tv_is_pinged_once_a_day_per_token() {
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let port = mock.port();
    let db = Db::open_in_memory().unwrap();
    let now = Utc.with_ymd_and_hms(2026, 11, 19, 21, 0, 0).unwrap();
    let notifier = Notifier::new(db, Identity::default(), now);
    notifier.set_upstreams(Upstreams::mock(&mock.url()));
    let pings = || {
        mock.requests()
            .into_iter()
            .filter(|r| r.path == "/plex.tv/api/v2/ping")
            .collect::<Vec<_>>()
    };

    // The connection test pings first, then asks the server.
    notifier.test(&plex(port, PLEX_TOKEN), now).await;
    let first = pings();
    assert_eq!(first.len(), 1, "{:?}", mock.requests());
    let q = &first[0].query;
    assert_eq!(q["X-Plex-Product"], "Sonarr");
    assert_eq!(q["X-Plex-Device-Name"], "Sonarr");
    assert_eq!(q["X-Plex-Platform"], "Windows");
    assert_eq!(q["X-Plex-Platform-Version"], "7");
    assert_eq!(q["X-Plex-Version"], unarr_core::pin::SONARR.version);
    assert_eq!(q["X-Plex-Token"], PLEX_TOKEN);
    // The identifier the server requests carry, stored once.
    let server = mock
        .requests()
        .into_iter()
        .find(|r| r.path == "/library/sections")
        .unwrap();
    assert_eq!(
        q["X-Plex-Client-Identifier"],
        server.query["X-Plex-Client-Identifier"]
    );
    assert_eq!(first[0].method, "GET");

    // Within the day the same token is not pinged again; another token is.
    // A blank one is not pinged at all (where the originals throw), and
    // the server is still asked.
    notifier.test(&plex(port, PLEX_TOKEN), now).await;
    assert_eq!(pings().len(), 1);
    notifier.test(&plex(port, "another-token"), now).await;
    let asked = mock.requests().len();
    notifier.test(&plex(port, " "), now).await;
    let tokens: Vec<String> = pings().iter().map(|r| r.query["X-Plex-Token"].clone()).collect();
    assert_eq!(tokens, [PLEX_TOKEN, "another-token"]);
    assert!(mock.requests()[asked..]
        .iter()
        .any(|r| r.path == "/library/sections"));
}

/// A Plex connection that hears of imports, stored where the notifier
/// looks for its connections.
fn plex_provider(port: u16, token: &str) -> Provider {
    Provider {
        kind: ProviderKind::Notification,
        id: 1,
        name: "Plex".into(),
        implementation: "PlexServer".into(),
        config_contract: "PlexServerSettings".into(),
        settings: json!({"host": "127.0.0.1", "port": port, "authToken": token, "updateLibrary": true}),
        tags: Vec::new(),
        attributes: json!({"onDownload": true}),
    }
}

fn store(db: &Db, provider: Provider) {
    db.call_blocking(move |conn| unarr_core::store::providers::insert_provider(conn, &provider))
        .unwrap();
}

fn pings_with(mock: &Mock, token: &str) -> usize {
    mock.requests()
        .iter()
        .filter(|r| {
            r.path == "/plex.tv/api/v2/ping" && r.query.get("X-Plex-Token").map(String::as_str) == Some(token)
        })
        .count()
}

/// The import is what keeps a token alive day after day: an episode or a
/// movie imported into a library with a Plex connection pings plex.tv
/// before the library update is queued (`PlexServer.UpdateIfEnabled`).
#[tokio::test]
async fn an_import_pings_plex_tv_for_series_and_movies() {
    use std::sync::Arc;
    use unarr_core::mediafiles::events::{EpisodeImported, ImportEvent};
    use unarr_core::mediafiles::local::LocalEpisode;
    use unarr_core::model::{EpisodeFile, QualityModel};
    use unarr_core::movies::import::events::{MovieImportEvent, MovieImported};
    use unarr_core::movies::import::local::LocalMovie;
    use unarr_core::movies::model::MovieFile;
    use unarr_core::movies::notifications::MovieNotifier;

    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let port = mock.port();
    let now = Utc.with_ymd_and_hms(2026, 11, 19, 21, 0, 0).unwrap();
    let quality = QualityModel {
        quality_id: 7,
        version: 1,
        real: 0,
        is_repack: false,
    };

    // An episode, into Sonarr's library.
    let db = Db::open_in_memory().unwrap();
    store(&db, plex_provider(port, "series-token"));
    let notifier = Notifier::new(db, Identity::default(), now);
    notifier.set_upstreams(Upstreams::mock(&mock.url()));
    let series = unarr_core::decision::testing::series(1, "The Pitt", 1);
    let episode = ImportEvent::EpisodeImported(Box::new(EpisodeImported {
        local: LocalEpisode {
            series: Some(series),
            ..LocalEpisode::default()
        },
        file: EpisodeFile {
            id: 1,
            series_id: 1,
            season_number: 1,
            relative_path: "Season 01/The Pitt - S01E01.mkv".into(),
            original_file_path: None,
            size: 1,
            date_added: now,
            scene_name: None,
            release_group: None,
            release_hash: None,
            quality,
            languages: vec![1],
            media_info: None,
            indexer_flags: 0,
            release_type: Default::default(),
        },
        old_files: Vec::new(),
        new_download: true,
        client: None,
        download_id: None,
    }));
    notifier.on_import_events(&[episode], now).await;
    assert_eq!(pings_with(&mock, "series-token"), 1, "{:?}", mock.requests());

    // A movie, into Radarr's.
    let db = Db::open_in_memory_for(unarr_core::Personality::Radarr).unwrap();
    store(&db, plex_provider(port, "movie-token"));
    let base = Notifier::new(db, Identity::default(), now);
    base.set_upstreams(Upstreams::mock(&mock.url()));
    let movies = MovieNotifier::new(Arc::new(base));
    let movie = unarr_core::movies::decision::testing::movie(1, "Candyman", 1992);
    let imported = MovieImportEvent::Imported(Box::new(MovieImported {
        local: LocalMovie {
            movie: Some(movie),
            ..LocalMovie::default()
        },
        file: MovieFile {
            id: 1,
            movie_id: 1,
            relative_path: "Candyman (1992).mkv".into(),
            original_file_path: None,
            size: 1,
            date_added: now,
            scene_name: None,
            release_group: None,
            edition: None,
            quality,
            languages: vec![1],
            media_info: None,
            indexer_flags: 0,
        },
        old_files: Vec::new(),
        new_download: true,
        client: None,
        download_id: None,
    }));
    movies.on_import_events(&[imported], now).await;
    assert_eq!(pings_with(&mock, "movie-token"), 1, "{:?}", mock.requests());
}
