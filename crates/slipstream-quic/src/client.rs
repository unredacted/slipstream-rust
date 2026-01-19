//! QUIC client implementation using tquic.

use crate::config::Config;
use crate::error::Error;
use crate::multipath::{PathEvent, PathId, PathInfo, PathManager, PathMode};
use bytes::Bytes;
use std::cell::RefCell;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::rc::Rc;
use tquic::{Connection, Endpoint, PacketInfo, PacketSendHandler, TransportHandler};

/// QUIC client for connecting to a server.
pub struct Client {
    config: Config,
}

impl Client {
    /// Create a new QUIC client with the given configuration.
    pub fn new(config: Config) -> Result<Self, Error> {
        Ok(Self { config })
    }

    /// Connect to a server at the given address.
    pub fn connect(
        &self,
        local_addr: SocketAddr,
        server_addr: SocketAddr,
        server_name: &str,
    ) -> Result<ClientConnection, Error> {
        let tquic_config = self.config.to_tquic_client_config()?;

        // Create the connection state
        let state = Rc::new(RefCell::new(ConnectionState::new()));

        // Create handler and sender
        let handler = Box::new(ClientHandler {
            state: state.clone(),
        });
        let sender = Rc::new(PacketSender::new());

        // Create tquic endpoint
        let mut endpoint = Endpoint::new(
            Box::new(tquic_config),
            false, // is_server = false for client
            handler,
            sender.clone(),
        );

        // Initiate connection (6 args: local, remote, server_name, session, token, config)
        let conn_id = endpoint
            .connect(local_addr, server_addr, Some(server_name), None, None, None)
            .map_err(|e| Error::Quic(e.to_string()))?;

        tracing::info!(
            "Connecting to {} ({}), conn_id={}",
            server_name,
            server_addr,
            conn_id
        );

        Ok(ClientConnection {
            endpoint,
            conn_id,
            state,
            sender,
            local_addr,
            server_addr,
        })
    }
}

/// Internal state shared between the handler and the connection.
struct ConnectionState {
    ready: bool,
    closing: bool,
    streams: HashMap<u64, StreamState>,
    path_events: Vec<PathEvent>,
}

impl ConnectionState {
    fn new() -> Self {
        Self {
            ready: false,
            closing: false,
            streams: HashMap::new(),
            path_events: Vec::new(),
        }
    }
}

struct StreamState {
    readable: bool,
    writable: bool,
    finished: bool,
}

/// Handler for tquic transport events.
struct ClientHandler {
    state: Rc<RefCell<ConnectionState>>,
}

impl TransportHandler for ClientHandler {
    fn on_conn_created(&mut self, _conn: &mut Connection) {
        tracing::debug!("Connection created");
    }

    fn on_conn_established(&mut self, _conn: &mut Connection) {
        tracing::info!("Connection established");
        self.state.borrow_mut().ready = true;
    }

    fn on_conn_closed(&mut self, _conn: &mut Connection) {
        tracing::info!("Connection closed");
        self.state.borrow_mut().closing = true;
    }

    fn on_stream_created(&mut self, _conn: &mut Connection, stream_id: u64) {
        tracing::debug!("Stream {} created", stream_id);
        self.state.borrow_mut().streams.insert(
            stream_id,
            StreamState {
                readable: false,
                writable: true,
                finished: false,
            },
        );
    }

    fn on_stream_readable(&mut self, _conn: &mut Connection, stream_id: u64) {
        tracing::trace!("Stream {} readable", stream_id);
        if let Some(stream) = self.state.borrow_mut().streams.get_mut(&stream_id) {
            stream.readable = true;
        }
    }

    fn on_stream_writable(&mut self, _conn: &mut Connection, stream_id: u64) {
        tracing::trace!("Stream {} writable", stream_id);
        if let Some(stream) = self.state.borrow_mut().streams.get_mut(&stream_id) {
            stream.writable = true;
        }
    }

    fn on_stream_closed(&mut self, _conn: &mut Connection, stream_id: u64) {
        tracing::debug!("Stream {} closed", stream_id);
        if let Some(stream) = self.state.borrow_mut().streams.get_mut(&stream_id) {
            stream.finished = true;
        }
    }

    fn on_new_token(&mut self, _conn: &mut Connection, _token: Vec<u8>) {
        // Token management for 0-RTT
    }
}

/// Packet sender for tquic.
struct PacketSender {
    pending_packets: RefCell<Vec<(Vec<u8>, PacketInfo)>>,
}

impl PacketSender {
    fn new() -> Self {
        Self {
            pending_packets: RefCell::new(Vec::new()),
        }
    }

    fn take_packets(&self) -> Vec<(Vec<u8>, PacketInfo)> {
        std::mem::take(&mut *self.pending_packets.borrow_mut())
    }
}

impl PacketSendHandler for PacketSender {
    fn on_packets_send(&self, pkts: &[(Vec<u8>, PacketInfo)]) -> tquic::Result<usize> {
        let mut pending = self.pending_packets.borrow_mut();
        for (data, info) in pkts {
            pending.push((data.clone(), *info));
        }
        Ok(pkts.len())
    }
}

/// An established QUIC client connection.
pub struct ClientConnection {
    endpoint: Endpoint,
    conn_id: u64,
    state: Rc<RefCell<ConnectionState>>,
    sender: Rc<PacketSender>,
    local_addr: SocketAddr,
    server_addr: SocketAddr,
}

impl ClientConnection {
    /// Check if the connection is ready (handshake complete).
    pub fn is_ready(&self) -> bool {
        self.state.borrow().ready
    }

