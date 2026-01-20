//! UDP capture proxy with delay/jitter simulation.
//!
//! This replaces the Python udp_capture_proxy.py with a proper async Rust implementation.
//! Features:
//! - Delay distribution from sorted pool (prevents natural reordering)
//! - Controlled reordering via periodic adjacent swaps
//! - JSON logging of all packets

use crate::{now_ts, LogWriter};
use rand::prelude::*;
use rand_distr::{Distribution, Normal, Uniform};
use serde::Serialize;
use std::collections::{BinaryHeap, HashMap};
use std::io::Write;
use std::net::SocketAddr;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;

/// Pending packet to be sent at a scheduled time.
#[derive(Debug, Clone)]
struct PendingPacket {
    send_at: Instant,
    seq: u64,
    data: Vec<u8>,
    dst: SocketAddr,
    direction: String,
    natural_delay_ms: f64,
}

impl Eq for PendingPacket {}
impl PartialEq for PendingPacket {
    fn eq(&self, other: &Self) -> bool {
        self.seq == other.seq
    }
}
impl Ord for PendingPacket {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Reverse order for min-heap
        other
            .send_at
            .cmp(&self.send_at)
            .then(other.seq.cmp(&self.seq))
    }
}
impl PartialOrd for PendingPacket {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Sorted delay model that samples from a pre-generated sorted pool.
struct SortedDelayModel {
    sorted_pool: Vec<f64>,
    pool_size: usize,
    stride: f64,
    base_ms: f64,
    jitter_ms: f64,
    dist: DelayDist,
    state: HashMap<String, f64>,
    rng: StdRng,
}

#[derive(Clone, Copy)]
enum DelayDist {
    Normal,
    Uniform,
}

impl SortedDelayModel {
    fn new(
        base_ms: f64,
        jitter_ms: f64,
        pool_size: usize,
        dist: DelayDist,
        seed: Option<u64>,
    ) -> Self {
        let rng = match seed {
            Some(s) => StdRng::seed_from_u64(s),
            None => StdRng::from_entropy(),
        };
        let stride = pool_size as f64 / pool_size as f64;
        let mut model = Self {
            sorted_pool: Vec::new(),
            pool_size,
            stride,
            base_ms,
            jitter_ms,
            dist,
            state: HashMap::new(),
            rng,
        };
        model.generate_pool();
        model
    }

    fn generate_pool(&mut self) {
        let mut delays = Vec::with_capacity(self.pool_size);
        for _ in 0..self.pool_size {
            let jitter = if self.jitter_ms <= 0.0 {
                0.0
            } else {
                match self.dist {
                    DelayDist::Uniform => {
                        let dist = Uniform::new(-self.jitter_ms, self.jitter_ms);
                        dist.sample(&mut self.rng)
                    }
                    DelayDist::Normal => {
                        let dist = Normal::new(0.0, self.jitter_ms).unwrap();
                        dist.sample(&mut self.rng)
                    }
                }
            };
            delays.push((self.base_ms + jitter).max(0.0));
        }
        delays.sort_by(|a, b| a.partial_cmp(b).unwrap());
        self.sorted_pool = delays;
    }

    fn sample(&mut self, direction: &str) -> f64 {
        // Get or create the float_index for this direction
        let float_index = *self
            .state
            .entry(direction.to_string())
            .or_insert_with(|| rand::random::<f64>() * self.pool_size as f64);

        let idx = (float_index as usize) % self.pool_size;

        // Check if we need to regenerate the pool
        if idx == 0 && float_index >= self.pool_size as f64 {
            self.generate_pool();
            self.state.insert(direction.to_string(), 0.0);
        }

        let delay_ms = self.sorted_pool[idx];

        // Update the index
        if let Some(fi) = self.state.get_mut(direction) {
            *fi += self.stride;
        }

        delay_ms
    }
}

/// Reorder controller that injects controlled reordering by swapping adjacent packets.
struct ReorderController {
    #[allow(dead_code)]
    reorder_rate: f64,
    min_gap_s: f64,
    idle_timeout_s: f64,
    interval: usize,
    state: HashMap<String, ReorderState>,
    stats: HashMap<String, ReorderStats>,
}

struct ReorderState {
    floor: Instant,
    prev: Option<PendingPacket>,
    count: u64,
    last_recv: Instant,
}

impl ReorderState {
    fn new() -> Self {
        Self {
            floor: Instant::now(),
            prev: None,
            count: 0,
            last_recv: Instant::now(),
        }
    }
}

#[derive(Default)]
struct ReorderStats {
    total: u64,
    reordered: u64,
}

impl ReorderController {
    fn new(reorder_rate: f64, min_gap_ms: f64, idle_timeout_ms: f64) -> Self {
        let interval = if reorder_rate > 0.0 {
            (1.0 / reorder_rate).round() as usize
        } else {
            0
        };
        let mut stats = HashMap::new();
        stats.insert("client_to_server".to_string(), ReorderStats::default());
        stats.insert("server_to_client".to_string(), ReorderStats::default());
        Self {
            reorder_rate: reorder_rate.max(0.0),
            min_gap_s: min_gap_ms.max(0.0) / 1000.0,
            idle_timeout_s: idle_timeout_ms.max(0.0) / 1000.0,
            interval,
            state: HashMap::new(),
            stats,
        }
    }

