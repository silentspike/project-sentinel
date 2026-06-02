//! Event-Log CAS transport (#464).
//!
//! `sentinel-console-plane` already owns CDC chunking, zstd block storage, and
//! `HelloManifest -> Delta`. The event log additionally needs an ordered manifest:
//! without the append order a client can know which blocks are missing, but cannot
//! reassemble the immutable event stream.

use std::collections::HashSet;

use sentinel_console_plane::{BlockHash, ConsolePlane, Delta, HelloManifest};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct EventLogCasStats {
    pub event_count: usize,
    pub max_event_id: i64,
    pub full_state_bytes: u64,
    pub delta_transfer_bytes: u64,
    pub known_blocks: usize,
    pub total_blocks: usize,
    pub unique_blocks: usize,
    pub dedup_ratio: f64,
    pub savings_ratio: f64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct EventLogCasResponse {
    pub topic: String,
    /// Full append-order block manifest. Repeated hashes are intentional: the client appends the
    /// same decompressed block each time the hash appears.
    pub manifest: Vec<BlockHash>,
    pub delta: Delta,
    pub stats: EventLogCasStats,
}

#[derive(Default)]
pub struct EventLogCasPlane {
    plane: ConsolePlane,
    seen_ids: HashSet<i64>,
    max_event_id: i64,
    full_state_bytes: u64,
}

impl EventLogCasPlane {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.seen_ids.is_empty()
    }

    pub fn max_event_id(&self) -> i64 {
        self.max_event_id
    }

    pub fn event_count(&self) -> usize {
        self.seen_ids.len()
    }

    pub fn ingest_events(&mut self, events: &[serde_json::Value]) -> anyhow::Result<usize> {
        let mut inserted = 0;
        for event in events {
            let Some(id) = event.get("id").and_then(serde_json::Value::as_i64) else {
                continue;
            };
            if !self.seen_ids.insert(id) {
                continue;
            }
            let mut bytes = serde_json::to_vec(event)?;
            bytes.push(b'\n');
            self.full_state_bytes += bytes.len() as u64;
            self.plane.ingest(&bytes);
            self.max_event_id = self.max_event_id.max(id);
            inserted += 1;
        }
        Ok(inserted)
    }

    pub fn response_for(&self, hello: HelloManifest) -> EventLogCasResponse {
        let client_has: HashSet<BlockHash> = hello.have.into_iter().collect();
        let delta = self.plane.delta(&client_has);
        EventLogCasResponse {
            topic: "event_log_cas".to_string(),
            manifest: self.plane.log_manifest(),
            stats: EventLogCasStats {
                event_count: self.event_count(),
                max_event_id: self.max_event_id,
                full_state_bytes: self.full_state_bytes,
                delta_transfer_bytes: delta.transfer_bytes(),
                known_blocks: client_has.len(),
                total_blocks: self.plane.total_blocks(),
                unique_blocks: self.plane.unique_blocks(),
                dedup_ratio: self.plane.dedup_ratio(),
                savings_ratio: self.plane.savings_ratio(),
            },
            delta,
        }
    }

    pub fn stats_snapshot(&self) -> EventLogCasStats {
        EventLogCasStats {
            event_count: self.event_count(),
            max_event_id: self.max_event_id,
            full_state_bytes: self.full_state_bytes,
            delta_transfer_bytes: 0,
            known_blocks: 0,
            total_blocks: self.plane.total_blocks(),
            unique_blocks: self.plane.unique_blocks(),
            dedup_ratio: self.plane.dedup_ratio(),
            savings_ratio: self.plane.savings_ratio(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(id: i64, shared_len: usize) -> serde_json::Value {
        serde_json::json!({
            "shared": "A".repeat(shared_len),
            "id": id,
            "event_id": format!("evt-{id}"),
            "event_type": "agent_action_received",
            "aggregate_id": "agent-1",
            "payload": serde_json::json!({"content": format!("msg-{id}")}).to_string(),
            "correlation_id": "",
            "causation_id": null,
            "tick": id * 10,
            "timestamp_ms": 1_700_000_000_000_i64 + id,
            "compensation_type": "none",
        })
    }

    fn response_bytes(resp: &EventLogCasResponse) -> Vec<u8> {
        let mut compressed_by_hash = std::collections::HashMap::new();
        for (hash, bytes) in &resp.delta.blocks {
            compressed_by_hash.insert(*hash, bytes.clone());
        }
        let mut out = Vec::new();
        for hash in &resp.manifest {
            let compressed = compressed_by_hash
                .get(hash)
                .expect("test client has no prior cache, every manifest block is in delta");
            let raw = zstd::decode_all(compressed.as_slice()).expect("block decompresses");
            out.extend(raw);
        }
        out
    }

    #[test]
    fn event_log_cas_reassembles_ordered_ndjson() {
        let mut plane = EventLogCasPlane::new();
        plane
            .ingest_events(&[event(2, 1024), event(1, 1024), event(2, 1024)])
            .unwrap();

        let resp = plane.response_for(HelloManifest { have: vec![] });
        let raw = response_bytes(&resp);
        let lines = std::str::from_utf8(&raw)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();

        assert_eq!(resp.stats.event_count, 2);
        assert_eq!(lines[0]["id"], 2);
        assert_eq!(lines[1]["id"], 1);
    }

    #[test]
    fn partial_manifest_sends_only_missing_blocks() {
        let mut plane = EventLogCasPlane::new();
        let events = (1..=25).map(|id| event(id, 196 * 1024)).collect::<Vec<_>>();
        plane.ingest_events(&events).unwrap();

        let full = plane.response_for(HelloManifest { have: vec![] });
        let known_count = (full.delta.missing.len() * 9) / 10;
        let have = full
            .delta
            .missing
            .iter()
            .take(known_count)
            .copied()
            .collect();
        let partial = plane.response_for(HelloManifest { have });

        assert!(partial.stats.known_blocks >= known_count);
        assert!(
            partial.stats.delta_transfer_bytes * 5 < partial.stats.full_state_bytes,
            "CAS delta {} should be <20% of full raw state {}",
            partial.stats.delta_transfer_bytes,
            partial.stats.full_state_bytes
        );
    }
}
