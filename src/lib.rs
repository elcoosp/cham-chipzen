//! Chipzen External-API bot — Rust port of the reference client at
//! `chipzen-sdk/examples/external-api-bot` (`client.py` / `run.py` / `strategy.py`).
//!
//! Flow: lobby WS → `authenticate` → wait for `matched` → per-match gateway WS
//! (token in `Sec-WebSocket-Protocol`, CZ issue 2932) → Layer-1 handshake →
//! game loop until `match_end`. The poker brain is the CHAMELEON
//! [`ChameleonAgent`](cham_agent::pipeline::ChameleonAgent); when artifacts are
//! unavailable the bot degrades to the trivial check/call/fold policy of the
//! Python reference so the protocol path is always exercisable.

pub mod agent;
pub mod client;
pub mod error;
pub mod proto;
pub mod strategy;

pub use error::Error;

/// Re-export so downstream drivers can embed `play_match` without naming it.
pub use client::{handle_match_message, play_match, resolve_gateway_url, run_once, wait_for_matched};
