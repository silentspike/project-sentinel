//! Criterion-Benchmarks fuer sentinel-wasm.
//!
//! Performance-Budgets (aus Issue #19):
//! - tool_cold_start    < 50ms
//! - tool_warm_start    < 5ms
//! - tool_execution_p50 < 10ms
//! - tool_execution_p99 < 100ms
//! - sandbox_overhead   < 2ms
//! - registry_lookup    < 1ms
//!
//! WICHTIG: Diese Benchmarks auf der Deploy-VM (192.0.2.240)
//! oder lokal ausfuehren — NICHT auf dem Build-Server (cargo remote)!

use criterion::{criterion_group, criterion_main, Criterion};
use sentinel_wasm::{ExecutionContext, SandboxConfig, ToolDefinition, ToolRuntime, ToolType};
use std::hint::black_box;

fn make_tool(name: &str, tool_type: ToolType) -> ToolDefinition {
    ToolDefinition {
        name: name.to_string(),
        description: format!("Bench tool {name}"),
        wasm_path: None,
        tool_type,
        required_capabilities: Vec::new(),
    }
}

fn bench_ctx(sandbox: SandboxConfig) -> ExecutionContext {
    ExecutionContext {
        agent_id: "AGENT-01".to_string(),
        agent_capabilities: vec!["file_read".to_string(), "file_write".to_string()],
        sandbox,
        correlation_id: "bench-corr".to_string(),
        tick: 1,
        #[cfg(feature = "wasm")]
        agent_snapshot: None,
        #[cfg(feature = "wasm")]
        rooms: None,
    }
}

/// Benchmark: Registry-Lookup (Budget: < 1ms)
fn bench_registry_lookup(c: &mut Criterion) {
    let mut runtime = ToolRuntime::new();
    for i in 0..100 {
        runtime
            .register_tool(ToolDefinition {
                name: format!("tool_{i}"),
                description: format!("Tool {i}"),
                wasm_path: None,
                tool_type: ToolType::FileRead,
                required_capabilities: vec!["file_read".to_string()],
            })
            .unwrap();
    }

    c.bench_function("wasm.registry_lookup_time", |b| {
        b.iter(|| {
            let result = runtime.get_tool(black_box("tool_50"));
            black_box(result)
        })
    });
}

/// Benchmark: Sandbox-Overhead (Budget: < 2ms)
/// Misst die Zeit fuer SandboxConfig-Erstellung und is_path_allowed().
fn bench_sandbox_overhead(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("bench.txt");
    std::fs::write(&file, "bench data").unwrap();

    c.bench_function("wasm.sandbox_overhead", |b| {
        b.iter(|| {
            let sandbox = SandboxConfig::with_paths(vec![black_box(dir.path().to_path_buf())]);
            let allowed = sandbox.is_path_allowed(black_box(&file));
            black_box(allowed)
        })
    });
}

/// Benchmark: FileRead Execution (Budget: p50 < 10ms)
fn bench_file_read_execution(c: &mut Criterion) {
    let mut runtime = ToolRuntime::new();
    runtime
        .register_tool(make_tool("file_read", ToolType::FileRead))
        .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("bench_read.txt");
    std::fs::write(&file, "x".repeat(1024)).unwrap(); // 1KB File

    let sandbox = SandboxConfig::with_paths(vec![dir.path().to_path_buf()]);
    let ctx = bench_ctx(sandbox);
    let path_str = file.to_str().unwrap().to_string();

    c.bench_function("wasm.tool_execution_p50", |b| {
        b.iter(|| {
            let result = runtime.execute(black_box("file_read"), black_box(&path_str), &ctx);
            black_box(result)
        })
    });

    // Separate bench fuer p99 — gleicher Code, Criterion trackt Percentile
    c.bench_function("wasm.tool_execution_p99", |b| {
        b.iter(|| {
            let result =
                runtime.execute(black_box("file_read"), black_box(path_str.as_str()), &ctx);
            black_box(result)
        })
    });
}

/// Benchmark: ToolRuntime::new() Cold Start (Budget: < 50ms)
fn bench_cold_start(c: &mut Criterion) {
    c.bench_function("wasm.tool_cold_start", |b| {
        b.iter(|| {
            let runtime = ToolRuntime::new();
            black_box(runtime)
        })
    });
}

