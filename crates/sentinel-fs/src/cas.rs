//! Content-Addressed Storage (CAS) with SHA-256 dedup and zstd compression.
//!
//! Blobs are stored as `{cas_dir}/{hex[0..2]}/{hex[2..64]}` where `hex` is the
//! SHA-256 hash of the original (uncompressed) content. Each blob has a 1-byte
//! prefix indicating the encoding: `0x00` = raw, `0x01` = zstd compressed.

use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use tracing::instrument;

use crate::block_resolver::BlobResolve;

/// Blob prefix: uncompressed raw data.
const PREFIX_RAW: u8 = 0x00;

/// Blob prefix: zstd compressed data.
const PREFIX_ZSTD: u8 = 0x01;

/// zstd compression level (3 = good balance of speed/ratio).
const ZSTD_LEVEL: i32 = 3;

/// Minimum data size for compression (below this, store raw).
const MIN_COMPRESS_SIZE: usize = 256;

/// Content-Addressed Storage with inline deduplication.
///
/// Each unique blob is stored exactly once. The SHA-256 hash serves as the
/// content address. Blobs above 256 bytes are zstd-compressed. Writes are
/// atomic (temp file + rename) for crash safety.
pub struct CasStore {
    cas_dir: PathBuf,
    /// #498 4c (V9): set in cluster mode; on a local-read miss the CAS resolves the blob
    /// by hash (pull from a peer + durable store), then retries. Unset = single-node, the
    /// read path is unchanged.
    resolver: OnceLock<Arc<dyn BlobResolve>>,
}

/// Statistics about the CAS store.
#[derive(Debug, Clone)]
pub struct CasStats {
    pub blob_count: u64,
    pub total_bytes_on_disk: u64,
}

/// Result of a garbage collection run.
#[derive(Debug, Clone)]
pub struct GcStats {
    pub removed: u64,
    pub freed_bytes: u64,
}

impl CasStore {
    /// Open or create a CAS store. Blobs are stored under `{data_dir}/cas/`.
    #[instrument(level = "debug", fields(data_dir = %data_dir.display()))]
    pub fn open(data_dir: &Path) -> anyhow::Result<Self> {
        let cas_dir = data_dir.join("cas");
        fs::create_dir_all(&cas_dir)?;
        Ok(Self {
            cas_dir,
            resolver: OnceLock::new(),
        })
    }

    /// Wire the #498 4c block resolver (V9). Called once in cluster mode after the
    /// resolver is built; a local-read miss then pulls the blob from a peer + retries.
    pub fn set_resolver(&self, resolver: Arc<dyn BlobResolve>) {
        let _ = self.resolver.set(resolver);
    }

