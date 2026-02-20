//! Benchmarks for sentinel-fs: CAS, metadata, layer operations, and Artifact Plane.

use criterion::{criterion_group, criterion_main, Criterion};
use sentinel_fs::artifact::ArtifactPlane;
use sentinel_fs::cas::CasStore;
use sentinel_fs::chunker::chunk_data;
use sentinel_fs::gc::{gc_chunks, release_object};
use sentinel_fs::ingest::{begin_ingest, commit_ingest};
use sentinel_fs::layer::LayerManager;
use sentinel_fs::metadata::MetadataStore;
use sentinel_fs::read_planner::read_object;

// === Existing CAS / Layer Benchmarks ===

fn cas_hash_throughput(c: &mut Criterion) {
    let data = vec![0u8; 4096];
    c.bench_function("cas_hash_4k", |b| {
        b.iter(|| CasStore::hash(std::hint::black_box(&data)))
    });
}

fn cas_store_dedup(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let store = CasStore::open(dir.path()).unwrap();
    let data = vec![0xAA; 4096];

    // Pre-store so subsequent calls hit the dedup path
    store.store(&data).unwrap();

    c.bench_function("cas_store_dedup_4k", |b| {
        b.iter(|| store.store(std::hint::black_box(&data)).unwrap())
    });
}

fn cas_read(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let store = CasStore::open(dir.path()).unwrap();
    let data = vec![0xAA; 4096];
    let (hash, _) = store.store(&data).unwrap();

    c.bench_function("cas_read_4k", |b| {
        b.iter(|| store.read(std::hint::black_box(&hash)).unwrap())
    });
}

fn metadata_lookup(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let meta = MetadataStore::open(dir.path().join("bench.redb")).unwrap();

    // Pre-populate
    let data = sentinel_fs::metadata::InodeData::regular([0xBB; 32], 1024, 0o644);
    meta.set_inode("AGENT-01", 42, &data).unwrap();

    c.bench_function("metadata_get_inode", |b| {
        b.iter(|| {
            meta.get_inode(std::hint::black_box("AGENT-01"), std::hint::black_box(42))
                .unwrap()
        })
    });
}

fn metadata_dirent_lookup(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let meta = MetadataStore::open(dir.path().join("bench.redb")).unwrap();

    meta.set_dirent("AGENT-01", 1, "hello.txt", 42).unwrap();

    c.bench_function("metadata_get_dirent", |b| {
        b.iter(|| {
            meta.get_dirent(
                std::hint::black_box("AGENT-01"),
                std::hint::black_box(1),
                std::hint::black_box("hello.txt"),
            )
            .unwrap()
        })
    });
}

fn layer_read_fallback(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let cas = CasStore::open(dir.path()).unwrap();
    let meta = MetadataStore::open(dir.path().join("bench.redb")).unwrap();
    let lm = LayerManager::new(cas, meta);
    lm.init_base_root().unwrap();

    // Populate base with a file
    let base_inode = lm
        .populate_base_file(1, "shared.txt", b"shared content data", 0o644)
        .unwrap();

    c.bench_function("layer_read_base_fallback", |b| {
        b.iter(|| {
            lm.read_file(
                std::hint::black_box("AGENT-01"),
                std::hint::black_box(base_inode),
            )
            .unwrap()
        })
    });
}

fn concurrent_writes(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let cas = CasStore::open(dir.path()).unwrap();
    let meta = MetadataStore::open(dir.path().join("bench.redb")).unwrap();
    let lm = LayerManager::new(cas, meta);
    lm.init_base_root().unwrap();

    let mut counter = 0u64;

    c.bench_function("layer_write_file", |b| {
        b.iter(|| {
            counter += 1;
            let name = format!("file-{counter}.txt");
            lm.write_file(
                std::hint::black_box("AGENT-01"),
                1,
                &name,
                std::hint::black_box(b"benchmark content payload"),
                0o644,
            )
            .unwrap()
        })
    });
}

// === New Artifact Plane Benchmarks ===

fn chunker_64kb_target(c: &mut Criterion) {
    // 8 MB of pseudo-random data — measures chunker throughput
    let data: Vec<u8> = (0..8_388_608u32)
        .map(|i| ((i.wrapping_mul(1664525).wrapping_add(1013904223)) >> 24) as u8)
        .collect();

    c.bench_function("chunker_64kb_target", |b| {
        b.iter(|| {
            let chunks: Vec<_> = chunk_data(std::hint::black_box(&data)).collect();
            std::hint::black_box(chunks.len())
        })
    });
}

fn ingest_1mb_file(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let data: Vec<u8> = (0..1_048_576u32)
        .map(|i| (i * 1664525 + 1013904223) as u8)
        .collect();

    c.bench_function("ingest_1mb_file", |b| {
        b.iter_batched(
            || {
                ArtifactPlane::open(dir.path().join(format!("ingest1mb_{}.redb", uuid_simple())))
                    .unwrap()
            },
            |plane| {
                let mut s = begin_ingest(&plane, "application/octet-stream");
                s.write(std::hint::black_box(&data));
                std::hint::black_box(commit_ingest(s).unwrap())
            },
            criterion::BatchSize::SmallInput,
        )
    });
}

