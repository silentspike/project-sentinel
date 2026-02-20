//! Integration tests for sentinel-fs: CAS + Metadata + Layer Manager.
//!
//! These tests verify cross-component behavior that unit tests can't cover.

use sentinel_fs::cas::CasStore;
use sentinel_fs::layer::LayerManager;
use sentinel_fs::metadata::{FileKind, MetadataStore};

fn setup() -> (LayerManager, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let cas = CasStore::open(dir.path()).unwrap();
    let meta = MetadataStore::open(dir.path().join("meta.redb")).unwrap();
    let lm = LayerManager::new(cas, meta);
    lm.init_base_root().unwrap();
    (lm, dir)
}

// === AC-1: Deduplication Rate ===

#[test]
fn ac1_dedup_15_identical_trees() {
    let (lm, _dir) = setup();

    // Simulate 15 agents each with identical file trees
    let files = [
        (
            "package.json",
            br#"{"name":"app","version":"1.0.0"}"#.as_slice(),
        ),
        ("index.js", b"console.log('hello world');"),
        ("README.md", b"# My App\n\nA sample application."),
        ("config.toml", b"[server]\nport = 8080\nhost = '0.0.0.0'"),
        ("Makefile", b"all:\n\techo build\n\ntest:\n\techo test"),
    ];

    // Populate base layer with these files
    for (name, content) in &files {
        lm.populate_base_file(1, name, content, 0o644).unwrap();
    }

    // Each agent writes the same files (simulating npm install / copy)
    for agent_num in 1..=15 {
        let agent_id = format!("AGENT-{agent_num:02}");
        for (name, content) in &files {
            lm.write_file(&agent_id, 1, name, content, 0o644).unwrap();
        }
    }

    let stats = lm.cas().stats().unwrap();

    // 5 unique files × (base + 15 agents) = 80 writes, but only 5 unique blobs
    assert_eq!(
        stats.blob_count, 5,
        "Should have exactly 5 unique blobs, got {}",
        stats.blob_count
    );

    // Total raw size = 5 files × 16 agents = 80 copies
    let total_raw: u64 = files.iter().map(|(_, c)| c.len() as u64).sum::<u64>() * 16;
    let dedup_ratio = 1.0 - (stats.total_bytes_on_disk as f64 / total_raw as f64);
    assert!(
        dedup_ratio > 0.87,
        "Dedup ratio should be >87%, got {:.1}%",
        dedup_ratio * 100.0
    );
}

// === AC-3: Agent Isolation ===

#[test]
fn ac3_agent_01_writes_agent_02_cannot_see() {
    let (lm, _dir) = setup();

    // Agent-01 writes a private file
    let inode = lm
        .write_file("AGENT-01", 1, "secret.txt", b"top secret data", 0o600)
        .unwrap();

    // Agent-01 can read it
    let content = lm.read_file("AGENT-01", inode).unwrap();
    assert_eq!(content, b"top secret data");

    // Agent-02 cannot see the dirent
    assert!(
        lm.lookup_dirent("AGENT-02", 1, "secret.txt")
            .unwrap()
            .is_none(),
        "AGENT-02 should not see AGENT-01's file"
    );

    // Agent-02 cannot read the inode (doesn't exist in their layer)
    assert!(
        lm.lookup_inode("AGENT-02", inode).unwrap().is_none(),
        "AGENT-02 should not see AGENT-01's inode"
    );
}

#[test]
fn ac3_agent_isolation_readdir() {
    let (lm, _dir) = setup();

    lm.write_file("AGENT-01", 1, "a01.txt", b"a", 0o644)
        .unwrap();
    lm.write_file("AGENT-02", 1, "a02.txt", b"b", 0o644)
        .unwrap();

    let a01_entries = lm.readdir("AGENT-01", 1).unwrap();
    let a01_names: Vec<&str> = a01_entries.iter().map(|(n, _, _)| n.as_str()).collect();
    assert!(a01_names.contains(&"a01.txt"));
    assert!(
        !a01_names.contains(&"a02.txt"),
        "AGENT-01 must not see AGENT-02's file"
    );

    let a02_entries = lm.readdir("AGENT-02", 1).unwrap();
    let a02_names: Vec<&str> = a02_entries.iter().map(|(n, _, _)| n.as_str()).collect();
    assert!(a02_names.contains(&"a02.txt"));
    assert!(
        !a02_names.contains(&"a01.txt"),
        "AGENT-02 must not see AGENT-01's file"
    );
}

