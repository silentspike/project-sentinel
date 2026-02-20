//! Content-Defined Chunking (CDC) with a rolling Rabin-style hash.
//!
//! Implements a simple but fast gear-hash based CDC. The same input always
//! produces the same chunk boundaries (deterministic). Target chunk size is
//! configurable; min/max bounds prevent degenerate splits.
//!
//! Typical settings: min=16KB, target=64KB, max=256KB.

/// Default chunking parameters.
pub const MIN_CHUNK_BYTES: usize = 16_384; // 16 KB
pub const TARGET_CHUNK_BYTES: usize = 65_536; // 64 KB
pub const MAX_CHUNK_BYTES: usize = 262_144; // 256 KB

/// BLAKE3-128 fingerprint of a chunk's uncompressed content.
/// Truncated to 16 bytes for fast index keys; SHA-256 is kept at the object
/// level for compliance/integrity.
pub type ChunkHash = [u8; 16];

/// A single chunk produced by the CDC.
#[derive(Debug, Clone)]
pub struct Chunk {
    pub hash: ChunkHash,
    /// Raw (uncompressed) chunk data.
    pub data: Vec<u8>,
}

// Gear table: 256 pseudo-random u64 values used as the rolling hash.
// These are fixed constants so chunking is deterministic across runs.
const GEAR: [u64; 256] = {
    // Generated from a fixed seed using a simple LCG for reproducibility.
    // IMPORTANT: Never change these values — doing so invalidates all existing chunk hashes!
    let mut table = [0u64; 256];
    let mut i = 0usize;
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    while i < 256 {
        // xorshift64*
        state ^= state << 12;
        state ^= state >> 25;
        state ^= state << 27;
        table[i] = state.wrapping_mul(0x2545_f491_4f6c_dd1d);
        i += 1;
    }
    table
};

/// Mask derived from target chunk size. We use popcount(target-1) bits.
/// For target=64KB (0xFFFF), mask has 16 bits set.
fn make_mask(target: usize) -> u64 {
    // Round up to next power of two, then subtract 1 to get all-ones mask.
    let pot = target.next_power_of_two();
    (pot - 1) as u64
}

/// Split `data` into content-defined chunks.
///
/// Returns an iterator of `Chunk` values. The chunking is fully deterministic:
/// identical input always produces identical chunk boundaries and hashes.
pub fn chunk_data(data: &[u8]) -> ChunkIter<'_> {
    ChunkIter::new(data, MIN_CHUNK_BYTES, TARGET_CHUNK_BYTES, MAX_CHUNK_BYTES)
}

/// Split `data` into content-defined chunks with **parallel** BLAKE3 hashing.
///
/// CDC boundary detection is serial (rolling hash), but the expensive BLAKE3
/// fingerprinting runs in parallel across all chunks via rayon.
pub fn chunk_data_parallel(data: &[u8]) -> Vec<Chunk> {
    use rayon::prelude::*;

    // Phase 1: Serial CDC boundary detection (no hashing)
    let mut boundaries = Vec::new();
    let mut pos = 0;
    let min_size = MIN_CHUNK_BYTES;
    let mask = make_mask(TARGET_CHUNK_BYTES);
    let max_size = MAX_CHUNK_BYTES;

    while pos < data.len() {
        let start = pos;
        let remaining = data.len() - start;

        if remaining <= max_size {
            boundaries.push((start, start + remaining));
            break;
        }

        let mut fp: u64 = 0;
        let end = std::cmp::min(start + max_size, data.len());
        let min_end = start + min_size;
        let mut split = end;
        for i in start..end {
            fp = (fp << 1).wrapping_add(GEAR[data[i] as usize]);
            if i >= min_end && (fp & mask) == 0 {
                split = i + 1;
                break;
            }
        }
        boundaries.push((start, split));
        pos = split;
    }

    // Phase 2: Parallel BLAKE3 hashing + data copy
    boundaries
        .par_iter()
        .map(|&(start, end)| {
            let chunk_data = data[start..end].to_vec();
            let hash = blake3_hash_128(&chunk_data);
            Chunk {
                hash,
                data: chunk_data,
            }
        })
        .collect()
}

/// CDC iterator over a byte slice.
pub struct ChunkIter<'a> {
    data: &'a [u8],
    pos: usize,
    min_size: usize,
    mask: u64,
    max_size: usize,
}

impl<'a> ChunkIter<'a> {
    pub fn new(data: &'a [u8], min_size: usize, target_size: usize, max_size: usize) -> Self {
        Self {
            data,
            pos: 0,
            min_size,
            mask: make_mask(target_size),
            max_size,
        }
    }
}

