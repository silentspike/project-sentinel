//! Content-Addressed Storage (CAS) with SHA-256 dedup and zstd compression.
//!
//! Blobs are stored as `{cas_dir}/{hex[0..2]}/{hex[2..64]}` where `hex` is the
//! SHA-256 hash of the original (uncompressed) content. Each blob has a 1-byte
//! prefix indicating the encoding: `0x00` = raw, `0x01` = zstd compressed.

use sha2::{Digest, Sha256};
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use tracing::instrument;

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
        Ok(Self { cas_dir })
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

    /// Read and decode a blob by its hash.
    #[instrument(skip(self), level = "trace")]
    pub fn read(&self, hash: &[u8; 32]) -> anyhow::Result<Vec<u8>> {
        let blob_path = self.blob_path(hash);
        let encoded =
            fs::read(&blob_path).map_err(|e| anyhow::anyhow!("Blob {}: {e}", hex_encode(hash)))?;
        decode_blob(&encoded)
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
}

/// Encode 32 bytes as lowercase hex string.
pub fn hex_encode(bytes: &[u8; 32]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(64);
    for byte in bytes {
        write!(s, "{byte:02x}").unwrap();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_cas() -> (CasStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path()).unwrap();
        (store, dir)
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
