//! Deterministic Replay Engine for Nightrun.
//!
//! Loads events from the EventStore, rebuilds the hash chain,
//! and verifies against an expected final hash.
//! Replay is READ-ONLY — it never mutates events or creates new ones.

use anyhow::{bail, Context, Result};
use serde::Serialize;

use sentinel_common::{DomainEvent, DomainEventPayload};
use sentinel_limbo::EventStore;

use crate::hash_chain::HashChain;

/// Result of a replay verification.
#[derive(Debug, Clone, Serialize)]
pub struct ReplayResult {
    /// The run ID that was replayed.
    pub run_id: String,
    /// Number of events loaded from the EventStore for this run.
    pub events_loaded: usize,
    /// Number of events replayed.
    pub events_replayed: usize,
    /// Whether the hash chain matches the expected value.
    pub hash_chain_valid: bool,
    /// The computed final hash from replay.
    pub final_hash: String,
    /// The expected hash that was compared against.
    pub expected_hash: String,
}

/// Deterministic replay engine.
///
/// Reads events from an EventStore and verifies hash chain integrity.
/// Does NOT write or modify any events (read-only operation).
pub struct ReplayEngine<'a> {
    event_store: &'a EventStore,
}

impl<'a> ReplayEngine<'a> {
    /// Create a replay engine with a reference to an event store.
    pub fn new(event_store: &'a EventStore) -> Self {
        Self { event_store }
    }

    /// Replay all events for a given run and verify the hash chain.
    ///
    /// 1. Load all events correlated with `run_id`
    /// 2. Build hash chain from seed
    /// 3. Compare final hash with `expected_hash`
    pub fn replay(&self, run_id: &str, seed: &str, expected_hash: &str) -> Result<ReplayResult> {
        // Load events by correlation_id (run_id is used as correlation)
        let events = self
            .event_store
            .get_events_by_correlation(run_id, 100_000)
            .context("Failed to load events for replay")?;

        let (final_hash, events_replayed) = compute_replay_hash(&events, seed, run_id);
        let valid = final_hash == expected_hash;

        Ok(ReplayResult {
            run_id: run_id.to_string(),
            events_loaded: events.len(),
            events_replayed,
            hash_chain_valid: valid,
            final_hash,
            expected_hash: expected_hash.to_string(),
        })
    }

    /// Compute the hash chain for a run without verification.
    ///
    /// Useful for capturing the expected hash after an initial run.
    pub fn capture_hash(&self, run_id: &str, seed: &str) -> Result<(String, usize)> {
        let events = self
            .event_store
            .get_events_by_correlation(run_id, 100_000)
            .context("Failed to load events for hash capture")?;

        let (hash, count) = compute_replay_hash(&events, seed, run_id);
        Ok((hash, count))
    }

    /// Read the expected hash from the `nightrun_completed` event for a run.
    pub fn expected_hash_from_completed(&self, run_id: &str) -> Result<String> {
        let events = self
            .event_store
            .get_events_by_correlation(run_id, 100_000)
            .context("Failed to load events for expected hash extraction")?;

        for event in events.iter().rev() {
            if event.event_type != "nightrun_completed" {
                continue;
            }

            let payload: DomainEventPayload = serde_json::from_str(&event.payload)
                .context("Failed to parse nightrun_completed payload")?;
            if let DomainEventPayload::NightRunCompleted {
                hash_chain: Some(hash),
                ..
            } = payload
            {
                return Ok(hash);
            }
        }

        bail!("No nightrun_completed event with hash_chain found for run_id={run_id}")
    }

    /// Replay from a pre-loaded event list (for testing without EventStore).
    pub fn replay_from_events(
        events: &[DomainEvent],
        run_id: &str,
        seed: &str,
        expected_hash: &str,
    ) -> ReplayResult {
        let (final_hash, events_replayed) = compute_replay_hash(events, seed, run_id);
        let valid = final_hash == expected_hash;

        ReplayResult {
            run_id: run_id.to_string(),
            events_loaded: events.len(),
            events_replayed,
            hash_chain_valid: valid,
            final_hash,
            expected_hash: expected_hash.to_string(),
        }
    }
}

fn compute_replay_hash(events: &[DomainEvent], seed: &str, run_id: &str) -> (String, usize) {
    let mut chain = HashChain::new(seed, run_id);
    let mut events_replayed = 0usize;

    for event in events.iter().filter(|event| is_replay_chain_event(event)) {
        chain.extend(event);
        events_replayed += 1;
    }

    (chain.current_hash(), events_replayed)
}

fn is_replay_chain_event(event: &DomainEvent) -> bool {
    event.event_type != "nightrun_completed"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_event(id: &str, payload: &str, tick: u64) -> DomainEvent {
        DomainEvent::new("test_event", "run-1", payload, "run-1", tick).with_operation_id(id)
    }

    #[test]
    fn replay_from_events_matches() {
        let events = vec![
            test_event("op-1", r#"{"a":1}"#, 1),
            test_event("op-2", r#"{"b":2}"#, 2),
        ];
        let expected = HashChain::compute(&events, "seed-1", "run-1");

        let result = ReplayEngine::replay_from_events(&events, "run-1", "seed-1", &expected);

        assert!(result.hash_chain_valid);
        assert_eq!(result.events_replayed, 2);
        assert_eq!(result.final_hash, expected);
    }

    #[test]
    fn replay_detects_tampering() {
        let events = vec![
            test_event("op-1", r#"{"a":1}"#, 1),
            test_event("op-2", r#"{"b":2}"#, 2),
        ];
        let expected = HashChain::compute(&events, "seed-1", "run-1");

        // Tamper: different payload
        let tampered = vec![
            test_event("op-1", r#"{"a":1}"#, 1),
            test_event("op-2", r#"{"b":TAMPERED}"#, 2),
        ];

        let result = ReplayEngine::replay_from_events(&tampered, "run-1", "seed-1", &expected);

        assert!(!result.hash_chain_valid);
        assert_ne!(result.final_hash, expected);
    }

    #[test]
    fn replay_readonly_no_mutation() {
        let events = vec![
            test_event("op-1", r#"{"a":1}"#, 1),
            test_event("op-2", r#"{"b":2}"#, 2),
        ];
        let events_clone = events.clone();
        let expected = HashChain::compute(&events, "seed", "run-1");

        let _ = ReplayEngine::replay_from_events(&events, "run-1", "seed", &expected);

        // Events unchanged after replay
        assert_eq!(events.len(), events_clone.len());
        for (a, b) in events.iter().zip(events_clone.iter()) {
            assert_eq!(a.event_id, b.event_id);
            assert_eq!(a.payload, b.payload);
            assert_eq!(a.tick, b.tick);
        }
    }

    #[test]
    fn replay_empty_events() {
        let events: Vec<DomainEvent> = vec![];
        let expected = HashChain::compute(&events, "seed", "run-1");

        let result = ReplayEngine::replay_from_events(&events, "run-1", "seed", &expected);

        assert!(result.hash_chain_valid);
        assert_eq!(result.events_replayed, 0);
    }
}
