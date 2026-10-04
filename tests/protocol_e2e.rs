//! End-to-end protocol test: spin up in-process mock lobby + match WebSocket
//! servers on ephemeral localhost ports, then drive `run_once` through the
//! full flow — lobby handshake → matched notification → match handshake →
//! `turn_request` → `turn_action` reply → `match_end`.
//!
//! This is the counterpart to `chameleon_wiring.rs`: that suite verifies the
//! *brain* wiring (and needs an artifact bundle); this suite verifies the
//! *protocol* wiring (and needs nothing beyond tokio + tokio-tungstenite).
//! Together they cover the two contracts the bot must satisfy at runtime.

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::handshake::server::{
    ErrorResponse, Request as WsRequest, Response as WsResponse,
};
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{accept_async, accept_hdr_async};
use tokio_tungstenite::WebSocketStream;

type Ws = WebSocketStream<tokio::net::TcpStream>;

/// Accept a WebSocket connection, echoing the first offered subprotocol back
/// to the client. The production client always sends
/// `Sec-WebSocket-Protocol: chipzen-bot-token, <token>` on the match leg;
/// tokio-tungstenite refuses the handshake if the server accepts without
/// selecting a subprotocol, so the mock must echo one.
#[allow(clippy::result_large_err)] // tung's ErrorResponse is intrinsic to the callback signature
async fn accept_with_subprotocol_echo(
    stream: tokio::net::TcpStream,
) -> Result<Ws, tokio_tungstenite::tungstenite::Error> {
    accept_hdr_async(
        stream,
        |req: &WsRequest, mut resp: WsResponse| -> Result<WsResponse, ErrorResponse> {
            if let Some(offered) = req.headers().get("Sec-WebSocket-Protocol")
                && let Ok(s) = offered.to_str()
                && let Some(first) = s.split(',').map(str::trim).next()
                && let Ok(hv) = HeaderValue::from_str(first)
            {
                resp.headers_mut()
                    .insert("Sec-WebSocket-Protocol", hv);
            }
            Ok(resp)
        },
    )
    .await
}

/// Read the next `Text` frame, ignoring ping/pong/other frames.
async fn next_text(ws: &mut Ws) -> Option<String> {
    while let Some(msg) = ws.next().await {
        match msg {
            Ok(Message::Text(t)) => return Some(t.to_string()),
            Ok(Message::Close(_)) | Err(_) => return None,
            _ => {}
        }
    }
    None
}

async fn send_json(ws: &mut Ws, value: Value) {
    let _ = ws.send(Message::Text(value.to_string().into())).await;
}

/// Mock lobby: accept one connection, do the handshake, deliver `matched`
/// pointing at `gateway_ws_url`, then close.
async fn spawn_lobby_server(bot_id: &str, gateway_ws_url: String) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("ws://{addr}/ws/external/bot/{bot_id}");

    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("lobby accept");
        let mut ws = accept_async(stream).await.expect("lobby ws handshake");

        // 1) client authenticate
        let _ = next_text(&mut ws).await;
        // 2) server hello
        send_json(&mut ws, json!({"type": "hello", "endpoint": "lobby"})).await;
        // 3) matched
        send_json(
            &mut ws,
            json!({
                "type": "matched",
                "match_id": "m-test",
                "participant_id": "p-test",
                "gateway_ws_url": gateway_ws_url,
                "rated": false,
            }),
        )
        .await;

        // Hold the socket open long enough for the client to read; the client
        // drops its own side after `wait_for_matched` returns.
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let _ = ws.close(None).await;
    });

    url
}

