//! Garbage collection for the Artifact Plane chunk store.
//!
//! `gc_chunks` removes chunks from `FS_CHUNKS` whose refcount in
//! `FS_CHUNK_REFCOUNT` is zero (i.e., no manifest references them).
//! This integrates with the existing CasStore GC pattern.

use crate::artifact::{ArtifactPlane, FS_CHUNK_REFCOUNT, FS_CHUNKS};
use crate::cas::ChunkGcStats;
use redb::ReadableTable;

/// Run GC on the Artifact Plane: delete all chunks with refcount == 0.
///
/// Since zero-refcount entries are removed from FS_CHUNK_REFCOUNT on
/// decrement, "orphan" chunks are those present in FS_CHUNKS but absent
/// from FS_CHUNK_REFCOUNT.
pub fn gc_chunks(plane: &ArtifactPlane) -> anyhow::Result<ChunkGcStats> {
    // Collect orphans in a read transaction first
    let orphans = plane.zero_ref_chunks()?;

    if orphans.is_empty() {
        return Ok(ChunkGcStats::default());
    }

    let mut stats = ChunkGcStats::default();

    // Delete in a write transaction
    let wtxn = plane.begin_write()?;
    {
        let mut chunks_table = wtxn.open_table(FS_CHUNKS)?;
        // Also clean up any zero-value refcount entries that might exist
        let mut refcount_table = wtxn.open_table(FS_CHUNK_REFCOUNT)?;

        for hash in &orphans {
            // Measure the on-disk size before deletion (approximation: compressed bytes)
            if let Some(g) = chunks_table.get(hash)? {
                let data: &[u8] = g.value();
                stats.freed_bytes += data.len() as u64;
            }
            chunks_table.remove(hash)?;
            // Also remove any stale zero-refcount entry if present
            refcount_table.remove(hash)?;
            stats.removed += 1;
        }
    }
    wtxn.commit()?;

    Ok(stats)
}

/// Decrement the refcount for all chunks in an object's manifest and
/// optionally remove the manifest + object metadata.
///
/// Call this when an object is deleted to maintain refcount invariants.
pub fn release_object(plane: &ArtifactPlane, object_id: u64) -> anyhow::Result<()> {
    let manifest = match plane.get_manifest(object_id)? {
        Some(m) => m,
        None => return Ok(()), // already gone
    };

    let wtxn = plane.begin_write()?;
    {
        let mut refcount_table = wtxn.open_table(FS_CHUNK_REFCOUNT)?;
        let mut manifests_table = wtxn.open_table(crate::artifact::FS_MANIFESTS)?;
        let mut objects_table = wtxn.open_table(crate::artifact::FS_OBJECTS)?;

        // Decrement refcounts
        for hash in &manifest {
            let current: u32 = refcount_table.get(hash)?.map(|g| g.value()).unwrap_or(0);
            if current <= 1 {
                // Drop to zero: remove the entry entirely
                refcount_table.remove(hash)?;
            } else {
                refcount_table.insert(hash, current - 1)?;
            }
        }

        // Remove manifest and object metadata
        manifests_table.remove(object_id)?;
        objects_table.remove(object_id)?;
    }
    wtxn.commit()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::ArtifactPlane;
    use crate::ingest::{begin_ingest, commit_ingest};

    fn temp_plane() -> (ArtifactPlane, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let plane = ArtifactPlane::open(dir.path().join("gc_test.redb")).unwrap();
        (plane, dir)
    }

    #[test]
    fn gc_no_orphans() {
        let (plane, _dir) = temp_plane();

        let mut s = begin_ingest(&plane, "text/plain");
        s.write(&vec![0xAA; 100_000]);
        commit_ingest(s).unwrap();

        let stats = gc_chunks(&plane).unwrap();
        assert_eq!(stats.removed, 0, "no orphans after fresh ingest");
    }

    #[test]
    fn gc_removes_orphaned_chunks_after_release() {
        let (plane, _dir) = temp_plane();

        let mut s = begin_ingest(&plane, "text/plain");
        s.write(&vec![0xBB; 100_000]);
        let id = commit_ingest(s).unwrap();

        let manifest = plane.get_manifest(id).unwrap().unwrap();
        let chunk_count = manifest.len();
        assert!(chunk_count > 0);

        // Verify chunks exist
        for h in &manifest {
            assert!(plane.has_chunk(h).unwrap());
        }

        // Release the object
        release_object(&plane, id).unwrap();

        // Verify refcounts are zero
        for h in &manifest {
            assert_eq!(plane.get_chunk_refcount(h).unwrap(), 0);
        }

        // GC should remove them
        let stats = gc_chunks(&plane).unwrap();
        assert_eq!(
            stats.removed, chunk_count as u64,
            "GC must remove all orphaned chunks"
        );
        assert!(stats.freed_bytes > 0);

        // Chunks are gone
        for h in &manifest {
            assert!(!plane.has_chunk(h).unwrap());
        }
    }

    #[test]
    fn gc_preserves_shared_chunks() {
        let (plane, _dir) = temp_plane();
        let data = vec![0xCC; 150_000];

        let mut s1 = begin_ingest(&plane, "text/plain");
        s1.write(&data);
        let id1 = commit_ingest(s1).unwrap();

        let mut s2 = begin_ingest(&plane, "text/plain");
        s2.write(&data);
        let id2 = commit_ingest(s2).unwrap();

        // Release only id1
        release_object(&plane, id1).unwrap();

        // GC: chunks still referenced by id2, so nothing should be removed
        let stats = gc_chunks(&plane).unwrap();
        assert_eq!(stats.removed, 0, "shared chunks must not be GC'd");

        // Chunks still accessible via id2
        let manifest2 = plane.get_manifest(id2).unwrap().unwrap();
        for h in &manifest2 {
            assert!(plane.has_chunk(h).unwrap());
        }

        // Release id2 too — now chunks become orphans
        release_object(&plane, id2).unwrap();
        let stats = gc_chunks(&plane).unwrap();
        assert!(stats.removed > 0, "after releasing both, GC must find orphans");
    }

    #[test]
    fn release_nonexistent_object_is_noop() {
        let (plane, _dir) = temp_plane();
        // Must not panic or error
        release_object(&plane, 99999).unwrap();
    }
}
