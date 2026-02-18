//! Benchmarks for sentinel-fs: CAS, metadata, and layer operations.

use criterion::{criterion_group, criterion_main, Criterion};
use sentinel_fs::cas::CasStore;
use sentinel_fs::layer::LayerManager;
use sentinel_fs::metadata::MetadataStore;

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

criterion_group!(
    benches,
    cas_hash_throughput,
    cas_store_dedup,
    cas_read,
    metadata_lookup,
    metadata_dirent_lookup,
    layer_read_fallback,
    concurrent_writes
);
criterion_main!(benches);
