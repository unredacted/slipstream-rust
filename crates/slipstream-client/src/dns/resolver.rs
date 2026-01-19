#![allow(dead_code)]

use crate::error::ClientError;
use crate::pacing::{PacingBudgetSnapshot, PacingPollBudget};
use slipstream_core::{resolve_host_port, ResolverMode, ResolverSpec};
use std::collections::HashMap;
use std::net::{SocketAddr, SocketAddrV6};
use tracing::warn;

use super::debug::DebugMetrics;

pub(crate) struct ResolverState {
    pub(crate) addr: SocketAddr,
    pub(crate) mode: ResolverMode,
    pub(crate) added: bool,
    /// tquic path ID for multipath support
    pub(crate) path_id_tquic: Option<u64>,
    pub(crate) probe_attempts: u32,
    pub(crate) next_probe_at: u64,
    pub(crate) pending_polls: usize,
    pub(crate) inflight_poll_ids: HashMap<u16, u64>,
    pub(crate) pacing_budget: Option<PacingPollBudget>,
    pub(crate) last_pacing_snapshot: Option<PacingBudgetSnapshot>,
    pub(crate) debug: DebugMetrics,
}

impl ResolverState {
    pub(crate) fn label(&self) -> String {
        format!(
            "path_id_tquic={:?} resolver={} mode={:?}",
            self.path_id_tquic, self.addr, self.mode
        )
    }
}

pub(crate) fn resolve_resolvers(
    resolvers: &[ResolverSpec],
    mtu: u32,
    debug_poll: bool,
) -> Result<Vec<ResolverState>, ClientError> {
    let mut resolved = Vec::with_capacity(resolvers.len());
    let mut seen = HashMap::new();
    for (idx, resolver) in resolvers.iter().enumerate() {
        let addr = resolve_host_port(&resolver.resolver)
            .map_err(|err| ClientError::new(err.to_string()))?;
        let addr = normalize_dual_stack_addr(addr);
        if let Some(existing_mode) = seen.get(&addr) {
            return Err(ClientError::new(format!(
                "Duplicate resolver address {} (modes: {:?} and {:?})",
                addr, existing_mode, resolver.mode
            )));
        }
        seen.insert(addr, resolver.mode);
        let is_primary = idx == 0;
        resolved.push(ResolverState {
            addr,
            mode: resolver.mode,
            added: is_primary,
            path_id_tquic: if is_primary { Some(0) } else { None },
            probe_attempts: 0,
            next_probe_at: 0,
            pending_polls: 0,
            inflight_poll_ids: HashMap::new(),
            pacing_budget: match resolver.mode {
                ResolverMode::Authoritative => Some(PacingPollBudget::new(mtu)),
                ResolverMode::Recursive => None,
            },
            last_pacing_snapshot: None,
            debug: DebugMetrics::new(debug_poll),
        });
    }
    Ok(resolved)
}

pub(crate) fn reset_resolver_path(resolver: &mut ResolverState) {
    warn!(
        "Path for resolver {} became unavailable; resetting state",
        resolver.addr
    );
    resolver.added = false;
    resolver.path_id_tquic = None;
    resolver.pending_polls = 0;
    resolver.inflight_poll_ids.clear();
    resolver.last_pacing_snapshot = None;
    resolver.probe_attempts = 0;
    resolver.next_probe_at = 0;
}

pub(crate) fn normalize_dual_stack_addr(addr: SocketAddr) -> SocketAddr {
    match addr {
        SocketAddr::V4(v4) => {
            SocketAddr::V6(SocketAddrV6::new(v4.ip().to_ipv6_mapped(), v4.port(), 0, 0))
        }
        SocketAddr::V6(v6) => SocketAddr::V6(v6),
    }
}

#[cfg(test)]
mod tests {
    use super::resolve_resolvers;
    use slipstream_core::{AddressFamily, HostPort, ResolverMode, ResolverSpec};

    #[test]
    fn rejects_duplicate_resolver_addr() {
        let resolvers = vec![
            ResolverSpec {
                resolver: HostPort {
                    host: "127.0.0.1".to_string(),
                    port: 8853,
                    family: AddressFamily::V4,
                },
                mode: ResolverMode::Recursive,
            },
            ResolverSpec {
                resolver: HostPort {
                    host: "127.0.0.1".to_string(),
                    port: 8853,
                    family: AddressFamily::V4,
                },
                mode: ResolverMode::Authoritative,
            },
        ];

        match resolve_resolvers(&resolvers, 900, false) {
            Ok(_) => panic!("expected duplicate resolver error"),
            Err(err) => assert!(err.to_string().contains("Duplicate resolver address")),
        }
    }
}