    /// Compute SHA-256 hash of data.
    pub fn hash(data: &[u8]) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(data);
        hasher.finalize().into()
    }

    /// Get the filesystem path for a blob by its hash.
    fn blob_path(&self, hash: &[u8; 32]) -> PathBuf {
        let hex = hex_encode(hash);
        self.cas_dir.join(&hex[..2]).join(&hex[2..])
    }

    /// Check if a blob with the given hash exists.
    pub fn contains(&self, hash: &[u8; 32]) -> bool {
        self.blob_path(hash).exists()
    }

    /// Store data in the CAS. Returns `(hash, deduplicated)`.
    ///
    /// If `deduplicated` is true, the blob already existed and no disk I/O
    /// was performed for the content — only metadata needs updating.
    #[instrument(skip(self, data), level = "trace", fields(data_len = data.len()))]
    pub fn store(&self, data: &[u8]) -> anyhow::Result<([u8; 32], bool)> {
        let hash = Self::hash(data);

        if self.contains(&hash) {
            return Ok((hash, true));
        }

        let blob_path = self.blob_path(&hash);
        if let Some(parent) = blob_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let encoded = encode_blob(data);

        // Atomic write: temp file + rename
        let tmp_path = blob_path.with_extension("tmp");
        fs::write(&tmp_path, &encoded)?;
        fs::rename(&tmp_path, &blob_path)?;

        Ok((hash, false))
    }

    /// Read and decode a blob by its hash. On a local miss with a #498 4c resolver wired
    /// (cluster mode), the blob is pulled from a peer by hash + durably stored, then the
    /// read is retried once (V9). Without a resolver this is the unchanged local read.
    #[instrument(skip(self), level = "trace")]
    pub fn read(&self, hash: &[u8; 32]) -> anyhow::Result<Vec<u8>> {
        let blob_path = self.blob_path(hash);
        let encoded = match fs::read(&blob_path) {
            Ok(bytes) => bytes,
            Err(miss) => {
                if self.resolve_missing_blob(hash) {
                    fs::read(&blob_path)
                        .map_err(|e| anyhow::anyhow!("Blob {}: {e}", hex_encode(hash)))?
                } else {
                    return Err(anyhow::anyhow!("Blob {}: {miss}", hex_encode(hash)));
                }
            }
        };
        decode_blob(&encoded)
    }

    /// #498 4c: ask the wired resolver to make a missing blob local (pull + verify +
    /// durable store). Returns whether it is now local. No resolver → `false` (the read
    /// fails locally as before).
    fn resolve_missing_blob(&self, hash: &[u8; 32]) -> bool {
        self.resolver.get().is_some_and(|r| r.ensure_blob(hash))
    }

    /// Remove a blob from the store. Returns true if it existed.
    pub fn remove(&self, hash: &[u8; 32]) -> anyhow::Result<bool> {
        let path = self.blob_path(hash);
        if path.exists() {
            fs::remove_file(&path)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Remove blobs with zero references.
    #[instrument(skip(self, zero_ref_hashes), level = "debug", fields(count = zero_ref_hashes.len()))]
    pub fn gc(&self, zero_ref_hashes: &[[u8; 32]]) -> anyhow::Result<GcStats> {
        let mut removed = 0u64;
        let mut freed_bytes = 0u64;

        for hash in zero_ref_hashes {
            let path = self.blob_path(hash);
            if path.exists() {
                if let Ok(meta) = fs::metadata(&path) {
                    freed_bytes += meta.len();
                }
                fs::remove_file(&path)?;
                removed += 1;
            }
        }

        Ok(GcStats {
            removed,
            freed_bytes,
        })
    }

    /// Get statistics about the CAS store.
    pub fn stats(&self) -> anyhow::Result<CasStats> {
        let mut blob_count = 0u64;
        let mut total_bytes = 0u64;

        if !self.cas_dir.exists() {
            return Ok(CasStats {
                blob_count: 0,
                total_bytes_on_disk: 0,
            });
        }

        for entry in fs::read_dir(&self.cas_dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                for blob_entry in fs::read_dir(entry.path())? {
                    let blob_entry = blob_entry?;
                    if blob_entry.file_type()?.is_file() {
                        blob_count += 1;
                        total_bytes += blob_entry.metadata()?.len();
                    }
                }
            }
        }

        Ok(CasStats {
            blob_count,
            total_bytes_on_disk: total_bytes,
        })
    }

    /// Get the CAS directory path.
    pub fn cas_dir(&self) -> &Path {
        &self.cas_dir
    }

    /// List every durably-stored blob as a [`BlockRef`](sentinel_common::BlockRef)
    /// (namespace `Blob`, SHA-256). Used at startup (#498 / V28 reconcile) to rebuild
    /// this node's holder advertisements from the files that actually survived on disk.
    ///
    /// In-progress `.tmp` writes are skipped (an incomplete blob is not a holder). The
    /// original (uncompressed) content size — the V7 identity component — is recovered
    /// by decoding each blob, so an advertised `BlockRef` matches the one a `store`
    /// produced.
    pub fn list_block_refs(&self) -> anyhow::Result<Vec<sentinel_common::BlockRef>> {
        let mut refs = Vec::new();
        if !self.cas_dir.exists() {
            return Ok(refs);
        }
        for shard in fs::read_dir(&self.cas_dir)? {
            let shard = shard?;
            if !shard.file_type()?.is_dir() {
                continue;
            }
            let Some(prefix) = shard.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if prefix.len() != 2 {
                continue;
            }
            for blob in fs::read_dir(shard.path())? {
                let blob = blob?;
                if !blob.file_type()?.is_file() {
                    continue;
                }
                let Some(rest) = blob.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                // Skip in-progress temp writes; a complete blob name is 62 hex chars.
                if rest.len() != 62 {
                    continue;
                }
                let Some(hash) = hex_decode_32(&format!("{prefix}{rest}")) else {
                    continue;
                };
                let size = self.read(&hash)?.len() as u64;
                refs.push(sentinel_common::BlockRef::blob_sha256(hash, size));
            }
        }
        Ok(refs)
    }

    /// The raw on-disk **encoded** bytes of a stored blob (for the #498 block-pull
    /// server's `BlockProvider`). `None` if not held. Takes only a content hash, never a
    /// path (V10).
    pub fn encoded_blob(&self, hash: &[u8; 32]) -> Option<Vec<u8>> {
        fs::read(self.blob_path(hash)).ok()
    }

    /// Store a blob **pulled from a peer** (#498 / V28): verify the content hash + size
    /// against the expected `BlockRef` identity, then **durably** publish it
    /// (`fsync(file)` → atomic rename → `fsync(parent dir)`) so it is on disk before it
    /// can be advertised. The input is the raw on-disk **encoded** form streamed from the
    /// holder. A corrupt / tampered blob (digest or size mismatch) is **rejected, never
    /// published** (AC-3).
    pub fn store_pulled_blob(
        &self,
        encoded: &[u8],
        expected_hash: &[u8; 32],
        expected_size: u64,
    ) -> anyhow::Result<()> {
        // Verify the identity the digest covers — the DECODED content, not the wire bytes.
        let decoded =
            decode_blob(encoded).map_err(|e| anyhow::anyhow!("pulled blob decode failed: {e}"))?;
        let actual = Self::hash(&decoded);
        if &actual != expected_hash {
            anyhow::bail!(
                "pulled blob digest mismatch: got {} want {} — rejected, not published",
                hex_encode(&actual),
                hex_encode(expected_hash)
            );
        }
        if decoded.len() as u64 != expected_size {
            anyhow::bail!(
                "pulled blob size mismatch: got {} want {expected_size} — rejected",
                decoded.len()
            );
        }
        self.durable_write_blob(expected_hash, encoded)
    }

    /// Durably write `encoded` to the canonical path for `hash` (V28): write a temp file,
    /// `fsync` it, atomically rename it into place, then `fsync` the parent directory so
    /// the rename itself survives a crash.
    fn durable_write_blob(&self, hash: &[u8; 32], encoded: &[u8]) -> anyhow::Result<()> {
        let blob_path = self.blob_path(hash);
        if self.contains(hash) {
            return Ok(()); // already durable (dedup)
        }
        let parent = blob_path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("blob path has no parent"))?;
        fs::create_dir_all(parent)?;
        let tmp_path = blob_path.with_extension("tmp");
        {
            let mut f = fs::File::create(&tmp_path)?;
            f.write_all(encoded)?;
            f.sync_all()?; // fsync(file): the content is durable before the rename
        }
        fs::rename(&tmp_path, &blob_path)?; // atomic publish
        if let Ok(dir) = fs::File::open(parent) {
            let _ = dir.sync_all(); // fsync(dir): the rename is durable
        }
        Ok(())
    }

    /// Startup reconcile (#498 / V28): delete incomplete `.tmp` writes left by a crash
    /// mid-store, so a half-written blob is never mistaken for a durable one (and never
    /// re-advertised). Returns the count removed.
    pub fn reconcile_temp(&self) -> anyhow::Result<usize> {
        let mut removed = 0;
        if !self.cas_dir.exists() {
            return Ok(0);
        }
        for shard in fs::read_dir(&self.cas_dir)? {
            let shard = shard?;
            if !shard.file_type()?.is_dir() {
                continue;
            }
            for f in fs::read_dir(shard.path())? {
                let f = f?;
                if f.path().extension().and_then(|e| e.to_str()) == Some("tmp") {
                    fs::remove_file(f.path())?;
                    removed += 1;
                }
            }
        }
        Ok(removed)
    }
}

