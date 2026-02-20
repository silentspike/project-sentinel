//! Artifact Plane data model: 5 redb tables for content-defined chunked storage.
//!
//! Tables:
//! - `FS_OBJECTS`: ObjectId -> ObjectMetadata (size, mime, created_at, chunk_count)
//! - `FS_MANIFESTS`: ObjectId -> JSON-serialized Vec<[u8;32]> (ordered chunk list)
//! - `FS_CHUNKS`: &[u8;32] (SHA-256) -> zstd-compressed chunk data
//! - `FS_CHUNK_REFCOUNT`: &[u8;32] -> u32 (how many manifests reference this chunk)
//! - `FS_OBJECT_REFS`: &str (name) -> u64 (ObjectId, named references)

use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition};
use serde::{Deserialize, Serialize};
use std::time::SystemTime;

// --- Table Definitions ---

/// Object metadata: ObjectId -> JSON-serialized ObjectMetadata.
pub const FS_OBJECTS: TableDefinition<u64, &[u8]> = TableDefinition::new("fs_objects");

/// Manifests: ObjectId -> JSON-serialized list of chunk hashes (ordered).
pub const FS_MANIFESTS: TableDefinition<u64, &[u8]> = TableDefinition::new("fs_manifests");

/// Chunk data: SHA-256 hash -> zstd-compressed chunk bytes.
pub const FS_CHUNKS: TableDefinition<&[u8; 32], &[u8]> = TableDefinition::new("fs_chunks");

/// Chunk reference counts: SHA-256 hash -> refcount.
pub const FS_CHUNK_REFCOUNT: TableDefinition<&[u8; 32], u32> =
    TableDefinition::new("fs_chunk_refcount");

/// Named object references: name -> ObjectId.
pub const FS_OBJECT_REFS: TableDefinition<&str, u64> = TableDefinition::new("fs_object_refs");

// --- Types ---

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

/// A chunk hash (SHA-256, 32 bytes).
pub type ChunkHash = [u8; 32];

// --- ArtifactPlane ---

/// The Artifact Plane: wraps a redb Database and provides access to all 5 tables.
pub struct ArtifactPlane {
    pub(crate) db: Database,
}

impl ArtifactPlane {
    /// Open or create an ArtifactPlane database.
    pub fn open(path: impl AsRef<std::path::Path>) -> anyhow::Result<Self> {
        let db = Database::create(path.as_ref())
            .map_err(|e| anyhow::anyhow!("ArtifactPlane open: {e}"))?;

        // Initialize all tables in one transaction.
        let wtxn = db.begin_write()?;
        {
            wtxn.open_table(FS_OBJECTS)?;
            wtxn.open_table(FS_MANIFESTS)?;
            wtxn.open_table(FS_CHUNKS)?;
            wtxn.open_table(FS_CHUNK_REFCOUNT)?;
            wtxn.open_table(FS_OBJECT_REFS)?;
        }
        wtxn.commit()?;

        Ok(Self { db })
    }

    /// Allocate the next ObjectId (monotonic counter stored at key 0 in FS_OBJECTS using a
    /// separate counter key convention: we use u64::MAX as the counter slot).
    pub fn next_object_id(&self) -> anyhow::Result<u64> {
        let wtxn = self.db.begin_write()?;
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

    /// Check if a chunk exists.
    pub fn has_chunk(&self, hash: &ChunkHash) -> anyhow::Result<bool> {
        let rtxn = self.db.begin_read()?;
        let table = rtxn.open_table(FS_CHUNKS)?;
        Ok(table.get(hash)?.is_some())
    }

    /// Read raw (compressed) chunk bytes.
    pub fn read_chunk_raw(&self, hash: &ChunkHash) -> anyhow::Result<Vec<u8>> {
        let rtxn = self.db.begin_read()?;
        let table = rtxn.open_table(FS_CHUNKS)?;
        match table.get(hash)? {
            Some(g) => Ok(g.value().to_vec()),
            None => Err(anyhow::anyhow!(
                "Chunk {} not found",
                crate::cas::hex_encode(hash)
            )),
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
        let wtxn = self.db.begin_write()?;
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
}
