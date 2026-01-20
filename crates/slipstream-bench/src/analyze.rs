//! JSON log analysis utilities.
//!
//! Provides subcommands for analyzing JSON log files from benchmarks.

use serde::Deserialize;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

/// Event from a benchmark log file.
#[derive(Deserialize, Debug)]
#[allow(dead_code)]
struct LogEvent {
    ts: Option<f64>,
    event: Option<String>,
    bytes: Option<u64>,
    secs: Option<f64>,
    first_payload_ts: Option<f64>,
    last_payload_ts: Option<f64>,
    direction: Option<String>,
}

/// Load all events from a JSONL file.
fn load_events(path: &Path) -> Result<Vec<LogEvent>, Box<dyn std::error::Error>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut events = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if let Ok(event) = serde_json::from_str::<LogEvent>(&line) {
            events.push(event);
        }
    }
    Ok(events)
}

/// Find the "done" event in a log file.
fn find_done_event(events: &[LogEvent]) -> Option<&LogEvent> {
    events.iter().find(|e| e.event.as_deref() == Some("done"))
}

/// Calculate E2E throughput from two log files.
/// Returns MiB/s.
pub fn e2e_throughput(
    start_log: &Path,
    end_log: &Path,
    bytes: u64,
) -> Result<f64, Box<dyn std::error::Error>> {
    let start_events = load_events(start_log)?;
    let end_events = load_events(end_log)?;

    let start = find_done_event(&start_events)
        .ok_or("Missing done event in start log")?;
    let end = find_done_event(&end_events)
        .ok_or("Missing done event in end log")?;

    let start_ts = start
        .first_payload_ts
        .ok_or("Missing first_payload_ts in start log")?;
    let end_ts = end
        .last_payload_ts
        .ok_or("Missing last_payload_ts in end log")?;

    let elapsed = end_ts - start_ts;
    if elapsed <= 0.0 {
        return Err(format!("Invalid timing window secs={:.6}", elapsed).into());
    }

    let mib_s = (bytes as f64 / (1024.0 * 1024.0)) / elapsed;
    Ok(mib_s)
}

/// Run E2E report: calculate and print throughput.
pub fn run_e2e_report(
    label: &str,
    start_log: &Path,
    end_log: &Path,
    bytes: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let mib_s = e2e_throughput(start_log, end_log, bytes)?;
    println!("{}: {:.2} MiB/s", label, mib_s);
    Ok(())
}

/// Extract just the MiB/s value (for command substitution).
pub fn extract_mib_s(
    start_log: &Path,
    end_log: &Path,
    bytes: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let mib_s = e2e_throughput(start_log, end_log, bytes)?;
    println!("{:.2}", mib_s);
    Ok(())
}

/// Enforce minimum average throughput from multiple runs.
pub fn enforce_min_avg(
    run_dir: &Path,
    transfer_bytes: u64,
    min_avg: Option<f64>,
    min_avg_exfil: Option<f64>,
    min_avg_download: Option<f64>,
    run_exfil: bool,
    run_download: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut exfil_rates = Vec::new();
    let mut download_rates = Vec::new();

    // Scan for run directories
    for entry in std::fs::read_dir(run_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().unwrap().to_string_lossy();
            if name.starts_with("run-") {
                // Look for exfil and download subdirs
                let exfil_dir = path.join("exfil");
                let download_dir = path.join("download");

                if run_exfil && exfil_dir.exists() {
                    let bench = exfil_dir.join("bench.jsonl");
                    let target = exfil_dir.join("target.jsonl");
                    if bench.exists() && target.exists() {
                        if let Ok(rate) = e2e_throughput(&bench, &target, transfer_bytes) {
                            exfil_rates.push(rate);
                        }
                    }
                }

                if run_download && download_dir.exists() {
                    let bench = download_dir.join("bench.jsonl");
                    let target = download_dir.join("target.jsonl");
                    if bench.exists() && target.exists() {
                        if let Ok(rate) = e2e_throughput(&target, &bench, transfer_bytes) {
                            download_rates.push(rate);
                        }
                    }
                }
            }
        }
    }

    // Calculate and check averages
    if run_exfil && !exfil_rates.is_empty() {
        let avg: f64 = exfil_rates.iter().sum::<f64>() / exfil_rates.len() as f64;
        println!("avg exfil MiB/s={:.2} (n={})", avg, exfil_rates.len());
        if let Some(min) = min_avg_exfil.or(min_avg) {
            if avg < min {
                return Err(format!("exfil throughput {:.2} < minimum {:.2}", avg, min).into());
            }
        }
    }

    if run_download && !download_rates.is_empty() {
        let avg: f64 = download_rates.iter().sum::<f64>() / download_rates.len() as f64;
        println!("avg download MiB/s={:.2} (n={})", avg, download_rates.len());
        if let Some(min) = min_avg_download.or(min_avg) {
            if avg < min {
                return Err(format!("download throughput {:.2} < minimum {:.2}", avg, min).into());
            }
        }
    }

    Ok(())
}

/// Check capture logs for bidirectional traffic.
pub fn check_capture(
    recursive_log: &Path,
    authoritative_log: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    for (label, path) in [("recursive", recursive_log), ("authoritative", authoritative_log)] {
        let events = load_events(path)?;
        let mut c2s = 0u64;
        let mut s2c = 0u64;
        for event in &events {
            match event.direction.as_deref() {
                Some("client_to_server") => c2s += 1,
                Some("server_to_client") => s2c += 1,
                _ => {}
            }
        }
        if c2s == 0 || s2c == 0 {
            return Err(format!(
                "{} capture missing directions: client_to_server={} server_to_client={}",
                label, c2s, s2c
            )
            .into());
        }
        println!(
            "{} capture: client_to_server={} server_to_client={}",
            label, c2s, s2c
        );
    }
    Ok(())
}

/// Enforce minimum throughput for a single value.
pub fn enforce_min_throughput(
    label: &str,
    value: f64,
    threshold: f64,
) -> Result<(), Box<dyn std::error::Error>> {
    if value < threshold {
        return Err(format!(
            "{} throughput {:.2} < minimum {:.2}",
            label, value, threshold
        )
        .into());
    }
    println!(
        "{} throughput minimum ok ({:.2} >= {:.2})",
        label, value, threshold
    );
    Ok(())
}