fn ingest_100mb_file(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    // 100 MB — tests scaling behavior
    let data: Vec<u8> = (0..104_857_600u32)
        .map(|i| (i.wrapping_mul(1664525u32).wrapping_add(1013904223u32) >> 24) as u8)
        .collect();

    let mut group = c.benchmark_group("scaling");
    group.sample_size(10); // fewer samples for slow bench
    group.bench_function("ingest_100mb_file", |b| {
        b.iter_batched(
            || {
                ArtifactPlane::open(
                    dir.path()
                        .join(format!("ingest100mb_{}.redb", uuid_simple())),
                )
                .unwrap()
            },
            |plane| {
                let mut s = begin_ingest(&plane, "application/octet-stream");
                s.write(std::hint::black_box(&data));
                std::hint::black_box(commit_ingest(s).unwrap())
            },
            criterion::BatchSize::SmallInput,
        )
    });
    group.finish();
}

fn dedup_identical_files(c: &mut Criterion) {
    // Measures that dedup path (chunk already present) is fast
    let dir = tempfile::tempdir().unwrap();
    let plane = ArtifactPlane::open(dir.path().join("dedup_identical.redb")).unwrap();
    let data: Vec<u8> = (0..1_048_576u32).map(|i| (i * 3 + 5) as u8).collect();

    // Pre-ingest once so subsequent ingests hit dedup path entirely
    let mut s = begin_ingest(&plane, "application/octet-stream");
    s.write(&data);
    commit_ingest(s).unwrap();

    c.bench_function("dedup_identical_files", |b| {
        b.iter(|| {
            let mut s = begin_ingest(&plane, "application/octet-stream");
            s.write(std::hint::black_box(&data));
            std::hint::black_box(commit_ingest(s).unwrap())
        })
    });
}

fn dedup_similar_files(c: &mut Criterion) {
    // Slightly different files: last byte changed — most chunks should still dedup
    let dir = tempfile::tempdir().unwrap();
    let plane = ArtifactPlane::open(dir.path().join("dedup_similar.redb")).unwrap();

    let base: Vec<u8> = (0..512_000u32).map(|i| (i * 7 + 3) as u8).collect();
    let mut s = begin_ingest(&plane, "application/octet-stream");
    s.write(&base);
    commit_ingest(s).unwrap();

    let mut counter = 0u8;
    c.bench_function("dedup_similar_files", |b| {
        b.iter(|| {
            counter = counter.wrapping_add(1);
            let mut variant = base.clone();
            *variant.last_mut().unwrap() = counter;
            let mut s = begin_ingest(&plane, "application/octet-stream");
            s.write(std::hint::black_box(&variant));
            std::hint::black_box(commit_ingest(s).unwrap())
        })
    });
}

fn read_planner_1mb(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let plane = ArtifactPlane::open(dir.path().join("read1mb.redb")).unwrap();
    let data: Vec<u8> = (0..1_048_576u32).map(|i| (i * 11 + 7) as u8).collect();

    let mut s = begin_ingest(&plane, "application/octet-stream");
    s.write(&data);
    let object_id = commit_ingest(s).unwrap();

    c.bench_function("read_planner_1mb", |b| {
        b.iter(|| {
            std::hint::black_box(read_object(&plane, std::hint::black_box(object_id)).unwrap())
        })
    });
}

fn gc_1000_orphans(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let plane = ArtifactPlane::open(dir.path().join("gc1000.redb")).unwrap();

    // Ingest 1000 small objects and release them — creates many orphan chunks
    let data_small: Vec<u8> = (0..70_000u32).map(|i| (i * 17 + 3) as u8).collect();

    let mut ids = Vec::new();
    for i in 0u8..=99 {
        let mut variant = data_small.clone();
        *variant.last_mut().unwrap() = i;
        let mut s = begin_ingest(&plane, "application/octet-stream");
        s.write(&variant);
        let id = commit_ingest(s).unwrap();
        ids.push(id);
    }

    // Release all objects — chunks become orphans
    for id in &ids {
        release_object(&plane, *id).unwrap();
    }

    let mut group = c.benchmark_group("gc");
    group.sample_size(10);
    group.bench_function("gc_1000_orphans", |b| {
        b.iter(|| std::hint::black_box(gc_chunks(&plane).unwrap()))
    });
    group.finish();
}

/// Generate a simple monotonic unique suffix for temp DB names.
fn uuid_simple() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static CTR: AtomicU64 = AtomicU64::new(0);
    CTR.fetch_add(1, Ordering::Relaxed)
}

criterion_group!(
    benches,
    // Legacy benchmarks (must remain green)
    cas_hash_throughput,
    cas_store_dedup,
    cas_read,
    metadata_lookup,
    metadata_dirent_lookup,
    layer_read_fallback,
    concurrent_writes,
    // New Artifact Plane benchmarks
    chunker_64kb_target,
    ingest_1mb_file,
    ingest_100mb_file,
    dedup_identical_files,
    dedup_similar_files,
    read_planner_1mb,
    gc_1000_orphans,
);
criterion_main!(benches);
