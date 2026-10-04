use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{hash_map::RandomState, VecDeque};
use std::hash::BuildHasher;
use std::io::Write;
use std::sync::{Mutex, OnceLock};

use crate::{WorkflowError, WorkflowErrorCode};

pub(crate) fn canonical_sha256<T: Serialize>(
    domain: &'static str,
    value: &T,
) -> Result<String, WorkflowError> {
    let mut writer = DigestWriter::new(domain);
    serde_json::to_writer(&mut writer, value).map_err(|_| {
        WorkflowError::new(
            WorkflowErrorCode::InvalidInput,
            false,
            "canonical workflow serialization failed",
        )
    })?;
    Ok(writer.finish())
}

const DIGEST_BUFFER_BYTES: usize = 8192;

struct DigestWriter {
    hasher: Sha256,
    buffer: [u8; DIGEST_BUFFER_BYTES],
    used: usize,
}

impl DigestWriter {
    fn new(domain: &str) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(domain.as_bytes());
        hasher.update([0]);
        Self {
            hasher,
            buffer: [0; DIGEST_BUFFER_BYTES],
            used: 0,
        }
    }

    fn append(&mut self, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            if self.used == 0 && bytes.len() >= DIGEST_BUFFER_BYTES {
                self.hasher.update(bytes);
                return;
            }
            let count = bytes.len().min(DIGEST_BUFFER_BYTES - self.used);
            self.buffer[self.used..self.used + count].copy_from_slice(&bytes[..count]);
            self.used += count;
            bytes = &bytes[count..];
            if self.used == DIGEST_BUFFER_BYTES {
                self.hasher.update(self.buffer.as_slice());
                self.used = 0;
            }
        }
    }

    fn finish(mut self) -> String {
        self.hasher.update(&self.buffer[..self.used]);
        hex_sha256(&self.hasher.finalize())
    }
}

impl Write for DigestWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.append(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.hasher.update(&self.buffer[..self.used]);
        self.used = 0;
        Ok(())
    }
}

struct DecimalByte {
    bytes: [u8; 4],
    len: usize,
}

const fn decimal_bytes() -> [DecimalByte; 256] {
    let mut values = [const {
        DecimalByte {
            bytes: [0; 4],
            len: 0,
        }
    }; 256];
    let mut index = 0;
    while index < values.len() {
        let value = index as u8;
        values[index].bytes[0] = b',';
        if value >= 100 {
            values[index].bytes[1] = b'0' + value / 100;
            values[index].bytes[2] = b'0' + (value / 10) % 10;
            values[index].bytes[3] = b'0' + value % 10;
            values[index].len = 4;
        } else if value >= 10 {
            values[index].bytes[1] = b'0' + value / 10;
            values[index].bytes[2] = b'0' + value % 10;
            values[index].len = 3;
        } else {
            values[index].bytes[1] = b'0' + value;
            values[index].len = 2;
        }
        index += 1;
    }
    values
}

const DECIMAL_BYTES: [DecimalByte; 256] = decimal_bytes();

// Persisted byte digests hash the JSON numeric array, not the raw bytes.
// Preserve that contract without allocating or formatting the expanded array.
pub(crate) fn canonical_bytes_sha256(domain: &'static str, bytes: &[u8]) -> String {
    static CACHE: OnceLock<ContentDigestCache> = OnceLock::new();
    CACHE
        .get_or_init(ContentDigestCache::new)
        .digest(domain, bytes)
}

fn uncached_bytes_sha256(domain: &'static str, bytes: &[u8]) -> String {
    let mut writer = DigestWriter::new(domain);
    writer.append(b"[");
    if let Some((first, rest)) = bytes.split_first() {
        let decimal = &DECIMAL_BYTES[usize::from(*first)];
        writer.append(&decimal.bytes[1..decimal.len]);
        for byte in rest {
            let decimal = &DECIMAL_BYTES[usize::from(*byte)];
            writer.append(&decimal.bytes[..decimal.len]);
        }
    }
    writer.append(b"]");
    writer.finish()
}

const CONTENT_DIGEST_SHARDS: usize = 8;
const CONTENT_DIGEST_SHARD_BYTES: usize = 4 * 1024 * 1024;
const CONTENT_DIGEST_SHARD_ENTRIES: usize = 256;
const CONTENT_DIGEST_MAX_PAYLOAD: usize = 1024 * 1024;

struct ContentDigestEntry {
    fingerprint: u64,
    domain: &'static str,
    bytes: Box<[u8]>,
    digest: String,
}

