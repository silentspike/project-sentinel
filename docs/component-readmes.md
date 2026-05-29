# Component READMEs

Issue #383 baseline. Current product component scope:

- Rust component directories: 21
- Go module/service directories: 4
- Excluded: WASM test fixture crates under `crates/sentinel-wasm/tests/fixtures/`

Each component README must include purpose, interfaces, dependencies, and verify guidance.

## Rust Crates

| Component | README |
| --- | --- |
| `crates/sentinel-bio` | [README](../crates/sentinel-bio/README.md) |
| `crates/sentinel-common` | [README](../crates/sentinel-common/README.md) |
| `crates/sentinel-ebpf` | [README](../crates/sentinel-ebpf/README.md) |
| `crates/sentinel-ebpf-probes` | [README](../crates/sentinel-ebpf-probes/README.md) |
| `crates/sentinel-ecs` | [README](../crates/sentinel-ecs/README.md) |
| `crates/sentinel-fs` | [README](../crates/sentinel-fs/README.md) |
| `crates/sentinel-hippocampus` | [README](../crates/sentinel-hippocampus/README.md) |
| `crates/sentinel-inference` | [README](../crates/sentinel-inference/README.md) |
| `crates/sentinel-limbo` | [README](../crates/sentinel-limbo/README.md) |
| `crates/sentinel-physics` | [README](../crates/sentinel-physics/README.md) |
| `crates/sentinel-projection` | [README](../crates/sentinel-projection/README.md) |
| `crates/sentinel-redb` | [README](../crates/sentinel-redb/README.md) |
| `crates/sentinel-runtime` | [README](../crates/sentinel-runtime/README.md) |
| `crates/sentinel-sandbox` | [README](../crates/sentinel-sandbox/README.md) |
| `crates/sentinel-telemetry` | [README](../crates/sentinel-telemetry/README.md) |
| `crates/sentinel-wasm` | [README](../crates/sentinel-wasm/README.md) |
| `crates/sentinel-zenoh` | [README](../crates/sentinel-zenoh/README.md) |

## Rust Services

| Component | README |
| --- | --- |
| `services/agent-runtime` | [README](../services/agent-runtime/README.md) |
| `services/sentinel-daemon` | [README](../services/sentinel-daemon/README.md) |
| `services/sentinel-nightrun` | [README](../services/sentinel-nightrun/README.md) |
| `services/sentinel-projection` | [README](../services/sentinel-projection/README.md) |

## Go Modules And Services

| Component | README |
| --- | --- |
| `cmd/cortex-gateway` | [README](../cmd/cortex-gateway/README.md) |
| `pkg/sentinel-go` | [README](../pkg/sentinel-go/README.md) |
| `services/sentinel-judge` | [README](../services/sentinel-judge/README.md) |
| `services/sentinel-nats-bridge` | [README](../services/sentinel-nats-bridge/README.md) |

## Verify

```bash
scripts/check-component-readmes.sh
```
