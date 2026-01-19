//! QUIC server runtime using tquic.
//!
//! This module provides the QUIC server runtime using the pure-Rust tquic library.

use slipstream_core::{resolve_host_port, HostPort};
use slipstream_dns::{
    decode_query_with_domains, encode_response, DecodeQueryError, FragmentBuffer, Question, Rcode, ResponseParams,
};
use slipstream_quic::{Config as QuicConfig, Server};
use std::collections::HashMap;
use std::fmt;
use std::net::{Ipv6Addr, SocketAddr, SocketAddrV6};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::net::{TcpStream, UdpSocket as TokioUdpSocket};
use tokio::sync::mpsc;
use tokio::time::sleep;
use tracing::{debug, info, warn};

// Protocol defaults matching picoquic server
const DNS_MAX_QUERY_SIZE: usize = 512;
const IDLE_SLEEP_MS: u64 = 10;
const MAX_PACKET_SIZE: usize = 1500;
pub(crate) const STREAM_READ_CHUNK_BYTES: usize = 4096;

static SHOULD_SHUTDOWN: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_sigterm(_signum: libc::c_int) {
    SHOULD_SHUTDOWN.store(true, Ordering::Relaxed);
}

#[derive(Debug)]
pub struct TquicServerError {
    message: String,
}

impl TquicServerError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for TquicServerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for TquicServerError {}

/// Server configuration for tquic runtime (mirrors ServerConfig from server.rs).
#[allow(dead_code)]
pub struct TquicServerConfig {
    pub dns_listen_port: u16,
    pub target_address: HostPort,
    pub cert: String,
    pub key: String,
    pub domains: Vec<String>,
    pub max_connections: u32,
    pub debug_streams: bool,
    pub debug_commands: bool,
}

/// Stream state for tracking QUIC stream to TCP connection mapping.
#[allow(dead_code)]
struct StreamState {
    tcp_stream: Option<TcpStream>,
    write_tx: mpsc::UnboundedSender<StreamWrite>,
    rx_bytes: u64,
    tx_bytes: u64,
}

/// Commands for stream management.
#[allow(dead_code)]
enum StreamWrite {
    Data(Vec<u8>),
    Fin,
}

/// Slot for pending DNS response (mirrors Slot from server.rs).
#[allow(dead_code)]
struct Slot {
    peer: SocketAddr,
    id: u16,
    rd: bool,
    cd: bool,
    question: Question,
    rcode: Option<Rcode>,
    conn_id: Option<u64>,
}

