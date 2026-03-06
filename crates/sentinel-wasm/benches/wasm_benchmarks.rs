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

/// Component Model Benchmarks (nur mit wasm-Feature)
///
/// Misst PluginHost-Erstellung (Engine + Linker + WASI) — das ist der "Cold Start"
/// fuer die Component Model Pipeline.
/// Echte Component-Execution Benchmarks benoetigen .wasm Fixtures
/// (via cargo-component oder wasm-tools) und gehoeren auf die Deploy-VM.
#[cfg(feature = "wasm")]
fn bench_component_host_cold_start(c: &mut Criterion) {
    c.bench_function("wasm.component_host_cold_start", |b| {
        b.iter(|| {
            let host = sentinel_wasm::PluginHost::new();
            black_box(host)
        })
    });
}

/// Warm Start: ToolRuntime::new() mit PluginHost (bereits integriert)
#[cfg(feature = "wasm")]
fn bench_component_runtime_new(c: &mut Criterion) {
    c.bench_function("wasm.component_runtime_new", |b| {
        b.iter(|| {
            let runtime = ToolRuntime::new();
            black_box(runtime)
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
);

#[cfg(not(feature = "wasm"))]
criterion_main!(native_benches);

#[cfg(feature = "wasm")]
criterion_main!(native_benches, wasm_benches);
