//! L1 RAM cache for decompressed chunk data with anti-pollution admission.
//!
//! Caches decompressed chunk data to avoid redundant segment reads + zstd
//! decompression. Uses a two-hit admission policy: chunks are only cached
//! after being read at least twice (prevents scan pollution).
//!
//! Eviction is FIFO-based: when the cache exceeds its size limit, the
//! oldest entries are evicted until under budget.

use std::collections::{HashMap, VecDeque};

use crate::artifact::ChunkHash;

/// Default L1 cache capacity: 64 MB of decompressed chunk data.
pub const DEFAULT_CACHE_BYTES: usize = 64 * 1024 * 1024;

/// L1 chunk cache with two-hit admission policy.
pub struct ChunkCache {
    /// Cached decompressed chunk data.
    entries: HashMap<ChunkHash, Vec<u8>>,
    /// FIFO eviction order (oldest first).
    order: VecDeque<ChunkHash>,
    /// Current total size of cached data in bytes.
    current_bytes: usize,
    /// Maximum allowed cache size in bytes.
    max_bytes: usize,
    /// Access counter for anti-pollution admission.
    /// Tracks hashes that have been seen but not yet cached.
    seen: HashMap<ChunkHash, u8>,
    /// Stats
    hits: u64,
    misses: u64,
}

/// Cache statistics snapshot.
#[derive(Debug, Clone)]
pub struct CacheStats {
    pub entries: usize,
    pub current_bytes: usize,
    pub max_bytes: usize,
    pub hits: u64,
    pub misses: u64,
}

impl ChunkCache {
    /// Create a new cache with the given max size in bytes.
    pub fn new(max_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            current_bytes: 0,
            max_bytes,
            seen: HashMap::new(),
            hits: 0,
            misses: 0,
        }
    }

    /// Try to get a cached chunk. Returns None on miss.
    pub fn get(&mut self, hash: &ChunkHash) -> Option<&[u8]> {
        if let Some(data) = self.entries.get(hash) {
            self.hits += 1;
            Some(data.as_slice())
        } else {
            self.misses += 1;
            // Record the access for admission policy
            let count = self.seen.entry(*hash).or_insert(0);
            *count = count.saturating_add(1);
            None
        }
    }

    /// Insert a chunk into the cache (subject to admission policy).
    /// The chunk is only admitted if it has been seen at least once before
    /// (two-hit admission: first read = record, second read = cache).
    pub fn insert(&mut self, hash: ChunkHash, data: Vec<u8>) {
        // Skip if already cached
        if self.entries.contains_key(&hash) {
            return;
        }

        // Anti-pollution: only admit after 2+ accesses
        let seen_count = self.seen.get(&hash).copied().unwrap_or(0);
        if seen_count < 1 {
            return; // First read: just record it, don't cache
        }

        let data_len = data.len();

        // Don't cache chunks larger than 25% of the cache (prevents single huge entries)
        if data_len > self.max_bytes / 4 {
            return;
        }

        // Evict until there's room
        while self.current_bytes + data_len > self.max_bytes {
            if !self.evict_one() {
                return; // Cache is empty but chunk still doesn't fit
            }
        }

        self.current_bytes += data_len;
        self.entries.insert(hash, data);
        self.order.push_back(hash);
        self.seen.remove(&hash); // No longer tracking — it's cached now
    }

    /// Get cache statistics.
    pub fn stats(&self) -> CacheStats {
        CacheStats {
            entries: self.entries.len(),
            current_bytes: self.current_bytes,
            max_bytes: self.max_bytes,
            hits: self.hits,
            misses: self.misses,
        }
    }

    /// Evict one entry (FIFO: oldest first). Returns false if cache is empty.
    fn evict_one(&mut self) -> bool {
        while let Some(hash) = self.order.pop_front() {
            if let Some(data) = self.entries.remove(&hash) {
                self.current_bytes -= data.len();
                return true;
            }
            // Hash was already removed (shouldn't happen, but be defensive)
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_hash(byte: u8) -> ChunkHash {
        [byte; 16]
    }

    #[test]
    fn miss_then_hit() {
        let mut cache = ChunkCache::new(1024);
        let hash = make_hash(0xAA);
        let data = vec![0xAA; 100];

        // First access: miss + record
        assert!(cache.get(&hash).is_none());

        // Insert is allowed now (seen_count = 1)
        cache.insert(hash, data.clone());

        // Second access: hit
        assert_eq!(cache.get(&hash).unwrap(), &data);

        let stats = cache.stats();
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.misses, 1);
        assert_eq!(stats.entries, 1);
        assert_eq!(stats.current_bytes, 100);
    }

    #[test]
    fn anti_pollution_first_read_not_cached() {
        let mut cache = ChunkCache::new(1024);
        let hash = make_hash(0xBB);

        // Insert without prior access: should be rejected
        cache.insert(hash, vec![0xBB; 100]);
        assert!(cache.get(&hash).is_none());
        assert_eq!(cache.stats().entries, 0);
    }

    #[test]
    fn eviction_fifo() {
        // Cache holds 200 bytes; chunks are 40 bytes each (< 25% of 800 = 200 oversized limit).
        // Use 800 bytes cache so 200/4=200 threshold isn't hit. Actually simpler:
        // use cache = 200, chunk size = 40 each (40 < 200/4=50), so 5 fit at capacity.
        let mut cache = ChunkCache::new(200);

        let h1 = make_hash(1);
        let h2 = make_hash(2);
        let h3 = make_hash(3);

        // Access each once (record for admission)
        cache.get(&h1);
        cache.get(&h2);
        cache.get(&h3);

        // Insert h1 and h2 (40 bytes each = 80 total, under capacity)
        cache.insert(h1, vec![1; 40]);
        cache.insert(h2, vec![2; 40]);
        assert_eq!(cache.stats().entries, 2);
        assert_eq!(cache.stats().current_bytes, 80);

        // Fill to capacity: 5 * 40 = 200
        let h4 = make_hash(4);
        let h5 = make_hash(5);
        cache.get(&h4);
        cache.get(&h5);
        cache.insert(h4, vec![4; 40]);
        cache.insert(h5, vec![5; 40]);
        // now: h1(40) + h2(40) + h4(40) + h5(40) = 160

        // Insert h3: fits without eviction (160 + 40 = 200 = capacity)
        cache.insert(h3, vec![3; 40]);
        assert_eq!(cache.stats().entries, 5);
        assert_eq!(cache.stats().current_bytes, 200);

        // Insert one more to trigger eviction of h1 (FIFO oldest)
        let h6 = make_hash(6);
        cache.get(&h6);
        cache.insert(h6, vec![6; 40]);
        assert_eq!(cache.stats().entries, 5); // h1 evicted
        assert!(!cache.entries.contains_key(&h1)); // FIFO: h1 was oldest
        assert!(cache.entries.contains_key(&h2));
        assert!(cache.entries.contains_key(&h6));
    }

    #[test]
    fn oversized_chunk_rejected() {
        let mut cache = ChunkCache::new(400);
        let hash = make_hash(0xFF);

        // Record access
        cache.get(&hash);

        // 200 bytes > 400/4 = 100 → rejected
        cache.insert(hash, vec![0xFF; 200]);
        assert_eq!(cache.stats().entries, 0);
    }
}
