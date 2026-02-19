//! CLI interface for sentinel-fs operations.
//!
//! Commands: stats, gc, populate. FUSE mount/unmount requires the `fuse-tests` feature.

use crate::cas::CasStore;
use crate::layer::LayerManager;
use crate::metadata::MetadataStore;
use std::path::Path;

/// Print CAS store statistics.
pub fn cmd_stats(data_dir: &Path) -> anyhow::Result<()> {
    let cas = CasStore::open(data_dir)?;
    let stats = cas.stats()?;
    println!("CAS Store: {}", data_dir.display());
    println!("  Blobs:       {}", stats.blob_count);
    println!(
        "  Disk usage:  {} bytes ({:.2} MB)",
        stats.total_bytes_on_disk,
        stats.total_bytes_on_disk as f64 / 1_048_576.0
    );
    Ok(())
}

/// Run garbage collection on unreferenced blobs.
pub fn cmd_gc(data_dir: &Path) -> anyhow::Result<()> {
    let cas = CasStore::open(data_dir)?;
    let meta_path = data_dir.join("metadata.redb");
    let meta = MetadataStore::open(&meta_path)?;

    // Find all blobs on disk
    let mut all_hashes: Vec<[u8; 32]> = Vec::new();
    let cas_dir = cas.cas_dir();
    if cas_dir.exists() {
        for prefix_entry in std::fs::read_dir(cas_dir)? {
            let prefix_entry = prefix_entry?;
            if prefix_entry.file_type()?.is_dir() {
                for blob_entry in std::fs::read_dir(prefix_entry.path())? {
                    let blob_entry = blob_entry?;
                    if blob_entry.file_type()?.is_file() {
                        let prefix_name = prefix_entry.file_name();
                        let blob_name = blob_entry.file_name();
                        let hex = format!(
                            "{}{}",
                            prefix_name.to_string_lossy(),
                            blob_name.to_string_lossy()
                        );
                        if hex.len() == 64 {
                            if let Ok(hash) = hex_to_hash(&hex) {
                                all_hashes.push(hash);
                            }
                        }
                    }
                }
            }
        }
    }

    // Filter to zero-refcount hashes
    let zero_refs: Vec<[u8; 32]> = all_hashes
        .into_iter()
        .filter(|h| meta.get_refcount(h).unwrap_or(0) == 0)
        .collect();

    if zero_refs.is_empty() {
        println!("No unreferenced blobs found.");
        return Ok(());
    }

    let gc_stats = cas.gc(&zero_refs)?;
    println!(
        "GC complete: removed {} blobs, freed {} bytes ({:.2} MB)",
        gc_stats.removed,
        gc_stats.freed_bytes,
        gc_stats.freed_bytes as f64 / 1_048_576.0
    );
    Ok(())
}

/// Populate the base layer from a source directory.
pub fn cmd_populate(data_dir: &Path, source_dir: &Path) -> anyhow::Result<()> {
    let cas = CasStore::open(data_dir)?;
    let meta_path = data_dir.join("metadata.redb");
    let meta = MetadataStore::open(&meta_path)?;
    let layer = LayerManager::new(cas, meta);
    layer.init_base_root()?;

    let mut file_count = 0u64;
    let mut dir_count = 0u64;
    let mut total_bytes = 0u64;

    populate_recursive(
        &layer,
        source_dir,
        1,
        &mut file_count,
        &mut dir_count,
        &mut total_bytes,
    )?;

    println!("Populated base layer from: {}", source_dir.display());
    println!("  Directories: {dir_count}");
    println!("  Files:       {file_count}");
    println!(
        "  Total bytes: {total_bytes} ({:.2} MB)",
        total_bytes as f64 / 1_048_576.0
    );

    // Show dedup stats
    let stats = layer.cas().stats()?;
    println!(
        "  CAS blobs:   {} ({:.2} MB on disk)",
        stats.blob_count,
        stats.total_bytes_on_disk as f64 / 1_048_576.0
    );
    if total_bytes > 0 {
        let ratio = 1.0 - (stats.total_bytes_on_disk as f64 / total_bytes as f64);
        println!("  Dedup ratio: {:.1}%", ratio * 100.0);
    }

    Ok(())
}

