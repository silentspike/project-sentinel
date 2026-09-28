//! Transactional ingest pipeline for the Artifact Plane.
//!
//! Single ingest:
//! ```ignore
//! let session = begin_ingest(&plane, "text/plain");
//! session.write(data);
//! let object_id = commit_ingest(session)?;  // 1 redb txn + 1 fsync
//! ```
//!
//! Batch ingest (amortizes fsync across N objects):
//! ```ignore
//! let mut batch = BatchIngest::new(&plane);
//! batch.add(data1, "text/plain");
//! batch.add(data2, "application/pdf");
//! let ids = batch.commit()?;  // 1 redb txn + 1 fsync for all N objects
//! ```

use crate::artifact::{
    ArtifactPlane, IngestSessionState, ObjectMetadata, FS_CHUNKS, FS_CHUNK_REFCOUNT,
    FS_INGEST_SESSIONS, FS_MANIFESTS, FS_OBJECTS,
};
use crate::chunker::chunk_data;
use redb::ReadableTable;
use sha2::{Digest, Sha256};
use std::io::Cursor;

/// zstd compression level for chunk storage.
const ZSTD_LEVEL: i32 = 3;

/// Minimum chunk size before we try zstd compression.
const MIN_COMPRESS_BYTES: usize = 256;

/// Minimum byte delta before we flush progress to FS_INGEST_SESSIONS.
/// Prevents thrashing the DB on small streaming writes.
const PROGRESS_FLUSH_BYTES: u64 = 262_144; // 256 KB

/// An in-progress ingest session. Holds buffered data before commit.
/// Created via `begin_ingest`, finalized via `commit_ingest` or `abort_ingest`.
///
/// The session pre-allocates its ObjectId and registers in `FS_INGEST_SESSIONS`
/// so the FUSE layer can show it as a `.part` file during streaming downloads.
pub struct IngestSession<'a> {
    plane: &'a ArtifactPlane,
    /// Accumulated input data (streaming writes are buffered here).
    buffer: Vec<u8>,
    /// MIME type hint.
    mime: String,
    /// Pre-allocated ObjectId (doubles as session ID in FS_INGEST_SESSIONS).
    object_id: u64,
    /// Bytes received at last DB flush (for throttled progress updates).
    last_flushed_bytes: u64,
}

/// Start a new ingest session. Pre-allocates ObjectId and registers in FS_INGEST_SESSIONS.
pub fn begin_ingest<'a>(plane: &'a ArtifactPlane, mime: impl Into<String>) -> IngestSession<'a> {
    let mime = mime.into();
    let object_id = plane.next_object_id().unwrap_or(0);
    let state = IngestSessionState::new(&mime, format!("ingest-{object_id}"));
    let _ = plane.register_session(object_id, &state);
    IngestSession {
        plane,
        buffer: Vec::new(),
        mime,
        object_id,
        last_flushed_bytes: 0,
    }
}

impl IngestSession<'_> {
    /// Append data to this session. Can be called multiple times for streaming ingest.
    /// Throttled progress updates to FS_INGEST_SESSIONS (every 256KB) for .part visibility.
    pub fn write(&mut self, data: &[u8]) {
        self.buffer.extend_from_slice(data);
        let current = self.buffer.len() as u64;
        if current - self.last_flushed_bytes >= PROGRESS_FLUSH_BYTES {
            let _ = self.plane.update_session_progress(self.object_id, current);
            self.last_flushed_bytes = current;
        }
    }

    /// Total bytes buffered so far.
    pub fn buffered_len(&self) -> usize {
        self.buffer.len()
    }

    /// Pre-allocated ObjectId (also the session ID for .part tracking).
    pub fn object_id(&self) -> u64 {
        self.object_id
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
    let IngestSession {
        plane,
        buffer,
        mime,
        object_id,
        ..
    } = session;
    anyhow::ensure!(object_id != 0, "ingest session id allocation failed");
    let txn = plane.begin_write()?;
    store_object(plane, &txn, object_id, &buffer, &mime)?;
    txn.open_table(FS_INGEST_SESSIONS)?.remove(object_id)?;
    plane
        .segments
        .lock()
        .map_err(|e| anyhow::anyhow!("segment lock: {e}"))?
        .sync()?;
    txn.commit()?;
    Ok(object_id)
}

/// Abort the session and remove its .part entry.
pub fn abort_ingest(session: IngestSession<'_>) {
    let _ = session.plane.remove_session(session.object_id);
}

struct PreparedIngest {
    data: Vec<u8>,
    mime: String,
}

/// Batch ingest: chunk and compress under the publication transaction so both
/// intra-batch and concurrent dedup happen before compression or chunk copies.
pub struct BatchIngest<'a> {
    plane: &'a ArtifactPlane,
    prepared: Vec<PreparedIngest>,
}

impl<'a> BatchIngest<'a> {
    pub fn new(plane: &'a ArtifactPlane) -> Self {
        Self {
            plane,
            prepared: Vec::new(),
        }
    }

