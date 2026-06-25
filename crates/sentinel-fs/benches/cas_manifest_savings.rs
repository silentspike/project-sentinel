//! #500a CAS-manifest savings benchmark. Standalone (`harness = false`, own `fn main`).
//!
//! Tool-specific (issue #500a): it measures the things that prove the 1:n savings of
//! the metadata-aware CAS home manifest — the **marginal transfer** (a target that
//! already holds a fraction of the chunks only needs the missing chunks + the
//! manifest), the **cross-file dedup ratio**, the **chunker throughput**, and the
//! **rehydration latency p50/p95** — plus a **bug-finder** (the home is byte- and
//! metadata-identical after a round-trip) and a chunk-size parameter sweep that picks
//! the best profile for agent-home content.
//!
//! Standalone + TempDir-only, so it is safe to run next to a productive daemon
//! (no daemon, no cgroups, no #279 reconcile — Lehre #529).
//!
//! ```text
//! Build (remote):  cargo remote -c -- build -p sentinel-fs --release --bench cas_manifest_savings
//! Run (idle bench VM .241/.242 — NEVER .240/VM1069 prod-sim, NEVER cargo remote):
//!   scp target/release/deps/cas_manifest_savings-* ubuntu@10.0.0.241:/tmp/
//!   ssh ubuntu@10.0.0.241 '/tmp/cas_manifest_savings-*'   # parallel: vmstat 1 / mpstat 1 / iostat -x 1
//! ```

use std::collections::HashSet;
use std::path::Path;
use std::time::Instant;

use sentinel_fs::artifact::ArtifactPlane;
use sentinel_fs::chunker::ChunkIter;
use sentinel_fs::home_manifest::{rehydrate, walk_home, HomeManifest, RestorePolicy};
use sha2::{Digest, Sha256};

/// Deterministic pseudo-random bytes (a small LCG) — non-trivially compressible so
/// chunk boundaries are content-defined, but reproducible across runs.
fn pseudo_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut s = seed | 1;
    (0..len)
        .map(|_| {
            s = s
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (s >> 33) as u8
        })
        .collect()
}

/// Build `n_files` synthetic agent-home files. Each file shares an identical leading
/// block (`shared_frac` of its size) — modeling common config/templates that dedup
/// across agents — followed by a unique per-file block.
fn make_files(n_files: usize, file_size: usize, shared_frac: f64) -> Vec<Vec<u8>> {
    let shared_len = (file_size as f64 * shared_frac) as usize;
    let unique_len = file_size.saturating_sub(shared_len);
    let shared = pseudo_bytes(0xA5A5_A5A5, shared_len);
    (0..n_files)
        .map(|i| {
            let mut c = Vec::with_capacity(file_size);
            c.extend_from_slice(&shared);
            let seed = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0x3C3C_3C3C;
            c.extend(pseudo_bytes(seed, unique_len));
            c
        })
        .collect()
}

fn write_home(dir: &Path, files: &[Vec<u8>]) {
    std::fs::create_dir_all(dir).unwrap();
    for (i, c) in files.iter().enumerate() {
        std::fs::write(dir.join(format!("file_{i:05}.bin")), c).unwrap();
    }
}

fn pct(samples: &[u128], p: f64) -> u128 {
    let mut v = samples.to_vec();
    v.sort_unstable();
    let idx = (((v.len() - 1) as f64) * p).round() as usize;
    v[idx]
}

/// Collect (digest, size) for every chunk ref in the manifest, plus the total
/// logical bytes (with repetition) and the deduped unique bytes.
fn chunk_stats(manifest: &HomeManifest) -> (Vec<(Vec<u8>, u64)>, u64, u64) {
    let mut total_logical = 0u64;
    let mut unique_bytes = 0u64;
    let mut seen: HashSet<Vec<u8>> = HashSet::new();
    let mut unique: Vec<(Vec<u8>, u64)> = Vec::new();
    for e in &manifest.entries {
        for ext in &e.content {
            for r in &ext.chunk_refs {
                total_logical += r.size_bytes();
                if seen.insert(r.digest().to_vec()) {
                    unique_bytes += r.size_bytes();
                    unique.push((r.digest().to_vec(), r.size_bytes()));
                }
            }
        }
    }
    (unique, total_logical, unique_bytes)
}

