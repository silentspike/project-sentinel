//! #498 PR3 (4c) — the `BlockResolver` (V9): the single attach point through which
//! every CAS/chunk read path resolves a block that may be remote.
//!
//! A read path (cas blob, artifact chunk) calls the resolver on a **local miss**. The
//! resolver then, under **single-flight** (parallel reads of the same missing block do
//! exactly one pull) and a **negative-cache** (a holder that just failed is not re-asked
//! within a TTL), pulls the block by hash from a peer, verifies it, and durably stores it
//! locally (the `RemotePull` impl does the pull+verify+store, #498 4b). The read then
//! retries against the now-local store.
//!
//! Strangler (S3): wiring the resolver into a read path is **behavior-preserving for a
//! local hit** — it only adds a remote fallback on a miss, and only when a `RemotePull`
//! (cluster mode) is present. Single-node prod resolves every read locally, unchanged.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::artifact::ChunkHash;

/// Default negative-cache TTL: a block that failed to pull is not retried for this long.
const DEFAULT_NEGATIVE_TTL: Duration = Duration::from_secs(5);

/// Default pull-pin grace: a freshly-pulled block is GC-protected for this long, so the
/// read that triggered the pull can register its reference before GC sees it as zero-ref.
const DEFAULT_PIN_GRACE: Duration = Duration::from_secs(30);

/// Local existence check for a block (no pull). Lets the resolver re-check after waiting
/// on single-flight without coupling to a concrete store type (avoids an Arc cycle).
pub trait BlockStore: Send + Sync {
    fn has_blob(&self, hash: &[u8; 32]) -> bool;
    fn has_chunk(&self, hash: &ChunkHash) -> bool;
}

/// Pull a block by hash from a peer, **verify** it against its content id, and **durably
/// store** it locally (#498 4b `store_pulled_blob` / the chunk equivalent). Returns
/// `true` if the block is local afterwards. Sync — the impl bridges to the async
/// transport; it is never called while the resolver holds the inflight-map lock.
pub trait RemotePull: Send + Sync {
    /// Pull a blob by hash. The impl resolves the full `BlockRef` (size + holders) from
    /// the block map by hash itself — the read paths only know the hash.
    fn pull_blob(&self, hash: &[u8; 32]) -> bool;
    fn pull_chunk(&self, hash: &ChunkHash) -> bool;
}

/// What a CAS read path calls to make a missing **blob** local (object-safe hook, so
/// `CasStore` holds an `Arc<dyn BlobResolve>` without depending on the daemon).
pub trait BlobResolve: Send + Sync {
    fn ensure_blob(&self, hash: &[u8; 32]) -> bool;
}

/// What an artifact read path calls to make a missing **chunk** local.
pub trait ChunkResolve: Send + Sync {
    fn ensure_chunk(&self, hash: &ChunkHash) -> bool;
}

impl BlobResolve for BlockResolver {
    fn ensure_blob(&self, hash: &[u8; 32]) -> bool {
        BlockResolver::ensure_blob(self, hash)
    }
}

