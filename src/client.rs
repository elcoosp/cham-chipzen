//! Lobby + match data-plane client — Rust port of `client.py`.
//!
//! Two WebSocket legs, raw JSON frames (see [`crate::proto`]):
//!
//! ```text
//! lobby WS  ──►  authenticate  ──►  hello  ──►  (wait)  ──►  matched
//!                                                               │
//!                                        resolve gateway_ws_url │
//!                                                               ▼
//! match WS  ──►  authenticate → server hello → client hello  ──►  play  ──►  match_end
//! ```

use futures_util::{SinkExt, StreamExt};
use http::HeaderValue;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info, warn};
use url::{Host, Url};

use crate::agent::ChamBrain;
use crate::error::Error;
use crate::proto::{bot_token_subprotocols, parse_frame, Frame, OutFrame, StateView};
use crate::strategy;

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

// ---------------------------------------------------------------------------
// URL helpers (port of client.lobby_ws_url / resolve_gateway_url / _normalise_base)
// ---------------------------------------------------------------------------

/// Strip any path/query from a base origin, keeping scheme + netloc. Tolerates
/// a trailing slash or an accidental path; defaults a missing scheme to `wss`.
fn normalise_base(base_url: &str) -> Result<String, Error> {
    let raw = if base_url.contains("://") {
        base_url.to_string()
    } else {
        format!("wss://{base_url}")
    };
    let u = Url::parse(&raw).map_err(|e| Error::Connection(format!("bad base-url {base_url:?}: {e}")))?;
    let scheme = match u.scheme() {
        "ws" | "wss" => u.scheme().to_string(),
        "http" => "ws".to_string(),
        "https" => "wss".to_string(),
        "" => "wss".to_string(),
        other => return Err(Error::Connection(format!("unsupported scheme {other:?}"))),
    };
    let host_str = match u.host() {
        Some(Host::Domain(d)) => d.to_string(),
        Some(Host::Ipv4(v4)) => v4.to_string(),
        Some(Host::Ipv6(v6)) => format!("[{v6}]"),
        None => return Err(Error::Connection("base-url has no host".into())),
    };
    // Keep only an *explicit* port from the input URL. Default ports implied
    // by scheme (80/http, 443/https, etc.) must not leak into the output —
    // tests assert `http://example.com` → `ws://example.com` (no `:80`).
    let port = u.port();
    Ok(match port {
        Some(p) => format!("{scheme}://{host_str}:{p}"),
        None => format!("{scheme}://{host_str}"),
    })
}

/// `<base>/ws/external/bot/{bot_id}` — the lobby endpoint.
pub fn lobby_ws_url(base_url: &str, bot_id: &str) -> Result<String, Error> {
    Ok(format!("{}/ws/external/bot/{}", normalise_base(base_url)?, bot_id))
}

/// Resolve the `matched.gateway_ws_url` path against the base origin. The
/// notification carries a *path* (`/ws/external/match/{mid}/{pid}`); the token
/// is NOT on the query string (CZ issue 2932) — it travels in
/// `Sec-WebSocket-Protocol` (see [`bot_token_subprotocols`]).
pub fn resolve_gateway_url(base_url: &str, gateway_ws_path: &str) -> Result<String, Error> {
    if gateway_ws_path.starts_with("ws://") || gateway_ws_path.starts_with("wss://") {
        return Ok(gateway_ws_path.to_string()); // defensive: full URL from a future server
    }
    let base = normalise_base(base_url)?;
    let path = if gateway_ws_path.starts_with('/') {
        gateway_ws_path.to_string()
    } else {
        format!("/{gateway_ws_path}")
    };
    Ok(format!("{base}{path}"))
}

