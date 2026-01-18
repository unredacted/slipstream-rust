//! QUIC transport layer for Slipstream using tquic.
//!
//! This crate wraps tquic to provide QUIC transport with multipath support
//! for the Slipstream DNS tunnel.

pub mod client;
pub mod config;
pub mod error;
pub mod multipath;
pub mod server;
pub mod stream;

pub use client::Client;
pub use config::Config;
pub use error::Error;
pub use server::Server;
pub use stream::{RecvStream, SendStream};

/// Result type for slipstream-quic operations.
pub type Result<T> = std::result::Result<T, Error>;
