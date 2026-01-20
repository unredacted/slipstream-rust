//! TCP benchmark harness for slipstream tests.
//!
//! This replaces the Python tcp_bench.py and tcp_echo.py scripts with a proper
//! async Rust implementation for reliable CI benchmarks.

mod analyze;
mod echo;
mod sink;
mod source;
mod udp_proxy;

use clap::{Parser, Subcommand};
use std::io::Write;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing_subscriber::EnvFilter;

/// TCP benchmark harness for slipstream tests.
#[derive(Parser, Debug)]
#[command(name = "slipstream-bench", about = "TCP benchmark harness")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run as TCP echo server
    Echo {
        /// Listen address (host:port)
        #[arg(long)]
        listen: SocketAddr,

        /// Log file path (use - for stdout)
        #[arg(long, default_value = "-")]
        log: String,
    },

    /// Run as TCP sink (receive data)
    Sink {
        /// Listen address (host:port)
        #[arg(long)]
        listen: SocketAddr,

        /// Expected bytes to receive (0 = unlimited)
        #[arg(long, default_value = "0")]
        bytes: u64,

        /// Read chunk size
        #[arg(long, default_value = "16384")]
        chunk_size: usize,

        /// Socket timeout in seconds
        #[arg(long, default_value = "30")]
        timeout: u64,

        /// Log file path (use - for stdout)
        #[arg(long, default_value = "-")]
        log: String,
    },

    /// Run as TCP source (send data)
    Source {
        /// Listen address (host:port)
        #[arg(long)]
        listen: SocketAddr,

        /// Bytes to send
        #[arg(long)]
        bytes: u64,

        /// Write chunk size
        #[arg(long, default_value = "16384")]
        chunk_size: usize,

        /// Preface bytes to receive before starting
        #[arg(long, default_value = "0")]
        preface_bytes: u64,

        /// Socket timeout in seconds
        #[arg(long, default_value = "30")]
        timeout: u64,

        /// Log file path (use - for stdout)
        #[arg(long, default_value = "-")]
        log: String,
    },

    /// Run as TCP client sending data
    Send {
        /// Connect address (host:port)
        #[arg(long)]
        connect: SocketAddr,

        /// Bytes to send
        #[arg(long)]
        bytes: u64,

        /// Write chunk size
        #[arg(long, default_value = "16384")]
        chunk_size: usize,

        /// Socket timeout in seconds
        #[arg(long, default_value = "30")]
        timeout: u64,

        /// Log file path (use - for stdout)
        #[arg(long, default_value = "-")]
        log: String,
    },

    /// Run as TCP client receiving data
    Recv {
        /// Connect address (host:port)
        #[arg(long)]
        connect: SocketAddr,

        /// Expected bytes to receive (0 = unlimited)
        #[arg(long, default_value = "0")]
        bytes: u64,

        /// Read chunk size
        #[arg(long, default_value = "16384")]
        chunk_size: usize,

        /// Preface bytes to send before receiving
        #[arg(long, default_value = "0")]
        preface_bytes: u64,

        /// Socket timeout in seconds
        #[arg(long, default_value = "30")]
        timeout: u64,

        /// Log file path (use - for stdout)
        #[arg(long, default_value = "-")]
        log: String,
    },

    /// Run as UDP proxy with delay/jitter simulation
    UdpProxy {
        /// Listen address (host:port)
        #[arg(long)]
        listen: SocketAddr,

        /// Upstream address (host:port)
        #[arg(long)]
        upstream: SocketAddr,

        /// Base delay in milliseconds
        #[arg(long, default_value = "0")]
        delay_ms: f64,

        /// Jitter standard deviation in milliseconds
        #[arg(long, default_value = "0")]
        jitter_ms: f64,

        /// Delay distribution (normal or uniform)
        #[arg(long, default_value = "normal")]
        dist: String,

        /// Stop after N packets (0 = unlimited)
        #[arg(long, default_value = "0")]
        max_packets: u64,

        /// Random seed
        #[arg(long)]
        seed: Option<u64>,

        /// Target reorder rate 0.0-1.0 (0 disables reordering)
        #[arg(long, default_value = "0")]
        reorder_rate: f64,

        /// Log file path (use - for stdout)
        #[arg(long, default_value = "-")]
        log: String,
    },

    /// Calculate E2E throughput from two log files
    E2eReport {
        /// Label for the output
        #[arg(long)]
        label: String,

        /// Path to start log file
        #[arg(long)]
        start_log: PathBuf,

        /// Path to end log file
        #[arg(long)]
        end_log: PathBuf,

        /// Number of bytes transferred
        #[arg(long)]
        bytes: u64,
    },

    /// Extract raw MiB/s value from two log files (for command substitution)
    ExtractMibS {
        /// Path to start log file
        #[arg(long)]
        start_log: PathBuf,

        /// Path to end log file
        #[arg(long)]
        end_log: PathBuf,

        /// Number of bytes transferred
        #[arg(long)]
        bytes: u64,
    },

    /// Enforce minimum average throughput from multiple runs
    EnforceMinAvg {
        /// Run directory containing run-N subdirectories
        #[arg(long)]
        run_dir: PathBuf,

        /// Bytes transferred per run
        #[arg(long)]
        bytes: u64,

        /// Minimum average MiB/s (applies to both if specific not set)
        #[arg(long)]
        min_avg: Option<f64>,

        /// Minimum average MiB/s for exfil
        #[arg(long)]
        min_avg_exfil: Option<f64>,

        /// Minimum average MiB/s for download
        #[arg(long)]
        min_avg_download: Option<f64>,

        /// Check exfil runs
        #[arg(long, default_value = "true")]
        run_exfil: bool,

        /// Check download runs
        #[arg(long, default_value = "true")]
        run_download: bool,
    },

    /// Check capture logs for bidirectional traffic
    CheckCapture {
        /// Path to recursive capture log
        #[arg(long)]
        recursive_log: PathBuf,

        /// Path to authoritative capture log
        #[arg(long)]
        authoritative_log: PathBuf,
    },

    /// Enforce minimum throughput for a single value
    EnforceMinThroughput {
        /// Label for the value
        #[arg(long)]
        label: String,

        /// Throughput value in MiB/s
        #[arg(long)]
        value: f64,

        /// Minimum threshold in MiB/s
        #[arg(long)]
        threshold: f64,
    },
}

