use std::collections::HashMap;

const AUTHORITATIVE_POLL_TIMEOUT_US: u64 = 5_000_000;

pub(crate) fn expire_inflight_polls(inflight_poll_ids: &mut HashMap<u16, u64>, now: u64) {
    if inflight_poll_ids.is_empty() {
        return;
    }
    let expire_before = now.saturating_sub(AUTHORITATIVE_POLL_TIMEOUT_US);
    let mut expired = Vec::new();
    for (id, sent_at) in inflight_poll_ids.iter() {
        if *sent_at <= expire_before {
            expired.push(*id);
        }
    }
    for id in expired {
        inflight_poll_ids.remove(&id);
    }
}
