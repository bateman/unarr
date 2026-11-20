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
    assert_eq!(q["X-Plex-Version"], "4.0.20.3014");
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
