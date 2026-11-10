//! Unarr's Skyhook client against the mock serving the recorded answers.

use unarr_core::metadata::{MetadataError, MetadataProvider, Skyhook, Upstreams};
use unarr_core::model::SeriesType;
use unarr_mock::{fixtures_dir, Mock};

async fn skyhook() -> (Mock, Skyhook) {
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let provider = Skyhook::with_client(
        unarr_core::http::build(std::time::Duration::from_secs(10)),
        Upstreams::mock(&mock.url()),
    );
    (mock, provider)
}

#[tokio::test]
async fn series_info_with_the_originals_user_agent() {
    let (mock, skyhook) = skyhook().await;
    let info = skyhook.series_info(81189).await.unwrap();
    assert_eq!(info.series.title, "Breaking Bad");
    assert_eq!(info.series.year, 2008);
    assert!(info.episodes.len() > 60);
    let requests = mock.requests();
    let paths: Vec<&str> = requests.iter().map(|r| r.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "/skyhook.sonarr.tv/v1/tvdb/shows/en/81189",
            "/services.sonarr.tv/v1/dailyseries"
        ]
    );
    assert!(requests
        .iter()
        .all(|r| r.user_agent == unarr_core::http::servarr_proxy_user_agent()));
}

#[tokio::test]
async fn daily_series_are_asked_once_an_hour() {
    let (mock, skyhook) = skyhook().await;
    assert_eq!(
        skyhook.series_info(271728).await.unwrap().series.series_type,
        SeriesType::Daily
    );
    skyhook.series_info(448176).await.unwrap();
    let daily = mock
        .requests()
        .iter()
        .filter(|r| r.path.ends_with("/dailyseries"))
        .count();
    assert_eq!(daily, 1);
}

#[tokio::test]
async fn not_found_and_search() {
    let (_mock, skyhook) = skyhook().await;
    assert_eq!(
        skyhook.series_info(999999999).await.unwrap_err(),
        MetadataError::SeriesNotFound(999999999)
    );
    let found = skyhook.search("The Pitt ").await.unwrap();
    assert_eq!(found[0].title, "The Pitt");
    assert!(found.len() > 5);
    assert!(skyhook.search("imdb:tt11280740").await.unwrap()[0].title == "Severance");
    assert!(skyhook.search("xyzzynothing").await.unwrap().is_empty());
}

#[tokio::test]
async fn an_unreachable_proxy_fails_the_search_as_the_original() {
    let skyhook = Skyhook::with_client(
        unarr_core::http::build(std::time::Duration::from_secs(2)),
        Upstreams::mock("http://127.0.0.1:9"),
    );
    let err = skyhook.search("anything").await.unwrap_err().to_string();
    assert!(
        err.starts_with("Search for 'anything' failed. Unable to communicate with SkyHook."),
        "{err}"
    );
}