/// Reject non-localhost plain `ws://` at connect time (the platform permits
/// unencrypted ws:// only on localhost).
fn check_transport_security(url: &str) -> Result<(), Error> {
    let u = Url::parse(url).map_err(|e| Error::Connection(format!("bad url {url:?}: {e}")))?;
    if u.scheme() == "ws" {
        let local = matches!(u.host(), Some(Host::Domain(d)) if d == "localhost")
            || matches!(u.host(), Some(Host::Ipv4(v4)) if v4.is_loopback());
        if !local {
            return Err(Error::Connection(format!(
                "unencrypted ws:// permitted only on localhost, got {url}"
            )));
        }
    }
    Ok(())
}

async fn send(ws: &mut Ws, frame: &OutFrame<'_>) -> Result<(), Error> {
    ws.send(Message::Text(frame.to_json().into())).await?;
    Ok(())
}

/// Receive one text frame, decoding to a `Frame`; skips binary/ping/pong and
/// malformed JSON like the Python `_loads` (returns `{}` → ignored type).
async fn recv_frame(ws: &mut Ws) -> Result<Option<Frame>, Error> {
    while let Some(msg) = ws.next().await {
        match msg? {
            Message::Text(t) => {
                if let Some(f) = parse_frame(&t) {
                    return Ok(Some(f));
                }
                debug!("ignoring malformed frame");
            }
            Message::Close(_) => return Ok(None),
            // tungstenite answers protocol pings automatically; ignore others.
            _ => {}
        }
    }
    Ok(None)
}

// ---------------------------------------------------------------------------
// Lobby
// ---------------------------------------------------------------------------

/// Connect the lobby, authenticate, and return the first `matched` notify
/// (docs §3–§4): open `/ws/external/bot/{bot_id}`, send `authenticate` as the
/// first frame, log the server `hello`, answer `ping` with `pong`, and return
/// the first `matched` payload. Raises [`Error::Connection`] if the lobby
/// closes before a `matched` arrives or we are evicted.
pub async fn wait_for_matched(base_url: &str, bot_id: &str, token: &str) -> Result<Frame, Error> {
    let url = lobby_ws_url(base_url, bot_id)?;
    check_transport_security(&url)?;
    info!("lobby: connecting to {url}");
    let (mut ws, _resp) = connect_async(&url).await?;

    send(&mut ws, &OutFrame::AuthenticateLobby { token }).await?;

    while let Some(msg) = recv_frame(&mut ws).await? {
        match msg.r#type.as_str() {
            "hello" => info!("lobby: connected (endpoint={:?})", msg.endpoint),
            "ping" => {
                // Heartbeat — must pong within 5s or the lobby closes us (4000).
                send(&mut ws, &OutFrame::PongLobby).await?;
            }
            "matched" => {
                info!(
                    "lobby: matched match={:?} participant={:?} rated={:?}",
                    msg.match_id, msg.participant_id, msg.rated
                );
                return Ok(msg);
            }
            "evict" => {
                return Err(Error::Connection(
                    "lobby: evicted (replaced by a newer connection)".into(),
                ));
            }
            other => debug!("lobby: ignoring frame type={other}"), // forward-compat
        }
    }
    Err(Error::Connection(
        "lobby: connection closed before a 'matched' notification arrived".into(),
    ))
}

// ---------------------------------------------------------------------------
// Match data plane
// ---------------------------------------------------------------------------