impl ContentDigestEntry {
    fn cost(&self) -> usize {
        Self::payload_cost(self.bytes.len())
    }

    fn payload_cost(len: usize) -> usize {
        len + 64 + std::mem::size_of::<Self>()
    }
}

#[derive(Default)]
struct ContentDigestShard {
    entries: VecDeque<ContentDigestEntry>,
    bytes: usize,
}

impl ContentDigestShard {
    fn lookup(&self, fingerprint: u64, domain: &str, bytes: &[u8]) -> Option<String> {
        self.entries
            .iter()
            .find(|entry| {
                entry.fingerprint == fingerprint
                    && entry.domain == domain
                    && entry.bytes.as_ref() == bytes
            })
            .map(|entry| entry.digest.clone())
    }

    fn admit(&mut self, fingerprint: u64, domain: &'static str, bytes: &[u8], digest: &str) {
        if self.lookup(fingerprint, domain, bytes).is_some() {
            return;
        }
        let cost = ContentDigestEntry::payload_cost(bytes.len());
        while self.entries.len() >= CONTENT_DIGEST_SHARD_ENTRIES
            || self.bytes + cost > CONTENT_DIGEST_SHARD_BYTES
        {
            let Some(entry) = self.entries.pop_front() else {
                return;
            };
            self.bytes -= entry.cost();
        }
        self.entries.push_back(ContentDigestEntry {
            fingerprint,
            domain,
            bytes: bytes.into(),
            digest: digest.to_owned(),
        });
        self.bytes += cost;
    }
}

struct ContentDigestCache {
    fingerprints: RandomState,
    shards: [Mutex<ContentDigestShard>; CONTENT_DIGEST_SHARDS],
    #[cfg(test)]
    computations: std::sync::atomic::AtomicUsize,
}

