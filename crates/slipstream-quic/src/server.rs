//! QUIC server implementation.

use crate::config::Config;
use crate::error::Error;
use crate::stream::{BiStream, RecvStream, SendStream};
use std::net::SocketAddr;
use std::sync::Arc;

/// QUIC server for accepting connections.
pub struct Server {
    config: Config,
    local_addr: SocketAddr,
    // tquic endpoint will be stored here
}

impl Server {
    /// Create a new QUIC server bound to the given address.
    pub async fn bind(addr: SocketAddr, config: Config) -> Result<Self, Error> {
        // TODO: Implement with tquic
        // 1. Create tquic server endpoint
        // 2. Configure TLS with cert/key
        // 3. Enable multipath
        tracing::info!("Server binding to {}", addr);

        if config.cert_path.is_none() || config.key_path.is_none() {
            return Err(Error::Config(
                "server requires cert_path and key_path".to_string(),
            ));
        }

        Ok(Self {
            config,
            local_addr: addr,
        })
    }

    /// Get the local address the server is bound to.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Accept an incoming connection.
    pub async fn accept(&mut self) -> Result<Option<ServerConnection>, Error> {
        // TODO: Implement with tquic
        Ok(None)
    }

    /// Process incoming packets (for manual polling).
    pub fn process_incoming(&mut self, packet: &[u8], from: SocketAddr) -> Result<(), Error> {
        // TODO: Implement with tquic
        Ok(())
    }

    /// Get the next packet to send (for custom transport).
    pub fn poll_send(&mut self) -> Option<(Vec<u8>, SocketAddr)> {
        // TODO: Implement with tquic
        None
    }
}

/// A server-side QUIC connection.
pub struct ServerConnection {
    connection_id: u64,
    peer_addr: SocketAddr,
}

impl ServerConnection {
    /// Get the peer's address.
    pub fn peer_addr(&self) -> SocketAddr {
        self.peer_addr
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

    /// Open a new bidirectional stream.
    pub async fn open_bi(&mut self) -> Result<BiStream, Error> {
        // TODO: Implement with tquic
        let stream_id = 0;
        Ok(BiStream::new(stream_id))
    }

    /// Open a new unidirectional send stream.
    pub async fn open_uni(&mut self) -> Result<SendStream, Error> {
        // TODO: Implement with tquic
        let stream_id = 0;
        Ok(SendStream::new(stream_id))
    }

    /// Close the connection.
    pub async fn close(&mut self, error_code: u64, reason: &str) -> Result<(), Error> {
        tracing::info!("Closing server connection: {} (code {})", reason, error_code);
        // TODO: Implement with tquic
        Ok(())
    }

    /// Check if the connection is still open.
    pub fn is_alive(&self) -> bool {
        // TODO: Implement with tquic
        true
    }
}