    fn process(
        &mut self,
        direction: &str,
        recv_time: Instant,
        pkt: PendingPacket,
    ) -> Vec<PendingPacket> {
        let state = self
            .state
            .entry(direction.to_string())
            .or_insert_with(ReorderState::new);
        state.count += 1;
        if let Some(s) = self.stats.get_mut(direction) {
            s.total += 1;
        }
        state.last_recv = recv_time;

        let natural_send_at = pkt.send_at;
        let send_at = if natural_send_at >= state.floor {
            natural_send_at
        } else {
            state.floor + Duration::from_secs_f64(self.min_gap_s)
        };

        if self.interval == 0 {
            // No reordering
            state.prev = None;
            state.floor = send_at;
            return vec![PendingPacket { send_at, ..pkt }];
        }

        // First packet: stash and wait for next
        if state.prev.is_none() {
            state.prev = Some(PendingPacket { send_at, ..pkt });
            return vec![];
        }

        let prev = state.prev.take().unwrap();
        let should_reorder = self.interval > 0 && state.count as usize % self.interval == 0;

        if should_reorder {
            // Swap: send current first, then previous
            let first = PendingPacket { send_at, ..pkt };
            let second_send_at = send_at + Duration::from_secs_f64(self.min_gap_s);
            let second_send_at = second_send_at.max(prev.send_at);
            let second = PendingPacket {
                send_at: second_send_at,
                ..prev
            };
            state.floor = second_send_at;
            if let Some(s) = self.stats.get_mut(direction) {
                s.reordered += 1;
            }
            vec![first, second]
        } else {
            // Normal: send previous, keep current queued
            let scheduled_prev_at = prev.send_at.max(state.floor);
            state.floor = scheduled_prev_at;
            state.prev = Some(PendingPacket { send_at, ..pkt });
            vec![PendingPacket {
                send_at: scheduled_prev_at,
                ..prev
            }]
        }
    }

    fn flush(&mut self, direction: &str) -> Option<PendingPacket> {
        if let Some(state) = self.state.get_mut(direction) {
            if let Some(prev) = state.prev.take() {
                let send_at = prev.send_at.max(state.floor);
                state.floor = send_at;
                return Some(PendingPacket { send_at, ..prev });
            }
        }
        None
    }

    fn release_idle(&mut self, now: Instant) -> Vec<(String, PendingPacket)> {
        let mut entries = Vec::new();
        for (direction, state) in &mut self.state {
            if let Some(prev) = &state.prev {
                if now.duration_since(state.last_recv).as_secs_f64() >= self.idle_timeout_s {
                    let send_at = prev.send_at.max(state.floor);
                    state.floor = send_at;
                    let pkt = PendingPacket {
                        send_at,
                        ..state.prev.take().unwrap()
                    };
                    entries.push((direction.clone(), pkt));
                }
            }
        }
        entries
    }

