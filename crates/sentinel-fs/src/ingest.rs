//! Transactional ingest pipeline for the Artifact Plane.
//!
//! Usage:
//! ```ignore
//! let session = begin_ingest(&plane);
//! session.write(data);          // can call multiple times (streaming)
//! let object_id = commit_ingest(session)?;  // atomic redb transaction
//! // -- or --
//! abort_ingest(session);        // temp cleanup, no DB changes
//! ```

use crate::artifact::{ArtifactPlane, ChunkHash, ObjectMetadata, FS_CHUNK_REFCOUNT, FS_CHUNKS, FS_MANIFESTS, FS_OBJECTS};
use crate::chunker::chunk_data;
use redb::{ReadableDatabase, ReadableTable};
use std::io::Cursor;

/// zstd compression level for chunk storage.
const ZSTD_LEVEL: i32 = 3;

/// Minimum chunk size before we try zstd compression.
const MIN_COMPRESS_BYTES: usize = 256;

/// An in-progress ingest session. Holds buffered data before commit.
/// Created via `begin_ingest`, finalized via `commit_ingest` or `abort_ingest`.
pub struct IngestSession<'a> {
    plane: &'a ArtifactPlane,
    /// Accumulated input data (streaming writes are buffered here).
    buffer: Vec<u8>,
    /// MIME type hint.
    mime: String,
}

/// Start a new ingest session.
pub fn begin_ingest<'a>(plane: &'a ArtifactPlane, mime: impl Into<String>) -> IngestSession<'a> {
    IngestSession {
        plane,
        buffer: Vec::new(),
        mime: mime.into(),
    }
}

impl IngestSession<'_> {
    /// Append data to this session. Can be called multiple times for streaming ingest.
    pub fn write(&mut self, data: &[u8]) {
        self.buffer.extend_from_slice(data);
    }

    /// Total bytes buffered so far.
    pub fn buffered_len(&self) -> usize {
        self.buffer.len()
    }
}

/// Commit the session atomically. Returns the new ObjectId on success.
///
/// The atomic redb transaction:
/// 1. Chunk the buffered data via CDC
/// 2. For each chunk: store in FS_CHUNKS (skip if already exists = dedup)
/// 3. Increment FS_CHUNK_REFCOUNT for each chunk
/// 4. Write manifest to FS_MANIFESTS
/// 5. Write object metadata to FS_OBJECTS
/// 6. Commit write transaction
pub fn commit_ingest(session: IngestSession<'_>) -> anyhow::Result<u64> {
    let IngestSession { plane, buffer, mime } = session;

    // Chunk the data
    let chunks: Vec<_> = chunk_data(&buffer).collect();
    let total_size = buffer.len() as u64;
    let chunk_count = chunks.len() as u32;

    // Pre-check which chunks already exist (read-only transaction, no fsync).
    // This avoids expensive zstd compression for chunks that are already stored.
    let existing_chunks = {
        let rtxn = plane.db.begin_read()?;
        let chunks_table = rtxn.open_table(FS_CHUNKS)?;
        let mut set = std::collections::HashSet::with_capacity(chunks.len());
        for chunk in &chunks {
            if chunks_table.get(&chunk.hash)?.is_some() {
                set.insert(chunk.hash);
            }
        }
        set
    };

    // Compress only NEW chunks — skip compression entirely for dedup hits
    let mut chunk_entries: Vec<(ChunkHash, Option<Vec<u8>>)> = Vec::with_capacity(chunks.len());
    for chunk in &chunks {
        if existing_chunks.contains(&chunk.hash) {
            chunk_entries.push((chunk.hash, None)); // dedup hit: no compressed data needed
        } else {
            let compressed = compress_chunk(&chunk.data);
            chunk_entries.push((chunk.hash, Some(compressed)));
        }
    }
    let manifest: Vec<ChunkHash> = chunk_entries.iter().map(|(h, _)| *h).collect();
    let manifest_bytes = serde_json::to_vec(&manifest)
        .map_err(|e| anyhow::anyhow!("manifest serialize: {e}"))?;

    // Allocate ObjectId (separate small transaction — this is idempotent if we crash after)
    let object_id = plane.next_object_id()?;

    let meta = ObjectMetadata::new(total_size, &mime, chunk_count);
    let meta_bytes = meta.serialize()?;

    // Atomic write transaction: all-or-nothing
    let wtxn = plane.db.begin_write()?;
    {
        let mut chunks_table = wtxn.open_table(FS_CHUNKS)?;
        let mut refcount_table = wtxn.open_table(FS_CHUNK_REFCOUNT)?;
        let mut manifests_table = wtxn.open_table(FS_MANIFESTS)?;
        let mut objects_table = wtxn.open_table(FS_OBJECTS)?;

        // 1. Store new chunks + 2. Increment refcounts for all
        for (hash, compressed) in &chunk_entries {
            if let Some(data) = compressed {
                // New chunk: store compressed data
                chunks_table.insert(hash, data.as_slice())?;
            }
            // Always increment refcount (even for deduplicated chunks)
            let current = refcount_table.get(hash)?.map(|g| g.value()).unwrap_or(0);
            refcount_table.insert(hash, current + 1)?;
        }

        // 3. Write manifest
        manifests_table.insert(object_id, manifest_bytes.as_slice())?;

        // 4. Write object metadata
        objects_table.insert(object_id, meta_bytes.as_slice())?;
    }
    wtxn.commit()?;

    Ok(object_id)
}

/// Abort the session. Drops the buffered data without touching the database.
/// No cleanup needed — nothing was written.
pub fn abort_ingest(session: IngestSession<'_>) {
    // Simply drop the session — buffer is on the heap, no DB changes were made.
    drop(session);
}