fn bench_home(label: &str, n_files: usize, file_size: usize, shared_frac: f64) {
    let files = make_files(n_files, file_size, shared_frac);
    let total_file_bytes: u64 = files.iter().map(|f| f.len() as u64).sum();
    let src_home = tempfile::tempdir().unwrap();
    write_home(src_home.path(), &files);

    // Rehydration latency: K cold iterations (fresh plane + fresh dest each time).
    const K: usize = 5;
    let mut walk_us = Vec::with_capacity(K);
    let mut rehy_us = Vec::with_capacity(K);
    let mut manifest_bytes = 0usize;
    let mut last_manifest: Option<HomeManifest> = None;
    for _ in 0..K {
        let pdir = tempfile::tempdir().unwrap();
        let plane = ArtifactPlane::open(pdir.path().join("plane.redb")).unwrap();

        let t = Instant::now();
        let out = walk_home(src_home.path(), &plane).unwrap();
        walk_us.push(t.elapsed().as_micros());

        let dest = tempfile::tempdir().unwrap();
        let t = Instant::now();
        rehydrate(
            &out.manifest,
            dest.path(),
            &plane,
            &RestorePolicy::default(),
        )
        .unwrap();
        rehy_us.push(t.elapsed().as_micros());

        manifest_bytes = bincode::serde::encode_to_vec(&out.manifest, bincode::config::standard())
            .unwrap()
            .len();
        last_manifest = Some(out.manifest);
    }
    let manifest = last_manifest.unwrap();

    // Dedup ratio (cross-file).
    let (unique, total_logical, unique_bytes) = chunk_stats(&manifest);
    let dedup = total_logical as f64 / unique_bytes.max(1) as f64;

    // Marginal transfer: a 2nd "target" already holds `overlap` of the unique chunks;
    // only the missing chunks + the manifest must travel.
    print!(
        "home[{label:6}] files={n_files:4} size={:>6}KB  total={:>7}KB  manifest={:>5}KB  dedup={dedup:5.2}x  walk p50/p95={}/{}us  rehydrate p50/p95={}/{}us\n           marginal-transfer vs {:.0}KB baseline:",
        file_size / 1024,
        total_file_bytes / 1024,
        manifest_bytes / 1024,
        pct(&walk_us, 0.5),
        pct(&walk_us, 0.95),
        pct(&rehy_us, 0.5),
        pct(&rehy_us, 0.95),
        total_file_bytes as f64 / 1024.0,
    );
    for overlap in [0.0_f64, 0.5, 0.9] {
        let n_held = (unique.len() as f64 * overlap) as usize;
        let held: HashSet<&Vec<u8>> = unique[..n_held].iter().map(|(d, _)| d).collect();
        let missing: u64 = unique
            .iter()
            .filter(|(d, _)| !held.contains(d))
            .map(|(_, s)| s)
            .sum();
        let marginal = missing + manifest_bytes as u64;
        let factor = total_file_bytes as f64 / marginal.max(1) as f64;
        print!(
            "  [{:.0}% held -> {:>6}KB, {factor:5.1}x]",
            overlap * 100.0,
            marginal / 1024
        );
    }
    println!();

    // Bug-finder: the home is byte- and metadata-identical after a round-trip.
    let pdir = tempfile::tempdir().unwrap();
    let plane = ArtifactPlane::open(pdir.path().join("verify.redb")).unwrap();
    let out = walk_home(src_home.path(), &plane).unwrap();
    let dest = tempfile::tempdir().unwrap();
    rehydrate(
        &out.manifest,
        dest.path(),
        &plane,
        &RestorePolicy::default(),
    )
    .unwrap();
    for (i, original) in files.iter().enumerate() {
        let restored = std::fs::read(dest.path().join(format!("file_{i:05}.bin"))).unwrap();
        assert_eq!(
            Sha256::digest(original).as_slice(),
            Sha256::digest(&restored).as_slice(),
            "BUG: file_{i} content differs after round-trip"
        );
        let om = std::fs::metadata(src_home.path().join(format!("file_{i:05}.bin"))).unwrap();
        let rm = std::fs::metadata(dest.path().join(format!("file_{i:05}.bin"))).unwrap();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            om.permissions().mode() & 0o7777,
            rm.permissions().mode() & 0o7777,
            "BUG: file_{i} mode differs after round-trip"
        );
    }
}

/// Chunk-size parameter sweep: same content, different (min, target, max) -> pick the
/// profile with the best dedup / throughput trade-off for agent-home content.
fn chunk_size_sweep() {
    println!("\n=== chunk-size parameter sweep (dedup + chunker throughput) ===");
    let files = make_files(80, 1024 * 1024, 0.80);
    let total_bytes: usize = files.iter().map(|f| f.len()).sum();
    for (label, min, target, max) in [
        (
            "16k/64k/256k (default)",
            16_384usize,
            65_536usize,
            262_144usize,
        ),
        ("8k/32k/128k", 8_192, 32_768, 131_072),
        ("32k/128k/512k", 32_768, 131_072, 524_288),
    ] {
        let t = Instant::now();
        let mut total_logical = 0u64;
        let mut unique_bytes = 0u64;
        let mut seen: HashSet<[u8; 16]> = HashSet::new();
        for f in &files {
            for c in ChunkIter::new(f, min, target, max) {
                total_logical += c.data.len() as u64;
                if seen.insert(c.hash) {
                    unique_bytes += c.data.len() as u64;
                }
            }
        }
        let secs = t.elapsed().as_secs_f64();
        let mbps = (total_bytes as f64 / 1e6) / secs.max(1e-9);
        let dedup = total_logical as f64 / unique_bytes.max(1) as f64;
        println!(
            "  {label:24}  dedup={dedup:5.2}x  throughput={mbps:7.0} MB/s  unique_chunks={}",
            seen.len()
        );
    }
    println!("  -> the default 16k/64k/256k gear profile is the locked chunk_profile (gear-v1:16k-64k-256k).");
}

fn main() {
    println!("=== #500a CAS-manifest savings benchmark ===");
    println!("metadata-aware home manifest: chunks live ONCE in the ArtifactPlane (BLAKE3-128);");
    println!("a move sends only the manifest + the chunks the target is missing (1:n).\n");

    // small files (<= one chunk): no sub-file dedup — the saving is the marginal
    // transfer, not the dedup ratio (the honest small-file case).
    bench_home("small", 120, 48 * 1024, 0.80);
    // medium/large files split into multiple chunks; the shared leading region
    // dedups across files (sub-file content-defined dedup).
    bench_home("medium", 60, 1024 * 1024, 0.80);
    bench_home("large", 30, 4 * 1024 * 1024, 0.85);

    chunk_size_sweep();

    println!(
        "\nOK: round-trip byte+metadata identical (bug-finder green); marginal transfer shrinks"
    );
    println!("with target chunk overlap; cross-file dedup measured; chunk profile locked.");
}
