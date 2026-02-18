//! Benchmarks for sentinel-fs CAS store operations.

use criterion::{Criterion, criterion_group, criterion_main};

fn cas_hash_throughput(c: &mut Criterion) {
    let data = vec![0u8; 4096];
    c.bench_function("cas_hash_4k", |b| {
        b.iter(|| sentinel_fs::cas::CasStore::hash(std::hint::black_box(&data)))
    });
}

fn cas_store_dedup(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let store = sentinel_fs::cas::CasStore::open(dir.path()).unwrap();
    let data = vec![0xAA; 4096];

    // Pre-store so subsequent calls hit the dedup path
    store.store(&data).unwrap();

    c.bench_function("cas_store_dedup_4k", |b| {
        b.iter(|| store.store(std::hint::black_box(&data)).unwrap())
    });
}

fn cas_read(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let store = sentinel_fs::cas::CasStore::open(dir.path()).unwrap();
    let data = vec![0xAA; 4096];
    let (hash, _) = store.store(&data).unwrap();

    c.bench_function("cas_read_4k", |b| {
        b.iter(|| store.read(std::hint::black_box(&hash)).unwrap())
    });
}

criterion_group!(benches, cas_hash_throughput, cas_store_dedup, cas_read);
criterion_main!(benches);