    /// Retain input for deferred publication. Chunk bytes are borrowed at commit.
    pub fn add(&mut self, data: &[u8], mime: impl Into<String>) -> anyhow::Result<()> {
        self.prepared.push(PreparedIngest {
            data: data.to_vec(),
            mime: mime.into(),
        });
        Ok(())
    }

    /// Commit all objects in one transaction, syncing segment bytes first.
    pub fn commit(self) -> anyhow::Result<Vec<u64>> {
        if self.prepared.is_empty() {
            return Ok(Vec::new());
        }
        let mut ids = Vec::with_capacity(self.prepared.len());
        for _ in &self.prepared {
            ids.push(self.plane.next_object_id()?);
        }
        let txn = self.plane.begin_write()?;
        for (id, prepared) in ids.iter().zip(&self.prepared) {
            store_object(self.plane, &txn, *id, &prepared.data, &prepared.mime)?;
        }
        self.plane
            .segments
            .lock()
            .map_err(|e| anyhow::anyhow!("segment lock: {e}"))?
            .sync()?;
        txn.commit()?;
        Ok(ids)
    }
}

/// Database transaction precedes the segment lock on every publication path.
/// Inserting each index entry immediately also dedups repeated chunks in a file.
fn store_object(
    plane: &ArtifactPlane,
    txn: &redb::WriteTransaction,
    object_id: u64,
    data: &[u8],
    mime: &str,
) -> anyhow::Result<()> {
    let mut manifest = Vec::new();
    {
        let mut chunks = txn.open_table(FS_CHUNKS)?;
        let mut counts = txn.open_table(FS_CHUNK_REFCOUNT)?;
        let mut trash = txn.open_table(crate::artifact::FS_TRASH_QUEUE)?;
        let mut iter = chunk_data(data);
        while let Some(chunk) = iter.next_borrowed() {
            if chunks.get(&chunk.hash)?.is_none() {
                let compressed = compress_chunk(chunk.data);
                let loc = plane
                    .segments
                    .lock()
                    .map_err(|e| anyhow::anyhow!("segment lock: {e}"))?
                    .append(&compressed)?;
                chunks.insert(&chunk.hash, loc.to_bytes().as_slice())?;
            }
            let count = counts.get(&chunk.hash)?.map(|g| g.value()).unwrap_or(0);
            counts.insert(
                &chunk.hash,
                count
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("chunk refcount overflow"))?,
            )?;
            trash.remove(&chunk.hash)?;
            manifest.push(chunk.hash);
        }
    }
    let sha256: [u8; 32] = Sha256::digest(data).into();
    let meta = ObjectMetadata::new(
        data.len() as u64,
        mime,
        u32::try_from(manifest.len())?,
        sha256,
    );
    txn.open_table(FS_MANIFESTS)?
        .insert(object_id, serde_json::to_vec(&manifest)?.as_slice())?;
    txn.open_table(FS_OBJECTS)?
        .insert(object_id, meta.serialize()?.as_slice())?;
    Ok(())
}

