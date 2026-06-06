# Zenoh Topic Wiring Status

Catalog of all 22 Zenoh topics in `crates/sentinel-zenoh/src/topics.rs`.
Reference for future wiring issues.

**ADR:** ADR-001-judge-nats-communication (internal - see CHANGELOG)

## Topics

| # | Topic | Type | Status | Reference |
|---|-------|-----|--------|----------|
| 1 | `sentinel/agent/{name}/action` | fn | dead | No Zenoh subscriber |
| 2 | `sentinel/agent/{name}/perception` | fn | dead | No Zenoh subscriber |
| 3 | `sentinel/agent/{name}/state` | fn | dead | No Zenoh subscriber |
| 4 | `sentinel/room/{id}/audio` | fn | dead | No Zenoh subscriber |
| 5 | `sentinel/room/{id}/smell` | fn | dead | No Zenoh subscriber |
| 6 | `sentinel/room/{id}/presence` | fn | dead | No Zenoh subscriber |
| 7 | `sentinel/physics/tick/{n}` | fn | dead | No Zenoh subscriber |
| 8 | `sentinel/chaos/event` | const | dead | No Zenoh subscriber |
| 9 | `sentinel/judge/alert` | const | **deprecated** | ADR-001: NATS `sentinel.judge.alert.*` |
| 10 | `sentinel/agent/{name}/psi` | fn | dead | No Zenoh subscriber |
| 11 | `sentinel/cortex/inject/{name}` | fn | dead | No Zenoh subscriber |
| 12 | `sentinel/meta/model-swap` | const | **deprecated** | ADR-001: NATS alert + HTTP |
| 13 | `sentinel/ebpf/agent-health` | const | **live** | Daemon publishes (Zenoh + NATS) |
| 14 | `sentinel/ebpf/io-profile` | const | **live** | Daemon publishes (Zenoh + NATS) |
| 15 | `sentinel/ebpf/network` | const | **live** | Daemon publishes (Zenoh + NATS) |
| 16 | `sentinel/ebpf/psi` | const | **live** | Daemon publishes (Zenoh + NATS) |
| 17 | `sentinel/ebpf/status` | const | **live** | Daemon publishes (Zenoh + NATS) |
| 18 | `sentinel/query/agent/{name}/request` | fn | dead | No Zenoh subscriber |
| 19 | `sentinel/query/room/{id}/request` | fn | dead | No Zenoh subscriber |
| 20 | `sentinel/query/global/request` | const | dead | No Zenoh subscriber |
| 21 | `sentinel/query/response/{name}` | fn | dead | No Zenoh subscriber |
| 22 | `sentinel` (PREFIX) | const | **live** | Namespace prefix |

## Summary

- **Live:** 6 (5 eBPF + PREFIX)
- **Deprecated:** 2 (JUDGE_ALERT, MODEL_SWAP - ADR-001)
- **Dead:** 14 (never wired, no subscribers)

## Next Steps

1. **Agent Action/Perception/State (1-3):** wire when ECS->Zenoh tick loop is active
2. **Room Topics (4-6):** wire when room events flow via Zenoh
3. **Physics Tick (7):** wire when Zenoh tick broadcast is implemented
4. **Chaos Event (8):** wire when Physics Engine publishes Chaos->Zenoh
5. **Agent PSI (10):** wire when Sandbox publishes PSI->Zenoh
6. **Cortex Inject (11):** wire when Gateway uses Perception->Zenoh
7. **Query Topics (18-21):** already implemented as a feature in sentinel-zenoh (InFlightTracker)
8. **Dead Topics (all):** remove when the wiring sprint is complete