/// Run one match end-to-end over the per-match gateway WS (docs §5–§6).
///
/// The `cz_extbot_` token authenticates via the `Sec-WebSocket-Protocol`
/// header, NOT the query string. Runs the Layer-1 handshake then drives the
/// game loop until `match_end`, delegating decisions to `brain` when present
/// and to the trivial reference policy otherwise. Returns the `match_end`
/// payload, or `None` if the socket closed without one.
pub async fn play_match(
    gateway_url: &str,
    match_id: &str,
    token: &str,
    brain: Option<Arc<Mutex<ChamBrain>>>,
) -> Result<Option<Frame>, Error> {
    check_transport_security(gateway_url)?;
    info!("match: connecting (match={match_id})");
    let (mut ws, _resp) = connect_async_with_protocols(gateway_url, token).await?;

    // ---- Layer-1 handshake (docs §6.1) ----
    // The executor ignores this token (the gateway's internal JWT is
    // authoritative), but the authenticate frame MUST be sent first or the
    // handshake stalls.
    send(&mut ws, &OutFrame::AuthenticateMatch { match_id }).await?;

    let server_hello = match recv_frame(&mut ws).await? {
        Some(f) => f,
        None => return Ok(None),
    };
    if server_hello.r#type != "hello" {
        error!(
            "match: expected server hello, got {:?}",
            server_hello.r#type
        );
        return Ok(None);
    }
    info!(
        "match: server hello version={:?} game_type={:?}",
        server_hello.selected_version, server_hello.game_type
    );
    send(&mut ws, &OutFrame::ClientHello { match_id }).await?;

    // ---- Game loop (docs §6.2) ----
    while let Some(msg) = recv_frame(&mut ws).await? {
        if let Some(end) = handle_match_message(&mut ws, match_id, &msg, &brain).await? {
            if let Some(b) = &brain {
                b.lock().await.on_match_end();
            }
            return Ok(Some(end));
        }
    }
    info!("match: connection closed without a match_end");
    Ok(None)
}

async fn connect_async_with_protocols(url: &str, token: &str) -> Result<(Ws, http::Response<Option<Vec<u8>>>), Error> {
    let mut request = url
        .into_client_request()
        .map_err(|e| Error::Connection(format!("bad request url {url:?}: {e}")))?;
    // Join the offer list with ", " exactly as the `websockets` lib does.
    let joined = bot_token_subprotocols(token).join(", ");
    let value = HeaderValue::from_str(&joined)
        .map_err(|_| Error::Connection("subprotocol offer contained illegal header bytes".into()))?;
    request.headers_mut().insert("Sec-WebSocket-Protocol", value);
    let (ws, resp) = connect_async(request).await?;
    Ok((ws, resp))
}

/// Handle one server→bot match frame (port of `handle_match_message`).
///
/// Returns the `match_end` payload when the match ends (signalling the caller
/// to stop), otherwise `None`. Exposed for unit testing with a fake sink.
pub async fn handle_match_message(
    ws: &mut Ws,
    match_id: &str,
    msg: &Frame,
    brain: &Option<Arc<Mutex<ChamBrain>>>,
) -> Result<Option<Frame>, Error> {
    match msg.r#type.as_str() {
        "ping" => {
            send(ws, &OutFrame::PongMatch { match_id }).await?;
            Ok(None)
        }
        "turn_request" => {
            let state = msg.state.clone().unwrap_or(StateView::default());
            let (action, params) = decide_action(brain, state, &msg.valid_actions).await;
            debug!("turn_request -> {action} {params}");
            send(
                ws,
                &OutFrame::TurnAction {
                    match_id,
                    request_id: msg.request_id.as_ref().unwrap_or(&serde_json::Value::Null),
                    action: &action,
                    params: &params,
                },
            )
            .await?;
            Ok(None)
        }
        "action_rejected" => {
            // Retry with the SAME request_id using a guaranteed-legal fallback
            // from the rejection's valid_actions (or check/fold per the
            // server's auto-action guarantee when the field is absent).
            let fallback = strategy::rejection_fallback(&msg.valid_actions);
            info!(
                "match: action rejected ({:?}); retrying with {}",
                msg.reason, fallback.action
            );
            // The shadow advanced with the rejected action; it can no longer
            // be trusted for this hand. Reset it so subsequent turns degrade
            // safely via the actor-mismatch guard.
            if let Some(b) = brain {
                b.lock().await.note_action_rejected();
            }
            send(
                ws,
                &OutFrame::TurnAction {
                    match_id,
                    request_id: msg.request_id.as_ref().unwrap_or(&serde_json::Value::Null),
                    action: &fallback.action,
                    params: &fallback.params,
                },
            )
            .await?;
            Ok(None)
        }
        "match_end" => {
            info!("match: ended reason={:?}", msg.reason);
            Ok(Some(msg.clone()))
        }
        "error" => {
            warn!("match: error [{:?}] {:?}", msg.code, msg.message);
            Ok(None)
        }
        // Frames carrying the villain's latest public action. We only ingest
        // `opponent_action` — the frame whose semantics unambiguously say
        // "the opponent did this". `action_accepted` typically echoes the
        // action *we* just sent (already applied to the shadow in
        // `decide_turn`), so ingesting it would double-advance the shadow.
        // `turn_result` is likewise ambiguous (round summary vs. per-actor
        // notification); we log it for observability but do not ingest.
        "opponent_action" => {
            if let Some(b) = brain {
                if let Some(name) = extract_action_name(&msg.raw) {
                    let params = msg
                        .raw
                        .get("params")
                        .cloned()
                        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                    b.lock().await.observe_opponent_action(&name, &params);
                }
            }
            Ok(None)
        }
        "action_accepted" | "turn_result" => {
            // Not ingested (see comment above). Log at debug so we can
            // diagnose the platform's actual frame semantics without
            // polluting the shadow state.
            debug!(
                "match: observed {} frame (not ingested); shape={}",
                msg.r#type,
                msg.raw
                    .get("action")
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "<no action field>".into())
            );
            Ok(None)
        }
        // match_start / round_start / phase_change / round_result /
        // action_timeout / session_control / session_token / reconnected and
        // any unknown future type: silently ignore (forward compat).
        other => {
            debug!("match: observed frame type={other}");
            Ok(None)
        }
    }
}