/// Benchmark: Warm Start — execute() auf bereits registriertem Tool (Budget: < 5ms)
fn bench_warm_start(c: &mut Criterion) {
    let mut runtime = ToolRuntime::new();
    runtime
        .register_tool(make_tool("file_read", ToolType::FileRead))
        .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("warm.txt");
    std::fs::write(&file, "warm data").unwrap();
    let sandbox = SandboxConfig::with_paths(vec![dir.path().to_path_buf()]);
    let ctx = bench_ctx(sandbox);
    let path_str = file.to_str().unwrap().to_string();

    // Erster Aufruf (Warm-Up)
    let _ = runtime.execute("file_read", &path_str, &ctx);

    c.bench_function("wasm.tool_warm_start", |b| {
        b.iter(|| {
            let result =
                runtime.execute(black_box("file_read"), black_box(path_str.as_str()), &ctx);
            black_box(result)
        })
    });
}

/// Benchmark: Capability-Check Overhead
fn bench_capability_check(c: &mut Criterion) {
    let tool = ToolDefinition {
        name: "cap_tool".to_string(),
        description: "Cap bench".to_string(),
        wasm_path: None,
        tool_type: ToolType::FileRead,
        required_capabilities: vec![
            "file_read".to_string(),
            "file_write".to_string(),
            "admin".to_string(),
        ],
    };
    let caps = vec![
        "file_read".to_string(),
        "file_write".to_string(),
        "admin".to_string(),
        "network".to_string(),
    ];

    c.bench_function("wasm.capability_check", |b| {
        b.iter(|| {
            let result = sentinel_wasm::registry::can_execute(black_box(&caps), black_box(&tool));
            black_box(result)
        })
    });
}

// ---- Component Model Benchmarks (nur mit wasm-Feature) ----

#[cfg(feature = "wasm")]
use sentinel_wasm::{AgentSnapshot, PluginConfig};

#[cfg(feature = "wasm")]
fn echo_fixture() -> std::path::PathBuf {
    let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("tests/fixtures/echo-plugin.wasm");
    path
}

/// Cold Start: PluginHost::new() — Engine + Linker + WASI Registration.
#[cfg(feature = "wasm")]
fn bench_component_host_cold_start(c: &mut Criterion) {
    c.bench_function("wasm.component_host_cold_start", |b| {
        b.iter(|| {
            let host = sentinel_wasm::PluginHost::new();
            black_box(host)
        })
    });
}

/// Cold Start: ToolRuntime::new() mit PluginHost.
#[cfg(feature = "wasm")]
fn bench_component_runtime_new(c: &mut Criterion) {
    c.bench_function("wasm.component_runtime_new", |b| {
        b.iter(|| {
            let runtime = ToolRuntime::new();
            black_box(runtime)
        })
    });
}

/// Component Load: Component::from_file() + Cache (einmalig pro .wasm).
#[cfg(feature = "wasm")]
fn bench_component_load(c: &mut Criterion) {
    c.bench_function("wasm.component_load", |b| {
        b.iter(|| {
            let mut host = sentinel_wasm::PluginHost::new().unwrap();
            host.load(PluginConfig {
                wasm_path: echo_fixture(),
                ..Default::default()
            })
            .unwrap();
            black_box(host)
        })
    });
}

/// Warm Execute: Plugin ist geladen, neuer Store pro Call.
/// Das ist der Hot-Path im Betrieb (Budget: < 10ms p50).
#[cfg(feature = "wasm")]
fn bench_component_warm_execute(c: &mut Criterion) {
    let mut host = sentinel_wasm::PluginHost::new().unwrap();
    host.load(PluginConfig {
        wasm_path: echo_fixture(),
        ..Default::default()
    })
    .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let agent = AgentSnapshot {
        agent_id: "AGENT-01".to_string(),
        name: "Bench Agent".to_string(),
        ..Default::default()
    };

    c.bench_function("wasm.component_warm_execute", |b| {
        b.iter(|| {
            let result = host.execute(
                &echo_fixture(),
                black_box("benchmark input"),
                agent.clone(),
                std::collections::HashMap::new(),
                black_box(42),
                dir.path().to_path_buf(),
            );
            black_box(result)
        })
    });
}

