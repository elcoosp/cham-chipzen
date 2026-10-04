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
    #[serde(default)]
    pub min_raise: Value,
    #[serde(default)]
    pub max_raise: Value,
    /// Public board cards (e.g. `["Ah","Kd","7c"]`); empty preflop. Tolerated
    /// either as an array of strings or as an array of `{rank,suit}` objects.
    #[serde(default)]
    pub board: Value,
    /// Street name (`"preflop"|"flop"|"turn"|"river"`) or a numeric index.
    /// Absent → treated as `"preflop"` for backward compatibility.
    #[serde(default)]
    pub street: Value,
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
    pub fn min_raise(&self) -> i64 {
        Self::as_int(&self.min_raise)
    }
    pub fn max_raise(&self) -> i64 {
        Self::as_int(&self.max_raise)
    }

    /// Board cards as plain rank+suit strings; empty if absent or an
    /// unrecognised shape. Tolerates both `["Ah","Kd"]` and
    /// `[{"rank":"A","suit":"h"}, ...]`.
    pub fn board_cards(&self) -> Vec<String> {
        match &self.board {
            Value::Array(items) => items
                .iter()
                .filter_map(|v| {
                    if let Some(s) = v.as_str() {
                        return Some(s.to_string());
                    }
                    let r = v.get("rank").and_then(|x| x.as_str())?;
                    let s = v.get("suit").and_then(|x| x.as_str())?;
                    Some(format!("{r}{s}"))
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Street name, normalising numeric indices. Defaults to `"preflop"` when
    /// absent or unrecognised (backward-compatible with older servers that
    /// did not send a `street` field).
    pub fn street_name(&self) -> String {
        match &self.street {
            Value::String(s) => s.to_lowercase(),
            Value::Number(n) => match n.as_i64() {
                Some(0) => "preflop".into(),
                Some(1) => "flop".into(),
                Some(2) => "turn".into(),
                Some(3) => "river".into(),
                _ => "preflop".into(),
            },
            _ => "preflop".into(),
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn as_int_handles_number_and_string() {
        assert_eq!(StateView::as_int(&json!(42)), 42);
        assert_eq!(StateView::as_int(&json!("42")), 42);
    }

    #[test]
    fn as_int_defaults_to_zero_on_bad_input() {
        assert_eq!(StateView::as_int(&json!(null)), 0);
        assert_eq!(StateView::as_int(&json!("oops")), 0);
        assert_eq!(StateView::as_int(&json!([1, 2])), 0);
    }

    #[test]
    fn state_view_default_is_zero() {
        let s = StateView::default();
        assert_eq!(s.to_call(), 0);
        assert_eq!(s.pot(), 0);
        assert_eq!(s.min_raise(), 0);
        assert_eq!(s.max_raise(), 0);
    }

    #[test]
    fn state_view_parses_raise_bounds() {
        let v: StateView =
            serde_json::from_value(json!({"min_raise": 200, "max_raise": "5000"})).unwrap();
        assert_eq!(v.min_raise(), 200);
        assert_eq!(v.max_raise(), 5000);
    }

    #[test]
    fn state_view_defaults_street_to_preflop() {
        let v = StateView::default();
        assert_eq!(v.street_name(), "preflop");
        assert!(v.board_cards().is_empty());
    }

    #[test]
    fn state_view_parses_street_from_name_or_index() {
        let v: StateView = serde_json::from_value(json!({"street": "Flop"})).unwrap();
        assert_eq!(v.street_name(), "flop");
        let v: StateView = serde_json::from_value(json!({"street": 2})).unwrap();
        assert_eq!(v.street_name(), "turn");
        let v: StateView = serde_json::from_value(json!({"street": 99})).unwrap();
        assert_eq!(v.street_name(), "preflop");
    }

    #[test]
    fn state_view_parses_board_strings_and_objects() {
        let v: StateView =
            serde_json::from_value(json!({"board": ["Ah", "Kd", "7c"]})).unwrap();
        assert_eq!(v.board_cards(), vec!["Ah".to_string(), "Kd".to_string(), "7c".to_string()]);
        let v: StateView = serde_json::from_value(json!({
            "board": [{"rank": "A", "suit": "h"}, {"rank": "K", "suit": "d"}]
        }))
        .unwrap();
        assert_eq!(v.board_cards(), vec!["Ah".to_string(), "Kd".to_string()]);
        let v: StateView = serde_json::from_value(json!({"board": "not-an-array"})).unwrap();
        assert!(v.board_cards().is_empty());
    }

    #[test]
    fn parse_frame_rejects_non_objects() {
        assert!(parse_frame("null").is_none());
        assert!(parse_frame("[]").is_none());
        assert!(parse_frame("garbage").is_none());
    }

    #[test]
    fn parse_frame_populates_raw_and_type() {
        let f = parse_frame(r#"{"type":"hello","extra":42}"#).unwrap();
        assert_eq!(f.r#type, "hello");
        assert_eq!(f.raw.get("extra").and_then(|v| v.as_i64()), Some(42));
    }

    #[test]
    fn bot_token_subprotocols_shape() {
        assert_eq!(
            bot_token_subprotocols("cz_extbot_abc"),
            vec!["chipzen-bot-token".to_string(), "cz_extbot_abc".to_string()]
        );
    }

    #[test]
    fn outframe_authenticate_lobby_omits_match_id() {
        let v: serde_json::Value =
            serde_json::from_str(&OutFrame::AuthenticateLobby { token: "tok" }.to_json()).unwrap();
        assert_eq!(v["type"], "authenticate");
        assert_eq!(v["token"], "tok");
        assert!(v.get("match_id").is_none());
    }

    #[test]
    fn outframe_authenticate_match_sends_empty_token() {
        let v: serde_json::Value =
            serde_json::from_str(&OutFrame::AuthenticateMatch { match_id: "m1" }.to_json()).unwrap();
        assert_eq!(v["type"], "authenticate");
        assert_eq!(v["token"], "");
        assert_eq!(v["match_id"], "m1");
    }

    #[test]
    fn outframe_client_hello_carries_protocol_version() {
        let v: serde_json::Value =
            serde_json::from_str(&OutFrame::ClientHello { match_id: "m1" }.to_json()).unwrap();
        assert_eq!(v["type"], "hello");
        assert_eq!(v["match_id"], "m1");
        assert_eq!(v["supported_versions"][0], "1.0");
        assert_eq!(v["client_name"], CLIENT_NAME);
        assert_eq!(v["client_version"], CLIENT_VERSION);
    }

    #[test]
    fn outframe_pong_lobby_has_no_match_id() {
        let v: serde_json::Value =
            serde_json::from_str(&OutFrame::PongLobby.to_json()).unwrap();
        assert_eq!(v["type"], "pong");
        assert!(v.get("match_id").is_none());
    }

    #[test]
    fn outframe_pong_match_carries_match_id() {
        let v: serde_json::Value =
            serde_json::from_str(&OutFrame::PongMatch { match_id: "m1" }.to_json()).unwrap();
        assert_eq!(v["type"], "pong");
        assert_eq!(v["match_id"], "m1");
    }

    #[test]
    fn outframe_turn_action_echoes_request_id() {
        let rid = json!(7);
        let params = json!({"to": 500});
        let v: serde_json::Value = serde_json::from_str(
            &OutFrame::TurnAction {
                match_id: "m1",
                request_id: &rid,
                action: "raise",
                params: &params,
            }
            .to_json(),
        )
        .unwrap();
        assert_eq!(v["type"], "turn_action");
        assert_eq!(v["match_id"], "m1");
        assert_eq!(v["request_id"], 7);
        assert_eq!(v["action"], "raise");
        assert_eq!(v["params"]["to"], 500);
    }
}
