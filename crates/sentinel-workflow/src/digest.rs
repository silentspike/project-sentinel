use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{hash_map::RandomState, VecDeque};
use std::hash::BuildHasher;
use std::io::Write;
use std::sync::{Mutex, OnceLock};

use crate::{WorkflowError, WorkflowErrorCode};

#[cfg(test)]
const MAX_DIGEST_DIAGNOSTIC_DOMAINS: usize = 64;

#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct DigestCost {
    pub(crate) calls: u64,
    pub(crate) cache_hits: u64,
    pub(crate) failures: u64,
    pub(crate) serialized_bytes: u64,
    // Bytes actually passed to SHA-256; a failed serializer may leave a buffer unflushed.
    pub(crate) hashed_bytes: u64,
    pub(crate) elapsed: std::time::Duration,
}

#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct DigestDiagnostic {
    // None aggregates domains beyond the bounded inventory, without retaining names.
    pub(crate) domain: Option<&'static str>,
    pub(crate) canonical: DigestCost,
    pub(crate) byte_array: DigestCost,
}

#[cfg(test)]
#[derive(Default)]
struct DigestDiagnostics {
    records: Vec<DigestDiagnostic>,
    overflow: Option<DigestDiagnostic>,
}

#[cfg(test)]
thread_local! {
    static DIGEST_DIAGNOSTICS: std::cell::RefCell<Option<DigestDiagnostics>> = const {
        std::cell::RefCell::new(None)
    };
}

#[cfg(test)]
struct DigestDiagnosticsScope {
    previous: Option<DigestDiagnostics>,
    active: bool,
}

#[cfg(test)]
impl DigestDiagnosticsScope {
    fn enter() -> Option<Self> {
        DIGEST_DIAGNOSTICS
            .try_with(|slot| {
                let mut slot = slot.try_borrow_mut().ok()?;
                Some(Self {
                    previous: slot.replace(DigestDiagnostics::default()),
                    active: true,
                })
            })
            .ok()
            .flatten()
    }

    fn restore(&mut self) -> Option<DigestDiagnostics> {
        if !self.active {
            return None;
        }
        self.active = false;
        DIGEST_DIAGNOSTICS
            .try_with(|slot| {
                let mut slot = slot.try_borrow_mut().ok()?;
                std::mem::replace(&mut *slot, self.previous.take())
            })
            .ok()
            .flatten()
    }
}

