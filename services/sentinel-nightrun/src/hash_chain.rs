//! Deterministic Hash Chain for Nightrun Replay verification.
//!
//! Builds a SHA-256 chain over event sequences:
//! `hash_n = SHA256(hash_{n-1} || event_id || payload || tick)`
//!
//! Enables replay verification: same seed + same events => identical final hash.

use sha2::{Digest, Sha256};

use sentinel_common::DomainEvent;

/// A SHA-256 hash chain over domain events.
#[derive(Debug, Clone)]
pub struct HashChain {
    current: [u8; 32],
    length: usize,
}

impl HashChain {
    /// Create a new hash chain with a deterministic seed.
    ///
    /// The initial hash is `SHA256(run_id || ":" || seed)`.
    pub fn new(seed: &str, run_id: &str) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(run_id.as_bytes());
        hasher.update(b":");
        hasher.update(seed.as_bytes());
        Self {
            current: hasher.finalize().into(),
            length: 0,
        }
    }

    /// Extend the chain with a domain event.
    ///
    /// `hash_n = SHA256(hash_{n-1} || event_id || payload || tick)`
    pub fn extend(&mut self, event: &DomainEvent) {
        let mut hasher = Sha256::new();
        hasher.update(self.current);
        hasher.update(event.event_id.as_bytes());
        hasher.update(event.payload.as_bytes());
        hasher.update(event.tick.to_le_bytes());
        self.current = hasher.finalize().into();
        self.length += 1;
    }

    /// Get the current hash as a hex-encoded string.
    pub fn current_hash(&self) -> String {
        hex_encode(&self.current)
    }

    /// Number of events in the chain.
    pub fn length(&self) -> usize {
        self.length
    }

    /// Verify that a sequence of events produces the expected final hash.
    pub fn verify(events: &[DomainEvent], seed: &str, run_id: &str, expected: &str) -> bool {
        let mut chain = Self::new(seed, run_id);
        for event in events {
            chain.extend(event);
        }
        chain.current_hash() == expected
    }

    /// Compute the final hash for a sequence of events (convenience).
    pub fn compute(events: &[DomainEvent], seed: &str, run_id: &str) -> String {
        let mut chain = Self::new(seed, run_id);
        for event in events {
            chain.extend(event);
        }
        chain.current_hash()
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_event(id: &str, payload: &str, tick: u64) -> DomainEvent {
        DomainEvent::new("test_event", "test-agent", payload, "corr-1", tick)
            .with_operation_id(id)
    }

    #[test]
    fn deterministic_same_inputs_same_hash() {
        let events = vec![
            test_event("op-1", r#"{"action":"move"}"#, 1),
            test_event("op-2", r#"{"action":"speak"}"#, 2),
        ];

        let hash_a = HashChain::compute(&events, "seed-42", "run-1");
        let hash_b = HashChain::compute(&events, "seed-42", "run-1");
        assert_eq!(hash_a, hash_b);
    }

    #[test]
    fn different_seed_different_hash() {
        let events = vec![test_event("op-1", r#"{"x":1}"#, 1)];

        let hash_a = HashChain::compute(&events, "seed-a", "run-1");
        let hash_b = HashChain::compute(&events, "seed-b", "run-1");
        assert_ne!(hash_a, hash_b);
    }

    #[test]
    fn different_run_id_different_hash() {
        let events = vec![test_event("op-1", r#"{"x":1}"#, 1)];

        let hash_a = HashChain::compute(&events, "seed", "run-a");
        let hash_b = HashChain::compute(&events, "seed", "run-b");
        assert_ne!(hash_a, hash_b);
    }

    #[test]
    fn extend_changes_hash() {
        let mut chain = HashChain::new("seed", "run-1");
        let before = chain.current_hash();

        chain.extend(&test_event("op-1", r#"{"x":1}"#, 1));
        let after = chain.current_hash();

        assert_ne!(before, after);
        assert_eq!(chain.length(), 1);
    }

    #[test]
    fn verify_ok() {
        let events = vec![
            test_event("op-1", r#"{"a":1}"#, 1),
            test_event("op-2", r#"{"b":2}"#, 2),
        ];
        let expected = HashChain::compute(&events, "seed", "run-1");
        assert!(HashChain::verify(&events, "seed", "run-1", &expected));
    }

    #[test]
    fn verify_tampered_event_fails() {
        let events = vec![
            test_event("op-1", r#"{"a":1}"#, 1),
            test_event("op-2", r#"{"b":2}"#, 2),
        ];
        let expected = HashChain::compute(&events, "seed", "run-1");

        // Tamper: change payload of second event
        let tampered = vec![
            test_event("op-1", r#"{"a":1}"#, 1),
            test_event("op-2", r#"{"b":999}"#, 2),
        ];
        assert!(!HashChain::verify(&tampered, "seed", "run-1", &expected));
    }

    #[test]
    fn empty_chain_has_seed_hash() {
        let chain = HashChain::new("seed", "run-1");
        assert_eq!(chain.length(), 0);
        assert_eq!(chain.current_hash().len(), 64); // SHA-256 hex
    }

    #[test]
    fn order_matters() {
        let e1 = test_event("op-1", r#"{"a":1}"#, 1);
        let e2 = test_event("op-2", r#"{"b":2}"#, 2);

        let hash_ab = HashChain::compute(&[e1.clone(), e2.clone()], "seed", "run");
        let hash_ba = HashChain::compute(&[e2, e1], "seed", "run");
        assert_ne!(hash_ab, hash_ba);
    }
}