impl ContentDigestCache {
    fn new() -> Self {
        Self {
            fingerprints: RandomState::new(),
            shards: std::array::from_fn(|_| Mutex::new(ContentDigestShard::default())),
            #[cfg(test)]
            computations: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn digest(&self, domain: &'static str, bytes: &[u8]) -> String {
        if bytes.len() > CONTENT_DIGEST_MAX_PAYLOAD {
            return self.compute(domain, bytes);
        }
        self.digest_at(self.fingerprints.hash_one((domain, bytes)), domain, bytes)
    }

    fn digest_at(&self, fingerprint: u64, domain: &'static str, bytes: &[u8]) -> String {
        let shard = &self.shards[(fingerprint % CONTENT_DIGEST_SHARDS as u64) as usize];
        if let Ok(cache) = shard.lock() {
            if let Some(digest) = cache.lookup(fingerprint, domain, bytes) {
                return digest;
            }
        }
        // The fingerprint is only a lookup hint. Reuse requires the complete
        // domain and bytes; currentness and authorization remain caller checks.
        // Hash misses outside the lock; recheck before retaining one shared copy.
        let digest = self.compute(domain, bytes);
        if bytes.len() <= CONTENT_DIGEST_MAX_PAYLOAD {
            if let Ok(mut cache) = shard.lock() {
                cache.admit(fingerprint, domain, bytes, &digest);
            }
        }
        digest
    }

    fn compute(&self, domain: &'static str, bytes: &[u8]) -> String {
        #[cfg(test)]
        self.computations
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        uncached_bytes_sha256(domain, bytes)
    }
}

pub(crate) fn serialized_json_size<T: Serialize>(value: &T) -> Result<usize, serde_json::Error> {
    struct Counter(usize);
    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .ok_or_else(|| std::io::Error::other("serialized workflow size overflow"))?;
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    serde_json::to_writer(&mut counter, value)?;
    Ok(counter.0)
}

pub(crate) fn derive_principal_authority_digest(
    principal_generation: u64,
    credential_digest: &[u8; 32],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"sentinel.workflow.principal-authority.v1\0");
    hasher.update(principal_generation.to_be_bytes());
    hasher.update(credential_digest);
    let digest = hasher.finalize();
    hex_sha256(&digest)
}

pub(crate) fn validate_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

pub(crate) fn constant_time_eq(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.bytes()
        .zip(right.bytes())
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn hex_sha256(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legacy_digest<T: Serialize>(domain: &'static str, value: &T) -> String {
        let mut hasher = Sha256::new();
        hasher.update(domain.as_bytes());
        hasher.update([0]);
        hasher.update(serde_json::to_vec(value).unwrap());
        hex_sha256(&hasher.finalize())
    }

    fn computations(cache: &ContentDigestCache) -> usize {
        cache
            .computations
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    #[test]
    fn content_digest_reuses_exact_bytes_from_a_different_allocation() {
        let cache = ContentDigestCache::new();
        let payload = b"immutable journal payload";
        let first = cache.digest("journal.v1", payload);
        let separately_owned = payload.to_vec();
        assert_ne!(payload.as_ptr(), separately_owned.as_ptr());
        let second = cache.digest("journal.v1", &separately_owned);
        assert_eq!(first, legacy_digest("journal.v1", &payload.as_slice()));
        assert_eq!(second, first);
        assert_eq!(computations(&cache), 1);
    }

    #[test]
    fn content_digest_collision_and_changed_domain_never_reuse_a_proof() {
        let cache = ContentDigestCache::new();
        for (domain, payload) in [
            ("first", &b"same"[..]),
            ("first", &b"edit"[..]),
            ("second", &b"same"[..]),
            ("first", &b"same-longer"[..]),
            ("first", &b""[..]),
        ] {
            assert_eq!(
                cache.digest_at(0, domain, payload),
                legacy_digest(domain, &payload)
            );
        }
        assert_eq!(computations(&cache), 5);
        assert_eq!(
            cache.digest_at(0, "first", b"same"),
            legacy_digest("first", &b"same".as_slice())
        );
        assert_eq!(computations(&cache), 5);
    }

    #[test]
    fn content_digest_retains_owned_bytes_not_the_callers_mutable_buffer() {
        let cache = ContentDigestCache::new();
        let mut payload = b"old".to_vec();
        let old = cache.digest_at(0, "journal", &payload);
        payload.copy_from_slice(b"new");
        let new = cache.digest_at(0, "journal", &payload);
        assert_ne!(old, new);
        assert_eq!(new, legacy_digest("journal", &payload));
        assert_eq!(cache.digest_at(0, "journal", b"old"), old);
        assert_eq!(computations(&cache), 2);
    }

    #[test]
    fn content_digest_eviction_and_node_limit_only_cause_recomputation() {
        let cache = ContentDigestCache::new();
        for index in 0..=CONTENT_DIGEST_SHARD_ENTRIES {
            let payload = index.to_le_bytes();
            assert_eq!(
                cache.digest_at(0, "bounded", &payload),
                legacy_digest("bounded", &payload.as_slice())
            );
        }
        {
            let shard = cache.shards[0].lock().unwrap();
            assert_eq!(shard.entries.len(), CONTENT_DIGEST_SHARD_ENTRIES);
            assert!(shard.bytes <= CONTENT_DIGEST_SHARD_BYTES);
            assert!(shard.lookup(0, "bounded", &0_usize.to_le_bytes()).is_none());
        }
        assert_eq!(
            cache.digest_at(0, "bounded", &0_usize.to_le_bytes()),
            legacy_digest("bounded", &0_usize.to_le_bytes().as_slice())
        );
        assert_eq!(computations(&cache), CONTENT_DIGEST_SHARD_ENTRIES + 2);
    }

    #[test]
    fn content_digest_byte_limit_and_oversized_payloads_are_bounded() {
        let cache = ContentDigestCache::new();
        for value in 0..8 {
            let payload = vec![value; CONTENT_DIGEST_MAX_PAYLOAD];
            assert_eq!(
                cache.digest_at(0, "bounded", &payload),
                legacy_digest("bounded", &payload)
            );
            let shard = cache.shards[0].lock().unwrap();
            assert!(shard.bytes <= CONTENT_DIGEST_SHARD_BYTES);
            assert_eq!(
                shard.bytes,
                shard
                    .entries
                    .iter()
                    .map(ContentDigestEntry::cost)
                    .sum::<usize>()
            );
        }
        let before = computations(&cache);
        let oversized = vec![42; CONTENT_DIGEST_MAX_PAYLOAD + 1];
        for _ in 0..2 {
            assert_eq!(
                cache.digest("bounded", &oversized),
                legacy_digest("bounded", &oversized)
            );
        }
        assert_eq!(computations(&cache), before + 2);
        assert!(cache.shards.iter().all(|shard| shard
            .lock()
            .unwrap()
            .entries
            .iter()
            .all(|entry| entry.bytes.len() <= CONTENT_DIGEST_MAX_PAYLOAD)));
    }

    #[test]
    fn content_digest_poisoned_shard_recomputes_without_a_workflow_failure() {
        let cache = ContentDigestCache::new();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = cache.shards[0].lock().unwrap();
            panic!("injected cache poison");
        }));
        assert!(result.is_err());
        for _ in 0..2 {
            assert_eq!(
                cache.digest_at(0, "journal", b"safe"),
                legacy_digest("journal", &b"safe".as_slice())
            );
        }
        assert_eq!(computations(&cache), 2);
    }

