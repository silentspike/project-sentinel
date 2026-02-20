//! Artifact Plane data model: 6 redb tables for content-defined chunked storage.
//!
//! Tables:
//! - `FS_OBJECTS`: ObjectId -> ObjectMetadata (size, mime, created_at, chunk_count)
//! - `FS_MANIFESTS`: ObjectId -> JSON-serialized `Vec<[u8;16]>` (ordered chunk list)
//! - `FS_CHUNKS`: `[u8;16]` (BLAKE3-128) -> zstd-compressed chunk data
//! - `FS_CHUNK_REFCOUNT`: `[u8;16]` -> u32 (how many manifests reference this chunk)
//! - `FS_OBJECT_REFS`: &str (name) -> u64 (ObjectId, named references)
//! - `FS_INGEST_SESSIONS`: session_id (u64) -> JSON-serialized IngestSessionState

use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition, WriteTransaction};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Mutex;
use std::time::SystemTime;

use crate::segment::{ChunkLocation, SegmentStore};

/// Durability level for ArtifactPlane write transactions.
///
/// Controls the fsync behavior on commit:
/// - `Immediate` (default): every commit fsyncs to disk — safe against VM crashes.
/// - `Eventual`: commits skip fsync — significantly faster on VMs but data written
///   since the last `Immediate` commit may be lost on crash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurabilityLevel {
    /// Every commit is durable on disk (fsync). Default.
    Immediate,
    /// Commits skip fsync. Much faster on VMs, but not crash-safe.
    Eventual,
}

// --- Table Definitions ---

/// Object metadata: ObjectId -> JSON-serialized ObjectMetadata.
pub const FS_OBJECTS: TableDefinition<u64, &[u8]> = TableDefinition::new("fs_objects");

/// Manifests: ObjectId -> JSON-serialized list of chunk hashes (ordered).
pub const FS_MANIFESTS: TableDefinition<u64, &[u8]> = TableDefinition::new("fs_manifests");

/// Chunk index: BLAKE3-128 fingerprint -> ChunkLocation (segment_id + offset + len).
/// Actual compressed data is stored in segment pack files, not in redb.
pub const FS_CHUNKS: TableDefinition<&[u8; 16], &[u8]> = TableDefinition::new("fs_chunks");

/// Chunk reference counts: BLAKE3-128 fingerprint -> refcount.
pub const FS_CHUNK_REFCOUNT: TableDefinition<&[u8; 16], u32> =
    TableDefinition::new("fs_chunk_refcount");

/// Named object references: name -> ObjectId.
pub const FS_OBJECT_REFS: TableDefinition<&str, u64> = TableDefinition::new("fs_object_refs");

/// Ingest session tracking: session_id -> JSON-serialized IngestSessionState.
/// Shows active downloads as .part files in the FUSE layer.
pub const FS_INGEST_SESSIONS: TableDefinition<u64, &[u8]> =
    TableDefinition::new("fs_ingest_sessions");

// --- Types ---

/// State of an active ingest session, stored in FS_INGEST_SESSIONS.
/// Visible as `.part` files in the FUSE layer while streaming is in progress.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IngestSessionState {
    /// MIME type hint for the in-progress object.
    pub mime: String,
    /// Filename hint (shown as `{name}.part` in FUSE).
    pub name: String,
    /// Bytes received so far.
    pub bytes_received: u64,
    /// Unix epoch seconds when the session started.
    pub started_at: u64,
}

impl IngestSessionState {
    pub fn new(mime: impl Into<String>, name: impl Into<String>) -> Self {
        let started_at = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            mime: mime.into(),
            name: name.into(),
            bytes_received: 0,
            started_at,
        }
    }

    pub fn serialize(&self) -> anyhow::Result<Vec<u8>> {
        serde_json::to_vec(self).map_err(|e| anyhow::anyhow!("IngestSessionState serialize: {e}"))
    }

    pub fn deserialize(data: &[u8]) -> anyhow::Result<Self> {
        serde_json::from_slice(data)
            .map_err(|e| anyhow::anyhow!("IngestSessionState deserialize: {e}"))
    }
}

/// Metadata stored per object in FS_OBJECTS.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectMetadata {
    /// Original (uncompressed) size in bytes.
    pub size: u64,
    /// MIME type hint (e.g. "application/octet-stream", "text/html").
    pub mime: String,
    /// Unix epoch seconds of creation.
    pub created_at: u64,
    /// Number of chunks in the manifest.
    pub chunk_count: u32,
}

