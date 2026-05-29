# sentinel-wasm

## Purpose

`sentinel-wasm` is the tool runtime for agent capabilities. It supports native handlers plus WASM Component Model plugins under the DEV-006 default runtime contract: WASM/WASI on Wasmtime by default, native only through explicit escape hatches.

## Interfaces

- `ToolRuntime`, `ToolDefinition`, `ToolType`, `ExecutionContext`, and `ToolResult` are the core execution API.
- `SandboxConfig` constrains filesystem and runtime access.
- `registry` and `runner` resolve and execute tools.
- `host` and `plugin` are compiled with the `wasm` feature for Wasmtime Component Model plugins.

## Dependencies

- `sentinel-common`, `anyhow`, `serde`, `serde_json`, and `tracing`.
- Optional `wasmtime 44.0.2` and `wasmtime-wasi 44.0.2` under the `wasm` feature.

## Verify

```bash
cargo remote -c -- test -p sentinel-wasm
cargo remote -c -- test -p sentinel-wasm --features wasm
```

Fixture crates under `tests/fixtures/` are test inputs, not product components.
