//! Error type for the Chipzen client.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("websocket: {0}")]
    Ws(#[from] tokio_tungstenite::tungstenite::Error),

    #[error("connection error: {0}")]
    Connection(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