/// Run the server.
pub async fn run_server(config: &TquicServerConfig) -> Result<i32, TquicServerError> {
    let _target_addr = resolve_host_port(&config.target_address)
        .map_err(|e| TquicServerError::new(e.to_string()))?;

    let (_command_tx, mut command_rx) = mpsc::unbounded_channel::<()>(); // Placeholder for commands
    let debug_streams = config.debug_streams;

    // Create tquic server config with multipath and TLS
    let quic_config = QuicConfig::new()
        .with_multipath(true)
        .with_tls(&config.cert, &config.key);

    // Create QUIC server
    let addr = SocketAddr::V6(SocketAddrV6::new(
        Ipv6Addr::UNSPECIFIED,
        config.dns_listen_port,
        0,
        0,
    ));
    let mut server = Server::new(addr, quic_config)
        .map_err(|e| TquicServerError::new(format!("Failed to create QUIC server: {}", e)))?;
    info!("Server listening on {}", addr);

    // Bind UDP socket for DNS
    let udp = bind_udp_socket(config.dns_listen_port).await?;
    warn_overlapping_domains(&config.domains);
    let domains: Vec<&str> = config.domains.iter().map(String::as_str).collect();
    if domains.is_empty() {
        return Err(TquicServerError::new(
            "At least one domain must be configured",
        ));
    }

    // Set up signal handler
    unsafe {
        libc::signal(libc::SIGTERM, handle_sigterm as usize);
    }

    let mut recv_buf = vec![0u8; DNS_MAX_QUERY_SIZE];
    let _send_buf = vec![0u8; MAX_PACKET_SIZE];
    let mut streams: HashMap<(u64, u64), StreamState> = HashMap::new();
    let mut fragment_buffer = FragmentBuffer::new();

    loop {
        if SHOULD_SHUTDOWN.load(Ordering::Relaxed) {
            info!("Shutdown requested");
            break;
        }

        let mut slots = Vec::new();
        let timeout = server
            .timeout()
            .unwrap_or(Duration::from_millis(IDLE_SLEEP_MS));

        tokio::select! {
            // Handle commands
            command = command_rx.recv() => {
                if command.is_some() {
                    // TODO: Handle server commands
                }
            }

            // Handle incoming UDP packets (DNS queries)
            recv = udp.recv_from(&mut recv_buf) => {
                match recv {
                    Ok((size, peer)) => {
                        if let Some(slot) = decode_slot_tquic(
                            &recv_buf[..size],
                            peer,
                            &domains,
                            &mut server,
                            &mut fragment_buffer,
                        )? {
                            slots.push(slot);
                        }

                        // Try to receive more packets in burst
                        for _ in 1..64 {
                            match udp.try_recv_from(&mut recv_buf) {
                                Ok((size, peer)) => {
                                    if let Some(slot) = decode_slot_tquic(
                                        &recv_buf[..size],
                                        peer,
                                        &domains,
                                        &mut server,
                                        &mut fragment_buffer,
                                    )? {
                                        slots.push(slot);
                                    }
                                }
                                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                                Err(e) => return Err(map_io(e)),
                            }
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(map_io(e)),
                }
            }

            // Handle timeout
            _ = sleep(timeout) => {
                server.on_timeout();
            }
        }

        // Process ready connections
        for conn_id in server.ready_connections() {
            let mut read_buf = vec![0u8; STREAM_READ_CHUNK_BYTES];

            // Try to read from streams
            for stream_id in 0..100u64 {
                match server.stream_read(conn_id, stream_id, &mut read_buf) {
                    Ok((n, fin)) if n > 0 => {
                        if debug_streams {
                            debug!(
                                "conn {} stream {}: read {} bytes, fin={}",
                                conn_id, stream_id, n, fin
                            );
                        }

                        // Forward to target
                        // TODO: Implement TCP forwarding to target_addr
                    }
                    Ok((_, true)) => {
                        // Stream finished
                        streams.remove(&(conn_id, stream_id));
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        }

        // Send DNS responses
        for slot in slots.iter_mut() {
            // Get QUIC packet to send
            let mut quic_payload = None;

            if slot.rcode.is_none() {
                // Poll for outgoing packet
                let packets = server.poll_send();
                for (packet_data, dest) in packets {
                    if normalize_dual_stack_addr(dest) == normalize_dual_stack_addr(slot.peer) {
                        quic_payload = Some(packet_data);
                        break;
                    }
                    // Send other packets
                    if let Err(e) = udp.send_to(&packet_data, dest).await {
                        warn!("Failed to send packet to {}: {}", dest, e);
                    }
                }
            }

            // Encode DNS response
            let (payload, rcode) = if let Some(ref data) = quic_payload {
                (Some(data.as_slice()), slot.rcode)
            } else if slot.rcode.is_none() {
                (None, Some(Rcode::Ok))
            } else {
                (None, slot.rcode)
            };

            let response = encode_response(&ResponseParams {
                id: slot.id,
                rd: slot.rd,
                cd: slot.cd,
                question: &slot.question,
                payload,
                rcode,
            })
            .map_err(|e| TquicServerError::new(e.to_string()))?;

            let peer = normalize_dual_stack_addr(slot.peer);
            udp.send_to(&response, peer).await.map_err(map_io)?;
        }

        // Poll and send any remaining packets
        let packets = server.poll_send();
        for (packet_data, dest) in packets {
            // Encode as DNS response (for unsolicited data)
            // In a full implementation, we'd need to track pending queries
            if let Err(e) = udp.send_to(&packet_data, dest).await {
                warn!("Failed to send packet: {}", e);
            }
        }
    }

    Ok(0)
}

/// Decode a DNS query slot using tquic (mirrors decode_slot from server.rs).
fn decode_slot_tquic(
    packet: &[u8],
    peer: SocketAddr,
    domains: &[&str],
    server: &mut Server,
    fragment_buffer: &mut FragmentBuffer,
) -> Result<Option<Slot>, TquicServerError> {
    match decode_query_with_domains(packet, domains) {
        Ok(query) => {
            // Try to reassemble fragment
            if let Some(complete_packet) = fragment_buffer.receive_fragment(&query.payload) {
                // Complete packet - feed to tquic
                if let Err(e) = server.recv(&complete_packet, peer) {
                    debug!("Failed to process QUIC packet: {}", e);
                }
            } else if query.payload.len() < 4 {
                // Too small to be a fragment, try direct processing
                if let Err(e) = server.recv(&query.payload, peer) {
                    debug!("Failed to process QUIC packet (direct): {}", e);
                }
            }
            // Note: If it's a fragment still waiting for more pieces, we just return
            // the slot for DNS response purposes but don't process the QUIC data yet

            Ok(Some(Slot {
                peer: normalize_dual_stack_addr(peer),
                id: query.id,
                rd: query.rd,
                cd: query.cd,
                question: query.question,
                rcode: None,
                conn_id: None, // Will be populated by ready_connections
            }))
        }
        Err(DecodeQueryError::Drop) => Ok(None),
        Err(DecodeQueryError::Reply {
            id,
            rd,
            cd,
            question,
            rcode,
        }) => {
            let question = match question {
                Some(q) => q,
                None => return Ok(None),
            };
            Ok(Some(Slot {
                peer: normalize_dual_stack_addr(peer),
                id,
                rd,
                cd,
                question,
                rcode: Some(rcode),
                conn_id: None,
            }))
        }
    }
}

async fn bind_udp_socket(port: u16) -> Result<TokioUdpSocket, TquicServerError> {
    let addr = SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, port, 0, 0));
    TokioUdpSocket::bind(addr).await.map_err(map_io)
}

fn normalize_dual_stack_addr(addr: SocketAddr) -> SocketAddr {
    match addr {
        SocketAddr::V4(v4) => {
            SocketAddr::V6(SocketAddrV6::new(v4.ip().to_ipv6_mapped(), v4.port(), 0, 0))
        }
        SocketAddr::V6(v6) => SocketAddr::V6(v6),
    }
}

fn map_io(err: std::io::Error) -> TquicServerError {
    TquicServerError::new(err.to_string())
}

fn warn_overlapping_domains(domains: &[String]) {
    if domains.len() < 2 {
        return;
    }

    let trimmed: Vec<String> = domains
        .iter()
        .map(|domain| domain.trim_end_matches('.').to_ascii_lowercase())
        .collect();

    for i in 0..trimmed.len() {
        for j in (i + 1)..trimmed.len() {
            let left = &trimmed[i];
            let right = &trimmed[j];

            if left == right {
                tracing::warn!(
                    "Duplicate domain configured: '{}' and '{}'",
                    domains[i],
                    domains[j]
                );
                continue;
            }

            if is_label_suffix(left, right) || is_label_suffix(right, left) {
                tracing::warn!(
                    "Configured domains overlap; longest suffix wins: '{}' and '{}'",
                    domains[i],
                    domains[j]
                );
            }
        }
    }
}

fn is_label_suffix(domain: &str, suffix: &str) -> bool {
    if domain.len() <= suffix.len() {
        return false;
    }
    if !domain.ends_with(suffix) {
        return false;
    }
    domain.as_bytes()[domain.len() - suffix.len() - 1] == b'.'
}