// === AC-4: Crash Recovery (redb ACID) ===

#[test]
fn ac4_crash_recovery_redb_acid() {
    let dir = tempfile::tempdir().unwrap();

    // Phase 1: Write data
    {
        let cas = CasStore::open(dir.path()).unwrap();
        let meta = MetadataStore::open(dir.path().join("meta.redb")).unwrap();
        let lm = LayerManager::new(cas, meta);
        lm.init_base_root().unwrap();

        lm.write_file("AGENT-01", 1, "persist.txt", b"persistent data", 0o644)
            .unwrap();
        lm.populate_base_file(1, "base.txt", b"base data", 0o644)
            .unwrap();
    }
    // LayerManager + MetadataStore dropped — simulates crash/restart

    // Phase 2: Reopen and verify data survived
    {
        let cas = CasStore::open(dir.path()).unwrap();
        let meta = MetadataStore::open(dir.path().join("meta.redb")).unwrap();
        let lm = LayerManager::new(cas, meta);

        let dirent = lm.lookup_dirent("AGENT-01", 1, "persist.txt").unwrap();
        assert!(dirent.is_some(), "Agent file should survive restart");

        let inode = dirent.unwrap();
        let content = lm.read_file("AGENT-01", inode).unwrap();
        assert_eq!(content, b"persistent data");

        let base_dirent = lm.lookup_dirent("AGENT-01", 1, "base.txt").unwrap();
        assert!(base_dirent.is_some(), "Base file should survive restart");
    }
}

// === Copy-on-Write semantics ===

#[test]
fn cow_agent_override_does_not_modify_base() {
    let (lm, _dir) = setup();

    let base_inode = lm
        .populate_base_file(1, "config.txt", b"default config", 0o644)
        .unwrap();

    // Agent overrides the file
    let _agent_inode = lm
        .write_file("AGENT-01", 1, "config.txt", b"custom config", 0o644)
        .unwrap();

    // Agent sees their version
    let agent_dirent = lm
        .lookup_dirent("AGENT-01", 1, "config.txt")
        .unwrap()
        .unwrap();
    let agent_content = lm.read_file("AGENT-01", agent_dirent).unwrap();
    assert_eq!(agent_content, b"custom config");

    // Base layer still has original
    let base_content = lm.read_file("__BASE__", base_inode).unwrap();
    assert_eq!(base_content, b"default config");

    // Other agent sees base version
    let other_dirent = lm
        .lookup_dirent("AGENT-02", 1, "config.txt")
        .unwrap()
        .unwrap();
    let other_content = lm.read_file("AGENT-02", other_dirent).unwrap();
    assert_eq!(other_content, b"default config");
}

// === Whiteout + Readdir merge ===

#[test]
fn whiteout_and_readdir_complex() {
    let (lm, _dir) = setup();

    // Base: 4 files
    lm.populate_base_file(1, "keep1.txt", b"k1", 0o644).unwrap();
    lm.populate_base_file(1, "keep2.txt", b"k2", 0o644).unwrap();
    let del1_inode = lm
        .populate_base_file(1, "delete1.txt", b"d1", 0o644)
        .unwrap();
    let del2_inode = lm
        .populate_base_file(1, "delete2.txt", b"d2", 0o644)
        .unwrap();

    // Agent: delete 2, add 1
    lm.unlink("AGENT-01", 1, "delete1.txt", del1_inode).unwrap();
    lm.unlink("AGENT-01", 1, "delete2.txt", del2_inode).unwrap();
    lm.write_file("AGENT-01", 1, "new.txt", b"new", 0o644)
        .unwrap();

    let entries = lm.readdir("AGENT-01", 1).unwrap();
    let names: Vec<&str> = entries.iter().map(|(n, _, _)| n.as_str()).collect();

    assert_eq!(names.len(), 3, "should have keep1, keep2, new");
    assert!(names.contains(&"keep1.txt"));
    assert!(names.contains(&"keep2.txt"));
    assert!(names.contains(&"new.txt"));
    assert!(!names.contains(&"delete1.txt"));
    assert!(!names.contains(&"delete2.txt"));
}

