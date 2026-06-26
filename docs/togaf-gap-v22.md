# TOGAF v22.1 — Per-Cluster Implementation Status

This document tracks how much of each TOGAF v22.1 cluster is actually
implemented in the current codebase. The architecture guide
([docs/architecture/togaf-architecture-guide.html](architecture/togaf-architecture-guide.html))
is the authoritative spec; this gap analysis is the implementation report.

**Legend**

| Mark | Meaning |
|------|---------|
| ✅ | Implemented and exercised in CI / on the runtime VM |
| 🟡 | Partially implemented — main path works, edges deferred |
| ⏳ | Designed, not yet implemented |
| —  | Out of scope for the current public release |

The numbers reflect the state at the v0.1.0-alpha boundary.

---

## Cluster 01 — Strategischer Kontext

| Item                          | Status | Evidence |
|-------------------------------|--------|----------|
| 60 LLM-persona agents defined | ✅ | `config/agents/AGENT-01.toml` … `AGENT-60.toml` |
| Gaia company bootstrap (#414/#415/#416) | ✅ | `sentinel-gaia` generates and validates `gaia-spec.toml`, Agent TOMLs, `rooms.toml`, `daemon.toml`, and `nightrun.toml`; AgentId bounds are configurable while preserving the default 60-agent cohort |
| 4-shift assignment (1+2+3+0)  | ✅ | `shift_set` field in each agent TOML; counts 17/17/17/9 |
| PixelPerfekt narrative        | ✅ | `config/company-context.md`, [docs/glossary.md](glossary.md) |
| 5 background services         | ✅ | sentinel-daemon, cortex-gateway, sentinel-judge, sentinel-nightrun, sentinel-nats-bridge |

## Cluster 02 — Agent-Ontologie

| Item                          | Status | Evidence |
|-------------------------------|--------|----------|
| Big-Five personality model    | ✅ | `crates/sentinel-common/src/personality.rs` |
| Bio-engine (6 differential eqs) | ✅ | `crates/sentinel-bio/src/` |
| Mood (valence + arousal)      | ✅ | `crates/sentinel-bio/src/mood.rs` |
| Memory (episodic + semantic)  | ✅ | `crates/sentinel-hippocampus/`; calibrated NMDA selection profile finalized in #382 (`threshold=0.25`, max 10), Deploy-VM Night-Run consolidated 2/3 controlled episodes with deterministic replay evidence in `test-382-verification.md` |
| Action ontology               | ✅ | `crates/sentinel-common/src/action.rs` |

## Cluster 03 — Infrastruktur

| Item                          | Status | Evidence |
|-------------------------------|--------|----------|
| ECS world (`bevy_ecs`)        | ✅ | `crates/sentinel-ecs/` |
| Event store (Limbo / SQLite)  | ✅ | `crates/sentinel-limbo/`; idempotent via `operation_id` |
| State store (`redb`)          | ✅ | `crates/sentinel-redb/` |
| Pub/Sub (Zenoh, Rust)         | ✅ | `crates/sentinel-zenoh/`; SHM local <10µs |
| Pub/Sub (NATS JetStream, Go)  | ✅ | `pkg/sentinel-go/messaging/`, two streams |
| Sandbox (bwrap + Landlock + cgroups + netns) | ✅ | `crates/sentinel-sandbox/`; 9/9 breakout tests pass |
| sentinel-fs CAS-FUSE (#379)  | 🟡 | Deploy-VM daemon namespace shows active `fuse sentinel-fs` at `/opt/sentinel/fs` and agent homes bound through `/opt/sentinel/fs/AGENT-*`; same-VM optimization loop on Intel i7-3930K @ 3.20 GHz (2011) reached 99.22% dedup savings and improved median dedup-hit write p95 from 40,411 us to 189 us, but the strict `<100us` target remains unmet on this VM |
| WASM tool runtime             | ✅ | `crates/sentinel-wasm/` |
| eBPF monitoring (aya-rs)      | ✅ | `crates/sentinel-ebpf/`; #380 verified production kernel mode on Deploy-VM with CAP_BPF fallback smoke and dashboard evidence |
| Security threat model (#390)  | ✅ | [docs/security/threat-model.md](security/threat-model.md) documents compromised-agent, external-attacker, and supply-chain attacker classes, protected assets, mitigations, and prioritized gaps linked to #391/#392/#393 |
| Unsafe audit baseline (#392)  | ✅ | [docs/security/unsafe-audit.md](security/unsafe-audit.md) documents 19 first-party unsafe constructs, SAFETY justifications, `scripts/check-unsafe-baseline.py`, and shift FFI regression evidence |

## Cluster 04 — Cortex Gateway

| Item                          | Status | Evidence |
|-------------------------------|--------|----------|
| 7-step LLM pipeline           | ✅ | `cmd/cortex-gateway/internal/{normalizer,compiler,detection,extraction,capability,proxy}/` |
| Prompt-injection action validation (#391) | 🟡 | Go Gateway loads per-agent `[capabilities].tools`, filters unauthorized extracted actions before response/persistence, and emits `agent_action_rejected` audit events; unit/integration evidence in `test-391-verification.md`, Deploy-VM gateway runtime smoke deferred because gateway remains inactive for token-safe verification |
| Synthesis engine (10 rules)   | ✅ | `cmd/cortex-gateway/internal/synthesis/rules.go` |
| Fourth-wall detection (15 regex + judge) | ✅ | `cmd/cortex-gateway/internal/detection/` |
| Perception injection          | ✅ | `cmd/cortex-gateway/internal/injection/` + 8 system blocks |
| API controlplane              | ✅ | `cmd/cortex-gateway/internal/apicp/` |
| MITM forward (`/v1/messages`) | ✅ | `cmd/cortex-gateway/internal/intercept/` |
| Provider registry             | ✅ | claude-code + anthropic-direct + ollama (config-selectable) |

## Cluster 05 — Tech Stack Reference

| Item                          | Status | Evidence |
|-------------------------------|--------|----------|
| Rust workspace (17 crates + 5 Rust services) | ✅ | `Cargo.toml` workspace members plus component README coverage in [docs/component-readmes.md](component-readmes.md) |
| Go workspace (4 modules)      | ✅ | `go.work` |
| SolidJS console + Rust WebTransport backend | ✅ | `console/`, `services/sentinel-dashboard-backend/` |
| Build server pattern          | ✅ | `cargo remote --` (server IP injected via `.make.local`) |
| 16 GitHub Actions workflows   | ✅ | `.github/workflows/*.yml` |

## Cluster 05b — Telemetrie & Observability

| Item                          | Status | Evidence |
|-------------------------------|--------|----------|
| Prometheus metrics            | ✅ | `crates/sentinel-telemetry/`, gateway `/metrics` |
| Health + readiness endpoints  | ✅ | each service exposes `/healthz`, `/readyz` |
| eBPF probes (write-syscall, stall) | ✅ | `crates/sentinel-ebpf/`; #380 Deploy-VM evidence shows `mode=kernel`, ring drops 0, I/O read/write values, TCP request deltas, and dashboard eBPF cards without `N/A` |
| MARBLE Observatory            | ✅ | `cmd/cortex-gateway/internal/observatory/` |
| Dashboard polling efficiency (#277) | ✅ | Projection-owned `projection_watermarks` table gives one indexed change-detection lookup per active WebSocket poll; Deploy-VM evidence on Intel i7-3930K @ 3.20 GHz (2011), gateway inactive, 3-tab Playwright: no `ERR_INSUFFICIENT_RESOURCES` |
| OTel tracing                  | 🟡 | structured spans in place, OTLP exporter behind feature flag |

## Cluster 06 — Performance & Hardware

| Item                          | Status | Evidence |
|-------------------------------|--------|----------|
| BFS room search benchmark     | ✅ | 2.59 – 7.09 µs (criterion) |
| Encounter-detection benchmark | ✅ | 7.34 – 8.05 µs |
| Bio-tick benchmark            | ✅ | 154 – 171 µs |
| Synthesis-engine latency      | ✅ | 95 – 103 µs end-to-end |
| Tick-loop hot-path (#276)     | ✅ | Deploy-VM relative before/after on Intel i7-3930K @ 3.20 GHz (2011): physics 26.86% faster, perception 26.34% faster, persist e2e batch 52.57% faster, persist write-only batch 86.95% faster, full tick 17.23% faster; absolute TOGAF baselines intentionally not used for this hardware |
| eBPF monitoring overhead (#380) | ✅ | Deploy-VM Intel i7-3930K @ 3.20 GHz (2011): collector avg 1265.83 us per 10 ticks, amortized 0.012658% of tick budget, ring drops 0; vmstat/mpstat/iostat captured in parallel |

## Cluster 07 — Deployment & Tuning

| Item                          | Status | Evidence |
|-------------------------------|--------|----------|
| systemd units (12)            | ✅ | `deploy/systemd/*.service` |
| Release manifest schema       | ✅ | `deploy/release-manifest.schema.json` |
| Demo compose stack            | ✅ | `docker-compose.demo.yml` (7 services), `scripts/demo.sh`, `make demo-binaries` 3-tier (release-fetch / cargo-remote / local cargo); pre-built `linux-x86_64` binaries on every release |
| 4-tier IP / path strategy     | ✅ | `.env.example`, `.make.local.example`, `deploy/systemd/sentinel-env.example`, `Makefile` |

## Cluster 08 — LLM Behavioral Science

| Item                          | Status | Evidence |
|-------------------------------|--------|----------|
| Personality-driven prompting  | ✅ | gateway prompt compiler injects Big-Five vector |
| Drift / quality / fatigue / swap heuristics | ✅ | `pkg/sentinel-go/judge/` |
| LLM-judge for self-recognition pattern | ✅ | `cmd/cortex-gateway/internal/detection/llm_judge.go` (formerly "fourth-wall"; renamed in glossary, anchor preserved) |
| Voice analysis (semantic drift) | ✅ | `services/sentinel-judge/internal/analyzer/` |

## Cluster 09 — Wissenschaftliche Grundlagen

| Item                          | Status | Evidence |
|-------------------------------|--------|----------|
| Reproducible scenario runs    | ✅ | nightrun deterministic replay + SHA-256 hash chain |
| Event-sourcing primitives     | ✅ | `crates/sentinel-limbo/` |
| Snapshot tiered retention     | ✅ | `crates/sentinel-limbo/src/snapshot.rs` |
| Kani formal verification (#393) | ✅ | [docs/security/kani-verification.md](security/kani-verification.md) and `scripts/verify-kani.sh` prove six harnesses on build-server Kani/CBMC |

## Cluster 10 — Software Design Description

| Item                          | Status | Evidence |
|-------------------------------|--------|----------|
| Component-level READMEs       | ✅ | #383: 26 current Rust/Go component READMEs indexed in [docs/component-readmes.md](component-readmes.md); coverage checked by `scripts/check-component-readmes.sh` |
| llms.txt index                | ✅ | `llms.txt` |
| Glossary                      | ✅ | [docs/glossary.md](glossary.md) |
| Governance ↔ code map         | ✅ | [docs/governance.md](governance.md) |

## Cluster 11 — Time Machine

| Item                          | Status | Evidence |
|-------------------------------|--------|----------|
| WorldSnapshot (bincode 2)     | ✅ | `crates/sentinel-limbo/src/snapshot.rs` |
| WorldSnapshot coverage (#486) | 🟡 | redb dump/restore covers 12 tables including `api_patterns`; `RoomPhysicsState` is not serialized in `EcsSnapshot` and restore/projection reconstructs room occupancy from agent positions; ArtifactPlane tables (`FS_OBJECTS`, `FS_MANIFESTS`, `FS_CHUNKS`, and related indexes) live in `sentinel-fs` separately and are not implied by WorldSnapshot coverage. TOGAF Cluster 11 HTML corrections are main-session-only handoff items. |
| Hot-swap restore              | ✅ | `services/sentinel-daemon/src/runtime_health.rs` (snapshot reload path) |
| Deterministic replay          | ✅ | `services/sentinel-nightrun/` |
| Non-blocking evolution LLM (#278) | 🟡 | Async `evolution_task` moves nightrun/shift LLM calls out of the ECS tick loop; Deploy-VM nightrun returned in 0.669 ms and gateway-down shift jobs failed safe, but total shift transition measured ~1.455 s on the i7-3930K VM, above the strict `<1s` AC target due remaining Hippocampus/sandbox work |
| Time-travel debugging UI      | ✅ | Issue #384: dashboard "Zeitreise" view — visual snapshot timeline, point-in-time world-state preview (`GET /api/control/snapshot-state`), hot-swap restore with confirm + live WebSocket refresh; Deploy-VM Playwright evidence in `test-384-verification.md` |

## Cluster 12 — Zielarchitektur / Nano-Container Platform

| Item                          | Status | Evidence |
|-------------------------------|--------|----------|
| Runtime contract decision (#407) | ✅ | DEV-007 in [docs/togaf-deviations-v22.md](togaf-deviations-v22.md) supersedes DEV-006: Nano-Container uses a runtime-agnostic CRI contract, four runtime families, and no global default runtime |
| NanoRuntime trait + ECS reference (#408) | ✅ | `crates/sentinel-common/src/nano_runtime.rs` defines `spawn`/`exec`/`snapshot`/`restore`/`migrate`/`health`/`isolate`; `crates/sentinel-runtime/src/nano.rs` proves the ECS-native reference through the shared conformance harness |
| WASM/Wasmtime adapter (#409)  | ✅ | `crates/sentinel-wasm/src/nano.rs` implements the same contract with documented re-execute snapshot semantics, not a bitwise Wasmtime Store dump |
| bwrap+Landlock adapter (#410) | ✅ | `crates/sentinel-sandbox/src/nano.rs` implements the same contract with config+filesystem snapshot semantics, not process RAM or CRIU checkpointing |
| Registry and workload selection (#411) | ✅ | `NanoRuntimeRegistry` selects explicit workload runtime keys with only configured fallback policy; `services/sentinel-daemon/tests/nano_runtime_registry.rs` covers `ecs-native`, `wasm-wasmtime`, and `bwrap-landlock` routing |
| microVM adapter + local live-migration (#417 / #413) | 🟡 | `crates/sentinel-microvm/src/{microvm,firecracker}.rs` implements the contract and passes the conformance harness (live-verified on the Deploy-VM, real Firecracker/KVM); #413 adds local ECS-native live-migration (`POST /operator/migrate` → `MigrationCompleted` event, live-verified). **Adapter + conformance only — like all four adapters, microVM is not yet wired into the productive daemon spawn path (`spawn_agent_full` uses `SandboxEnforcer`/`RuntimeOrchestrator` directly, not the registry); that productive `NanoRuntimeRegistry` integration is tracked in #472 (open)** |
| Cross-architecture gate (#394/#406) | 🟡 | DEV-007 keeps cross-architecture replay, snapshot, and isolation evidence as a gate for runtime families that leave the local ECS/native host boundary |
| Tracking epic (#397)          | 🟡 | #397 remains the Cluster 12 tracking epic; #407-#411 deliver the first coherent contract chain and #413/#417 add local live-migration + the microVM adapter (see above). Productive `NanoRuntimeRegistry` integration into the daemon spawn path (#472) and cross-node live migration remain out of scope |

---

## Open work tracked elsewhere

| GitHub issue | Cluster | Topic |
|--------------|---------|-------|
| #278 | 11 | Non-blocking nightrun |
| #336 | 10 | Acronym glossary section + README cross-links (good-first-issue) |

For known intentional deviations from the spec, see
[docs/togaf-deviations-v22.md](togaf-deviations-v22.md).
