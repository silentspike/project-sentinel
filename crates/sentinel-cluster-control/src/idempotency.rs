//! Idempotency dedup for control RPCs (V5/V39).
//!
//! A re-sent `ControlEnvelope` (same `idempotency_key`) must produce a single
//! effect: the handler runs once, and the re-send returns the cached reply. This
//! is the **in-memory** cache for the Phase-3a0 skeleton; durable dedup across a
//! daemon restart is the ADR-3 redb `PROVISION_OPS`/cluster-meta concern.

use std::collections::HashMap;
use std::sync::Mutex;

/// A response cache keyed by `idempotency_key`. `R` is the cached reply type.
pub struct IdempotencyCache<R> {
    inner: Mutex<HashMap<String, R>>,
}

impl<R: Clone> IdempotencyCache<R> {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// The cached reply for `key`, if this key was already handled.
    pub fn get(&self, key: &str) -> Option<R> {
        self.inner
            .lock()
            .expect("idempotency lock")
            .get(key)
            .cloned()
    }

    /// Record the reply for `key`. First writer wins (a concurrent duplicate does
    /// not overwrite the original effect's reply).
    pub fn record(&self, key: &str, reply: R) {
        self.inner
            .lock()
            .expect("idempotency lock")
            .entry(key.to_string())
            .or_insert(reply);
    }

    /// Run `f` exactly once per `key`: return the cached reply on a re-send,
    /// otherwise compute, cache and return it.
    pub fn get_or_compute(&self, key: &str, f: impl FnOnce() -> R) -> (R, bool) {
        if let Some(cached) = self.get(key) {
            return (cached, true);
        }
        let reply = f();
        self.record(key, reply.clone());
        // Re-read in case of a race: the first writer's reply is authoritative.
        (self.get(key).unwrap_or(reply), false)
    }

    pub fn len(&self) -> usize {
        self.inner.lock().expect("idempotency lock").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<R: Clone> Default for IdempotencyCache<R> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_call_same_key_is_cached_handler_runs_once() {
        let cache: IdempotencyCache<u64> = IdempotencyCache::new();
        let mut runs = 0u64;
        let (a, cached_a) = cache.get_or_compute("k", || {
            runs += 1;
            42
        });
        assert_eq!(a, 42);
        assert!(!cached_a, "first call computes");
        let (b, cached_b) = cache.get_or_compute("k", || {
            runs += 1;
            99
        });
        assert_eq!(b, 42, "re-send returns the cached reply, not the new value");
        assert!(cached_b, "second call is a cache hit");
        assert_eq!(runs, 1, "handler ran exactly once (exactly-once effect)");
    }

    #[test]
    fn distinct_keys_each_compute() {
        let cache: IdempotencyCache<u64> = IdempotencyCache::new();
        let (a, _) = cache.get_or_compute("a", || 1);
        let (b, _) = cache.get_or_compute("b", || 2);
        assert_eq!((a, b), (1, 2));
        assert_eq!(cache.len(), 2);
    }
}
