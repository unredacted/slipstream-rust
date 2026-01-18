//! QUIC client implementation.

use crate::config::Config;
use crate::error::Error;
use crate::multipath::{PathEvent, PathId, PathInfo, PathManager, PathMode};
use crate::stream::{BiStream, RecvStream, SendStream};
use std::net::SocketAddr;
use std::sync::Arc;

/// QUIC client for connecting to a server.
pub struct Client {
    config: Config,
    // tquic endpoint will be stored here
}

impl Client {
    /// Create a new QUIC client with the given configuration.
    pub fn new(config: Config) -> Result<Self, Error> {
        Ok(Self { config })
    }

    /// Connect to a server at the given address.
    pub async fn connect(
        &self,
        server_addr: SocketAddr,
        server_name: &str,
    ) -> Result<Connection, Error> {
        // TODO: Implement with tquic
        // 1. Create tquic endpoint
        // 2. Configure multipath, CC, TLS
        // 3. Connect to server
        tracing::info!("Connecting to {} ({})", server_name, server_addr);
        Err(Error::Config("not yet implemented".to_string()))
    }
}

/// An established QUIC connection.
pub struct Connection {
    // tquic connection handle
    connection_id: u64,
}

impl Connection {
    /// Open a new bidirectional stream.
    pub async fn open_bi(&mut self) -> Result<BiStream, Error> {
        // TODO: Implement with tquic
        let stream_id = 0; // Will be assigned by tquic
        Ok(BiStream::new(stream_id))
    }

    /// Open a new unidirectional send stream.
    pub async fn open_uni(&mut self) -> Result<SendStream, Error> {
        // TODO: Implement with tquic
        let stream_id = 0;
        Ok(SendStream::new(stream_id))
    }

    /// Accept an incoming bidirectional stream.
    pub async fn accept_bi(&mut self) -> Result<Option<BiStream>, Error> {
        // TODO: Implement with tquic
        Ok(None)
    }

    /// Accept an incoming unidirectional receive stream.
    pub async fn accept_uni(&mut self) -> Result<Option<RecvStream>, Error> {
        // TODO: Implement with tquic
        Ok(None)
    }

    /// Close the connection.
    pub async fn close(&mut self, error_code: u64, reason: &str) -> Result<(), Error> {
        tracing::info!("Closing connection: {} (code {})", reason, error_code);
        // TODO: Implement with tquic
        Ok(())
    }

    /// Check if the connection is still open.
    pub fn is_alive(&self) -> bool {
        // TODO: Implement with tquic
        true
    }

    /// Get the current RTT estimate in microseconds.
    pub fn rtt(&self) -> u64 {
        // TODO: Implement with tquic
        0
    }

    /// Get the current pacing rate in bytes per second.
    pub fn pacing_rate(&self) -> u64 {
        // TODO: Implement with tquic
        0
    }
}

impl PathManager for Connection {
    fn probe_path(&mut self, peer_addr: SocketAddr) -> Result<PathId, Error> {
        tracing::debug!("Probing new path to {}", peer_addr);
        // TODO: Implement with tquic multipath API
        Err(Error::Path("multipath not yet implemented".to_string()))
    }

    fn path_info(&self, path_id: PathId) -> Option<PathInfo> {
        // TODO: Implement with tquic
        None
    }

    fn active_paths(&self) -> Vec<PathInfo> {
        // TODO: Implement with tquic
        Vec::new()
    }

    fn set_path_mode(&mut self, path_id: PathId, mode: PathMode) -> Result<(), Error> {
        tracing::debug!("Setting path {} mode to {:?}", path_id, mode);
        // TODO: Implement with tquic
        Ok(())
    }

    fn drain_path_events(&mut self) -> Vec<PathEvent> {
        // TODO: Implement with tquic
        Vec::new()
    }
}
