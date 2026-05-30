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

## DEV-006 — Nano-Container runtime defaults to WASM/WASI with native escape hatch

| Field | Value |
|-------|-------|
| Cluster | 12 (Zielarchitektur / Nano-Container Platform) |
| Status | Superseded by DEV-007 (#407) on 2026-05-29. Historical record only; no longer the active runtime contract. |
| Spec | Cluster 12 leaves the defining platform fork open: a WASM-bound runtime with millisecond spin-up and high density, or arbitrary native code with full runtime freedom but container/Firecracker-like cost. |
| Decision | WASM/WASI on Wasmtime is the default Nano-Container runtime contract. Arbitrary native code is not part of the default density/portability promise; it is allowed only through an explicit native Escape-Hatch-Pool with separate scheduling, stronger isolation, and no millisecond spin-up guarantee. |
| Runtime contract | Default runtime: `wasm+wasi` via `crates/sentinel-wasm/` and the sandbox capability registry. Native runtime: opt-in pool for workloads that prove they cannot fit WASM/WASI, isolated outside the default hot path. |
| Trade-offs | WASM/WASI wins density, portability, reproducible cold start, and least-privilege capability control, but constrains runtime freedom. Native code wins language/runtime freedom and compatibility with existing binaries, but loses the core density, portability, and deterministic spin-up advantages and increases host-isolation burden. |
| Why | The current platform strength is small, portable, capability-scoped execution that can emerge from the agent system without becoming a generic container platform first. A native default would discard that advantage and pull the project back toward the existing container/Firecracker design space. The escape hatch preserves product flexibility without making native execution the baseline contract. |
| Consequences | Follow-up work under #397 must design around `wasm+wasi` as the baseline. Native support must be tracked as an explicit exception path with separate capacity planning, security review, and verification. Clusters 00-11 remain untouched until a Cluster 12 building block is validated from concrete agent-system need. |
| Files | `docs/togaf-deviations-v22.md`, `crates/sentinel-wasm/`, `crates/sentinel-sandbox/`, #396, #397 |
| Revisit when | real customer or agent workloads repeatedly require native runtimes, WASI cannot cover the needed system interface, or native escape-hatch usage becomes common enough to threaten the platform's default density and security assumptions |

## DEV-007 — Nano-Container CRI contract without a default runtime

| Field | Value |
|-------|-------|
| Cluster | 12 (Zielarchitektur / Nano-Container Platform) |
| Spec | Cluster 12 needs a Nano-Container execution contract but does not require a single runtime family. The architecture must remain open for dense in-process ECS workloads, WASM/WASI tools, hardened host processes, and later microVM isolation. |
| Decision | The active Nano-Container contract is runtime-agnostic and CRI-style. Workloads select an explicit runtime key; there is no global default runtime. An orchestrator may configure an explicit fallback key, but that fallback is policy data, not an architectural default. |
| Runtime contract | A compliant runtime implements seven operations: `spawn`, `exec`, `snapshot`, `restore`, `migrate`, `health`, and `isolate`. Initial runtime families are `ecs-native`, `wasm-wasmtime`, `bwrap-landlock`, and future `microvm`. |
| Options considered | Option 1: fixed WASM/WASI default with native escape hatch (DEV-006). Option 2: native/container-first runtime as the baseline. Option 3: plural CRI contract with per-workload runtime selection. DEV-007 chooses Option 3. |
| Trade-offs | The plural contract keeps runtime density and portability available where they fit, while allowing stronger process or microVM isolation for workloads that need it. The cost is a stricter conformance harness and explicit workload routing: every runtime must document snapshot semantics and every caller must choose a runtime key or a configured fallback. |
| Why | The maintainer decision on 2026-05-29 defines Project Sentinel's Nano-Container axis as "Beyond Kubernetes": one contract, multiple runtime implementations, and workload-specific selection. This supersedes the earlier WASM-default choice without rejecting WASM/WASI as one strong runtime option. |
| Consequences | #408 defines the shared `NanoRuntime` trait and conformance harness. #409 and #410 must prove their adapters against that harness. #411 owns registry and selection policy. Cross-architecture gate work (#394/#406) remains coupled: runtime contracts that cross architecture boundaries must keep replay, snapshot, and isolation evidence explicit. |
| Files | `docs/togaf-deviations-v22.md`, `docs/togaf-gap-v22.md`, `crates/sentinel-common/src/nano_runtime.rs`, `crates/sentinel-runtime/`, `crates/sentinel-wasm/`, `crates/sentinel-sandbox/`, #397, #407, #408, #409, #410, #411 |
| Revisit when | microVM support moves from future runtime family to implemented adapter; cross-node migration becomes a product requirement; or conformance evidence shows the seven-operation contract is too weak or too broad for real workloads |

---

For governance mechanisms see [docs/governance.md](governance.md). For
the underlying spec see
[docs/architecture/togaf-architecture-guide.html](architecture/togaf-architecture-guide.html).