#[cfg(test)]
impl Drop for DigestDiagnosticsScope {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

/// Thread-local, isolated nested observations of serialization and hashing work.
/// Serialized bytes exclude the domain/NUL prefix; hashed bytes include it.
/// Elapsed costs are inclusive and include test instrumentation, not authority clocks.
/// Canonical calls include serialized cache hits; byte-array calls count only computations.
#[cfg(test)]
pub(crate) fn with_digest_diagnostics<T>(action: impl FnOnce() -> T) -> (T, Vec<DigestDiagnostic>) {
    let Some(mut scope) = DigestDiagnosticsScope::enter() else {
        return (action(), Vec::new());
    };
    let result = action();
    let mut diagnostics = scope.restore().unwrap_or_default();
    if let Some(overflow) = diagnostics.overflow {
        diagnostics.records.push(overflow);
    }
    (result, diagnostics.records)
}

#[cfg(test)]
enum DigestKind {
    Canonical,
    ByteArray,
}

#[cfg(test)]
struct DigestObservation {
    domain: &'static str,
    kind: DigestKind,
    started: std::time::Instant,
    serialized_bytes: u64,
    hashed_bytes: u64,
    cache_hit: bool,
    succeeded: bool,
}

#[cfg(test)]
impl DigestObservation {
    fn start(domain: &'static str, kind: DigestKind) -> Option<Self> {
        let active = DIGEST_DIAGNOSTICS
            .try_with(|slot| slot.try_borrow().is_ok_and(|slot| slot.is_some()))
            .unwrap_or(false);
        active.then(|| Self {
            domain,
            kind,
            started: std::time::Instant::now(),
            serialized_bytes: 0,
            hashed_bytes: u64::try_from(domain.len())
                .unwrap_or(u64::MAX)
                .saturating_add(1),
            cache_hit: false,
            succeeded: false,
        })
    }
}

#[cfg(test)]
impl Drop for DigestObservation {
    fn drop(&mut self) {
        let elapsed = self.started.elapsed();
        let _ = DIGEST_DIAGNOSTICS.try_with(|slot| {
            let Ok(mut slot) = slot.try_borrow_mut() else {
                return;
            };
            let Some(diagnostics) = slot.as_mut() else {
                return;
            };
            let record = if let Some(index) = diagnostics
                .records
                .iter()
                .position(|record| record.domain == Some(self.domain))
            {
                diagnostics.records.get_mut(index)
            } else if diagnostics.records.len() < MAX_DIGEST_DIAGNOSTIC_DOMAINS {
                diagnostics.records.push(DigestDiagnostic {
                    domain: Some(self.domain),
                    ..DigestDiagnostic::default()
                });
                diagnostics.records.last_mut()
            } else {
                Some(diagnostics.overflow.get_or_insert_default())
            };
            let Some(record) = record else {
                return;
            };
            let cost = match self.kind {
                DigestKind::Canonical => &mut record.canonical,
                DigestKind::ByteArray => &mut record.byte_array,
            };
            cost.calls = cost.calls.saturating_add(1);
            cost.cache_hits = cost.cache_hits.saturating_add(u64::from(self.cache_hit));
            cost.failures = cost.failures.saturating_add(u64::from(!self.succeeded));
            cost.serialized_bytes = cost.serialized_bytes.saturating_add(self.serialized_bytes);
            cost.hashed_bytes = cost.hashed_bytes.saturating_add(self.hashed_bytes);
            cost.elapsed = cost.elapsed.saturating_add(elapsed);
        });
    }
}

#[cfg(test)]
thread_local! {
    static CANONICAL_DIGEST_CACHE_ENABLED: std::cell::Cell<bool> = const {
        std::cell::Cell::new(true)
    };
}

#[cfg(test)]
pub(crate) fn with_canonical_digest_cache_enabled<T>(
    enabled: bool,
    action: impl FnOnce() -> T,
) -> T {
    struct Restore(Option<bool>);
    impl Drop for Restore {
        fn drop(&mut self) {
            if let Some(previous) = self.0 {
                let _ = CANONICAL_DIGEST_CACHE_ENABLED.try_with(|slot| slot.set(previous));
            }
        }
    }
    let _restore = Restore(
        CANONICAL_DIGEST_CACHE_ENABLED
            .try_with(|slot| slot.replace(enabled))
            .ok(),
    );
    action()
}

pub(crate) fn canonical_sha256<T: Serialize>(
    domain: &'static str,
    value: &T,
) -> Result<String, WorkflowError> {
    canonical_sha256_with_cache(domain, value, content_digest_cache())
}

fn canonical_sha256_with_cache<T: Serialize>(
    domain: &'static str,
    value: &T,
    cache: &ContentDigestCache,
) -> Result<String, WorkflowError> {
    #[cfg(test)]
    if !CANONICAL_DIGEST_CACHE_ENABLED
        .try_with(|slot| slot.get())
        .unwrap_or(false)
    {
        return canonical_sha256_streamed(domain, value);
    }
    let mut writer = CanonicalDigestWriter::new(domain);
    serde_json::to_writer(&mut writer, value).map_err(|_| {
        WorkflowError::new(
            WorkflowErrorCode::InvalidInput,
            false,
            "canonical workflow serialization failed",
        )
    })?;
    Ok(writer.finish(cache))
}

#[cfg(test)]
fn canonical_sha256_streamed<T: Serialize>(
    domain: &'static str,
    value: &T,
) -> Result<String, WorkflowError> {
    #[cfg(test)]
    let diagnostic = DigestObservation::start(domain, DigestKind::Canonical);
    let mut writer = DigestWriter::new(domain);
    #[cfg(test)]
    {
        writer.diagnostic = diagnostic;
    }
    serde_json::to_writer(&mut writer, value).map_err(|_| {
        WorkflowError::new(
            WorkflowErrorCode::InvalidInput,
            false,
            "canonical workflow serialization failed",
        )
    })?;
    Ok(writer.finish())
}

struct CanonicalDigestWriter {
    domain: &'static str,
    buffered: Vec<u8>,
    streaming: Option<DigestWriter>,
    #[cfg(test)]
    diagnostic: Option<DigestObservation>,
}

impl CanonicalDigestWriter {
    fn new(domain: &'static str) -> Self {
        #[cfg(test)]
        let diagnostic =
            DigestObservation::start(domain, DigestKind::Canonical).map(|mut diagnostic| {
                diagnostic.hashed_bytes = 0;
                diagnostic
            });
        Self {
            domain,
            buffered: Vec::new(),
            streaming: None,
            #[cfg(test)]
            diagnostic,
        }
    }