/// Best-effort extraction of the actor's action verb from an
/// action-notification frame (`action: "call"` / `action: {name: ...}` /
/// `action_name: ...` shapes all tolerated).
fn extract_action_name(raw: &serde_json::Value) -> Option<String> {
    // Tolerate the shapes observed / anticipated on the wire:
    //   {"action": "call"}
    //   {"action": {"name": "call"}}
    //   {"action": {"type": "call"}}
    //   {"action_name": "call"}
    if let Some(v) = raw.get("action_name").and_then(|v| v.as_str()) {
        return Some(v.to_string());
    }
    let v = raw.get("action")?;
    if let Some(s) = v.as_str() {
        return Some(s.to_string());
    }
    if let Some(s) = v.get("name").and_then(|n| n.as_str()) {
        return Some(s.to_string());
    }
    if let Some(s) = v.get("type").and_then(|n| n.as_str()) {
        return Some(s.to_string());
    }
    None
}

/// Decide the reply to a turn_request: CHAMELEON brain when loaded, trivial
/// reference policy otherwise. Blocking agent work runs on the blocking pool.
async fn decide_action(
    brain: &Option<Arc<Mutex<ChamBrain>>>,
    state: StateView,
    valid_actions: &[String],
) -> (String, serde_json::Value) {
    // Build a params hint from the platform's raise bounds so the brain's
    // mapping layer can clamp requested sizes to server-accepted ranges.
    let mut hint_map = serde_json::Map::new();
    let mn = state.min_raise();
    let mx = state.max_raise();
    if mx > 0 {
        hint_map.insert("min".into(), serde_json::Value::Number(mn.into()));
        hint_map.insert("max".into(), serde_json::Value::Number(mx.into()));
    }
    let hint = serde_json::Value::Object(hint_map);
    match brain {
        Some(b) => {
            let b = b.clone();
            let va = valid_actions.to_vec();
            let (to_call, pot) = (state.to_call(), state.pot());
            let street = state.street_name();
            let result = tokio::task::spawn_blocking(move || {
                b.blocking_lock()
                    .decide_turn(to_call, pot, &street, &va, &hint)
            })
            .await;
            match result {
                Ok(pair) => pair,
                Err(e) => {
                    warn!("brain panicked on decision ({e}); falling back to trivial policy");
                    let d = strategy::decide(state.to_call(), state.pot(), valid_actions);
                    (d.action, d.params)
                }
            }
        }
        None => {
            let d = strategy::decide(state.to_call(), state.pot(), valid_actions);
            (d.action, d.params)
        }
    }
}

