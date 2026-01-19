//! QUIC client runtime using tquic.
//!
//! This module provides the QUIC client runtime using the pure-Rust tquic library.
//! The tquic runtime is now the default (replacing the legacy picoquic FFI).

mod path;

use self::path::{
    apply_path_mode_tquic, drain_path_events_tquic, fetch_path_quality_tquic,
    find_resolver_by_addr_mut, loop_burst_total,
};
use crate::dns::{expire_inflight_polls, normalize_dual_stack_addr, resolve_resolvers};
use crate::error::ClientError;
use crate::pacing::{cwnd_target_polls, inflight_packet_estimate};
use crate::streams::{spawn_acceptor, Command};
use slipstream_core::ResolverMode;
use slipstream_dns::{
    build_qname, decode_response, encode_query, fragment_packet, is_fragmented,
    max_payload_len_for_domain, FragmentBuffer, QueryParams, CLASS_IN, RR_TXT,
};
use slipstream_quic::{Client, ClientConnection, Config as QuicConfig};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener as TokioTcpListener, UdpSocket};
use tokio::sync::{mpsc, Notify};
use tokio::time::sleep;
use tracing::{debug, info, trace, warn};

// Protocol defaults matching picoquic runtime
const DNS_WAKE_DELAY_MAX_US: u64 = 10_000_000;
const DNS_POLL_SLICE_US: u64 = 50_000;
const MAX_PACKET_SIZE: usize = 1500;
const PACKET_LOOP_SEND_MAX: usize = 64;
const PACKET_LOOP_RECV_MAX: usize = 64;

/// Client configuration for tquic runtime (mirrors ClientConfig from slipstream-ffi).
#[allow(dead_code)]
pub struct TquicClientConfig<'a> {
    pub tcp_listen_port: u16,
    pub resolvers: &'a [slipstream_core::ResolverSpec],
    pub domain: &'a str,
    pub cert: Option<&'a str>,
    pub congestion_control: Option<&'a str>,
    pub gso: bool,
    pub keep_alive_interval: usize,
    pub debug_poll: bool,
    pub debug_streams: bool,
}

/// Stream state for tracking QUIC stream to TCP connection mapping.
#[allow(dead_code)]
struct StreamState {
    write_tx: mpsc::UnboundedSender<Vec<u8>>,
    queued_bytes: usize,
    rx_bytes: u64,
    tx_bytes: u64,
}

