//! TCP echo server implementation.

use crate::{LogEvent, LogWriter};
use std::net::SocketAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

pub async fn run(listen: SocketAddr, log_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut log = LogWriter::open(log_path)?;

    let listener = TcpListener::bind(listen).await?;

    let mut event = LogEvent::new("listening");
    event.listen = Some(listen.to_string());
    log.log(&event);

    loop {
        let (mut socket, peer) = listener.accept().await?;
        let peer_str = peer.to_string();

        let mut event = LogEvent::new("connect");
        event.peer = Some(peer_str.clone());
        log.log(&event);

        let mut buf = vec![0u8; 4096];
        loop {
            match socket.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    if let Err(e) = socket.write_all(&buf[..n]).await {
                        tracing::warn!("Echo write error: {}", e);
                        break;
                    }
                    let _ = socket.flush().await;

                    let mut event = LogEvent::new("echo");
                    event.peer = Some(peer_str.clone());
                    event.len = Some(n);
                    log.log(&event);
                }
                Err(e) => {
                    tracing::warn!("Echo read error: {}", e);
                    break;
                }
            }
        }

        let mut event = LogEvent::new("disconnect");
        event.peer = Some(peer_str);
        log.log(&event);
    }
}