// ---------------------------------------------------------------------------
// End-to-end driver
// ---------------------------------------------------------------------------

/// Drive one full path: lobby → matched → play one match to `match_end`.
/// Returns the `match_end` payload (or `None` if the match WS closed without
/// one). Composes [`wait_for_matched`] + [`play_match`].
pub async fn run_once(
    base_url: &str,
    bot_id: &str,
    token: &str,
    brain: Option<Arc<Mutex<ChamBrain>>>,
) -> Result<Option<Frame>, Error> {
    let matched = wait_for_matched(base_url, bot_id, token).await?;
    let gw_path = matched
        .gateway_ws_url
        .clone()
        .ok_or_else(|| Error::Connection("matched frame lacks gateway_ws_url".into()))?;
    let gateway_url = resolve_gateway_url(base_url, &gw_path)?;
    let match_id = matched
        .match_id
        .clone()
        .ok_or_else(|| Error::Connection("matched frame lacks match_id".into()))?;
    play_match(&gateway_url, &match_id, token, brain).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalise_base_strips_path_and_downgrades_https() {
        assert_eq!(
            normalise_base("wss://example.com/ws/external").unwrap(),
            "wss://example.com"
        );
        assert_eq!(
            normalise_base("https://example.com/api").unwrap(),
            "wss://example.com"
        );
        assert_eq!(
            normalise_base("http://example.com").unwrap(),
            "ws://example.com"
        );
    }

    #[test]
    fn normalise_base_defaults_scheme_to_wss() {
        assert_eq!(normalise_base("example.com").unwrap(), "wss://example.com");
    }

    #[test]
    fn normalise_base_keeps_explicit_port() {
        assert_eq!(
            normalise_base("ws://localhost:8001").unwrap(),
            "ws://localhost:8001"
        );
    }

    #[test]
    fn lobby_ws_url_shape() {
        assert_eq!(
            lobby_ws_url("wss://staging.chipzen.ai", "abc-123").unwrap(),
            "wss://staging.chipzen.ai/ws/external/bot/abc-123"
        );
    }

    #[test]
    fn resolve_gateway_url_appends_path() {
        assert_eq!(
            resolve_gateway_url("wss://staging.chipzen.ai", "/ws/external/match/m1/p1").unwrap(),
            "wss://staging.chipzen.ai/ws/external/match/m1/p1"
        );
    }

    #[test]
    fn resolve_gateway_url_inserts_missing_leading_slash() {
        assert_eq!(
            resolve_gateway_url("wss://staging.chipzen.ai", "ws/external/match/m1/p1").unwrap(),
            "wss://staging.chipzen.ai/ws/external/match/m1/p1"
        );
    }

    #[test]
    fn resolve_gateway_url_passes_absolute_through() {
        assert_eq!(
            resolve_gateway_url("wss://staging.chipzen.ai", "wss://other.host/foo").unwrap(),
            "wss://other.host/foo"
        );
    }

    #[test]
    fn transport_security_rejects_remote_ws() {
        assert!(check_transport_security("ws://example.com").is_err());
        assert!(check_transport_security("wss://example.com").is_ok());
        assert!(check_transport_security("ws://localhost:8001").is_ok());
        assert!(check_transport_security("ws://127.0.0.1:8001").is_ok());
    }

    #[test]
    fn extract_action_name_tolerates_observed_shapes() {
        use serde_json::json;
        assert_eq!(
            extract_action_name(&json!({"action": "call"})),
            Some("call".to_string())
        );
        assert_eq!(
            extract_action_name(&json!({"action": {"name": "raise"}})),
            Some("raise".to_string())
        );
        assert_eq!(
            extract_action_name(&json!({"action": {"type": "bet"}})),
            Some("bet".to_string())
        );
        assert_eq!(
            extract_action_name(&json!({"action_name": "fold"})),
            Some("fold".to_string())
        );
        assert_eq!(extract_action_name(&json!({"foo": 1})), None);
    }
}
