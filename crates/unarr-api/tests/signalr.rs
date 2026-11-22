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
    assert_eq!(
        received[1]["arguments"][0]["body"]["version"],
        unarr_core::pin::SONARR.version
    );

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

/// One message from the hub, past the handshake.
async fn next_message<S>(ws: &mut S) -> serde_json::Value
where
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    let frame = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
        .await
        .expect("a message in time")
        .expect("frame")
        .unwrap();
    serde_json::from_str(frame.into_text().unwrap().trim_end_matches('\u{1e}')).unwrap()
}

/// Rows 2.12 and 2.13 of `docs/audit/2-http.md`: a client asking for
/// version 1 gets it with a connection token; a new connection's `version`
/// message goes to every client, as `MessageHub.OnConnectedAsync` sends it.
#[tokio::test]
async fn negotiate_version_one_and_version_to_every_client() {
    let app = unarr_api::router(build_state());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let negotiate = |query: &'static str| async move {
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let request = format!(
            "POST /signalr/messages/negotiate?{query}access_token={API_KEY} HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut answer = String::new();
        stream.read_to_string(&mut answer).await.unwrap();
        let body = answer.split("\r\n\r\n").nth(1).unwrap().to_owned();
        serde_json::from_str::<serde_json::Value>(&body).unwrap()
    };
    let v1 = negotiate("negotiateVersion=1&").await;
    assert_eq!(v1["negotiateVersion"], 1);
    let token = v1["connectionToken"].as_str().unwrap();
    assert_ne!(token, v1["connectionId"].as_str().unwrap());
    let v0 = negotiate("").await;
    assert_eq!(v0["negotiateVersion"], 0);
    assert!(v0.get("connectionToken").is_none());

    let connect = || async {
        let (mut ws, _) = tokio_tungstenite::connect_async(format!(
            "ws://{addr}/signalr/messages?id=x&access_token={API_KEY}"
        ))
        .await
        .expect("websocket upgrade");
        ws.send(Message::Text(
            "{\"protocol\":\"json\",\"version\":1}\u{1e}".into(),
        ))
        .await
        .unwrap();
        assert_eq!(next_message(&mut ws).await, serde_json::json!({}));
        assert_eq!(next_message(&mut ws).await["arguments"][0]["name"], "version");
        ws
    };
    let mut first = connect().await;
    let _second = connect().await;
    // The first client hears of the second.
    let heard = next_message(&mut first).await;
    assert_eq!(heard["arguments"][0]["name"], "version");
    assert_eq!(
        heard["arguments"][0]["body"]["version"],
        unarr_core::pin::SONARR.version
    );
}
