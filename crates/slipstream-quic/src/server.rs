//! QUIC server implementation using tquic.

use crate::config::Config;
use crate::error::Error;
use bytes::Bytes;
use std::cell::RefCell;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::rc::Rc;
use tquic::{Connection, Endpoint, PacketInfo, PacketSendHandler, TransportHandler};

/// QUIC server for accepting connections.
pub struct Server {
    endpoint: Endpoint,
    sender: Rc<PacketSender>,
    local_addr: SocketAddr,
    state: Rc<RefCell<ServerState>>,
}

struct ServerState {
    connections: HashMap<u64, ConnectionInfo>,
}

#[allow(dead_code)]
struct ConnectionInfo {
    peer_addr: SocketAddr,
    ready: bool,
    streams: HashMap<u64, StreamState>,
}

struct StreamState {
    readable: bool,
    writable: bool,
}

impl Server {
    /// Create a new QUIC server bound to the given address.
    pub fn new(addr: SocketAddr, config: Config) -> Result<Self, Error> {
        if config.cert_path.is_none() || config.key_path.is_none() {
            return Err(Error::Config(
                "server requires cert_path and key_path".to_string(),
            ));
        }

        let tquic_config = config.to_tquic_server_config()?;
        let state = Rc::new(RefCell::new(ServerState {
            connections: HashMap::new(),
        }));

        let handler = Box::new(ServerHandler {
            state: state.clone(),
        });
        let sender = Rc::new(PacketSender::new());

        let endpoint = Endpoint::new(
            Box::new(tquic_config),
            true, // is_server = true
            handler,
            sender.clone(),
        );

        tracing::info!("Server created for {}", addr);

        Ok(Self {
            endpoint,
            sender,
            local_addr: addr,
            state,
        })
    }

