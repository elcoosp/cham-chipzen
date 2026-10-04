//! Wire-format frame types for the Chipzen External-API protocol.
//!
//! Mirrors `docs/EXTERNAL-API-BOT-PROTOCOL.md` §3–§6 as observed in the
//! reference client: every frame is a JSON object with a string `type` tag;
//! unknown fields are ignored (forward compat) and unknown `type`s are skipped
//! by the handlers, exactly like the Python `_loads` + if-chain.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Protocol version this client speaks (Layer-1 negotiation).
pub const PROTOCOL_VERSIONS: [&str; 1] = ["1.0"];
pub const CLIENT_NAME: &str = "chipzen-extapi-rust";
pub const CLIENT_VERSION: &str = "0.1.0";

/// Sentinel subprotocol marking the `cz_extbot_` token inside the
/// `Sec-WebSocket-Protocol` offer (CZ issue 2932 — the token must never appear
/// on a URL/query string, or it leaks into proxy access logs).
pub const BOT_TOKEN_SUBPROTOCOL: &str = "chipzen-bot-token";

/// Build the `Sec-WebSocket-Protocol` offer carrying the bot token:
/// `[sentinel, token]`. The api gateway extracts the token from this header
/// and echoes the sentinel back on accept.
pub fn bot_token_subprotocols(token: &str) -> Vec<String> {
    vec![BOT_TOKEN_SUBPROTOCOL.to_string(), token.to_string()]
}

/// One decoded inbound WS frame (any JSON object tagged with `type`).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Frame {
    /// Full original JSON object — lets handlers probe optional/unknown fields
    /// (e.g. villain action notifications) without a schema break.
    #[serde(default)]
    pub raw: Value,
    #[serde(default)]
    pub r#type: String,
    /// Lobby `hello`: which endpoint we landed on ("lobby").
    #[serde(default)]
    pub endpoint: Option<String>,
    // ---- lobby `matched` notify ----
    #[serde(default)]
    pub match_id: Option<String>,
    #[serde(default)]
    pub participant_id: Option<String>,
    #[serde(default)]
    pub gateway_ws_url: Option<String>,
    #[serde(default)]
    pub rated: Option<bool>,
    // ---- match server hello ----
    #[serde(default)]
    pub selected_version: Option<String>,
    #[serde(default)]
    pub game_type: Option<String>,
    // ---- turn_request (Layer-2) ----
    #[serde(default)]
    pub request_id: Option<Value>,
    #[serde(default)]
    pub state: Option<StateView>,
    #[serde(default)]
    pub valid_actions: Vec<String>,
    // ---- action_rejected / error ----
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    // ---- match_end ----
    /// Raw results array kept as JSON so we can echo it verbatim to stdout.
    #[serde(default)]
    pub results: Option<Value>,
}

/// The subset of a Layer-2 `turn_request.state` the strategy reads. All fields
/// tolerant: absent → 0, matching the Python `_as_int` defensive coercion.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct StateView {
    #[serde(default)]
    pub to_call: Value,
    #[serde(default)]
    pub pot: Value,
}

impl StateView {
    /// Coerce a possibly-missing / possibly-stringy numeric field to i64 (0 on
    /// failure) — port of Python `_as_int`.
    pub fn as_int(v: &Value) -> i64 {
        match v {
            Value::Number(n) => n.as_i64().unwrap_or(0),
            Value::String(s) => s.parse::<i64>().unwrap_or(0),
            _ => 0,
        }
    }

    pub fn to_call(&self) -> i64 {
        Self::as_int(&self.to_call)
    }
    pub fn pot(&self) -> i64 {
        Self::as_int(&self.pot)
    }
}

/// Outbound `authenticate` frame (lobby leg carries the real token; the match
/// leg sends an empty one — the gateway's internal JWT is authoritative there,
/// but the frame must be first or the handshake stalls).
#[derive(Serialize)]
struct AuthenticateOut<'a> {
    r#type: &'a str,
    token: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    match_id: Option<&'a str>,
}

/// Outbound client `hello` (match leg, Layer-1 negotiation).
#[derive(Serialize)]
struct HelloOut<'a> {
    r#type: &'a str,
    match_id: &'a str,
    supported_versions: &'a [&'a str],
    client_name: &'a str,
    client_version: &'a str,
}

/// Outbound `pong` (lobby variant has no match_id).
#[derive(Serialize)]
struct PongOut<'a> {
    r#type: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    match_id: Option<&'a str>,
}

/// Outbound `turn_action`; `request_id` MUST echo the turn_request's value.
#[derive(Serialize)]
struct TurnActionOut<'a> {
    r#type: &'a str,
    match_id: &'a str,
    request_id: &'a Value,
    action: &'a str,
    params: &'a Value,
}

pub enum OutFrame<'a> {
    AuthenticateLobby { token: &'a str },
    AuthenticateMatch { match_id: &'a str },
    ClientHello { match_id: &'a str },
    PongLobby,
    PongMatch { match_id: &'a str },
    TurnAction {
        match_id: &'a str,
        request_id: &'a Value,
        action: &'a str,
        params: &'a Value,
    },
}

impl OutFrame<'_> {
    /// Serialize to the JSON text sent over the socket.
    pub fn to_json(&self) -> String {
        let v = match self {
            OutFrame::AuthenticateLobby { token } => serde_json::to_value(AuthenticateOut {
                r#type: "authenticate",
                token,
                match_id: None,
            }),
            OutFrame::AuthenticateMatch { match_id } => {
                serde_json::to_value(AuthenticateOut {
                    r#type: "authenticate",
                    token: "",
                    match_id: Some(match_id),
                })
            }
            OutFrame::ClientHello { match_id } => serde_json::to_value(HelloOut {
                r#type: "hello",
                match_id,
                supported_versions: &PROTOCOL_VERSIONS,
                client_name: CLIENT_NAME,
                client_version: CLIENT_VERSION,
            }),
            OutFrame::PongLobby => serde_json::to_value(PongOut {
                r#type: "pong",
                match_id: None,
            }),
            OutFrame::PongMatch { match_id } => serde_json::to_value(PongOut {
                r#type: "pong",
                match_id: Some(match_id),
            }),
            OutFrame::TurnAction {
                match_id,
                request_id,
                action,
                params,
            } => serde_json::to_value(TurnActionOut {
                r#type: "turn_action",
                match_id,
                request_id,
                action,
                params,
            }),
        };
        // Serializing these plain structs is infallible.
        serde_json::to_string(&v.expect("out-frame serialization")).expect("json")
    }
}

/// Parse a WS text frame into a `Frame`; malformed/non-object frames yield
/// `None` (the reference client treats them as `{}` and ignores them).
pub fn parse_frame(raw: &str) -> Option<Frame> {
    serde_json::from_str::<Value>(raw)
        .ok()
        .filter(|v| v.is_object())
        .and_then(|v| {
            let mut f = serde_json::from_value::<Frame>(v.clone()).ok()?;
            f.raw = v;
            Some(f)
        })
}
