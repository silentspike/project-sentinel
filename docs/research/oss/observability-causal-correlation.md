# OSS Observability, Tracing, and Causal-Correlation Study

Status: source-backed synthesis accepted and implementation ownership materialized
Issue: [#718](https://github.com/silentspike/project-sentinel/issues/718)
Parent: [#659](https://github.com/silentspike/project-sentinel/issues/659)
Sentinel baseline: `e85eb67f13beb240b4c4638d3f37d76f219b8463`
Research date: 2026-07-29

## Executive decision

Sentinel should retain its event store and domain identifiers as the authority
for customer-work causality. It should not make an external trace backend,
OpenTelemetry trace IDs, sampled spans, or telemetry delivery part of business
correctness.

The recommended direction is:

1. **Port algorithm/contract** for a Sentinel-owned `CausalContextV1` carried by
   the canonical event and transport envelopes. It binds stable request,
   operation, workflow, work-item, invocation, event, generation, and artifact
   references without copying their authoritative records.
2. **Configure existing dependency** `tracing` for bounded, structured spans at
   material service, effect, and state-transition boundaries. Do not emit one
   span per entity or routine simulation tick.
3. **Reimplement minimal** W3C Trace Context handling as a Sentinel-owned
   boundary adapter limited to a valid optional `traceparent` diagnostic
   reference. It rejects `tracestate` and baggage in M0. OTel SDK/bridge use and
   broader interoperability remain POST_M0 decisions under
   [#705](https://github.com/silentspike/project-sentinel/issues/705).
4. **Keep Sentinel** structured logs, existing `tracing`, and atomic
   histogram/counter metrics as the M0 observability path. Span export remains
   debug-only. Do not add a Collector, OTLP queue, external backend, or second
   telemetry buffer/store for M0.
5. **Reject** Tempo, Jaeger, OpenObserve, SigNoz, and Vector as M0 runtime
   additions. They add useful mechanisms, but a second query/store/control
   plane would violate the current one-authority and 1:n fit. Vector remains a
   useful implementation reference for explicit buffer behavior and
   cardinality controls.
6. **Keep Sentinel** event, projection, artifact, QA, release, and delivery
   records as the incident-reconstruction source. Derived telemetry may point
   to those records but must not copy or supersede them.

This is an architecture study, not a benchmark. No upstream system was run as
a Sentinel workload, no runtime or build host was accessed, and no performance
claim is inferred from upstream benchmark numbers.

## Method and decision rules

### Evidence standard

- Sentinel claims are pinned to the baseline above and cite current source,
  tests, target contracts, or live issues.
- Upstream claims are pinned to exact commits and cite implementation and
  tests, not documentation alone.
- Repository activity, releases, and licenses are screening evidence, not
  correctness proof.
- An upstream test proves only its own behavior. It does not prove Sentinel
  integration, security, operational cost, or M0 readiness.
- Performance statements in this document are hypotheses. Any implementation
  must measure on its declared product target under its own issue.
- A dependency requires necessity review in
  [#705](https://github.com/silentspike/project-sentinel/issues/705), an upgrade
  contract in [#656](https://github.com/silentspike/project-sentinel/issues/656),
  a runtime owner, and a security boundary.
- Business facts remain 1:n: one authoritative record may have many event,
  span, log, metric, and view references; telemetry must not become another
  writable truth.

### Screening rubric

Each criterion is scored from 0 (absent or incompatible) to 3 (strong,
source-backed fit):

| Criterion | Question |
|---|---|
| Causal fit | Does it preserve parent/link/context semantics across processes and queues? |
| Maturity and tests | Are the load-bearing mechanisms actively maintained and tested? |
| Failure and recovery | Are drops, retries, restart, queue, and storage failures explicit? |
| Resource and operations | Are memory, disk, cardinality, backpressure, and deployment controls explicit? |
| Security and privacy | Are authentication, redaction, tenant, and disclosure boundaries explicit? |
| License and maintenance | Is use and long-term ownership compatible with Sentinel? |
| Language and dependency | Can the useful mechanism be integrated without a disproportionate runtime? |
| Local-first and 1:n | Can it remain derived, bounded, local-first, and outside business authority? |

The score is a comparison aid, not an adoption threshold. A high score cannot
override authority, privacy, or operational-fit failures.

## Sentinel baseline

### Current causal and telemetry mechanisms

| Surface | Source-backed behavior | Consequence |
|---|---|---|
| Telemetry context | [`TraceContext`](https://github.com/silentspike/project-sentinel/blob/e85eb67f13beb240b4c4638d3f37d76f219b8463/crates/sentinel-telemetry/src/context.rs#L34-L89) contains a generated correlation ID plus optional agent and tick and can create a `tracing` span. Repository-wide use is confined to the telemetry crate and its tests. | It is a useful prototype, not a runtime propagation contract. It lacks request digest, direct causation, workflow/work-item/invocation/artifact references, generation, and transport adapters. |
| Telemetry export | [`TelemetryExporter`](https://github.com/silentspike/project-sentinel/blob/e85eb67f13beb240b4c4638d3f37d76f219b8463/crates/sentinel-telemetry/src/export.rs#L103-L193) publishes metric, health, and error snapshots. It has no trace export method. | The trace topic constant does not prove an active trace path. |
| Domain event | [`DomainEvent`](https://github.com/silentspike/project-sentinel/blob/e85eb67f13beb240b4c4638d3f37d76f219b8463/crates/sentinel-common/src/events.rs#L24-L108) persists event, correlation, optional causation, operation, tick, timestamp, schema, and compensation fields. Default event and operation IDs are random unless a producer overrides them. | This is the current durable causal substrate, but producer discipline is uneven and the schema does not yet carry the M0 work identifiers. |
| Event and outbox storage | The event table persists all causal fields, while the outbox row stores only event ID, topic, payload, status, and retry data ([schema](https://github.com/silentspike/project-sentinel/blob/e85eb67f13beb240b4c4638d3f37d76f219b8463/crates/sentinel-limbo/src/event_store.rs#L34-L79)). Event and outbox append are atomic and operation-idempotent ([append](https://github.com/silentspike/project-sentinel/blob/e85eb67f13beb240b4c4638d3f37d76f219b8463/crates/sentinel-limbo/src/event_store.rs#L993-L1051)). | Durable local truth is strong, but an outbox consumer must join the event or receive a canonical envelope to preserve context. |
| Rust outbox publisher | [`OutboxPublisher`](https://github.com/silentspike/project-sentinel/blob/e85eb67f13beb240b4c4638d3f37d76f219b8463/crates/sentinel-limbo/src/outbox_publisher.rs#L140-L200) publishes only the stored topic and payload. | Identity can disappear on this path unless the payload itself happens to contain it. |
| Go NATS bridge | The Go event store joins event metadata when polling the outbox ([poll](https://github.com/silentspike/project-sentinel/blob/e85eb67f13beb240b4c4638d3f37d76f219b8463/pkg/sentinel-go/eventstore/store.go#L329-L367)); the bridge emits operation, event, type, aggregate, tick, and correlation headers ([publish](https://github.com/silentspike/project-sentinel/blob/e85eb67f13beb240b4c4638d3f37d76f219b8463/services/sentinel-nats-bridge/main.go#L198-L233)). | This path preserves more identity than the Rust publisher but omits direct causation. The two paths do not share one envelope contract. |
| Zenoh fanout | [`EventFanout`](https://github.com/silentspike/project-sentinel/blob/e85eb67f13beb240b4c4638d3f37d76f219b8463/services/sentinel-daemon/src/fanout.rs#L1-L61) publishes the full domain event as FlatBuffers or JSON and explicitly leaves Limbo as source of truth. Only selected event types fan out ([filter](https://github.com/silentspike/project-sentinel/blob/e85eb67f13beb240b4c4638d3f37d76f219b8463/services/sentinel-daemon/src/fanout.rs#L64-L120)). | Full event identity survives selected Zenoh paths, but absence from fanout is not absence from truth. |
| ECS producers | Action, bio, smell, and transit producers can share correlation and set a direct trigger event as causation ([systems](https://github.com/silentspike/project-sentinel/blob/e85eb67f13beb240b4c4638d3f37d76f219b8463/crates/sentinel-ecs/src/systems.rs#L284-L340), [autonomy](https://github.com/silentspike/project-sentinel/blob/e85eb67f13beb240b4c4638d3f37d76f219b8463/crates/sentinel-ecs/src/autonomy.rs#L268-L303)). | The mechanism works where used, but there is no repository-wide invariant that every non-root event has direct causation. |
| Gateway request | The gateway accepts or generates `X-Request-ID` but does not extract W3C Trace Context ([pipeline](https://github.com/silentspike/project-sentinel/blob/e85eb67f13beb240b4c4638d3f37d76f219b8463/cmd/cortex-gateway/internal/proxy/pipeline.go#L309-L354)). Persisted actions and rejections use request ID as correlation and derive operation IDs ([actions](https://github.com/silentspike/project-sentinel/blob/e85eb67f13beb240b4c4638d3f37d76f219b8463/cmd/cortex-gateway/internal/proxy/pipeline.go#L1342-L1363), [rejections](https://github.com/silentspike/project-sentinel/blob/e85eb67f13beb240b4c4638d3f37d76f219b8463/cmd/cortex-gateway/internal/proxy/pipeline.go#L1395-L1451)). | A useful request chain exists, but it is not a complete business or trace context and the emitted events lack direct causation. |
| Provider call | The daemon sends stable `X-Request-ID` and `X-Request-Digest`; request ID is agent/tick-derived and digest is SHA-256 over the request ([construction](https://github.com/silentspike/project-sentinel/blob/e85eb67f13beb240b4c4638d3f37d76f219b8463/services/sentinel-daemon/src/llm_bridge.rs#L276-L302)). It rejects response-ID mismatch and durably records the completion ([completion](https://github.com/silentspike/project-sentinel/blob/e85eb67f13beb240b4c4638d3f37d76f219b8463/services/sentinel-daemon/src/llm_bridge.rs#L483-L523)); restart recovery rejects digest reuse conflicts ([recovery](https://github.com/silentspike/project-sentinel/blob/e85eb67f13beb240b4c4638d3f37d76f219b8463/services/sentinel-daemon/src/llm_bridge.rs#L800-L843)). | This is the strongest current identity/digest pattern and should be generalized, not replaced by trace IDs. |
| Projection | The worker reads ordered events, commits view work, then advances an external offset ([loop](https://github.com/silentspike/project-sentinel/blob/e85eb67f13beb240b4c4638d3f37d76f219b8463/crates/sentinel-projection/src/worker.rs#L118-L147)); rebuild clears views and replays from zero ([rebuild](https://github.com/silentspike/project-sentinel/blob/e85eb67f13beb240b4c4638d3f37d76f219b8463/crates/sentinel-projection/src/worker.rs#L151-L205)). The store documents view-before-offset idempotent restart semantics ([contract](https://github.com/silentspike/project-sentinel/blob/e85eb67f13beb240b4c4638d3f37d76f219b8463/crates/sentinel-projection/src/store.rs#L1-L8)). | Projection state is derived and restartable, but current views do not expose a complete causal chain or generation identity. |

### End-to-end M0 request trace

The target company-work contract requires stable IDs, canonical request
digests, immutable artifact digests, explicit review/release/delivery
generations, and read-model-only Console views
([actors and commands](https://github.com/silentspike/project-sentinel/blob/e85eb67f13beb240b4c4638d3f37d76f219b8463/docs/virtual-company-work-execution.md#L56-L87),
[domain records](https://github.com/silentspike/project-sentinel/blob/e85eb67f13beb240b4c4638d3f37d76f219b8463/docs/virtual-company-work-execution.md#L89-L132),
[recovery](https://github.com/silentspike/project-sentinel/blob/e85eb67f13beb240b4c4638d3f37d76f219b8463/docs/virtual-company-work-execution.md#L316-L331),
[observability](https://github.com/silentspike/project-sentinel/blob/e85eb67f13beb240b4c4638d3f37d76f219b8463/docs/virtual-company-work-execution.md#L348-L365)).
Those records are target contracts; repository search finds no current runtime
types for the complete customer-request, work-item, workbench-invocation,
artifact-manifest, release-candidate, delivery-receipt, and closeout chain.

The required causal path is:

```text
CustomerRequest(request_id, request_digest)
  -> Agreement/Project(workflow_id, project_id, generation)
  -> WorkItem(work_item_id, expected_version)
  -> WorkbenchInvocation(invocation_id, attempt, input_digest)
  -> Agent/tick(agent_id, tick) when simulation participation exists
  -> Gateway/provider(provider_request_id, provider_attempt, request_digest)
  -> DomainEvent(event_id, operation_id, correlation_id, causation_id)
  -> Outbox/transport(message_id, event envelope; NATS or selected Zenoh fanout)
  -> Projection(projection_name, source_event_id, generation, offset)
  -> Artifact(artifact_id, artifact_digest, producer_invocation_id)
  -> QA/release/delivery(qa_run_id, release_id, delivery_id, exact digest)
```

No span creates or advances any node in this chain. A span references the
smallest relevant set of these identifiers. Fan-out uses span links and direct
event causation rather than pretending that one temporal parent owns every
child. Retries retain the stable operation/request identity and use a distinct
attempt and span identity.

### Accepted `CausalContextV1`

This is a propagation record, not a copy of the business aggregates:

| Field | Rule |
|---|---|
| `schema_version` | Exactly `1`; unknown major versions fail closed at mutating boundaries. |
| `request_id`, `request_digest` | Stable idempotency pair; same ID with a different canonical digest is a typed conflict. |
| `correlation_id` | Stable intent/workflow correlation, unchanged across the chain. |
| `causation_event_id` | Direct durable trigger; absent only for an admitted root command. |
| `operation_id`, `attempt` | Stable logical effect plus monotonically identified execution attempt. |
| `project_id`, `workflow_id`, `work_item_id` | References only; authoritative state remains in the owning aggregates. |
| `agent_id`, `tick` | Optional simulation participation; absence is distinct from zero. |
| `invocation_id` | Stable workbench execution reference. |
| `source_generation`, `source_digest` | Exact input/projection/artifact generation used by the operation. |
| `artifact_id`, `artifact_digest` | Optional immutable produced or consumed artifact reference. |
| `qa_run_id`, `release_id`, `delivery_id` | Optional downstream lineage references; no acceptance authority. |
| `trace_id`, `span_id` | Optional derived W3C diagnostic references; never used for CAS, idempotency, or business lookup authority. |

The wire representation must be canonical and size-bounded. Authentication
tokens, authorization decisions, prompts, customer text, tool input/output,
artifact content, arbitrary baggage, and unbounded labels are forbidden.

### Target-architecture conflict and delta

The TOGAF telemetry target deliberately rejects OpenTelemetry and Jaeger as
extra single-host overhead
([decision](https://github.com/silentspike/project-sentinel/blob/e85eb67f13beb240b4c4638d3f37d76f219b8463/docs/architecture/togaf-architecture-guide.html#L1481-L1505)),
describes per-agent spans as debug-only
([span hierarchy](https://github.com/silentspike/project-sentinel/blob/e85eb67f13beb240b4c4638d3f37d76f219b8463/docs/architecture/togaf-architecture-guide.html#L1530-L1546)),
and states that the active metrics path is the polled `:9090` endpoint rather
than an instantiated Zenoh publisher
([transport reality](https://github.com/silentspike/project-sentinel/blob/e85eb67f13beb240b4c4638d3f37d76f219b8463/docs/architecture/togaf-architecture-guide.html#L1570-L1587)).
Its event target already requires unchanged intent correlation and direct
trigger causation
([event contract](https://github.com/silentspike/project-sentinel/blob/e85eb67f13beb240b4c4638d3f37d76f219b8463/docs/architecture/togaf-architecture-guide.html#L2106-L2117)).

The proposed TOGAF delta is therefore narrow:

1. Keep `tracing`, atomic metrics, and the authoritative event store.
2. Preserve the M0 rejection of an OTel SDK/bridge/Collector, external backend,
   and second telemetry buffer/store.
3. Allow only a minimal Sentinel-owned `traceparent` boundary adapter in M0;
   broader W3C interoperability is POST_M0 and decision-gated.
4. State explicitly that diagnostic trace/span IDs are derived references,
   while Sentinel IDs, digests, events, artifacts, and generations remain
   authority.
5. Add bounded source context, redaction, cardinality, sampling, retention, and
   typed telemetry-loss counters.
6. Keep SpanExport debug-only and per-agent/tick hot-path spans disabled.

This study does not edit TOGAF. The main-session owner must review and apply
any accepted target change after implementation ownership is approved.

### Existing owners and non-overlap

| Owner | Current contract | #718 delta or boundary |
|---|---|---|
| [#34](https://github.com/silentspike/project-sentinel/issues/34) | Closed telemetry foundation. | Delivered history, not proof that `TraceContext` is runtime-integrated or complete. |
| [#140](https://github.com/silentspike/project-sentinel/issues/140) | Closed NATS/Zenoh integration history. | Transport history does not replace one canonical context envelope. |
| [#288](https://github.com/silentspike/project-sentinel/issues/288) | Closed gateway pipeline. | Gateway owns HTTP/provider propagation points; this study does not mutate them. |
| [#296](https://github.com/silentspike/project-sentinel/issues/296) | Closed MITM observability and redaction work. | Its private-data boundary constrains trace attributes and export. |
| [#432](https://github.com/silentspike/project-sentinel/issues/432) | Closed dashboard event push. | Dashboard is a derived consumer, never causal authority. |
| [#556](https://github.com/silentspike/project-sentinel/issues/556) | Open cluster GA and cluster observability. | Cross-node trace federation is POST_M0 and remains subordinate to canonical cluster events. |
| [#650](https://github.com/silentspike/project-sentinel/issues/650) | M0 single-node product acceptance. | Requires reconstructable customer work; does not require an external trace backend. |
| [#693](https://github.com/silentspike/project-sentinel/issues/693) | Verified M0 work contract. | Defines authority and identity invariants used here. |
| [#694](https://github.com/silentspike/project-sentinel/issues/694) | Workbench execution and artifacts. | Owns invocation/effect/artifact causal references and material spans. |
| [#695](https://github.com/silentspike/project-sentinel/issues/695) | Customer/project workflow. | Owns request/workflow/work-item identity and state transitions. |
| [#696](https://github.com/silentspike/project-sentinel/issues/696) | QA, release, delivery, and closeout. | Owns exact-digest downstream lineage and release/delivery spans. |
| [#706](https://github.com/silentspike/project-sentinel/issues/706) | Supervision, readiness, repair, and quarantine. | Owns fail-closed startup for invalid required observability policy, not telemetry delivery availability. |
| [#722](https://github.com/silentspike/project-sentinel/issues/722) | Whole-product backup and recovery target. | Backs up authoritative events/configuration; derived short-lived traces are optional. |
| [#731](https://github.com/silentspike/project-sentinel/issues/731) | Event-truth correction program. | Parent for the event envelope, transport outcome, projection, and retention owners below. |
| [#732](https://github.com/silentspike/project-sentinel/issues/732) | Canonical event envelope. | Primary owner for `CausalContextV1` and producer validation. |
| [#733](https://github.com/silentspike/project-sentinel/issues/733) | Outbox/inbox and publish outcomes. | Owns context preservation and typed delivery/drop outcomes across NATS/Zenoh. |
| [#734](https://github.com/silentspike/project-sentinel/issues/734) | Projection catalog and generations. | Owns source-event/generation lineage in read models. |
| [#736](https://github.com/silentspike/project-sentinel/issues/736) | Retention, frontiers, and recovery. | Owns durable causal-event retention; trace retention cannot weaken it. |
| [#758](https://github.com/silentspike/project-sentinel/issues/758) | Bounded Sentinel-native causal observability and reconstruction. | Owns source policy, material spans, atomic loss counters, minimal boundary adapter, and read-only reconstruction without OTel/Collector in M0. |
| [#705](https://github.com/silentspike/project-sentinel/issues/705) | Dependency necessity and ownership. | Records that any OTel SDK/bridge/Collector, broader W3C interop, or external backend remains a separate POST_M0 necessity decision; nothing is preapproved. |
| [#656](https://github.com/silentspike/project-sentinel/issues/656) | Dependency upgrade operations. | Owns upgrades only after dependency approval. |

## OSS landscape

### Reproducible inventory

Repository state and GitHub's latest-release endpoint were read on 2026-07-29.
Multi-crate repositories may report the most recently published component
rather than a platform-wide release. "Deep" means pinned implementation,
tests, failures, security/license material, and operations were reviewed below.

| Candidate | Pin | License | Pin date | Latest release at review | Score / 24 | Deep | Disposition |
|---|---|---|---:|---|---:|---:|---|
| [Rust `tracing`](https://github.com/tokio-rs/tracing/tree/d9d4c542de10f5d3a711b7a45ffe450fd0666437) | `d9d4c542de10f5d3a711b7a45ffe450fd0666437` | MIT | 2026-05-30 | `tracing-appender-0.2.5` | 19 | No | Keep existing in-process instrumentation; it is not a propagation protocol or trace store. |
| [OpenTelemetry Rust](https://github.com/open-telemetry/opentelemetry-rust/tree/0e78170d712e5046b8ed93b6f99b2b003af15cd7) + [`tracing-opentelemetry`](https://github.com/tokio-rs/tracing-opentelemetry/tree/1d5422f1f37932fd65e434da618b305d4c94ee9c) | `0e78170...` / `1d5422f...` | Apache-2.0 / MIT | 2026-07-22 / 2026-05-19 | `opentelemetry-semantic-conventions-0.32.1` / `v0.33.0` | 18 | Yes | Source reference only; SDK/bridge adoption is POST_M0 and decision-gated. |
| [OpenTelemetry Collector](https://github.com/open-telemetry/opentelemetry-collector/tree/259f177f8c1aea6f1a98c0a23ef1817c88afeb92) + [Contrib](https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/baf8c2342f650d0b36bbd5dec5ba7fb763e65391) | `259f177...` / `baf8c23...` | Apache-2.0 | 2026-07-28 | `v0.157.0` | 21 | Yes | Source reference for explicit loss/resource contracts; reject M0 integration. |
| [Grafana Tempo](https://github.com/grafana/tempo/tree/bb8b3766272f75b4d09481b86d38c8d8b4b2e3f2) | `bb8b376...` | AGPL-3.0 | 2026-07-28 | `v3.0.2` | 14 | Yes | Reject M0 store/runtime addition. |
| [Vector](https://github.com/vectordotdev/vector/tree/f54459dbf288badc902d291c66e5a8a06fa92b6b) | `f54459d...` | MPL-2.0 | 2026-07-28 | `vdev-v0.3.9` reported by latest endpoint | 19 | Yes | Reject runtime addition; port explicit buffer/cardinality contract ideas. |
| [OpenObserve](https://github.com/openobserve/openobserve/tree/17ef03b8d6cf4e0764d593e8acd8381e90203719) | `17ef03b...` | AGPL-3.0 | 2026-07-29 | `v0.91.4` | 14 | Yes | Reject integrated backend and second data authority. |
| [Jaeger](https://github.com/jaegertracing/jaeger/tree/fc6d11f19d2ef2624163562b7e765b2265f68f6d) | `fc6d11f...` | Apache-2.0 | 2026-07-28 | `v2.20.0` | 16 | No | Mature backend, but duplicates an external collection/store/query boundary rejected for M0. |
| [SigNoz](https://github.com/SigNoz/signoz/tree/bca23708621a1a7008ddbf75a9e473b428bd05dc) | `bca2370...` | MIT core, separate enterprise license | 2026-07-29 | `v0.134.0` | 14 | No | Broad ClickHouse-backed platform; reject M0 operational and authority overlap. |

Score detail:

| Candidate | Causal | Tests | Failure | Ops | Security | License | Language/deps | Local-first/1:n |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| `tracing` | 2 | 3 | 1 | 2 | 2 | 3 | 3 | 3 |
| OTel Rust + bridge | 3 | 3 | 1 | 2 | 2 | 3 | 2 | 2 |
| OTel Collector | 3 | 3 | 3 | 3 | 3 | 3 | 1 | 2 |
| Tempo | 3 | 3 | 3 | 2 | 2 | 1 | 0 | 0 |
| Vector | 2 | 3 | 3 | 3 | 3 | 2 | 1 | 2 |
| OpenObserve | 3 | 3 | 3 | 2 | 2 | 1 | 0 | 0 |
| Jaeger | 3 | 3 | 3 | 2 | 2 | 3 | 0 | 0 |
| SigNoz | 3 | 3 | 2 | 2 | 2 | 2 | 0 | 0 |

### Shortlist rationale

The five deep candidates cover distinct mechanisms instead of five brands:

1. OTel Rust plus the `tracing` bridge covers W3C context, spans, links,
   sampling, and SDK queue failure.
2. OTel Collector core plus Contrib covers protocol routing, persistent
   export, redaction processors, resource limits, and tail-sampling failure.
3. Tempo covers a purpose-built trace store, query, ingestion limits, WAL,
   compaction, retention, and authentication defaults.
4. Vector covers a general local-first pipeline with explicit disk buffers,
   overflow policy, transformations, and cardinality limits.
5. OpenObserve covers an integrated local observability backend with WAL,
   recovery, query, redaction, retention, and authorization boundaries.

`tracing` is already present and receives a focused retain/reject-boundary
review. Jaeger substantially overlaps OTel Collector plus a trace backend.
SigNoz substantially overlaps the integrated backend role. Their exclusion
from the deep five is mechanism de-duplication, not a claim that they are
immature.

### Source-backed rejection and retain checks

**Rust `tracing`.** Its non-blocking appender explicitly chooses backpressure
or loss at capacity and exposes a dropped-line counter
([implementation](https://github.com/tokio-rs/tracing/blob/d9d4c542de10f5d3a711b7a45ffe450fd0666437/tracing-appender/src/non_blocking.rs#L49-L78),
[configuration](https://github.com/tokio-rs/tracing/blob/d9d4c542de10f5d3a711b7a45ffe450fd0666437/tracing-appender/src/non_blocking.rs#L176-L205)).
Tests exercise both backpressure and lossy behavior
([tests](https://github.com/tokio-rs/tracing/blob/d9d4c542de10f5d3a711b7a45ffe450fd0666437/tracing-appender/src/non_blocking.rs#L372-L452)).
This is strong in-process instrumentation but supplies no W3C transport
propagation, durable queue, query store, or business causality. Keep it and
make its loss policy explicit.

**Jaeger.** The pinned v2 code consumes OTel Collector component contracts and
represents traces by shared trace ID
([feature-gate integration](https://github.com/jaegertracing/jaeger/blob/fc6d11f19d2ef2624163562b7e765b2265f68f6d/internal/featuregate/renamed.go#L1-L73),
[UI model](https://github.com/jaegertracing/jaeger/blob/fc6d11f19d2ef2624163562b7e765b2265f68f6d/internal/uimodel/model.go#L1-L75)).
Its conversion tests cover probabilistic and rate-limited remote sampling
([tests](https://github.com/jaegertracing/jaeger/blob/fc6d11f19d2ef2624163562b7e765b2265f68f6d/internal/converter/thrift/jaeger/sampling_to_domain_test.go#L14-L103)).
The Apache-2.0 repository has a detailed disclosure policy
([security](https://github.com/jaegertracing/jaeger/blob/fc6d11f19d2ef2624163562b7e765b2265f68f6d/SECURITY.md)).
It is a credible backend, but adding it would duplicate the selected
instrumentation/collector mechanisms and create a new trace store. Reject for
M0; reconsider only under the POST_M0 trace-backend decision gate.

**SigNoz.** Trace-detail endpoints are protected by view-access middleware
([routes](https://github.com/SigNoz/signoz/blob/bca23708621a1a7008ddbf75a9e473b428bd05dc/pkg/apiserver/signozapiserver/tracedetail.go#L1-L66)),
and middleware tests preserve typed authorization failures
([test](https://github.com/SigNoz/signoz/blob/bca23708621a1a7008ddbf75a9e473b428bd05dc/pkg/http/middleware/response_test.go#L1-L48)).
Its root license states an MIT core with separately licensed enterprise
directories
([license](https://github.com/SigNoz/signoz/blob/bca23708621a1a7008ddbf75a9e473b428bd05dc/LICENSE)),
and a disclosure policy exists
([security](https://github.com/SigNoz/signoz/blob/bca23708621a1a7008ddbf75a9e473b428bd05dc/SECURITY.md)).
The broad query/UI/storage platform is useful when a general observability
product is the goal; it is not a thin causal-correlation mechanism for
Sentinel M0. Reject runtime integration.

## Pinned deep reviews

### 1. OpenTelemetry Rust and `tracing-opentelemetry`

**Mechanisms.** The SDK's W3C propagator validates version, lower-case IDs,
trace flags, and span validity during extraction, then injects `traceparent`
and `tracestate`
([source](https://github.com/open-telemetry/opentelemetry-rust/blob/0e78170d712e5046b8ed93b6f99b2b003af15cd7/opentelemetry-sdk/src/propagation/trace_context.rs#L30-L152)).
The sampler supports parent-based and deterministic trace-ID-ratio decisions
([source](https://github.com/open-telemetry/opentelemetry-rust/blob/0e78170d712e5046b8ed93b6f99b2b003af15cd7/opentelemetry-sdk/src/trace/sampler.rs#L142-L255)).
The bridge exposes parent context and links to `tracing` spans; propagation
tests exercise remote parents, sampling state, baggage composition, and
invalid late-parent mutation
([tests](https://github.com/tokio-rs/tracing-opentelemetry/blob/1d5422f1f37932fd65e434da618b305d4c94ee9c/tests/trace_state_propagation.rs#L1-L180)).

**Failure behavior.** The async batch span processor uses a bounded channel.
When it cannot enqueue, it drops the span, increments a process-lifetime
counter, and emits a warning
([source](https://github.com/open-telemetry/opentelemetry-rust/blob/0e78170d712e5046b8ed93b6f99b2b003af15cd7/opentelemetry-sdk/src/trace/span_processor_with_async_runtime.rs#L85-L160)).
Tests cover force-flush success and exporter timeout
([tests](https://github.com/open-telemetry/opentelemetry-rust/blob/0e78170d712e5046b8ed93b6f99b2b003af15cd7/opentelemetry-sdk/src/trace/span_processor_with_async_runtime.rs#L564-L625)).
This queue is not durable; a successful business operation cannot depend on
the span surviving.

**Security, license, and operations.** W3C context fields are interoperable but
untrusted input and must never grant authority. Baggage is especially
unsuitable for secrets or unrestricted customer metadata. OTel Rust is
Apache-2.0, the bridge is MIT, and both are actively maintained at the pins.
OTel Rust has no root security-policy file at the pin, but its issue template
routes vulnerability reports to private GitHub advisories
([source](https://github.com/open-telemetry/opentelemetry-rust/blob/0e78170d712e5046b8ed93b6f99b2b003af15cd7/.github/ISSUE_TEMPLATE/config.yml#L1-L10)).
Their process overhead is smaller than a backend but still adds crates,
configuration, propagation adapters, sampling policy, and exporter lifecycle.

**Sentinel decision.** Do not add the OTel SDK or `tracing-opentelemetry` in
M0. Reimplement only a minimal bounded `traceparent` boundary adapter under
#758, keep `CausalContextV1` authoritative, and reject `tracestate`, baggage,
and trace IDs as operation IDs, event causation, or release evidence. Any
broader interoperability is POST_M0 and requires a fresh #705 decision.

### 2. OpenTelemetry Collector core and Contrib

**Mechanisms.** Exporter helper provides bounded queues, retry, timeouts, and an
optional storage-backed persistent queue. Queue insertion can reject data
before exporter retry; persistent queues resume after restart and do not
preserve authentication-extension context
([queue contract](https://github.com/open-telemetry/opentelemetry-collector/blob/259f177f8c1aea6f1a98c0a23ef1817c88afeb92/exporter/exporterhelper/README.md#L20-L91)).
The source defines queue-full as a typed error and distinguishes memory from
persistent configuration
([queue](https://github.com/open-telemetry/opentelemetry-collector/blob/259f177f8c1aea6f1a98c0a23ef1817c88afeb92/exporter/exporterhelper/internal/queue/queue.go#L30-L105),
[persistent queue](https://github.com/open-telemetry/opentelemetry-collector/blob/259f177f8c1aea6f1a98c0a23ef1817c88afeb92/exporter/exporterhelper/internal/queue/persistent_queue.go#L50-L120)).
Persistent-queue tests cover full-queue and shutdown preservation
([tests](https://github.com/open-telemetry/opentelemetry-collector/blob/259f177f8c1aea6f1a98c0a23ef1817c88afeb92/exporter/exporterhelper/internal/queuebatch/queue_batch_test.go#L100-L221)).

**Failure and resource behavior.** The memory limiter returns typed refusal for
traces, metrics, logs, and profiles and records accepted/refused telemetry
([source](https://github.com/open-telemetry/opentelemetry-collector/blob/259f177f8c1aea6f1a98c0a23ef1817c88afeb92/processor/memorylimiterprocessor/memorylimiter.go#L44-L109)).
The Contrib attributes processor supports ordered insert/update/upsert/delete/
hash/extract actions
([source](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/baf8c2342f650d0b36bbd5dec5ba7fb763e65391/processor/attributesprocessor/config.go#L15-L38)).
These exact-key operations are defense in depth, not proof that arbitrary
prompt, tool, or artifact payloads are safe.

**Tail-sampling limit.** The tail sampler can decide from latency, status,
attributes, rates, bytes, flags, and full trace data
([configuration](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/baf8c2342f650d0b36bbd5dec5ba7fb763e65391/processor/tailsamplingprocessor/config.go#L20-L67)).
Its tests explicitly reproduce races where a trace is dropped too early and
exercise bounded trace-map behavior
([tests](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/baf8c2342f650d0b36bbd5dec5ba7fb763e65391/processor/tailsamplingprocessor/processor_test.go#L420-L520)).
Therefore tail sampling cannot decide whether authoritative M0 evidence
exists.

**Security, license, and operations.** Both repositories are Apache-2.0. The
core repository has no root disclosure-policy file at the pin, but its pinned
security guide requires validated configuration, least privilege, encrypted
authenticated channels, and no sensitive health exposure by default
([guide](https://github.com/open-telemetry/opentelemetry-collector/blob/259f177f8c1aea6f1a98c0a23ef1817c88afeb92/docs/security-best-practices.md#L1-L85)).
A collector creates another process, configuration, listener, queue, storage,
metrics, upgrade, and disk-budget surface. Persistent export may be justified
for approved remote export, but its auth-context caveat requires
authentication at the destination rather than assuming queued request
identity survives.

**Sentinel decision.** Reject Collector integration for M0. Its explicit
refusal, queue, resource, and loss semantics inform Sentinel-owned source
policy and counters, but M0 adds no second process, listener, OTLP queue,
persistent telemetry buffer, or export lifecycle. Any future Collector
proposal is POST_M0 and requires a fresh #705 necessity decision.

### 3. Grafana Tempo

**Mechanisms.** Tempo is a purpose-built trace ingestion, block storage,
compaction, retention, and query system. Ingestion applies local or
cluster-wide byte-rate limits and returns `ResourceExhausted` while recording
discarded spans
([source](https://github.com/grafana/tempo/blob/bb8b3766272f75b4d09481b86d38c8d8b4b2e3f2/modules/distributor/distributor.go#L395-L434)).
Tests cover under-limit, burst, and rejected batches
([tests](https://github.com/grafana/tempo/blob/bb8b3766272f75b4d09481b86d38c8d8b4b2e3f2/modules/distributor/distributor_test.go#L1988-L2076)).

**Persistence, retention, and failure behavior.** The retention loop marks expired blocks
compacted, later clears them, records errors, and updates block lists
([source](https://github.com/grafana/tempo/blob/bb8b3766272f75b4d09481b86d38c8d8b4b2e3f2/tempodb/retention.go#L17-L138)).
Tests exercise local WAL/block storage and two-stage retention deletion
([tests](https://github.com/grafana/tempo/blob/bb8b3766272f75b4d09481b86d38c8d8b4b2e3f2/tempodb/retention_test.go#L28-L84)).
This is strong derived-trace storage, not durable business causality.

**Security, license, and operations.** Authentication and multitenancy default
to false in the application configuration
([source](https://github.com/grafana/tempo/blob/bb8b3766272f75b4d09481b86d38c8d8b4b2e3f2/cmd/tempo/app/config.go#L42-L100)).
The repository is AGPL-3.0 and has no root project security policy at the pin.
Even single-binary mode adds a WAL, block store, compactor, query surface,
ingestion limits, retention jobs, listener security, and a new incident domain.

**Sentinel decision.** Reject for M0. Tempo may be reconsidered POST_M0 only
after an approved trace-store need, data-flow threat model, license review,
resource budget, retention owner, backup boundary, and proof that event and
artifact truth remain authoritative.

### 4. Vector

**Mechanisms.** Vector receives and emits OTLP and can transform telemetry. Its
buffer contract makes `Block`, `DropNewest`, and staged `Overflow` explicit
([source](https://github.com/vectordotdev/vector/blob/f54459dbf288badc902d291c66e5a8a06fa92b6b/lib/vector-buffers/src/lib.rs#L49-L79)).
Disk-v2 buffers are size-bounded and get an owned data directory
([source](https://github.com/vectordotdev/vector/blob/f54459dbf288badc902d291c66e5a8a06fa92b6b/lib/vector-buffers/src/variants/disk_v2/mod.rs#L340-L421)).
The OTLP sink warns about JSON batching that receivers would reject
([source](https://github.com/vectordotdev/vector/blob/f54459dbf288badc902d291c66e5a8a06fa92b6b/src/sinks/opentelemetry/mod.rs#L20-L123)).

**Cardinality, failure behavior, and tests.** The cardinality limiter tracks per-metric/tag values,
reports tracked, dropped, or untracked outcomes, and can bound tracked keys
([source](https://github.com/vectordotdev/vector/blob/f54459dbf288badc902d291c66e5a8a06fa92b6b/src/transforms/tag_cardinality_limit/mod.rs#L40-L92)).
Tests prove event and tag drops once value limits are exceeded
([tests](https://github.com/vectordotdev/vector/blob/f54459dbf288badc902d291c66e5a8a06fa92b6b/src/transforms/tag_cardinality_limit/tests.rs#L162-L239)).

**Security, license, and operations.** Vector is MPL-2.0 and has a detailed
security policy. It adds a large Rust binary, its own topology and
configuration language, disk buffers, listeners, transforms, health checks,
and upgrade surface. Its transformation power is not a substitute for
source-side data minimization.

**Sentinel decision.** Reject runtime integration for M0. Port the explicit
source outcome vocabulary and per-key cardinality-budget contract into the
Sentinel-native #758 observability profile. Reconsider Vector only if a future
POST_M0 multi-source telemetry-routing requirement passes the #705 decision
gate.

### 5. OpenObserve

**Mechanisms.** OpenObserve provides integrated trace ingestion, WAL, query,
retention, and UI/API surfaces. The WAL compresses each entry, stores a CRC and
length, and optionally calls `sync_data`
([writer](https://github.com/openobserve/openobserve/blob/17ef03b8d6cf4e0764d593e8acd8381e90203719/src/wal/src/writer.rs#L115-L190)).
The reader checks length and CRC and returns typed mismatches
([reader](https://github.com/openobserve/openobserve/blob/17ef03b8d6cf4e0764d593e8acd8381e90203719/src/wal/src/reader.rs#L145-L220)).
Reader tests exercise WAL round trips, entry-length reporting, and advancing
file position
([tests](https://github.com/openobserve/openobserve/blob/17ef03b8d6cf4e0764d593e8acd8381e90203719/src/wal/src/reader.rs#L360-L434));
typed mismatch tests cover length and checksum error variants
([tests](https://github.com/openobserve/openobserve/blob/17ef03b8d6cf4e0764d593e8acd8381e90203719/src/wal/src/errors.rs#L95-L117)).

**Recovery failure behavior.** Ingester replay logs and skips unreadable,
length-mismatched, checksum-mismatched, and undecodable entries while other WAL
errors fail the replay
([source](https://github.com/openobserve/openobserve/blob/17ef03b8d6cf4e0764d593e8acd8381e90203719/src/ingester/src/wal.rs#L145-L209)).
This is an availability-oriented telemetry choice. It is unacceptable as the
only record of a required customer effect or release decision.

**Redaction boundary.** Trace ingestion contains field-pattern redaction, but
it is compiled behind `vectorscan` and calls the optional
`o2_enterprise` pattern manager. Pattern-manager or processing errors are
logged and ingestion continues
([source](https://github.com/openobserve/openobserve/blob/17ef03b8d6cf4e0764d593e8acd8381e90203719/src/core/src/traces/mod.rs#L870-L925),
[feature boundary](https://github.com/openobserve/openobserve/blob/17ef03b8d6cf4e0764d593e8acd8381e90203719/src/core/Cargo.toml#L13-L42)).
This cannot be treated as an OSS, fail-closed privacy control for Sentinel.

**Security, license, and operations.** The repository is AGPL-3.0 and has a
coordinated disclosure policy. It adds an integrated database/query/API/UI
platform and an enterprise boundary. Its WAL durability is useful evidence,
but the platform duplicates Sentinel event, projection, dashboard, identity,
retention, and backup responsibilities.

**Sentinel decision.** Reject integration. Keep the CRC/length/replay analysis
as a reminder that derived telemetry may skip corrupt records only when the
authoritative event/artifact path remains intact and the loss is observable.

## Mechanism comparison

Abbreviations: `S` is Sentinel today, `OR` is OTel Rust/bridge, `OC` is OTel
Collector, `T` is Tempo, `V` is Vector, and `O2` is OpenObserve.

| Mechanism | S | OR | OC | T | V | O2 | 1:n and deterministic fit | Security / failure / integration boundary |
|---|---|---|---|---|---|---|---|---|
| Span/log/metric context | Local `tracing`; prototype context unused | W3C parent/link propagation | OTLP routing and processors | Full trace ingestion/query | OTLP routing/transforms | Full integrated backend | Good only when IDs reference authoritative records | Reject untrusted context as authority; use owned envelope and minimal boundary adapters |
| Stable business IDs | Event/correlation/causation/operation plus request digest on provider leg | Trace/span IDs only | Transports telemetry IDs | Trace IDs | Event fields/tags | Trace and platform IDs | External IDs cannot replace Sentinel aggregates | `CausalContextV1` remains owned, canonical, and bounded |
| Async queue/retry/restart links | Outbox plus uneven headers; durable provider reservation | Span links available; SDK queue volatile | Bounded/persistent queue, retries, restart | Ingestion/WAL/store | Disk buffer and explicit overflow | WAL with skip-on-corruption replay | Causation and operation attempts stay in event truth | Telemetry drops are typed and observable, never business failure |
| Sampling | No repository-wide span policy | Parent/trace-ratio head sampling | Tail/head processors | Backend ingestion controls | Route/filter transforms | Backend controls | Deterministic head policy is reproducible; tail state is not authority | Always retain material errors locally within bounded policy; no per-tick flood |
| Cardinality | Prometheus labels vary by producer; no shared budget | Arbitrary span attributes possible | Attribute/filter processors | Limits and discarded-span metrics | Per-key limits and drop modes | Backend schema/stream controls | IDs belong in spans/logs, not metric labels | Owned attribute allowlist plus per-key budgets; defense in depth downstream |
| Redaction | Gateway history exists; no trace-wide attribute policy | Propagators do not sanitize app attributes | Exact-key delete/hash and OTTL possible | Operator configuration required | VRL transforms possible | Trace redaction is optional enterprise-coupled and fail-open | Source minimization preserves one clean source | Never emit secrets/private payloads; downstream filtering cannot repair source leakage |
| Retention/aggregation | Events/projections have separate owners; traces absent | Process queue only | Queue storage, no query retention by itself | Block compaction/retention | Buffer retention/routing | WAL/query/retention platform | Durable event retention and bounded structured-log retention remain separate | Diagnostic deletion must not destroy audit/release evidence |
| Backpressure | Outbox/provider paths explicit; telemetry export incomplete | SDK drops on full queue | Reject, block, retry, or persist by config | Rejects over rate/burst | Block/drop/overflow explicit | WAL and ingest limits | Business path must not wait for telemetry | Source loss counters are required; diagnostic sinks cannot fail business readiness |
| Local-first export | Metrics/logs local; no active trace export | Exporter-neutral | Can run node-local and export nowhere | Adds local/remote trace store | Can buffer locally | Adds local backend | M0 keeps local logs/atomic metrics and no export process | No listener, token, cloud endpoint, or second buffer/store in M0 |
| Incident reconstruction | Event store and digests strongest; views incomplete | Diagnostic graph only | Diagnostic routing only | Trace query | Routed telemetry | Integrated query | Event/project/artifact/release chain is authoritative | Console joins by stable refs; sampled traces supplement but never prove completion |

### Cross-cutting constraint matrix

| Concern | Correctness rule | Failure semantics | Performance hypothesis | Maintenance and dependency impact |
|---|---|---|---|---|
| Propagation | Validate canonical version, request digest pair, direct causation, and size before mutation. | Invalid business context is typed and fail-closed; the minimal diagnostic adapter rejects invalid `traceparent`. | Fixed-size parsing at material boundaries should be small; must be measured on target. | Owned envelope plus minimal Sentinel boundary adapter; #732/#733/#758. |
| Instrumentation | Spans reference committed facts and cannot advance state. | Span/log failure leaves business result unchanged and increments bounded source loss/error counters. | Material-boundary spans should avoid tick-budget impact; no per-entity spans. | Existing `tracing` only for M0; any OTel bridge is POST_M0. |
| Queue/export | M0 adds no telemetry queue or exporter. | Sink/drop/refusal outcomes are counted at source; no unbounded memory or disk and no business backpressure. | Avoiding a second buffer/process minimizes M0 runtime overhead; validate native instrumentation on target. | #758 owns native policy; Collector/export remains POST_M0 under #705. |
| Sampling | Sampling never decides whether durable evidence is retained. | Required error/retry/rollback diagnostic records use deterministic policy; tail sampler loss is tolerated. | Head sampling controls hot volume; tail sampling adds memory and delay. | Owned policy; no backend dependency required. |
| Cardinality | Metric labels come from a fixed low-cardinality schema. | Unknown/high-cardinality labels are rejected or converted to non-metric attributes. | Prevents memory and query amplification; exact limits need target tests. | CI schema/checker plus runtime counters under proposed owner. |
| Redaction | Source allowlist excludes content/secrets before serialization. | Invalid policy fails startup; per-record redaction failure drops the diagnostic record, not the business operation. | Pattern scanning of arbitrary content is avoided by not emitting content. | #758 source policy; no downstream Collector is required for M0. |
| Retention | Events/artifacts follow #736; structured diagnostic logs use existing bounded local retention. | Diagnostic pruning cannot cross an event/artifact/release frontier because it is never that frontier. | Existing bounded retention limits disk; exact sizing needs target evidence. | #736 authoritative frontier plus #758 policy; no trace store or trace backup. |
| Reconstruction | One query starts from request/project/release and joins immutable references. | Missing telemetry is shown as missing; it is never interpreted as no operation or success. | Indexed IDs should make targeted reconstruction cheap; validate in owner issue. | #734 read models and Console owner, no second authority. |

## Decisions

Every row has exactly one decision. Alternatives are rejected explicitly.

| Mechanism | Decision | Rationale | Rejected alternatives |
|---|---|---|---|
| Authoritative causal identity | **Port algorithm/contract** | Add bounded `CausalContextV1` to the canonical Sentinel envelope while retaining aggregate/event/artifact ownership. | Reject OTel trace IDs as authority; reject backend-generated identity; reject copying aggregate state into traces. |
| In-process spans/logs | **Configure existing dependency** | `tracing` is already present and supports structured material-boundary instrumentation. | Reject replacing it with another logger; reject per-agent/per-tick exported spans. |
| W3C HTTP propagation | **Reimplement minimal** | A Sentinel-owned boundary adapter accepts only a bounded valid optional `traceparent` reference; broader W3C behavior is POST_M0. | Reject OTel SDK/bridge in M0; reject `tracestate`, baggage, and trace ID as idempotency or causation. |
| NATS/Zenoh/outbox context | **Reimplement minimal** | One owned envelope adapter can remove current Rust/Go/Zenoh asymmetry without another broker or truth store. | Reject ad hoc headers per publisher; reject relying on payload coincidence; reject a telemetry side channel as causal transport. |
| Node-local collection/export | **Keep Sentinel** | Existing structured logs, `tracing`, and atomic metrics provide the M0 diagnostic path without a second process, listener, buffer, or store. | Reject Collector, OTLP queue, Vector, and cloud export for M0; reconsider only through the POST_M0 gate. |
| Trace storage/query | **Reject** | Event, projection, artifact, QA, and release records already own incident truth; M0 does not justify another store. | Reject Tempo, Jaeger, OpenObserve, and SigNoz for M0; reconsider only through the POST_M0 gate. |
| Sampling | **Reimplement minimal** | Owned deterministic policy can keep material failures and sample only non-critical diagnostics without correctness coupling. | Reject in-memory tail sampling as an evidence gate; reject always-on hot tick/entity spans. |
| Cardinality control | **Port algorithm/contract** | Vector's explicit tracked/dropped/untracked and per-key limits map well to an owned attribute/metric schema. | Reject unrestricted IDs as metric labels; reject downstream-only cleanup. |
| Redaction and baggage | **Keep Sentinel** | Extend the existing source-side redaction/data-minimization boundary; do not emit private content. | Reject regex-only downstream redaction; reject OpenObserve's enterprise-coupled fail-open path; reject secrets in baggage. |
| Backpressure/restart | **Port algorithm/contract** | Explicit source `accept`, `sample`, `drop`, `refuse`, and sink-failure counters make overload behavior testable without a second telemetry queue. | Reject implicit defaults, unbounded queues, and telemetry backpressure on company operations. |
| Retention/local-first | **Keep Sentinel** | Existing bounded structured-log retention and atomic metrics preserve privacy and operations fit while #736 owns durable truth. | Reject trace-store retention as event retention, a persistent telemetry buffer, default remote export, and derived-trace backup. |
| Incident reconstruction UI/API | **Integrate** | Extend authoritative projections to expose links and optional trace references; keep Console a read model. | Reject querying a trace backend for business completion; reject screenshots as evidence; reject duplicate workflow indexes. |

## M0 classification and owner routing

| Finding | Class | Evidence and reason | Owner |
|---|---|---|---|
| Customer request through delivery lacks one implemented canonical causal context. | `BLOCKS_M0` | M0 requires auditable workflow and exact-digest delivery; current runtime has only partial event/request identity. | #732, #695, #694, #696 |
| Rust outbox, Go NATS, and selected Zenoh paths preserve different identity sets. | `BLOCKS_M0` | A required async hop can lose direct causation or all envelope metadata, preventing reliable reconstruction. | #733, #732 |
| Current projections do not expose source generation and complete causal references. | `BLOCKS_M0` | Console/API cannot reconstruct authoritative M0 work solely from current views. | #734, #695, #696 |
| Provider request ID/digest and restart behavior are strong but not generalized. | `BLOCKS_M0` | The proven conflict/recovery pattern is required at customer, workbench, artifact, QA, release, and delivery effects. | #695, #694, #696 |
| No shared telemetry attribute, baggage, redaction, or cardinality policy exists. | `M0_HARDENING` | Unbounded/private telemetry can create operational or security failure, but source records can be corrected without external infrastructure. | #758, #296, #706 |
| Material service/effect spans and source loss counters are incomplete. | `M0_HARDENING` | Diagnosis is weaker, but correctness remains in events and artifacts. | #758, #694, #695, #696 |
| OTel SDK/bridge/Collector and broader W3C interoperability are absent. | `POST_M0` | The TOGAF target intentionally uses native logs/metrics and debug-only spans; external interoperability is not required for M0 correctness or acceptance. | #705 decision gate; no approved implementation owner |
| External trace store/query backend is absent. | `POST_M0` | M0 single-node reconstruction can use event/projection/artifact lineage; a second store is not justified. | #556 or future approved owner |
| Cross-node trace federation and tail sampling are absent. | `POST_M0` | Cluster-only scale/diagnostic optimizations cannot expand M0. | #556 |

## Materialized implementation contracts

Bundled ORC review accepted these contracts. The canonical bodies of
[#732](https://github.com/silentspike/project-sentinel/issues/732),
[#733](https://github.com/silentspike/project-sentinel/issues/733), and
[#734](https://github.com/silentspike/project-sentinel/issues/734) now contain
their reciprocal owner deltas; uncovered work is the quality-ready
[#758](https://github.com/silentspike/project-sentinel/issues/758) contract.

### Existing-owner delta A: canonical causal context

**Accepted owners:** #732 primary; #695, #694, and #696 producer/consumer
subcontracts; #733 transport.

**Dependencies:** #693 contract; ordered #732 before #733/#734 consumers;
workflow/workbench/QA schemas from #695/#694/#696.

**Scope and state:**

- Define versioned, canonical, size-bounded `CausalContextV1` and validation
  errors.
- Embed it by reference/value in EventEnvelopeV2 and every mutating command
  envelope; keep aggregate records authoritative.
- Enforce root-only missing causation, stable request-ID/digest pairing,
  deterministic operation identity, distinct attempt identity, exact
  generation/digest links, and no authority from trace IDs.
- Add producer helpers for customer request, workflow/work item, workbench,
  agent/provider, artifact, QA, release, delivery, and closeout.

**Acceptance criteria:**

1. One positive fixture traces a request through every listed boundary and
   reconstructs the same stable IDs and exact digests.
2. Same request ID with another digest, non-root missing causation, unknown
   major schema, oversized context, and mutated generation each fail typed and
   before effect.
3. Retry preserves operation/request identity, increments attempt, and creates
   a new optional span ID.
4. Fan-out/fan-in use direct event causation and explicit links without
   rewriting authority.
5. Existing event history has a documented additive migration/default path;
   unknown historical context is not fabricated.

**Negative criteria:** no private content or auth material; no trace ID as CAS,
idempotency, or aggregate identity; no duplicate workflow/artifact/QA store; no
per-tick requirement.

**Target tests:** unit/property tests for canonical encoding and bounds;
cross-language Rust/Go golden vectors; event-store/outbox restart tests;
single-node customer-work E2E under #650.

**Benchmark:** implementation issue declares M0 product target and measures
envelope encode/decode, event append, and 1 Hz tick impact with sidecars. No
build-server timing and no benchmark in this research issue.

**Rollout/rollback:** additive versioned read support, dual-read during
migration only, then writer activation behind one config generation. Roll back
writer/config while retaining readable additive fields and append-only events.

**TOGAF delta:** add `CausalContextV1`, trace-vs-authority distinction,
transport invariants, and root/direct-causation rules.

### Existing-owner delta B: transport and projection lineage

**Accepted owners:** #733 transport/outcome; #734 projection/generation.

**Dependencies:** accepted delta A and current event-truth ordering under #731.

**Scope and state:**

- Make Rust outbox, Go NATS bridge, and selected Zenoh fanout serialize one
  canonical envelope or lossless equivalent.
- Include correlation, direct causation, operation, request/digest, generation,
  and source-event identity; broker message ID remains transport identity.
- Persist typed publish/drop/retry/dead-letter outcomes without treating
  telemetry export as the event.
- Extend read models with source event, causal references, projection
  generation, and exact digest links needed for reconstruction.

**Acceptance criteria:**

1. Rust, Go/NATS, and Zenoh golden vectors preserve the same context.
2. Crash before/after publish and duplicate delivery converge without duplicate
   effects and retain source identity.
3. Missing/invalid required context fails a mutating consumer closed; a
   read-only diagnostic consumer reports and drops it.
4. Projection rebuild reproduces the same business lineage and new generation
   while preserving source-event references.
5. Console/API reconstruction reports missing optional telemetry as missing,
   never as proof of no work or success.

**Negative criteria:** no broker header is business authority; no event payload
copy into a telemetry store; no skipped poison record advances a mandatory
frontier; no screenshot-only acceptance.

**Target tests:** broker/Zenoh fault injection, process restart, projection
rebuild, poison-envelope negative tests, and #650 single-node reconstruction.

**Benchmark, rollout, rollback, TOGAF:** same target-only measurement rule;
versioned envelope compatibility; rollback consumers before writers; add the
transport/outcome and projection-generation contract to TOGAF.

### New uncovered contract: #758 bounded Sentinel-native causal observability

**Materialized owner:** [#758](https://github.com/silentspike/project-sentinel/issues/758),
child contract under #659. It owns native material-boundary instrumentation,
source policy and counters, the minimal `traceparent` boundary adapter, and the
read-only reconstruction surface.

**Dependencies:** accepted #732/#733/#734 deltas plus #694/#695/#696 producer
and authority schemas. #706 owns readiness/quarantine and #736 owns durable
evidence retention. #705 is only a POST_M0 dependency decision gate; #758 has
no new OTel/Collector dependency.

**Data and state:**

- Versioned `ObservabilityPolicyV1` with material-boundary and attribute
  allowlists, per-key byte/value/cardinality bounds, deterministic sampling,
  source redaction, existing local log-retention limits, and typed loss reasons.
- `TelemetryOutcomeV1` atomic counters for accepted, sampled, policy-dropped,
  redaction-dropped, cardinality-refused, sink-failed, and restart-reset
  diagnostics without customer-payload labels.
- Optional derived `trace_id`/`span_id` references attached to authoritative
  read models; no span payload or mutable business state is copied into them.
- `CausalReconstructionV1` joins authoritative event, projection-generation,
  artifact, QA, release, and delivery references and labels missing diagnostics
  explicitly.
- Existing structured logs, `tracing`, and atomic metrics are the M0 path.
  SpanExport is debug-only. There is no Collector, OTLP queue, trace store, or
  second telemetry buffer/control plane.

**Acceptance criteria:**

1. Instrument customer admission, workflow transition, workbench invocation
   and external effect, gateway/provider attempt, event/outbox publish,
   projection update, artifact commit, QA, release, and delivery at material
   boundaries.
2. Every diagnostic record carries only allowlisted bounded references; IDs
   never become Prometheus labels and routine tick/entity spans stay disabled.
3. Invalid policy, unknown required field, forbidden attribute, oversized
   value, or unbounded cardinality fails closed before emission.
4. Sink failure, scrape absence, log backpressure, counter saturation, and
   process restart leave business operations and authoritative evidence correct
   while exposing typed source loss counters.
5. Secret/private-content fixtures produce zero leaked bytes in logs, spans,
   metrics, and public evidence.
6. Deterministic sampling retains configured material error, retry,
   compensation, rollback, and release-failure diagnostics within declared
   bounds.
7. The minimal boundary adapter accepts only supported valid optional
   `traceparent`; `tracestate`, baggage, malformed IDs, and authority confusion
   fail closed.
8. Reconstruction starts from request/project/release identity, returns
   authoritative links, and labels missing telemetry honestly before and after
   projection rebuild.
9. Dependency/config/process/listener/storage scans prove M0 has no OTel
   SDK/bridge/Collector, OTLP queue, external backend, persistent telemetry
   buffer, or duplicate business index.
10. The issue-declared single-node target proves customer-work correctness,
    1 Hz stability, bounded resources, restart, reconstruction, and rollback.

**Negative criteria:** no OTel SDK/bridge/Collector or broader W3C interop in
M0; no trace backend or second telemetry buffer/store; no telemetry authority;
no prompt, customer text, tool payload, artifact content, credential, arbitrary
baggage, or unbounded label/value/cardinality; no missing-telemetry success; no
per-agent/per-tick export; no build-server/upstream benchmark evidence.

**Target-runtime tests and benchmarks:** unit/property/config, source-redaction,
cardinality, sampling, minimal `traceparent`, sink-failure, restart, projection
rebuild, and full-chain reconstruction tests precede a snapshot-scoped
single-node enabled/disabled matrix. On that declared product target, compare
tick and customer-work latency, CPU, RSS, log bytes, metric-series count,
reconstruction latency, source drops, and restart recovery. Correctness,
privacy, one-authority reconstruction, and bounded resources gate adoption.

**Rollout and rollback:** ship schemas and deterministic tests, then native
material-boundary instrumentation behind one immutable policy generation, then
the read-only reconstruction surface. Roll back to the prior policy,
binaries, and configuration while retaining additive causal fields and every
authoritative record. Any OTel/Collector/backend experiment is a separate
POST_M0 issue and decision.

**TOGAF delta:** add `CausalContextV1`, bounded source-side
attribute/redaction/cardinality/sampling policy, typed loss counters, existing
`tracing` plus atomic metrics as the M0 path, SpanExport as debug-only,
authoritative reconstruction references, and the prohibition on a second
telemetry buffer/store. Keep OTel SDK/bridge/Collector, broader W3C
interoperability, and external backends POST_M0.

### POST_M0 interoperability and backend decision gate

Do not create an implementation issue unless all conditions are met:

1. #556 or another live owner demonstrates an interoperability, export, or
   reconstruction/query need that #732/#733/#734/#758 cannot satisfy.
2. #705 approves the dependency/runtime necessity and #656 owns upgrades.
3. Security approves listener/authentication, tenant isolation, private-data
   flow, and disclosure policy.
4. Operations owns storage, compaction, retention, backup exclusion/inclusion,
   resource budget, upgrades, and rollback.
5. A dependency proposal compares the exact OTel SDK/bridge/Collector or
   backend surface against the minimal owned boundary; a backend comparison
   evaluates at least Tempo and Jaeger. Upstream benchmarks remain hypotheses.
6. The accepted adapter/export/backend remains derived and cannot advance
   workflow, QA, release, delivery, or customer acceptance.

## Live materialization and owner acknowledgement

The owner-body marker plus a fresh successful Issue Quality Gate is the
reciprocal acknowledgement for each accepted contract. #705 is a necessity
input only, not an adoption approval.

| Issue | Materialized contract | Live labels | Body SHA-256 | Fresh quality gate |
|---|---|---|---|---|
| [#718](https://github.com/silentspike/project-sentinel/issues/718) | Accepted decision, reciprocal routing, and AC-5/6/7 owner acknowledgement | `status:in-progress`, `quality:ready`, `type:docs`, `prio:high`, `size:XL`, `scope:full`, `comp:daemon`, `comp:runtime` | `ff45c1eb2433834c2f99ffed1a39901ddcef9fad6cea2bede94231b8e4bbb499` | [PASS run 30451959427](https://github.com/silentspike/project-sentinel/actions/runs/30451959427) |
| [#732](https://github.com/silentspike/project-sentinel/issues/732) | `CausalContextV1`, event-envelope producer validation, and authority rules | `status:blocked`, `quality:ready`, `type:feature`, `prio:high`, `size:XL`, `scope:full`, `comp:daemon`, `comp:runtime` | `ad4c8992a274111103f01d56769de97aa566404b3bab0ed45d17d34aeb434e58` | [PASS run 30451840446](https://github.com/silentspike/project-sentinel/actions/runs/30451840446) |
| [#733](https://github.com/silentspike/project-sentinel/issues/733) | Lossless context transport and typed delivery outcomes | `status:blocked`, `quality:ready`, `type:bug`, `prio:high`, `size:XL`, `scope:full`, `comp:daemon`, `comp:runtime` | `bf4bf1bc5290a690cdf5f2badb3a249ff7130d423a84a2961c408d108e38f1cd` | [PASS run 30451884086](https://github.com/silentspike/project-sentinel/actions/runs/30451884086) |
| [#734](https://github.com/silentspike/project-sentinel/issues/734) | Source-event/generation causal reconstruction fields | `status:blocked`, `quality:ready`, `type:feature`, `prio:high`, `size:XL`, `scope:full`, `comp:daemon`, `comp:dashboard` | `08caa021b8b4263ec6d1ac9236205c92040d59b12fe47db673a25ee36a826a8a` | [PASS run 30451914824](https://github.com/silentspike/project-sentinel/actions/runs/30451914824) |
| [#758](https://github.com/silentspike/project-sentinel/issues/758) | Bounded Sentinel-native policy, source counters, minimal adapter, and reconstruction API | `status:blocked`, `quality:ready`, `type:feature`, `prio:high`, `size:XL`, `scope:full`, `comp:daemon`, `comp:runtime` | `c9aaab0e9fb90bf3c769caf46a0f3c7aad41f536cd02b22eb069f4113ac0207b` | [PASS run 30451803253](https://github.com/silentspike/project-sentinel/actions/runs/30451803253) |
| [#705](https://github.com/silentspike/project-sentinel/issues/705) | POST_M0 necessity gate; no dependency or runtime adoption preapproved | `status:blocked`, `quality:ready`, `type:deps`, `prio:high`, `size:XL`, `scope:full`, `dependencies` | `0810f0b36891bacdff9aadbbc3dd99fe5052cf6967c593b4ab3b6cb2d1325f9f` | [PASS run 30452081230](https://github.com/silentspike/project-sentinel/actions/runs/30452081230) |

Hashes above use the reproducible live-body command:

```text
gh issue view ISSUE --json body --jq .body | sha256sum
```

## Acceptance-criteria mapping

| Criterion | Evidence in this study | State at REVIEW_READY |
|---|---|---|
| AC-1 | Sentinel source/test/runtime-contract map, M0 path, TOGAF conflict/delta, and live owner table. | Satisfied for research review. |
| AC-2 | Eight pinned candidates, eight-factor rubric, scores, shortlist rationale, and source-backed exclusions. | Satisfied. |
| AC-3 | Five pinned deep reviews cover source, tests, failures, persistence/recovery, security, license, and operations. | Satisfied. |
| AC-4 | Mechanism and cross-cutting matrices cover correctness, failure, deterministic/1:n fit, performance hypotheses, security, maintenance, dependency, and boundary. | Satisfied. |
| AC-5 | Decision table has exactly one decision for each of twelve mechanisms and explicit rejected alternatives; bundled ORC review accepted the Sentinel-native M0 direction. | Satisfied. |
| AC-6 | #732/#733/#734 contain complete reciprocal deltas; uncovered work is quality-ready #758; #705 records POST_M0 necessity input only. Live hashes, labels, and fresh PASS runs are above. | Satisfied. |
| AC-7 | Every accepted finding is classified and acknowledged by a canonical owner body; OTel/Collector/backend absence is explicitly POST_M0. | Satisfied. |
| AC-8 | This one English/ASCII, public-safe repository document contains provenance and verification below; focused outputs and exact-head CI are recorded in the PR. | Satisfied. |
| AC-N1 | No dependency is proposed from popularity or upstream use; #705 is mandatory. | Satisfied. |
| AC-N2 | Every reviewed mechanism includes license, provenance, security, and maintenance boundaries; no source is copied. | Satisfied. |
| AC-N3 | Closed issue status and tests are treated as history/evidence, not optimality proof. | Satisfied. |
| AC-N4 | No runtime, VM, build-server timing, deployment, or benchmark was used. | Satisfied. |
| AC-N5 | Every accepted gap maps to live quality-ready owners #732/#733/#734/#758; #705 is a decision gate, not a speculative implementation owner. | Satisfied. |

## Reproduction and verification

### Upstream provenance

The following read-only commands were run outside the repository under a
worker-owned research directory:

```text
for repo in opentelemetry-rust tracing-opentelemetry \
  opentelemetry-collector opentelemetry-collector-contrib tempo vector \
  openobserve jaeger tracing signoz; do
  git -C "$RESEARCH_ROOT/upstreams/$repo" show -s \
    --format='%H %cI %D' HEAD
done
```

Observed pins:

```text
opentelemetry-rust 0e78170d712e5046b8ed93b6f99b2b003af15cd7 2026-07-22
tracing-opentelemetry 1d5422f1f37932fd65e434da618b305d4c94ee9c 2026-05-19 v0.33.0
opentelemetry-collector 259f177f8c1aea6f1a98c0a23ef1817c88afeb92 2026-07-28
opentelemetry-collector-contrib baf8c2342f650d0b36bbd5dec5ba7fb763e65391 2026-07-28
tempo bb8b3766272f75b4d09481b86d38c8d8b4b2e3f2 2026-07-28
vector f54459dbf288badc902d291c66e5a8a06fa92b6b 2026-07-28
openobserve 17ef03b8d6cf4e0764d593e8acd8381e90203719 2026-07-29
jaeger fc6d11f19d2ef2624163562b7e765b2265f68f6d 2026-07-28
tracing d9d4c542de10f5d3a711b7a45ffe450fd0666437 2026-05-30
signoz bca23708621a1a7008ddbf75a9e473b428bd05dc 2026-07-29
```

### Focused checks

Final exact outputs are recorded in the PR evidence after running:

```text
python3 $CHECK_ROOT/verify_pinned_sources.py \
  docs/research/oss/observability-causal-correlation.md
python3 $CHECK_ROOT/verify_structure.py \
  docs/research/oss/observability-causal-correlation.md
python3 $CHECK_ROOT/verify_external_urls.py \
  docs/research/oss/observability-causal-correlation.md
python3 $CHECK_ROOT/verify_gfm.py \
  docs/research/oss/observability-causal-correlation.md
python3 $CHECK_ROOT/verify_public_ascii.py \
  docs/research/oss/observability-causal-correlation.md
typos docs/research/oss/observability-causal-correlation.md
git diff --check
```

The source verifier resolves each pinned GitHub object and validates cited line
ranges. The structure verifier checks the candidate/deep-review counts,
mechanism and decision coverage, all M0 classes, all acceptance criteria,
owner/proposed-contract sections, and the exactly-one-decision invariant. The
URL verifier checks external links without following repository-private
infrastructure. The GFM verifier renders through GitHub's Markdown API. The
public-safety check fails on non-ASCII, private host/address/user/home/temp
paths, credentials, and forbidden infrastructure details.

## Known limits

- The complete M0 company-work runtime is still being implemented by
  #694/#695/#696, so this study can verify target contracts and current partial
  paths but cannot run the future end-to-end reconstruction.
- No runtime overhead or capacity result exists. Every number must come from
  the implementation issue's declared product target.
- OTel SDK/bridge/Collector, broader W3C interoperability, and an external
  trace backend are not approved for M0. They require a future POST_M0 need and
  fresh #705 decision.
- Owner contracts are materialized in #732/#733/#734/#758 and acknowledged in
  #718. TOGAF edits remain main-session-only after verified implementation.