/// Host-Function Roundtrip: Plugin ruft get_agent_info() + get_tick() + log() auf.
/// Misst den Overhead der Host-Funktion-Aufrufe vom Plugin aus.
#[cfg(feature = "wasm")]
fn bench_component_host_roundtrip(c: &mut Criterion) {
    let mut host = sentinel_wasm::PluginHost::new().unwrap();
    host.load(PluginConfig {
        wasm_path: echo_fixture(),
        ..Default::default()
    })
    .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let agent = AgentSnapshot {
        agent_id: "AGENT-42".to_string(),
        name: "Roundtrip Agent".to_string(),
        role: "Developer".to_string(),
        hunger: 0.5,
        energy: 0.6,
        stress: 0.3,
        social_need: 0.4,
        caffeine: 0.7,
        bladder: 0.2,
        room_id: "buero-dev-1".to_string(),
    };
    let mut rooms = std::collections::HashMap::new();
    rooms.insert(
        "buero-dev-1".to_string(),
        sentinel_wasm::RoomSnapshot {
            room_id: "buero-dev-1".to_string(),
            name: "Dev Buero 1".to_string(),
            floor: 1,
            temperature: 22.0,
            noise_db: 40.0,
            occupant_count: 3,
        },
    );

    c.bench_function("wasm.component_host_roundtrip", |b| {
        b.iter(|| {
            let result = host.execute(
                &echo_fixture(),
                black_box("roundtrip"),
                agent.clone(),
                rooms.clone(),
                black_box(9999),
                dir.path().to_path_buf(),
            );
            black_box(result)
        })
    });
}

/// ToolRuntime E2E: Registrierung + Capability-Check + WASM-Execute.
/// Misst den vollen Pfad wie er im Daemon ausgefuehrt wird.
#[cfg(feature = "wasm")]
fn bench_runtime_e2e_execute(c: &mut Criterion) {
    let mut runtime = ToolRuntime::new();
    runtime
        .plugin_host_mut()
        .load(PluginConfig {
            wasm_path: echo_fixture(),
            ..Default::default()
        })
        .unwrap();
    runtime
        .register_tool(ToolDefinition {
            name: "echo".to_string(),
            description: "Echo WASM tool".to_string(),
            wasm_path: Some(echo_fixture().to_str().unwrap().to_string()),
            tool_type: ToolType::Wasm,
            required_capabilities: Vec::new(),
        })
        .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let ctx = ExecutionContext {
        agent_id: "AGENT-01".to_string(),
        agent_capabilities: vec!["file_read".to_string()],
        sandbox: SandboxConfig::with_paths(vec![dir.path().to_path_buf()]),
        correlation_id: "bench".to_string(),
        tick: 100,
        agent_snapshot: Some(AgentSnapshot::default()),
        rooms: Some(std::collections::HashMap::new()),
    };

    c.bench_function("wasm.runtime_e2e_execute", |b| {
        b.iter(|| {
            let result = runtime.execute(black_box("echo"), black_box("bench input"), &ctx);
            black_box(result)
        })
    });
}

/// query_meta: Tool-Name + Tool-Description abfragen.
#[cfg(feature = "wasm")]
fn bench_component_query_meta(c: &mut Criterion) {
    let mut host = sentinel_wasm::PluginHost::new().unwrap();
    host.load(PluginConfig {
        wasm_path: echo_fixture(),
        ..Default::default()
    })
    .unwrap();

    let dir = tempfile::tempdir().unwrap();

    c.bench_function("wasm.component_query_meta", |b| {
        b.iter(|| {
            let meta = host.query_meta(&echo_fixture(), dir.path().to_path_buf());
            black_box(meta)
        })
    });
}

// Benchmark-Gruppen
criterion_group!(
    native_benches,
    bench_registry_lookup,
    bench_sandbox_overhead,
    bench_file_read_execution,
    bench_cold_start,
    bench_warm_start,
    bench_capability_check,
);

#[cfg(feature = "wasm")]
criterion_group!(
    wasm_benches,
    bench_component_host_cold_start,
    bench_component_runtime_new,
    bench_component_load,
    bench_component_warm_execute,
    bench_component_host_roundtrip,
    bench_runtime_e2e_execute,
    bench_component_query_meta,
);

#[cfg(not(feature = "wasm"))]
criterion_main!(native_benches);

#[cfg(feature = "wasm")]
criterion_main!(native_benches, wasm_benches);
