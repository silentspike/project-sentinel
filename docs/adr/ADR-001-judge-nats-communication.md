# ADR-001: NATS-First Communication for Go Services (Judge, Gateway, Bridge)

- **Status:** Accepted
- **Date:** 2026-03-03
- **Issue:** [#140](https://github.com/obtFusi/project-sentinel/issues/140)

## Context

The TOGAF Architecture Guide (v19.0) specifies Zenoh for Judge alert output
(`sentinel/judge/alert`) and model-swap requests (`sentinel/meta/model-swap`).
However, sentinel-judge is a Go service and Zenoh has no production-quality Go SDK.

The codebase already uses a Dual-Bus architecture:
- **Zenoh** (Rust): SHM <10us, used by sentinel-daemon for ECS ticks, eBPF metrics
- **NATS JetStream** (Go): 1:n fan-out, durable pull consumers, used by sentinel-judge

Currently deployed (10.0.0.240):
- NATS JetStream: UP, 869k+ events, 2 active streams
- sentinel-judge: NATS consumer (events) + NATS publisher (alerts) — fully functional
- Zenoh: Only used in-process by daemon eBPF publisher, no external subscribers
- sentinel-nats-bridge: Limbo EventStore -> NATS (temporary Go bridge)

22 Zenoh topic constants are defined in `sentinel-zenoh`, but only 4 are actively
used (eBPF topics). The remaining 18 are dead code.

## Decision

**NATS-First for all Go-service communication. Daemon bridges eBPF data Zenoh->NATS.**

Specifically:
1. Judge alerts stay on NATS (`sentinel.judge.alert.*`) — no migration to Zenoh
2. Model-swap requests stay on NATS (reuse existing `sentinel.judge.alert.*` with
   `type: "swap"`) — daemon acts on swap alerts via HTTP to Gateway Control Plane
3. eBPF metrics are bridged by the daemon: published on Zenoh (existing) AND on
   NATS (`sentinel.ebpf.*`) via inline bridge in `ebpf_publisher`
4. Judge consumes eBPF metrics via new NATS consumer on `SENTINEL_EBPF` stream
5. TOGAF deviations documented in `docs/togaf-deviations.md`

## Rationale

### Options Considered

| Option | Description | Pros | Cons |
|--------|-------------|------|------|
| **1. NATS-First (chosen)** | Keep NATS for Go, bridge eBPF via daemon | Minimal change, proven stack, no CGO | TOGAF deviation |
| 2. Zenoh for Judge | Add zenoh-c/CGO bindings to Go judge | TOGAF-conformant | No stable Go SDK, CGO complexity, build fragility |
| 3. Hybrid with Zenoh pub | Judge publishes to Zenoh via CGO | Partial TOGAF conformance | Same CGO problems as option 2 |

### Why NATS-First

- **No stable zenoh-go SDK:** zenoh-pico exists for embedded C, but Go bindings
  require CGO with zenoh-c. This introduces build complexity, cross-compilation
  issues, and fragile linking — unacceptable for a production Go service.
- **NATS already proven:** 869k+ events processed, 21h+ uptime, durable consumers
  functioning correctly. Zero message loss observed.
- **Dual-Bus = Language separation:** Zenoh owns the Rust/kernel hot path (ECS,
  eBPF, SHM). NATS owns the Go service layer (Judge, Gateway, Bridge). This is
  a clean architectural boundary, not a compromise.
- **Bridge is trivial:** The daemon already has both Zenoh (via sentinel-zenoh)
  and NATS (via async-nats) as dependencies. Adding NATS publish alongside Zenoh
  publish in `ebpf_publisher` is ~20 lines of code.

## Consequences

### Positive
- No CGO dependency in any Go service
- Single bus per service (Judge = NATS only, Daemon = Zenoh + NATS bridge)
- eBPF metrics available to Judge for drift-detection enrichment
- Model-swap E2E flow works via existing NATS alert path

### Negative
- TOGAF deviation: Document explicitly states Zenoh for Judge alert/model-swap
- Dead Zenoh topics remain (deprecated, cataloged for future cleanup)
- Additional NATS load from eBPF bridge (~1 msg/s per topic, negligible)

### Neutral
- sentinel-nats-bridge (Limbo->NATS) remains active — sunset when daemon-internal
  Zenoh<->NATS event bridge replaces it (separate issue, not part of #140)

## sentinel-nats-bridge Sunset Plan

The temporary Go bridge (`services/sentinel-nats-bridge/`) will be replaced by a
Rust-native bridge inside `sentinel-daemon` when:

1. All Zenoh topics are actively wired (currently 4/22)
2. Daemon subscribes to Zenoh event topics and republishes on NATS
3. Limbo outbox can be consumed via Zenoh instead of direct SQLite polling

**Timeline:** Not part of #140. Separate issue after full Zenoh wiring is complete.
**Migration:** Gradual — run both bridges in parallel, verify message parity, then
decommission Go bridge.

## References

- TOGAF Architecture Guide v19.0: Section "Zenoh Topic-Hierarchie"
- Issue #140: Judge-Daemon Zenoh/NATS Integration
- `crates/sentinel-zenoh/src/topics.rs`: 22 topic constants (4 live, 18 dead)
- `docs/togaf-deviations.md`: Deviation register
