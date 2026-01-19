mod debug;
mod poll;
mod resolver;

pub(crate) use poll::expire_inflight_polls;
pub(crate) use resolver::{normalize_dual_stack_addr, resolve_resolvers, ResolverState};
