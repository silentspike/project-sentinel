//! SHM Performance Benchmarks fuer sentinel-zenoh.
//!
//! Misst Latenz, Throughput und Concurrency-Verhalten des Zenoh SHM Core-Bus.
//! NUR auf Deploy-VM (10.0.0.240) ausfuehren — NIEMALS auf cargo remote oder lokal!

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use sentinel_common::{AgentId, BioStateUpdate, ChaosEvent, EventType, RoomId, Tick, Timestamp};
use sentinel_zenoh::flatbuf;
use sentinel_zenoh::SentinelBus;
use std::time::Duration;
use tokio::runtime::Runtime;

/// Helper: Erstellt einen tokio Runtime fuer async Benchmarks.
fn bench_runtime() -> Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("tokio Runtime erstellen")
}

/// Benchmark 1: Pub/Sub Roundtrip-Latenz bei verschiedenen Payload-Groessen.
///
/// Misst p50/p95/p99 ueber viele Iterationen.
/// Vergleicht implizit SHM vs Network (abhaengig von SENTINEL_ZENOH_SHM ENV).
fn shm_pub_sub_latency(c: &mut Criterion) {
    let rt = bench_runtime();
    // Eine Bus-Instanz fuer Publish UND Subscribe (Self-Loopback).
    // Entspricht dem echten Daemon-Betrieb (ein SentinelBus).
    let bus = rt.block_on(SentinelBus::new()).expect("Bus fuer Latency");

    let mut group = c.benchmark_group("shm_pub_sub_latency");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(1000);

    for payload_size in [64, 256, 1024, 4096, 16384] {
        let payload = vec![0xABu8; payload_size];
        let topic = format!("sentinel/bench/latency/{payload_size}");

        // Subscribe EINMAL vor dem Benchmark-Loop.
        let sub = rt.block_on(bus.subscribe(&topic)).expect("subscribe");
        // Warm-up mit Retry — Self-Loopback sollte sofort funktionieren,
        // aber sicherheitshalber retry fuer Zenoh-interne Propagation.
        rt.block_on(async {
            for attempt in 0..5 {
                std::thread::sleep(Duration::from_millis(100));
                bus.publish(&topic, &[0u8]).await.expect("warmup pub");
                match tokio::time::timeout(Duration::from_millis(500), sub.recv_async()).await {
                    Ok(Ok(_)) => break,
                    _ => {
                        if attempt == 4 {
                            panic!("Subscription warmup failed for {topic}");
                        }
                    }
                }
            }
        });

        group.bench_with_input(
            BenchmarkId::from_parameter(payload_size),
            &payload_size,
            |b, _| {
                b.to_async(&rt).iter(|| {
                    let bus = bus.clone();
                    let sub_ref = &sub;
                    let p = payload.clone();
                    let t = topic.clone();
                    async move {
                        bus.publish(&t, &p).await.expect("publish");
                        let sample =
                            tokio::time::timeout(Duration::from_millis(500), sub_ref.recv_async())
                                .await
                                .expect("timeout")
                                .expect("recv");
                        black_box(sample.payload().to_bytes().len());
                    }
                });
            },
        );
    }
    group.finish();
}

/// Benchmark 2: Maximaler Durchsatz (Messages/s) bei 1KB Payloads.
fn shm_throughput(c: &mut Criterion) {
    let rt = bench_runtime();
    let bus = rt
        .block_on(SentinelBus::new())
        .expect("Bus fuer Throughput");

    let payload = vec![0xCDu8; 1024];
    let topic = "sentinel/bench/throughput";

    let mut group = c.benchmark_group("shm_throughput");
    group.measurement_time(Duration::from_secs(10));
    group.throughput(criterion::Throughput::Bytes(1024));
    group.sample_size(500);

    group.bench_function("1kb_publish", |b| {
        b.to_async(&rt).iter(|| {
            let bus = bus.clone();
            let p = payload.clone();
            async move {
                bus.publish(topic, &p).await.expect("publish");
            }
        });
    });
    group.finish();
}

/// Benchmark 3: 24 gleichzeitige Publisher (simuliert Agent-Last).
fn shm_concurrent_publishers(c: &mut Criterion) {
    let rt = bench_runtime();

    let mut group = c.benchmark_group("shm_concurrent_publishers");
    group.measurement_time(Duration::from_secs(15));
    group.sample_size(50);

    // Eine Bus-Instanz, gecloned an 24 Tasks (wie im Daemon — Clone ist billig).
    let bus = rt
        .block_on(SentinelBus::new())
        .expect("Bus fuer Concurrency");

    group.bench_function("24_publishers_x100_msgs", |b| {
        b.to_async(&rt).iter(|| {
            let bus = bus.clone();
            async move {
                let mut handles = Vec::new();
                for i in 0..24u8 {
                    let bus = bus.clone();
                    let handle = tokio::spawn(async move {
                        let topic = format!("sentinel/bench/concurrent/{i}");
                        let payload = vec![i; 256];
                        for _ in 0..100 {
                            bus.publish(&topic, &payload).await.expect("publish");
                        }
                    });
                    handles.push(handle);
                }
                for h in handles {
                    h.await.expect("join");
                }
            }
        });
    });
    group.finish();
}