/// Encode data as a CAS blob (prefix byte + optional zstd compression).
fn encode_blob(data: &[u8]) -> Vec<u8> {
    if data.len() >= MIN_COMPRESS_SIZE {
        if let Ok(compressed) = zstd::encode_all(Cursor::new(data), ZSTD_LEVEL) {
            // Only use compression if it actually saves space
            if compressed.len() < data.len() {
                let mut blob = Vec::with_capacity(1 + compressed.len());
                blob.push(PREFIX_ZSTD);
                blob.extend_from_slice(&compressed);
                return blob;
            }
        }
    }

    // Raw storage: small data or compression didn't help
    let mut blob = Vec::with_capacity(1 + data.len());
    blob.push(PREFIX_RAW);
    blob.extend_from_slice(data);
    blob
}

/// Decode a CAS blob back to original data.
fn decode_blob(encoded: &[u8]) -> anyhow::Result<Vec<u8>> {
    if encoded.is_empty() {
        return Err(anyhow::anyhow!("Empty blob"));
    }

    match encoded[0] {
        PREFIX_RAW => Ok(encoded[1..].to_vec()),
        PREFIX_ZSTD => {
            let decompressed = zstd::decode_all(Cursor::new(&encoded[1..]))?;
            Ok(decompressed)
        }
        other => Err(anyhow::anyhow!("Unknown blob prefix: 0x{other:02x}")),
    }
}

