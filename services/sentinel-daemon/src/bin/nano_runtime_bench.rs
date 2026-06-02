use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use sentinel_common::nano_runtime::{
    NanoExecRequest, NanoRuntime, NanoRuntimeRegistry, NanoWorkloadSpec, RUNTIME_BWRAP_LANDLOCK,
    RUNTIME_ECS_NATIVE, RUNTIME_WASM_WASMTIME,
};
use sentinel_common::AgentId;
use sentinel_runtime::EcsNativeRuntime;
use sentinel_sandbox::BwrapNanoRuntime;

#[cfg(feature = "wasm")]
use sentinel_wasm::{wasm_conformance_metadata, WasmtimeNanoRuntime};

#[cfg(not(feature = "wasm"))]
compile_error!("nano_runtime_bench requires the sentinel-daemon wasm feature");

const ECHO_WASM: &[u8] =
    include_bytes!("../../../../crates/sentinel-wasm/tests/fixtures/echo-plugin.wasm");

#[derive(Default)]
struct Samples {
    values: Vec<u128>,
}

impl Samples {
    fn push(&mut self, value: u128) {
        self.values.push(value);
    }

    fn count(&self) -> usize {
        self.values.len()
    }

    fn min(&self) -> u128 {
        *self.values.iter().min().unwrap_or(&0)
    }

    fn max(&self) -> u128 {
        *self.values.iter().max().unwrap_or(&0)
    }

    fn mean(&self) -> f64 {
        if self.values.is_empty() {
            return 0.0;
        }
        let sum: u128 = self.values.iter().sum();
        sum as f64 / self.values.len() as f64
    }

    fn median(&self) -> u128 {
        if self.values.is_empty() {
            return 0;
        }
        let mut values = self.values.clone();
        values.sort_unstable();
        values[values.len() / 2]
    }
}

#[derive(Default)]
struct RuntimeBench {
    spawn: Samples,
    exec: Samples,
    snapshot: Samples,
    restore: Samples,
    roundtrip_ok: usize,
}

fn timed<T>(operation: impl FnOnce() -> Result<T>) -> Result<(T, u128)> {
    let started = Instant::now();
    let result = operation()?;
    Ok((result, started.elapsed().as_micros()))
}

fn workload(
    id: &str,
    runtime_key: &str,
    agent_id: u16,
    command: Vec<String>,
    metadata: BTreeMap<String, String>,
) -> NanoWorkloadSpec {
    NanoWorkloadSpec {
        workload_id: id.to_string(),
        runtime_key: Some(runtime_key.to_string()),
        agent_id: Some(AgentId(agent_id)),
        agent_name: format!("nano-bench-agent-{agent_id}"),
        role: "Nano Runtime Bench".to_string(),
        room_id: "empfang".to_string(),
        shift_set: 1,
        command,
        capabilities: Vec::new(),
        metadata,
        ecs_snapshot: None,
    }
}

fn write_wasm_fixture() -> Result<PathBuf> {
    let path = std::env::temp_dir().join(format!(
        "sentinel-nano-runtime-bench-{}.wasm",
        std::process::id()
    ));
    fs::write(&path, ECHO_WASM)
        .with_context(|| format!("write WASM fixture to {}", path.display()))?;
    Ok(path)
}

