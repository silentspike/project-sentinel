//! Segment Pack storage: append-only files for chunk data.
//!
//! Chunks are packed sequentially into ~64 MB segment files on disk.
//! redb stores only the index (`ChunkHash -> ChunkLocation`), not the data.
//!
//! **Write path:** compress chunk → append to current segment → record offset.
//! **Read path:** index lookup → `pread()` from segment file → decompress.
//!
//! If the process crashes between appending and committing the redb index,
//! the appended bytes become dead space. Segment compaction reclaims it.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Segment file magic bytes.
const SEGMENT_MAGIC: &[u8; 4] = b"SPCK";

/// Segment file version.
const SEGMENT_VERSION: u32 = 1;

/// Header size: 4 (magic) + 4 (version) + 8 (reserved) = 16 bytes.
const SEGMENT_HEADER_SIZE: u64 = 16;

/// Default target segment size before sealing and starting a new one.
pub const DEFAULT_SEGMENT_TARGET_BYTES: u64 = 64 * 1024 * 1024; // 64 MB

/// Location of a chunk within the segment store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkLocation {
    pub segment_id: u32,
    pub offset: u64,
    pub compressed_len: u32,
}

impl ChunkLocation {
    /// Serialize to a fixed 16-byte representation for redb storage.
    pub fn to_bytes(&self) -> [u8; 16] {
        let mut buf = [0u8; 16];
        buf[0..4].copy_from_slice(&self.segment_id.to_le_bytes());
        buf[4..12].copy_from_slice(&self.offset.to_le_bytes());
        buf[12..16].copy_from_slice(&self.compressed_len.to_le_bytes());
        buf
    }

    /// Deserialize from the 16-byte redb representation.
    pub fn from_bytes(bytes: &[u8]) -> anyhow::Result<Self> {
        if bytes.len() < 16 {
            anyhow::bail!("ChunkLocation needs 16 bytes, got {}", bytes.len());
        }
        Ok(Self {
            segment_id: u32::from_le_bytes(bytes[0..4].try_into().unwrap()),
            offset: u64::from_le_bytes(bytes[4..12].try_into().unwrap()),
            compressed_len: u32::from_le_bytes(bytes[12..16].try_into().unwrap()),
        })
    }
}

/// Manages a directory of segment pack files.
pub struct SegmentStore {
    dir: PathBuf,
    target_bytes: u64,
    /// Current active segment for writes.
    writer: Option<ActiveSegment>,
}

/// The currently active (unsealed) segment.
struct ActiveSegment {
    id: u32,
    file: File,
    write_offset: u64,
}

impl SegmentStore {
    /// Open or create a segment store in `dir`.
    pub fn open(dir: impl AsRef<Path>) -> anyhow::Result<Self> {
        Self::open_with_target(dir, DEFAULT_SEGMENT_TARGET_BYTES)
    }