    fn start_streaming(&mut self) {
        let mut writer = DigestWriter::new(self.domain);
        #[cfg(test)]
        {
            writer.diagnostic = self.diagnostic.take().map(|mut diagnostic| {
                // Replay buffered bytes into the original stream exactly once.
                diagnostic.serialized_bytes = 0;
                diagnostic.hashed_bytes = u64::try_from(self.domain.len())
                    .unwrap_or(u64::MAX)
                    .saturating_add(1);
                diagnostic
            });
        }
        writer.append(&std::mem::take(&mut self.buffered));
        self.streaming = Some(writer);
    }

    fn finish(mut self, cache: &ContentDigestCache) -> String {
        if let Some(writer) = self.streaming.take() {
            return writer.finish();
        }
        let (digest, cache_hit) =
            cache.digest_encoded(DigestEncoding::CanonicalJson, self.domain, &self.buffered);
        #[cfg(test)]
        if let Some(diagnostic) = &mut self.diagnostic {
            diagnostic.cache_hit = cache_hit;
            diagnostic.succeeded = true;
            if !cache_hit {
                diagnostic.hashed_bytes = u64::try_from(self.buffered.len())
                    .unwrap_or(u64::MAX)
                    .saturating_add(u64::try_from(self.domain.len()).unwrap_or(u64::MAX))
                    .saturating_add(1);
            }
        }
        #[cfg(not(test))]
        let _ = cache_hit;
        digest
    }
}

impl Write for CanonicalDigestWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if let Some(writer) = &mut self.streaming {
            return writer.write(bytes);
        }
        let required = self.buffered.len().checked_add(bytes.len());
        if let Some(required) =
            required.filter(|size| *size <= CONTENT_DIGEST_CANONICAL_MAX_PAYLOAD)
        {
            if self.buffered.capacity() >= required || {
                let capacity = required
                    .max(DIGEST_BUFFER_BYTES)
                    .next_power_of_two()
                    .min(CONTENT_DIGEST_CANONICAL_MAX_PAYLOAD);
                self.buffered
                    .try_reserve_exact(capacity - self.buffered.len())
                    .is_ok()
            } {
                self.buffered.extend_from_slice(bytes);
                #[cfg(test)]
                if let Some(diagnostic) = &mut self.diagnostic {
                    diagnostic.serialized_bytes = diagnostic
                        .serialized_bytes
                        .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
                }
                return Ok(bytes.len());
            }
        }
        // Overflow/allocation failure changes retention, never canonical bytes or errors.
        self.start_streaming();
        self.write(bytes)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if let Some(writer) = &mut self.streaming {
            writer.flush()?;
        }
        Ok(())
    }
}

const DIGEST_BUFFER_BYTES: usize = 8192;

struct DigestWriter {
    hasher: Sha256,
    buffer: [u8; DIGEST_BUFFER_BYTES],
    used: usize,
    #[cfg(test)]
    diagnostic: Option<DigestObservation>,
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
            #[cfg(test)]
            diagnostic: None,
        }
    }

    fn append(&mut self, mut bytes: &[u8]) {
        #[cfg(test)]
        if let Some(diagnostic) = &mut self.diagnostic {
            diagnostic.serialized_bytes = diagnostic
                .serialized_bytes
                .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        }
        while !bytes.is_empty() {
            if self.used == 0 && bytes.len() >= DIGEST_BUFFER_BYTES {
                self.hasher.update(bytes);
                #[cfg(test)]
                self.record_hashed_bytes(bytes.len());
                return;
            }
            let count = bytes.len().min(DIGEST_BUFFER_BYTES - self.used);
            self.buffer[self.used..self.used + count].copy_from_slice(&bytes[..count]);
            self.used += count;
            bytes = &bytes[count..];
            if self.used == DIGEST_BUFFER_BYTES {
                self.hasher.update(self.buffer.as_slice());
                #[cfg(test)]
                self.record_hashed_bytes(DIGEST_BUFFER_BYTES);
                self.used = 0;
            }
        }
    }

    fn finish(mut self) -> String {
        self.hasher.update(&self.buffer[..self.used]);
        #[cfg(test)]
        self.record_hashed_bytes(self.used);
        let digest = hex_sha256(&self.hasher.finalize());
        #[cfg(test)]
        if let Some(diagnostic) = &mut self.diagnostic {
            diagnostic.succeeded = true;
        }
        digest
    }

    #[cfg(test)]
    fn record_hashed_bytes(&mut self, count: usize) {
        if let Some(diagnostic) = &mut self.diagnostic {
            diagnostic.hashed_bytes = diagnostic
                .hashed_bytes
                .saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
        }
    }
}