    /// Get the local address the server is bound to.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Process incoming packet data.
    pub fn recv(&mut self, data: &[u8], from: SocketAddr) -> Result<(), Error> {
        let info = PacketInfo {
            src: from,
            dst: self.local_addr,
            time: std::time::Instant::now(),
        };
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

    /// Get ready connections.
    pub fn ready_connections(&self) -> Vec<u64> {
        self.state
            .borrow()
            .connections
            .iter()
            .filter(|(_, info)| info.ready)
            .map(|(id, _)| *id)
            .collect()
    }

    /// Get all stream IDs for a connection.
    pub fn streams(&self, conn_id: u64) -> Vec<u64> {
        self.state
            .borrow()
            .connections
            .get(&conn_id)
            .map(|info| info.streams.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Get readable stream IDs for a connection.
    pub fn readable_streams(&self, conn_id: u64) -> Vec<u64> {
        self.state
            .borrow()
            .connections
            .get(&conn_id)
            .map(|info| {
                info.streams
                    .iter()
                    .filter(|(_, s)| s.readable)
                    .map(|(id, _)| *id)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Read data from a stream on a connection.
    pub fn stream_read(
        &mut self,
        conn_id: u64,
        stream_id: u64,
        buf: &mut [u8],
    ) -> Result<(usize, bool), Error> {
        if let Some(conn) = self.endpoint.conn_get_mut(conn_id) {
            conn.stream_read(stream_id, buf)
                .map_err(|e| Error::Stream(e.to_string()))
        } else {
            Err(Error::ConnectionClosed {
                reason: "connection not found".to_string(),
            })
        }
    }

    /// Write data to a stream on a connection.
    pub fn stream_write(
        &mut self,
        conn_id: u64,
        stream_id: u64,
        data: &[u8],
        fin: bool,
    ) -> Result<usize, Error> {
        if let Some(conn) = self.endpoint.conn_get_mut(conn_id) {
            conn.stream_write(stream_id, Bytes::copy_from_slice(data), fin)
                .map_err(|e| Error::Stream(e.to_string()))
        } else {
            Err(Error::ConnectionClosed {
                reason: "connection not found".to_string(),
            })
        }
    }

    /// Close a connection.
    pub fn close_connection(
        &mut self,
        conn_id: u64,
        error_code: u64,
        reason: &str,
    ) -> Result<(), Error> {
        if let Some(conn) = self.endpoint.conn_get_mut(conn_id) {
            conn.close(true, error_code, reason.as_bytes())
                .map_err(|e| Error::Quic(e.to_string()))?;
        }
        self.state.borrow_mut().connections.remove(&conn_id);
        Ok(())
    }
}

/// Handler for server-side tquic transport events.
struct ServerHandler {
    state: Rc<RefCell<ServerState>>,
}

impl TransportHandler for ServerHandler {
    fn on_conn_created(&mut self, conn: &mut Connection) {
        let conn_id = conn.trace_id();
        tracing::debug!("Server connection created: {}", conn_id);
    }

    fn on_conn_established(&mut self, conn: &mut Connection) {
        let conn_id = conn.index().unwrap_or(0);
        tracing::info!("Server connection established: {}", conn_id);

        let peer = conn.paths_iter().next().map(|p| p.remote);
        let mut state = self.state.borrow_mut();
        
        // Check if connection already exists (from on_stream_created)
        // If so, just update ready flag and peer_addr; otherwise create new entry
        if let Some(conn_info) = state.connections.get_mut(&conn_id) {
            conn_info.ready = true;
            conn_info.peer_addr = peer.unwrap_or_else(|| "0.0.0.0:0".parse().unwrap());
        } else {
            state.connections.insert(
                conn_id,
                ConnectionInfo {
                    peer_addr: peer.unwrap_or_else(|| "0.0.0.0:0".parse().unwrap()),
                    ready: true,
                    streams: HashMap::new(),
                },
            );
        }
    }

    fn on_conn_closed(&mut self, conn: &mut Connection) {
        let conn_id = conn.index().unwrap_or(0);
        tracing::info!("Server connection closed: {}", conn_id);
        self.state.borrow_mut().connections.remove(&conn_id);
    }

    fn on_stream_created(&mut self, conn: &mut Connection, stream_id: u64) {
        let conn_id = conn.index().unwrap_or(0);
        tracing::debug!("Server stream {} created on conn {}", stream_id, conn_id);

        let mut state = self.state.borrow_mut();
        // Create connection entry if it doesn't exist (stream events can arrive before conn_established)
        let conn_info = state.connections.entry(conn_id).or_insert_with(|| {
            let peer = conn.paths_iter().next().map(|p| p.remote);
            ConnectionInfo {
                peer_addr: peer.unwrap_or_else(|| "0.0.0.0:0".parse().unwrap()),
                ready: false, // Will be set to true by on_conn_established
                streams: HashMap::new(),
            }
        });
        conn_info.streams.insert(
            stream_id,
            StreamState {
                readable: false,
                writable: true,
            },
        );
    }

    fn on_stream_readable(&mut self, conn: &mut Connection, stream_id: u64) {
        let conn_id = conn.index().unwrap_or(0);
        tracing::trace!("Server stream {} readable on conn {}", stream_id, conn_id);

        if let Some(conn_info) = self.state.borrow_mut().connections.get_mut(&conn_id) {
            if let Some(stream) = conn_info.streams.get_mut(&stream_id) {
                stream.readable = true;
            }
        }
    }

    fn on_stream_writable(&mut self, conn: &mut Connection, stream_id: u64) {
        let conn_id = conn.index().unwrap_or(0);
        tracing::trace!("Server stream {} writable on conn {}", stream_id, conn_id);

        if let Some(conn_info) = self.state.borrow_mut().connections.get_mut(&conn_id) {
            if let Some(stream) = conn_info.streams.get_mut(&stream_id) {
                stream.writable = true;
            }
        }
    }

    fn on_stream_closed(&mut self, conn: &mut Connection, stream_id: u64) {
        let conn_id = conn.index().unwrap_or(0);
        tracing::debug!("Server stream {} closed on conn {}", stream_id, conn_id);

        if let Some(conn_info) = self.state.borrow_mut().connections.get_mut(&conn_id) {
            conn_info.streams.remove(&stream_id);
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
