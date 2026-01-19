#![allow(dead_code)]
#![allow(private_interfaces)]

use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener as TokioTcpListener, TcpStream as TokioTcpStream};
use tokio::sync::{mpsc, Notify};

const STREAM_READ_CHUNK_BYTES: usize = 4096;

pub(crate) enum Command {
    NewStream(TokioTcpStream),
    StreamData { stream_id: u64, data: Vec<u8> },
    StreamClosed { stream_id: u64 },
    StreamReadError { stream_id: u64 },
    StreamWriteError { stream_id: u64 },
    StreamWriteDrained { stream_id: u64, bytes: usize },
}

pub(crate) fn spawn_acceptor(
    listener: TokioTcpListener,
    command_tx: mpsc::UnboundedSender<Command>,
) {
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    if command_tx.send(Command::NewStream(stream)).is_err() {
                        break;
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
    });
}

/// Spawn a task that reads TCP data and sends it as StreamData commands for QUIC forwarding.
pub(crate) fn spawn_tcp_to_quic_reader(
    stream_id: u64,
    mut tcp_read: tokio::net::tcp::OwnedReadHalf,
    command_tx: mpsc::UnboundedSender<Command>,
) {
    tokio::spawn(async move {
        let mut buf = vec![0u8; STREAM_READ_CHUNK_BYTES];
        loop {
            match tcp_read.read(&mut buf).await {
                Ok(0) => {
                    // EOF - close the QUIC stream
                    let _ = command_tx.send(Command::StreamClosed { stream_id });
                    break;
                }
                Ok(n) => {
                    let data = buf[..n].to_vec();
                    if command_tx
                        .send(Command::StreamData { stream_id, data })
                        .is_err()
                    {
                        break;
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    let _ = command_tx.send(Command::StreamReadError { stream_id });
                    break;
                }
            }
        }
    });
}

/// Spawn a task that writes data from QUIC to TCP.
pub(crate) fn spawn_quic_to_tcp_writer(
    mut tcp_write: tokio::net::tcp::OwnedWriteHalf,
    mut data_rx: mpsc::UnboundedReceiver<Vec<u8>>,
) {
    tokio::spawn(async move {
        while let Some(data) = data_rx.recv().await {
            if tcp_write.write_all(&data).await.is_err() {
                break;
            }
        }
        let _ = tcp_write.shutdown().await;
    });
}

pub(crate) fn spawn_client_reader(
    stream_id: u64,
    mut read_half: tokio::net::tcp::OwnedReadHalf,
    command_tx: mpsc::UnboundedSender<Command>,
    data_tx: mpsc::Sender<Vec<u8>>,
    data_notify: Arc<Notify>,
) {
    tokio::spawn(async move {
        let mut buf = vec![0u8; STREAM_READ_CHUNK_BYTES];
        loop {
            match read_half.read(&mut buf).await {
                Ok(0) => {
                    break;
                }
                Ok(n) => {
                    let data = buf[..n].to_vec();
                    if data_tx.send(data).await.is_err() {
                        break;
                    }
                    data_notify.notify_one();
                }
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {
                    continue;
                }
                Err(_) => {
                    let _ = command_tx.send(Command::StreamReadError { stream_id });
                    break;
                }
            }
        }
        drop(data_tx);
        data_notify.notify_one();
    });
}

enum StreamWrite {
    Data(Vec<u8>),
    Fin,
}

pub(crate) fn spawn_client_writer(
    stream_id: u64,
    mut write_half: tokio::net::tcp::OwnedWriteHalf,
    mut write_rx: mpsc::UnboundedReceiver<StreamWrite>,
    command_tx: mpsc::UnboundedSender<Command>,
    coalesce_max_bytes: usize,
) {
    tokio::spawn(async move {
        let coalesce_max_bytes = coalesce_max_bytes.max(1);
        while let Some(msg) = write_rx.recv().await {
            match msg {
                StreamWrite::Data(data) => {
                    let mut buffer = data;
                    let mut saw_fin = false;
                    while buffer.len() < coalesce_max_bytes {
                        match write_rx.try_recv() {
                            Ok(StreamWrite::Data(more)) => {
                                buffer.extend_from_slice(&more);
                                if buffer.len() >= coalesce_max_bytes {
                                    break;
                                }
                            }
                            Ok(StreamWrite::Fin) => {
                                saw_fin = true;
                                break;
                            }
                            Err(mpsc::error::TryRecvError::Empty) => break,
                            Err(mpsc::error::TryRecvError::Disconnected) => {
                                saw_fin = true;
                                break;
                            }
                        }
                    }
                    let len = buffer.len();
                    if write_half.write_all(&buffer).await.is_err() {
                        let _ = command_tx.send(Command::StreamWriteError { stream_id });
                        return;
                    }
                    let _ = command_tx.send(Command::StreamWriteDrained {
                        stream_id,
                        bytes: len,
                    });
                    if saw_fin {
                        let _ = write_half.shutdown().await;
                        return;
                    }
                }
                StreamWrite::Fin => {
                    let _ = write_half.shutdown().await;
                    return;
                }
            }
        }
        let _ = write_half.shutdown().await;
    });
}