impl Write for DigestWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.append(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.hasher.update(&self.buffer[..self.used]);
        #[cfg(test)]
        self.record_hashed_bytes(self.used);
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
    content_digest_cache().digest(domain, bytes)
}

fn content_digest_cache() -> &'static ContentDigestCache {
    static CACHE: OnceLock<ContentDigestCache> = OnceLock::new();
    CACHE.get_or_init(ContentDigestCache::new)
}

fn uncached_bytes_sha256(domain: &'static str, bytes: &[u8]) -> String {
    #[cfg(test)]
    let diagnostic = DigestObservation::start(domain, DigestKind::ByteArray);
    let mut writer = DigestWriter::new(domain);
    #[cfg(test)]
    {
        writer.diagnostic = diagnostic;
    }
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
const CONTENT_DIGEST_CANONICAL_SHARD_BYTES: usize = 1024 * 1024;
const CONTENT_DIGEST_CANONICAL_SHARD_ENTRIES: usize = 32;
const CONTENT_DIGEST_CANONICAL_MAX_PAYLOAD: usize =
    CONTENT_DIGEST_CANONICAL_SHARD_BYTES.saturating_sub(ContentDigestEntry::payload_cost(0));

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum DigestEncoding {
    NumericByteArray,
    CanonicalJson,
}

impl DigestEncoding {
    fn max_payload(self) -> usize {
        match self {
            Self::NumericByteArray => CONTENT_DIGEST_MAX_PAYLOAD,
            Self::CanonicalJson => CONTENT_DIGEST_CANONICAL_MAX_PAYLOAD,
        }
    }
}

struct ContentDigestEntry {
    fingerprint: u64,
    encoding: DigestEncoding,
    domain: &'static str,
    bytes: Box<[u8]>,
    digest: String,
}

impl ContentDigestEntry {
    fn cost(&self) -> usize {
        Self::payload_cost(self.bytes.len())
    }

    const fn payload_cost(len: usize) -> usize {
        len + 64 + std::mem::size_of::<Self>()
    }
}

#[derive(Default)]
struct ContentDigestShard {
    entries: VecDeque<ContentDigestEntry>,
    bytes: usize,
}

impl ContentDigestShard {
    fn lookup_encoded(
        &self,
        fingerprint: u64,
        encoding: DigestEncoding,
        domain: &str,
        bytes: &[u8],
    ) -> Option<String> {
        self.entries
            .iter()
            .find(|entry| {
                entry.fingerprint == fingerprint
                    && entry.encoding == encoding
                    && entry.domain == domain
                    && entry.bytes.len() == bytes.len()
                    && entry.bytes.as_ref() == bytes
            })
            .map(|entry| entry.digest.clone())
    }

    #[cfg(test)]
    fn lookup(&self, fingerprint: u64, domain: &str, bytes: &[u8]) -> Option<String> {
        self.lookup_encoded(fingerprint, DigestEncoding::NumericByteArray, domain, bytes)
    }

    fn admit(
        &mut self,
        fingerprint: u64,
        encoding: DigestEncoding,
        domain: &'static str,
        bytes: &[u8],
        digest: &str,
    ) {
        if self
            .lookup_encoded(fingerprint, encoding, domain, bytes)
            .is_some()
        {
            return;
        }
        let cost = ContentDigestEntry::payload_cost(bytes.len());
        let (mut canonical_count, mut canonical_bytes) = self
            .entries
            .iter()
            .filter(|entry| entry.encoding == DigestEncoding::CanonicalJson)
            .fold((0_usize, 0_usize), |(count, bytes), entry| {
                (count + 1, bytes + entry.cost())
            });
        if encoding == DigestEncoding::CanonicalJson
            && (cost > CONTENT_DIGEST_CANONICAL_SHARD_BYTES
                || self.entries.len() - canonical_count >= CONTENT_DIGEST_SHARD_ENTRIES
                || self.bytes - canonical_bytes + cost > CONTENT_DIGEST_SHARD_BYTES)
        {
            // JSON may use spare capacity, never displace a retained numeric payload.
            return;
        }
        while self.entries.len() >= CONTENT_DIGEST_SHARD_ENTRIES
            || self.bytes + cost > CONTENT_DIGEST_SHARD_BYTES
            || (encoding == DigestEncoding::CanonicalJson
                && (canonical_count >= CONTENT_DIGEST_CANONICAL_SHARD_ENTRIES
                    || canonical_bytes + cost > CONTENT_DIGEST_CANONICAL_SHARD_BYTES))
        {
            let canonical_index = self
                .entries
                .iter()
                .position(|entry| entry.encoding == DigestEncoding::CanonicalJson);
            let index = match (encoding, canonical_index) {
                (_, Some(index)) => index,
                (DigestEncoding::NumericByteArray, None) => 0,
                (DigestEncoding::CanonicalJson, None) => return,
            };
            // Numeric pressure reclaims JSON before falling back to its original FIFO.
            let Some(entry) = self.entries.remove(index) else {
                return;
            };
            if entry.encoding == DigestEncoding::CanonicalJson {
                canonical_count -= 1;
                canonical_bytes -= entry.cost();
            }
            self.bytes -= entry.cost();
        }
        self.entries.push_back(ContentDigestEntry {
            fingerprint,
            encoding,
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
            return self.compute_encoded(DigestEncoding::NumericByteArray, domain, bytes);
        }
        self.digest_at(
            self.fingerprints
                .hash_one((DigestEncoding::NumericByteArray, domain, bytes)),
            domain,
            bytes,
        )
    }

    fn digest_at(&self, fingerprint: u64, domain: &'static str, bytes: &[u8]) -> String {
        self.digest_encoded_at(fingerprint, DigestEncoding::NumericByteArray, domain, bytes)
            .0
    }

    fn digest_encoded(
        &self,
        encoding: DigestEncoding,
        domain: &'static str,
        bytes: &[u8],
    ) -> (String, bool) {
        if bytes.len() > encoding.max_payload() {
            return (self.compute_encoded(encoding, domain, bytes), false);
        }
        self.digest_encoded_at(
            self.fingerprints.hash_one((encoding, domain, bytes)),
            encoding,
            domain,
            bytes,
        )
    }

    fn digest_encoded_at(
        &self,
        fingerprint: u64,
        encoding: DigestEncoding,
        domain: &'static str,
        bytes: &[u8],
    ) -> (String, bool) {
        if bytes.len() > encoding.max_payload() {
            return (self.compute_encoded(encoding, domain, bytes), false);
        }
        let shard = &self.shards[(fingerprint % CONTENT_DIGEST_SHARDS as u64) as usize];
        if let Ok(cache) = shard.try_lock() {
            if let Some(digest) = cache.lookup_encoded(fingerprint, encoding, domain, bytes) {
                return (digest, true);
            }
        }
        // The fingerprint is only a lookup hint. Reuse requires the complete
        // domain and bytes; currentness and authorization remain caller checks.
        // Hash misses outside the lock; recheck before retaining one shared copy.
        let digest = self.compute_encoded(encoding, domain, bytes);
        if let Ok(mut cache) = shard.try_lock() {
            cache.admit(fingerprint, encoding, domain, bytes, &digest);
        }
        (digest, false)
    }

    fn compute_encoded(
        &self,
        encoding: DigestEncoding,
        domain: &'static str,
        bytes: &[u8],
    ) -> String {
        #[cfg(test)]
        self.computations
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        match encoding {
            DigestEncoding::NumericByteArray => uncached_bytes_sha256(domain, bytes),
            DigestEncoding::CanonicalJson => {
                let mut writer = DigestWriter::new(domain);
                writer.append(bytes);
                writer.finish()
            }
        }
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
mod canonical_content_reuse_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_diagnostics_count_exact_streamed_bytes_without_reserializing() {
        struct Counted<'a> {
            value: &'a serde_json::Value,
            calls: std::cell::Cell<u64>,
        }
        impl Serialize for Counted<'_> {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                self.calls.set(self.calls.get() + 1);
                self.value.serialize(serializer)
            }
        }
        let value = serde_json::json!({
            "escaped": "\u{e4}\n\"\\\u{0}",
            "large": "x".repeat(DIGEST_BUFFER_BYTES * 3 + 17),
            "array": [0, 10, 255],
        });
        let serialized_bytes = u64::try_from(serde_json::to_vec(&value).unwrap().len()).unwrap();
        let counted = Counted {
            value: &value,
            calls: std::cell::Cell::new(0),
        };
        let cache = ContentDigestCache::new();
        let started = std::time::Instant::now();
        let (digests, diagnostics) = with_digest_diagnostics(|| {
            [
                canonical_sha256_with_cache("observed-canonical", &counted, &cache).unwrap(),
                canonical_sha256_with_cache("observed-canonical", &counted, &cache).unwrap(),
            ]
        });
        let elapsed = started.elapsed();
        assert_eq!(counted.calls.get(), 2);
        for digest in digests {
            assert_eq!(digest, legacy_digest("observed-canonical", &value));
        }
        assert_eq!(diagnostics.len(), 1);
        let record = &diagnostics[0];
        assert_eq!(record.domain, Some("observed-canonical"));
        assert_eq!(record.canonical.calls, 2);
        assert_eq!(record.canonical.cache_hits, 1);
        assert_eq!(record.canonical.failures, 0);
        assert_eq!(record.canonical.serialized_bytes, serialized_bytes * 2);
        assert_eq!(
            record.canonical.hashed_bytes,
            serialized_bytes + u64::try_from("observed-canonical".len()).unwrap() + 1
        );
        assert!(record.canonical.elapsed <= elapsed);
        assert_eq!(record.byte_array.calls, 0);
    }

    #[test]
    fn digest_diagnostics_count_only_actual_byte_cache_misses_and_separate_domains() {
        let cache = ContentDigestCache::new();
        let canonical_cache = ContentDigestCache::new();
        let first = [0_u8, 9, 10, 99, 100, 255];
        let changed = [1_u8, 10, 101];
        let ((initial, repeated, edited, other, canonical), diagnostics) =
            with_digest_diagnostics(|| {
                (
                    cache.digest_at(0, "observed-bytes", &first),
                    cache.digest_at(0, "observed-bytes", &first),
                    cache.digest_at(0, "observed-bytes", &changed),
                    cache.digest_at(0, "observed-other", &first),
                    canonical_sha256_with_cache("observed-bytes", &first, &canonical_cache)
                        .unwrap(),
                )
            });
        assert_eq!(initial, repeated);
        assert_eq!(initial, canonical);
        assert_eq!(initial, legacy_digest("observed-bytes", &first));
        assert_eq!(edited, legacy_digest("observed-bytes", &changed));
        assert_eq!(other, legacy_digest("observed-other", &first));
        assert_ne!(initial, other);
        assert_eq!(computations(&cache), 3);
        assert_eq!(diagnostics.len(), 2);
        let first_len = u64::try_from(serde_json::to_vec(&first).unwrap().len()).unwrap();
        let changed_len = u64::try_from(serde_json::to_vec(&changed).unwrap().len()).unwrap();
        let record = diagnostics
            .iter()
            .find(|record| record.domain == Some("observed-bytes"))
            .unwrap();
        assert_eq!(record.canonical.calls, 1);
        assert_eq!(record.canonical.serialized_bytes, first_len);
        assert_eq!(record.byte_array.calls, 2);
        assert_eq!(record.byte_array.failures, 0);
        assert_eq!(record.byte_array.serialized_bytes, first_len + changed_len);
        assert_eq!(
            record.byte_array.hashed_bytes,
            first_len + changed_len + (u64::try_from("observed-bytes".len()).unwrap() + 1) * 2
        );
        let other = diagnostics
            .iter()
            .find(|record| record.domain == Some("observed-other"))
            .unwrap();
        assert_eq!(other.canonical.calls, 0);
        assert_eq!(other.byte_array.calls, 1);
        assert_eq!(other.byte_array.serialized_bytes, first_len);
        let (_, hits) = with_digest_diagnostics(|| {
            assert_eq!(cache.digest_at(0, "observed-bytes", &first), initial);
            assert_eq!(cache.digest_at(0, "observed-bytes", &changed), edited);
        });
        assert!(hits.is_empty());
        assert_eq!(computations(&cache), 3);
    }

    #[test]
    fn digest_diagnostics_count_empty_streams_and_failed_partial_serialization() {
        struct Invalid;
        impl Serialize for Invalid {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                use serde::ser::SerializeSeq;
                let mut sequence = serializer.serialize_seq(Some(2))?;
                sequence.serialize_element(&123_u16)?;
                Err(serde::ser::Error::custom("injected serialization failure"))
            }
        }
        for (enabled, hashed_bytes) in [(true, 0), (false, 8)] {
            let ((empty, error), diagnostics) =
                with_canonical_digest_cache_enabled(enabled, || {
                    with_digest_diagnostics(|| {
                        (
                            uncached_bytes_sha256("empty", &[]),
                            canonical_sha256("partial", &Invalid).unwrap_err(),
                        )
                    })
                });
            assert_eq!(empty, legacy_digest("empty", &Vec::<u8>::new()));
            assert_eq!(error.code, WorkflowErrorCode::InvalidInput);
            assert_eq!(diagnostics.len(), 2);
            let empty = diagnostics
                .iter()
                .find(|record| record.domain == Some("empty"))
                .unwrap();
            assert_eq!(empty.byte_array.calls, 1);
            assert_eq!(empty.byte_array.serialized_bytes, 2);
            assert_eq!(empty.byte_array.hashed_bytes, 8);
            let partial = diagnostics
                .iter()
                .find(|record| record.domain == Some("partial"))
                .unwrap();
            assert_eq!(partial.canonical.calls, 1);
            assert_eq!(partial.canonical.failures, 1);
            assert_eq!(partial.canonical.serialized_bytes, 4);
            assert_eq!(partial.canonical.hashed_bytes, hashed_bytes);
        }
    }

    #[test]
    fn digest_diagnostics_hash_bytes_follow_flush_and_buffer_updates_once() {
        let payload = vec![b'a'; DIGEST_BUFFER_BYTES * 3 + 17];
        let mut expected = Sha256::new();
        expected.update(b"writer-counts\0");
        expected.update(&payload);
        for chunk_size in [1, 3, 8191, 8192, 8193, payload.len()] {
            let (digest, diagnostics) = with_digest_diagnostics(|| {
                let mut writer = DigestWriter::new("writer-counts");
                writer.diagnostic =
                    DigestObservation::start("writer-counts", DigestKind::Canonical);
                writer.write_all(&[]).unwrap();
                for chunk in payload.chunks(chunk_size) {
                    writer.write_all(chunk).unwrap();
                    writer.flush().unwrap();
                    writer.flush().unwrap();
                }
                writer.finish()
            });
            assert_eq!(digest, hex_sha256(&expected.clone().finalize()));
            assert_eq!(diagnostics.len(), 1);
            assert_eq!(diagnostics[0].canonical.calls, 1);
            assert_eq!(diagnostics[0].canonical.failures, 0);
            assert_eq!(
                diagnostics[0].canonical.serialized_bytes,
                u64::try_from(payload.len()).unwrap()
            );
            assert_eq!(
                diagnostics[0].canonical.hashed_bytes,
                u64::try_from(payload.len() + b"writer-counts\0".len()).unwrap()
            );
        }
    }

    #[test]
    fn digest_diagnostics_bound_domains_and_aggregate_overflow_without_names() {
        const DOMAINS: [&str; 66] = [
            "d00", "d01", "d02", "d03", "d04", "d05", "d06", "d07", "d08", "d09", "d10", "d11",
            "d12", "d13", "d14", "d15", "d16", "d17", "d18", "d19", "d20", "d21", "d22", "d23",
            "d24", "d25", "d26", "d27", "d28", "d29", "d30", "d31", "d32", "d33", "d34", "d35",
            "d36", "d37", "d38", "d39", "d40", "d41", "d42", "d43", "d44", "d45", "d46", "d47",
            "d48", "d49", "d50", "d51", "d52", "d53", "d54", "d55", "d56", "d57", "d58", "d59",
            "d60", "d61", "d62", "d63", "d64", "d65",
        ];
        let cache = ContentDigestCache::new();
        let (_, diagnostics) = with_digest_diagnostics(|| {
            for domain in DOMAINS {
                canonical_sha256_with_cache(domain, &0, &cache).unwrap();
            }
            canonical_sha256_with_cache(DOMAINS[0], &0, &cache).unwrap();
            uncached_bytes_sha256(DOMAINS[65], &[]);
        });
        assert_eq!(diagnostics.len(), MAX_DIGEST_DIAGNOSTIC_DOMAINS + 1);
        assert!(diagnostics
            .iter()
            .all(|record| record.domain != Some("d64") && record.domain != Some("d65")));
        let overflow = diagnostics
            .iter()
            .find(|record| record.domain.is_none())
            .unwrap();
        assert_eq!(overflow.canonical.calls, 2);
        assert_eq!(overflow.canonical.serialized_bytes, 2);
        assert_eq!(overflow.canonical.hashed_bytes, 10);
        assert_eq!(overflow.byte_array.calls, 1);
        assert_eq!(overflow.byte_array.serialized_bytes, 2);
        assert_eq!(overflow.byte_array.hashed_bytes, 6);
        let first = diagnostics
            .iter()
            .find(|record| record.domain == Some("d00"))
            .unwrap();
        assert_eq!(first.canonical.calls, 2);
        assert_eq!(
            diagnostics
                .iter()
                .map(|record| record.canonical.calls)
                .sum::<u64>(),
            67
        );
    }

    #[test]
    fn digest_diagnostics_nested_scopes_isolate_records_and_restore_outer() {
        let (inner, outer) = with_digest_diagnostics(|| {
            canonical_sha256("outer", &1).unwrap();
            let (_, inner) = with_digest_diagnostics(|| {
                canonical_sha256("inner", &2).unwrap();
                canonical_sha256("outer", &3).unwrap();
            });
            canonical_sha256("outer", &4).unwrap();
            inner
        });
        assert_eq!(outer.len(), 1);
        assert_eq!(outer[0].domain, Some("outer"));
        assert_eq!(outer[0].canonical.calls, 2);
        assert_eq!(inner.len(), 2);
        assert!(inner.iter().all(|record| record.canonical.calls == 1));
        DIGEST_DIAGNOSTICS.with(|slot| assert!(slot.borrow().is_none()));
    }

    #[test]
    fn digest_diagnostics_unwind_restores_outer_and_clears_top_level_scope() {
        let (_, outer) = with_digest_diagnostics(|| {
            canonical_sha256("outer", &1).unwrap();
            assert!(std::panic::catch_unwind(|| {
                with_digest_diagnostics(|| {
                    canonical_sha256("discarded-inner", &2).unwrap();
                    panic!("injected diagnostic unwind");
                });
            })
            .is_err());
            canonical_sha256("outer", &3).unwrap();
        });
        assert_eq!(outer.len(), 1);
        assert_eq!(outer[0].domain, Some("outer"));
        assert_eq!(outer[0].canonical.calls, 2);
        assert!(std::panic::catch_unwind(|| {
            with_digest_diagnostics(|| panic!("injected top-level unwind"));
        })
        .is_err());
        DIGEST_DIAGNOSTICS.with(|slot| assert!(slot.borrow().is_none()));
        let (result, fresh) = with_digest_diagnostics(|| 42);
        assert_eq!(result, 42);
        assert!(fresh.is_empty());
    }

    #[test]
    fn digest_diagnostics_unavailable_borrow_never_changes_hash_results() {
        let (digest, diagnostics) = with_digest_diagnostics(|| {
            DIGEST_DIAGNOSTICS.with(|slot| {
                let _borrow = slot.borrow_mut();
                let (digest, nested) =
                    with_digest_diagnostics(|| canonical_sha256("unavailable", &42).unwrap());
                assert!(nested.is_empty());
                digest
            })
        });
        assert_eq!(digest, legacy_digest("unavailable", &42));
        assert!(diagnostics.is_empty());
        DIGEST_DIAGNOSTICS.with(|slot| assert!(slot.borrow().is_none()));
    }

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
        // Contended admissions may skip retention; sequential reads must converge
        // to one entry per exact payload without changing any returned digest.
        for value in 0..8 {
            let payload = vec![value; 4096];
            assert_eq!(
                cache.digest_at(0, "journal", &payload),
                legacy_digest("journal", &payload)
            );
        }
        let shared = vec![42; 4096];
        assert_eq!(
            cache.digest_at(0, "journal", &shared),
            legacy_digest("journal", &shared)
        );
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