// === Nested directories ===

#[test]
fn nested_directory_structure() {
    let (lm, _dir) = setup();

    // Create: /root/src/main.rs
    let src_inode = lm.mkdir("AGENT-01", 1, "src", 0o755).unwrap();
    let main_inode = lm
        .write_file("AGENT-01", src_inode, "main.rs", b"fn main() {}", 0o644)
        .unwrap();

    // Verify lookup chain
    let found_src = lm.lookup_dirent("AGENT-01", 1, "src").unwrap().unwrap();
    assert_eq!(found_src, src_inode);

    let found_main = lm
        .lookup_dirent("AGENT-01", src_inode, "main.rs")
        .unwrap()
        .unwrap();
    assert_eq!(found_main, main_inode);

    let content = lm.read_file("AGENT-01", main_inode).unwrap();
    assert_eq!(content, b"fn main() {}");

    // Readdir of src/
    let entries = lm.readdir("AGENT-01", src_inode).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0, "main.rs");
    assert_eq!(entries[0].2, FileKind::Regular);
}

// === Refcount integrity ===

#[test]
fn refcount_integrity_across_operations() {
    let (lm, _dir) = setup();
    let content = b"shared content across operations";
    let hash = CasStore::hash(content);

    // Multiple agents reference same content
    lm.write_file("AGENT-01", 1, "f.txt", content, 0o644)
        .unwrap();
    lm.write_file("AGENT-02", 1, "f.txt", content, 0o644)
        .unwrap();
    lm.write_file("AGENT-03", 1, "f.txt", content, 0o644)
        .unwrap();
    assert_eq!(lm.meta().get_refcount(&hash).unwrap(), 3);

    // Remove one agent's file
    let a1_inode = lm.lookup_dirent("AGENT-01", 1, "f.txt").unwrap().unwrap();
    lm.meta()
        .remove_file("AGENT-01", 1, "f.txt", a1_inode)
        .unwrap();
    assert_eq!(lm.meta().get_refcount(&hash).unwrap(), 2);

    // Content still accessible by other agents
    let a2_inode = lm.lookup_dirent("AGENT-02", 1, "f.txt").unwrap().unwrap();
    let data = lm.read_file("AGENT-02", a2_inode).unwrap();
    assert_eq!(data, content);
}

// === Scale test: many agents ===

#[test]
fn scale_100_agents_with_shared_content() {
    let (lm, _dir) = setup();
    let content = b"identical package-lock content for all 100 agents";

    for i in 1..=100 {
        let agent = format!("AGENT-{i:04}");
        lm.write_file(&agent, 1, "lock.json", content, 0o644)
            .unwrap();
    }

    // Only 1 blob despite 100 writes
    let stats = lm.cas().stats().unwrap();
    assert_eq!(stats.blob_count, 1);

    // Refcount = 100
    let hash = CasStore::hash(content);
    assert_eq!(lm.meta().get_refcount(&hash).unwrap(), 100);
}

// ============================================================================
// Artifact Plane Integration Tests (AC-4: Dedup, AC-5: Multi-Format)
// ============================================================================

use sentinel_fs::artifact::ArtifactPlane;
use sentinel_fs::gc::{gc_chunks, release_object};
use sentinel_fs::ingest::{begin_ingest, commit_ingest};
use sentinel_fs::read_planner::read_object;

