//! TCP sink (receive) implementation.

use crate::{now_ts, summarize, LogEvent, LogWriter};
use std::net::SocketAddr;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

/// Run as server that receives data (sink mode).
pub async fn run_server(
    listen: SocketAddr,
    expected_bytes: u64,
    chunk_size: usize,
    socket_timeout: Duration,
    log_path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut log = LogWriter::open(log_path)?;

    let listener = TcpListener::bind(listen).await?;

    let mut event = LogEvent::new("listening");
    event.listen = Some(listen.to_string());
    event.mode = Some("sink".to_string());
    log.log(&event);

    let (socket, peer) = timeout(socket_timeout, listener.accept()).await??;
    let peer_str = peer.to_string();

    let mut event = LogEvent::new("accept");
    event.peer = Some(peer_str.clone());
    event.mode = Some("sink".to_string());
    log.log(&event);

    let result = receive_data(socket, expected_bytes, chunk_size, socket_timeout).await;

    match result {
        Ok((total, elapsed, first_ts, last_ts)) => {
            let mut event = LogEvent::new("done");
            event.mode = Some("sink".to_string());
            event.bytes = Some(total);
            event.secs = Some(elapsed);
            event.first_payload_ts = first_ts;
            event.last_payload_ts = last_ts;
            log.log(&event);

            summarize("server sink", total, elapsed);

            if expected_bytes > 0 && total < expected_bytes {
                return Err(
                    format!("received {} bytes, expected {}", total, expected_bytes).into(),
                );
            }
        }
        Err(e) => {
            tracing::error!("Sink receive error: {}", e);
            return Err(e);
        }
    }

    Ok(())
}

/// Run as client that sends data.
pub async fn run_client(
    connect: SocketAddr,
    bytes: u64,
    chunk_size: usize,
    socket_timeout: Duration,
    log_path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut log = LogWriter::open(log_path)?;

    let socket = timeout(socket_timeout, TcpStream::connect(connect)).await??;
    socket.set_nodelay(true)?;
    let peer_str = connect.to_string();

    let mut event = LogEvent::new("connect");
    event.peer = Some(peer_str.clone());
    event.mode = Some("send".to_string());
    log.log(&event);

    let result = send_data(socket, bytes, chunk_size, socket_timeout).await;

    match result {
        Ok((total, elapsed, first_ts, last_ts)) => {
            let mut event = LogEvent::new("done");
            event.mode = Some("send".to_string());
            event.bytes = Some(total);
            event.secs = Some(elapsed);
            event.first_payload_ts = first_ts;
            event.last_payload_ts = last_ts;
            log.log(&event);

            summarize("client send", total, elapsed);

            if total < bytes {
                return Err(format!("sent {} bytes, expected {}", total, bytes).into());
            }
        }
        Err(e) => {
            tracing::error!("Send error: {}", e);
            return Err(e);
        }
    }

    Ok(())
}

async fn receive_data(
    mut socket: TcpStream,
    expected_bytes: u64,
    chunk_size: usize,
    socket_timeout: Duration,
) -> Result<(u64, f64, Option<f64>, Option<f64>), Box<dyn std::error::Error>> {
    let mut buf = vec![0u8; chunk_size];
    let mut total = 0u64;
    let mut start: Option<Instant> = None;
    let mut first_payload_ts: Option<f64> = None;
    let mut last_payload_ts: Option<f64> = None;

    loop {
        match timeout(socket_timeout, socket.read(&mut buf)).await {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => {
                if first_payload_ts.is_none() {
                    first_payload_ts = Some(now_ts());
                    start = Some(Instant::now());
                }
                total += n as u64;
                last_payload_ts = Some(now_ts());

                if expected_bytes > 0 && total >= expected_bytes {
                    break;
                }
            }
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => return Err("read timeout".into()),
        }
    }

    let elapsed = start.map(|s| s.elapsed().as_secs_f64()).unwrap_or(0.0);
    Ok((total, elapsed, first_payload_ts, last_payload_ts))
}

async fn send_data(
    mut socket: TcpStream,
    bytes: u64,
    chunk_size: usize,
    socket_timeout: Duration,
) -> Result<(u64, f64, Option<f64>, Option<f64>), Box<dyn std::error::Error>> {
    let chunk = vec![b'b'; chunk_size];
    let mut remaining = bytes;
    let mut start: Option<Instant> = None;
    let mut first_payload_ts: Option<f64> = None;
    let mut last_payload_ts: Option<f64> = None;

    while remaining > 0 {
        let send_len = (remaining as usize).min(chunk_size);
        if first_payload_ts.is_none() {
            first_payload_ts = Some(now_ts());
            start = Some(Instant::now());
        }

        match timeout(socket_timeout, socket.write_all(&chunk[..send_len])).await {
            Ok(Ok(())) => {
                last_payload_ts = Some(now_ts());
                remaining -= send_len as u64;
            }
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => return Err("write timeout".into()),
        }
    }

    // Shutdown write side to signal EOF
    let _ = socket.shutdown().await;

    let elapsed = start.map(|s| s.elapsed().as_secs_f64()).unwrap_or(0.0);
    Ok((bytes, elapsed, first_payload_ts, last_payload_ts))
}