fn bench_ecs(iterations: usize) -> Result<RuntimeBench> {
    let mut bench = RuntimeBench::default();
    for i in 0..iterations {
        let mut runtime = EcsNativeRuntime::new(8);
        let workload = workload(
            &format!("ecs-native-bench-{i}"),
            RUNTIME_ECS_NATIVE,
            i as u16 + 1,
            Vec::new(),
            BTreeMap::new(),
        );

        let (handle, elapsed) = timed(|| runtime.spawn(workload))?;
        bench.spawn.push(elapsed);

        let (exec, elapsed) = timed(|| {
            runtime.exec(
                &handle,
                NanoExecRequest {
                    operation: "health".to_string(),
                    input: String::new(),
                },
            )
        })?;
        if !exec.success {
            return Err(anyhow!("ecs-native exec returned unsuccessful result"));
        }
        bench.exec.push(elapsed);

        let (snapshot, elapsed) = timed(|| runtime.snapshot(&handle))?;
        let expected_payload = snapshot.payload.clone();
        bench.snapshot.push(elapsed);

        let (restored, elapsed) = timed(|| runtime.restore(snapshot))?;
        bench.restore.push(elapsed);

        let after = runtime.snapshot(&restored)?;
        if after.payload == expected_payload {
            bench.roundtrip_ok += 1;
        }
    }
    Ok(bench)
}

fn bench_wasm(iterations: usize, wasm_path: PathBuf) -> Result<RuntimeBench> {
    let mut bench = RuntimeBench::default();
    for i in 0..iterations {
        let mut runtime = WasmtimeNanoRuntime::new();
        let metadata = wasm_conformance_metadata(wasm_path.clone(), "echo");
        let workload = workload(
            &format!("wasm-wasmtime-bench-{i}"),
            RUNTIME_WASM_WASMTIME,
            i as u16 + 100,
            Vec::new(),
            metadata,
        );

        let (handle, elapsed) = timed(|| runtime.spawn(workload))?;
        bench.spawn.push(elapsed);

        let (exec, elapsed) = timed(|| {
            runtime.exec(
                &handle,
                NanoExecRequest {
                    operation: "echo".to_string(),
                    input: format!("nano-runtime-bench-{i}"),
                },
            )
        })?;
        if !exec.success {
            return Err(anyhow!("wasm-wasmtime exec returned unsuccessful result"));
        }
        bench.exec.push(elapsed);

        let (snapshot, elapsed) = timed(|| runtime.snapshot(&handle))?;
        let expected_payload = snapshot.payload.clone();
        bench.snapshot.push(elapsed);

        let (restored, elapsed) = timed(|| runtime.restore(snapshot))?;
        bench.restore.push(elapsed);

        let after = runtime.snapshot(&restored)?;
        if after.payload == expected_payload {
            bench.roundtrip_ok += 1;
        }
    }
    Ok(bench)
}

fn bench_bwrap(iterations: usize) -> Result<RuntimeBench> {
    let mut bench = RuntimeBench::default();
    for i in 0..iterations {
        let mut runtime = BwrapNanoRuntime::detect();
        let workload = workload(
            &format!("bwrap-landlock-bench-{i}"),
            RUNTIME_BWRAP_LANDLOCK,
            i as u16 + 200,
            vec!["/usr/bin/sleep".to_string(), "30".to_string()],
            BTreeMap::new(),
        );

        let (handle, elapsed) = timed(|| runtime.spawn(workload))?;
        bench.spawn.push(elapsed);

        let (exec, elapsed) = timed(|| {
            runtime.exec(
                &handle,
                NanoExecRequest {
                    operation: "health".to_string(),
                    input: String::new(),
                },
            )
        })?;
        if !exec.success {
            return Err(anyhow!("bwrap-landlock exec returned unsuccessful result"));
        }
        bench.exec.push(elapsed);

        let (snapshot, elapsed) = timed(|| runtime.snapshot(&handle))?;
        let expected_payload = snapshot.payload.clone();
        bench.snapshot.push(elapsed);

        let (restored, elapsed) = timed(|| runtime.restore(snapshot))?;
        bench.restore.push(elapsed);

        let after = runtime.snapshot(&restored)?;
        if after.payload == expected_payload {
            bench.roundtrip_ok += 1;
        }
    }
    Ok(bench)
}