/// Compress a chunk with zstd, falling back to raw if compression doesn't help.
pub(crate) fn compress_chunk(data: &[u8]) -> Vec<u8> {
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

        assert!(object_id > 0, "ObjectId must be positive");

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
        assert_eq!(
            m1, m2,
            "streaming write must produce same chunks as single write"
        );
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
    fn batch_ingest_multiple_objects() {
        let (plane, _dir) = temp_plane();
        let data1 = vec![0xDD; 100_000];
        let data2 = vec![0xEE; 150_000];
        let data3 = b"short text".to_vec();

        let mut batch = BatchIngest::new(&plane);
        batch.add(&data1, "application/octet-stream").unwrap();
        batch.add(&data2, "application/octet-stream").unwrap();
        batch.add(&data3, "text/plain").unwrap();
        let ids = batch.commit().unwrap();

        assert_eq!(ids.len(), 3);
        assert_eq!(ids[0], 1);
        assert_eq!(ids[1], 2);
        assert_eq!(ids[2], 3);

        // Verify each object's metadata
        let m1 = plane.get_object(ids[0]).unwrap().unwrap();
        assert_eq!(m1.size, 100_000);
        let m2 = plane.get_object(ids[1]).unwrap().unwrap();
        assert_eq!(m2.size, 150_000);
        let m3 = plane.get_object(ids[2]).unwrap().unwrap();
        assert_eq!(m3.size, 10);
        assert_eq!(m3.mime, "text/plain");
    }

    #[test]
    fn batch_ingest_empty_is_noop() {
        let (plane, _dir) = temp_plane();
        let batch = BatchIngest::new(&plane);
        let ids = batch.commit().unwrap();
        assert!(ids.is_empty());
    }

    #[test]
    fn batch_ingest_dedup_across_objects() {
        let (plane, _dir) = temp_plane();
        let data = vec![0xFF; 200_000];

        let mut batch = BatchIngest::new(&plane);
        batch.add(&data, "application/octet-stream").unwrap();
        batch.add(&data, "application/octet-stream").unwrap();
        let ids = batch.commit().unwrap();

        assert_ne!(ids[0], ids[1]);

        let m1 = plane.get_manifest(ids[0]).unwrap().unwrap();
        let m2 = plane.get_manifest(ids[1]).unwrap().unwrap();
        assert_eq!(m1, m2, "identical data must have identical manifests");

        for hash in &m1 {
            let rc = plane.get_chunk_refcount(hash).unwrap();
            assert_eq!(rc, 2, "batch dedup must increment refcount for each object");
        }
    }

    #[test]
    fn object_sha256_matches_source_data() {
        let (plane, _dir) = temp_plane();
        let data = b"The quick brown fox jumps over the lazy dog";

        let mut session = begin_ingest(&plane, "text/plain");
        session.write(data);
        let object_id = commit_ingest(session).unwrap();

        let meta = plane.get_object(object_id).unwrap().unwrap();
        let expected: [u8; 32] = Sha256::digest(data).into();
        assert_eq!(
            meta.sha256, expected,
            "ObjectMetadata SHA-256 must match source data digest"
        );
    }

    #[test]
    fn sha256_streaming_matches_single_write() {
        let (plane, _dir) = temp_plane();
        let data: Vec<u8> = (0..200_000u32).map(|i| (i * 7 + 3) as u8).collect();

        // Single write
        let mut s1 = begin_ingest(&plane, "application/octet-stream");
        s1.write(&data);
        let id1 = commit_ingest(s1).unwrap();

        // Streaming write (3 pieces)
        let mut s2 = begin_ingest(&plane, "application/octet-stream");
        s2.write(&data[..80_000]);
        s2.write(&data[80_000..150_000]);
        s2.write(&data[150_000..]);
        let id2 = commit_ingest(s2).unwrap();

        let m1 = plane.get_object(id1).unwrap().unwrap();
        let m2 = plane.get_object(id2).unwrap().unwrap();
        assert_eq!(
            m1.sha256, m2.sha256,
            "SHA-256 must be identical for same data regardless of write pattern"
        );
    }

    #[test]
    fn compress_decompress_roundtrip() {
        let data = vec![0xCC; 4096];
        let compressed = compress_chunk(&data);
        let decompressed = decompress_chunk(&compressed).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn session_visible_during_ingest() {
        let (plane, _dir) = temp_plane();

        let mut session = begin_ingest(&plane, "text/plain");
        let oid = session.object_id();

        // Session should be visible in active_sessions
        let sessions = plane.active_sessions().unwrap();
        assert_eq!(sessions.len(), 1, "one active session expected");
        assert_eq!(sessions[0].0, oid);
        assert_eq!(sessions[0].1.mime, "text/plain");

        session.write(b"data");
        commit_ingest(session).unwrap();

        // After commit, no active sessions
        let sessions = plane.active_sessions().unwrap();
        assert!(sessions.is_empty(), "sessions must be empty after commit");
    }

    #[test]
    fn session_removed_after_abort() {
        let (plane, _dir) = temp_plane();

        let mut session = begin_ingest(&plane, "application/pdf");
        session.write(&vec![0xAA; 1000]);

        let sessions = plane.active_sessions().unwrap();
        assert_eq!(sessions.len(), 1);

        abort_ingest(session);

        let sessions = plane.active_sessions().unwrap();
        assert!(sessions.is_empty(), "sessions must be empty after abort");
    }

    #[test]
    fn session_progress_throttled() {
        let (plane, _dir) = temp_plane();

        let mut session = begin_ingest(&plane, "text/plain");
        let oid = session.object_id();

        // Small writes should NOT update the DB (below PROGRESS_FLUSH_BYTES threshold)
        session.write(&vec![0x11; 1000]);
        let state = plane.get_session(oid).unwrap().unwrap();
        assert_eq!(
            state.bytes_received, 0,
            "small write should not flush progress"
        );

        // Writing past threshold should flush
        session.write(&vec![0x22; PROGRESS_FLUSH_BYTES as usize]);
        let state = plane.get_session(oid).unwrap().unwrap();
        assert!(
            state.bytes_received > 0,
            "large write should flush progress"
        );

        commit_ingest(session).unwrap();
    }

    #[test]
    fn multiple_concurrent_sessions() {
        let (plane, _dir) = temp_plane();

        let mut s1 = begin_ingest(&plane, "text/plain");
        let mut s2 = begin_ingest(&plane, "application/pdf");

        s1.write(b"data1");
        s2.write(b"data2");

        let sessions = plane.active_sessions().unwrap();
        assert_eq!(sessions.len(), 2, "two concurrent sessions expected");

        let id1 = commit_ingest(s1).unwrap();
        let sessions = plane.active_sessions().unwrap();
        assert_eq!(sessions.len(), 1, "one session after first commit");

        abort_ingest(s2);
        let sessions = plane.active_sessions().unwrap();
        assert!(sessions.is_empty(), "no sessions after abort");

        // Only s1's object should exist
        assert!(plane.get_object(id1).unwrap().is_some());
    }
}