/// Compress a chunk with zstd, falling back to raw if compression doesn't help.
fn compress_chunk(data: &[u8]) -> Vec<u8> {
    if data.len() >= MIN_COMPRESS_BYTES {
        if let Ok(compressed) = zstd::encode_all(Cursor::new(data), ZSTD_LEVEL) {
            if compressed.len() < data.len() {
                let mut out = Vec::with_capacity(1 + compressed.len());
                out.push(0x01u8); // ZSTD prefix
                out.extend_from_slice(&compressed);
                return out;
            }
        }
    }
    let mut out = Vec::with_capacity(1 + data.len());
    out.push(0x00u8); // RAW prefix
    out.extend_from_slice(data);
    out
}

/// Decompress a chunk (inverse of compress_chunk).
pub fn decompress_chunk(encoded: &[u8]) -> anyhow::Result<Vec<u8>> {
    if encoded.is_empty() {
        return Err(anyhow::anyhow!("Empty chunk"));
    }
    match encoded[0] {
        0x00 => Ok(encoded[1..].to_vec()),
        0x01 => {
            let decompressed = zstd::decode_all(Cursor::new(&encoded[1..]))?;
            Ok(decompressed)
        }
        other => Err(anyhow::anyhow!("Unknown chunk prefix: 0x{other:02x}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_plane() -> (ArtifactPlane, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let plane = ArtifactPlane::open(dir.path().join("ingest_test.redb")).unwrap();
        (plane, dir)
    }

    #[test]
    fn ingest_lifecycle_basic() {
        let (plane, _dir) = temp_plane();

        let mut session = begin_ingest(&plane, "text/plain");
        session.write(b"hello world");
        let object_id = commit_ingest(session).unwrap();

        assert_eq!(object_id, 1);

        let meta = plane.get_object(object_id).unwrap().unwrap();
        assert_eq!(meta.size, 11);
        assert_eq!(meta.mime, "text/plain");
        assert!(meta.chunk_count >= 1);
    }

    #[test]
    fn abort_leaves_no_artifacts() {
        let (plane, _dir) = temp_plane();

        let mut session = begin_ingest(&plane, "application/octet-stream");
        session.write(&vec![0xAA; 100_000]);
        abort_ingest(session);

        // No objects should have been created
        // next_object_id returns 1 if no objects exist
        let id = plane.next_object_id().unwrap();
        // The counter-slot at u64::MAX gets incremented by next_object_id itself,
        // but our abort should leave no manifest/object entries for valid IDs
        assert!(
            plane.get_object(1).unwrap().is_none(),
            "aborted ingest must leave no object"
        );
        // Suppress "id unused" warning
        let _ = id;
    }

    #[test]
    fn dedup_identical_content() {
        let (plane, _dir) = temp_plane();
        let data = vec![0xBB; 200_000];

        let mut s1 = begin_ingest(&plane, "application/octet-stream");
        s1.write(&data);
        let id1 = commit_ingest(s1).unwrap();

        let mut s2 = begin_ingest(&plane, "application/octet-stream");
        s2.write(&data);
        let id2 = commit_ingest(s2).unwrap();

        // Different object IDs
        assert_ne!(id1, id2);

        // Manifests have the same chunk hashes
        let m1 = plane.get_manifest(id1).unwrap().unwrap();
        let m2 = plane.get_manifest(id2).unwrap().unwrap();
        assert_eq!(m1, m2, "identical data must produce identical manifests");

        // Each chunk hash has refcount 2 (referenced by both manifests)
        for hash in &m1 {
            let rc = plane.get_chunk_refcount(hash).unwrap();
            assert_eq!(rc, 2, "refcount must be 2 for chunk shared by 2 objects");
        }
    }

    #[test]
    fn streaming_write_equals_single_write() {
        let (plane, _dir) = temp_plane();
        let data: Vec<u8> = (0..300_000u32).map(|i| (i * 13 + 7) as u8).collect();

        // Single write
        let mut s1 = begin_ingest(&plane, "application/octet-stream");
        s1.write(&data);
        let id1 = commit_ingest(s1).unwrap();

        // Streaming write (same data in 3 pieces)
        let mut s2 = begin_ingest(&plane, "application/octet-stream");
        s2.write(&data[..100_000]);
        s2.write(&data[100_000..200_000]);
        s2.write(&data[200_000..]);
        let id2 = commit_ingest(s2).unwrap();

        // Both should produce the same manifest
        let m1 = plane.get_manifest(id1).unwrap().unwrap();
        let m2 = plane.get_manifest(id2).unwrap().unwrap();
        assert_eq!(m1, m2, "streaming write must produce same chunks as single write");
    }

    #[test]
    fn refcount_invariant_after_two_ingests() {
        let (plane, _dir) = temp_plane();
        let data = vec![0x42u8; 150_000];

        let mut s1 = begin_ingest(&plane, "text/plain");
        s1.write(&data);
        commit_ingest(s1).unwrap();

        let manifest = plane.get_manifest(1).unwrap().unwrap();
        for hash in &manifest {
            assert_eq!(plane.get_chunk_refcount(hash).unwrap(), 1);
        }

        let mut s2 = begin_ingest(&plane, "text/plain");
        s2.write(&data);
        commit_ingest(s2).unwrap();

        for hash in &manifest {
            assert_eq!(plane.get_chunk_refcount(hash).unwrap(), 2);
        }
    }

    #[test]
    fn compress_decompress_roundtrip() {
        let data = vec![0xCC; 4096];
        let compressed = compress_chunk(&data);
        let decompressed = decompress_chunk(&compressed).unwrap();
        assert_eq!(decompressed, data);
    }
}