/// Benchmark 4: Overhead der Fan-Out Bridge (Channel try_send).
///
/// Misst den zusaetzlichen Overhead von try_send() im ECS-Thread-Pfad.
fn shm_fanout_overhead(c: &mut Criterion) {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(256);

    // Drain receiver im Hintergrund
    let rt = bench_runtime();
    rt.spawn(async move { while rx.recv().await.is_some() {} });

    let payload = vec![0xEFu8; 512];

    let mut group = c.benchmark_group("shm_fanout_overhead");
    group.measurement_time(Duration::from_secs(5));

    group.bench_function("try_send_512b", |b| {
        b.iter(|| {
            let _ = tx.try_send(black_box(payload.clone()));
        });
    });
    group.finish();
}

/// Benchmark 5: Scoped Query Roundtrip.
///
/// Misst Query Request → Response Latenz (ohne redb I/O, nur Zenoh Transport).
fn shm_query_roundtrip(c: &mut Criterion) {
    let rt = bench_runtime();

    // Eine Bus-Instanz (Self-Loopback) — wie im echten Daemon.
    let bus = rt.block_on(SentinelBus::new()).expect("Bus fuer Query");

    let request_topic = "sentinel/bench/query/request";
    let response_topic = "sentinel/bench/query/response";

    // Subscriptions erstellen.
    let request_sub = rt
        .block_on(bus.subscribe(request_topic))
        .expect("request subscribe");
    let response_sub = rt
        .block_on(bus.subscribe(response_topic))
        .expect("response subscribe");

    // Responder-Task laeuft dauerhaft im Hintergrund.
    let resp_bus = bus.clone();
    rt.spawn(async move {
        while request_sub.recv_async().await.is_ok() {
            let _ = resp_bus.publish(response_topic, b"OK").await;
        }
    });

    // Warm-up mit Retry.
    rt.block_on(async {
        for attempt in 0..5 {
            std::thread::sleep(Duration::from_millis(100));
            bus.publish(request_topic, b"WARMUP")
                .await
                .expect("warmup req");
            match tokio::time::timeout(Duration::from_secs(1), response_sub.recv_async()).await {
                Ok(Ok(_)) => break,
                _ => {
                    if attempt == 4 {
                        panic!("Query roundtrip warmup failed");
                    }
                }
            }
        }
    });

    let mut group = c.benchmark_group("shm_query_roundtrip");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(200);

    group.bench_function("query_response_roundtrip", |b| {
        b.to_async(&rt).iter(|| {
            let bus = bus.clone();
            let sub_ref = &response_sub;
            async move {
                bus.publish(request_topic, b"QUERY").await.expect("request");
                let resp = tokio::time::timeout(Duration::from_secs(1), sub_ref.recv_async())
                    .await
                    .expect("timeout")
                    .expect("recv");
                black_box(resp.payload().to_bytes().len());
            }
        });
    });
    group.finish();
}

/// Benchmark 6: Impact verschiedener Channel-Kapazitaeten auf try_send.
fn shm_buffer_sizing(c: &mut Criterion) {
    let rt = bench_runtime();

    let mut group = c.benchmark_group("shm_buffer_sizing");
    group.measurement_time(Duration::from_secs(5));

    for capacity in [64, 128, 256, 512, 1024] {
        group.bench_with_input(
            BenchmarkId::from_parameter(capacity),
            &capacity,
            |b, &cap| {
                let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(cap);
                rt.spawn(async move { while rx.recv().await.is_some() {} });

                let payload = vec![0xABu8; 1024];

                b.iter(|| {
                    // Burst: 100 messages
                    let mut sent = 0u64;
                    let mut dropped = 0u64;
                    for _ in 0..100 {
                        match tx.try_send(payload.clone()) {
                            Ok(()) => sent += 1,
                            Err(_) => dropped += 1,
                        }
                    }
                    black_box((sent, dropped));
                });
            },
        );
    }
    group.finish();
}