/// Generate pseudo-random data with high entropy using xorshift64.
/// High entropy ensures CDC gear-hash hits boundaries near the 64KB target.
fn xorshift_data(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let bytes = state.to_le_bytes();
        let remaining = len - out.len();
        let take = remaining.min(8);
        out.extend_from_slice(&bytes[..take]);
    }
    out
}

fn artifact_plane(name: &str) -> (ArtifactPlane, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let plane =
        ArtifactPlane::open(dir.path().join(format!("{name}.redb"))).unwrap();
    (plane, dir)
}

/// Identical data ingested twice: zero net-new chunks on second ingest.
#[test]
fn ac4_dedup_identical_zero_net_new_chunks() {
    let (plane, _dir) = artifact_plane("dedup_identical");
    let data: Vec<u8> = (0..1_048_576u32)
        .map(|i| (i.wrapping_mul(2654435761) >> 24) as u8)
        .collect();

    let mut s1 = begin_ingest(&plane, "application/octet-stream");
    s1.write(&data);
    let id1 = commit_ingest(s1).unwrap();
    let chunks_after_first = plane.chunk_count().unwrap();
    let manifest1 = plane.get_manifest(id1).unwrap().unwrap();

    let mut s2 = begin_ingest(&plane, "application/octet-stream");
    s2.write(&data);
    let id2 = commit_ingest(s2).unwrap();
    let chunks_after_second = plane.chunk_count().unwrap();
    let manifest2 = plane.get_manifest(id2).unwrap().unwrap();

    assert_eq!(chunks_after_first, chunks_after_second,
        "ZERO net-new chunks (before={chunks_after_first}, after={chunks_after_second})");
    assert_eq!(manifest1, manifest2, "identical manifests");
    for hash in &manifest1 {
        assert_eq!(plane.get_chunk_refcount(hash).unwrap(), 2);
    }
    assert_eq!(read_object(&plane, id1).unwrap(), data);
    assert_eq!(read_object(&plane, id2).unwrap(), data);
}

/// 10 identical ingests: zero net-new chunks after first.
#[test]
fn ac4_dedup_10x_identical_no_growth() {
    let (plane, _dir) = artifact_plane("dedup_10x");
    let data: Vec<u8> = (0..512_000u32)
        .map(|i| (i.wrapping_mul(1664525).wrapping_add(1013904223) >> 16) as u8)
        .collect();

    let mut s = begin_ingest(&plane, "application/octet-stream");
    s.write(&data);
    commit_ingest(s).unwrap();
    let baseline = plane.chunk_count().unwrap();

    for _ in 1..10 {
        let mut s = begin_ingest(&plane, "application/octet-stream");
        s.write(&data);
        commit_ingest(s).unwrap();
    }
    assert_eq!(baseline, plane.chunk_count().unwrap(),
        "10 identical ingests: no chunk growth");
}

/// Similar data (1 byte changed): most chunks deduped.
#[test]
fn ac4_dedup_similar_data_high_ratio() {
    let (plane, _dir) = artifact_plane("dedup_similar");
    let base: Vec<u8> = xorshift_data(524_288, 0xDEAD_BEEF);

    let mut s1 = begin_ingest(&plane, "application/octet-stream");
    s1.write(&base);
    let id1 = commit_ingest(s1).unwrap();
    let chunks_after_base = plane.chunk_count().unwrap();
    let manifest1 = plane.get_manifest(id1).unwrap().unwrap();

    let mut variant = base.clone();
    *variant.last_mut().unwrap() ^= 0xFF;
    let mut s2 = begin_ingest(&plane, "application/octet-stream");
    s2.write(&variant);
    let id2 = commit_ingest(s2).unwrap();
    let manifest2 = plane.get_manifest(id2).unwrap().unwrap();

    let net_new = plane.chunk_count().unwrap() - chunks_after_base;
    assert!(net_new <= 2, "1-byte change: at most 2 net-new, got {net_new}");

    let shared = manifest2.iter().filter(|h| manifest1.contains(h)).count();
    let ratio = shared as f64 / manifest2.len() as f64;
    assert!(ratio >= 0.75, "dedup ratio >= 75%, got {:.1}%", ratio * 100.0);
    assert_eq!(read_object(&plane, id1).unwrap(), base);
    assert_eq!(read_object(&plane, id2).unwrap(), variant);
}

