# TOGAF Architecture Guide — Deviation Register

This document tracks intentional deviations between the TOGAF Architecture Guide
(v19.0) and the deployed codebase. Each deviation has an associated ADR.

## DEV-001: Judge Communication via NATS (not Zenoh)

- **ADR:** [ADR-001](adr/ADR-001-judge-nats-communication.md)
- **TOGAF Says:** Judge publishes anomalies on Zenoh `sentinel/judge/alert`,
  model-swap requests on Zenoh `sentinel/meta/model-swap`
- **Code Does:** Judge publishes alerts on NATS `sentinel.judge.alert.*`,
  daemon handles swap alerts via HTTP to Gateway Control Plane
- **Reason:** No production-quality zenoh-go SDK. Judge is Go-native, NATS is
  the Go-side bus in the Dual-Bus architecture.
- **Impact:** None on functionality. Judge alerts flow correctly via NATS.
  Daemon bridges eBPF metrics Zenoh->NATS for Judge consumption.
- **Resolution:** TOGAF will be updated to reflect Dual-Bus language separation
  (Zenoh=Rust, NATS=Go) as architectural principle, not per-topic assignment.

## DEV-002: eBPF Metrics Dual-Published (Zenoh + NATS)

- **ADR:** [ADR-001](adr/ADR-001-judge-nats-communication.md)
- **TOGAF Says:** eBPF metrics published only on Zenoh topics
- **Code Does:** Daemon publishes eBPF on Zenoh (for Rust subscribers) AND
  bridges to NATS (for Go subscribers like Judge)
- **Reason:** Judge needs eBPF data for drift-detection but cannot subscribe
  to Zenoh directly. Bridge pattern avoids CGO dependency.
- **Impact:** Minimal — ~5 additional NATS messages per second. Memory-backed
  `SENTINEL_EBPF` stream with 1-day retention.

## DEV-003: sentinel-nats-bridge Still Active (Not Replaced by Daemon Bridge)

- **TOGAF Says:** sentinel-nats-bridge is temporary, replaced by daemon Zenoh<->NATS
- **Code Does:** Go bridge still active, polls Limbo outbox -> NATS
- **Reason:** Full Zenoh wiring (22 topics) not yet complete. Bridge replacement
  requires daemon to subscribe all Zenoh event topics first.
- **Timeline:** Separate issue after full Zenoh wiring sprint.
