//! The shared HTTP client's reading of bodies: whole up to the ceiling,
//! refused past it, where an endless or heavily compressed answer would
//! otherwise be read into memory until the process is killed.

use unarr_core::http::{read_body, read_text, BodyError};
use unarr_mock::{fixtures_dir, Mock};

#[tokio::test]
async fn bodies_are_read_up_to_the_ceiling_and_refused_past_it() {
    let mock = Mock::start(fixtures_dir(), 0).await.unwrap();
    let http = unarr_core::http::build(std::time::Duration::from_secs(10));

    let response = http.get(format!("{}/big/512", mock.url())).send().await.unwrap();
    let body = read_body(response, 1024).await.unwrap();
    assert_eq!(body.len(), 512);
    assert!(body.iter().all(|b| *b == b'x'));

    let response = http.get(format!("{}/big/4096", mock.url())).send().await.unwrap();
    match read_body(response, 1024).await {
        Err(BodyError::TooLarge { limit }) => assert_eq!(limit, 1024),
        other => panic!("expected the ceiling to apply: {other:?}"),
    }

    // Exactly the limit passes; one more byte does not.
    let response = http.get(format!("{}/big/1024", mock.url())).send().await.unwrap();
    assert_eq!(read_text(response, 1024).await.unwrap().len(), 1024);
    let response = http.get(format!("{}/big/1025", mock.url())).send().await.unwrap();
    assert!(matches!(
        read_text(response, 1024).await,
        Err(BodyError::TooLarge { .. })
    ));
}