fn populate_recursive(
    layer: &LayerManager,
    dir: &Path,
    parent_inode: u64,
    file_count: &mut u64,
    dir_count: &mut u64,
    total_bytes: &mut u64,
) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if ft.is_file() {
            let content = std::fs::read(entry.path())?;
            *total_bytes += content.len() as u64;
            layer.populate_base_file(parent_inode, &name_str, &content, 0o644)?;
            *file_count += 1;
        } else if ft.is_dir() {
            let sub_inode = layer.populate_base_dir(parent_inode, &name_str, 0o755)?;
            *dir_count += 1;
            populate_recursive(
                layer,
                &entry.path(),
                sub_inode,
                file_count,
                dir_count,
                total_bytes,
            )?;
        }
        // Skip symlinks and other special files for now
    }
    Ok(())
}

/// Parse a 64-char hex string into a 32-byte hash.
fn hex_to_hash(hex: &str) -> anyhow::Result<[u8; 32]> {
    if hex.len() != 64 {
        return Err(anyhow::anyhow!("Expected 64 hex chars, got {}", hex.len()));
    }
    let mut hash = [0u8; 32];
    for i in 0..32 {
        hash[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
            .map_err(|e| anyhow::anyhow!("Invalid hex at pos {}: {e}", i * 2))?;
    }
    Ok(hash)
}

/// Start the FUSE daemon (requires `fuse-tests` feature).
#[cfg(feature = "fuse-tests")]
pub fn cmd_start(data_dir: &Path, mountpoint: &Path) -> anyhow::Result<()> {
    println!("Starting sentinel-fs FUSE daemon...");
    println!("  Data dir:   {}", data_dir.display());
    println!("  Mountpoint: {}", mountpoint.display());
    crate::fuse::start_fuse(data_dir, mountpoint)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_to_hash_roundtrip() {
        let original = [
            0xAB, 0xCD, 0xEF, 0x01, 0x23, 0x45, 0x67, 0x89, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55,
            0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x01, 0x02, 0x03, 0x04,
            0x05, 0x06, 0x07, 0x08,
        ];
        let hex = crate::cas::hex_encode(&original);
        let parsed = hex_to_hash(&hex).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn hex_to_hash_invalid_length() {
        assert!(hex_to_hash("abcd").is_err());
    }

    #[test]
    fn populate_and_stats() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(&data_dir).unwrap();

        // Create a source directory
        let source = dir.path().join("source");
        std::fs::create_dir_all(source.join("sub")).unwrap();
        std::fs::write(source.join("a.txt"), b"hello").unwrap();
        std::fs::write(source.join("b.txt"), b"hello").unwrap(); // dedup candidate
        std::fs::write(source.join("sub").join("c.txt"), b"world").unwrap();

        cmd_populate(&data_dir, &source).unwrap();

        let cas = CasStore::open(&data_dir).unwrap();
        let stats = cas.stats().unwrap();
        // a.txt and b.txt have same content -> deduped to 1 blob
        // c.txt is different -> 1 blob
        assert_eq!(stats.blob_count, 2, "should have 2 unique blobs (dedup)");
    }

    #[test]
    fn gc_removes_unreferenced() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(&data_dir).unwrap();

        let cas = CasStore::open(&data_dir).unwrap();
        let meta = MetadataStore::open(data_dir.join("metadata.redb")).unwrap();

        // Store a blob without any refcount entry
        let (hash, _) = cas.store(b"orphan blob").unwrap();
        assert!(cas.contains(&hash));
        assert_eq!(meta.get_refcount(&hash).unwrap(), 0);

        // Store a blob with refcount
        let (hash2, _) = cas.store(b"referenced blob").unwrap();
        meta.inc_refcount(&hash2).unwrap();

        drop(cas);
        drop(meta);

        cmd_gc(&data_dir).unwrap();

        let cas2 = CasStore::open(&data_dir).unwrap();
        assert!(!cas2.contains(&hash), "orphan should be removed");
        assert!(cas2.contains(&hash2), "referenced should remain");
    }
}
