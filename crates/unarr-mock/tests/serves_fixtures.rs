//! The mock answers over real TCP from the fixture tree, and the shared
//! `unarr_core::http` client can reach it.

use unarr_mock::{fixtures_dir, Mock, API_KEY};

#[tokio::test]
async fn caps_results_and_downloads_come_from_the_fixtures() {
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let client = unarr_core::http::build(std::time::Duration::from_secs(5));

    let caps = client
        .get(format!("{}/newznab/api?t=caps&apikey={API_KEY}", mock.url()))
        .send()
        .await
        .unwrap();
    assert_eq!(caps.status(), 200);
    let text = caps.text().await.unwrap();
    assert!(text.contains("<tv-search available=\"yes\""), "{text}");

    let wrong = client
        .get(format!("{}/torznab/api?t=caps&apikey=nope", mock.url()))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(wrong.contains("code=\"100\""));

    let missing = client
        .get(format!(
            "{}/newznab/api?t=tvsearch&tvdbid=1&season=9&ep=9&apikey={API_KEY}",
            mock.url()
        ))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(missing.contains("total=\"0\""));

    let nzb = client
        .get(format!("{}/newznab/download/abc.nzb", mock.url()))
        .send()
        .await
        .unwrap();
    assert_eq!(nzb.headers()["content-type"], "application/x-nzb");
    assert!(nzb.text().await.unwrap().contains("<nzb"));

    let requests = mock.requests();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[0].query["t"], "caps");
    assert!(
        requests[0].user_agent.starts_with("Unarr/"),
        "{}",
        requests[0].user_agent
    );
    assert_eq!(
        requests[2].served,
        "newznab/tvsearch-tvdbid1-S9E9.xml (missing: empty)"
    );
    assert!(requests[3].served.ends_with("(synthesised)"));
}
