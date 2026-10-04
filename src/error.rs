//! Error type for the Chipzen client.
//!
//! Errors are deliberately coarse-grained: they distinguish "the network is
//! broken" ([`Error::Ws`], [`Error::Io`]) from "the peer sent us something the
//! protocol does not allow" ([`Error::Protocol`], [`Error::MissingField`])
//! from "we could not serialize our own reply" ([`Error::Serialization`]).
//! That split lets `main.rs`'s `--loop` retry only on transport errors and
//! surface protocol violations as fatal, rather than retrying into an
//! infinite bad-handshake loop.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    /// Transport-level failure from the WebSocket stack.
    #[error("websocket: {0}")]
    Ws(#[from] tokio_tungstenite::tungstenite::Error),

    /// A connection could not be established, or was closed unexpectedly
    /// during a step that requires a live socket.
    #[error("connection error: {0}")]
    Connection(String),

    /// Local I/O failure outside the WebSocket stack.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// A required field was absent from an inbound frame. The field name is
    /// the wire name, not the Rust field name, so log lines are diagnosable.
    #[error("protocol: missing required field `{0}`")]
    MissingField(&'static str),

    /// The peer sent a frame whose `type` is not allowed at this point in the
    /// handshake or game loop. Distinct from [`Error::MissingField`] because
    /// the remedy differs (usually: upgrade the client).
    #[error("protocol violation: {0}")]
    Protocol(String),

    /// Serialization of an outbound frame failed. In practice this is
    /// infallible for the fixed shapes we send; surfaced as a `Result` rather
    /// than a panic so a future shape change (e.g. a `NaN` float slipping into
    /// `params`) fails gracefully.
    #[error("serialization: {0}")]
    Serialization(String),
}