impl Iterator for ChunkIter<'_> {
    type Item = Chunk;

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.data.len() {
            return None;
        }

        let start = self.pos;
        let remaining = self.data.len() - start;

        // If remaining data fits within max, take it all as the last chunk.
        if remaining <= self.max_size {
            let end = start + remaining;
            let chunk_data = self.data[start..end].to_vec();
            let hash = blake3_hash_128(&chunk_data);
            self.pos = end;
            return Some(Chunk {
                hash,
                data: chunk_data,
            });
        }

        // Roll through the data finding a split point.
        let mut fp: u64 = 0;
        let end = std::cmp::min(start + self.max_size, self.data.len());
        let min_end = start + self.min_size;

        let mut split = end; // default: hit max boundary
        for i in start..end {
            fp = (fp << 1).wrapping_add(GEAR[self.data[i] as usize]);
            if i >= min_end && (fp & self.mask) == 0 {
                split = i + 1;
                break;
            }
        }

        let chunk_data = self.data[start..split].to_vec();
        let hash = blake3_hash_128(&chunk_data);
        self.pos = split;
        Some(Chunk {
            hash,
            data: chunk_data,
        })
    }
}

/// Compute BLAKE3-128 fingerprint: full BLAKE3 hash truncated to 16 bytes.
/// Fast enough that parallelism is rarely needed at chunk level.
pub fn blake3_hash_128(data: &[u8]) -> ChunkHash {
    let full = blake3::hash(data);
    let bytes = full.as_bytes();
    let mut out = [0u8; 16];
    out.copy_from_slice(&bytes[..16]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_same_input_same_chunks() {
        let data: Vec<u8> = (0..200_000u32).map(|i| (i * 7 + 13) as u8).collect();
        let chunks1: Vec<_> = chunk_data(&data).collect();
        let chunks2: Vec<_> = chunk_data(&data).collect();

        assert_eq!(chunks1.len(), chunks2.len(), "chunk count must be stable");
        for (c1, c2) in chunks1.iter().zip(chunks2.iter()) {
            assert_eq!(c1.hash, c2.hash, "hashes must be stable");
            assert_eq!(c1.data.len(), c2.data.len(), "sizes must be stable");
        }
    }

    #[test]
    fn different_input_different_chunks() {
        let data1: Vec<u8> = vec![0xAA; 200_000];
        let data2: Vec<u8> = vec![0xBB; 200_000];
        let h1: Vec<_> = chunk_data(&data1).map(|c| c.hash).collect();
        let h2: Vec<_> = chunk_data(&data2).map(|c| c.hash).collect();
        assert_ne!(h1, h2);
    }

    #[test]
    fn chunk_sizes_within_bounds() {
        let data: Vec<u8> = (0..500_000u32).map(|i| (i * 3 + 7) as u8).collect();
        let chunks: Vec<_> = chunk_data(&data).collect();

        for (i, chunk) in chunks.iter().enumerate() {
            let is_last = i == chunks.len() - 1;
            if !is_last {
                assert!(
                    chunk.data.len() >= MIN_CHUNK_BYTES,
                    "chunk {} too small: {} < {}",
                    i,
                    chunk.data.len(),
                    MIN_CHUNK_BYTES
                );
            }
            assert!(
                chunk.data.len() <= MAX_CHUNK_BYTES,
                "chunk {} too large: {} > {}",
                i,
                chunk.data.len(),
                MAX_CHUNK_BYTES
            );
        }
    }

    #[test]
    fn chunks_reassemble_to_original() {
        let data: Vec<u8> = (0..300_000u32).map(|i| (i * 11 + 3) as u8).collect();
        let reassembled: Vec<u8> = chunk_data(&data).flat_map(|c| c.data).collect();
        assert_eq!(reassembled, data, "reassembly must reproduce original");
    }

    #[test]
    fn empty_input_yields_no_chunks() {
        let chunks: Vec<_> = chunk_data(&[]).collect();
        assert!(chunks.is_empty());
    }

    #[test]
    fn small_input_single_chunk() {
        let data = b"hello world";
        let chunks: Vec<_> = chunk_data(data).collect();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].data, data);
    }

    #[test]
    fn content_defined_boundary_shift() {
        // Prepending a byte shifts the start of all subsequent data but CDC
        // should still produce mostly the same chunk hashes after the first
        // changed chunk (boundary stability). We just verify no panic/hang.
        let data: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let mut shifted = vec![0xFFu8];
        shifted.extend_from_slice(&data);

        let orig_chunks: Vec<_> = chunk_data(&data).collect();
        let shift_chunks: Vec<_> = chunk_data(&shifted).collect();

        // Both must reassemble correctly.
        let orig_reassembled: Vec<u8> = orig_chunks
            .iter()
            .flat_map(|c| c.data.iter().copied())
            .collect();
        assert_eq!(orig_reassembled, data);

        let shift_reassembled: Vec<u8> = shift_chunks
            .iter()
            .flat_map(|c| c.data.iter().copied())
            .collect();
        assert_eq!(shift_reassembled, shifted);
    }
}
