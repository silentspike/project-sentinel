# Zenoh Topic Wiring Status

Katalog aller 22 Zenoh-Topics in `crates/sentinel-zenoh/src/topics.rs`.
Referenz fuer zukuenftige Verdrahtungs-Issues.

**ADR:** ADR-001-judge-nats-communication (internal — see CHANGELOG)

## Topics

| # | Topic | Typ | Status | Referenz |
|---|-------|-----|--------|----------|
| 1 | `sentinel/agent/{name}/action` | fn | dead | Kein Zenoh Subscriber |
| 2 | `sentinel/agent/{name}/perception` | fn | dead | Kein Zenoh Subscriber |
| 3 | `sentinel/agent/{name}/state` | fn | dead | Kein Zenoh Subscriber |
| 4 | `sentinel/room/{id}/audio` | fn | dead | Kein Zenoh Subscriber |
| 5 | `sentinel/room/{id}/smell` | fn | dead | Kein Zenoh Subscriber |
| 6 | `sentinel/room/{id}/presence` | fn | dead | Kein Zenoh Subscriber |
| 7 | `sentinel/physics/tick/{n}` | fn | dead | Kein Zenoh Subscriber |
| 8 | `sentinel/chaos/event` | const | dead | Kein Zenoh Subscriber |
| 9 | `sentinel/judge/alert` | const | **deprecated** | ADR-001: NATS `sentinel.judge.alert.*` |
| 10 | `sentinel/agent/{name}/psi` | fn | dead | Kein Zenoh Subscriber |
| 11 | `sentinel/cortex/inject/{name}` | fn | dead | Kein Zenoh Subscriber |
| 12 | `sentinel/meta/model-swap` | const | **deprecated** | ADR-001: NATS alert + HTTP |
| 13 | `sentinel/ebpf/agent-health` | const | **live** | Daemon publishes (Zenoh + NATS) |
| 14 | `sentinel/ebpf/io-profile` | const | **live** | Daemon publishes (Zenoh + NATS) |
| 15 | `sentinel/ebpf/network` | const | **live** | Daemon publishes (Zenoh + NATS) |
| 16 | `sentinel/ebpf/psi` | const | **live** | Daemon publishes (Zenoh + NATS) |
| 17 | `sentinel/ebpf/status` | const | **live** | Daemon publishes (Zenoh + NATS) |
| 18 | `sentinel/query/agent/{name}/request` | fn | dead | Kein Zenoh Subscriber |
| 19 | `sentinel/query/room/{id}/request` | fn | dead | Kein Zenoh Subscriber |
| 20 | `sentinel/query/global/request` | const | dead | Kein Zenoh Subscriber |
| 21 | `sentinel/query/response/{name}` | fn | dead | Kein Zenoh Subscriber |
| 22 | `sentinel` (PREFIX) | const | **live** | Namespace prefix |

## Summary

- **Live:** 6 (5 eBPF + PREFIX)
- **Deprecated:** 2 (JUDGE_ALERT, MODEL_SWAP — ADR-001)
- **Dead:** 14 (nie verdrahtet, keine Subscriber)

## Naechste Schritte

1. **Agent Action/Perception/State (1-3):** Verdrahten wenn ECS→Zenoh Tick-Loop aktiv
2. **Room Topics (4-6):** Verdrahten wenn Room-Events ueber Zenoh fliessen
3. **Physics Tick (7):** Verdrahten wenn Zenoh Tick-Broadcast implementiert
4. **Chaos Event (8):** Verdrahten wenn Physics Engine Chaos→Zenoh publiziert
5. **Agent PSI (10):** Verdrahten wenn Sandbox PSI→Zenoh publiziert
6. **Cortex Inject (11):** Verdrahten wenn Gateway Perception→Zenoh nutzt
7. **Query Topics (18-21):** Bereits als Feature in sentinel-zenoh implementiert (InFlightTracker)
8. **Dead Topics (alle):** Entfernen wenn Wiring-Sprint abgeschlossen