    #[test]
    fn content_digest_concurrent_readers_keep_one_exact_shared_payload() {
        let cache = ContentDigestCache::new();
        std::thread::scope(|scope| {
            for value in 0..8 {
                let cache = &cache;
                scope.spawn(move || {
                    let shared = vec![42; 4096];
                    let distinct = vec![value; 4096];
                    for _ in 0..16 {
                        for payload in [&shared, &distinct] {
                            assert_eq!(
                                cache.digest_at(0, "journal", payload),
                                legacy_digest("journal", payload)
                            );
                        }
                    }
                });
            }
        });
        let shard = cache.shards[0].lock().unwrap();
        assert_eq!(shard.entries.len(), 9);
        assert_eq!(
            shard.bytes,
            shard
                .entries
                .iter()
                .map(ContentDigestEntry::cost)
                .sum::<usize>()
        );
        assert_eq!(
            shard
                .entries
                .iter()
                .filter(|entry| entry.bytes.as_ref() == vec![42; 4096])
                .count(),
            1
        );
    }

    #[test]
    fn streamed_canonical_digest_preserves_legacy_json_and_domain() {
        let value = serde_json::json!({
            "control": "\u{0}\n\r\t\\\"",
            "unicode": "\u{e4}\u{1f642}",
            "numbers": [0, -42, u64::MAX, 1.25, 1e-30],
            "nested": {"null": null, "bool": true, "empty": []},
            "large": "x".repeat(DIGEST_BUFFER_BYTES * 3 + 1),
        });
        for domain in ["test.workflow.first.v1", "test.workflow.second.v1"] {
            assert_eq!(
                canonical_sha256(domain, &value).unwrap(),
                legacy_digest(domain, &value)
            );
        }
        assert_eq!(
            serialized_json_size(&value).unwrap(),
            serde_json::to_vec(&value).unwrap().len()
        );
        assert_ne!(
            canonical_sha256("first", &value).unwrap(),
            canonical_sha256("second", &value).unwrap()
        );
    }

    #[test]
    fn streamed_byte_digest_preserves_every_decimal_value_and_buffer_boundary() {
        let all: Vec<u8> = (0..=u8::MAX).collect();
        let domains = [
            "sentinel.workflow.company-entity-row.v1",
            "sentinel.workflow.company-event-payload.v1",
            "sentinel.workflow.company-operation-response.v1",
            "sentinel.workflow.company-projection-row.v1",
        ];
        for domain in domains {
            for len in [
                0, 1, 2, 9, 10, 99, 100, 255, 256, 2047, 2048, 2049, 8191, 8192, 8193, 65537,
            ] {
                let bytes: Vec<u8> = all.iter().copied().cycle().take(len).collect();
                assert_eq!(
                    canonical_bytes_sha256(domain, &bytes),
                    legacy_digest(domain, &bytes)
                );
                assert_eq!(
                    canonical_bytes_sha256(domain, &bytes),
                    canonical_sha256(domain, &bytes).unwrap()
                );
                assert_eq!(
                    serialized_json_size(&bytes).unwrap(),
                    serde_json::to_vec(&bytes).unwrap().len()
                );
            }
        }
        for byte in all {
            assert_eq!(
                canonical_bytes_sha256("single", &[byte]),
                legacy_digest("single", &[byte])
            );
        }
    }

    #[test]
    fn digest_writer_handles_fragmentation_flush_and_empty_writes() {
        let bytes = vec![b'a'; DIGEST_BUFFER_BYTES * 4 + 17];
        let mut expected = Sha256::new();
        expected.update(b"writer\0");
        expected.update(&bytes);
        for size in [1, 3, 8191, 8192, 8193, bytes.len()] {
            let mut writer = DigestWriter::new("writer");
            writer.write_all(&[]).unwrap();
            for chunk in bytes.chunks(size) {
                writer.write_all(chunk).unwrap();
                writer.flush().unwrap();
            }
            assert_eq!(writer.finish(), hex_sha256(&expected.clone().finalize()));
        }
    }

    #[test]
    fn streamed_canonical_digest_rejects_serialization_failure() {
        struct Invalid;
        impl Serialize for Invalid {
            fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("invalid test value"))
            }
        }
        assert!(canonical_sha256("invalid", &Invalid).is_err());
        assert!(serialized_json_size(&Invalid).is_err());
    }
}