/// GC statistics for the Artifact Plane chunk store.
#[derive(Debug, Clone, Default)]
pub struct ChunkGcStats {
    pub removed: u64,
    pub freed_bytes: u64,
    /// Chunks moved to trash queue (grace period before deletion).
    pub trashed: u64,
    /// Chunks freed from trash queue after grace period expired.
    pub freed_from_trash: u64,
}

/// Encode bytes as lowercase hex string.
pub fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(s, "{byte:02x}").unwrap();
    }
    s
}

/// Decode a 64-char lowercase hex string into a 32-byte array; `None` if malformed.
fn hex_decode_32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_cas() -> (CasStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path()).unwrap();
        (store, dir)
    }

    // ── #498 4c: cas.read resolves a missing blob via the wired resolver (V9) ──

    #[test]
    fn read_resolves_a_missing_blob_via_the_wired_resolver() {
        // A resolver that "pulls" by durably storing the blob into the same CAS dir.
        struct PullIntoDir {
            dir: std::path::PathBuf,
            data: Vec<u8>,
        }
        impl crate::block_resolver::BlobResolve for PullIntoDir {
            fn ensure_blob(&self, _hash: &[u8; 32]) -> bool {
                CasStore::open(&self.dir)
                    .and_then(|c| c.store(&self.data))
                    .is_ok()
            }
        }

        let (store, dir) = temp_cas();
        let data = b"a blob that lives only on a peer until the read resolves it";
        let hash = CasStore::hash(data);
        assert!(!store.contains(&hash), "not local initially");

        store.set_resolver(Arc::new(PullIntoDir {
            dir: dir.path().to_path_buf(),
            data: data.to_vec(),
        }));
        // The read misses locally -> the resolver pulls+stores -> the retry hits.
        assert_eq!(
            store.read(&hash).unwrap(),
            data,
            "read resolves the missing blob"
        );
        assert!(store.contains(&hash), "blob is local after the resolve");
    }

    #[test]
    fn read_without_a_resolver_fails_on_a_miss_unchanged() {
        let (store, _dir) = temp_cas();
        // No resolver wired (single-node) — a miss is the same local error as before.
        assert!(store.read(&CasStore::hash(b"never stored")).is_err());
    }

    // ── #498 4b: durable pulled-blob publish + verify (V28) ──

    #[test]
    fn pulled_blob_round_trips_through_durable_publish() {
        // Holder stores a blob; the puller receives its raw encoded bytes (the wire form)
        // and durably publishes them after verifying the content identity.
        let (holder, _h) = temp_cas();
        let data = b"distributed cas pull-by-hash content";
        let (hash, _) = holder.store(data).unwrap();
        let encoded = holder
            .encoded_blob(&hash)
            .expect("holder holds the encoded blob");

        let (puller, _p) = temp_cas();
        assert!(!puller.contains(&hash), "puller starts without the blob");
        puller
            .store_pulled_blob(&encoded, &hash, data.len() as u64)
            .unwrap();
        assert!(puller.contains(&hash), "durably published after verify");
        assert_eq!(
            puller.read(&hash).unwrap(),
            data,
            "content matches the holder"
        );
    }

    #[test]
    fn corrupt_pulled_blob_is_rejected_and_not_published() {
        let (holder, _h) = temp_cas();
        let data = b"the bytes that must not be tampered with";
        let (hash, _) = holder.store(data).unwrap();
        let mut encoded = holder.encoded_blob(&hash).unwrap();
        // Flip a content byte (index 0 is the encoding prefix) — a tampered/corrupt pull.
        encoded[1] ^= 0xFF;

        let (puller, _p) = temp_cas();
        let err = puller
            .store_pulled_blob(&encoded, &hash, data.len() as u64)
            .unwrap_err();
        assert!(
            err.to_string().contains("digest mismatch"),
            "a corrupt blob is rejected on the digest"
        );
        assert!(
            !puller.contains(&hash),
            "a rejected blob is NEVER published (AC-3)"
        );
    }

    #[test]
    fn wrong_size_pulled_blob_is_rejected() {
        let (holder, _h) = temp_cas();
        let data = b"size must match the BlockRef identity";
        let (hash, _) = holder.store(data).unwrap();
        let encoded = holder.encoded_blob(&hash).unwrap();

        let (puller, _p) = temp_cas();
        let err = puller
            .store_pulled_blob(&encoded, &hash, (data.len() + 1) as u64)
            .unwrap_err();
        assert!(err.to_string().contains("size mismatch"));
        assert!(!puller.contains(&hash));
    }

    #[test]
    fn reconcile_temp_deletes_incomplete_writes() {
        let (store, dir) = temp_cas();
        // Simulate a crash mid-store: a leftover .tmp in a shard dir.
        let shard = dir.path().join("cas").join("ab");
        std::fs::create_dir_all(&shard).unwrap();
        std::fs::write(shard.join("deadbeef.tmp"), b"half-written").unwrap();
        std::fs::write(shard.join("cafef00d"), b"complete blob").unwrap();

        let removed = store.reconcile_temp().unwrap();
        assert_eq!(removed, 1, "the incomplete .tmp is removed");
        assert!(!shard.join("deadbeef.tmp").exists());
        assert!(shard.join("cafef00d").exists(), "the complete blob is kept");
    }

    #[test]
    fn hash_deterministic() {
        let h1 = CasStore::hash(b"hello world");
        let h2 = CasStore::hash(b"hello world");
        assert_eq!(h1, h2);
    }

    #[test]
    fn hash_different_input() {
        let h1 = CasStore::hash(b"hello");
        let h2 = CasStore::hash(b"world");
        assert_ne!(h1, h2);
    }

    #[test]
    fn store_and_read_small() {
        let (cas, _dir) = temp_cas();
        let data = b"small data";
        let (hash, deduped) = cas.store(data).unwrap();
        assert!(!deduped);

        let read_back = cas.read(&hash).unwrap();
        assert_eq!(read_back, data);
    }

    #[test]
    fn store_and_read_large() {
        let (cas, _dir) = temp_cas();
        // Data larger than MIN_COMPRESS_SIZE to trigger zstd
        let data: Vec<u8> = (0..1024).map(|i| (i % 256) as u8).collect();
        let (hash, deduped) = cas.store(&data).unwrap();
        assert!(!deduped);

        let read_back = cas.read(&hash).unwrap();
        assert_eq!(read_back, data);
    }

    #[test]
    fn dedup_identical_data() {
        let (cas, _dir) = temp_cas();
        let data = b"duplicate data for dedup test";

        let (hash1, deduped1) = cas.store(data).unwrap();
        assert!(!deduped1);

        let (hash2, deduped2) = cas.store(data).unwrap();
        assert!(deduped2);
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn contains_check() {
        let (cas, _dir) = temp_cas();
        let data = b"test data";
        let hash = CasStore::hash(data);

        assert!(!cas.contains(&hash));
        cas.store(data).unwrap();
        assert!(cas.contains(&hash));
    }

    #[test]
    fn remove_blob() {
        let (cas, _dir) = temp_cas();
        let data = b"removable";
        let (hash, _) = cas.store(data).unwrap();

        assert!(cas.contains(&hash));
        assert!(cas.remove(&hash).unwrap());
        assert!(!cas.contains(&hash));
        assert!(!cas.remove(&hash).unwrap());
    }

    #[test]
    fn gc_removes_specified_blobs() {
        let (cas, _dir) = temp_cas();
        let (hash1, _) = cas.store(b"blob one").unwrap();
        let (hash2, _) = cas.store(b"blob two").unwrap();

        let gc_stats = cas.gc(&[hash1]).unwrap();
        assert_eq!(gc_stats.removed, 1);
        assert!(gc_stats.freed_bytes > 0);
        assert!(!cas.contains(&hash1));
        assert!(cas.contains(&hash2));
    }

    #[test]
    fn stats_counts_blobs() {
        let (cas, _dir) = temp_cas();

        let stats = cas.stats().unwrap();
        assert_eq!(stats.blob_count, 0);

        cas.store(b"blob1").unwrap();
        cas.store(b"blob2").unwrap();
        cas.store(b"blob1").unwrap(); // dedup, no new blob

        let stats = cas.stats().unwrap();
        assert_eq!(stats.blob_count, 2);
        assert!(stats.total_bytes_on_disk > 0);
    }

    #[test]
    fn zstd_roundtrip_compressible() {
        let (cas, _dir) = temp_cas();
        // Highly compressible repetitive data
        let data = vec![0xAA; 4096];
        let (hash, _) = cas.store(&data).unwrap();

        let read_back = cas.read(&hash).unwrap();
        assert_eq!(read_back, data);

        // Verify compression actually reduced size on disk
        let blob_path = cas.blob_path(&hash);
        let on_disk = fs::metadata(&blob_path).unwrap().len();
        assert!(
            on_disk < data.len() as u64,
            "zstd should compress repetitive data: {on_disk} >= {}",
            data.len()
        );
    }

    #[test]
    fn zstd_roundtrip_incompressible() {
        let (cas, _dir) = temp_cas();
        // Random-ish data that doesn't compress well but is above threshold
        let data: Vec<u8> = (0..512).map(|i| ((i * 7 + 13) % 256) as u8).collect();
        let (hash, _) = cas.store(&data).unwrap();

        let read_back = cas.read(&hash).unwrap();
        assert_eq!(read_back, data);
    }

    #[test]
    fn hex_encode_works() {
        let bytes = [0u8; 32];
        assert_eq!(
            hex_encode(&bytes),
            "0000000000000000000000000000000000000000000000000000000000000000"
        );

        let mut bytes = [0u8; 32];
        bytes[0] = 0xAB;
        bytes[1] = 0xCD;
        let hex = hex_encode(&bytes);
        assert!(hex.starts_with("abcd"));
        assert_eq!(hex.len(), 64);
    }

    #[test]
    fn blob_path_uses_prefix_subdirs() {
        let (cas, _dir) = temp_cas();
        let hash = CasStore::hash(b"test");
        let path = cas.blob_path(&hash);

        // Should be cas_dir / XX / YYYY...YY (2-char prefix subdir)
        let hex = hex_encode(&hash);
        let parent_name = path
            .parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(parent_name, &hex[..2]);

        let file_name = path.file_name().unwrap().to_str().unwrap();
        assert_eq!(file_name.len(), 62); // 64 hex - 2 prefix
    }

    #[test]
    fn encode_decode_roundtrip_raw() {
        let data = b"small";
        let encoded = encode_blob(data);
        assert_eq!(encoded[0], PREFIX_RAW);
        let decoded = decode_blob(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn encode_decode_roundtrip_compressed() {
        let data = vec![0x42; 1024]; // compressible
        let encoded = encode_blob(&data);
        assert_eq!(encoded[0], PREFIX_ZSTD);
        let decoded = decode_blob(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn decode_empty_blob_errors() {
        assert!(decode_blob(&[]).is_err());
    }

    #[test]
    fn decode_unknown_prefix_errors() {
        assert!(decode_blob(&[0xFF, 0x01, 0x02]).is_err());
    }
}
