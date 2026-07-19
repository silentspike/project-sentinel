//! Process-local idempotency dedup for control RPCs.
//!
//! A key is scoped to the authenticated peer and RPC method. The first request
//! digest bound to that scope wins; reusing the operator key with another payload
//! is a conflict, never a cache hit. Entries are bounded by age and count.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use sentinel_common::NodeId;

pub const DEFAULT_IDEMPOTENCY_TTL: Duration = Duration::from_secs(5 * 60);
pub const DEFAULT_IDEMPOTENCY_CAPACITY: usize = 4_096;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IdempotencyScope {
    pub peer_node: NodeId,
    pub method: &'static str,
    pub idempotency_key: String,
}

impl IdempotencyScope {
    pub fn new(
        peer_node: NodeId,
        method: &'static str,
        idempotency_key: impl Into<String>,
    ) -> Self {
        Self {
            peer_node,
            method,
            idempotency_key: idempotency_key.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestDigest(pub [u8; 32]);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdempotencyOutcome<R> {
    Computed(R),
    Cached(R),
    DigestConflict,
    CapacityExhausted,
}

struct CacheEntry<R> {
    digest: RequestDigest,
    inserted_at: Instant,
    response: Arc<OnceLock<R>>,
}

pub struct IdempotencyCache<R> {
    inner: Mutex<HashMap<IdempotencyScope, CacheEntry<R>>>,
    ttl: Duration,
    capacity: usize,
}

impl<R: Clone> IdempotencyCache<R> {
    pub fn new() -> Self {
        Self::with_limits(DEFAULT_IDEMPOTENCY_TTL, DEFAULT_IDEMPOTENCY_CAPACITY)
    }

    pub fn with_limits(ttl: Duration, capacity: usize) -> Self {
        assert!(capacity > 0, "idempotency capacity must be non-zero");
        Self {
            inner: Mutex::new(HashMap::new()),
            ttl,
            capacity,
        }
    }

    pub fn get_or_compute(
        &self,
        scope: IdempotencyScope,
        digest: RequestDigest,
        f: impl FnOnce() -> R,
    ) -> IdempotencyOutcome<R> {
        let (response, existed) = {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let now = Instant::now();
            inner.retain(|_, entry| {
                entry.response.get().is_none()
                    || now.saturating_duration_since(entry.inserted_at) < self.ttl
            });

            if let Some(entry) = inner.get(&scope) {
                if entry.digest != digest {
                    return IdempotencyOutcome::DigestConflict;
                }
                (Arc::clone(&entry.response), true)
            } else {
                if inner.len() >= self.capacity {
                    let oldest_completed = inner
                        .iter()
                        .filter(|(_, entry)| entry.response.get().is_some())
                        .min_by_key(|(_, entry)| entry.inserted_at)
                        .map(|(key, _)| key.clone());
                    if let Some(key) = oldest_completed {
                        inner.remove(&key);
                    } else {
                        return IdempotencyOutcome::CapacityExhausted;
                    }
                }
                let response = Arc::new(OnceLock::new());
                inner.insert(
                    scope,
                    CacheEntry {
                        digest,
                        inserted_at: now,
                        response: Arc::clone(&response),
                    },
                );
                (response, false)
            }
        };

        let value = response.get_or_init(f).clone();
        if existed {
            IdempotencyOutcome::Cached(value)
        } else {
            IdempotencyOutcome::Computed(value)
        }
    }

    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
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

    fn scope(key: &str) -> IdempotencyScope {
        IdempotencyScope::new(NodeId::new(), "owner_commit", key)
    }

    #[test]
    fn identical_scoped_request_computes_once() {
        let cache = IdempotencyCache::new();
        let scope = scope("k");
        let digest = RequestDigest([1; 32]);
        let mut runs = 0;
        assert_eq!(
            cache.get_or_compute(scope.clone(), digest, || {
                runs += 1;
                42
            }),
            IdempotencyOutcome::Computed(42)
        );
        assert_eq!(
            cache.get_or_compute(scope, digest, || {
                runs += 1;
                99
            }),
            IdempotencyOutcome::Cached(42)
        );
        assert_eq!(runs, 1);
    }

    #[test]
    fn same_operator_key_with_another_digest_is_conflict() {
        let cache = IdempotencyCache::new();
        let scope = scope("k");
        assert_eq!(
            cache.get_or_compute(scope.clone(), RequestDigest([1; 32]), || 1),
            IdempotencyOutcome::Computed(1)
        );
        assert_eq!(
            cache.get_or_compute(scope, RequestDigest([2; 32]), || 2),
            IdempotencyOutcome::DigestConflict
        );
    }

    #[test]
    fn peer_and_method_are_part_of_the_scope() {
        let cache = IdempotencyCache::new();
        let peer = NodeId::new();
        let digest = RequestDigest([1; 32]);
        let a = IdempotencyScope::new(peer, "ref_query", "k");
        let b = IdempotencyScope::new(peer, "pin_query", "k");
        let c = IdempotencyScope::new(NodeId::new(), "ref_query", "k");
        assert!(matches!(
            cache.get_or_compute(a, digest, || 1),
            IdempotencyOutcome::Computed(1)
        ));
        assert!(matches!(
            cache.get_or_compute(b, digest, || 2),
            IdempotencyOutcome::Computed(2)
        ));
        assert!(matches!(
            cache.get_or_compute(c, digest, || 3),
            IdempotencyOutcome::Computed(3)
        ));
    }

    #[test]
    fn capacity_evicts_oldest_completed_entry() {
        let cache = IdempotencyCache::with_limits(Duration::from_secs(60), 2);
        for n in 0..3 {
            let result =
                cache.get_or_compute(scope(&format!("k{n}")), RequestDigest([n; 32]), || n);
            assert!(matches!(result, IdempotencyOutcome::Computed(_)));
        }
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn expired_completed_entry_is_recomputed() {
        let cache = IdempotencyCache::with_limits(Duration::ZERO, 2);
        let scope = scope("expires");
        let digest = RequestDigest([9; 32]);
        assert_eq!(
            cache.get_or_compute(scope.clone(), digest, || 1),
            IdempotencyOutcome::Computed(1)
        );
        assert_eq!(
            cache.get_or_compute(scope, digest, || 2),
            IdempotencyOutcome::Computed(2)
        );
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn concurrent_duplicate_computes_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Barrier};

        let cache = Arc::new(IdempotencyCache::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let start = Arc::new(Barrier::new(3));
        let scope = scope("same");
        let mut threads = Vec::new();
        for _ in 0..2 {
            let cache = Arc::clone(&cache);
            let calls = Arc::clone(&calls);
            let start = Arc::clone(&start);
            let scope = scope.clone();
            threads.push(std::thread::spawn(move || {
                start.wait();
                cache.get_or_compute(scope, RequestDigest([1; 32]), || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(20));
                    42
                })
            }));
        }
        start.wait();
        let results: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, IdempotencyOutcome::Cached(42)))
                .count(),
            1
        );
    }
}