impl ObjectMetadata {
    pub fn new(size: u64, mime: impl Into<String>, chunk_count: u32) -> Self {
        let created_at = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            size,
            mime: mime.into(),
            created_at,
            chunk_count,
        }
    }

    pub fn serialize(&self) -> anyhow::Result<Vec<u8>> {
        serde_json::to_vec(self).map_err(|e| anyhow::anyhow!("ObjectMetadata serialize: {e}"))
    }

    pub fn deserialize(data: &[u8]) -> anyhow::Result<Self> {
        serde_json::from_slice(data).map_err(|e| anyhow::anyhow!("ObjectMetadata deserialize: {e}"))
    }
}

/// A chunk fingerprint (BLAKE3-128, 16 bytes).
pub type ChunkHash = [u8; 16];

// --- ArtifactPlane ---

/// The Artifact Plane: wraps a redb Database + SegmentStore for chunk storage.
pub struct ArtifactPlane {
    pub(crate) db: Database,
    durability: DurabilityLevel,
    /// Segment pack storage for chunk data (append-only files).
    /// Wrapped in Mutex for interior mutability (SegmentStore::append needs &mut).
    pub(crate) segments: Mutex<SegmentStore>,
}

impl ArtifactPlane {
    /// Open or create an ArtifactPlane database with `Immediate` durability.
    /// Segment packs are stored in a `segments/` subdirectory next to the redb file.
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        Self::open_with_durability(path, DurabilityLevel::Immediate)
    }

    /// Open or create an ArtifactPlane database with a specific durability level.
    pub fn open_with_durability(
        path: impl AsRef<Path>,
        durability: DurabilityLevel,
    ) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let db = Database::create(path)
            .map_err(|e| anyhow::anyhow!("ArtifactPlane open: {e}"))?;

        // Segment packs dir: sibling to the redb file
        let seg_dir = path.with_extension("segments");
        let segments = SegmentStore::open(&seg_dir)?;

        // Initialize all 6 tables in one transaction (always Immediate for schema init).
        let wtxn = db.begin_write()?;
        {
            wtxn.open_table(FS_OBJECTS)?;
            wtxn.open_table(FS_MANIFESTS)?;
            wtxn.open_table(FS_CHUNKS)?;
            wtxn.open_table(FS_CHUNK_REFCOUNT)?;
            wtxn.open_table(FS_OBJECT_REFS)?;
            wtxn.open_table(FS_INGEST_SESSIONS)?;
        }
        wtxn.commit()?;

        Ok(Self {
            db,
            durability,
            segments: Mutex::new(segments),
        })
    }

    /// Start a write transaction with the configured durability level.
    pub(crate) fn begin_write(&self) -> anyhow::Result<WriteTransaction> {
        let mut wtxn = self.db.begin_write()?;
        match self.durability {
            DurabilityLevel::Immediate => {} // redb default
            DurabilityLevel::Eventual => {
                wtxn.set_durability(redb::Durability::None)?;
            }
        }
        Ok(wtxn)
    }

    /// Allocate the next ObjectId (monotonic counter stored at key 0 in FS_OBJECTS using a
    /// separate counter key convention: we use u64::MAX as the counter slot).
    pub fn next_object_id(&self) -> anyhow::Result<u64> {
        let wtxn = self.begin_write()?;
        let id = {
            let mut table = wtxn.open_table(FS_OBJECTS)?;
            // Use key u64::MAX as counter slot (never a real ObjectId)
            let current = table
                .get(u64::MAX)?
                .map(|g| {
                    let bytes = g.value();
                    if bytes.len() == 8 {
                        u64::from_le_bytes(bytes.try_into().unwrap())
                    } else {
                        0
                    }
                })
                .unwrap_or(0);
            let next = current + 1;
            table.insert(u64::MAX, next.to_le_bytes().as_slice())?;
            next
        };
        wtxn.commit()?;
        Ok(id)
    }

    /// Get object metadata by ObjectId.
    pub fn get_object(&self, object_id: u64) -> anyhow::Result<Option<ObjectMetadata>> {
        let rtxn = self.db.begin_read()?;
        let table = rtxn.open_table(FS_OBJECTS)?;
        match table.get(object_id)? {
            Some(g) => Ok(Some(ObjectMetadata::deserialize(g.value())?)),
            None => Ok(None),
        }
    }

    /// Get ordered chunk hashes for an object.
    pub fn get_manifest(&self, object_id: u64) -> anyhow::Result<Option<Vec<ChunkHash>>> {
        let rtxn = self.db.begin_read()?;
        let table = rtxn.open_table(FS_MANIFESTS)?;
        match table.get(object_id)? {
            Some(g) => {
                let hashes: Vec<ChunkHash> = serde_json::from_slice(g.value())
                    .map_err(|e| anyhow::anyhow!("manifest deserialize: {e}"))?;
                Ok(Some(hashes))
            }
            None => Ok(None),
        }
    }

    /// Check if a chunk exists in the index.
    pub fn has_chunk(&self, hash: &ChunkHash) -> anyhow::Result<bool> {
        let rtxn = self.db.begin_read()?;
        let table = rtxn.open_table(FS_CHUNKS)?;
        Ok(table.get(hash)?.is_some())
    }

    /// Read raw (compressed) chunk bytes from the segment store.
    pub fn read_chunk_raw(&self, hash: &ChunkHash) -> anyhow::Result<Vec<u8>> {
        let rtxn = self.db.begin_read()?;
        let table = rtxn.open_table(FS_CHUNKS)?;
        let loc_bytes = table
            .get(hash)?
            .ok_or_else(|| {
                anyhow::anyhow!("Chunk {} not found", crate::cas::hex_encode(hash))
            })?;
        let loc = ChunkLocation::from_bytes(loc_bytes.value())?;
        let segments = self.segments.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        segments.read(&loc)
    }

    /// Get the ChunkLocation for an index entry (for direct segment reads).
    pub fn get_chunk_location(&self, hash: &ChunkHash) -> anyhow::Result<Option<ChunkLocation>> {
        let rtxn = self.db.begin_read()?;
        let table = rtxn.open_table(FS_CHUNKS)?;
        match table.get(hash)? {
            Some(g) => Ok(Some(ChunkLocation::from_bytes(g.value())?)),
            None => Ok(None),
        }
    }

    /// Get chunk refcount.
    pub fn get_chunk_refcount(&self, hash: &ChunkHash) -> anyhow::Result<u32> {
        let rtxn = self.db.begin_read()?;
        let table = rtxn.open_table(FS_CHUNK_REFCOUNT)?;
        Ok(table.get(hash)?.map(|g| g.value()).unwrap_or(0))
    }

    /// Resolve a named reference to an ObjectId.
    pub fn resolve_ref(&self, name: &str) -> anyhow::Result<Option<u64>> {
        let rtxn = self.db.begin_read()?;
        let table = rtxn.open_table(FS_OBJECT_REFS)?;
        Ok(table.get(name)?.map(|g| g.value()))
    }

    /// Set a named reference.
    pub fn set_ref(&self, name: &str, object_id: u64) -> anyhow::Result<()> {
        let wtxn = self.begin_write()?;
        {
            let mut table = wtxn.open_table(FS_OBJECT_REFS)?;
            table.insert(name, object_id)?;
        }
        wtxn.commit()?;
        Ok(())
    }

    /// Count total unique chunks in FS_CHUNKS.
    pub fn chunk_count(&self) -> anyhow::Result<u64> {
        let rtxn = self.db.begin_read()?;
        let table = rtxn.open_table(FS_CHUNKS)?;
        Ok(table.len()?)
    }

    // --- Ingest Session Tracking ---

    /// Register a new ingest session (shown as .part in FUSE).
    pub fn register_session(&self, session_id: u64, state: &IngestSessionState) -> anyhow::Result<()> {
        let wtxn = self.begin_write()?;
        {
            let mut table = wtxn.open_table(FS_INGEST_SESSIONS)?;
            table.insert(session_id, state.serialize()?.as_slice())?;
        }
        wtxn.commit()?;
        Ok(())
    }

    /// Update bytes_received for an active session.
    pub fn update_session_progress(&self, session_id: u64, bytes_received: u64) -> anyhow::Result<()> {
        // Read current state first (separate scope to drop borrow)
        let updated = {
            let rtxn = self.db.begin_read()?;
            let table = rtxn.open_table(FS_INGEST_SESSIONS)?;
            match table.get(session_id)? {
                Some(g) => {
                    let mut state = IngestSessionState::deserialize(g.value())?;
                    state.bytes_received = bytes_received;
                    Some(state)
                }
                None => None,
            }
        };
        if let Some(state) = updated {
            let wtxn = self.begin_write()?;
            {
                let mut table = wtxn.open_table(FS_INGEST_SESSIONS)?;
                table.insert(session_id, state.serialize()?.as_slice())?;
            }
            wtxn.commit()?;
        }
        Ok(())
    }

    /// Remove a session (on commit or abort).
    pub fn remove_session(&self, session_id: u64) -> anyhow::Result<()> {
        let wtxn = self.begin_write()?;
        {
            let mut table = wtxn.open_table(FS_INGEST_SESSIONS)?;
            table.remove(session_id)?;
        }
        wtxn.commit()?;
        Ok(())
    }

    /// List all active ingest sessions (for FUSE .part visibility).
    pub fn active_sessions(&self) -> anyhow::Result<Vec<(u64, IngestSessionState)>> {
        let rtxn = self.db.begin_read()?;
        let table = rtxn.open_table(FS_INGEST_SESSIONS)?;
        let mut sessions = Vec::new();
        for entry in table.iter()? {
            let (key, val) = entry?;
            let state = IngestSessionState::deserialize(val.value())?;
            sessions.push((key.value(), state));
        }
        Ok(sessions)
    }

    /// Get a specific session's state.
    pub fn get_session(&self, session_id: u64) -> anyhow::Result<Option<IngestSessionState>> {
        let rtxn = self.db.begin_read()?;
        let table = rtxn.open_table(FS_INGEST_SESSIONS)?;
        match table.get(session_id)? {
            Some(g) => Ok(Some(IngestSessionState::deserialize(g.value())?)),
            None => Ok(None),
        }
    }

    /// Get all chunk hashes with refcount == 0 (GC candidates).
    /// Since we remove zero-ref entries on decrement, this scans FS_CHUNKS for
    /// hashes not present in FS_CHUNK_REFCOUNT.
    pub fn zero_ref_chunks(&self) -> anyhow::Result<Vec<ChunkHash>> {
        let rtxn = self.db.begin_read()?;
        let chunks_table = rtxn.open_table(FS_CHUNKS)?;
        let refcount_table = rtxn.open_table(FS_CHUNK_REFCOUNT)?;

        let mut orphans = Vec::new();
        for entry in chunks_table.iter()? {
            let (key, _val) = entry?;
            let hash: ChunkHash = *key.value();
            if refcount_table.get(&hash)?.is_none() {
                orphans.push(hash);
            }
        }
        Ok(orphans)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_plane() -> (ArtifactPlane, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let plane = ArtifactPlane::open(dir.path().join("artifact.redb")).unwrap();
        (plane, dir)
    }

    #[test]
    fn next_object_id_sequential() {
        let (plane, _dir) = temp_plane();
        assert_eq!(plane.next_object_id().unwrap(), 1);
        assert_eq!(plane.next_object_id().unwrap(), 2);
        assert_eq!(plane.next_object_id().unwrap(), 3);
    }

    #[test]
    fn object_metadata_roundtrip() {
        let meta = ObjectMetadata::new(1024, "text/plain", 16);
        let bytes = meta.serialize().unwrap();
        let decoded = ObjectMetadata::deserialize(&bytes).unwrap();
        assert_eq!(decoded.size, 1024);
        assert_eq!(decoded.mime, "text/plain");
        assert_eq!(decoded.chunk_count, 16);
    }

    #[test]
    fn named_refs_roundtrip() {
        let (plane, _dir) = temp_plane();
        plane.set_ref("latest", 42).unwrap();
        assert_eq!(plane.resolve_ref("latest").unwrap(), Some(42));
        assert_eq!(plane.resolve_ref("unknown").unwrap(), None);
    }

    #[test]
    fn open_with_eventual_durability() {
        let dir = tempfile::tempdir().unwrap();
        let plane = ArtifactPlane::open_with_durability(
            dir.path().join("eventual.redb"),
            DurabilityLevel::Eventual,
        )
        .unwrap();
        assert_eq!(plane.durability, DurabilityLevel::Eventual);
        // Writes still work (just without fsync)
        let id = plane.next_object_id().unwrap();
        assert_eq!(id, 1);
    }
}
