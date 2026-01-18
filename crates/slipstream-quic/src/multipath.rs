//! Multipath QUIC support.
//!
//! This module provides abstractions for managing multiple network paths
//! within a single QUIC connection.

use std::net::SocketAddr;

/// Unique identifier for a path within a connection.
pub type PathId = u64;

/// Information about a network path.
#[derive(Debug, Clone)]
pub struct PathInfo {
    /// Unique path identifier.
    pub path_id: PathId,

    /// Local address for this path.
    pub local_addr: SocketAddr,

    /// Remote address for this path.
    pub peer_addr: SocketAddr,

    /// Current RTT estimate in microseconds.
    pub rtt_us: u64,

    /// Current congestion window in bytes.
    pub cwnd: u64,

    /// Current pacing rate in bytes per second.
    pub pacing_rate: u64,

    /// Bytes in flight on this path.
    pub bytes_in_flight: u64,

    /// Whether this path is currently active.
    pub is_active: bool,
}

/// Events related to path changes.
#[derive(Debug, Clone)]
pub enum PathEvent {
    /// A new path became available.
    Available(PathId),

    /// A path was suspended (temporarily unavailable).
    Suspended(PathId),

    /// A path was deleted.
    Deleted(PathId),

    /// Path quality changed significantly.
    QualityChanged(PathId),
}

/// Path management interface.
pub trait PathManager {
    /// Probe a new path to the given address.
    fn probe_path(&mut self, peer_addr: SocketAddr) -> Result<PathId, crate::Error>;

    /// Get information about a specific path.
    fn path_info(&self, path_id: PathId) -> Option<PathInfo>;

    /// Get all active paths.
    fn active_paths(&self) -> Vec<PathInfo>;

    /// Set the mode/priority for a path.
    fn set_path_mode(&mut self, path_id: PathId, mode: PathMode) -> Result<(), crate::Error>;

    /// Drain pending path events.
    fn drain_path_events(&mut self) -> Vec<PathEvent>;
}

/// Mode for a path (affects scheduling and congestion control).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathMode {
    /// Normal bidirectional path.
    Normal,

    /// Path primarily for sending.
    SendPrimary,

    /// Path primarily for receiving.
    RecvPrimary,

    /// Backup path (only used when primary fails).
    Backup,
}