/// Prepending data: CDC boundary stability.
#[test]
fn ac4_dedup_prepend_boundary_stability() {
    let (plane, _dir) = artifact_plane("dedup_prepend");
    let base: Vec<u8> = xorshift_data(1_048_576, 0xCAFE_BABE);

    let mut s1 = begin_ingest(&plane, "application/octet-stream");
    s1.write(&base);
    commit_ingest(s1).unwrap();
    let chunks_base = plane.chunk_count().unwrap();

    let mut with_header = vec![0xAA; 1024];
    with_header.extend_from_slice(&base);
    let mut s2 = begin_ingest(&plane, "application/octet-stream");
    s2.write(&with_header);
    let id2 = commit_ingest(s2).unwrap();

    let net_new = plane.chunk_count().unwrap() - chunks_base;
    let m2 = plane.get_manifest(id2).unwrap().unwrap();
    let ratio = net_new as f64 / m2.len() as f64;
    assert!(ratio < 0.5, "prepend 1KB to 1MB: net-new <50%, got {:.1}%", ratio * 100.0);
    assert_eq!(read_object(&plane, id2).unwrap(), with_header);
}

/// Dedup + GC lifecycle: ingest, release, GC.
#[test]
fn ac4_dedup_gc_full_lifecycle() {
    let (plane, _dir) = artifact_plane("dedup_gc");
    let data: Vec<u8> = (0..256_000u32)
        .map(|i| (i.wrapping_mul(31337) >> 16) as u8)
        .collect();

    let mut s1 = begin_ingest(&plane, "application/octet-stream");
    s1.write(&data);
    let id1 = commit_ingest(s1).unwrap();
    let mut s2 = begin_ingest(&plane, "application/octet-stream");
    s2.write(&data);
    let id2 = commit_ingest(s2).unwrap();
    let baseline = plane.chunk_count().unwrap();

    release_object(&plane, id1).unwrap();
    assert_eq!(gc_chunks(&plane).unwrap().removed, 0, "shared: no GC");
    assert_eq!(plane.chunk_count().unwrap(), baseline);

    release_object(&plane, id2).unwrap();
    let gc2 = gc_chunks(&plane).unwrap();
    assert_eq!(gc2.removed, baseline, "orphans: all GC'd");
    assert_eq!(plane.chunk_count().unwrap(), 0);
    assert!(gc2.freed_bytes > 0);
}

/// Compression: compressible data smaller on disk.
#[test]
fn ac4_compression_ratio() {
    let (plane, _dir) = artifact_plane("compression");
    let data: Vec<u8> = "Hello World! Repeating text pattern. "
        .repeat(50_000)
        .into_bytes();
    let raw_size = data.len();

    let mut s = begin_ingest(&plane, "text/plain");
    s.write(&data);
    let id = commit_ingest(s).unwrap();

    let manifest = plane.get_manifest(id).unwrap().unwrap();
    let compressed: usize = manifest
        .iter()
        .map(|h| plane.read_chunk_raw(h).unwrap().len())
        .sum();
    let ratio = 1.0 - (compressed as f64 / raw_size as f64);
    assert!(ratio > 0.5, ">50% compression, got {:.1}%", ratio * 100.0);
    assert_eq!(read_object(&plane, id).unwrap(), data);
}

