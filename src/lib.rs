//! Chipzen External-API bot — Rust port of the reference client at
//! `chipzen-sdk/examples/external-api-bot` (`client.py` / `run.py` /
//! `strategy.py`).
//!
//! # What this crate does
//!
//! It speaks the Chipzen External-API WebSocket protocol end-to-end:
//!
//! ```text
//!   lobby WS  →  authenticate → hello → (wait) → matched
//!                                                   │
//!                          resolve gateway_ws_url  │
//!                                                   ▼
//!   match WS  →  authenticate → server hello → client hello
//!                                                   │
//!                                                   ▼
//!                        turn_request ⇄ turn_action · match_end
//! ```
//!
//! The bot's poker brain is the CHAMELEON
//! [`ChameleonAgent`](https://github.com/elcoosp/chameleon) — see
//! [`agent::ChamBrain`] for how the wire protocol is adapted into a shadow
//! engine state that the agent reasons over. When artifacts are unavailable,
//! the bot degrades to the trivial check/call/fold policy of the Python
//! reference ([`strategy::decide`]) so the protocol path is always
//! exercisable.
//!
//! # Examples
//!
//! Parse an inbound frame and inspect its type:
//!
//! ```
//! use cham_chipzen::parse_frame;
//!
//! let frame = parse_frame(r#"{"type":"opponent_action","action":"raise"}"#).unwrap();
//! assert_eq!(frame.r#type, "opponent_action");
//! ```
//!
//! Serialize a `turn_action` reply with a raise size:
//!
//! ```
//! use cham_chipzen::{parse_frame, OutFrame};
//! use serde_json::json;
//!
//! let request_id = json!(7);
//! let params = json!({"to": 500});
//! let text = OutFrame::TurnAction {
//!     match_id: "m-1",
//!     request_id: &request_id,
//!     action: "raise",
//!     params: &params,
//! }
//! .to_json()
//! .unwrap();
//!
//! let frame = parse_frame(&text).unwrap();
//! assert_eq!(frame.r#type, "turn_action");
//! assert_eq!(frame.request_id.as_ref().and_then(|v| v.as_i64()), Some(7));
//! ```
//!
//! Compute the trivial reference policy for a turn:
//!
//! ```
//! use cham_chipzen::decide;
//!
//! let legal = vec!["fold".to_string(), "check".to_string()];
//! // Free to check → check.
//! assert_eq!(decide(0, 100, &legal).action, "check");
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![warn(rust_2018_idioms)]
#![warn(unreachable_pub)]

pub mod agent;
pub mod client;
pub mod error;
pub mod proto;
pub mod strategy;

pub use error::Error;

/// Re-export so downstream drivers can embed [`play_match`] without naming it.
pub use client::{
    handle_match_message, play_match, resolve_gateway_url, run_once, wait_for_matched,
};

/// Re-export the wire types so downstream tests can construct/parse frames
/// without depending on the module paths directly.
pub use proto::{
    bot_token_subprotocols, parse_frame, Frame, OutFrame, StateView, BOT_TOKEN_SUBPROTOCOL,
    CLIENT_NAME, CLIENT_VERSION, PROTOCOL_VERSIONS,
};

/// Re-export the strategy surface so downstream consumers can compare their
/// own policies against the reference policy the bot degrades to.
pub use strategy::{decide, rejection_fallback, Decision};