    /// Open with a custom target segment size.
    pub fn open_with_target(dir: impl AsRef<Path>, target_bytes: u64) -> anyhow::Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir)?;

        Ok(Self {
            dir,
            target_bytes,
            writer: None,
        })
    }

    /// Append compressed chunk data to the current segment.
    /// Returns the `ChunkLocation` for the index.
    pub fn append(&mut self, compressed: &[u8]) -> anyhow::Result<ChunkLocation> {
        let len = compressed.len() as u32;

        // Ensure we have an active segment, potentially rotating if full.
        let seg = self.ensure_active_segment()?;

        let offset = seg.write_offset;
        seg.file.write_all(compressed)?;
        seg.write_offset += len as u64;

        let loc = ChunkLocation {
            segment_id: seg.id,
            offset,
            compressed_len: len,
        };

        // Check if segment is full — seal on next append.
        // (We don't seal mid-append to keep the current write simple.)

        Ok(loc)
    }

    /// Read compressed chunk data from a segment.
    pub fn read(&self, loc: &ChunkLocation) -> anyhow::Result<Vec<u8>> {
        let path = self.segment_path(loc.segment_id);
        let mut file = File::open(&path).map_err(|e| {
            anyhow::anyhow!(
                "Segment {} not found at {}: {e}",
                loc.segment_id,
                path.display()
            )
        })?;

        let mut buf = vec![0u8; loc.compressed_len as usize];
        file.seek(SeekFrom::Start(loc.offset))?;
        file.read_exact(&mut buf)?;
        Ok(buf)
    }

    /// Fsync the current active segment to disk.
    pub fn sync(&self) -> anyhow::Result<()> {
        if let Some(ref seg) = self.writer {
            seg.file.sync_data()?;
        }
        Ok(())
    }

    /// List all segment IDs on disk.
    pub fn segment_ids(&self) -> anyhow::Result<Vec<u32>> {
        let mut ids = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(id_str) = name.strip_prefix("seg_").and_then(|s| s.strip_suffix(".pack")) {
                if let Ok(id) = u32::from_str_radix(id_str, 16) {
                    ids.push(id);
                }
            }
        }
        ids.sort();
        Ok(ids)
    }

    /// Delete a segment file (used during compaction).
    pub fn remove_segment(&self, segment_id: u32) -> anyhow::Result<()> {
        let path = self.segment_path(segment_id);
        if path.exists() {
            fs::remove_file(&path)?;
        }
        Ok(())
    }

    /// Get the file size of a segment (for GC statistics).
    pub fn segment_size(&self, segment_id: u32) -> anyhow::Result<u64> {
        let path = self.segment_path(segment_id);
        Ok(fs::metadata(&path).map(|m| m.len()).unwrap_or(0))
    }

    /// Path for a segment file.
    fn segment_path(&self, id: u32) -> PathBuf {
        self.dir.join(format!("seg_{id:08x}.pack"))
    }

    /// Get or create the active segment for writing.
    fn ensure_active_segment(&mut self) -> anyhow::Result<&mut ActiveSegment> {
        // Check if current segment is full
        let needs_new = match &self.writer {
            Some(seg) => seg.write_offset >= self.target_bytes,
            None => true,
        };

        if needs_new {
            let id = self.next_segment_id()?;
            let path = self.segment_path(id);
            let mut file = OpenOptions::new()
                .create(true)
                .truncate(false) // preserve existing data if re-opening
                .write(true)
                .read(true)
                .open(&path)?;

            // Write header
            file.write_all(SEGMENT_MAGIC)?;
            file.write_all(&SEGMENT_VERSION.to_le_bytes())?;
            file.write_all(&[0u8; 8])?; // reserved

            self.writer = Some(ActiveSegment {
                id,
                file,
                write_offset: SEGMENT_HEADER_SIZE,
            });
        }

        Ok(self.writer.as_mut().unwrap())
    }

    /// Find the next segment ID (max existing + 1, or 0).
    fn next_segment_id(&self) -> anyhow::Result<u32> {
        let ids = self.segment_ids()?;
        Ok(ids.last().map(|&id| id + 1).unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_location_roundtrip() {
        let loc = ChunkLocation {
            segment_id: 42,
            offset: 1234567890,
            compressed_len: 65536,
        };
        let bytes = loc.to_bytes();
        let decoded = ChunkLocation::from_bytes(&bytes).unwrap();
        assert_eq!(loc, decoded);
    }

    #[test]
    fn append_and_read() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SegmentStore::open(dir.path().join("segments")).unwrap();

        let data = b"hello segment world";
        let loc = store.append(data).unwrap();

        assert_eq!(loc.segment_id, 0);
        assert_eq!(loc.offset, SEGMENT_HEADER_SIZE);
        assert_eq!(loc.compressed_len, data.len() as u32);

        let read_back = store.read(&loc).unwrap();
        assert_eq!(read_back, data);
    }

    #[test]
    fn multiple_appends_sequential() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SegmentStore::open(dir.path().join("segments")).unwrap();

        let data1 = vec![0xAAu8; 1000];
        let data2 = vec![0xBBu8; 2000];
        let data3 = vec![0xCCu8; 3000];

        let loc1 = store.append(&data1).unwrap();
        let loc2 = store.append(&data2).unwrap();
        let loc3 = store.append(&data3).unwrap();

        // All in same segment
        assert_eq!(loc1.segment_id, loc2.segment_id);
        assert_eq!(loc2.segment_id, loc3.segment_id);

        // Sequential offsets
        assert_eq!(loc1.offset, SEGMENT_HEADER_SIZE);
        assert_eq!(loc2.offset, loc1.offset + 1000);
        assert_eq!(loc3.offset, loc2.offset + 2000);

        // Read back all
        assert_eq!(store.read(&loc1).unwrap(), data1);
        assert_eq!(store.read(&loc2).unwrap(), data2);
        assert_eq!(store.read(&loc3).unwrap(), data3);
    }

    #[test]
    fn segment_rotation() {
        let dir = tempfile::tempdir().unwrap();
        // Target 64 bytes: header=16 + 80 data = 96 > 64 → seal after first append
        let mut store =
            SegmentStore::open_with_target(dir.path().join("segments"), 64).unwrap();

        let data = vec![0xDD; 80];
        let loc1 = store.append(&data).unwrap();
        assert_eq!(loc1.segment_id, 0);

        // write_offset is now 96 >= 64, so next append triggers rotation to segment 1
        let loc2 = store.append(&data).unwrap();
        assert_eq!(loc2.segment_id, 1);

        // Both readable
        assert_eq!(store.read(&loc1).unwrap(), data);
        assert_eq!(store.read(&loc2).unwrap(), data);

        let ids = store.segment_ids().unwrap();
        assert_eq!(ids, vec![0, 1]);
    }

    #[test]
    fn segment_list_and_remove() {
        let dir = tempfile::tempdir().unwrap();
        // Target 64: first 80-byte append fills past target, second goes to new segment
        let mut store =
            SegmentStore::open_with_target(dir.path().join("segments"), 64).unwrap();

        store.append(&[1; 80]).unwrap();
        store.append(&[2; 80]).unwrap(); // triggers rotation

        let ids = store.segment_ids().unwrap();
        assert_eq!(ids.len(), 2);

        store.remove_segment(ids[0]).unwrap();
        let ids = store.segment_ids().unwrap();
        assert_eq!(ids.len(), 1);
    }
}
