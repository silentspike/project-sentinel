# Functional Audit: sentinel-judge Communication

**Date:** 2026-03-03
**Issue:** [#140](https://github.com/silentspike/project-sentinel/issues/140)
**ADR:** ADR-001-judge-nats-communication (internal — see CHANGELOG)

## Audit Findings

### B-5: Judge Service Communication Architecture

| Aspect | TOGAF Spec | Deployed Reality | Status |
|--------|-----------|-----------------|--------|
| Judge Alert Output | Zenoh `sentinel/judge/alert` | NATS `sentinel.judge.alert.*` | DEVIATION (ADR-001) |
| Judge Event Input | NATS Consumer | NATS Consumer (SENTINEL_EVENTS) | CONFORMANT |
| Judge Batch API | HTTP :8082 | HTTP :8082 (POST /api/v1/analyze) | CONFORMANT |
| Judge eBPF Input | Zenoh Subscribe | NATS Consumer (SENTINEL_EBPF) | DEVIATION (ADR-001) |
| Model-Swap Output | Zenoh `sentinel/meta/model-swap` | NATS alert (type: "swap") + Daemon HTTP | DEVIATION (ADR-001) |

**Resolution:** ADR-001 documents NATS-First decision. TOGAF deviations registered.

### M-17: Judge Subscribes eBPF Metrics via Zenoh

- **Spec:** Judge subscribes to Zenoh eBPF topics for drift-detection enrichment
- **Reality:** Judge has new NATS consumer for `sentinel.ebpf.*` subjects (via
  daemon bridge). eBPF stall data enriches drift-detection scoring.
- **Status:** IMPLEMENTED (via NATS bridge, not direct Zenoh — ADR-001)

### M-18: Zenoh Judge Alert Topic

- **Spec:** `sentinel/judge/alert` Zenoh topic active, subscribers receive alerts
- **Reality:** Topic constant exists in `topics.rs:48` but is dead code. Alerts
  flow via NATS `sentinel.judge.alert.*`. Daemon consumes via `nats_consumer.rs`.
- **Status:** DEVIATION — Zenoh topic deprecated per ADR-001. NATS path functional.

### M-19: Zenoh Model-Swap Topic

- **Spec:** `sentinel/meta/model-swap` Zenoh topic for Judge->Gateway swap requests
- **Reality:** Topic constant exists in `topics.rs:61` but is dead code. Swap flow:
  Judge emits NATS alert (type: "swap") -> Daemon receives -> HTTP POST to Gateway
  Control Plane `/control/agent-provider`.
- **Status:** DEVIATION — Zenoh topic deprecated per ADR-001. NATS+HTTP path functional.

### M-20: eBPF + LLM Sentiment Correlation

- **Spec:** Judge correlates eBPF metrics with LLM sentiment for drift-detection
- **Reality:** Judge receives eBPF stall/IO data via NATS and uses it as weight
  factor in drift-score calculation: `finalDrift = 0.7*textDrift + 0.3*ebpfSignal`.
  Full LLM sentiment correlation (combining eBPF CPU pressure with LLM response
  quality) is out of scope for #140 — planned as follow-up feature.
- **Status:** PARTIAL — eBPF enrichment implemented, full sentiment correlation deferred.

## Summary

| Finding | Status | Resolution |
|---------|--------|------------|
| B-5 | 4/5 conformant, 1 deviation | ADR-001 |
| M-17 | Implemented (via NATS) | ADR-001 |
| M-18 | Deviation (Zenoh deprecated) | ADR-001 |
| M-19 | Deviation (NATS+HTTP path) | ADR-001 |
| M-20 | Partial (eBPF enrichment done, sentiment correlation deferred) | Follow-up issue |
