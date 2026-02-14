use anyhow::Context;
use sentinel_common::{AgentId, Emotion, RoomId, Tick, Timestamp};
use sentinel_ebpf::{AgentHealthChecker, IoProfiler, NetworkMonitor, PsiReader};
use sentinel_ecs::{attach_redb_store, create_simulation_world, spawn_agent, RedbStateStore, SimulationTime};
use sentinel_inference::{BitNetClient, BitNetConfig};
use sentinel_limbo::{ChatStore, NewMessage};
use sentinel_redb::StateStore;
use sentinel_telemetry::{HealthRegistry, HealthStatus, MetricsRegistry};
use sentinel_zenoh::SentinelBus;
use std::hint::black_box;
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use tracing::info_span;
use tracing_subscriber::fmt;
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

fn p95_us(values: &mut [f64]) -> f64 {
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = values.len();
    if n == 0 {
        return 0.0;
    }
    let idx = ((95 * n) + 99) / 100 - 1;
    values[idx.min(n - 1)]
}

fn print_metric(name: &str, value: f64, unit: &str) {
    println!("METRIC\t{name}\t{value:.4}\t{unit}");
}

fn stack_tempdir() -> anyhow::Result<tempfile::TempDir> {
    let base = std::env::var("STACK_HARNESS_TMPDIR").unwrap_or_else(|_| "/tmp".to_string());
    let path = std::path::Path::new(&base);
    std::fs::create_dir_all(path)?;
    let dir = tempfile::Builder::new()
        .prefix("sentinel-stack-harness-")
        .tempdir_in(path)?;
    Ok(dir)
}

fn bench_feature_surface() {
    let schema_count = std::fs::read_dir("schemas")
        .ok()
        .map(|it| {
            it.filter_map(|e| e.ok())
                .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("fbs"))
                .count()
        })
        .unwrap_or(0);
    let rust_generated_count = std::fs::read_dir("crates/sentinel-common/src/generated")
        .ok()
        .map(|it| {
            it.filter_map(|e| e.ok())
                .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("rs"))
                .count()
        })
        .unwrap_or(0);
    let go_generated_count = std::fs::read_dir("cmd/cortex-gateway/internal/generated")
        .ok()
        .map(|it| {
            it.filter_map(|e| e.ok())
                .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("go"))
                .count()
        })
        .unwrap_or(0);

    print_metric("flatbuffers.schema.count", schema_count as f64, "count");
    print_metric(
        "flatbuffers.generated_rust.count",
        rust_generated_count as f64,
        "count",
    );
    print_metric(
        "flatbuffers.generated_go.count",
        go_generated_count as f64,
        "count",
    );
    print_metric(
        "flatbuffers.codegen.ready",
        if rust_generated_count > 0 && go_generated_count > 0 {
            1.0
        } else {
            0.0
        },
        "bool",
    );

    // SGMV is only planned in comments; there is no active runtime kernel path.
    print_metric("sgmv.runtime.available", 0.0, "bool");
}

