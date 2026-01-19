//! tquic-based path management for the client runtime.
//!
//! This module provides path management functionality equivalent to
//! `runtime/path.rs` but using slipstream-quic instead of picoquic FFI.

use crate::dns::{normalize_dual_stack_addr, ResolverState};
use crate::error::ClientError;
use slipstream_ffi::ResolverMode;
use slipstream_quic::multipath::PathManager;
use slipstream_quic::ClientConnection;
use std::net::SocketAddr;

const AUTHORITATIVE_LOOP_MULTIPLIER: usize = 4;

/// Apply path mode settings to a resolver path via tquic.
pub(crate) fn apply_path_mode_tquic(
    conn: &mut ClientConnection,
    resolver: &mut ResolverState,
) -> Result<(), ClientError> {
    if !refresh_resolver_path_tquic(conn, resolver) {
        return Ok(());
    }

    // tquic doesn't have per-path mode settings like picoquic
    // The multipath scheduling algorithm handles this at the config level
    // For now, we track the mode but don't apply specific per-path settings

    resolver.added = true;
    Ok(())
}

/// Refresh resolver path information from tquic connection.
pub(crate) fn refresh_resolver_path_tquic(
    conn: &mut ClientConnection,
    resolver: &mut ResolverState,
) -> bool {
    // Check if the path is valid by trying to get path info
    if let Some(path_id) = resolver.path_id_tquic {
        if let Some(_info) = conn.path_info(path_id) {
            return true;
        }
    }
    // Path not available yet
    false
}

/// Fetch path quality from tquic connection.
pub(crate) fn fetch_path_quality_tquic(
    conn: &mut ClientConnection,
    resolver: &ResolverState,
) -> PathQuality {
    if let Some(path_id) = resolver.path_id_tquic {
        if let Some(info) = conn.path_info(path_id) {
            return PathQuality {
                rtt: info.rtt_us,
                cwin: info.cwnd,
                bytes_in_transit: info.bytes_in_flight,
                pacing_rate: info.pacing_rate,
            };
        }
    }

    // Fallback to connection-level stats
    PathQuality {
        rtt: conn.rtt(),
        cwin: conn.cwnd(),
        bytes_in_transit: 0,
        pacing_rate: 0,
    }
}

/// Path quality metrics.
#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub(crate) struct PathQuality {
    pub rtt: u64,
    pub cwin: u64,
    pub bytes_in_transit: u64,
    pub pacing_rate: u64,
}

/// Drain path events from the tquic connection and update resolver state.
pub(crate) fn drain_path_events_tquic(
    conn: &mut ClientConnection,
    resolvers: &mut [ResolverState],
) {
    let events = conn.drain_path_events();
    if events.is_empty() {
        return;
    }

    for event in events {
        match event {
            slipstream_quic::multipath::PathEvent::Available(path_id) => {
                // Find resolver by checking which one this path might belong to
                // In tquic, we need to query the connection for path addresses
                // For now, mark the first unassigned resolver as having this path
                for resolver in resolvers.iter_mut() {
                    if resolver.path_id_tquic.is_none() && !resolver.added {
                        resolver.path_id_tquic = Some(path_id);
                        resolver.added = true;
                        break;
                    }
                }
            }
            slipstream_quic::multipath::PathEvent::Deleted(path_id) => {
                if let Some(resolver) = find_resolver_by_path_id_mut(resolvers, path_id) {
                    reset_resolver_path_tquic(resolver);
                }
            }
            _ => {}
        }
    }
}

/// Reset resolver path state.
pub(crate) fn reset_resolver_path_tquic(resolver: &mut ResolverState) {
    resolver.path_id_tquic = None;
    resolver.added = false;
}

/// Calculate total loop burst based on resolver modes.
pub(crate) fn loop_burst_total(resolvers: &[ResolverState], base: usize) -> usize {
    resolvers.iter().fold(0usize, |acc, resolver| {
        acc.saturating_add(base.saturating_mul(path_loop_multiplier(resolver.mode)))
    })
}

/// Calculate max poll burst for a path.
#[allow(dead_code)]
pub(crate) fn path_poll_burst_max(resolver: &ResolverState) -> usize {
    64usize.saturating_mul(path_loop_multiplier(resolver.mode))
}

fn path_loop_multiplier(mode: ResolverMode) -> usize {
    match mode {
        ResolverMode::Authoritative => AUTHORITATIVE_LOOP_MULTIPLIER,
        ResolverMode::Recursive => 1,
    }
}

/// Find resolver by socket address.
pub(crate) fn find_resolver_by_addr_mut(
    resolvers: &mut [ResolverState],
    addr: SocketAddr,
) -> Option<&mut ResolverState> {
    let addr = normalize_dual_stack_addr(addr);
    resolvers.iter_mut().find(|resolver| resolver.addr == addr)
}

/// Find resolver by tquic path ID.
fn find_resolver_by_path_id_mut(
    resolvers: &mut [ResolverState],
    path_id: u64,
) -> Option<&mut ResolverState> {
    resolvers
        .iter_mut()
        .find(|resolver| resolver.path_id_tquic == Some(path_id))
}
