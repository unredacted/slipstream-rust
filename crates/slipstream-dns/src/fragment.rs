//! QUIC packet fragmentation for DNS tunneling.
//!
//! QUIC RFC 9000 mandates Initial packets be ≥1200 bytes, but DNS qname encoding
//! has limited capacity (~140 bytes for short domains). This module provides
//! application-layer fragmentation to split large QUIC packets into multiple
//! DNS queries.

use std::collections::HashMap;
use std::time::Instant;

/// Magic byte to identify fragment packets (ASCII 'S' for Slipstream)
const FRAGMENT_MAGIC: u8 = 0x53;

/// Header size for fragment metadata: magic (1) + packet_id (2) + frag_num (1) + total (1)
pub const FRAGMENT_HEADER_SIZE: usize = 5;

/// Default timeout for incomplete fragment reassembly (5 seconds)
const FRAGMENT_TIMEOUT_SECS: u64 = 5;

/// Fragment a QUIC packet into multiple chunks for DNS encoding.
///
/// Each fragment contains:
/// - packet_id (2 bytes): Identifies the original packet
/// - frag_num (1 byte): 0-indexed fragment sequence number
/// - total (1 byte): Total number of fragments
/// - payload: QUIC packet data for this fragment
///
/// # Arguments
/// * `packet` - The QUIC packet data to fragment
/// * `packet_id` - Unique identifier for this packet (wrapping u16)
/// * `max_payload` - Maximum payload size per fragment (including header)
///
/// # Returns
/// Vector of fragment byte arrays ready for DNS encoding
pub fn fragment_packet(packet: &[u8], packet_id: u16, max_payload: usize) -> Vec<Vec<u8>> {
    if max_payload <= FRAGMENT_HEADER_SIZE {
        // Can't fit any data
        return vec![];
    }

    let chunk_size = max_payload - FRAGMENT_HEADER_SIZE;
    if chunk_size == 0 {
        return vec![];
    }

    // If packet fits in one fragment, just add header
    if packet.len() <= chunk_size {
        let mut frag = Vec::with_capacity(FRAGMENT_HEADER_SIZE + packet.len());
        frag.push(FRAGMENT_MAGIC);
        frag.extend_from_slice(&packet_id.to_be_bytes());
        frag.push(0); // frag_num
        frag.push(1); // total
        frag.extend_from_slice(packet);
        return vec![frag];
    }

    let chunks: Vec<_> = packet.chunks(chunk_size).collect();
    let total = chunks.len().min(255) as u8;

    chunks
        .iter()
        .enumerate()
        .take(255) // Max 255 fragments
        .map(|(i, chunk)| {
            let mut frag = Vec::with_capacity(FRAGMENT_HEADER_SIZE + chunk.len());
            frag.push(FRAGMENT_MAGIC);
            frag.extend_from_slice(&packet_id.to_be_bytes());
            frag.push(i as u8);
            frag.push(total);
            frag.extend_from_slice(chunk);
            frag
        })
        .collect()
}

/// Parse a fragment header.
///
/// # Returns
/// (packet_id, frag_num, total, payload) or None if not a valid fragment
pub fn parse_fragment(data: &[u8]) -> Option<(u16, u8, u8, &[u8])> {
    if data.len() < FRAGMENT_HEADER_SIZE {
        return None;
    }
    // Check magic byte
    if data[0] != FRAGMENT_MAGIC {
        return None;
    }
    let packet_id = u16::from_be_bytes([data[1], data[2]]);
    let frag_num = data[3];
    let total = data[4];
    let payload = &data[FRAGMENT_HEADER_SIZE..];
    Some((packet_id, frag_num, total, payload))
}

/// Check if data represents a fragmented packet (has our magic byte header).
pub fn is_fragmented(data: &[u8]) -> bool {
    if data.len() < FRAGMENT_HEADER_SIZE {
        return false;
    }
    // Check magic byte
    data[0] == FRAGMENT_MAGIC
}

/// Buffer for reassembling fragmented QUIC packets.
pub struct FragmentBuffer {
    /// Fragments indexed by packet_id
    fragments: HashMap<u16, FragmentEntry>,
    /// Maximum age for incomplete reassembly
    timeout_secs: u64,
}

struct FragmentEntry {
    /// Fragment data indexed by fragment number
    data: Vec<Option<Vec<u8>>>,
    /// Total expected fragments
    total: u8,
    /// When first fragment was received
    created: Instant,
    /// Count of received fragments
    received: u8,
}

impl Default for FragmentBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl FragmentBuffer {
    /// Create a new fragment buffer with default timeout.
    pub fn new() -> Self {
        Self {
            fragments: HashMap::new(),
            timeout_secs: FRAGMENT_TIMEOUT_SECS,
        }
    }

    /// Create a new fragment buffer with custom timeout.
    pub fn with_timeout(timeout_secs: u64) -> Self {
        Self {
            fragments: HashMap::new(),
            timeout_secs,
        }
    }