/// Chunk size distribution near 64KB target.
#[test]
fn ac4_chunk_size_distribution() {
    let (plane, _dir) = artifact_plane("chunk_dist");
    let data: Vec<u8> = xorshift_data(4_194_304, 0x1234_5678);

    let mut s = begin_ingest(&plane, "application/octet-stream");
    s.write(&data);
    let id = commit_ingest(s).unwrap();
    let manifest = plane.get_manifest(id).unwrap().unwrap();

    assert!(manifest.len() >= 10 && manifest.len() <= 256,
        "4MB/64KB: 10-256 chunks, got {}", manifest.len());

    let sizes: Vec<usize> = manifest.iter().map(|h| {
        sentinel_fs::ingest::decompress_chunk(
            &plane.read_chunk_raw(h).unwrap(),
        ).unwrap().len()
    }).collect();
    let avg = sizes.iter().sum::<usize>() as f64 / sizes.len() as f64;
    let max_s = *sizes.iter().max().unwrap();

    assert!(avg > 32_000.0 && avg < 128_000.0, "avg ~64KB, got {avg:.0}");
    assert!(max_s <= 262_144, "max <= 256KB, got {max_s}");
    assert_eq!(sizes.iter().sum::<usize>(), data.len());
}

/// 10MB dedup scaling.
#[test]
fn ac4_scaling_10mb_dedup() {
    let (plane, _dir) = artifact_plane("scaling_10mb");
    let data: Vec<u8> = (0..10_485_760u32)
        .map(|i| (i.wrapping_mul(1664525).wrapping_add(1013904223) >> 24) as u8)
        .collect();

    let mut s1 = begin_ingest(&plane, "application/octet-stream");
    s1.write(&data);
    let id1 = commit_ingest(s1).unwrap();
    let baseline = plane.chunk_count().unwrap();

    let mut s2 = begin_ingest(&plane, "application/octet-stream");
    s2.write(&data);
    let id2 = commit_ingest(s2).unwrap();
    assert_eq!(plane.chunk_count().unwrap(), baseline, "10MB: zero net-new");
    assert_eq!(read_object(&plane, id1).unwrap(), data);
    assert_eq!(read_object(&plane, id2).unwrap(), data);
}

/// Multi-format round-trip (AC-5).
#[test]
fn ac5_multi_format_ingest_roundtrip() {
    let (plane, _dir) = artifact_plane("multi_format");

    let iso: Vec<u8> = (0..500_000u32)
        .map(|i| ((i.wrapping_mul(7919).wrapping_add(12347)) % 256) as u8)
        .collect();
    let html = b"<!DOCTYPE html><html><body><p>X</p></body></html>".repeat(5000);
    let mut pdf = b"%PDF-1.4\n".to_vec();
    pdf.extend((0..300_000u32).map(|i| (i % 256) as u8));
    let bin = vec![0u8; 200_000];

    for (mime, data) in &[
        ("application/x-iso9660-image", iso.as_slice()),
        ("text/html", html.as_slice()),
        ("application/pdf", pdf.as_slice()),
        ("application/octet-stream", bin.as_slice()),
    ] {
        let mut s = begin_ingest(&plane, *mime);
        s.write(data);
        let id = commit_ingest(s).unwrap();
        let rb = read_object(&plane, id).unwrap();
        assert_eq!(rb.len(), data.len(), "{mime}: size");
        assert_eq!(&rb, data, "{mime}: content");
        assert_eq!(plane.get_object(id).unwrap().unwrap().size, data.len() as u64);
    }
}

/// Named references lifecycle.
#[test]
fn artifact_named_refs_lifecycle() {
    let (plane, _dir) = artifact_plane("named_refs");

    let mut s1 = begin_ingest(&plane, "text/plain");
    s1.write(b"version 1");
    let id1 = commit_ingest(s1).unwrap();
    let mut s2 = begin_ingest(&plane, "text/plain");
    s2.write(b"version 2");
    let id2 = commit_ingest(s2).unwrap();

    plane.set_ref("latest", id1).unwrap();
    assert_eq!(plane.resolve_ref("latest").unwrap(), Some(id1));
    plane.set_ref("latest", id2).unwrap();
    assert_eq!(plane.resolve_ref("latest").unwrap(), Some(id2));
    assert_eq!(
        read_object(&plane, plane.resolve_ref("latest").unwrap().unwrap()).unwrap(),
        b"version 2"
    );
}