    fn print_stats(&self) {
        eprintln!("\n=== Reorder Statistics ===");
        for (direction, s) in &self.stats {
            let pct = if s.total > 0 {
                s.reordered as f64 / s.total as f64 * 100.0
            } else {
                0.0
            };
            eprintln!("  {}: {}/{} ({:.4}%)", direction, s.reordered, s.total, pct);
        }
    }
}

/// Log event for UDP proxy.
#[derive(Serialize)]
struct ProxyLogEvent {
    ts: f64,
    direction: String,
    len: usize,
    src: String,
    dst: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    hex: Option<String>,
    delay_ms: f64,
}

pub async fn run(
    listen: SocketAddr,
    upstream: SocketAddr,
    log_path: &str,
    delay_ms: f64,
    jitter_ms: f64,
    dist: &str,
    max_packets: u64,
    seed: Option<u64>,
    reorder_rate: f64,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut log = LogWriter::open(log_path)?;

    let socket = UdpSocket::bind(listen).await?;

    eprintln!("UDP proxy listening on {}", listen);
    eprintln!("  Upstream: {}", upstream);
    eprintln!("  Delay: {}ms ± {}ms", delay_ms, jitter_ms);
    if reorder_rate > 0.0 {
        eprintln!("  Target reorder rate: {:.4}%", reorder_rate * 100.0);
    }

    let dist_type = if dist == "uniform" {
        DelayDist::Uniform
    } else {
        DelayDist::Normal
    };
    let mut delay_model = SortedDelayModel::new(delay_ms, jitter_ms, 20000, dist_type, seed);
    let mut reorder_ctrl = ReorderController::new(reorder_rate, 0.1, 50.0);

    let mut last_client: Option<SocketAddr> = None;
    let mut packet_count = 0u64;
    let mut pending: BinaryHeap<PendingPacket> = BinaryHeap::new();
    let mut seq = 0u64;
    let mut buf = vec![0u8; 65535];

    loop {
        let now = Instant::now();

        // Release any idle packets from the reorder controller
        for (_direction, pkt) in reorder_ctrl.release_idle(now) {
            log_packet(&mut log, &pkt, pkt.data.len());
            pending.push(pkt);
        }

        // Calculate timeout for next pending packet
        let timeout = if let Some(next) = pending.peek() {
            let now = Instant::now();
            if next.send_at <= now {
                Duration::ZERO
            } else {
                next.send_at.duration_since(now)
            }
        } else {
            Duration::from_secs(3600) // Long timeout when nothing pending
        };

        // Send any due packets
        while let Some(pkt) = pending.peek() {
            if pkt.send_at <= Instant::now() {
                let pkt = pending.pop().unwrap();
                socket.send_to(&pkt.data, pkt.dst).await?;
            } else {
                break;
            }
        }

        // Wait for incoming packet or timeout
        let recv_result = tokio::time::timeout(timeout, socket.recv_from(&mut buf)).await;

        match recv_result {
            Ok(Ok((len, addr))) => {
                let data = buf[..len].to_vec();
                let (direction, dst) = if addr == upstream {
                    ("server_to_client", last_client)
                } else {
                    last_client = Some(addr);
                    ("client_to_server", Some(upstream))
                };

                let Some(dst) = dst else { continue };

                let natural_delay_ms = delay_model.sample(direction);
                let send_at = Instant::now() + Duration::from_secs_f64(natural_delay_ms / 1000.0);

                seq += 1;
                let pkt = PendingPacket {
                    send_at,
                    seq,
                    data: data.clone(),
                    dst,
                    direction: direction.to_string(),
                    natural_delay_ms,
                };

                // Process through reorder controller
                let scheduled = reorder_ctrl.process(direction, Instant::now(), pkt);
                for pkt in scheduled {
                    log_packet(&mut log, &pkt, len);
                    pending.push(pkt);
                }

                packet_count += 1;
                if max_packets > 0 && packet_count >= max_packets {
                    break;
                }
            }
            Ok(Err(e)) => {
                tracing::warn!("UDP recv error: {}", e);
            }
            Err(_) => {
                // Timeout - just loop to send pending packets
            }
        }
    }

    // Flush any held packets
    for direction in ["client_to_server", "server_to_client"] {
        if let Some(pkt) = reorder_ctrl.flush(direction) {
            log_packet(&mut log, &pkt, pkt.data.len());
            pending.push(pkt);
        }
    }

    // Drain any remaining packets
    while let Some(pkt) = pending.pop() {
        let delay = pkt.send_at.saturating_duration_since(Instant::now());
        if delay > Duration::ZERO {
            tokio::time::sleep(delay).await;
        }
        socket.send_to(&pkt.data, pkt.dst).await?;
    }

    reorder_ctrl.print_stats();

    Ok(())
}

fn log_packet(log: &mut LogWriter, pkt: &PendingPacket, len: usize) {
    let event = ProxyLogEvent {
        ts: now_ts(),
        direction: pkt.direction.clone(),
        len,
        src: "".to_string(), // Not tracked in this simplified version
        dst: pkt.dst.to_string(),
        hex: Some(hex::encode(&pkt.data).to_uppercase()),
        delay_ms: pkt.natural_delay_ms,
    };
    let line = serde_json::to_string(&event).unwrap_or_default();
    match log {
        LogWriter::Stdout => println!("{}", line),
        LogWriter::File(f) => {
            let _ = writeln!(f, "{}", line);
            let _ = f.flush();
        }
    }
}