fn bench_ecs() -> anyhow::Result<()> {
    let (mut world, mut schedule) = create_simulation_world();
    let persist_enabled = std::env::var("ECS_ENABLE_PERSIST")
        .ok()
        .map(|v| {
            let s = v.to_ascii_lowercase();
            matches!(s.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(true);
    let persist_every_n_ticks = std::env::var("ECS_PERSIST_EVERY_N_TICKS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(1)
        .max(1);
    let mut _persist_dir = None;

    if persist_enabled {
        let dir = stack_tempdir()?;
        let db_path = dir.path().join("ecs-persist.redb");
        let store = StateStore::open(
            db_path
                .to_str()
                .context("ecs persist redb path contains non-utf8 chars")?,
        )?;
        attach_redb_store(&mut world, store);
        if persist_every_n_ticks > 1 {
            world.resource_mut::<RedbStateStore>().persist_every_n_ticks = persist_every_n_ticks;
        }
        _persist_dir = Some(dir);
    }

    for i in 1..=15u16 {
        spawn_agent(
            &mut world,
            AgentId::new(i)?,
            &format!("Agent-{i:02}"),
            "Mitarbeiter",
            1,
        );
    }

    let ticks = 1000u64;
    let start = Instant::now();
    for tick in 0..ticks {
        let mut time = world.resource_mut::<SimulationTime>();
        time.tick = Tick(tick);
        time.tick_count = tick;
        time.delta_seconds = 1.0;
        time.sim_hour = 8.0 + (tick as f32 / 3600.0);
        schedule.run(&mut world);
    }
    let elapsed = start.elapsed();
    let total_us = elapsed.as_micros() as f64;
    let us_per_tick = total_us / ticks as f64;
    let ticks_per_s = ticks as f64 / elapsed.as_secs_f64();

    print_metric("ecs.ticks_per_s", ticks_per_s, "ticks/s");
    print_metric("ecs.us_per_tick", us_per_tick, "us");
    Ok(())
}

fn bench_redb() -> anyhow::Result<()> {
    let dir = stack_tempdir()?;
    let db_path = dir.path().join("state.redb");
    let store = StateStore::open(
        db_path
            .to_str()
            .context("redb path contains non-utf8 chars")?,
    )?;

    let n = 5000u16;
    let mut entries = Vec::with_capacity(n as usize);
    for i in 0..n {
        let id = AgentId::new((i % 54) + 1)?;
        let payload = format!("state-{i}");
        entries.push((id, payload.into_bytes()));
    }
    let start_w = Instant::now();
    store.set_agent_states_batch(&entries)?;
    let write_elapsed = start_w.elapsed();

    let start_r = Instant::now();
    for i in 0..n {
        let id = AgentId::new((i % 54) + 1)?;
        let _ = store.get_agent_state(id)?;
    }
    let read_elapsed = start_r.elapsed();

    print_metric(
        "redb.write_ops_s",
        n as f64 / write_elapsed.as_secs_f64(),
        "ops/s",
    );
    print_metric(
        "redb.read_ops_s",
        n as f64 / read_elapsed.as_secs_f64(),
        "ops/s",
    );
    Ok(())
}

fn bench_redb_mvcc() -> anyhow::Result<()> {
    let dir = stack_tempdir()?;
    let db_path = dir.path().join("state_mvcc.redb");
    let store = Arc::new(StateStore::open(
        db_path
            .to_str()
            .context("redb mvcc path contains non-utf8 chars")?,
    )?);
    let agent = AgentId::new(1)?;
    store.set_agent_state(agent, b"mvcc-state")?;

    let threads = std::env::var("REDB_MVCC_THREADS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(8)
        .max(1);
    let reads_per_thread = std::env::var("REDB_MVCC_READS_PER_THREAD")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(100_000)
        .max(1);

    let start = Instant::now();
    let mut handles = Vec::with_capacity(threads);
    for _ in 0..threads {
        let store = Arc::clone(&store);
        handles.push(thread::spawn(move || -> anyhow::Result<()> {
            for _ in 0..reads_per_thread {
                let v = store
                    .get_agent_state(agent)?
                    .ok_or_else(|| anyhow::anyhow!("mvcc state unexpectedly missing"))?;
                black_box(v);
            }
            Ok(())
        }));
    }
    for handle in handles {
        handle
            .join()
            .map_err(|_| anyhow::anyhow!("redb mvcc thread panicked"))??;
    }
    let elapsed = start.elapsed();
    let total_ops = (threads * reads_per_thread) as f64;
    print_metric(
        "redb.mvcc_read_ops_s",
        total_ops / elapsed.as_secs_f64(),
        "ops/s",
    );
    print_metric(
        "redb.mvcc_read_us",
        elapsed.as_micros() as f64 / total_ops,
        "us/op",
    );
    Ok(())
}

async fn bench_limbo() -> anyhow::Result<()> {
    let dir = stack_tempdir()?;
    let db_path = dir.path().join("chat.db");
    let store = ChatStore::open(
        db_path
            .to_str()
            .context("limbo path contains non-utf8 chars")?,
    )
    .await?;

    let n = 1000u64;
    let room = RoomId::new(1)?;
    let mut messages = Vec::with_capacity(n as usize);
    for i in 0..n {
        let agent = AgentId::new((i as u16 % 54) + 1)?;
        let content = format!("msg-{i}");
        messages.push(NewMessage {
            room_id: room,
            agent_id: agent,
            content,
            emotion: Some(Emotion::Neutral),
            timestamp: Timestamp(i),
            tick: Tick(i),
        });
    }
    let start_w = Instant::now();
    store.insert_messages_batch(&messages).await?;
    let write_elapsed = start_w.elapsed();

    let start_q = Instant::now();
    let _rows = store.get_room_messages(room, 100).await?;
    let query_elapsed = start_q.elapsed();

    print_metric(
        "limbo.insert_ops_s",
        n as f64 / write_elapsed.as_secs_f64(),
        "ops/s",
    );
    print_metric("limbo.query_us", query_elapsed.as_micros() as f64, "us");
    Ok(())
}

async fn bench_limbo_concurrent_writes() -> anyhow::Result<()> {
    let dir = stack_tempdir()?;
    let db_path = dir.path().join("chat_mvcc.db");
    let store = Arc::new(
        ChatStore::open(
            db_path
                .to_str()
                .context("limbo mvcc path contains non-utf8 chars")?,
        )
        .await?,
    );

    let workers = std::env::var("LIMBO_MVCC_WORKERS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(8)
        .max(1);
    let batch_size = std::env::var("LIMBO_MVCC_BATCH_SIZE")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(1000)
        .max(1);
    let room = RoomId::new(1)?;

    let start = Instant::now();
    let mut handles = Vec::with_capacity(workers);
    for worker in 0..workers {
        let store = Arc::clone(&store);
        handles.push(tokio::spawn(async move {
            let mut batch = Vec::with_capacity(batch_size);
            for i in 0..batch_size {
                let n = (worker * batch_size + i) as u64;
                let agent = AgentId::new(((n as u16) % 54) + 1)?;
                batch.push(NewMessage {
                    room_id: room,
                    agent_id: agent,
                    content: format!("mvcc-msg-{worker}-{i}"),
                    emotion: Some(Emotion::Neutral),
                    timestamp: Timestamp(n),
                    tick: Tick(n),
                });
            }
            store.insert_messages_batch(&batch).await?;
            Ok::<usize, anyhow::Error>(batch_size)
        }));
    }

    let mut total_rows = 0usize;
    for handle in handles {
        total_rows += handle.await??;
    }
    let elapsed = start.elapsed();
    let total_ops = total_rows as f64;
    print_metric(
        "limbo.mvcc_write_ops_s",
        total_ops / elapsed.as_secs_f64(),
        "ops/s",
    );
    print_metric(
        "limbo.mvcc_write_us",
        elapsed.as_micros() as f64 / total_ops,
        "us/op",
    );
    Ok(())
}

async fn bench_zenoh() -> anyhow::Result<()> {
    let bus = SentinelBus::new().await?;
    let topic = format!("sentinel/bench/latency/{}", std::process::id());
    let subscriber = bus.subscribe(&topic).await?;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let n = 500usize;
    let mut lats = Vec::with_capacity(n);
    for i in 0..n {
        let payload = (i as u64).to_le_bytes();
        let st = Instant::now();
        bus.publish(&topic, &payload).await?;
        let recv_res = tokio::time::timeout(Duration::from_secs(2), subscriber.recv_async())
            .await
            .context("zenoh recv timeout")?;
        let _sample = recv_res.map_err(|e| anyhow::anyhow!("zenoh recv error: {e}"))?;
        lats.push(st.elapsed().as_micros() as f64);
    }

    let mean = lats.iter().sum::<f64>() / lats.len() as f64;
    let p95 = p95_us(&mut lats);
    print_metric("zenoh.roundtrip_mean_us", mean, "us");
    print_metric("zenoh.roundtrip_p95_us", p95, "us");
    Ok(())
}

fn bench_ebpf_userspace() -> anyhow::Result<()> {
    let mut health = AgentHealthChecker::new();
    let n = 200_000u64;
    let st = Instant::now();
    for i in 0..n {
        health.record_write(i % 64, i);
    }
    let elapsed = st.elapsed();
    let ops_s = n as f64 / elapsed.as_secs_f64();
    print_metric("ebpf.userspace_health_ops_s", ops_s, "ops/s");

    let mut io = IoProfiler::new();
    for i in 0..50_000u64 {
        io.record_read(i % 8, "bench", 4096);
        io.record_write(i % 8, "bench", 4096);
    }
    let total_iops: u64 = io.all_metrics().values().map(|m| m.total_iops()).sum();
    print_metric(
        "ebpf.userspace_io_events",
        total_iops as f64,
        "events(total)",
    );

    let mut net = NetworkMonitor::new();
    for _ in 0..1000 {
        net.record_request(
            "api.anthropic.com:443",
            Duration::from_millis(120),
            1024,
            8192,
        );
    }
    let avg = net
        .get_metrics("api.anthropic.com:443")
        .and_then(|m| m.avg_latency())
        .map(|d| d.as_micros() as f64)
        .unwrap_or_default();
    print_metric("ebpf.userspace_net_avg_us", avg, "us");

    let psi = PsiReader::new("/sys/fs/cgroup");
    if let (Ok(cpu), Ok(mem), Ok(io_p)) = (
        psi.read_cpu_pressure(),
        psi.read_memory_pressure(),
        psi.read_io_pressure(),
    ) {
        let stress = sentinel_ebpf::psi::combined_stress_factor(&cpu, &mem, &io_p);
        print_metric("ebpf.psi_combined_stress", stress as f64, "ratio");
    } else {
        print_metric("ebpf.psi_combined_stress", -1.0, "ratio(unavailable)");
    }

    Ok(())
}

fn bench_telemetry_micro() -> anyhow::Result<()> {
    let registry = MetricsRegistry::global();
    let pid = std::process::id();

    let counter = registry.counter(&format!("sentinel.bench.counter.{pid}.count"));
    let counter_iters = 20_000_000u64;
    let start = Instant::now();
    for _ in 0..counter_iters {
        counter.increment();
    }
    let counter_ns = start.elapsed().as_nanos() as f64 / counter_iters as f64;
    print_metric("telemetry.counter_increment_ns", counter_ns, "ns/op");

    let histogram = registry.histogram(
        &format!("sentinel.bench.histogram.{pid}.duration_us"),
        &[1.0, 10.0, 50.0, 100.0, 500.0, 1000.0],
    );
    let hist_iters = 3_000_000u64;
    let start = Instant::now();
    for i in 0..hist_iters {
        histogram.observe((i % 1000) as f64);
    }
    let histogram_ns = start.elapsed().as_nanos() as f64 / hist_iters as f64;
    print_metric("telemetry.histogram_record_ns", histogram_ns, "ns/op");

    let span_iters = 2_000_000u64;
    let start = Instant::now();
    for _ in 0..span_iters {
        let span = info_span!("bench_span", tick = 1u64);
        let _guard = span.enter();
        black_box(1u64);
    }
    let span_ns = start.elapsed().as_nanos() as f64 / span_iters as f64;
    print_metric("telemetry.span_enter_exit_ns", span_ns, "ns/op");

    for i in 0..128u64 {
        let c = registry.counter(&format!("sentinel.bench.snapshot.{pid}.{i}.count"));
        c.increment_by(i + 1);
        let h = registry.histogram(
            &format!("sentinel.bench.snapshot.{pid}.{i}.duration_us"),
            &[5.0, 10.0, 50.0, 100.0, 500.0],
        );
        h.observe(i as f64);
    }
    let snapshot_iters = 500u64;
    let start = Instant::now();
    for _ in 0..snapshot_iters {
        let (counters, histograms) = registry.snapshot_raw();
        black_box((counters.len(), histograms.len()));
    }
    let snapshot_us = start.elapsed().as_micros() as f64 / snapshot_iters as f64;
    print_metric("telemetry.metrics_snapshot_us", snapshot_us, "us/op");

    let health = HealthRegistry::global();
    for i in 0..64u64 {
        let name = format!("bench-health-{pid}-{i}");
        health.register(&name, || HealthStatus::Healthy);
    }
    let health_iters = 2000u64;
    let start = Instant::now();
    for _ in 0..health_iters {
        let checks = health.check_all();
        black_box(checks.len());
    }
    let health_us = start.elapsed().as_micros() as f64 / health_iters as f64;
    print_metric("telemetry.health_check_us", health_us, "us/op");

    let subscriber = tracing_subscriber::registry()
        .with(EnvFilter::new("info"))
        .with(
            fmt::layer()
                .json()
                .with_ansi(false)
                .with_target(false)
                .without_time()
                .with_writer(std::io::sink),
        );
    let dispatch = tracing::Dispatch::new(subscriber);
    let log_iters = 100_000u64;
    let start = Instant::now();
    tracing::dispatcher::with_default(&dispatch, || {
        for i in 0..log_iters {
            tracing::info!(seq = i, bench = "telemetry", event = "log_emit");
        }
    });
    let log_ns = start.elapsed().as_nanos() as f64 / log_iters as f64;
    print_metric("telemetry.log_emission_ns", log_ns, "ns/op");

    Ok(())
}

fn bench_decision() -> anyhow::Result<()> {
    use bevy_ecs::prelude::*;
    use sentinel_common::components::BioState;

    let (mut world, _) = create_simulation_world();

    // 24 Agents spawnen (15 Schicht + 9 Sonder)
    let mut entities = Vec::new();
    for i in 1..=24u16 {
        let shift_set = if i <= 15 { 1 } else { 0 };
        let entity = spawn_agent(
            &mut world,
            AgentId::new(i)?,
            &format!("Agent-{i:02}"),
            "Mitarbeiter",
            shift_set,
        );
        entities.push(entity);
    }

    // Realistische Bio-Mischung: verschiedene Prioritaeten triggern
    for (idx, &entity) in entities.iter().enumerate() {
        let mut bio = world.get_mut::<BioState>(entity).unwrap();
        match idx % 6 {
            0 => bio.bladder = 92.0,    // P0: Toilette-Notfall
            1 => bio.energy = 12.0,     // P0: Energie-Notfall
            2 => bio.hunger = 85.0,     // P1: Hunger
            3 => bio.stress = 75.0,     // P2: Stress
            4 => bio.caffeine_mg = 10.0, // P2: Koffein-Entzug
            _ => {}                      // Default: keine Events
        }
    }

    // Decision-only Schedule (isolierte Messung)
    let mut decision_schedule = Schedule::default();
    decision_schedule.add_systems(sentinel_ecs::decision::decision_system);

    // Warmup (10 Ticks)
    for tick in 0..10u64 {
        world.resource_mut::<SimulationTime>().tick = Tick(tick);
        decision_schedule.run(&mut world);
    }

    // Messung (1000 Ticks)
    let ticks = 1000u64;
    let mut tick_latencies = Vec::with_capacity(ticks as usize);
    for tick in 10..10 + ticks {
        world.resource_mut::<SimulationTime>().tick = Tick(tick);
        let st = Instant::now();
        decision_schedule.run(&mut world);
        tick_latencies.push(st.elapsed().as_micros() as f64);
    }

    let mean_us = tick_latencies.iter().sum::<f64>() / tick_latencies.len() as f64;
    let p95 = p95_us(&mut tick_latencies);
    print_metric("decision.24_agents.mean_us", mean_us, "us");
    print_metric("decision.24_agents.p95_us", p95, "us");
    print_metric(
        "decision.24_agents.ticks_per_s",
        1_000_000.0 / mean_us,
        "ticks/s",
    );

    Ok(())
}

fn bench_bitnet() {
    let binary = std::env::var("BITNET_BINARY").unwrap_or_else(|_| "bitnet/bitnet-inference".into());
    let model = std::env::var("BITNET_MODEL").unwrap_or_else(|_| "bitnet/model.gguf".into());
    let threads = std::env::var("BITNET_THREADS")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(8);
    let max_tokens = std::env::var("BITNET_MAX_TOKENS")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(128);
    let prompt = std::env::var("BITNET_PROMPT")
        .unwrap_or_else(|_| "Schreibe einen kurzen Satz ueber Teamarbeit.".to_string());

    let binary_available = Path::new(&binary).exists();
    let model_available = Path::new(&model).exists();
    print_metric(
        "bitnet.binary.available",
        if binary_available { 1.0 } else { 0.0 },
        "bool",
    );
    print_metric(
        "bitnet.model.available",
        if model_available { 1.0 } else { 0.0 },
        "bool",
    );

    if !binary_available || !model_available {
        print_metric("bitnet.generate.ok", 0.0, "bool");
        print_metric("bitnet.inference_tok_s", -1.0, "tok/s(unavailable)");
        print_metric("bitnet.inference_latency_ms", -1.0, "ms(unavailable)");
        return;
    }

    let client = BitNetClient::new(BitNetConfig {
        binary_path: binary,
        model_path: model,
        threads,
        max_tokens,
    });

    let start = Instant::now();
    match client.generate(&prompt) {
        Ok(text) => {
            let elapsed = start.elapsed();
            let tokens = text.split_whitespace().count().max(1) as f64;
            print_metric("bitnet.generate.ok", 1.0, "bool");
            print_metric(
                "bitnet.inference_tok_s",
                tokens / elapsed.as_secs_f64(),
                "tok/s",
            );
            print_metric(
                "bitnet.inference_latency_ms",
                elapsed.as_millis() as f64,
                "ms",
            );
            print_metric("bitnet.generated_tokens", tokens, "count");
        }
        Err(_) => {
            print_metric("bitnet.generate.ok", 0.0, "bool");
            print_metric("bitnet.inference_tok_s", -1.0, "tok/s(error)");
            print_metric("bitnet.inference_latency_ms", -1.0, "ms(error)");
        }
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> anyhow::Result<()> {
    bench_feature_surface();
    bench_ecs()?;
    bench_decision()?;
    bench_redb()?;
    bench_redb_mvcc()?;
    bench_limbo().await?;
    bench_limbo_concurrent_writes().await?;
    bench_zenoh().await?;
    bench_ebpf_userspace()?;
    bench_telemetry_micro()?;
    bench_bitnet();
    Ok(())
}