    /// Check if the connection is closing.
    pub fn is_closing(&self) -> bool {
        self.state.borrow().closing
    }

    /// Process incoming packet data.
    pub fn recv(&mut self, data: &[u8], from: SocketAddr) -> Result<(), Error> {
        let info = PacketInfo {
            src: from,
            dst: self.local_addr,
            time: std::time::Instant::now(),
        };
        // tquic recv takes &mut [u8], so we need to copy
        let mut buf = data.to_vec();
        self.endpoint
            .recv(&mut buf, &info)
            .map_err(|e| Error::Quic(e.to_string()))?;
        let _ = self.endpoint.process_connections();
        Ok(())
    }

    /// Get packets to send.
    pub fn poll_send(&mut self) -> Vec<(Vec<u8>, SocketAddr)> {
        let _ = self.endpoint.process_connections();
        self.sender
            .take_packets()
            .into_iter()
            .map(|(data, info)| (data, info.dst))
            .collect()
    }

    /// Get the next timeout.
    pub fn timeout(&self) -> Option<std::time::Duration> {
        self.endpoint.timeout()
    }

    /// Handle timeout.
    pub fn on_timeout(&mut self) {
        self.endpoint.on_timeout(std::time::Instant::now());
        let _ = self.endpoint.process_connections();
    }

    /// Open a new bidirectional stream.
    pub fn open_bi(&mut self) -> Result<u64, Error> {
        if let Some(conn) = self.endpoint.conn_get_mut(self.conn_id) {
            // stream_bidi_new(priority, urgency)
            let stream_id = conn
                .stream_bidi_new(0, false)
                .map_err(|e| Error::Stream(e.to_string()))?;
            self.state.borrow_mut().streams.insert(
                stream_id,
                StreamState {
                    readable: false,
                    writable: true,
                    finished: false,
                },
            );
            Ok(stream_id)
        } else {
            Err(Error::ConnectionClosed {
                reason: "connection not found".to_string(),
            })
        }
    }

    /// Write data to a stream.
    pub fn stream_write(&mut self, stream_id: u64, data: &[u8], fin: bool) -> Result<usize, Error> {
        // Process connections first to update flow control state
        let _ = self.endpoint.process_connections();
        if let Some(conn) = self.endpoint.conn_get_mut(self.conn_id) {
            conn.stream_write(stream_id, Bytes::copy_from_slice(data), fin)
                .map_err(|e| Error::Stream(e.to_string()))
        } else {
            Err(Error::ConnectionClosed {
                reason: "connection not found".to_string(),
            })
        }
    }

    /// Read data from a stream.
    pub fn stream_read(&mut self, stream_id: u64, buf: &mut [u8]) -> Result<(usize, bool), Error> {
        if let Some(conn) = self.endpoint.conn_get_mut(self.conn_id) {
            conn.stream_read(stream_id, buf)
                .map_err(|e| Error::Stream(e.to_string()))
        } else {
            Err(Error::ConnectionClosed {
                reason: "connection not found".to_string(),
            })
        }
    }

    /// Get stream IDs that have readable data.
    pub fn readable_streams(&self) -> Vec<u64> {
        self.state
            .borrow()
            .streams
            .iter()
            .filter(|(_, s)| s.readable)
            .map(|(id, _)| *id)
            .collect()
    }

    /// Get stream write capacity (available flow control credits).
    pub fn stream_capacity(&mut self, stream_id: u64) -> usize {
        if let Some(conn) = self.endpoint.conn_get_mut(self.conn_id) {
            conn.stream_capacity(stream_id).unwrap_or(0)
        } else {
            0
        }
    }

    /// Drain path events.
    pub fn drain_path_events(&mut self) -> Vec<PathEvent> {
        std::mem::take(&mut self.state.borrow_mut().path_events)
    }

    /// Close the connection.
    pub fn close(&mut self, error_code: u64, reason: &str) -> Result<(), Error> {
        if let Some(conn) = self.endpoint.conn_get_mut(self.conn_id) {
            conn.close(true, error_code, reason.as_bytes())
                .map_err(|e| Error::Quic(e.to_string()))?;
        }
        Ok(())
    }

    /// Get the current RTT estimate in microseconds.
    pub fn rtt(&mut self) -> u64 {
        // TODO: Implement proper stats access for tquic
        // ConnectionStats fields differ from expected
        0
    }

    /// Get the current congestion window.
    pub fn cwnd(&mut self) -> u64 {
        // TODO: Implement proper stats access for tquic
        0
    }
}

impl PathManager for ClientConnection {
    fn probe_path(&mut self, peer_addr: SocketAddr) -> Result<PathId, Error> {
        if let Some(conn) = self.endpoint.conn_get_mut(self.conn_id) {
            conn.add_path(self.local_addr, peer_addr)
                .map_err(|e| Error::Path(e.to_string()))
        } else {
            Err(Error::ConnectionClosed {
                reason: "connection not found".to_string(),
            })
        }
    }

    fn path_info(&mut self, path_id: PathId) -> Option<PathInfo> {
        Some(PathInfo {
            path_id,
            local_addr: self.local_addr,
            peer_addr: self.server_addr,
            rtt_us: self.rtt(),
            cwnd: self.cwnd(),
            pacing_rate: 0,
            bytes_in_flight: 0,
            is_active: true,
        })
    }

    fn active_paths(&mut self) -> Vec<PathInfo> {
        vec![]
    }

    fn set_path_mode(&mut self, _path_id: PathId, _mode: PathMode) -> Result<(), Error> {
        Ok(())
    }

    fn drain_path_events(&mut self) -> Vec<PathEvent> {
        ClientConnection::drain_path_events(self)
    }
}