/// Mock match gateway: perform the Layer-1 handshake, send one
/// `turn_request`, capture the client's `turn_action`, then send `match_end`.
/// Returns the gateway URL and a handle yielding all client→server frames as
/// decoded `Value`s for assertions.
async fn spawn_match_server() -> (String, tokio::task::JoinHandle<Vec<Value>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("ws://{addr}/ws/external/match/m-test/p-test");

    let handle = tokio::spawn(async move {
        let mut received: Vec<Value> = Vec::new();

        let Ok((stream, _)) = listener.accept().await else {
            return received;
        };
        let Ok(mut ws) = accept_with_subprotocol_echo(stream).await else {
            return received;
        };

        // 1) client authenticate
        if let Some(t) = next_text(&mut ws).await
            && let Ok(v) = serde_json::from_str::<Value>(&t)
        {
            received.push(v);
        }
        // 2) server hello
        send_json(
            &mut ws,
            json!({
                "type": "hello",
                "selected_version": "1.0",
                "game_type": "hu-nl",
            }),
        )
        .await;
        // 3) client hello
        if let Some(t) = next_text(&mut ws).await
            && let Ok(v) = serde_json::from_str::<Value>(&t)
        {
            received.push(v);
        }
        // 4) turn_request
        send_json(
            &mut ws,
            json!({
                "type": "turn_request",
                "request_id": 42,
                "state": {
                    "to_call": 100,
                    "pot": 150,
                    "min_raise": 200,
                    "max_raise": 2000,
                    "street": "preflop"
                },
                "valid_actions": ["fold", "call", "raise"]
            }),
        )
        .await;
        // 5) client turn_action
        if let Some(t) = next_text(&mut ws).await
            && let Ok(v) = serde_json::from_str::<Value>(&t)
        {
            received.push(v);
        }
        // 6) match_end
        send_json(
            &mut ws,
            json!({"type": "match_end", "reason": "normal", "results": [1, -1]}),
        )
        .await;

        // Drain until the client closes.
        while let Some(msg) = ws.next().await {
            match msg {
                Ok(Message::Close(_)) | Err(_) => break,
                _ => {}
            }
        }
        received
    });

    (url, handle)
}

/// Like `spawn_match_server` but rejects the first `turn_action` with
/// `action_rejected` and then expects a retry with the SAME `request_id`.
/// After the retry, sends `match_end`. This exercises the rejection-fallback
/// path end-to-end (`strategy::rejection_fallback` + the brain's
/// `note_action_rejected`).
async fn spawn_match_server_with_rejection() -> (String, tokio::task::JoinHandle<Vec<Value>>)
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("ws://{addr}/ws/external/match/m-test/p-test");

    let handle = tokio::spawn(async move {
        let mut received: Vec<Value> = Vec::new();

        let Ok((stream, _)) = listener.accept().await else {
            return received;
        };
        let Ok(mut ws) = accept_with_subprotocol_echo(stream).await else {
            return received;
        };

        // authenticate
        if let Some(t) = next_text(&mut ws).await
            && let Ok(v) = serde_json::from_str::<Value>(&t)
        {
            received.push(v);
        }
        // server hello
        send_json(
            &mut ws,
            json!({"type": "hello", "selected_version": "1.0", "game_type": "hu-nl"}),
        )
        .await;
        // client hello
        if let Some(t) = next_text(&mut ws).await
            && let Ok(v) = serde_json::from_str::<Value>(&t)
        {
            received.push(v);
        }
        // turn_request
        send_json(
            &mut ws,
            json!({
                "type": "turn_request",
                "request_id": 99,
                "state": {
                    "to_call": 100,
                    "pot": 150,
                    "street": "preflop"
                },
                "valid_actions": ["fold", "call", "raise"]
            }),
        )
        .await;
        // first turn_action (should be rejected)
        if let Some(t) = next_text(&mut ws).await
            && let Ok(v) = serde_json::from_str::<Value>(&t)
        {
            received.push(v);
        }
        // action_rejected with the same request_id and a narrow valid set
        send_json(
            &mut ws,
            json!({
                "type": "action_rejected",
                "request_id": 99,
                "reason": "illegal_action",
                "valid_actions": ["fold", "check"]
            }),
        )
        .await;
        // retry turn_action
        if let Some(t) = next_text(&mut ws).await
            && let Ok(v) = serde_json::from_str::<Value>(&t)
        {
            received.push(v);
        }
        // match_end
        send_json(
            &mut ws,
            json!({"type": "match_end", "reason": "normal", "results": [1, -1]}),
        )
        .await;

        while let Some(msg) = ws.next().await {
            match msg {
                Ok(Message::Close(_)) | Err(_) => break,
                _ => {}
            }
        }
        received
    });

    (url, handle)
}

