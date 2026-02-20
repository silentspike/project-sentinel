//! Read planner: reassemble an object from its chunks via streaming.
//!
//! `read_object` returns the full decompressed content by reading the manifest
//! and concatenating all chunks in order. The reassembly is streaming (chunks
//! are decompressed one at a time), keeping peak memory proportional to the
//! largest single chunk rather than the full object size.

use crate::artifact::{ArtifactPlane, ChunkHash, FS_CHUNKS};
use crate::ingest::decompress_chunk;
use redb::ReadableDatabase;

/// Read an object's full content by ObjectId.
///
/// Reads the manifest, decompresses each chunk in order, and returns
/// the concatenated result.
pub fn read_object(plane: &ArtifactPlane, object_id: u64) -> anyhow::Result<Vec<u8>> {
    let meta = plane
        .get_object(object_id)?
        .ok_or_else(|| anyhow::anyhow!("Object {object_id} not found"))?;

    let manifest = plane
        .get_manifest(object_id)?
        .ok_or_else(|| anyhow::anyhow!("Manifest for object {object_id} not found"))?;

    let mut result = Vec::with_capacity(meta.size as usize);

    let rtxn = plane.db.begin_read()?;
    let chunks_table = rtxn.open_table(FS_CHUNKS)?;

    for hash in &manifest {
        let compressed = chunks_table
            .get(hash)?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Chunk {} missing for object {object_id}",
                    crate::cas::hex_encode(hash)
                )
            })?;
        let chunk_data = decompress_chunk(compressed.value())?;
        result.extend_from_slice(&chunk_data);
    }

    Ok(result)
}

/// Read object content using a streaming iterator over decompressed chunks.
///
/// Returns an iterator that yields decompressed `Vec<u8>` per chunk.
/// This avoids loading the whole object into memory at once.
pub fn read_object_streaming(
    plane: &ArtifactPlane,
    object_id: u64,
) -> anyhow::Result<impl Iterator<Item = anyhow::Result<Vec<u8>>> + '_> {
    let manifest = plane
        .get_manifest(object_id)?
        .ok_or_else(|| anyhow::anyhow!("Manifest for object {object_id} not found"))?;

    Ok(ChunkStream {
        plane,
        manifest,
        index: 0,
    })
}

/// Iterator that decompresses one chunk at a time.
struct ChunkStream<'a> {
    plane: &'a ArtifactPlane,
    manifest: Vec<ChunkHash>,
    index: usize,
}

impl Iterator for ChunkStream<'_> {
    type Item = anyhow::Result<Vec<u8>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index >= self.manifest.len() {
            return None;
        }
        let hash = &self.manifest[self.index];
        self.index += 1;

        let result = (|| {
            let compressed = self.plane.read_chunk_raw(hash)?;
            decompress_chunk(&compressed)
        })();
        Some(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::{begin_ingest, commit_ingest};

    fn temp_plane() -> (ArtifactPlane, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let plane = ArtifactPlane::open(dir.path().join("read_test.redb")).unwrap();
        (plane, dir)
    }

    #[test]
    fn read_object_roundtrip_small() {
        let (plane, _dir) = temp_plane();
        let data = b"hello from the read planner";

        let mut s = begin_ingest(&plane, "text/plain");
        s.write(data);
        let id = commit_ingest(s).unwrap();

        let read_back = read_object(&plane, id).unwrap();
        assert_eq!(read_back, data);
    }

    #[test]
    fn read_object_roundtrip_large() {
        let (plane, _dir) = temp_plane();
        let data: Vec<u8> = (0..500_000u32).map(|i| (i * 7 + 3) as u8).collect();

        let mut s = begin_ingest(&plane, "application/octet-stream");
        s.write(&data);
        let id = commit_ingest(s).unwrap();

        let read_back = read_object(&plane, id).unwrap();
        assert_eq!(read_back, data);
    }

    #[test]
    fn read_nonexistent_object_errors() {
        let (plane, _dir) = temp_plane();
        assert!(read_object(&plane, 99999).is_err());
    }

    #[test]
    fn streaming_reader_produces_same_content() {
        let (plane, _dir) = temp_plane();
        let data: Vec<u8> = (0..300_000u32).map(|i| (i * 11 + 5) as u8).collect();

        let mut s = begin_ingest(&plane, "application/octet-stream");
        s.write(&data);
        let id = commit_ingest(s).unwrap();

        // Bulk read
        let bulk = read_object(&plane, id).unwrap();

        // Streaming read
        let streaming: Vec<u8> = read_object_streaming(&plane, id)
            .unwrap()
            .flat_map(|r| r.unwrap())
            .collect();

        assert_eq!(bulk, streaming);
        assert_eq!(streaming, data);
    }

    #[test]
    fn multi_format_ingest_and_read() {
        let (plane, _dir) = temp_plane();

        // Simulate ISO: incompressible high-entropy data
        let iso_data: Vec<u8> = (0..200_000u32).map(|i| ((i * 7919 + 12347) % 256) as u8).collect();
        let mut s = begin_ingest(&plane, "application/x-iso9660-image");
        s.write(&iso_data);
        let iso_id = commit_ingest(s).unwrap();

        // Simulate HTML: compressible text
        let html_data = b"<!DOCTYPE html><html><body>".repeat(10000);
        let mut s = begin_ingest(&plane, "text/html");
        s.write(&html_data);
        let html_id = commit_ingest(s).unwrap();

        // Simulate PDF: mixed data
        let pdf_data: Vec<u8> = (0..150_000u32)
            .flat_map(|i| {
                if i % 3 == 0 {
                    vec![(i % 256) as u8; 1]
                } else {
                    vec![0x25u8, 0x50u8] // PDF magic
                }
            })
            .collect();
        let mut s = begin_ingest(&plane, "application/pdf");
        s.write(&pdf_data);
        let pdf_id = commit_ingest(s).unwrap();

        // All must round-trip correctly
        assert_eq!(read_object(&plane, iso_id).unwrap(), iso_data);
        assert_eq!(read_object(&plane, html_id).unwrap(), html_data.to_vec());
        assert_eq!(read_object(&plane, pdf_id).unwrap(), pdf_data);
    }
}
