//! Error types for slipstream-quic.

use thiserror::Error;

/// Errors that can occur in slipstream-quic operations.
#[derive(Debug, Error)]
pub enum Error {
    /// QUIC transport error.
    #[error("QUIC error: {0}")]
    Quic(String),

    /// TLS/crypto error.
    #[error("TLS error: {0}")]
    Tls(String),

    /// Connection closed.
    #[error("connection closed: {reason}")]
    ConnectionClosed { reason: String },

    /// Stream error.
    #[error("stream error: {0}")]
    Stream(String),

    /// IO error.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Configuration error.
    #[error("config error: {0}")]
    Config(String),

    /// Path/multipath error.
    #[error("path error: {0}")]
    Path(String),
}

impl From<tquic::Error> for Error {
    fn from(err: tquic::Error) -> Self {
        Error::Quic(err.to_string())
    }
}