/// Benchmark 7: FlatBuffer encode + decode (ohne Zenoh, reine Serialisierung).
fn flatbuffer_encode_decode(c: &mut Criterion) {
    let bio = BioStateUpdate {
        agent_id: AgentId(7),
        hunger: 45.5,
        energy: 72.0,
        caffeine_mg: 95.0,
        bladder: 30.0,
        stress: 55.0,
        social_need: 20.0,
        comfort: 80.0,
        timestamp: Timestamp(2000),
        tick: Tick(100),
    };

    let chaos = ChaosEvent {
        event_type: EventType::PrinterBroken,
        target_room: Some(RoomId(5)),
        target_agent: None,
        description: "Drucker zeigt Papierstau an".to_string(),
        duration_minutes: Some(30),
        timestamp: Timestamp(5000),
        tick: Tick(200),
    };

    let mut group = c.benchmark_group("flatbuffer_encode_decode");
    group.measurement_time(Duration::from_secs(5));

    group.bench_function("bio_state_encode", |b| {
        b.iter(|| black_box(flatbuf::encode_bio_state(black_box(&bio))))
    });

    let bio_bytes = flatbuf::encode_bio_state(&bio);
    group.bench_function("bio_state_decode", |b| {
        b.iter(|| black_box(flatbuf::decode_bio_state(black_box(&bio_bytes)).unwrap()))
    });

    group.bench_function("bio_state_roundtrip", |b| {
        b.iter(|| {
            let bytes = flatbuf::encode_bio_state(black_box(&bio));
            black_box(flatbuf::decode_bio_state(&bytes).unwrap())
        })
    });

    group.bench_function("chaos_event_encode", |b| {
        b.iter(|| black_box(flatbuf::encode_chaos_event(black_box(&chaos))))
    });

    let chaos_bytes = flatbuf::encode_chaos_event(&chaos);
    group.bench_function("chaos_event_decode", |b| {
        b.iter(|| black_box(flatbuf::decode_chaos_event(black_box(&chaos_bytes)).unwrap()))
    });

    group.finish();
}

/// Benchmark 8: JSON vs FlatBuffer Encode-Vergleich.
fn json_vs_flatbuffer_encode(c: &mut Criterion) {
    let bio = BioStateUpdate {
        agent_id: AgentId(7),
        hunger: 45.5,
        energy: 72.0,
        caffeine_mg: 95.0,
        bladder: 30.0,
        stress: 55.0,
        social_need: 20.0,
        comfort: 80.0,
        timestamp: Timestamp(2000),
        tick: Tick(100),
    };

    let mut group = c.benchmark_group("json_vs_flatbuffer");
    group.measurement_time(Duration::from_secs(5));

    group.bench_function("json_encode", |b| {
        b.iter(|| black_box(serde_json::to_vec(black_box(&bio)).unwrap()))
    });

    group.bench_function("flatbuffer_encode", |b| {
        b.iter(|| black_box(flatbuf::encode_bio_state(black_box(&bio))))
    });

    // Decode-Vergleich
    let json_bytes = serde_json::to_vec(&bio).unwrap();
    let fb_bytes = flatbuf::encode_bio_state(&bio);

    group.bench_function("json_decode", |b| {
        b.iter(|| {
            black_box(serde_json::from_slice::<BioStateUpdate>(black_box(&json_bytes)).unwrap())
        })
    });

    group.bench_function("flatbuffer_decode", |b| {
        b.iter(|| black_box(flatbuf::decode_bio_state(black_box(&fb_bytes)).unwrap()))
    });

    group.finish();
}

/// Benchmark 9: FlatBuffer-Payload durch Zenoh SHM Pub/Sub Roundtrip.
///
/// Dies ist der EIGENTLICHE AC-1 Beweis: p99 Roundtrip < 200us.
fn flatbuffer_shm_roundtrip(c: &mut Criterion) {
    let rt = bench_runtime();
    let bus = rt
        .block_on(SentinelBus::new())
        .expect("Bus fuer FB roundtrip");

    let bio = BioStateUpdate {
        agent_id: AgentId(7),
        hunger: 45.5,
        energy: 72.0,
        caffeine_mg: 95.0,
        bladder: 30.0,
        stress: 55.0,
        social_need: 20.0,
        comfort: 80.0,
        timestamp: Timestamp(2000),
        tick: Tick(100),
    };
    let fb_payload = flatbuf::encode_bio_state(&bio);
    let topic = "sentinel/bench/fb_roundtrip/state";

    let sub = rt.block_on(bus.subscribe(topic)).expect("subscribe");

    // Warmup
    rt.block_on(async {
        for attempt in 0..5 {
            std::thread::sleep(Duration::from_millis(100));
            bus.publish(topic, &fb_payload).await.expect("warmup pub");
            match tokio::time::timeout(Duration::from_millis(500), sub.recv_async()).await {
                Ok(Ok(_)) => break,
                _ => {
                    if attempt == 4 {
                        panic!("FB roundtrip warmup failed");
                    }
                }
            }
        }
    });

    let mut group = c.benchmark_group("flatbuffer_shm_roundtrip");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(1000);

    group.bench_function("bio_state_pub_sub_decode", |b| {
        b.to_async(&rt).iter(|| {
            let bus = bus.clone();
            let sub_ref = &sub;
            let payload = fb_payload.clone();
            async move {
                bus.publish(topic, &payload).await.expect("publish");
                let sample = tokio::time::timeout(Duration::from_millis(500), sub_ref.recv_async())
                    .await
                    .expect("timeout")
                    .expect("recv");
                let bytes = sample.payload().to_bytes();
                let decoded = flatbuf::decode_bio_state(bytes.as_ref()).expect("decode");
                black_box(decoded.hunger);
            }
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    shm_pub_sub_latency,
    shm_throughput,
    shm_concurrent_publishers,
    shm_fanout_overhead,
    shm_query_roundtrip,
    shm_buffer_sizing,
    flatbuffer_encode_decode,
    json_vs_flatbuffer_encode,
    flatbuffer_shm_roundtrip,
);
criterion_main!(benches);
