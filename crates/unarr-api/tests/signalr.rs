//! The official UI's SignalR client against Unarr's hub: negotiate, then the
//! JSON hub protocol handshake over a WebSocket and the `version` message.

use std::path::Path;

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;
use unarr_api::{AppState, LogBuffer, RuntimeInfo, Settings};
use unarr_core::{Db, Personality};

const API_KEY: &str = "0123456789abcdef0123456789abcdef";

fn build_state() -> AppState {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/sonarr");
    let db = Db::open_in_memory().unwrap();
    unarr_import::sonarr::import(&dir.join("sonarr.db"), &db).unwrap();
    let host = unarr_import::config_xml::read(&dir.join("config.xml"), 8989).unwrap();
    AppState::new(
        Personality::Sonarr,
        db,
        Settings::from_host_config(host, Personality::Sonarr),
        RuntimeInfo::detect(dir),
        LogBuffer::new(),
    )
}

#[tokio::test]
async fn ui_client_handshake_and_version_message() {
    let state = build_state();
    let hub = state.hub().clone();
    let app = unarr_api::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    // Without the token the hub is closed.
    let denied = tokio_tungstenite::connect_async(format!("ws://{addr}/signalr/messages")).await;
    assert!(denied.is_err(), "connection without access_token must be refused");

    let (mut ws, _) =
        tokio_tungstenite::connect_async(format!("ws://{addr}/signalr/messages?access_token={API_KEY}"))
            .await
            .expect("websocket upgrade");
    ws.send(Message::Text(
        "{\"protocol\":\"json\",\"version\":1}\u{1e}".into(),
    ))
    .await
    .unwrap();

    let mut received = Vec::new();
    while received.len() < 2 {
        match ws.next().await.expect("frame").unwrap() {
            Message::Text(t) => received.extend(
                t.split('\u{1e}')
                    .filter(|r| !r.is_empty())
                    .map(|r| serde_json::from_str::<serde_json::Value>(r).unwrap()),
            ),
            other => panic!("unexpected frame {other:?}"),
        }
    }
    assert_eq!(received[0], serde_json::json!({}), "handshake response");
    assert_eq!(received[1]["type"], 1);
    assert_eq!(received[1]["target"], "receiveMessage");
    assert_eq!(received[1]["arguments"][0]["name"], "version");
    assert_eq!(received[1]["arguments"][0]["body"]["version"], "4.0.20.3014");

    // A broadcast reaches the client as another receiveMessage invocation.
    hub.broadcast(
        "series",
        serde_json::json!({"action": "updated", "resource": {"id": 1}}),
    );
    let frame = ws.next().await.unwrap().unwrap();
    let text = frame.into_text().unwrap();
    let v: serde_json::Value = serde_json::from_str(text.trim_end_matches('\u{1e}')).unwrap();
    assert_eq!(v["arguments"][0]["name"], "series");

    // Client pings are tolerated; a close message ends the session cleanly.
    ws.send(Message::Text("{\"type\":6}\u{1e}".into())).await.unwrap();
    ws.send(Message::Text("{\"type\":7}\u{1e}".into())).await.unwrap();
    let closed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                _ => {}
            }
        }
    })
    .await;
    assert!(closed.is_ok(), "server should close after a type 7 message");
}
