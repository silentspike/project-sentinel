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
| 4-shift assignment (1+2+3+0)  | ✅ | `shift_set` field in each agent TOML; counts 17/17/17/9 |
| PixelPerfekt narrative        | ✅ | `config/company-context.md`, [docs/glossary.md](glossary.md) |
| 5 background services         | ✅ | sentinel-daemon, cortex-gateway, sentinel-judge, sentinel-nightrun, sentinel-nats-bridge |

## Cluster 02 — Agent-Ontologie

| Item                          | Status | Evidence |
|-------------------------------|--------|----------|
| Big-Five personality model    | ✅ | `crates/sentinel-common/src/personality.rs` |
| Bio-engine (6 differential eqs) | ✅ | `crates/sentinel-bio/src/` |
| Mood (valence + arousal)      | ✅ | `crates/sentinel-bio/src/mood.rs` |
| Memory (episodic + semantic)  | 🟡 | `crates/sentinel-hippocampus/`; multi-tier in place, NMDA selection still tuning |
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
| WASM tool runtime             | ✅ | `crates/sentinel-wasm/` |
| eBPF monitoring (aya-rs)      | ✅ | `crates/sentinel-ebpf/` |

## Cluster 04 — Cortex Gateway

| Item                          | Status | Evidence |
|-------------------------------|--------|----------|
| 7-step LLM pipeline           | ✅ | `cmd/cortex-gateway/internal/{normalizer,compiler,detection,extraction,capability,proxy}/` |
| Synthesis engine (10 rules)   | ✅ | `cmd/cortex-gateway/internal/synthesis/rules.go` |
| Fourth-wall detection (15 regex + judge) | ✅ | `cmd/cortex-gateway/internal/detection/` |
| Perception injection          | ✅ | `cmd/cortex-gateway/internal/injection/` + 8 system blocks |
| API controlplane              | ✅ | `cmd/cortex-gateway/internal/apicp/` |
| MITM forward (`/v1/messages`) | ✅ | `cmd/cortex-gateway/internal/intercept/` |
| Provider registry             | ✅ | claude-code + anthropic-direct + ollama (config-selectable) |

## Cluster 05 — Tech Stack Reference

| Item                          | Status | Evidence |
|-------------------------------|--------|----------|
| Rust workspace (15 crates + 2 services) | ✅ | `Cargo.toml` workspace members |
| Go workspace (4 modules)      | ✅ | `go.work` |
| Bun + Hono dashboard          | ✅ | `dashboard/` |
| Build server pattern          | ✅ | `cargo remote --` (server IP injected via `.make.local`) |
| 16 GitHub Actions workflows   | ✅ | `.github/workflows/*.yml` |

## Cluster 05b — Telemetrie & Observability

| Item                          | Status | Evidence |
|-------------------------------|--------|----------|
| Prometheus metrics            | ✅ | `crates/sentinel-telemetry/`, gateway `/metrics` |
| Health + readiness endpoints  | ✅ | each service exposes `/healthz`, `/readyz` |
| eBPF probes (write-syscall, stall) | ✅ | `crates/sentinel-ebpf/` |
| MARBLE Observatory            | ✅ | `cmd/cortex-gateway/internal/observatory/` |
| OTel tracing                  | 🟡 | structured spans in place, OTLP exporter behind feature flag |

## Cluster 06 — Performance & Hardware

| Item                          | Status | Evidence |
|-------------------------------|--------|----------|
| BFS room search benchmark     | ✅ | 2.59 – 7.09 µs (criterion) |
| Encounter-detection benchmark | ✅ | 7.34 – 8.05 µs |
| Bio-tick benchmark            | ✅ | 154 – 171 µs |
| Synthesis-engine latency      | ✅ | 95 – 103 µs end-to-end |
| Tick-loop hot-path (#276)     | ⏳ | open issue, planned post-v0.1.0-alpha |

## Cluster 07 — Deployment & Tuning

| Item                          | Status | Evidence |
|-------------------------------|--------|----------|
| systemd units (12)            | ✅ | `deploy/systemd/*.service` |
| Release manifest schema       | ✅ | `deploy/release-manifest.schema.json` |
| Demo compose stack            | ⏳ | Phase 11 of public-readiness sprint |
| 4-tier IP / path strategy     | ✅ | `.env.example`, `.make.local.example`, `deploy/systemd/sentinel-env.example`, `Makefile` |

## Cluster 08 — LLM Behavioral Science

| Item                          | Status | Evidence |
|-------------------------------|--------|----------|
| Personality-driven prompting  | ✅ | gateway prompt compiler injects Big-Five vector |
| Drift / quality / fatigue / swap heuristics | ✅ | `pkg/sentinel-go/judge/` |
| LLM-judge for fourth-wall     | ✅ | `cmd/cortex-gateway/internal/detection/llm_judge.go` |
| Voice analysis (semantic drift) | ✅ | `services/sentinel-judge/internal/analyzer/` |

## Cluster 09 — Wissenschaftliche Grundlagen

| Item                          | Status | Evidence |
|-------------------------------|--------|----------|
| Reproducible scenario runs    | ✅ | nightrun deterministic replay + SHA-256 hash chain |
| Event-sourcing primitives     | ✅ | `crates/sentinel-limbo/` |
| Snapshot tiered retention     | ✅ | `crates/sentinel-limbo/src/snapshot.rs` |

## Cluster 10 — Software Design Description

| Item                          | Status | Evidence |
|-------------------------------|--------|----------|
| Component-level READMEs       | 🟡 | `config/README.md`, `deploy/README.md`; per-crate READMEs deferred |
| llms.txt index                | ✅ | `llms.txt` |
| Glossary                      | ✅ | [docs/glossary.md](glossary.md) |
| Governance ↔ code map         | ✅ | [docs/governance.md](governance.md) |

## Cluster 11 — Time Machine

| Item                          | Status | Evidence |
|-------------------------------|--------|----------|
| WorldSnapshot (bincode 2)     | ✅ | `crates/sentinel-limbo/src/snapshot.rs` |
| Hot-swap restore              | ✅ | `services/sentinel-daemon/src/runtime_health.rs` (snapshot reload path) |
| Deterministic replay          | ✅ | `services/sentinel-nightrun/` |
| Time-travel debugging UI      | ⏳ | dashboard hook designed, frontend deferred |

---

## Open work tracked elsewhere

| GitHub issue | Cluster | Topic |
|--------------|---------|-------|
| #266 | 01 | Gaia firmen-konfigurator (multi-tenant company definitions) |
| #276 | 06 | Tick-loop hot-path optimisation |
| #277 | 05b | Dashboard polling efficiency |
| #278 | 11 | Non-blocking nightrun |
| #279 | 03 | Daemon hardening |

For known intentional deviations from the spec, see
[docs/togaf-deviations-v22.md](togaf-deviations-v22.md).
