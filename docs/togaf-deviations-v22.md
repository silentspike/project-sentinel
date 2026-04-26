# TOGAF v22.1 — Deviation Register

This document records intentional deviations between the architecture
spec ([docs/architecture/togaf-architecture-guide.html](architecture/togaf-architecture-guide.html))
and the implementation. A deviation here is a deliberate choice, not a
bug or a missing feature. For implementation status see
[docs/togaf-gap-v22.md](togaf-gap-v22.md).

Each deviation records: **what** the spec says, **what** the code does,
**why** the choice was made, and **how** to revisit it later.

---

## DEV-001 — In-process controlplane kernel instead of separate service

| Field | Value |
|-------|-------|
| Cluster | 05b (Telemetrie & Observability) + cross-cutting |
| Spec | controlplane reasoner is described as a long-lived independent process with its own isolation profile |
| Implementation | controlplane runs in-process inside `sentinel-daemon` as three loops (Agent CP, Platform CP, API CP) |
| Files | `services/sentinel-daemon/src/controlplane/`, `services/sentinel-daemon/src/platform_controlplane/`, `cmd/cortex-gateway/internal/apicp/` |
| Why | the controlplane needs sub-millisecond access to ECS state for every tick. An out-of-process design adds two IPC hops and a serialisation boundary per tick, which destroys the deterministic-replay guarantee. The daemon already runs under the strict sandbox profile, so isolation is preserved without a separate service. |
| Revisit when | the platform-CP component grows beyond ~5k LoC or starts being reused by another binary |

## DEV-002 — Two pub/sub buses (Zenoh + NATS JetStream) instead of one

| Field | Value |
|-------|-------|
| Cluster | 03 (Infrastruktur) |
| Spec | a single message bus across the whole system |
| Implementation | Zenoh inside the Rust core (shared-memory <10 µs); NATS JetStream for the Go services and durable / fan-out work |
| Files | `crates/sentinel-zenoh/`, `pkg/sentinel-go/messaging/` |
| Why | Zenoh's Rust SHM transport is the only way to keep ECS-tick perception inside its 1-ms budget. NATS gives Go services real durable consumers, replay, and 1:N fan-out without hand-rolling persistence on top of Zenoh. Go-Zenoh bindings are immature (as of v22.1 baseline). |
| Revisit when | Zenoh ships a stable Go API with comparable durable-consumer semantics, or the cost of operating two buses begins to outweigh latency gains |

## DEV-003 — Single-binary daemon owns multiple clusters

| Field | Value |
|-------|-------|
| Cluster | 03 (Infrastruktur), 02 (Agent-Ontologie), 05b (Observability) |
| Spec | each cluster is a self-contained deployable unit |
| Implementation | `sentinel-daemon` packages ECS world, agent runtime, controlplane, runtime-health, projections, and orchestrator into one process |
| Files | `services/sentinel-daemon/` |
| Why | the ECS world is a single shared mutable resource. Splitting it across processes means cross-process locking and copy-on-write of the entire world per tick, which the CPU budget does not allow. |
| Revisit when | horizontal scaling becomes a goal (today the runtime targets one VM with all 60 agents); also when WASM-based agent isolation lets us evict agents to subprocesses without crossing the world-state line |

---

## DEV-004 — Internal-only ADRs

| Field | Value |
|-------|-------|
| Cluster | 10 (Software Design Description) |
| Spec | publish per-decision ADR records under `docs/adr/` |
| Implementation | ADRs live in the internal workspace, not the public repository |
| Files | (excluded from public repo) |
| Why | pre-public ADRs reference iteration history, defunct designs, and infrastructure specifics that would be misleading to a public reader and would not survive history rewrites between major refactors. The CHANGELOG plus this deviation register cover the publicly relevant subset. |
| Revisit when | a stable post-1.0 architectural baseline is reached and ADRs would describe forward decisions only |

## DEV-005 — Internal-only verification matrices

| Field | Value |
|-------|-------|
| Cluster | 09 (Wissenschaftliche Grundlagen) |
| Spec | publish per-acceptance-criterion verification matrices alongside the docs they verify |
| Implementation | the 61 historical verification artefacts (B01–B03 benchmarks, T01–T42 test matrices, raw run logs, v2/v3 duplicates, issue-specific evidence) live in the internal workspace |
| Files | (excluded from public repo) |
| Why | these artefacts describe AC-level evidence for issues that are now closed and whose acceptance criteria are no longer the public contract. They consist mostly of raw command output and intermediate iteration runs and are not useful as living documentation. The current public contract is reflected by the CI workflow status and the gap report. |
| Revisit when | a public test-evidence portal becomes part of the release process |

---

For governance mechanisms see [docs/governance.md](governance.md). For
the underlying spec see
[docs/architecture/togaf-architecture-guide.html](architecture/togaf-architecture-guide.html).