impl ChunkResolve for BlockResolver {
    fn ensure_chunk(&self, hash: &ChunkHash) -> bool {
        BlockResolver::ensure_chunk(self, hash)
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum Key {
    Blob([u8; 32]),
    Chunk(ChunkHash),
}

/// The V9 resolver: local-or-pull with single-flight + negative-cache.
pub struct BlockResolver {
    store: Arc<dyn BlockStore>,
    remote: Arc<dyn RemotePull>,
    /// Per-block single-flight gate: concurrent resolvers of the same key serialize on
    /// the same `Mutex`, so only the first does the pull; the rest re-check local.
    inflight: Mutex<HashMap<Key, Arc<Mutex<()>>>>,
    /// Recently-failed keys → when they failed (skip a re-pull within the TTL).
    negative: Mutex<HashMap<Key, Instant>>,
    negative_ttl: Duration,
    /// Pull-pin (V9): recently-pulled keys → when pulled. GC must not delete a block in
    /// this grace window (it was just fetched for an in-progress read).
    recently_pulled: Mutex<HashMap<Key, Instant>>,
    pin_grace: Duration,
}

impl BlockResolver {
    pub fn new(store: Arc<dyn BlockStore>, remote: Arc<dyn RemotePull>) -> Self {
        Self {
            store,
            remote,
            inflight: Mutex::new(HashMap::new()),
            negative: Mutex::new(HashMap::new()),
            negative_ttl: DEFAULT_NEGATIVE_TTL,
            recently_pulled: Mutex::new(HashMap::new()),
            pin_grace: DEFAULT_PIN_GRACE,
        }
    }

    /// Whether a blob was pulled recently enough to be pull-pinned (GC must keep it).
    pub fn is_blob_pull_pinned(&self, hash: &[u8; 32]) -> bool {
        self.is_pinned(&Key::Blob(*hash))
    }

    /// Whether a chunk was pulled recently enough to be pull-pinned (GC must keep it).
    pub fn is_chunk_pull_pinned(&self, hash: &ChunkHash) -> bool {
        self.is_pinned(&Key::Chunk(*hash))
    }

    fn is_pinned(&self, key: &Key) -> bool {
        let pins = self
            .recently_pulled
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        pins.get(key).is_some_and(|t| t.elapsed() < self.pin_grace)
    }

    #[cfg(test)]
    fn with_negative_ttl(mut self, ttl: Duration) -> Self {
        self.negative_ttl = ttl;
        self
    }

    /// Ensure a blob is available locally, pulling it if missing. Returns whether it is
    /// local afterwards. The read paths know only the hash; the `RemotePull` impl
    /// resolves the size/holders from the block map.
    pub fn ensure_blob(&self, hash: &[u8; 32]) -> bool {
        if self.store.has_blob(hash) {
            return true;
        }
        let store = Arc::clone(&self.store);
        let remote = Arc::clone(&self.remote);
        let h = *hash;
        self.guarded(Key::Blob(h), &move || store.has_blob(&h), &move || {
            remote.pull_blob(&h)
        })
    }

    /// Ensure a chunk is available locally, pulling it if missing.
    pub fn ensure_chunk(&self, hash: &ChunkHash) -> bool {
        if self.store.has_chunk(hash) {
            return true;
        }
        let store = Arc::clone(&self.store);
        let remote = Arc::clone(&self.remote);
        let h = *hash;
        self.guarded(Key::Chunk(h), &move || store.has_chunk(&h), &move || {
            remote.pull_chunk(&h)
        })
    }

    /// Single-flight + negative-cache around `pull`. `is_local` re-checks after the gate
    /// is taken (another thread may have pulled it while we waited).
    fn guarded(&self, key: Key, is_local: &dyn Fn() -> bool, pull: &dyn Fn() -> bool) -> bool {
        // Take (or create) the per-key gate, then release the map lock before locking it.
        let gate = {
            let mut map = self.inflight.lock().unwrap_or_else(|p| p.into_inner());
            Arc::clone(map.entry(key.clone()).or_default())
        };
        let _held = gate.lock().unwrap_or_else(|p| p.into_inner());

        let result = if is_local() {
            true // someone else pulled it while we waited on the gate
        } else if self.in_negative_cache(&key) {
            false // a recent failure — do not hammer the holder
        } else {
            let ok = pull();
            if ok {
                self.clear_negative(&key);
                self.pin(key.clone()); // pull-pin: GC must keep it during the read window
            } else {
                self.record_negative(key.clone());
            }
            ok
        };

        // Drop the gate from the map once no other waiter holds it (bounded growth).
        let mut map = self.inflight.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(g) = map.get(&key) {
            if Arc::strong_count(g) <= 2 {
                map.remove(&key);
            }
        }
        result
    }

    fn in_negative_cache(&self, key: &Key) -> bool {
        let neg = self.negative.lock().unwrap_or_else(|p| p.into_inner());
        neg.get(key)
            .is_some_and(|t| t.elapsed() < self.negative_ttl)
    }

    fn record_negative(&self, key: Key) {
        self.negative
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(key, Instant::now());
    }

    fn clear_negative(&self, key: &Key) {
        self.negative
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(key);
    }

    fn pin(&self, key: Key) {
        self.recently_pulled
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(key, Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Barrier;

    /// A store whose contents are flipped to "present" once a pull "succeeds".
    #[derive(Default)]
    struct MockState {
        present: Mutex<std::collections::HashSet<[u8; 32]>>,
        blob_pulls: AtomicUsize,
        pull_succeeds: bool,
    }

    struct MockStore(Arc<MockState>);
    impl BlockStore for MockStore {
        fn has_blob(&self, hash: &[u8; 32]) -> bool {
            self.0.present.lock().unwrap().contains(hash)
        }
        fn has_chunk(&self, _hash: &ChunkHash) -> bool {
            false
        }
    }

    struct MockRemote(Arc<MockState>);
    impl RemotePull for MockRemote {
        fn pull_blob(&self, hash: &[u8; 32]) -> bool {
            self.0.blob_pulls.fetch_add(1, Ordering::SeqCst);
            // Simulate a slow pull so concurrent callers pile up on the gate.
            std::thread::sleep(Duration::from_millis(20));
            if self.0.pull_succeeds {
                self.0.present.lock().unwrap().insert(*hash);
                true
            } else {
                false
            }
        }
        fn pull_chunk(&self, _hash: &ChunkHash) -> bool {
            false
        }
    }

    fn resolver(pull_succeeds: bool) -> (BlockResolver, Arc<MockState>) {
        let state = Arc::new(MockState {
            pull_succeeds,
            ..Default::default()
        });
        let r = BlockResolver::new(
            Arc::new(MockStore(Arc::clone(&state))),
            Arc::new(MockRemote(Arc::clone(&state))),
        );
        (r, state)
    }

    #[test]
    fn local_hit_short_circuits_without_a_pull() {
        let (r, state) = resolver(true);
        state.present.lock().unwrap().insert([1; 32]);
        assert!(r.ensure_blob(&[1; 32]));
        assert_eq!(
            state.blob_pulls.load(Ordering::SeqCst),
            0,
            "no pull on a local hit"
        );
    }

    #[test]
    fn miss_pulls_then_is_local() {
        let (r, state) = resolver(true);
        assert!(r.ensure_blob(&[2; 32]));
        assert_eq!(state.blob_pulls.load(Ordering::SeqCst), 1);
        // A second resolve is a local hit now — no second pull.
        assert!(r.ensure_blob(&[2; 32]));
        assert_eq!(state.blob_pulls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn single_flight_collapses_parallel_misses_to_one_pull() {
        let (r, state) = resolver(true);
        let r = Arc::new(r);
        let n = 8;
        let barrier = Arc::new(Barrier::new(n));
        let mut handles = Vec::new();
        for _ in 0..n {
            let r = Arc::clone(&r);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                r.ensure_blob(&[3; 32])
            }));
        }
        for h in handles {
            assert!(h.join().unwrap(), "every caller sees the block local");
        }
        assert_eq!(
            state.blob_pulls.load(Ordering::SeqCst),
            1,
            "single-flight: 8 parallel misses → exactly 1 pull"
        );
    }

    #[test]
    fn negative_cache_skips_a_re_pull_after_a_failure() {
        let (r, state) = resolver(false); // pulls always fail
        assert!(!r.ensure_blob(&[4; 32]));
        assert_eq!(state.blob_pulls.load(Ordering::SeqCst), 1);
        // Within the TTL, a second resolve does NOT hit the holder again.
        assert!(!r.ensure_blob(&[4; 32]));
        assert_eq!(
            state.blob_pulls.load(Ordering::SeqCst),
            1,
            "negative-cache: failed key not re-pulled within the TTL"
        );
    }

    #[test]
    fn negative_cache_expires_and_allows_a_retry() {
        let (r, state) = resolver(false);
        let r = r.with_negative_ttl(Duration::from_millis(10));
        assert!(!r.ensure_blob(&[5; 32]));
        assert_eq!(state.blob_pulls.load(Ordering::SeqCst), 1);
        std::thread::sleep(Duration::from_millis(25));
        assert!(!r.ensure_blob(&[5; 32]));
        assert_eq!(
            state.blob_pulls.load(Ordering::SeqCst),
            2,
            "after the TTL the key is retried"
        );
    }

    #[test]
    fn a_freshly_pulled_block_is_pull_pinned() {
        let (r, _state) = resolver(true);
        assert!(!r.is_blob_pull_pinned(&[6; 32]), "not pinned before a pull");
        assert!(r.ensure_blob(&[6; 32]));
        assert!(
            r.is_blob_pull_pinned(&[6; 32]),
            "pull-pinned after a pull — GC must keep it during the read window (V9)"
        );
        assert!(
            !r.is_blob_pull_pinned(&[99; 32]),
            "an unrelated block is not pinned"
        );
    }
}