/// Full flow with no brain (trivial reference policy).
///
/// The `turn_request` we send is a preflop spot where the trivial policy
/// folds deterministically: `to_call = 100`, `pot = 150`, so
/// `to_call <= pot/2` is false (100 > 75) and `fold` is in `valid_actions`.
#[tokio::test]
async fn end_to_end_lobby_and_match_without_brain() {
    let (match_url, match_handle) = spawn_match_server().await;
    let lobby_url = spawn_lobby_server("test-bot", match_url).await;

    // Strip the lobby path to get the platform origin the CLI would receive.
    let base = lobby_url
        .split("/ws/external")
        .next()
        .expect("lobby url has origin")
        .to_string();
    assert!(base.starts_with("ws://127.0.0.1:"));

    let end = cham_chipzen::client::run_once(&base, "test-bot", "cz_extbot_test", None)
        .await
        .expect("run_once should succeed")
        .expect("match_end should be produced");

    assert_eq!(end.r#type, "match_end");
    assert_eq!(end.reason.as_deref(), Some("normal"));

    let received = match_handle.await.expect("match server task");

    // Frames the client must have sent: authenticate, hello, turn_action.
    assert_eq!(received.len(), 3, "frames: {received:#?}");
    assert_eq!(received[0]["type"], "authenticate");
    assert_eq!(received[0]["match_id"], "m-test");
    assert_eq!(received[1]["type"], "hello");
    assert_eq!(received[1]["match_id"], "m-test");
    assert_eq!(received[1]["supported_versions"][0], "1.0");
    assert_eq!(received[2]["type"], "turn_action");
    assert_eq!(received[2]["match_id"], "m-test");
    assert_eq!(received[2]["request_id"], 42);
    // Deterministic trivial-policy outcome for the spot we sent.
    assert_eq!(received[2]["action"], "fold");
    assert!(received[2]["params"].is_object());
}

/// Same lobby + handshake plumbing as the happy path, but the match server
/// rejects the first `turn_action`. The client must retry with the SAME
/// `request_id` using a guaranteed-legal fallback verb from the rejection's
/// `valid_actions` (in this case `fold` or `check`), and then the match
/// closes cleanly.
#[tokio::test]
async fn end_to_end_action_rejected_retry_same_request_id() {
    let (match_url, match_handle) = spawn_match_server_with_rejection().await;
    let lobby_url = spawn_lobby_server("test-bot", match_url).await;
    let base = lobby_url
        .split("/ws/external")
        .next()
        .expect("lobby url has origin")
        .to_string();

    let end = cham_chipzen::client::run_once(&base, "test-bot", "cz_extbot_test", None)
        .await
        .expect("run_once should succeed")
        .expect("match_end should be produced");
    assert_eq!(end.r#type, "match_end");

    let received = match_handle.await.expect("match server task");

    // Frames: authenticate, hello, first turn_action, retry turn_action.
    assert_eq!(received.len(), 4, "frames: {received:#?}");
    assert_eq!(received[2]["type"], "turn_action");
    assert_eq!(received[2]["request_id"], 99);
    assert_eq!(received[3]["type"], "turn_action");
    // Critical: retry MUST echo the same request_id (protocol contract).
    assert_eq!(received[3]["request_id"], 99);
    // And it MUST be one of the fallback verbs offered by the rejection.
    let retry_action = received[3]["action"].as_str().unwrap_or("");
    assert!(
        retry_action == "fold" || retry_action == "check",
        "retry action was {retry_action:?}, not in the rejection's valid_actions"
    );
    assert!(received[3]["params"].is_object());
}

