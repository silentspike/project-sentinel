# OSS inference engines and Cortex Gateway control study

Issue: [#714](https://github.com/silentspike/project-sentinel/issues/714)
Research parent: [#659](https://github.com/silentspike/project-sentinel/issues/659)
Repository baseline: `ae31ed303bf039c78b666ed1bf0a29e5ac334a93`
Research date: 2026-07-29
Runtime target class: `NONE`

This study compares mechanisms, not product checklists. It reads the current
Sentinel implementation and tests, then reads immutable upstream source and tests.
Upstream benchmarks are not Sentinel evidence. No provider, runtime host, GPU,
build server, or model was exercised.

## Executive decision

Sentinel should keep the Cortex Gateway and the durable Rust LLM bridge as its
provider-independent control authority. It should not replace them with vLLM,
SGLang, TGI, llama.cpp server, LiteLLM, or another proxy. An inference engine
may later be wrapped behind Sentinel's `Provider` contract, but it must not own
provider selection, hierarchy policy, request identity, budget authority,
durable effect completion, or usage reconciliation.

The accepted decisions are:

1. **KEEP** the immutable provider catalog, hierarchy model mapping, token-free
   readiness, Gate B activation, and Gateway request classification.
2. **KEEP** the Rust bridge's stable request ID and digest, durable
   `provider_in_flight` reservation, completion recovery, action claim, and
   idempotent usage operation. Rust/EventStore remains the durable request,
   effect, usage, atomic budget-reservation, and canonical attempt authority.
3. **REIMPLEMENT** bounded Gateway admission in Sentinel Go: a finite queue,
   typed overload outcome, cancellation-safe FIFO grant, per-class limits, and
   observable pressure. Go owns edge admission, provider/model selection, and
   provider-execution deadlines. Port only channel/semaphore ideas from SGLang
   and fail-fast overload semantics from TGI; neither proves bounded ingress.
4. **REIMPLEMENT** capability-preserving provider wrappers. The current queue
   wrapper preserves model inventory but drops both streaming and provider-status
   reporting when the wrapped provider implements them.
5. **REIMPLEMENT** terminal streaming usage and cancellation reconciliation.
   Streaming must use the same request identity, cost source, and durable outcome
   rules as non-streaming calls.
6. **PORT THE CONTRACT, NOT THE DEPENDENCY** for budget reservation: reserve a
   conservative maximum before dispatch, reconcile exactly once to reported or
   catalog cost, and retain incurred input cost after post-dispatch cancellation.
   LiteLLM demonstrates the idea, not crash durability. #695 and Rust/EventStore
   remain Sentinel's budget authority; LiteLLM must not become routing authority.
7. **KEEP** provider-side prompt-cache hints. Engine-side KV/prefix caching is an
   implementation detail behind a provider boundary and is never a durable
   business-state authority.
8. **WRAP LATER, AFTER A TARGET DECISION GATE**: vLLM or SGLang for a supported
   GPU lane, and llama.cpp server for a constrained CPU/local lane. No engine is
   selected by this research because no Sentinel target benchmark was authorized.
9. **REJECT** TGI as a new strategic Sentinel dependency. Its mechanisms remain
   useful comparison evidence, but its current pin has weaker repository security
   governance and offers no unique authority Sentinel should import.
10. **REJECT** LiteLLM as an in-process dependency or sidecar authority. Its broad
    proxy and provider surface would duplicate policy, retry, budget, and usage
    ownership.
11. **KEEP OUT OF PRODUCTION** the current `sentinel-inference` prototypes. They
    are subprocess/string-level experiments rather than an inference server.
    Their dependency necessity and future ownership belong to [#705](https://github.com/silentspike/project-sentinel/issues/705).
12. **REJECT AUTOMATIC RETRY OR FAILOVER AFTER AMBIGUOUS DISPATCH**. A timeout,
    disconnect, or lost response after provider dispatch may already be billable.
    Fallback is safe only before dispatch or after a typed, definitive non-billable
    rejection.

The immediate accepted gaps are Gateway admission, wrapper capability
preservation, streaming terminal accounting, and #695's durable budget/attempt
delta. Only the Go Gateway implementation is uncovered work. Proposed contracts
are included below, but are not live owner assignments until the ORC approves
materialization.

## Method and decision rules

### Evidence standard

An upstream claim requires an immutable commit and a source or test path. A
README claim without implementation or test evidence is not sufficient.
Sentinel claims use the baseline above and exact repository paths. Tests prove
only the behavior they assert; they do not prove production composition,
operational capacity, or performance.

The review asks the same questions of every candidate:

- Where are admission, queue capacity, scheduling, cancellation, and shutdown?
- Where are streaming terminal state and usage computed?
- What happens after timeout, client disconnect, provider error, and process
  restart?
- What cache is shared, what key owns it, and what invalidates it?
- Which component may retry or fail over, and how does it avoid duplicate charge
  or effect?
- Which authentication, request-size, model allowlist, and network controls are
  source-backed?
- What are the license, security-reporting, dependency, and operational costs?

### Decision vocabulary

| Decision | Meaning |
|---|---|
| `KEEP` | Sentinel already owns the mechanism and retains authority. |
| `PORT` | Reimplement a small, understood mechanism under Sentinel contracts; do not copy upstream code. |
| `WRAP` | Run an external engine behind the provider boundary; Sentinel remains authoritative. |
| `INTEGRATE` | Add an upstream library or service as an owned dependency after #705 approval. |
| `REIMPLEMENT` | Build a Sentinel-native mechanism because no dependency should own it. |
| `REJECT` | Do not adopt the product or mechanism for the stated scope. |

### Screening rubric

Candidates were scored qualitatively on five load-bearing dimensions:

| Dimension | Required evidence |
|---|---|
| Admission and failure | Bounded admission or explicit rejection, cancellation, overload, retry, and shutdown source/tests |
| Cache and scheduling | Prefix/KV cache ownership, eviction, preemption, fairness, and scheduler tests |
| Streaming and usage | Disconnect handling, terminal usage, partial-result semantics, and error mapping |
| Security and operations | Auth, request/model limits, health/readiness, deployment controls, security policy |
| Fit and ownership | Provider-independent integration, no duplicate authority, maintainable license/dependency surface |

Popularity, throughput claims, and upstream hardware timings do not establish
Sentinel suitability.

## Sentinel baseline

### End-to-end request and authority path

The current productive path is:

1. The daemon produces perceptions into a bounded synchronous channel and the
   LLM bridge forwards them into a bounded Tokio channel
   ([`orchestrator.rs:1556-1558`](../../../services/sentinel-daemon/src/orchestrator.rs#L1556),
   [`llm_bridge.rs:702-714`](../../../services/sentinel-daemon/src/llm_bridge.rs#L702)).
2. The bridge coalesces per-agent work, applies tick-rate policy, and shares an
   eight-slot semaphore by default
   ([`llm_bridge.rs:650-726`](../../../services/sentinel-daemon/src/llm_bridge.rs#L650)).
3. Before a provider call, it derives `agent-runtime-{agent}-{tick}`, hashes the
   request, checks prior completion, and fails closed on an ambiguous
   `provider_in_flight` record
   ([`llm_bridge.rs:276-302`](../../../services/sentinel-daemon/src/llm_bridge.rs#L276),
   [`llm_bridge.rs:800-844`](../../../services/sentinel-daemon/src/llm_bridge.rs#L800)).
4. It reserves that identity before HTTP dispatch. A completed provider response
   is durably enqueued before local action and usage recovery
   ([`llm_bridge.rs:895-947`](../../../services/sentinel-daemon/src/llm_bridge.rs#L895)).
5. The authenticated `/internal/agent-runtime` request enters the Go pipeline.
   External compatibility, platform-control, service-internal, and agent-runtime
   requests are distinct classes
   ([`provider.go:8-40`](../../../cmd/cortex-gateway/internal/proxy/provider.go#L8),
   [`pipeline.go:358-417`](../../../cmd/cortex-gateway/internal/proxy/pipeline.go#L358)).
6. The immutable catalog validates exact provider/model inventory or requires an
   explicit provider-plus-catalog Gate B attestation
   ([`catalog.go:150-209`](../../../cmd/cortex-gateway/internal/proxy/catalog.go#L150),
   [`catalog.go:344-364`](../../../cmd/cortex-gateway/internal/proxy/catalog.go#L344)).
7. Model selection is resolved from request class, hierarchy tier, policy, and
   catalog allowlist, then the selected provider is called under deadline and
   circuit-breaker policy
   ([`catalog.go:240-298`](../../../cmd/cortex-gateway/internal/proxy/catalog.go#L240),
   [`pipeline.go:653-745`](../../../cmd/cortex-gateway/internal/proxy/pipeline.go#L653)).
8. Every non-streaming response passes one sink that resolves cost, records
   cache-aware usage, and exposes private usage-v2 fields only to agent-runtime
   callers
   ([`pipeline.go:1454-1531`](../../../cmd/cortex-gateway/internal/proxy/pipeline.go#L1454)).
9. The bridge persists one `AgentLlmUsage` operation and claims completion actions
   idempotently. Startup and periodic recovery finish committed provider results
   before new work
   ([`llm_bridge.rs:174-274`](../../../services/sentinel-daemon/src/llm_bridge.rs#L174),
   [`llm_bridge.rs:666-700`](../../../services/sentinel-daemon/src/llm_bridge.rs#L666)).
10. The canonical event and projection path aggregates cost independently of
    provider process lifetime
    ([`events.rs:428-455`](../../../crates/sentinel-common/src/events.rs#L428),
    [`cost.rs:26-45`](../../../crates/sentinel-projection/src/handlers/cost.rs#L26),
    [`worker.rs:254-299`](../../../crates/sentinel-projection/src/worker.rs#L254)).

This split is load-bearing: Go owns request-edge policy and provider adaptation;
Rust owns durable simulation effects and usage. An engine owns only inference
execution and ephemeral engine cache.

### Exact impact map

| Path | Current role | Load-bearing observation | Existing owner |
|---|---|---|---|
| `cmd/cortex-gateway/main.go:70-146` | Composition root | Requires the exact catalog, constructs one shared forward queue, wraps remote providers, and validates activation | Cortex Gateway; #650/#695 |
| `cmd/cortex-gateway/main.go:341-350` | HTTP surface | Exposes compatibility, internal, agent-runtime, health, readiness, and metrics routes | Cortex Gateway |
| `cmd/cortex-gateway/main.go:663-721` | Readiness | Token-free local readiness plus inventory/Gate B validation; provider calls are not readiness probes | #650 |
| `cmd/cortex-gateway/internal/proxy/provider.go:21-109` | Provider ABI | Usage/cost fields plus optional inventory, streaming, and status interfaces | Cortex Gateway |
| `cmd/cortex-gateway/internal/proxy/catalog.go:28-75` | Provider catalog | Routing semantics have a stable digest; endpoints and credentials are deliberately outside it | #395 historical contract |
| `cmd/cortex-gateway/internal/proxy/catalog.go:240-364` | Model policy | Hierarchy resolution and activation fail closed against allowlisted models | #395, #650 |
| `cmd/cortex-gateway/internal/capability/detection.go:5-103` | Capability map | Capabilities are mutable, hand-maintained process state and are not bound to the catalog digest | Proposed #732 schema delta |
| `cmd/cortex-gateway/internal/forwardqueue/manager.go:37-96` | Admission | Concurrency is bounded, but the waiter slice has no capacity limit; cancellation/grant race is handled | #764 pressure; #769 schedules |
| `cmd/cortex-gateway/internal/proxy/queued_provider.go:10-53` | Queue wrapper | Preserves inventory only; it drops `StreamingProvider` and `ProviderStatusReporter` | Proposed G1 |
| `cmd/cortex-gateway/internal/proxy/provider_test.go:49-67` | Wrapper test | Proves inventory preservation only | Proposed G1; #769 |
| `cmd/cortex-gateway/internal/proxy/pipeline.go:420-449` | Circuit-open response | Uses `ProviderStatusReporter` for Claude-Code typed 429/503 status; the productive wrapper hides it | Proposed G1 |
| `cmd/cortex-gateway/internal/proxy/pipeline_test.go:482-529` | Stream test | Registers an unwrapped mock, so productive queue composition is not covered | Proposed G1; #769 |
| `cmd/cortex-gateway/internal/proxy/pipeline.go:1082-1103` | Budget fallback | Budget exhaustion may switch providers before dispatch; there is no general provider retry loop | #695 |
| `cmd/cortex-gateway/internal/proxy/pipeline.go:1454-1531` | Response sink | Non-stream terminal usage/cost has a single process-local sink | #695, #758 |
| `cmd/cortex-gateway/internal/proxy/pipeline.go:1689-1747` | Streaming | Requires an optional interface and records latency/count only; it does not parse terminal usage or enter the response sink | Proposed G1 plus #695/#732 |
| `cmd/cortex-gateway/internal/proxy/claude.go:120-206` | Anthropic non-stream | Context deadline, response bound, provider usage and cache-token split | Cortex Gateway |
| `cmd/cortex-gateway/internal/proxy/claude.go:247-334` | Anthropic stream | Relays raw SSE and propagates write/read error; no terminal usage reconciliation | Proposed G1 plus #695/#732 |
| `cmd/cortex-gateway/internal/proxy/claude_code.go:103-195` | Claude CLI adapter | Separate internal semaphore plus per-request subprocess and stderr drain | Cortex Gateway |
| `cmd/cortex-gateway/internal/proxy/ollama.go:85-225` | Ollama adapter | Non-stream HTTP generation and token-free inventory | Cortex Gateway |
| `cmd/cortex-gateway/internal/proxy/local_loop.go:20-237` | Deterministic fixture | Token-free, no network/subprocess, stable scenario digest, supports synthetic SSE | #650 test path |
| `cmd/cortex-gateway/internal/guardrails/budget.go:20-87` | Budget | Hour/day counters are process memory; check and record are separate operations | #695 |
| `cmd/cortex-gateway/internal/guardrails/ratelimit.go:45-99` | Rate limit | Global and per-agent token buckets; the agent map is process-local | #764 |
| `services/sentinel-daemon/src/llm_bridge.rs:35-73` | Bridge config | Deadline, concurrency, usage-v2, and bounded recovery attempts | #695 |
| `services/sentinel-daemon/src/llm_bridge.rs:174-302` | Durable identity | Request reservation, completion, usage, actions, stable ID and digest | #732/#733 |
| `services/sentinel-daemon/src/llm_bridge.rs:650-714` | Rust admission | Shared semaphore plus bounded bridge channel and restart recovery | #764/#733 |
| `services/sentinel-daemon/src/llm_bridge.rs:800-947` | Exactly-once boundary | Ambiguous dispatch is not retried; result is committed before effect recovery | #733/#695 |
| `services/sentinel-daemon/src/platform_controlplane/llm_analyzer.rs:86-173` | Platform analysis | Bounded queue, sequential worker, typed queue-full error and drop counter | Platform control-plane owner |
| `crates/sentinel-limbo/src/outbox_publisher.rs:62-185` | Event delivery | Publish then mark means at-least-once after a crash; stable consumer identity must absorb duplicates | #732/#733 |
| `crates/sentinel-inference/src/bitnet.rs:16-65` | Prototype | Synchronous child process without service admission or cancellation contract | #705 |
| `crates/sentinel-inference/src/kv_cache.rs:3-88` | Prototype | String prompt-prefix map, not engine KV memory | #705 |
| `crates/sentinel-inference/src/multi_lora.rs:12-20` | Prototype | Filesystem adapter inventory, not concurrent adapter serving | #705 |
| `crates/sentinel-inference/src/speculative.rs:15-69` | Prototype | Serial subprocesses and whitespace prefix comparison, not token speculative decoding | #705 |

### Verified gaps and non-gaps

**G1, productive optional-interface composition.** `ClaudeProvider` implements
`StreamingProvider`, and `ClaudeCodeProvider` implements
`ProviderStatusReporter`, but `NewQueuedProvider` returns a wrapper that
implements only `Provider` and optionally `ModelInventoryProvider`. Productive
composition wraps both providers. `streamAnthropicResponse` therefore cannot
type-assert the streaming wrapper and predicts a 502. The circuit-open path at
`pipeline.go:431` cannot recover Claude-Code's typed cooldown status and falls
back to generic 503. These are source-backed defects, not runtime observations.

**G2, unbounded waiting admission.** The Go queue bounds active forwards but
appends every waiter to a slice. Slow providers can turn authenticated request
pressure into unbounded process memory and latency. The Rust bridge's bounded
channels do not protect external compatibility routes.

**G3, stream terminal state.** The raw SSE relay records request count and
latency, but it does not parse terminal usage, compute catalog cost, record the
single response sink, or publish a durable usage outcome. A disconnected client
may still have incurred provider input/output cost.

**G4, budget check/record race.** The in-memory budget checks an estimate before
dispatch and records actual usage later. Concurrent requests can all pass the
same remaining balance. Restart also clears the counter. This conflicts with
#695's no-cost-ceiling-bypass intent.

**G5, failover target drift.** Current code has budget-triggered pre-dispatch
provider substitution and circuit breaking, not general automatic provider
retry/failover. That is safer than blind retries, but the TOGAF target must say
that post-dispatch failover requires a typed definitive outcome.

The following are not gaps:

- Provider-independent model policy already exists and should not move into an
  engine router.
- The Rust bridge already prevents automatic replay of ambiguous provider work.
- Canonical cost is already a durable event/projection concern, not an engine
  metric.
- Token-free readiness already avoids spending tokens to prove configuration.
- Outbox delivery is intentionally at-least-once; exactly-once business effect
  belongs at stable producer/consumer identities, not in an inference engine.

### Target-architecture constraints and delta

The current TOGAF target describes the Gateway as the provider proxy
([`togaf-architecture-guide.html:959-1012`](../../architecture/togaf-architecture-guide.html#L959)),
provider-side inference as productive while `sentinel-inference` remains planned
([`togaf-architecture-guide.html:1354-1368`](../../architecture/togaf-architecture-guide.html#L1354)),
and future SGLang/KVFlow/Multi-LoRA mechanisms
([`togaf-architecture-guide.html:2055-2058`](../../architecture/togaf-architecture-guide.html#L2055)).

Target-only delta proposed for the main-session owner:

1. Define Cortex Gateway as the edge-admission, provider/model-selection, and
   provider-execution-deadline authority. Caller deadlines remain end-to-end
   policy; there is no Gateway-owned global deadline.
2. Define engines as replaceable execution adapters. Engine queue/KV cache state
   is ephemeral and cannot become event, budget, request, or ownership authority.
3. Define Rust/EventStore as the durable request/effect/usage, atomic
   budget-reservation, and canonical attempt-outcome authority. Every billable
   Gateway route must obtain an authoritative reservation through the shared
   port before dispatch or fail closed; a digest-bound non-billable exemption is
   the only exception.
4. Replace unconditional "provider failover" language with: failover is allowed
   before dispatch or after a typed definitive non-billable outcome; ambiguous
   dispatch is quarantined/reconciled and never blindly retried.
5. Replace fixed concurrency claims with versioned bounded-admission policy:
   global, request-class, provider, queue-capacity, deadline, overload outcome,
   and pressure metrics.
6. Bind streaming to the same terminal usage, cost-source, request-digest, and
   cancellation outcome as non-streaming.
7. Mark SGLang RadixAttention, vLLM prefix caching, KVFlow, Multi-LoRA, grammar
   engines, and local-engine selection `POST_M0` until target hardware,
   dependency ownership, security, and rollback gates pass.

Once the architecture and owners approve this decision set, the main session
should update both language-specific TOGAF target copies immediately. Target
contracts do not wait for implementation evidence; measured results remain
delivery evidence.

No TOGAF file is changed by this worker.

### Existing owners and non-overlap

| Issue | Live role | #714 routing rule |
|---|---|---|
| [#395](https://github.com/silentspike/project-sentinel/issues/395) | Closed, verified hierarchy/catalog history | Preserve the contract; do not reopen or assign new work to a closed issue. |
| [#650](https://github.com/silentspike/project-sentinel/issues/650) | M0 product-acceptance epic | Proposed parent for M0 Gateway hardening after approval. |
| [#695](https://github.com/silentspike/project-sentinel/issues/695) | M0 workflow and cost-ceiling behavior | Owns cost-ceiling acceptance and authoritative action schema, not queue implementation. |
| [#696](https://github.com/silentspike/project-sentinel/issues/696) | QA/release/lineage | Consumes final inference evidence; no Gateway implementation ownership. |
| [#705](https://github.com/silentspike/project-sentinel/issues/705) | Dependency necessity/ownership audit | Must approve any engine/library dependency and disposition of `sentinel-inference`. |
| [#656](https://github.com/silentspike/project-sentinel/issues/656) | Dependency upgrades | Owns later upgrade mechanics after a dependency is approved, not adoption. |
| [#732](https://github.com/silentspike/project-sentinel/issues/732) | Canonical event/envelope authority | Owns shared durable inference attempt/usage event schemas. |
| [#733](https://github.com/silentspike/project-sentinel/issues/733) | Durable outbox and consumer outcomes | Owns lossless delivery and retry outcome, not provider routing. |
| [#758](https://github.com/silentspike/project-sentinel/issues/758) | Causal observability policy | Consumes request/attempt IDs and bounded counters without becoming business authority. |
| [#764](https://github.com/silentspike/project-sentinel/issues/764) | Pressure governor and exactly-once admission | Owns cross-runtime pressure policy; Gateway queue implementation must conform. |
| [#769](https://github.com/silentspike/project-sentinel/issues/769) | Go synctest/race/barrier schedules | Owns deterministic proof of Gateway grant/cancel/queue schedules, not production behavior. |

## OSS landscape

### Reproducible inventory

The scan used repository search and immutable `HEAD` resolution on 2026-07-29.
The deep-review shortlist covers all five systems required by #714. Five
credible alternatives were retained as screening evidence.

| Candidate | Immutable commit | Lane | Review depth | Initial disposition |
|---|---|---|---|---|
| [vLLM](https://github.com/vllm-project/vllm/tree/a0c092ee72c0dcefbb3b3e74f97ac62d842e5f4b) | `a0c092ee72c0dcefbb3b3e74f97ac62d842e5f4b` | GPU engine/server | Deep | Wrap candidate, post-M0 |
| [SGLang](https://github.com/sgl-project/sglang/tree/e1f2f9d1fa84cd1b8d9020377fdd707b3a485687) | `e1f2f9d1fa84cd1b8d9020377fdd707b3a485687` | GPU engine/gateway | Deep | Mechanism source; wrap candidate |
| [TGI](https://github.com/huggingface/text-generation-inference/tree/b4adbf2f6e2e721280bd0ea5f91d70f7d033f5ed) | `b4adbf2f6e2e721280bd0ea5f91d70f7d033f5ed` | GPU server | Deep | Reject adoption; port overload semantics |
| [llama.cpp](https://github.com/ggml-org/llama.cpp/tree/caa596ab3f0f8768ee326d6e3d5d39782194676c) | `caa596ab3f0f8768ee326d6e3d5d39782194676c` | CPU/GPU local server | Deep | Wrap candidate, post-M0 |
| [LiteLLM](https://github.com/BerriAI/litellm/tree/c274cf321c5c35c629220a89bb497d15b56f870f) | `c274cf321c5c35c629220a89bb497d15b56f870f` | Provider proxy | Deep | Reject authority; port reservation idea |
| [NVIDIA Dynamo](https://github.com/ai-dynamo/dynamo/tree/29ef3b5def0ea37bfcac015a81edbcbcd9ff1c31) | `29ef3b5def0ea37bfcac015a81edbcbcd9ff1c31` | Distributed serving | Scan | Post-M0 background |
| [TensorRT-LLM](https://github.com/NVIDIA/TensorRT-LLM/tree/f20ea652dd621006148aa918c970ba686cdda407) | `f20ea652dd621006148aa918c970ba686cdda407` | NVIDIA engine/runtime | Scan | Reject for current portable lane |
| [LMDeploy](https://github.com/InternLM/lmdeploy/tree/821730d650d5999260cef6f3ce464edeced6047e) | `821730d650d5999260cef6f3ce464edeced6047e` | GPU engine/server | Scan | No unique mechanism |
| [Ollama](https://github.com/ollama/ollama/tree/4713800b08b2ddf5e14acf8398953cf7b12f169b) | `4713800b08b2ddf5e14acf8398953cf7b12f169b` | Local model server | Scan | Keep existing adapter only |
| [LocalAI](https://github.com/mudler/LocalAI/tree/ecdb32193d88cb80d6c2eb6ef2c2bc8205b53a94) | `ecdb32193d88cb80d6c2eb6ef2c2bc8205b53a94` | Local API/backend hub | Scan | Reject proxy duplication |

Reproduction:

```bash
git ls-remote https://github.com/vllm-project/vllm.git HEAD
git ls-remote https://github.com/sgl-project/sglang.git HEAD
git ls-remote https://github.com/huggingface/text-generation-inference.git HEAD
git ls-remote https://github.com/ggml-org/llama.cpp.git HEAD
git ls-remote https://github.com/BerriAI/litellm.git HEAD
git ls-remote https://github.com/ai-dynamo/dynamo.git HEAD
git ls-remote https://github.com/NVIDIA/TensorRT-LLM.git HEAD
git ls-remote https://github.com/InternLM/lmdeploy.git HEAD
git ls-remote https://github.com/ollama/ollama.git HEAD
git ls-remote https://github.com/mudler/LocalAI.git HEAD
```

### Shortlist and rejection rationale

- vLLM and SGLang have the strongest source-backed scheduler, prefix-cache, and
  cancellation evidence for a future GPU lane.
- TGI gives a compact typed overload model and useful security contrast, but its
  repository pin lacks a security-policy file and its validation path includes
  unbounded channels. It contributes mechanisms, not a product choice.
- llama.cpp server is the strongest reviewed constrained local/CPU candidate and
  has practical API-key/CORS/path security tests, but its task queues are not
  capacity-bounded.
- LiteLLM directly tests the provider-router/budget problem, including
  process-local cancellation reservation reconciliation, but importing its proxy
  would create duplicate authority and would not supply crash durability.
- Dynamo adds distributed KV-aware routing and operational machinery that is
  disproportionate before a single-node M0 target is accepted.
- TensorRT-LLM is hardware/vendor-specific and cannot satisfy a portable default
  lane.
- LMDeploy did not expose a unique mechanism missing from the deep shortlist.
- Existing Ollama support should remain a thin adapter; adopting its scheduler
  as Sentinel policy would invert ownership.
- LocalAI duplicates provider/back-end routing without improving Sentinel's
  durable request/effect contract.

## Pinned deep reviews

### 1. vLLM

**Pin and governance.** Commit
[`a0c092e`](https://github.com/vllm-project/vllm/commit/a0c092ee72c0dcefbb3b3e74f97ac62d842e5f4b)
is [Apache-2.0](https://github.com/vllm-project/vllm/blob/a0c092ee72c0dcefbb3b3e74f97ac62d842e5f4b/LICENSE).
The pin contains a
[`SECURITY.md`](https://github.com/vllm-project/vllm/blob/a0c092ee72c0dcefbb3b3e74f97ac62d842e5f4b/SECURITY.md)
and a separate security deployment guide.

**Mechanisms.** The v1 scheduler enforces token and sequence budgets, maintains
running and waiting sets, and preempts when KV allocation cannot proceed
([`scheduler.py`](https://github.com/vllm-project/vllm/blob/a0c092ee72c0dcefbb3b3e74f97ac62d842e5f4b/vllm/v1/core/sched/scheduler.py#L100-L125),
[`scheduler.py`](https://github.com/vllm-project/vllm/blob/a0c092ee72c0dcefbb3b3e74f97ac62d842e5f4b/vllm/v1/core/sched/scheduler.py#L420-L455),
[`scheduler.py`](https://github.com/vllm-project/vllm/blob/a0c092ee72c0dcefbb3b3e74f97ac62d842e5f4b/vllm/v1/core/sched/scheduler.py#L570-L615)).
Its KV manager allocates, frees, and evicts prefix-cache blocks behind the
scheduler
([`kv_cache_manager.py`](https://github.com/vllm-project/vllm/blob/a0c092ee72c0dcefbb3b3e74f97ac62d842e5f4b/vllm/v1/core/kv_cache_manager.py)).
The OpenAI serving layer emits final and optional continuous stream usage,
including cached prompt tokens
([`serving.py`](https://github.com/vllm-project/vllm/blob/a0c092ee72c0dcefbb3b3e74f97ac62d842e5f4b/vllm/entrypoints/openai/chat_completion/serving.py#L434-L570)).

**Failure and tests.** Scheduler tests cover admission, priority, preemption,
abort, and KV reclamation
([`test_scheduler.py`](https://github.com/vllm-project/vllm/blob/a0c092ee72c0dcefbb3b3e74f97ac62d842e5f4b/tests/v1/core/test_scheduler.py)).
An explicit final-step race test requires abort to win and KV resources to be
freed
([`test_abort_final_step.py`](https://github.com/vllm-project/vllm/blob/a0c092ee72c0dcefbb3b3e74f97ac62d842e5f4b/tests/v1/engine/test_abort_final_step.py)).
OpenAI server middleware supports request IDs and optional API-key auth, but
deployment defaults such as permissive CORS still require Sentinel-owned
hardening
([`api_server.py`](https://github.com/vllm-project/vllm/blob/a0c092ee72c0dcefbb3b3e74f97ac62d842e5f4b/vllm/entrypoints/openai/api_server.py#L275-L350)).
Its CLI exposes scheduler, cache, model, TLS, middleware, and serving controls;
that is useful operational tooling, but it is also a substantial Python/CUDA
service and image lifecycle rather than a small library.

**Decision.** `WRAP`, never embed, after a post-M0 target benchmark and #705
approval. Port no scheduler code. The provider adapter must translate Sentinel
IDs, deadlines, cancellation, usage, and definitive/ambiguous outcomes.

### 2. SGLang

**Pin and governance.** Commit
[`e1f2f9d`](https://github.com/sgl-project/sglang/commit/e1f2f9d1fa84cd1b8d9020377fdd707b3a485687)
is [Apache-2.0](https://github.com/sgl-project/sglang/blob/e1f2f9d1fa84cd1b8d9020377fdd707b3a485687/LICENSE).
No repository security-policy file exists at this pin, which is an adoption risk
requiring an explicit deployment threat model.

**Mechanisms.** The model gateway creates a bounded job channel of 1,000 and a
200-permit semaphore. This is not end-to-end bounded admission:
`tx.send(job).await` can suspend an unbounded number of submit futures outside
the channel, and `status_map` is populated before the await
([`job_queue.rs`](https://github.com/sgl-project/sglang/blob/e1f2f9d1fa84cd1b8d9020377fdd707b3a485687/sgl-model-gateway/src/core/job_queue.rs#L90-L238)).
The runtime scheduler supports request abort and retraction, while its unified
radix cache matches/inserts prefixes, tracks lock references, evicts, and caches
finished requests
([`scheduler.py`](https://github.com/sgl-project/sglang/blob/e1f2f9d1fa84cd1b8d9020377fdd707b3a485687/python/sglang/srt/managers/scheduler.py),
[`unified_radix_cache.py`](https://github.com/sgl-project/sglang/blob/e1f2f9d1fa84cd1b8d9020377fdd707b3a485687/python/sglang/srt/mem_cache/unified_radix_cache.py#L370-L470)).
The OpenAI layer separately calculates non-stream and streaming prompt,
completion, and cached-token usage
([`usage_processor.py`](https://github.com/sgl-project/sglang/blob/e1f2f9d1fa84cd1b8d9020377fdd707b3a485687/python/sglang/srt/entrypoints/openai/usage_processor.py)).

**Failure and tests.** The Rust model-gateway reliability suite proves that
dropping a client response stream cancels the upstream worker
([`upstream_cancel_test.rs`](https://github.com/sgl-project/sglang/blob/e1f2f9d1fa84cd1b8d9020377fdd707b3a485687/sgl-model-gateway/tests/reliability/upstream_cancel_test.rs)).
Python tests exercise request abort and disconnect detection
([`test_abort_with_metrics.py`](https://github.com/sgl-project/sglang/blob/e1f2f9d1fa84cd1b8d9020377fdd707b3a485687/test/registered/scheduler/test_abort_with_metrics.py),
[`test_abort_request.py`](https://github.com/sgl-project/sglang/blob/e1f2f9d1fa84cd1b8d9020377fdd707b3a485687/test/manual/entrypoints/http_server/test_abort_request.py)).
The gateway exposes worker API keys, control-plane keys, CORS, metrics, and
service discovery configuration in one operational surface
([`main.rs`](https://github.com/sgl-project/sglang/blob/e1f2f9d1fa84cd1b8d9020377fdd707b3a485687/sgl-model-gateway/src/main.rs#L540-L615),
[`server.rs`](https://github.com/sgl-project/sglang/blob/e1f2f9d1fa84cd1b8d9020377fdd707b3a485687/sgl-model-gateway/src/server.rs#L1030-L1150)).
That surface must not replace Sentinel's control plane.

**Decision.** `WRAP` only after the same post-M0 gate as vLLM. `PORT` only the
bounded-channel and semaphore mechanism into Sentinel Go, not SGLang's ingress
contract or gateway authority. Sentinel additionally requires a hard ingress/
waiter cap, immediate typed rejection or a bounded deadline, and tests for
pre-send status cardinality. RadixAttention remains engine-local and ephemeral.

### 3. Text Generation Inference

**Pin and governance.** Commit
[`b4adbf2`](https://github.com/huggingface/text-generation-inference/commit/b4adbf2f6e2e721280bd0ea5f91d70f7d033f5ed)
is [Apache-2.0](https://github.com/huggingface/text-generation-inference/blob/b4adbf2f6e2e721280bd0ea5f91d70f7d033f5ed/LICENSE).
The repository contains no security-policy file at the pin.

**Mechanisms.** TGI immediately tries a semaphore permit before validation and
scheduling; overload is a typed failure rather than an unbounded queue
([`infer/mod.rs`](https://github.com/huggingface/text-generation-inference/blob/b4adbf2f6e2e721280bd0ea5f91d70f7d033f5ed/router/src/infer/mod.rs#L83-L120)).
The permit is returned with the generation stream and backend errors mark health
false
([`infer/mod.rs`](https://github.com/huggingface/text-generation-inference/blob/b4adbf2f6e2e721280bd0ea5f91d70f7d033f5ed/router/src/infer/mod.rs#L130-L200)).
The server converts overload to HTTP 429 and exposes generated-token, queue,
batch, latency, and failure metrics
([`server.rs`](https://github.com/huggingface/text-generation-inference/blob/b4adbf2f6e2e721280bd0ea5f91d70f7d033f5ed/router/src/server.rs#L2395-L2415)).
Generation routes can be protected by an optional bearer key, while health and
metrics remain separate
([`server.rs`](https://github.com/huggingface/text-generation-inference/blob/b4adbf2f6e2e721280bd0ea5f91d70f7d033f5ed/router/src/server.rs#L2199-L2253)).

**Limits.** The tokenizer validation fan-out uses unbounded Tokio channels
([`validation.rs`](https://github.com/huggingface/text-generation-inference/blob/b4adbf2f6e2e721280bd0ea5f91d70f7d033f5ed/router/src/validation.rs#L25-L90)).
The reviewed source has weaker explicit disconnect/cancel test evidence than
vLLM or SGLang.
Streaming keeps the semaphore permit until the returned stream ends, but the
reviewed router tests do not prove Sentinel-style terminal cost reconciliation
after client disconnect. Operationally, TGI remains a full router/backend/model
service with tokenizer workers and metrics, not a control-plane library.

**Decision.** `REJECT` as a new strategic dependency. `PORT` only the
fail-fast typed overload idea as one selectable Sentinel admission policy.

### 4. llama.cpp server

**Pin and governance.** Commit
[`caa596a`](https://github.com/ggml-org/llama.cpp/commit/caa596ab3f0f8768ee326d6e3d5d39782194676c)
is [MIT](https://github.com/ggml-org/llama.cpp/blob/caa596ab3f0f8768ee326d6e3d5d39782194676c/LICENSE).
Its
[`SECURITY.md`](https://github.com/ggml-org/llama.cpp/blob/caa596ab3f0f8768ee326d6e3d5d39782194676c/SECURITY.md)
includes server tooling but documents a limited, volunteer-supported scope.

**Mechanisms.** The server has main and deferred task deques, slot-oriented
scheduling, cancellation tasks, and a response-reader destructor that stops
outstanding work
([`server-queue.h`](https://github.com/ggml-org/llama.cpp/blob/caa596ab3f0f8768ee326d6e3d5d39782194676c/tools/server/server-queue.h#L11-L115),
[`server-queue.h`](https://github.com/ggml-org/llama.cpp/blob/caa596ab3f0f8768ee326d6e3d5d39782194676c/tools/server/server-queue.h#L165-L208),
[`server-queue.cpp`](https://github.com/ggml-org/llama.cpp/blob/caa596ab3f0f8768ee326d6e3d5d39782194676c/tools/server/server-queue.cpp#L22-L100)).
Neither deque has a source-visible capacity bound.
Final and streaming response builders expose prompt, completion, and cached
token usage
([`server-task.cpp`](https://github.com/ggml-org/llama.cpp/blob/caa596ab3f0f8768ee326d6e3d5d39782194676c/tools/server/server-task.cpp#L393-L545)).

**Security and tests.** Server tests cover API keys, CORS, proxy-header handling,
and local media-path restrictions
([`test_security.py`](https://github.com/ggml-org/llama.cpp/blob/caa596ab3f0f8768ee326d6e3d5d39782194676c/tools/server/tests/unit/test_security.py)).
The server suite also covers slots and streaming usage
([`test_slot_save.py`](https://github.com/ggml-org/llama.cpp/blob/caa596ab3f0f8768ee326d6e3d5d39782194676c/tools/server/tests/unit/test_slot_save.py),
[`test_stream.py`](https://github.com/ggml-org/llama.cpp/blob/caa596ab3f0f8768ee326d6e3d5d39782194676c/tools/server/tests/unit/test_stream.py),
[`test_compat_anthropic.py`](https://github.com/ggml-org/llama.cpp/blob/caa596ab3f0f8768ee326d6e3d5d39782194676c/tools/server/tests/unit/test_compat_anthropic.py#L155-L210)).
Model files, slots, cache persistence, API keys, and process lifecycle remain an
operator-owned service boundary; these tests do not establish Sentinel admission
or canonical usage semantics.

**Decision.** `WRAP` as a possible post-M0 constrained local provider after
target-platform evidence. Do not import its queue or make its slot/cache state
durable Sentinel state.

### 5. LiteLLM

**Pin and governance.** Commit
[`c274cf3`](https://github.com/BerriAI/litellm/commit/c274cf321c5c35c629220a89bb497d15b56f870f)
uses [MIT](https://github.com/BerriAI/litellm/blob/c274cf321c5c35c629220a89bb497d15b56f870f/LICENSE)
for the non-enterprise tree, while enterprise paths carry separate terms. Its
[`security.md`](https://github.com/BerriAI/litellm/blob/c274cf321c5c35c629220a89bb497d15b56f870f/security.md)
defines private reporting.

**Mechanisms.** `Router` combines retries, fallbacks, cooldowns, and parallel
limits
([`router.py`](https://github.com/BerriAI/litellm/blob/c274cf321c5c35c629220a89bb497d15b56f870f/litellm/router.py#L321-L646)).
The proxy's budget reservation estimates worst-case cost, applies reservations
to all relevant counters, rolls back partial reservation failure, and can fail
closed
([`budget_reservation.py`](https://github.com/BerriAI/litellm/blob/c274cf321c5c35c629220a89bb497d15b56f870f/litellm/proxy/spend_tracking/budget_reservation.py#L146-L252)).
Reconciliation uses a `finalized` flag in the request's process-local Python
dictionary. It prevents repeated in-process reconciliation but is not
crash-durable exactly-once. Cancellation retains estimated input cost, releases
the output portion, and shields that process-local reconciliation from
cancellation
([`budget_reservation.py`](https://github.com/BerriAI/litellm/blob/c274cf321c5c35c629220a89bb497d15b56f870f/litellm/proxy/spend_tracking/budget_reservation.py#L255-L311)).
Unit suites cover Redis reservation failure, reservation accounting, router
parallel limits, cooldown, retry, and streaming fallback metadata
([`test_budget_reservation.py`](https://github.com/BerriAI/litellm/blob/c274cf321c5c35c629220a89bb497d15b56f870f/tests/test_litellm/proxy/test_budget_reservation.py),
[`test_router_max_parallel_requests.py`](https://github.com/BerriAI/litellm/blob/c274cf321c5c35c629220a89bb497d15b56f870f/tests/local_testing/test_router_max_parallel_requests.py),
[`test_router_streaming_fallback_metadata.py`](https://github.com/BerriAI/litellm/blob/c274cf321c5c35c629220a89bb497d15b56f870f/tests/test_litellm/test_router_streaming_fallback_metadata.py)).

**Fit and failure risk.** Its generic retry/fallback authority cannot know
Sentinel's durable action/effect boundary. A retry after ambiguous dispatch may
double provider charge even if the proxy suppresses duplicate local logging.
Its provider, auth, spend, cache, and enterprise surface is a larger dependency
and operations boundary than Sentinel needs.

**Decision.** `REJECT` integration and sidecar authority. `PORT` only the
reserve/reconcile/cancel idea. Sentinel's atomic multi-scope store operation,
stable identities, restart recovery, and exactly-once reconciliation are
stronger Sentinel-owned contracts, not LiteLLM behavior.

## Mechanism and decision matrices

### Mechanism matrix

| Mechanism | Sentinel now | Strongest reviewed source | Decision | Authority after decision |
|---|---|---|---|---|
| Provider/model policy | Immutable catalog and hierarchy mapping | LiteLLM router is broader | `KEEP` | Cortex Gateway |
| Token-free readiness | Inventory/Gate B/local-loop | Engines expose health/model routes | `KEEP` | Cortex Gateway |
| Active-call limit | Go semaphore-like manager; Rust semaphore | TGI fail-fast; SGLang channel/semaphore | `REIMPLEMENT` finite policy | Gateway plus #764 policy |
| Waiting queue | Unbounded Go waiter slice | TGI immediate reject; SGLang bounded channel but unbounded submit futures | `PORT` primitives plus hard ingress cap | Cortex Gateway |
| Queue cancel race | Go cancellation/grant handling | SGLang drop cancellation | `KEEP` and expand tests | #769 evidence |
| Optional capabilities/status | Streaming and status reporter lost by wrapper | Engine/provider stream and status APIs | `REIMPLEMENT` exact wrapper matrix | Cortex Gateway |
| Stream cancellation | HTTP context and write error | SGLang upstream cancel test | `PORT` terminal semantics | Cortex Gateway |
| Stream usage/cost | Missing from single response sink | LiteLLM process-local reconciliation idea | `REIMPLEMENT` | Gateway adapter + #695/EventStore |
| Budget admission | In-memory check then record | LiteLLM reservation idea | `PORT` idea, strengthen durability | #695 + Rust/EventStore |
| Retry/failover | Pre-dispatch budget fallback only | LiteLLM broad retries | `REJECT` ambiguous retry | Rust/EventStore attempt outcome |
| Circuit breaking | Gateway and bridge local breakers | Engines have backend health | `KEEP`, make outcome-aware | Gateway |
| Request/effect identity | Stable Rust request ID/digest | No engine knows Sentinel effect | `KEEP` | Rust bridge/event store |
| Usage authority | Gateway response plus durable event | Engines report token counters | `KEEP`; adapt reports | Event/projection |
| Prompt-cache hints | Anthropic cache blocks and usage split | Provider APIs | `KEEP` | Gateway/provider |
| KV/prefix cache | No productive local engine cache | vLLM/SGLang | `WRAP`, post-M0 | Engine ephemeral state |
| Multi-LoRA | Filesystem prototype only | vLLM/SGLang engine mechanisms | `REJECT` for M0 | Post-M0 engine decision |
| Speculative decode | String/subprocess prototype | Engine-native schedulers | `REJECT` prototype | Post-M0 engine decision |
| Structured output | Capability flags plus downstream validation | Engine grammar features | `KEEP` validation; wrap grammar | #695 |
| Outbox delivery | At-least-once publish/mark | Not an engine concern | `KEEP` | #732/#733 |

### Dependency, security, and operations matrix

| Candidate | Dependency/operations cost | Security posture at pin | Cache/admission value | Sentinel decision |
|---|---|---|---|---|
| vLLM | Python/CUDA/runtime image and GPU-specific deployment | Apache-2.0, security policy present | Strong scheduler, preemption, prefix cache | Post-M0 external wrapper candidate |
| SGLang | Python/CUDA plus Rust gateway and broad runtime surface | Apache-2.0, no repo security policy | Strong radix cache/cancellation; bounded channel but unbounded submit futures | Post-M0 wrapper; port primitives only |
| TGI | Rust/Python/CUDA service image | Apache-2.0, no repo security policy | Typed overload; unbounded tokenizer channels | Reject adoption |
| llama.cpp | C/C++ build and model-file operations; portable local lane | MIT, limited security policy | Slot/cache server, unbounded task deques | Post-M0 external wrapper candidate |
| LiteLLM | Large Python proxy, provider/auth/storage operations | Mixed tree; MIT non-enterprise, separate enterprise terms | Useful reservation idea; finalized guard is process-local | Reject authority; port idea only |
| Dynamo | Distributed router/KV/control infrastructure | Apache-2.0, security policy present | Advanced disaggregated routing | Too early; post-M0 background |
| TensorRT-LLM | NVIDIA-specific compiler/runtime stack | Apache-2.0, security policy present | High-performance executor/KV stack | Reject as portable default |
| LMDeploy | Python/CUDA/TurboMind service | Apache-2.0; no reviewed security file | Similar engine scheduling | No unique accepted mechanism |
| Ollama | Existing local service boundary | MIT, security policy present | Existing adapter/inventory | Keep adapter only |
| LocalAI | Broad backend/proxy service | MIT, security policy present | Duplicates provider routing | Reject |

### Butterfly-effect matrix

| Change | Direct files/contract | Downstream effects | Guardrail |
|---|---|---|---|
| Finite Gateway queue | `forwardqueue`, composition config, metrics | External callers receive overload; Rust urgent/normal policy sees typed outcomes | Versioned defaults, class-specific policy, no silent drop |
| Preserve optional interfaces | `queued_provider`, provider capability tests | Anthropic stream and Claude-Code typed cooldown/retry status survive wrapping | Test all inventory/stream/status combinations without invented capabilities |
| Parse terminal stream usage | Claude adapter and response sink | Cost projection and budget reconciliation change; partial streams become billable outcomes | Raw chunks are not durable effects; outcome is bound to admission/attempt/reservation digests |
| Durable budget reservation | #695 plus shared event/schema and Gateway port | Concurrent cost admission, restart recovery, projection, operator diagnostics | Atomic integer-unit multi-scope reserve, exactly-once reconcile, release/quarantine |
| Typed attempt outcome | Go response and Rust bridge/event | Retry/failover, circuit breaker, outbox, observability | Unknown after dispatch is `AMBIGUOUS`, never inferred safe |
| Engine adapter | provider catalog/config/deployment | Model inventory, readiness, image/license/CVE, cache metrics | #705 decision, target benchmark, rollback to prior provider |
| Capability digest | catalog/capability schema | Readiness and routing reject drift | Bind to semantic catalog digest; no mutable undocumented capability |
| Remove/reclassify prototypes | workspace/dependency audit only | Build graph and future architecture docs | #705 owns; no pruning in #714 |

## Accepted API, schema, and state-machine proposals

### `InferenceAdmissionV1`

```text
version
admission_id
admission_digest
request_id
request_digest
request_class
agent_id_optional
provider_id
model_id
catalog_digest
capability_digest
hierarchy_tier_optional
max_input_tokens_optional_u64
max_output_tokens_u64
provider_execution_deadline_unix_ms
queue_policy_id
budget_reservation_id_optional
budget_exemption:
  NONE | NON_BILLABLE_LOCAL_LOOP | NON_BILLABLE_FAKE_PROVIDER_TEST
budget_exemption_reason_digest_optional
```

Validation rejects missing identity, unknown enum/version, digest mismatch,
uncataloged provider/model, expired deadline, negative limits, and a reservation
bound to another request digest. `admission_digest` is SHA-256 over the canonical
V1 admission fields, including catalog, capability, reservation or exemption,
and provider-execution deadline. A billable admission without an authoritative
reservation fails closed. Exemption is an allowlisted, digest-bound reason; an
arbitrary caller string cannot create a free route.

### `BudgetAuthorityPortV1`

The Go Gateway calls a versioned port backed by #695's Rust/EventStore authority
before dispatching any billable external or internal request. The request carries
the canonical admission, required budget scopes, and maximum integer cost. The
response is either a bound `BudgetReservationV1`, a typed deny, or a validated
non-billable exemption. Port timeout/unavailability rejects billable admission.
Gateway memory may cache neither balance nor authorization. This solves external
compatibility routes without moving durable authority into Go.

### `ProviderAttemptOutcomeV1`

```text
version
outcome_operation_id
admission_id
admission_digest
request_id
request_digest
attempt_id
attempt_digest
provider_id
model_id
catalog_digest
capability_digest
budget_reservation_id_optional
budget_exemption_optional
dispatch_state:
  NOT_DISPATCHED | DISPATCHED | DEFINITIVE_REJECT | COMPLETED | AMBIGUOUS
terminal_reason:
  OVERLOADED | DEADLINE_BEFORE_DISPATCH | CLIENT_CANCEL_BEFORE_DISPATCH |
  PROVIDER_REJECT | PROVIDER_SUCCESS | CLIENT_CANCEL_AFTER_DISPATCH |
  DEADLINE_AFTER_DISPATCH | TRANSPORT_LOST | INVALID_RESPONSE
provider_request_id_optional
occurred_at
```

`attempt_digest` is SHA-256 over the canonical admission binding, reservation or
exemption, provider/model/catalog/capability binding, dispatch state, terminal
reason, provider request ID, and occurrence time. `outcome_operation_id` is
stable for that attempt and is the idempotency key for append.

No component may convert `AMBIGUOUS` to `NOT_DISPATCHED`. A fallback attempt is
allowed only for `NOT_DISPATCHED` or a provider-specific `DEFINITIVE_REJECT`
contract proven non-billable.

### `UsageOutcomeV1`

```text
usage_operation_id
attempt_id
attempt_digest
budget_reservation_id
input_tokens_u64
output_tokens_u64
cache_read_input_tokens_u64
cache_creation_input_tokens_u64
reported_cost_microusd_u64_optional
resolved_cost_microusd_u64
cost_source: PROVIDER_REPORTED | CATALOG_COMPUTED | CONSERVATIVE_RESERVED
terminal: true
partial_stream: bool
```

One micro-USD is one millionth of a US dollar. Integers avoid float/NaN,
rounding, and cross-language comparison ambiguity. The usage operation is bound
to exactly one attempt digest and reservation.

Only a terminal usage outcome can reconcile a reservation. Missing provider
usage uses catalog cost when token counts are trustworthy; otherwise the
conservative reservation remains quarantined for operator reconciliation.

### `BudgetReservationV1`

```text
reservation_id
request_id
request_digest
admission_id
admission_digest
scopes[]:
  scope_kind: PROVIDER | PROJECT | AGREEMENT | CUSTOMER
  scope_id
  scope_generation_u64
  window_kind: LIFETIME | CALENDAR_HOUR | CALENDAR_DAY | FIXED_RANGE
  window_start_unix_ms
  window_end_unix_ms_optional
reserved_microusd_u64
estimated_input_microusd_u64
status:
  RESERVED | PRE_DISPATCH_RELEASED | RECONCILED | QUARANTINED
expires_at
reconciled_usage_operation_id_optional
quarantine_reason_optional
```

Reserve and compare are one atomic store operation across applicable budget
scopes. Reconciliation is idempotent by reservation and usage operation IDs.
Pre-dispatch rejection/cancellation moves `RESERVED` to
`PRE_DISPATCH_RELEASED`. Post-dispatch cancellation retains known or estimated
input cost and reconciles only from a definitive terminal outcome. Expiry never
silently refunds an ambiguous attempt; it becomes `QUARANTINED`.

### `ProviderCapabilitiesV1`

The semantic catalog binds supported request format, streaming, usage-in-stream,
structured output, tool use, inventory, cache accounting, cancellation, and
definitive-rejection semantics. It also binds typed provider status and
`retry_after_ms` reporting. A wrapper must preserve every currently supported
optional interface combination, including `ModelInventoryProvider`,
`StreamingProvider`, and `ProviderStatusReporter`, without inventing or dropping
a capability.

### Attempt state machine

| From | Event/guard | To | Durable action | Retry/fallback |
|---|---|---|---|---|
| `RECEIVED` | schema/catalog/capability valid | `VALIDATED` | persist/bind admission digest | none |
| `VALIDATED` | non-billable exemption valid | `ADMITTED_EXEMPT` | persist exemption reason digest | no reservation needed |
| `VALIDATED` | billable and atomic reserve succeeds | `BUDGET_RESERVED` | persist all scope/window generations | none |
| `VALIDATED` | billable, deny/port unavailable | `REJECTED` | typed deny; no provider call | no |
| `BUDGET_RESERVED` | queue full, deadline, or cancel before dispatch | `PRE_DISPATCH_RELEASED` | atomically release reservation | a new admission may retry |
| `ADMITTED_EXEMPT` or `BUDGET_RESERVED` | provider dispatch starts | `DISPATCHED` | append bound attempt operation | no automatic retry |
| `DISPATCHED` | proven non-billable provider reject | `DEFINITIVE_REJECT` | release reservation exactly once | policy may create a new admission |
| `DISPATCHED` | terminal provider result and usage | `COMPLETED` | append usage operation and reconcile reservation exactly once | no |
| `DISPATCHED` | timeout, disconnect, lost/invalid terminal state | `AMBIGUOUS` | quarantine reservation and attempt | never automatic |
| `COMPLETED` | durable usage reconciled | `USAGE_RECONCILED` | stable usage operation readback | no |
| `USAGE_RECONCILED` | action claim succeeds | `EFFECT_RECOVERED` | one durable action/effect claim | no |

`AMBIGUOUS` has no transition to usage or effect without explicit authoritative
reconciliation evidence. Restart resumes from admission, reservation, attempt,
usage, and effect operation IDs. No state is reconstructed from process memory.

## Negative and failure matrix

| Schedule/failure | Required outcome | Forbidden outcome |
|---|---|---|
| Queue at capacity | Typed 429/overload with bounded metadata | Append unbounded waiter or silent drop |
| Waiter context cancels before grant | Remove waiter and never call provider | Consume a permit or dispatch later |
| Grant races cancellation | Exactly one grant/release outcome | Permit leak or duplicate dispatch |
| Streaming wrapper around streaming provider | Capability preserved and permit held until terminal | 502 due to wrapper type loss |
| Status-reporting provider behind wrapper | Typed cooldown status and retry-after preserved | Generic 503 caused by type loss |
| Wrapper around non-stream/non-status provider | Unsupported interfaces remain absent | Invented interface/capability |
| Billable route without reservation | Fail closed before dispatch | In-memory check or provider call |
| Non-billable exemption | Allowlisted reason and admission digest agree | Caller-defined free route |
| Budget scope generation/window mismatch | Reject reservation and append | Spend against stale scope |
| Client disconnect before dispatch | No provider call; reservation released | Billable attempt or retry |
| Client disconnect after dispatch | Cancel upstream, persist terminal/ambiguous outcome, reconcile incurred cost | Full refund or blind retry |
| Provider deadline before headers | Outcome depends on dispatch acknowledgement; unknown is ambiguous | Assume non-billable |
| Provider 429 before execution | Definitive reject only when adapter contract proves it | Cross-provider retry from status code alone |
| SSE ends without terminal usage | Quarantine conservative reservation | Record zero cost |
| Duplicate terminal chunk | One usage reconciliation | Duplicate charge/event |
| Gateway restart after dispatch | Rust bridge remains fail closed on `provider_in_flight` | Repeat provider call |
| Restart after completion commit | Recover one usage and one action claim | Lose or duplicate effect |
| Catalog/capability digest drift | Readiness/routing fail closed | Mutate capability map silently |
| Budget store unavailable | Reject cost-bearing request | In-memory fail-open |
| Concurrent near-limit requests | Atomic reservations keep total within ceiling | All requests pass stale balance |
| Fractional/NaN/overflow cost | Canonical checked micro-USD integer rejects input | Float coercion or wraparound |
| Outbox publish then crash | Duplicate delivery absorbed by stable operation ID | Duplicate usage/effect |
| Engine cache eviction/restart | Recompute prompt/KV only | Change durable request/effect identity |
| Provider/model removed | Token-free readiness or catalog validation fails | Send to uncataloged model |

## M0 classification and owner routing

| Finding | Class | Rationale | Proposed owner |
|---|---|---|---|
| Concurrent budget check/record can bypass cost ceiling | `BLOCKS_M0` pending #695 acknowledgement | #695 explicitly requires cost ceiling under concurrency | Precise #695 delta or successor |
| Productive queue wrapper drops streaming and status interfaces | `M0_HARDENING` | Compatibility streaming breaks and Claude-Code typed cooldown status is hidden | Proposed G1, tests #769 |
| Go waiter queue has no capacity bound | `M0_HARDENING` | External/internal pressure can exhaust memory; no current runtime evidence was taken | #764 policy, proposed G1 |
| Stream terminal usage/cost absent | `M0_HARDENING` | Streaming can incur cost outside the single sink | Proposed G1 plus #695/#732 |
| Blind post-dispatch failover must remain forbidden | `M0_HARDENING` | Prevents future target drift from creating duplicate charge | #695/#732 |
| Capability map not bound to catalog digest | `M0_HARDENING` | Mutable source map can disagree with productive wrapper capability | #732 schema delta plus G1 |
| vLLM/SGLang/llama.cpp engine selection | `POST_M0` | Requires target hardware, security, dependency, and rollback evidence | #705/#656 decision gate |
| Prefix/KV reuse, Multi-LoRA, speculative decode, grammar engine | `POST_M0` | Performance/engine mechanisms do not block product semantics | #705 and later engine owner |
| `sentinel-inference` prototype disposition | `POST_M0` | No productive path; audit before retain/rewrite/remove | #705 |
| Provider-independent catalog/tiering | `M0_HARDENING`, delivered | Existing verified contract should remain unchanged | #395 history, #650 |
| Durable request/effect/usage completion | `BLOCKS_M0`, implemented | Load-bearing no-duplicate authority; regression protection continues | #732/#733/#695 |

`AC-5`, `AC-6`, and `AC-7` remain pending maintainer approval, live
materialization, and owner acknowledgement. The classifications above are the
research recommendation, not owner acceptance.

## Proposed implementation-owner contracts

Materialization is forbidden until ORC approval. There is no new coordination
epic and no duplicate durable-budget child. Existing owners receive precise
deltas; only G1 is genuinely uncovered Go implementation work.

```text
#732 schema/append delta S0
  +-> #695 cost/attempt delta C0
  +-> proposed G1 Go Gateway implementation

#733 consumes S0 events and owns outbox/consumer outcomes.
#764 supplies pressure policy to G1.
#769 supplies deterministic Go schedules to G1.

C0 and G1 implementation may proceed in parallel after S0.
Billable G1 activation depends on the authoritative C0 port being live.
```

This graph is acyclic. S0 is schema authority, C0 is durable budget/attempt
authority, and G1 is edge implementation. G1 may be built against fake S0/C0
ports in parallel, but its billable producer flag cannot activate before C0.
Cross-language behavior is bound by versioned vectors, not shared implementation.

### Existing-owner delta S0: #732 schemas, validators, and append

**Owned write scope:** the exact shared schema, append validation, fixtures, and
schema-only Go mirrors assigned by #732; no queue, provider, budget policy,
bridge orchestration, outbox, or projection implementation.
**Dependencies:** #732 canonical envelope/append authority; #705 only if a new
dependency is proposed. S0 blocks C0/G1 production of V1 records.
**Deliverables:** versioned `InferenceAdmissionV1`,
`ProviderAttemptOutcomeV1`, `UsageOutcomeV1`, `BudgetReservationV1`, and
`ProviderCapabilitiesV1`; canonical JSON vectors and invalid fixtures; unknown
version/field policy; schema digest.

**Acceptance:**

1. Rust and Go accept every valid vector and reject every invalid vector with a
   typed reason.
2. Request, catalog, provider, model, attempt, reservation, and usage operation
   identities are bound and validated.
3. `AMBIGUOUS` cannot be transformed to a retry-safe state.
4. Capability digest includes stream usage and definitive-rejection semantics.
5. CI path routing runs both language validators for schema/vector changes.

**Negative tests:** unknown version/enum/field; missing digest; admission,
attempt, reservation, catalog, or capability digest mismatch; reservation rebound
to another request; negative/overflow integer cost; terminal usage without a
terminal attempt; invented wrapper capability; ambiguous marked non-billable;
float/NaN cost representation; billable admission without reservation.

**Runtime target block:** `NONE`; deploy, read-only, and benchmark targets none;
`.155`, `.240`, `.241`, `.242`, providers, and Proxmox are forbidden. Local
deterministic fixtures only. Rollback owner is the S0 implementer by PR revert.
**Benchmark:** structural vector count and schema-size ceiling only; no timing.
**Rollout/rollback:** readers accept old plus V1 before producers emit V1;
rollback stops V1 production without deleting records.
**Evidence:** exact vector hashes, validator outputs, CI paths, one-authority
matrix.
**TOGAF target delta:** once approved, immediately add the versioned
admission/attempt/usage authority target to both language copies.

### Proposed new child G1: Go Gateway bounded admission and streaming

**Owned write scope:** `cmd/cortex-gateway` queue, provider wrappers, stream
adapter, Gateway metrics/config/tests only. No Rust, event store, projection, or
TOGAF file.
**Parent/dependencies:** new child under #650; research #714; S0 schema port;
#764 pressure policy; #769 deterministic Go schedules. Implementation may run in
parallel with C0, but billable activation depends on C0's live authoritative
reservation port.
**Deliverables:** finite queue capacity and class limits; typed overload and
retry-after metadata; cancellation-safe grant; exact optional-interface wrapper
matrix; stream terminal parser/outcome; request/cost propagation through the S0
contract and C0 port.

**Acceptance:**

1. Active and waiting work are both bounded by configuration with safe defaults.
2. Queue-full is typed and never calls a provider.
3. Every supported inventory/stream/status optional-interface combination is
   preserved exactly, including Claude-Code cooldown status and retry-after.
4. A stream holds one permit until EOF/error/cancel and emits exactly one
   terminal outcome.
5. Client disconnect cancels upstream work and distinguishes pre/post dispatch.
6. Non-stream and stream share request identity, catalog digest, cost source, and
   terminal usage rules.
7. Readiness remains token-free.
8. Every billable route obtains a bound C0 reservation before dispatch or fails
   closed. Only digest-bound local-loop/fake-provider exemptions bypass it.

**Negative tests:** full ingress and full waiter cap; cancel-before-grant;
grant/cancel race; timeout while waiting; all optional-interface combinations;
status reporter with typed 429/503 and retry-after; duplicate terminal SSE;
missing usage; disconnect after headers; provider 429 with and without definitive
non-billable contract; queue-config overflow; C0 deny/timeout/unavailable;
unrecognized exemption; readiness attempts provider generation. Use Go
`testing/synctest`, race detector, and deterministic barriers under #769; no
sleeps as proof.

**Runtime target block:** `SINGLE_NODE`; deploy and benchmark target `.240`;
read-only target `.240`; forbidden `.155`, `.241`, and `.242`; no real provider call.
Create an issue-specific `.240` snapshot before deployment. Use token-free
`local-loop` and fake HTTP providers for queue, stream, status, cancellation,
reservation-port, and restart probes.

**Benchmark contract:** on `.240`, report issue-specific p50/p95/max for queue
wait, Gateway handler completion, and cancel propagation; maximum queue/waiter
cardinality, goroutine count, RSS, open connections, and status-map cardinality;
typed overload count; provider call count in reject paths. Include exact workload
shape, sample count, warm-up, limits, and raw normalized output. Build-server
timings and upstream numbers are invalid.

**Rollout:** snapshot; deploy complete affected Gateway/daemon/store set; enable
metrics and bounded non-billable paths; validate S0/C0 port fail-closed behavior;
then enable billable producer only after C0 readback. Scan health/restarts/logs
and compare usage/cardinality before approval.

**Rollback:** disable billable/stream producer flags, restore prior complete
service set or issue snapshot, verify queue/process/store health, then revert G1.
The G1 implementation owner performs and records rollback. Never convert
quarantined attempts during rollback.

**Evidence:** local schedule/race tests; `.240` snapshot/deploy/rollback commands;
token-free queue/cancel/stream/status/restart outputs; p50/p95/max and resource/
cardinality data; zero provider calls in reject paths; S0/C0 readback.
**TOGAF target delta:** once approved, immediately add bounded admission,
provider-execution deadline, status/retry-after, and stream-terminal targets to
both language copies.

### Existing-owner delta C0: #695 durable budget and attempt reconciliation

**Owned write scope:** daemon bridge and the exact persistence/projection files
assigned by #695, using #732 append schemas and #733 delivery outcomes. No Go
queue/provider implementation and no parallel budget owner.
**Ownership/dependencies:** append this precise delta to active #695. If #695
finishes before the delta can land, create an explicit successor linked after
#695 rather than a parallel child. S0 precedes V1 production; #733 remains the
outbox/consumer authority. C0 and G1 implementation may run in parallel after
S0, but C0 blocks billable G1 activation.
**Deliverables:** atomic budget reservation; attempt/outcome persistence;
idempotent terminal usage reconciliation; cancellation and restart recovery;
typed bridge response handling; authoritative `BudgetAuthorityPortV1`.

**Acceptance:**

1. Concurrent reservations cannot exceed an accepted budget scope.
2. Reservation is durable before provider dispatch and bound to request digest.
3. Every scope kind, generation, and time window is part of one atomic integer
   micro-USD comparison/reservation.
4. Definitive completion reconciles exactly once to provider-reported or catalog
   cost.
5. Pre-dispatch cancellation releases exactly once; post-dispatch cancellation
   retains incurred input or conservative cost.
6. Ambiguous dispatch remains quarantined across restart and cannot retry,
   reconcile usage, or recover effects without authoritative evidence.
7. Completion recovery produces exactly one usage event and one action claim.
8. Outbox redelivery is absorbed by stable operation IDs.
9. Port/store unavailable fails closed for every billable route.

**Negative tests:** concurrent last-budget race; crash before/after reservation;
crash before/after dispatch; timeout with unknown provider state; duplicate
terminal outcome; mismatched request digest; duplicate outbox; projection
restart; expired ambiguous reservation; stale scope generation/window; float,
negative, overflow, or malformed micro-USD; provider cost outside catalog sanity
bounds; billable admission without reservation; arbitrary exemption reason.

**Runtime target block:** `SINGLE_NODE`; deploy and benchmark target `.240`;
read-only target `.240`; forbidden `.155`, `.241`, and `.242`; no real provider call.
Create an issue-specific snapshot, deploy the complete affected daemon/Gateway/
store/projection set, and use local-loop/fake-provider journeys for concurrent
reserve, cancel, crash, restart, outbox replay, and projection readback.

**Benchmark contract:** on `.240`, report p50/p95/max reservation-port and
reconciliation latency; maximum outstanding reservation/attempt rows, recovery
batch, RSS, task/thread count, and event/projection cardinality; exact workload,
sample count, warm-up, and raw normalized output. Prove totals remain equal
across restart/redelivery. No build-server timing.

**Rollout:** snapshot; land S0 readers; deploy C0 shadow reservation and compare
against existing #695 cost decisions; enable authoritative reserve/reconcile;
then permit G1 billable activation. Readiness fails closed on incompatible
schema/port/store.

**Rollback:** disable new billable admission, drain definitive records, preserve
and expose quarantined attempts, restore the complete service set or snapshot,
then revert C0. The #695/C0 owner performs and records rollback. Never delete or
refund ambiguous records automatically.

**Evidence:** `.240` snapshot/deploy/rollback commands; token-free state
transitions; store/event/projection readbacks; restart/redelivery histories;
p50/p95/max plus resources/cardinality; zero duplicate provider/effect/usage.
**TOGAF target delta:** once approved, immediately add durable integer-unit
multi-scope reservation, attempt transition, and safe-failover targets to both
language copies.

### Existing-owner deltas after approval

- **#695:** own C0 `BudgetReservationV1`, `BudgetAuthorityPortV1`, and canonical
  attempt reconciliation as its existing provider/project cost-ceiling
  implementation; require schema-validated actions and add concurrent reserve,
  cancellation, restart, and ambiguous-dispatch negative ACs. If timing requires
  follow-up, create a successor after #695 rather than parallel authority.
- **#696:** consume exact S0/C0/G1 release evidence and preserve model/catalog/
  request lineage in delivery records.
- **#705:** decide retain/rewrite/remove for `sentinel-inference`; separately
  decide any vLLM/SGLang/llama.cpp adapter dependency with license, security,
  image, CVE, owner, update, migration, and rollback evidence.
- **#656:** only after #705 accepts a dependency, own update cadence and
  compatibility matrices.
- **#732:** own S0 canonical attempt/usage/reservation schema and append
  validation, not provider or budget policy.
- **#733:** own durable delivery, retry outcome, and consumer idempotency for new
  events, not provider retry.
- **#758:** consume bounded queue/attempt/reservation counters and causal IDs;
  never become a second business-state store.
- **#764:** define pressure tiers and admission policy inputs consumed by G1.
- **#769:** add wrapper inventory/stream/status combinations, ingress/waiter
  capacity, grant/cancel, timeout, retry-after, and disconnect schedules. It
  remains test ownership, not production implementation.

Closed #395 receives a follow-up link only if materialization is approved; its
historical body and status must not be rewritten.

## Rollout, rollback, and benchmark decision gates

### M0 hardening rollout

1. After architecture/owner approval, update both TOGAF target-language copies;
   do not wait for implementation evidence.
2. Land S0 readers, invalid fixtures, append validation, and CI paths.
3. Build C0 and G1 in parallel behind independent producer flags. G1 may exercise
   only non-billable local-loop/fake-provider paths until C0 is live.
4. Compare old usage totals with C0 V1 reconciliation using local deterministic
   fakes; any unexplained difference blocks enforcement.
5. Snapshot `.240`, deploy complete affected sets, and pass C0/G1 live,
   restart, rollback, p50/p95/max, and resource/cardinality contracts.
6. Enable C0 authoritative reservation after existing pending completions
   reconcile.
7. Enable G1 billable admission only after successful C0 port/readback.
8. Enable streaming terminal accounting only after wrapper composition and
   disconnect schedules pass; #696 consumes the complete release evidence.

Rollback never retries ambiguous attempts, deletes reservations, or switches to
an uncataloged provider. It disables new producers, retains compatible readers,
drains known terminal work, and reverts the owning PR.

### Post-M0 engine selection gate

No engine is selected until an approved target declares:

- CPU/GPU model and memory, driver/runtime, container and filesystem boundary;
- exact model, quantization, context, request-class mix, prompt-prefix mix, and
  output-token distribution;
- warm/cold cache protocol, cancellation rate, overload schedule, restart and
  cache-eviction schedule;
- time-to-first-token, inter-token latency, end-to-end latency, throughput,
  admission rejection, queue depth, memory, cache hit/eviction, correctness,
  cancellation residue, and usage reconciliation;
- security exposure, model provenance, license, SBOM/CVE, upgrade owner, and
  rollback to the prior provider.

The comparison must use identical Sentinel provider contracts and request
vectors. Engine-native benchmark numbers, developer machines, `.155`, and
upstream claims are not acceptance evidence.

## Acceptance-criteria mapping

| Criterion | State | Evidence |
|---|---|---|
| AC-1 Sentinel baseline | `PASS` | End-to-end path, exact impact map, gaps/non-gaps, TOGAF and live owner map |
| AC-2 ecosystem scan | `PASS` | Ten immutable candidates, screening rubric, shortlist/rejections |
| AC-3 deep reviews | `PASS` | Five pinned reviews covering source, tests, failures, security, license, operations |
| AC-4 complete mechanism matrix | `PASS` | Mechanism, dependency/security/operations, butterfly, schema, and failure matrices |
| AC-5 explicit decision per mechanism | `PENDING_MAINTAINER_APPROVAL` | Twelve executive decisions and mechanism matrix await ORC decision |
| AC-6 accepted gap has live quality owner | `PENDING_MATERIALIZATION` | Complete #732 S0, #695 C0, and sole new G1 contracts exist only in this study |
| AC-7 M0 classification and acknowledgement | `PENDING_OWNER_ACK` | Every finding classified; live owners have not yet acknowledged deltas |
| AC-8 public-safe study | `PASS` after final gates | One English ASCII document, no secrets, provider calls, copied code, or runtime data |
| AC-N1 no popularity dependency | `PASS` | Rubric and #705 gate; no dependency mutation |
| AC-N2 provenance/license/security | `PASS` | Immutable pins and governance evidence for every deep review |
| AC-N3 tests/status not proof | `PASS` | Limits and source-backed composition findings are explicit |
| AC-N4 no invalid performance evidence | `PASS` | No runtime/build/upstream timing used as Sentinel evidence |
| AC-N5 no ownerless accepted gap | `PASS` as proposal | Every gap maps to existing owners or a complete proposed child contract |

## Reproduction and verification

### Baseline and provenance

```bash
git rev-parse HEAD
git status --short
gh issue view 714 --json number,state,title,body,labels
rg -n "NewQueuedProvider|StreamingProvider|streamAnthropicResponse" cmd/cortex-gateway
rg -n "reserve_request|provider_in_flight|enqueue_completion|persist_usage" \
  services/sentinel-daemon/src/llm_bridge.rs
rg -n "AgentLlmUsage|operation_id" \
  crates/sentinel-common crates/sentinel-projection services/sentinel-daemon
rg -n "sentinel-inference|SGLang|KVFlow|provider failover|query deadlines" \
  docs/architecture/togaf-architecture-guide.html
```

The upstream pin commands are listed in the landscape section. A reproducible
deep verifier should run `git cat-file -e <pin>:<path>` for every linked source,
test, license, and security path, then verify every cited line range lies within
the pinned object.

### Required final gates

```bash
# Exact one-file scope
git diff --name-only <final-base>...HEAD

# ASCII and public safety
LC_ALL=C grep -nP '[^\x00-\x7F]' \
  docs/research/oss/inference-cortex-gateway.md

# Local source and line anchors, immutable upstream objects, external URLs,
# GFM rendering, required sections/counts, and negative fixtures
python3 <private-verifier> --all \
  docs/research/oss/inference-cortex-gateway.md

typos docs/research/oss/inference-cortex-gateway.md
git diff --check <final-base>...HEAD

# GitHub readback
gh pr view <pr> --json headRefOid,baseRefOid,isDraft,mergeable,mergeStateStatus,closingIssuesReferences,statusCheckRollup
```

Negative structure fixtures must reject:

- fewer than five deep reviews or a mutable upstream reference;
- a deep review without source, test, failure, security, license, or operations;
- an accepted mechanism without exactly one decision;
- a finding without M0 class and owner;
- any claim that an engine owns provider policy, budget, durable effect, or
  canonical usage;
- automatic retry/failover from `AMBIGUOUS`;
- an unbounded accepted queue;
- engine/upstream/build-server timing presented as Sentinel evidence;
- a live materialization claim while AC-5/AC-6/AC-7 remain pending;
- a new coordination epic or parallel durable-budget owner beside #650/#695;
- a cyclic S0/C0/G1 owner graph or billable G1 activation before C0;
- a repository file outside this document.

## Known limits

- This is source and test analysis, not a deployment or performance evaluation.
- Upstream commits are snapshots; a later dependency decision must refresh
  security, license, maintenance, and source evidence.
- The productive streaming wrapper defect is inferred from Go interface
  composition and tests. No provider or runtime was called.
- Provider billing semantics differ. A definitive non-billable outcome requires
  provider-specific evidence; HTTP status alone is insufficient.
- No target hardware/model mix was authorized, so vLLM, SGLang, and llama.cpp
  remain candidates rather than a ranking.
- AC-5, AC-6, and AC-7 are intentionally not claimed complete before ORC
  approval, live issue materialization, and owner acknowledgement.