/// JSON log event.
#[derive(serde::Serialize)]
struct LogEvent {
    ts: f64,
    event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    listen: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    peer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    secs: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    first_payload_ts: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_payload_ts: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    len: Option<usize>,
}

impl LogEvent {
    fn new(event: &str) -> Self {
        Self {
            ts: now_ts(),
            event: event.to_string(),
            listen: None,
            peer: None,
            mode: None,
            bytes: None,
            secs: None,
            first_payload_ts: None,
            last_payload_ts: None,
            len: None,
        }
    }
}

fn now_ts() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

enum LogWriter {
    Stdout,
    File(std::fs::File),
}

impl LogWriter {
    fn open(path: &str) -> std::io::Result<Self> {
        if path == "-" {
            Ok(Self::Stdout)
        } else {
            Ok(Self::File(std::fs::File::create(path)?))
        }
    }

    fn log(&mut self, event: &LogEvent) {
        let line = serde_json::to_string(event).unwrap_or_default();
        match self {
            Self::Stdout => {
                println!("{}", line);
            }
            Self::File(f) => {
                let _ = writeln!(f, "{}", line);
                let _ = f.flush();
            }
        }
    }
}

fn summarize(label: &str, total: u64, elapsed: f64) {
    let mib = total as f64 / (1024.0 * 1024.0);
    let mib_s = if elapsed > 0.0 { mib / elapsed } else { 0.0 };
    println!(
        "{}: bytes={} secs={:.3} MiB/s={:.2}",
        label, total, elapsed, mib_s
    );
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .without_time()
        .init();

    let args = Args::parse();

    match args.command {
        Command::Echo { listen, log } => {
            echo::run(listen, &log).await?;
        }
        Command::Sink {
            listen,
            bytes,
            chunk_size,
            timeout,
            log,
        } => {
            sink::run_server(
                listen,
                bytes,
                chunk_size,
                Duration::from_secs(timeout),
                &log,
            )
            .await?;
        }
        Command::Source {
            listen,
            bytes,
            chunk_size,
            preface_bytes,
            timeout,
            log,
        } => {
            source::run_server(
                listen,
                bytes,
                chunk_size,
                preface_bytes,
                Duration::from_secs(timeout),
                &log,
            )
            .await?;
        }
        Command::Send {
            connect,
            bytes,
            chunk_size,
            timeout,
            log,
        } => {
            sink::run_client(
                connect,
                bytes,
                chunk_size,
                Duration::from_secs(timeout),
                &log,
            )
            .await?;
        }
        Command::Recv {
            connect,
            bytes,
            chunk_size,
            preface_bytes,
            timeout,
            log,
        } => {
            source::run_client(
                connect,
                bytes,
                chunk_size,
                preface_bytes,
                Duration::from_secs(timeout),
                &log,
            )
            .await?;
        }
        Command::UdpProxy {
            listen,
            upstream,
            delay_ms,
            jitter_ms,
            dist,
            max_packets,
            seed,
            reorder_rate,
            log,
        } => {
            udp_proxy::run(
                listen,
                upstream,
                &log,
                delay_ms,
                jitter_ms,
                &dist,
                max_packets,
                seed,
                reorder_rate,
            )
            .await?;
        }
        Command::E2eReport {
            label,
            start_log,
            end_log,
            bytes,
        } => {
            analyze::run_e2e_report(&label, &start_log, &end_log, bytes)?;
        }
        Command::ExtractMibS {
            start_log,
            end_log,
            bytes,
        } => {
            analyze::extract_mib_s(&start_log, &end_log, bytes)?;
        }
        Command::EnforceMinAvg {
            run_dir,
            bytes,
            min_avg,
            min_avg_exfil,
            min_avg_download,
            run_exfil,
            run_download,
        } => {
            analyze::enforce_min_avg(
                &run_dir,
                bytes,
                min_avg,
                min_avg_exfil,
                min_avg_download,
                run_exfil,
                run_download,
            )?;
        }
        Command::CheckCapture {
            recursive_log,
            authoritative_log,
        } => {
            analyze::check_capture(&recursive_log, &authoritative_log)?;
        }
        Command::EnforceMinThroughput {
            label,
            value,
            threshold,
        } => {
            analyze::enforce_min_throughput(&label, value, threshold)?;
        }
    }

    Ok(())
}