    /// Receive a fragment and return the reassembled packet if complete.
    ///
    /// # Arguments
    /// * `data` - Raw fragment data including header
    ///
    /// # Returns
    /// * `Some(packet)` if all fragments received and reassembly complete
    /// * `None` if waiting for more fragments or invalid data
    pub fn receive_fragment(&mut self, data: &[u8]) -> Option<Vec<u8>> {
        let (packet_id, frag_num, total, payload) = parse_fragment(data)?;

        if total == 0 || frag_num >= total {
            return None;
        }

        let entry = self
            .fragments
            .entry(packet_id)
            .or_insert_with(|| FragmentEntry {
                data: vec![None; total as usize],
                total,
                created: Instant::now(),
                received: 0,
            });

        // Verify consistent total
        if entry.total != total {
            return None;
        }

        // Store fragment if not already received
        let idx = frag_num as usize;
        if idx < entry.data.len() && entry.data[idx].is_none() {
            entry.data[idx] = Some(payload.to_vec());
            entry.received += 1;
        }

        // Check if all fragments received
        if entry.received == entry.total {
            // Reassemble
            let packet: Vec<u8> = entry
                .data
                .iter()
                .flat_map(|f| f.as_ref().unwrap().iter().cloned())
                .collect();
            self.fragments.remove(&packet_id);
            return Some(packet);
        }

        None
    }

    /// Clean up stale incomplete reassemblies.
    pub fn cleanup_stale(&mut self) {
        let timeout = std::time::Duration::from_secs(self.timeout_secs);
        self.fragments
            .retain(|_, entry| entry.created.elapsed() < timeout);
    }

    /// Number of pending incomplete reassemblies.
    pub fn pending_count(&self) -> usize {
        self.fragments.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragment_small_packet() {
        let data = b"hello";
        let fragments = fragment_packet(data, 42, 100);
        assert_eq!(fragments.len(), 1);

        let (packet_id, frag_num, total, payload) = parse_fragment(&fragments[0]).unwrap();
        assert_eq!(packet_id, 42);
        assert_eq!(frag_num, 0);
        assert_eq!(total, 1);
        assert_eq!(payload, b"hello");
    }

    #[test]
    fn fragment_large_packet() {
        let data: Vec<u8> = (0..100).collect();
        // 20 bytes per fragment = 4 header + 16 payload
        let fragments = fragment_packet(&data, 1, 20);

        // 100 bytes / 16 bytes per chunk = 7 fragments (6 full + 1 partial)
        assert_eq!(fragments.len(), 7);

        for (i, frag) in fragments.iter().enumerate() {
            let (packet_id, frag_num, total, _payload) = parse_fragment(frag).unwrap();
            assert_eq!(packet_id, 1);
            assert_eq!(frag_num, i as u8);
            assert_eq!(total, 7);
        }
    }

    #[test]
    fn reassemble_in_order() {
        let data: Vec<u8> = (0..100).collect();
        let fragments = fragment_packet(&data, 1, 20);

        let mut buffer = FragmentBuffer::new();
        for (i, frag) in fragments.iter().enumerate() {
            let result = buffer.receive_fragment(frag);
            if i < fragments.len() - 1 {
                assert!(result.is_none());
            } else {
                assert_eq!(result, Some(data.clone()));
            }
        }
    }

    #[test]
    fn reassemble_out_of_order() {
        let data: Vec<u8> = (0..100).collect();
        let mut fragments = fragment_packet(&data, 1, 20);
        fragments.reverse(); // Receive in reverse order

        let mut buffer = FragmentBuffer::new();
        for (i, frag) in fragments.iter().enumerate() {
            let result = buffer.receive_fragment(frag);
            if i < fragments.len() - 1 {
                assert!(result.is_none());
            } else {
                assert_eq!(result, Some(data.clone()));
            }
        }
    }

    #[test]
    fn multiple_packets() {
        let data1: Vec<u8> = (0..50).collect();
        let data2: Vec<u8> = (100..150).collect();
        let frags1 = fragment_packet(&data1, 1, 20);
        let frags2 = fragment_packet(&data2, 2, 20);

        let mut buffer = FragmentBuffer::new();

        // Interleave fragments
        let result1 = buffer.receive_fragment(&frags1[0]);
        assert!(result1.is_none());
        let result2 = buffer.receive_fragment(&frags2[0]);
        assert!(result2.is_none());

        // Complete packet 1
        for frag in frags1.iter().skip(1) {
            buffer.receive_fragment(frag);
        }
        // Last fragment should return assembled packet
        // (already processed above, check pending)

        // Complete packet 2
        for frag in frags2.iter().skip(1) {
            buffer.receive_fragment(frag);
        }

        assert_eq!(buffer.pending_count(), 0);
    }
}