fn bench_registry(iterations: usize) -> Result<Samples> {
    let mut registry = NanoRuntimeRegistry::new(Some(RUNTIME_ECS_NATIVE.to_string()));
    registry.register(EcsNativeRuntime::new(8))?;
    registry.register(WasmtimeNanoRuntime::new())?;
    registry.register(BwrapNanoRuntime::detect())?;

    let workloads = [
        workload(
            "registry-ecs",
            RUNTIME_ECS_NATIVE,
            301,
            Vec::new(),
            BTreeMap::new(),
        ),
        workload(
            "registry-wasm",
            RUNTIME_WASM_WASMTIME,
            302,
            Vec::new(),
            BTreeMap::new(),
        ),
        workload(
            "registry-bwrap",
            RUNTIME_BWRAP_LANDLOCK,
            303,
            Vec::new(),
            BTreeMap::new(),
        ),
    ];

    let mut samples = Samples::default();
    for i in 0..iterations {
        let workload = &workloads[i % workloads.len()];
        let (_key, elapsed) = timed(|| registry.select_key(workload))?;
        samples.push(elapsed);
    }
    Ok(samples)
}

fn print_samples(runtime: &str, operation: &str, samples: &Samples) {
    println!(
        "| {runtime} | {operation} | {} | {:.2} | {} | {} | {} |",
        samples.count(),
        samples.mean(),
        samples.median(),
        samples.min(),
        samples.max()
    );
}

fn print_runtime(runtime: &str, bench: &RuntimeBench, iterations: usize) {
    print_samples(runtime, "spawn", &bench.spawn);
    print_samples(runtime, "exec", &bench.exec);
    print_samples(runtime, "snapshot", &bench.snapshot);
    print_samples(runtime, "restore", &bench.restore);
    println!(
        "roundtrip {runtime}: {}/{} restore(snapshot(x)) payload checks passed",
        bench.roundtrip_ok, iterations
    );
}

fn cpu_model() -> String {
    fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|cpuinfo| {
            cpuinfo
                .lines()
                .find_map(|line| line.strip_prefix("model name\t: "))
                .map(str::to_string)
        })
        .unwrap_or_else(|| "unknown".to_string())
}

fn parse_iterations() -> usize {
    std::env::args()
        .skip(1)
        .find_map(|arg| arg.parse::<usize>().ok())
        .filter(|iterations| *iterations > 0)
        .unwrap_or(10)
}

fn main() -> Result<()> {
    let iterations = parse_iterations();
    let registry_iterations = iterations * 1000;
    let wasm_path = write_wasm_fixture()?;

    println!("NanoRuntime benchmark");
    println!("hardware: {}", cpu_model());
    println!("iterations: {iterations}");
    println!("registry_iterations: {registry_iterations}");
    println!("gateway: not used");
    println!("benchmark_note: deployment VM evidence only; no TOGAF absolute latency gate");
    println!();

    let ecs = bench_ecs(iterations).context("ecs-native benchmark failed")?;
    let wasm =
        bench_wasm(iterations, wasm_path.clone()).context("wasm-wasmtime benchmark failed")?;
    let bwrap = bench_bwrap(iterations).context("bwrap-landlock benchmark failed")?;
    let registry = bench_registry(registry_iterations).context("registry benchmark failed")?;

    println!("| runtime | operation | count | mean_us | median_us | min_us | max_us |");
    println!("|---|---:|---:|---:|---:|---:|---:|");
    print_runtime(RUNTIME_ECS_NATIVE, &ecs, iterations);
    print_runtime(RUNTIME_WASM_WASMTIME, &wasm, iterations);
    print_runtime(RUNTIME_BWRAP_LANDLOCK, &bwrap, iterations);
    print_samples("registry", "select_key", &registry);
    println!(
        "snapshot_semantics wasm-wasmtime: input+ECS re-execute state, no Wasmtime Store dump"
    );
    println!("snapshot_semantics bwrap-landlock: config+agent-home filesystem state, no RAM/CRIU checkpoint");
    println!("snapshot_semantics microvm: out of scope for #407-#411");

    let _ = fs::remove_file(wasm_path);
    Ok(())
}
