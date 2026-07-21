# sentinel-wasm

## Purpose

`sentinel-wasm` is the tool runtime for agent capabilities. It supports native handlers plus WASM Component Model plugins under the DEV-007 plural NanoRuntime contract: WASM/WASI on Wasmtime is one explicit runtime key, not a global default.

## Interfaces

- `ToolRuntime`, `ToolDefinition`, `ToolType`, `ExecutionContext`, and `ToolResult` are the core execution API.
- `SandboxConfig` constrains filesystem and runtime access.
- `registry` and `runner` resolve and execute tools.
- `host` and `plugin` are compiled with the `wasm` feature for Wasmtime Component Model plugins.
- `WasmtimeNanoRuntime` implements the shared `NanoRuntime` contract for the
  `wasm-wasmtime` key. Its `snapshot` is declarative ToolRuntime/input state
  plus ECS-side state for deterministic re-execute. It is not a bitwise
  Wasmtime `Store` dump because current plugin calls create fresh stores. Its
  idempotent `stop` removes only the addressed workload state and releases a
  tool definition or compiled component after its final owning workload stops.

## Dependencies

- `sentinel-common`, `anyhow`, `serde`, `serde_json`, and `tracing`.
- Optional `wasmtime 45.0.3` and `wasmtime-wasi 45.0.3` under the `wasm` feature.

## Verify

```bash
cargo remote -c -- test -p sentinel-wasm
cargo remote -c -- test -p sentinel-wasm --features wasm
cargo remote -c -- test -p sentinel-wasm --features wasm --test nano_runtime_conformance
```

Fixture crates under `tests/fixtures/` are test inputs, not product components.