/// Run the client.
pub async fn run_client(config: &TquicClientConfig<'_>) -> Result<i32, ClientError> {
    let domain_len = config.domain.len();
    let mtu = compute_mtu(domain_len)?;
    let mut resolvers = resolve_resolvers(config.resolvers, mtu, config.debug_poll)?;
    if resolvers.is_empty() {
        return Err(ClientError::new("At least one resolver is required"));
    }

    // Bind UDP socket for DNS queries (use IPv6 dual-stack for compatibility with tquic)
    let udp = UdpSocket::bind("[::]:0")
        .await
        .map_err(|e| ClientError::new(format!("Failed to bind UDP socket: {}", e)))?;
    let local_addr = udp
        .local_addr()
        .map_err(|e| ClientError::new(format!("Failed to get local addr: {}", e)))?;

    // Setup TCP listener for incoming connections
    let (command_tx, mut command_rx) = mpsc::unbounded_channel();
    let data_notify = Arc::new(Notify::new());
    let debug_streams = config.debug_streams;
    let listener = TokioTcpListener::bind(("0.0.0.0", config.tcp_listen_port))
        .await
        .map_err(|e| ClientError::new(format!("Failed to bind TCP: {}", e)))?;
    spawn_acceptor(listener, command_tx.clone());
    info!("Listening on TCP port {}", config.tcp_listen_port);

    // Create tquic client config with multipath and DNS-appropriate packet size
    let mut quic_config = QuicConfig::new()
        .with_multipath(true)
        .with_send_udp_payload_size(mtu as usize);
    if config.keep_alive_interval > 0 {
        quic_config =
            quic_config.with_keep_alive(Duration::from_millis(config.keep_alive_interval as u64));
    }

    // Certificate pinning: use the provided cert as the only trusted CA
    if let Some(cert) = config.cert {
        quic_config = quic_config.with_ca(cert);
    }

    // TODO: Add congestion control override for tquic
    if config.congestion_control.is_some() {
        warn!("Congestion control override not yet implemented for tquic runtime");
    }

    if config.gso {
        warn!("GSO is not implemented in the tquic client runtime.");
    }

    // Create QUIC client
    let client = Client::new(quic_config)
        .map_err(|e| ClientError::new(format!("Failed to create QUIC client: {}", e)))?;

    // Connect to first resolver using domain as SNI
    let server_addr = resolvers[0].addr;
    let mut conn = client
        .connect(local_addr, server_addr, config.domain)
        .map_err(|e| ClientError::new(format!("Failed to connect: {}", e)))?;

    info!("Connecting to {}", server_addr);

    // Mark first resolver as connected
    resolvers[0].added = true;
    resolvers[0].path_id_tquic = Some(0);

    let mut dns_id = 1u16;
    let mut packet_id = 0u16; // For fragment tracking
    let mut recv_fragment_buffer = FragmentBuffer::new(); // For reassembling fragmented responses
    let mut recv_buf = vec![0u8; 4096];
    let _send_buf = vec![0u8; MAX_PACKET_SIZE];
    let packet_loop_send_max = loop_burst_total(&resolvers, PACKET_LOOP_SEND_MAX);
    let packet_loop_recv_max = loop_burst_total(&resolvers, PACKET_LOOP_RECV_MAX);
    let mut streams: HashMap<u64, StreamState> = HashMap::new();
    let mut zero_send_loops = 0u64;
    let mut ready = false;

    // Main event loop (mirrors picoquic runtime loop)
    loop {
        // Check connection state
        if conn.is_ready() && !ready {
            ready = true;
            info!("Connection ready");

            // Add additional paths for multipath
            for resolver in resolvers.iter_mut().skip(1) {
                if !resolver.added {
                    match conn.probe_path(resolver.addr) {
                        Ok(path_id) => {
                            resolver.path_id_tquic = Some(path_id);
                            debug!("Probing path to {}", resolver.addr);
                        }
                        Err(e) => {
                            warn!("Failed to probe path to {}: {}", resolver.addr, e);
                        }
                    }
                }
            }
        }

        if conn.is_closing() {
            info!("Connection closing");
            break;
        }

        // Drain path events
        drain_path_events_tquic(&mut conn, &mut resolvers);

        // Expire inflight polls for authoritative resolvers
        let current_time_us = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_micros() as u64)
            .unwrap_or(0);
        for resolver in resolvers.iter_mut() {
            if resolver.mode == ResolverMode::Authoritative {
                expire_inflight_polls(&mut resolver.inflight_poll_ids, current_time_us);
            }
        }

        // Calculate delay and work status
        let delay_us = conn
            .timeout()
            .map(|d| d.as_micros() as u64)
            .unwrap_or(DNS_WAKE_DELAY_MAX_US);
        let streams_len = streams.len();
        let mut has_work = streams_len > 0;

        for resolver in resolvers.iter_mut() {
            if !resolver.added {
                continue;
            }
            let pending_for_sleep = match resolver.mode {
                ResolverMode::Authoritative => {
                    let quality = fetch_path_quality_tquic(&mut conn, resolver);
                    let target = cwnd_target_polls(quality.cwin, mtu);
                    let inflight_packets = inflight_packet_estimate(quality.bytes_in_transit, mtu);
                    target.saturating_sub(inflight_packets)
                }
                ResolverMode::Recursive => resolver.pending_polls,
            };
            if pending_for_sleep > 0 {
                has_work = true;
            }
            if resolver.mode == ResolverMode::Authoritative
                && !resolver.inflight_poll_ids.is_empty()
            {
                has_work = true;
            }
        }

        let timeout_us = if has_work {
            delay_us.clamp(1, DNS_POLL_SLICE_US)
        } else {
            delay_us.max(1)
        };
        let timeout = Duration::from_micros(timeout_us);

        // Main select loop
        tokio::select! {
            // Handle incoming commands (new TCP connections, stream data)
            command = command_rx.recv() => {
                if let Some(command) = command {
                    handle_command(&mut conn, &mut streams, command, &command_tx, &data_notify, debug_streams)?;
                }
            }

            // Handle data notification
            _ = data_notify.notified() => {}

            // Handle incoming UDP packets (DNS responses)
            recv = udp.recv_from(&mut recv_buf) => {
                match recv {
                    Ok((size, from)) => {
                        // Decode DNS response to extract QUIC payload
                        if let Some(quic_payload) = decode_response(&recv_buf[..size]) {
                            // Handle fragmented responses
                            let complete_packet = if is_fragmented(&quic_payload) {
                                recv_fragment_buffer.receive_fragment(&quic_payload)
                            } else {
                                Some(quic_payload)
                            };

                            if let Some(data) = complete_packet {
                                if let Err(e) = conn.recv(&data, from) {
                                    debug!("Failed to process QUIC packet from {}: {}", from, e);
                                }
                            }
                        } else {
                            // Not a valid DNS response - try as raw QUIC packet
                            // (fallback for empty responses or direct UDP)
                            if let Err(e) = conn.recv(&recv_buf[..size], from) {
                                trace!("Failed to process raw packet from {}: {}", from, e);
                            }
                        }

                        // Try to receive more packets in burst
                        for _ in 1..packet_loop_recv_max {
                            match udp.try_recv_from(&mut recv_buf) {
                                Ok((size, from)) => {
                                    // Decode DNS response
                                    if let Some(quic_payload) = decode_response(&recv_buf[..size]) {
                                        let complete_packet = if is_fragmented(&quic_payload) {
                                            recv_fragment_buffer.receive_fragment(&quic_payload)
                                        } else {
                                            Some(quic_payload)
                                        };

                                        if let Some(data) = complete_packet {
                                            if let Err(e) = conn.recv(&data, from) {
                                                debug!("Failed to process QUIC packet: {}", e);
                                            }
                                        }
                                    } else {
                                        // Fallback to raw packet
                                        let _ = conn.recv(&recv_buf[..size], from);
                                    }
                                }
                                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                                Err(e) => return Err(ClientError::new(format!("UDP recv error: {}", e))),
                            }
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(ClientError::new(format!("UDP recv error: {}", e))),
                }
            }

            // Handle timeout
            _ = sleep(timeout) => {
                conn.on_timeout();
            }
        }

        // Read from QUIC streams and forward to TCP connections
        for stream_id in conn.readable_streams() {
            let mut read_buf = vec![0u8; 4096];
            match conn.stream_read(stream_id, &mut read_buf) {
                Ok((n, fin)) if n > 0 => {
                    if let Some(state) = streams.get(&stream_id) {
                        // Send data to TCP writer via channel
                        let _ = state.write_tx.send(read_buf[..n].to_vec());
                    }
                    if fin {
                        streams.remove(&stream_id);
                    }
                }
                Ok((_, true)) => {
                    // Stream finished
                    streams.remove(&stream_id);
                }
                _ => {}
            }
        }

        // Drain pending commands
        while let Ok(command) = command_rx.try_recv() {
            handle_command(
                &mut conn,
                &mut streams,
                command,
                &command_tx,
                &data_notify,
                debug_streams,
            )?;
        }

        // Poll for outgoing packets
        let packets = conn.poll_send();
        if packets.is_empty() {
            zero_send_loops = zero_send_loops.saturating_add(1);
        }

        for (packet_data, dest) in packets.into_iter().take(packet_loop_send_max) {
            // Update resolver stats
            let dest = normalize_dual_stack_addr(dest);
            if let Some(resolver) = find_resolver_by_addr_mut(&mut resolvers, dest) {
                resolver.debug.send_packets = resolver.debug.send_packets.saturating_add(1);
                resolver.debug.send_bytes = resolver
                    .debug
                    .send_bytes
                    .saturating_add(packet_data.len() as u64);
            }

            // Get max payload for domain
            let max_payload = max_payload_len_for_domain(config.domain)
                .map_err(|e| ClientError::new(format!("Failed to get max payload: {}", e)))?;

            // Fragment the QUIC packet if needed
            let fragments = fragment_packet(&packet_data, packet_id, max_payload);
            packet_id = packet_id.wrapping_add(1);

            // Send each fragment as a separate DNS query
            for fragment in fragments {
                let qname = build_qname(&fragment, config.domain)
                    .map_err(|e| ClientError::new(format!("Failed to build qname: {}", e)))?;
                let params = QueryParams {
                    id: dns_id,
                    qname: &qname,
                    qtype: RR_TXT,
                    qclass: CLASS_IN,
                    rd: true,
                    cd: false,
                    qdcount: 1,
                    is_query: true,
                };
                dns_id = dns_id.wrapping_add(1);

                let dns_packet = encode_query(&params)
                    .map_err(|e| ClientError::new(format!("Failed to encode DNS query: {}", e)))?;

                // Send to resolver
                udp.send_to(&dns_packet, dest)
                    .await
                    .map_err(|e| ClientError::new(format!("Failed to send DNS: {}", e)))?;
            }
        }

        // Path event handling and polling (for authoritative mode)
        drain_path_events_tquic(&mut conn, &mut resolvers);

        for resolver in resolvers.iter_mut() {
            if !resolver.added {
                continue;
            }
            apply_path_mode_tquic(&mut conn, resolver)?;
        }
    }

    // Close connection
    conn.close(0, "client shutdown")
        .map_err(|e| ClientError::new(format!("Failed to close: {}", e)))?;

    Ok(0)
}

/// Handle a command.
fn handle_command(
    conn: &mut ClientConnection,
    streams: &mut HashMap<u64, StreamState>,
    command: Command,
    command_tx: &mpsc::UnboundedSender<Command>,
    _data_notify: &Arc<Notify>,
    debug_streams: bool,
) -> Result<(), ClientError> {
    match command {
        Command::NewStream(tcp_stream) => {
            let _ = tcp_stream.set_nodelay(true);
            match conn.open_bi() {
                Ok(stream_id) => {
                    let (write_tx, write_rx) = mpsc::unbounded_channel();
                    streams.insert(
                        stream_id,
                        StreamState {
                            write_tx,
                            queued_bytes: 0,
                            rx_bytes: 0,
                            tx_bytes: 0,
                        },
                    );
                    if debug_streams {
                        debug!("stream {}: accepted", stream_id);
                    } else {
                        info!("Accepted TCP stream {}", stream_id);
                    }

                    // Split TCP stream and spawn reader/writer for bidirectional forwarding
                    let (tcp_read, tcp_write) = tcp_stream.into_split();
                    
                    // TCP→QUIC: Read TCP data and send to QUIC stream
                    crate::streams::spawn_tcp_to_quic_reader(
                        stream_id,
                        tcp_read,
                        command_tx.clone(),
                    );
                    
                    // QUIC→TCP: Write data from QUIC stream to TCP
                    crate::streams::spawn_quic_to_tcp_writer(tcp_write, write_rx);
                }
                Err(e) => {
                    warn!("Failed to open QUIC stream: {}", e);
                }
            }
        }
        Command::StreamData { stream_id, data } => {
            if let Err(e) = conn.stream_write(stream_id, &data, false) {
                warn!("Failed to write to stream {}: {}", stream_id, e);
                streams.remove(&stream_id);
            } else if let Some(stream) = streams.get_mut(&stream_id) {
                stream.tx_bytes = stream.tx_bytes.saturating_add(data.len() as u64);
            }
        }
        Command::StreamClosed { stream_id } => {
            if let Err(e) = conn.stream_write(stream_id, &[], true) {
                warn!("Failed to close stream {}: {}", stream_id, e);
            }
            streams.remove(&stream_id);
        }
        Command::StreamReadError { stream_id } => {
            warn!("stream {}: read error", stream_id);
            streams.remove(&stream_id);
        }
        Command::StreamWriteError { stream_id } => {
            warn!("stream {}: write error", stream_id);
            streams.remove(&stream_id);
        }
        Command::StreamWriteDrained { stream_id, bytes } => {
            if let Some(stream) = streams.get_mut(&stream_id) {
                stream.queued_bytes = stream.queued_bytes.saturating_sub(bytes);
            }
        }
    }
    Ok(())
}

/// Compute MTU based on domain length (mirrors setup.rs).
fn compute_mtu(domain_len: usize) -> Result<u32, ClientError> {
    // DNS query overhead + domain length considerations
    // Maximum DNS UDP payload is typically 512 bytes, but EDNS can extend this
    const BASE_MTU: u32 = 1200;
    const DOMAIN_OVERHEAD_PER_CHAR: u32 = 1;
    let overhead = domain_len as u32 * DOMAIN_OVERHEAD_PER_CHAR;
    if overhead >= BASE_MTU {
        return Err(ClientError::new("Domain too long for DNS tunneling"));
    }
    Ok(BASE_MTU - overhead)
}

// Re-export PathManager trait for multipath
use slipstream_quic::multipath::PathManager;
