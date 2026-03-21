use anyhow::{bail, Context};
use sentinel_common::{AgentId, Tick};
use sentinel_ecs::{
    attach_redb_store, create_simulation_world, spawn_agent, PersistTelemetry, RedbStateStore,
    SimulationTime,
};
use sentinel_redb::StateStore;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

fn env_required(name: &str) -> anyhow::Result<String> {
    std::env::var(name).with_context(|| format!("missing required env var: {name}"))
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(default)
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(default)
}

fn env_f32(name: &str, default: f32) -> f32 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
        .unwrap_or(default)
}

fn env_bool(name: &str, default: bool) -> bool {
    std::env::var(name)
        .ok()
        .map(|v| {
            let s = v.to_ascii_lowercase();
            matches!(s.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(default)
}

fn metric(name: &str, value: f64, unit: &str) {
    println!("METRIC\t{name}\t{value:.4}\t{unit}");
}

fn result(name: &str, value: &str) {
    println!("RESULT\t{name}\t{value}");
}

fn percentile(values: &[f64], pct: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let rank = ((pct / 100.0) * (sorted.len() as f64 - 1.0)).round() as usize;
    sorted[rank.min(sorted.len() - 1)]
}

fn extract_tick(state: &[u8]) -> Option<u64> {
    let s = std::str::from_utf8(state).ok()?;
    let rest = s.strip_prefix("t=")?;
    let tick = rest.split(';').next()?;
    tick.parse::<u64>().ok()
}

fn compute_state_digest(store: &StateStore) -> anyhow::Result<(String, usize, u64, u64, usize)> {
    let mut ids = store.list_agents()?;
    ids.sort_by_key(|id| id.0);

    let mut hasher = Sha256::new();
    let mut min_tick = u64::MAX;
    let mut max_tick = 0u64;
    let mut tick_parse_failures = 0usize;

    for id in &ids {
        let Some(state) = store.get_agent_state(*id)? else {
            continue;
        };
        hasher.update(id.0.to_le_bytes());
        hasher.update((state.len() as u32).to_le_bytes());
        hasher.update(&state);

        if let Some(tick) = extract_tick(&state) {
            min_tick = min_tick.min(tick);
            max_tick = max_tick.max(tick);
        } else {
            tick_parse_failures += 1;
        }
    }

    if ids.is_empty() {
        min_tick = 0;
        max_tick = 0;
    }

    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for b in digest {
        hex.push_str(&format!("{b:02x}"));
    }

    Ok((hex, ids.len(), min_tick, max_tick, tick_parse_failures))
}

fn simulate() -> anyhow::Result<()> {
    let db_path = env_required("DB_PATH")?;
    let ticks = env_u64("SIM_TICKS", 5000);
    let duration_secs = env_u64("SIM_DURATION_SECS", 0);
    let agents = env_usize("SIM_AGENTS", 15);
    let persist_every = env_u64("PERSIST_EVERY", 20).max(1);
    let sleep_us = env_u64("SIM_SLEEP_US", 0);
    let dt_seconds = env_f32("SIM_DT_SECONDS", 1.0).max(0.001);
    let collect_hist = env_bool("SIM_COLLECT_TICK_HIST", true);

    if agents == 0 {
        bail!("SIM_AGENTS must be >=1");
    }
    if agents > u16::MAX as usize {
        bail!("SIM_AGENTS must be <= {}", u16::MAX);
    }

    if let Some(parent) = Path::new(&db_path).parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create parent directory for {db_path}"))?;
    }

    let store = StateStore::open(&db_path)?;
    let (mut world, mut schedule) = create_simulation_world();
    attach_redb_store(&mut world, store);
    world.resource_mut::<RedbStateStore>().persist_every_n_ticks = persist_every;

    for i in 1..=agents {
        let id = AgentId::new(i as u16)?;
        spawn_agent(&mut world, id, &format!("Agent-{i:02}"), "Mitarbeiter", 1, "empfang");
    }

    let start = Instant::now();
    let duration_target = if duration_secs > 0 {
        Some(Duration::from_secs(duration_secs))
    } else {
        None
    };
    let mut tick_lat_us = Vec::new();
    let mut executed_ticks = 0u64;

    loop {
        if let Some(target) = duration_target {
            if start.elapsed() >= target {
                break;
            }
        } else if executed_ticks >= ticks {
            break;
        }

        let tick = executed_ticks;
        {
            let mut time = world.resource_mut::<SimulationTime>();
            time.tick = Tick(tick);
            time.tick_count = tick;
            time.delta_seconds = dt_seconds;
            time.sim_hour = 8.0 + (tick as f32 * dt_seconds / 3600.0);
        }

        let tick_start = Instant::now();
        schedule.run(&mut world);
        let tick_us = tick_start.elapsed().as_secs_f64() * 1_000_000.0;
        if collect_hist {
            tick_lat_us.push(tick_us);
        }

        executed_ticks += 1;
        if sleep_us > 0 {
            thread::sleep(Duration::from_micros(sleep_us));
        }
    }

    let elapsed = start.elapsed();
    let total_us = elapsed.as_secs_f64() * 1_000_000.0;
    let ticks_per_s = if elapsed.as_secs_f64() > 0.0 {
        executed_ticks as f64 / elapsed.as_secs_f64()
    } else {
        0.0
    };
    let us_per_tick = if executed_ticks > 0 {
        total_us / executed_ticks as f64
    } else {
        0.0
    };

    metric("ecs.ticks_executed", executed_ticks as f64, "ticks");
    metric("ecs.runtime_s", elapsed.as_secs_f64(), "s");
    metric("ecs.ticks_per_s", ticks_per_s, "ticks/s");
    metric("ecs.us_per_tick", us_per_tick, "us");

    if collect_hist && !tick_lat_us.is_empty() {
        metric("ecs.tick_us_p50", percentile(&tick_lat_us, 50.0), "us");
        metric("ecs.tick_us_p95", percentile(&tick_lat_us, 95.0), "us");
        metric("ecs.tick_us_p99", percentile(&tick_lat_us, 99.0), "us");
        metric(
            "ecs.tick_us_max",
            tick_lat_us
                .iter()
                .copied()
                .fold(f64::NEG_INFINITY, f64::max)
                .max(0.0),
            "us",
        );
    }

    let persist = world.resource::<PersistTelemetry>().clone();
    metric("persist.enabled", if persist.enabled { 1.0 } else { 0.0 }, "bool");
    metric(
        "persist.write_behind_enabled",
        if persist.write_behind_enabled { 1.0 } else { 0.0 },
        "bool",
    );
    metric("persist.interval_ticks", persist.interval_ticks as f64, "ticks");
    metric("persist.ticks_observed", persist.ticks_observed as f64, "ticks");
    metric("persist.skipped_ticks", persist.skipped_ticks as f64, "ticks");
    metric("persist.flush_attempts", persist.flush_attempts as f64, "count");
    metric("persist.flush_success", persist.flush_success as f64, "count");
    metric("persist.flush_failures", persist.flush_failures as f64, "count");
    metric("persist.batch_size_last", persist.batch_size_last as f64, "agents");
    metric("persist.batch_size_avg", persist.avg_batch_size(), "agents");
    metric("persist.batch_size_max", persist.batch_size_max as f64, "agents");
    metric(
        "persist.flush_latency_us_avg",
        persist.avg_flush_latency_us(),
        "us",
    );
    metric("persist.flush_latency_us_max", persist.flush_latency_us_max, "us");
    metric(
        "persist.queue_depth_current",
        persist.queue_depth_current as f64,
        "count",
    );
    metric(
        "persist.queue_depth_max",
        persist.queue_depth_max as f64,
        "count",
    );
    metric("persist.drop_count", persist.drop_count as f64, "count");
    metric("persist.coalesce_count", persist.coalesce_count as f64, "count");

    // Release world/resources first so redb file lock is dropped.
    drop(schedule);
    drop(world);

    // Reopen store and verify persisted state digest.
    let verify = StateStore::open(&db_path)?;
    let (digest, state_count, min_tick, max_tick, tick_parse_failures) = compute_state_digest(&verify)?;
    result("state_hash", &digest);
    result("state_count", &state_count.to_string());
    result("state_tick_min", &min_tick.to_string());
    result("state_tick_max", &max_tick.to_string());
    result("state_tick_parse_failures", &tick_parse_failures.to_string());
    result("persist_every", &persist_every.to_string());

    Ok(())
}

fn validate() -> anyhow::Result<()> {
    let db_path = env_required("DB_PATH")?;
    let min_agents = env_usize("VALIDATE_MIN_AGENTS", 1);
    let strict = env_bool("VALIDATE_STRICT", true);

    let store = StateStore::open(&db_path)?;
    let mut ids = store.list_agents()?;
    ids.sort_by_key(|id| id.0);

    let mut missing = 0usize;
    let mut invalid = 0usize;
    for id in &ids {
        match store.get_agent_state(*id)? {
            Some(data) => {
                if strict {
                    if std::str::from_utf8(&data).is_err() {
                        invalid += 1;
                        continue;
                    }
                    if extract_tick(&data).is_none() {
                        invalid += 1;
                    }
                }
            }
            None => {
                missing += 1;
            }
        }
    }

    let (digest, state_count, min_tick, max_tick, tick_parse_failures) =
        compute_state_digest(&store)?;
    result("state_hash", &digest);
    result("state_count", &state_count.to_string());
    result("state_tick_min", &min_tick.to_string());
    result("state_tick_max", &max_tick.to_string());
    result("state_tick_parse_failures", &tick_parse_failures.to_string());
    result("missing_rows", &missing.to_string());
    result("invalid_rows", &invalid.to_string());
    result("min_agents_required", &min_agents.to_string());

    let pass = state_count >= min_agents && missing == 0 && (!strict || invalid == 0);
    result("validate_pass", if pass { "1" } else { "0" });

    if !pass {
        bail!(
            "validation failed: state_count={}, missing={}, invalid={}, min_agents={}",
            state_count,
            missing,
            invalid,
            min_agents
        );
    }

    Ok(())
}

fn main() -> anyhow::Result<()> {
    let mode = std::env::var("MODE").unwrap_or_else(|_| "simulate".to_string());
    match mode.as_str() {
        "simulate" => simulate(),
        "validate" => validate(),
        other => bail!("unsupported MODE={other}; expected simulate|validate"),
    }
}
