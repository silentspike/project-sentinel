//! Idempotency dedup for control RPCs (V5/V39).
//!
//! A re-sent `ControlEnvelope` (same `idempotency_key`) must produce a single
//! effect: the handler runs once, and the re-send returns the cached reply. This
//! is the **in-memory** cache for the Phase-3a0 skeleton; durable dedup across a
//! daemon restart is the ADR-3 redb `PROVISION_OPS`/cluster-meta concern.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// A response cache keyed by `idempotency_key`. `R` is the cached reply type.
pub struct IdempotencyCache<R> {
    inner: Mutex<HashMap<String, Arc<OnceLock<R>>>>,
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
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(key)
            .and_then(|entry| entry.get().cloned())
    }

    /// Record the reply for `key`. First writer wins (a concurrent duplicate does
    /// not overwrite the original effect's reply).
    pub fn record(&self, key: &str, reply: R) {
        let entry = self.entry(key).0;
        let _ = entry.set(reply);
    }

    /// Run `f` exactly once per `key`: return the cached reply on a re-send,
    /// otherwise compute, cache and return it.
    pub fn get_or_compute(&self, key: &str, f: impl FnOnce() -> R) -> (R, bool) {
        let (entry, existed) = self.entry(key);
        (entry.get_or_init(f).clone(), existed)
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

    fn entry(&self, key: &str) -> (Arc<OnceLock<R>>, bool) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(entry) = inner.get(key) {
            return (Arc::clone(entry), true);
        }
        let entry = Arc::new(OnceLock::new());
        inner.insert(key.to_string(), Arc::clone(&entry));
        (entry, false)
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

    #[test]
    fn concurrent_duplicate_computes_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Barrier};

        let cache = Arc::new(IdempotencyCache::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let start = Arc::new(Barrier::new(3));
        let mut threads = Vec::new();
        for _ in 0..2 {
            let cache = Arc::clone(&cache);
            let calls = Arc::clone(&calls);
            let start = Arc::clone(&start);
            threads.push(std::thread::spawn(move || {
                start.wait();
                cache.get_or_compute("same", || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    42
                })
            }));
        }
        start.wait();
        let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
        assert_eq!(
            results.iter().map(|(value, _)| *value).collect::<Vec<_>>(),
            vec![42, 42]
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(results.iter().filter(|(_, cached)| *cached).count(), 1);
    }

    #[test]
    fn distinct_keys_do_not_serialize_effects() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{mpsc, Arc};
        use std::time::Duration;

        let cache = Arc::new(IdempotencyCache::new());
        let release = Arc::new(AtomicBool::new(false));
        let (entered_tx, entered_rx) = mpsc::channel();
        let mut threads = Vec::new();
        for key in ["a", "b"] {
            let cache = Arc::clone(&cache);
            let release = Arc::clone(&release);
            let entered_tx = entered_tx.clone();
            threads.push(std::thread::spawn(move || {
                cache.get_or_compute(key, || {
                    entered_tx.send(key).unwrap();
                    while !release.load(Ordering::Acquire) {
                        std::thread::yield_now();
                    }
                    key
                })
            }));
        }
        drop(entered_tx);
        let first = entered_rx.recv_timeout(Duration::from_secs(1));
        let second = entered_rx.recv_timeout(Duration::from_secs(1));
        release.store(true, Ordering::Release);
        for thread in threads {
            assert!(!thread.join().unwrap().1);
        }
        assert!(first.is_ok(), "first independent effect did not start");
        assert!(second.is_ok(), "distinct keys were serialized");
    }
}
