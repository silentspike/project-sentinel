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
   observable pressure. Go owns edge queuing and deterministically proposes a
   provider/model route and bounded execution deadline. C0 is final admission
   authority: it independently validates hierarchy/policy routing, allowed
   capability/catalog/pricing generations, token ceilings, and a policy-clamped
   maximum deadline. Port only channel/semaphore ideas from SGLang and fail-fast
   overload semantics from TGI; neither proves bounded ingress.
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
   remain Sentinel's budget authority. Go submits an untrusted intent bound to a
   #695 governance receipt; C0 derives all required scopes and computes the
   conservative maximum before final admission. LiteLLM must not become routing
   authority.
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
| `cmd/cortex-gateway/internal/capability/detection.go:5-103` | Capability map | Capabilities are mutable, hand-maintained process state and are not bound to the catalog digest | Materialized #732 schema delta |
| `cmd/cortex-gateway/internal/forwardqueue/manager.go:37-96` | Admission | Concurrency is bounded, but the waiter slice has no capacity limit; cancellation/grant race is handled | #764 pressure; #769 schedules |
| `cmd/cortex-gateway/internal/proxy/queued_provider.go:10-53` | Queue wrapper | Preserves inventory only; it drops `StreamingProvider` and `ProviderStatusReporter` | [#773](https://github.com/silentspike/project-sentinel/issues/773) |
| `cmd/cortex-gateway/internal/proxy/provider_test.go:49-67` | Wrapper test | Proves inventory preservation only | #773; #769 |
| `cmd/cortex-gateway/internal/proxy/pipeline.go:420-449` | Circuit-open response | Uses `ProviderStatusReporter` for Claude-Code typed 429/503 status; the productive wrapper hides it | #773 |
| `cmd/cortex-gateway/internal/proxy/pipeline_test.go:482-529` | Stream test | Registers an unwrapped mock, so productive queue composition is not covered | #773; #769 |
| `cmd/cortex-gateway/internal/proxy/pipeline.go:1082-1103` | Budget fallback | Budget exhaustion may switch providers before dispatch; there is no general provider retry loop | #695 |
| `cmd/cortex-gateway/internal/proxy/pipeline.go:1454-1531` | Response sink | Non-stream terminal usage/cost has a single process-local sink | #695, #758 |
| `cmd/cortex-gateway/internal/proxy/pipeline.go:1689-1747` | Streaming | Requires an optional interface and records latency/count only; it does not parse terminal usage or enter the response sink | #773 plus #695/#732 |
| `cmd/cortex-gateway/internal/proxy/claude.go:120-206` | Anthropic non-stream | Context deadline, response bound, provider usage and cache-token split | Cortex Gateway |
| `cmd/cortex-gateway/internal/proxy/claude.go:247-334` | Anthropic stream | Relays raw SSE and propagates write/read error; no terminal usage reconciliation | #773 plus #695/#732 |
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

1. Define Cortex Gateway as the edge-queue authority and deterministic proposer
   of provider/model route and bounded provider-execution deadline. Define C0 as
   final admission authority: it independently validates the exact hierarchy/
   policy-to-provider/model mapping, allowed capability/catalog/pricing
   generations, token ceilings, and policy-clamped maximum deadline. A stale or
   compromised Gateway cannot select a merely existing but disallowed/expensive
   model or extend the deadline. Caller deadlines remain end-to-end policy;
   there is no Gateway-owned global deadline.
2. Define engines as replaceable execution adapters. Engine queue/KV cache state
   is ephemeral and cannot become event, budget, request, or ownership authority.
3. Define Rust/EventStore as the durable request/effect/usage, atomic
   budget-reservation, and canonical attempt-outcome authority. Every billable
   Gateway route must obtain an authoritative reservation through the shared
   port before dispatch or fail closed; a digest-bound non-billable exemption is
   the only exception. The Gateway submits only an untrusted intent. #695
   governance determines mandatory scope generations and C0 computes the
   conservative integer maximum from pinned catalog/pricing and policy token
   ceilings before final admission.
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
| [#650](https://github.com/silentspike/project-sentinel/issues/650) | M0 product-acceptance epic | Native parent of the approved G1 child #773. |
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
| Provider/model policy | Immutable catalog and hierarchy mapping | LiteLLM router is broader | `KEEP`; Gateway proposes, C0 final-validates | #395 policy plus C0 admission |
| Token-free readiness | Inventory/Gate B/local-loop | Engines expose health/model routes | `KEEP` | Cortex Gateway |
| Active-call limit | Go semaphore-like manager; Rust semaphore | TGI fail-fast; SGLang channel/semaphore | `REIMPLEMENT` finite policy | Gateway plus #764 policy |
| Waiting queue | Unbounded Go waiter slice | TGI immediate reject; SGLang bounded channel but unbounded submit futures | `PORT` primitives plus hard ingress cap | Cortex Gateway |
| Queue cancel race | Go cancellation/grant handling | SGLang drop cancellation | `KEEP` and expand tests | #769 evidence |
| Optional capabilities/status | Streaming and status reporter lost by wrapper | Engine/provider stream and status APIs | `REIMPLEMENT` exact wrapper matrix | Cortex Gateway |
| Stream cancellation | HTTP context and write error | SGLang upstream cancel test | `PORT` terminal semantics | Cortex Gateway |
| Stream usage/cost | Missing from single response sink | LiteLLM process-local reconciliation idea | `REIMPLEMENT` | Gateway adapter + #695/EventStore |
| Budget admission | In-memory check then record | LiteLLM reservation idea | `PORT` idea; use intent/reserve/finalize with governance-derived scopes/cost | #695 + Rust/EventStore |
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
| Parse terminal stream usage | Claude adapter and response sink | Cost projection and budget reconciliation change; partial streams become billable outcomes | Raw chunks are not durable effects; usage binds stable attempt plus terminal outcome and reservation/exemption |
| Final route/deadline admission | Gateway proposal plus #395/#695 policy and C0 | Stale/compromised Gateway, expensive route, deadline and token bounds | C0 independently resolves policy mapping and clamps final fields |
| Durable budget reservation | #695 plus shared event/schema and authority port | Concurrent cost admission, restart recovery, projection, operator diagnostics | Immutable create plus append-only transitions in one C0 SQLite transaction |
| Complete mutation port | Go Gateway to C0 | Every provider-call, outcome, usage, retry and crash boundary | Six authenticated/versioned methods; fresh durable dispatch ACK before I/O |
| Typed attempt outcome | Go response and Rust bridge/event | Retry/failover, circuit breaker, outbox, observability | Stable attempt binding; separate dispatch/terminal operations; predecessor CAS |
| Engine adapter | provider catalog/config/deployment | Model inventory, readiness, image/license/CVE, cache metrics | #705 decision, target benchmark, rollback to prior provider |
| Capability digest | catalog/capability schema | Readiness and routing reject drift | Bind to semantic catalog digest; no mutable undocumented capability |
| Remove/reclassify prototypes | workspace/dependency audit only | Build graph and future architecture docs | #705 owns; no pruning in #714 |

## Accepted API, schema, and state-machine proposals

### S0 canonical digest codec

Rust and Go must hash identical bytes, not merely equivalent JSON. S0 therefore
uses [RFC 8949 deterministic CBOR](https://www.rfc-editor.org/rfc/rfc8949.html#section-4.2)
with this exact preimage:

```text
ASCII "sentinel.inference.control"
0x00
u16be(record_type_ascii_byte_length)
record_type_ascii_bytes
u16be(schema_version)
u32be(deterministic_cbor_payload_byte_length)
deterministic_cbor_payload
```

`record_type` is the exact V1 type name, including `V1`; it and
`schema_version` provide cross-record and cross-version domain separation. The
payload is a definite-length CBOR map whose keys are the exact ASCII field names
shown in the schema. RFC 8949 core deterministic map ordering applies. The
record's own digest field is omitted from its hash payload; every referenced
record/operation digest remains included. SHA-256 over the complete preimage is
stored as exactly 32 bytes and rendered as lowercase 64-character hex only at
text/API boundaries.

The value rules are closed:

- strings are valid UTF-8 in Unicode NFC; validators reject non-NFC input rather
  than silently normalizing it;
- authority IDs and enum symbols are non-empty ASCII and at most 128 bytes;
  provider request IDs are NFC UTF-8 and at most 512 bytes;
- optional fields are represented only by absence; CBOR `null` is forbidden;
- unsigned values use shortest-form CBOR `uint64`; negative integers, bignums,
  tags, floating values, NaN, and infinity are forbidden;
- timestamps are Unix milliseconds encoded as `uint64`;
- booleans are CBOR booleans, never integer or string aliases;
- byte digests are exactly 32 bytes;
- scope arrays are sorted by `(scope_kind, scope_id, scope_generation_u64,
  window_kind, window_start_unix_ms, window_end_unix_ms_optional)` and reject
  duplicate or out-of-order rows; other arrays preserve schema-defined semantic
  order;
- unknown, duplicate, or case-variant fields are rejected before hashing.

S0 owns strict Rust/Go encoders, decoders, and golden vectors containing the
record type, version, canonical CBOR hex, and SHA-256 for every record,
reservation transition, and port request/response. Required mutation vectors
cover cross-record type confusion, omitted-versus-null ambiguity, reordered
scopes, non-NFC Unicode, `uint64` overflow, unknown/duplicate fields, float
injection, and self-digest inclusion. Either language accepting a byte sequence
the other rejects blocks production.

### `AdmissionIntentV1`

```text
version
admission_intent_id
admission_intent_digest
request_id
request_digest
request_class
agent_id_optional
caller_service_identity
authenticated_principal_digest
tenant_id
project_id
work_item_id
agreement_id
customer_id
governance_receipt_id
governance_receipt_digest
governance_generation_u64
provider_id_proposal
model_id_proposal
catalog_digest_proposal
capability_digest_proposal
pricing_digest_proposal
hierarchy_tier_optional
requested_max_input_tokens_optional_u64
requested_max_output_tokens_u64
provider_execution_deadline_proposal_unix_ms
queue_policy_id
```

The intent is phase one and contains no reservation ID, reservation digest,
required-scope list, or caller-selected maximum cost. Provider, model, and token
limits are untrusted proposals. The governance receipt is the authoritative
#695 binding for tenant, project, work item, agreement, customer, policy
generation, and applicable budget policy. C0 validates all claimed identities
against that receipt. `admission_intent_digest` uses the domain-separated S0
codec above and excludes only its own digest field. Validation rejects unknown
version/field, a missing or stale
governance receipt, receipt/identity mismatch, unauthorized caller, unrecognized
or policy-disallowed provider/model route, stale catalog/pricing/capability
generation, token ceiling above policy, and deadline above the policy-clamped
maximum.

### `BudgetReservationV1`

```text
version
reservation_id
reservation_digest
admission_intent_id
admission_intent_digest
request_id
request_digest
governance_receipt_id
governance_receipt_digest
governance_generation_u64
provider_id
model_id
hierarchy_policy_digest
routing_policy_generation_u64
catalog_digest
capability_digest
pricing_digest
effective_max_input_tokens_u64
effective_max_output_tokens_u64
effective_provider_execution_deadline_unix_ms
scopes[]:
  scope_kind: TENANT | PROJECT | WORK_ITEM | AGREEMENT | CUSTOMER | PROVIDER
  scope_id
  scope_generation_u64
  window_kind: LIFETIME | CALENDAR_HOUR | CALENDAR_DAY | FIXED_RANGE
  window_start_unix_ms
  window_end_unix_ms_optional
reserved_microusd_u64
estimated_input_microusd_u64
expires_at_unix_ms
```

C0 derives every mandatory scope from the authoritative governance receipt and
rejects missing, additional, mismatched, foreign-tenant/project, or stale-
generation scope material. It computes the conservative integer micro-USD
maximum itself from the validated provider/model, catalog and pricing digests,
effective policy token ceilings, and checked arithmetic. The Gateway cannot
submit a scope list or maximum-cost override. Reserve and compare are one atomic
store operation across all applicable scopes.

The immutable `reservation_digest` binds the intent, governance receipt,
independently validated hierarchy/routing policy and generations, execution
selection, effective token/deadline ceilings, complete derived scope/window set,
reserved amount, and expiry. It contains no mutable status, reconciliation, or
quarantine field.

### `BudgetReservationTransitionV1`

```text
version
transition_operation_id
transition_payload_digest
reservation_id
reservation_digest
expected_predecessor_operation_id
expected_predecessor_state: RESERVED
from_state: RESERVED
to_state:
  PRE_DISPATCH_RELEASED | DEFINITIVE_NON_BILLABLE_RELEASED |
  RECONCILED | QUARANTINED
transition_reason:
  QUEUE_FULL_BEFORE_DISPATCH | CLIENT_CANCEL_BEFORE_DISPATCH |
  DEADLINE_BEFORE_DISPATCH | PROVIDER_DEFINITIVE_NON_BILLABLE |
  PROVIDER_USAGE_RECONCILED | CLIENT_CANCEL_AFTER_DISPATCH |
  DEADLINE_AFTER_DISPATCH | TRANSPORT_LOST |
  INVALID_PROVIDER_RESPONSE | GATEWAY_LOST_AFTER_DISPATCH_COMMIT |
  EXPIRED_WITH_UNKNOWN_OUTCOME
authority_evidence_digest_optional
occurred_at_unix_ms
```

The creation record is immutable and represents initial `RESERVED`; its
`reservation_id` is the predecessor identity for the first transition. Every
later status is an immutable append-only transition. C0 appends the transition
and updates the reservation aggregate/projection in the same EventStore SQLite
transaction; no event row is updated or deleted. Repeating the same operation
and payload is idempotent. Reusing its ID with a different payload, naming a
wrong predecessor, or taking an illegal edge is rejected. The optional evidence
is a fixed 32-byte digest of bounded diagnostics or authority proof and cannot
change the typed decision.

Reservation state/reason is a closed legal matrix:

| From | To | Allowed transition reason |
|---|---|---|
| `RESERVED` | `PRE_DISPATCH_RELEASED` | `QUEUE_FULL_BEFORE_DISPATCH`, `CLIENT_CANCEL_BEFORE_DISPATCH`, `DEADLINE_BEFORE_DISPATCH` |
| `RESERVED` | `DEFINITIVE_NON_BILLABLE_RELEASED` | `PROVIDER_DEFINITIVE_NON_BILLABLE` |
| `RESERVED` | `RECONCILED` | `PROVIDER_USAGE_RECONCILED` |
| `RESERVED` | `QUARANTINED` | `CLIENT_CANCEL_AFTER_DISPATCH`, `DEADLINE_AFTER_DISPATCH`, `TRANSPORT_LOST`, `INVALID_PROVIDER_RESPONSE`, `GATEWAY_LOST_AFTER_DISPATCH_COMMIT`, `EXPIRED_WITH_UNKNOWN_OUTCOME` |

All four destination states are terminal for automatic processing. Any other
state/reason combination requires a separately approved operator-recovery
contract and is rejected by V1.

### `BudgetExemptionV1`

```text
version
exemption_id
exemption_digest
admission_intent_id
admission_intent_digest
governance_receipt_id
governance_receipt_digest
governance_generation_u64
exemption_kind:
  NON_BILLABLE_LOCAL_LOOP | NON_BILLABLE_FAKE_PROVIDER_TEST
authorized_service_identity
authorized_reason_digest
expires_at_unix_ms
```

An exemption is phase-two authority output, never a free-form Gateway field.
C0 validates the authenticated caller, allowlisted route/provider, governance
generation, and bounded expiry. Exemptions are valid only for deterministic
local-loop or test-fake execution and cannot authorize an external provider.

### `InferenceAdmissionV1`

```text
version
admission_id
admission_digest
admission_intent_id
admission_intent_digest
request_id
request_digest
provider_id
model_id
hierarchy_policy_digest
routing_policy_generation_u64
catalog_digest
capability_digest
pricing_digest
effective_max_input_tokens_u64
effective_max_output_tokens_u64
provider_execution_deadline_unix_ms
queue_policy_id
budget_reservation_id_optional
budget_reservation_digest_optional
budget_exemption_id_optional
budget_exemption_digest_optional
finalized_at_unix_ms
```

This is phase three. Exactly one reservation pair or exemption pair is required.
`admission_digest` binds the immutable intent digest to that prior authority
record and the validated execution fields; neither prior record includes the
final admission digest. This ordering is constructible and acyclic:
`AdmissionIntentV1 -> BudgetReservationV1 or BudgetExemptionV1 ->
InferenceAdmissionV1`. Rebinding an intent, reservation, or exemption to a
different admission is rejected. A billable admission without a reservation
fails closed. C0 finalization independently resolves the exact #395 hierarchy/
policy route, checks provider/model allowlisting and every catalog, capability,
pricing, and routing generation, applies policy token ceilings, and clamps the
deadline to the caller, Gateway proposal, and policy maximum. The resulting
fields are authoritative; the intent proposals are not.

### `AdmissionDispositionV1`

```text
version
disposition_operation_id
disposition_payload_digest
admission_id
admission_digest
expected_predecessor_state: FINAL_ADMITTED
disposition:
  PRE_DISPATCH_REJECTED | PRE_DISPATCH_CANCELLED |
  PRE_DISPATCH_DEADLINE_EXCEEDED
disposition_reason:
  QUEUE_FULL | AUTHORITY_DENIED | CLIENT_CANCELLED |
  EXECUTION_DEADLINE_EXPIRED
diagnostic_digest_optional
budget_reservation_id_optional
budget_reservation_digest_optional
budget_exemption_id_optional
budget_exemption_digest_optional
occurred_at_unix_ms
```

This terminal operation covers the race after final admission but before
provider dispatch. It competes by CAS with `ProviderDispatchReceiptV1` for the
same `FINAL_ADMITTED` predecessor, so release and dispatch cannot both win. A
reservation-backed disposition is coupled to a
`BudgetReservationTransitionV1` to `PRE_DISPATCH_RELEASED`; an exempt
disposition records no budget mutation. `disposition_reason` is a closed enum.
The optional diagnostic is a fixed 32-byte digest of bounded non-authoritative
details, never free text.

### `InferenceAuthorityPortV1`

```text
version
authority_request_digest
method:
  RESERVE_OR_EXEMPT | FINALIZE_ADMISSION |
  WIN_PRE_DISPATCH_DISPOSITION | BEGIN_DISPATCH |
  COMMIT_ATTEMPT_OUTCOME | RECONCILE_USAGE
caller_service_identity
authenticated_principal_digest
idempotency_key
record_type
record_id
record_payload_digest
expected_predecessor_operation_id_optional
expected_predecessor_state_optional
typed_payload
```

The Go Gateway uses one authenticated, versioned C0 mutation port backed by
#695's Rust/EventStore authority. Every request binds service identity, method,
idempotency key, record type/ID, canonical payload digest, and any required
predecessor. `authority_request_digest` uses the S0 codec over the complete
envelope except itself, so caller, method, key, record/payload digest, and
predecessor cannot be rebound independently. Unknown port or record versions and
unknown methods are rejected. The idempotency lookup key is
`(service, method, key, record_type, record_id)`. Same key plus the same
authority-request digest returns the original typed result; a different digest
returns `IDEMPOTENCY_CONFLICT`; stale predecessor returns `STALE_PREDECESSOR`.

Typed responses are:

```text
version
result:
  COMMITTED | REPLAYED_READBACK | DENIED | IDEMPOTENCY_CONFLICT |
  STALE_PREDECESSOR | ILLEGAL_TRANSITION | UNKNOWN_VERSION |
  UNKNOWN_METHOD | UNAUTHORIZED | UNAVAILABLE
committed_operation_id_optional
committed_payload_digest_optional
aggregate_state_optional
provider_io_authorized: bool
```

Only a fresh `COMMITTED` response to `BEGIN_DISPATCH` may set
`provider_io_authorized=true`. `REPLAYED_READBACK` is evidence of durable state
but never authorizes another provider call.

| Port method | Input record | C0 durable result |
|---|---|---|
| `RESERVE_OR_EXEMPT` | `AdmissionIntentV1` | Validate caller, governance, route proposals, generations, token/deadline bounds; append `BudgetReservationV1` or `BudgetExemptionV1`, or deny |
| `FINALIZE_ADMISSION` | intent plus reservation/exemption identities and finalization request | Independently resolve and validate route/policy; append authoritative `InferenceAdmissionV1` |
| `WIN_PRE_DISPATCH_DISPOSITION` | `AdmissionDispositionV1` | Win CAS against `FINAL_ADMITTED`; append disposition and, when reserved, release transition in one transaction |
| `BEGIN_DISPATCH` | `ProviderDispatchReceiptV1` | Win CAS against `FINAL_ADMITTED`; append dispatch receipt and aggregate update in one transaction |
| `COMMIT_ATTEMPT_OUTCOME` | `ProviderAttemptOutcomeV1` | Win CAS against `DISPATCHED`; append terminal outcome plus required release/quarantine transition and aggregate update in one transaction |
| `RECONCILE_USAGE` | `UsageOutcomeV1` | Validate `COMPLETED`; append usage plus reservation reconciliation transition and aggregate/outbox rows in one transaction, or validate exemption |

The exact network order is:

1. G1 wins its bounded edge queue and sends `RESERVE_OR_EXEMPT`.
2. After its commit ACK, G1 sends `FINALIZE_ADMISSION` and waits for commit ACK.
3. Before provider I/O, G1 sends exactly one of
   `WIN_PRE_DISPATCH_DISPOSITION` or `BEGIN_DISPATCH`.
4. G1 may open the provider connection or write provider bytes only after a
   fresh `BEGIN_DISPATCH` `COMMITTED` ACK with
   `provider_io_authorized=true`. Provider I/O before that ACK is forbidden.
5. A provider response is followed by `COMMIT_ATTEMPT_OUTCOME`; only a committed
   `COMPLETED` outcome may be followed by `RECONCILE_USAGE`.

If `BEGIN_DISPATCH` commits but its ACK is lost, a replay can read back the
commit but cannot authorize I/O. If the Gateway crashes after durable dispatch
and before or during the provider call, recovery cannot distinguish those
boundaries: C0 records `AMBIGUOUS` plus `QUARANTINED`, and no component
automatically retries. Port timeout/unavailability otherwise fails closed.
Gateway memory may cache neither balance nor authorization.

### `ProviderDispatchReceiptV1`

```text
version
dispatch_operation_id
dispatch_payload_digest
admission_id
admission_digest
attempt_id
attempt_binding_digest
expected_predecessor_state: FINAL_ADMITTED
provider_id
model_id
catalog_digest
capability_digest
budget_reservation_id_optional
budget_reservation_digest_optional
budget_exemption_id_optional
budget_exemption_digest_optional
provider_request_id_optional
occurred_at_unix_ms
```

`attempt_binding_digest` is stable for the entire attempt and covers admission,
provider/model/catalog/capability, and exactly one reservation or exemption
binding. It contains no transition state, reason, provider request ID, or time.
The dispatch receipt is the sole transition from `FINAL_ADMITTED` to
`DISPATCHED`; append uses compare-and-set against the expected predecessor.
Duplicate operation ID plus identical payload is idempotent. A different payload
or predecessor is rejected.

### `ProviderAttemptOutcomeV1`

```text
version
outcome_operation_id
outcome_payload_digest
admission_id
admission_digest
request_id
request_digest
attempt_id
attempt_binding_digest
dispatch_operation_id
expected_predecessor_state: DISPATCHED
provider_id
model_id
catalog_digest
capability_digest
budget_reservation_id_optional
budget_reservation_digest_optional
budget_exemption_id_optional
budget_exemption_digest_optional
terminal_state:
  DEFINITIVE_REJECT | COMPLETED | AMBIGUOUS
terminal_reason:
  PROVIDER_DEFINITIVE_NON_BILLABLE_REJECT | PROVIDER_SUCCESS |
  CLIENT_CANCEL_AFTER_DISPATCH | DEADLINE_AFTER_DISPATCH |
  TRANSPORT_LOST | INVALID_RESPONSE |
  GATEWAY_LOST_AFTER_DISPATCH_COMMIT
provider_request_id_optional
authority_evidence_digest_optional
occurred_at_unix_ms
```

There is exactly one terminal outcome after a dispatch receipt.
`outcome_operation_id` is the stable idempotency identity for that terminal
transition; `outcome_payload_digest` binds its canonical payload, including
terminal state, reason, provider request ID, and Unix-ms time. Append compares
the referenced dispatch operation and `DISPATCHED` predecessor. Duplicate
operation plus identical payload is idempotent; different payload, stale
predecessor, a second terminal outcome, or an outcome before dispatch is
rejected.

Terminal state/reason is a closed legal matrix:

| Terminal state | Allowed reason |
|---|---|
| `DEFINITIVE_REJECT` | `PROVIDER_DEFINITIVE_NON_BILLABLE_REJECT` |
| `COMPLETED` | `PROVIDER_SUCCESS` |
| `AMBIGUOUS` | `CLIENT_CANCEL_AFTER_DISPATCH`, `DEADLINE_AFTER_DISPATCH`, `TRANSPORT_LOST`, `INVALID_RESPONSE`, or `GATEWAY_LOST_AFTER_DISPATCH_COMMIT` |

Every other state/reason pair is invalid. The optional authority evidence is a
fixed 32-byte digest, not free text.

No component may convert `AMBIGUOUS` to a retry-safe state. A fallback attempt
is allowed only before a dispatch receipt exists or after a provider-specific
`DEFINITIVE_REJECT` contract proven non-billable.

### `UsageOutcomeV1`

```text
version
usage_operation_id
usage_payload_digest
attempt_id
attempt_binding_digest
terminal_outcome_operation_id
terminal_outcome_payload_digest
budget_reservation_id_optional
budget_reservation_digest_optional
budget_exemption_id_optional
budget_exemption_digest_optional
input_tokens_u64
output_tokens_u64
cache_read_input_tokens_u64
cache_creation_input_tokens_u64
reported_cost_microusd_u64_optional
resolved_cost_microusd_u64
cost_source: PROVIDER_REPORTED | CATALOG_COMPUTED | CONSERVATIVE_RESERVED
terminal: true
partial_stream: bool
occurred_at_unix_ms
```

One micro-USD is one millionth of a US dollar. Integers avoid float/NaN,
rounding, and cross-language comparison ambiguity. The usage operation is bound
to exactly one stable attempt binding, one `COMPLETED` terminal outcome, and
exactly one reservation or exemption. Exempt local-loop/fake usage must bind
the authorized exemption, omit reservation fields, and resolve to zero billable
micro-USD. Billable usage must bind a reservation and cannot carry an exemption.

Only a terminal usage outcome can reconcile a reservation. Missing provider
usage uses catalog cost when token counts are trustworthy; otherwise the
conservative reservation remains quarantined for operator reconciliation.
Reconciliation uses CAS from `RESERVED` with the stable usage operation and
payload digest. Pre-dispatch rejection/cancellation moves `RESERVED` to
`PRE_DISPATCH_RELEASED`; a proven post-dispatch non-billable rejection moves it
to `DEFINITIVE_NON_BILLABLE_RELEASED`. Post-dispatch cancellation retains known
or estimated input cost and reconciles only from a definitive terminal outcome.
Expiry never silently refunds an ambiguous attempt; it becomes `QUARANTINED`.

### `ProviderCapabilitiesV1`

```text
version
provider_id
model_id
catalog_digest
request_format_digest
supports_streaming
supports_usage_in_stream
supports_structured_output
supports_tool_use
supports_inventory
supports_cache_accounting
supports_cancellation
supports_definitive_rejection
supports_status_reporting
supports_retry_after
capability_digest
```

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
| `RECEIVED` | intent/schema/catalog/capability/governance valid | `INTENT_VALIDATED` | `RESERVE_OR_EXEMPT` validates `AdmissionIntentV1` | none |
| `INTENT_VALIDATED` | authoritative non-billable exemption succeeds | `EXEMPT_AUTHORIZED` | append `BudgetExemptionV1` | no reservation needed |
| `INTENT_VALIDATED` | billable atomic reserve succeeds | `RESERVED` | append immutable `BudgetReservationV1` and aggregate initial state | none |
| `INTENT_VALIDATED` | caller/governance invalid, billable deny, or port unavailable | `REJECTED` | typed deny; no final admission/provider call | no |
| `EXEMPT_AUTHORIZED` or `RESERVED` | C0 route/policy finalization validates | `FINAL_ADMITTED` | `FINALIZE_ADMISSION` appends authoritative `InferenceAdmissionV1` | none |
| `FINAL_ADMITTED` | queue full, deadline, or cancel wins CAS | `PRE_DISPATCH_REJECTED` | `WIN_PRE_DISPATCH_DISPOSITION` appends disposition plus release transition atomically when reserved | a new intent may retry |
| `FINAL_ADMITTED` | `BEGIN_DISPATCH` wins CAS and commit ACK reaches live handler | `DISPATCHED` | append dispatch receipt and aggregate; only fresh commit ACK authorizes provider I/O | no automatic retry |
| `DISPATCHED` | proven non-billable provider reject | `DEFINITIVE_REJECT` | `COMMIT_ATTEMPT_OUTCOME` appends outcome plus release transition atomically | policy may create a new intent |
| `DISPATCHED` | terminal provider success | `COMPLETED` | `COMMIT_ATTEMPT_OUTCOME` appends one terminal outcome by CAS | no |
| `DISPATCHED` | timeout, disconnect, crash, lost/invalid terminal state | `AMBIGUOUS` | `COMMIT_ATTEMPT_OUTCOME` appends outcome plus quarantine transition atomically | never automatic |
| `COMPLETED` | durable usage validates | `USAGE_RECONCILED` | `RECONCILE_USAGE` appends usage plus reconcile transition and outbox atomically, or validates exemption | no |
| `USAGE_RECONCILED` | action claim succeeds | `EFFECT_RECOVERED` | one durable action/effect claim | no |

`AMBIGUOUS` has no transition to usage or effect without explicit authoritative
reconciliation evidence. Every row names the persisted V1 record or reservation
status operation that performs it. Restart resumes from intent, reservation or
exemption, final admission, dispatch, terminal outcome, usage, and effect
operation IDs. No state is reconstructed from process memory.

Persistence identity is one-to-one with the state machine:

| Persistent record or operation | Stable idempotency identity | Payload binding / legal predecessor |
|---|---|---|
| `AdmissionIntentV1` | `admission_intent_id` | `admission_intent_digest`; no reservation fields |
| `BudgetReservationV1` create | `reservation_id` | `reservation_digest`; validated intent and governance receipt |
| `BudgetExemptionV1` create | `exemption_id` | `exemption_digest`; validated intent and authorized caller |
| `InferenceAdmissionV1` finalize | `admission_id` | `admission_digest`; exactly one prior reservation or exemption |
| `AdmissionDispositionV1` | `disposition_operation_id` | `disposition_payload_digest`; `FINAL_ADMITTED`, mutually exclusive with dispatch |
| `BudgetReservationTransitionV1` | `transition_operation_id` | `transition_payload_digest`; CAS from named predecessor operation/state |
| `ProviderDispatchReceiptV1` | `dispatch_operation_id` | `dispatch_payload_digest`; `FINAL_ADMITTED` |
| `ProviderAttemptOutcomeV1` | `outcome_operation_id` | `outcome_payload_digest`; matching dispatch in `DISPATCHED` |
| `UsageOutcomeV1` | `usage_operation_id` | `usage_payload_digest`; matching `COMPLETED` outcome |

For every row, replaying the same identity and digest returns the original
result. Reusing an identity with a different digest is a typed conflict. A stale
predecessor, skipped phase, or second terminal transition is rejected without a
provider call, budget mutation, usage append, or effect recovery.

### C0 transaction and recovery boundary

C0 is the sole mutation owner. It extends the canonical `sentinel-limbo`
append-only SQLite EventStore gateway; S0 owns codec/schema/append validation,
not transactions or business decisions. Each port method executes one fenced
SQLite transaction containing all of its applicable control records,
reservation aggregate update, attempt/admission aggregate update, canonical
domain event, and pending outbox row. Commit is the port ACK point. There is no
cross-database or cross-store atomicity claim.

The coupled writes are:

| Method | One C0 SQLite/EventStore transaction |
|---|---|
| `RESERVE_OR_EXEMPT` | immutable reservation or exemption, initial aggregate state, canonical event, pending outbox |
| `FINALIZE_ADMISSION` | final admission, admission aggregate CAS, canonical event, pending outbox |
| `WIN_PRE_DISPATCH_DISPOSITION` | disposition, reservation release transition when present, admission and reservation aggregate CAS, canonical event, pending outbox |
| `BEGIN_DISPATCH` | dispatch receipt, attempt/admission aggregate CAS, canonical event, pending outbox |
| `COMMIT_ATTEMPT_OUTCOME` | terminal outcome, release or quarantine transition when required, attempt and reservation aggregate CAS, canonical event, pending outbox |
| `RECONCILE_USAGE` | usage, reconciliation transition when billable, usage/reservation aggregate CAS, canonical event, pending outbox |

Append-only events and transitions are never updated. The authoritative
aggregate rows are transactionally advanced projections of that history.
External projections and transport publication occur after commit. Their
consumers use operation ID plus payload digest, so replay is safe; they cannot
authorize provider I/O or change the C0 aggregate.

Required failpoints and recovery are:

| Failpoint | Durable observation after restart | Recovery |
|---|---|---|
| Before transaction begin | no new record or aggregate state | caller may submit the same operation |
| After one append but before aggregate/outbox insert | transaction rolls back completely | same operation may retry |
| After aggregate update but before commit | transaction rolls back completely | same operation may retry |
| After commit but before port ACK | complete records, aggregate, event, and pending outbox exist | same request returns `REPLAYED_READBACK`; `BEGIN_DISPATCH` readback does not authorize provider I/O |
| After dispatch commit ACK and before/during provider I/O | durable `DISPATCHED`, provider execution unknown | append `AMBIGUOUS` plus quarantine in one outcome transaction; never auto-retry |
| After provider response but before outcome commit | durable `DISPATCHED`, response not authoritative | recover as `AMBIGUOUS`; never infer success or retry |
| After outcome commit but before usage reconciliation | durable terminal outcome | retry `RECONCILE_USAGE` only for `COMPLETED` with the same IDs/digests |
| After commit but before outbox publication | pending outbox row exists | publisher retries; consumer deduplicates |
| After outbox publication but before published marker | duplicate transport delivery possible | consumer absorbs same operation/digest; conflicting digest rejects |
| Before or after external projection apply | canonical EventStore state remains authoritative | projection resumes from its durable offset |

Failpoint tests must cover every row. No recovery path reconstructs an authority
decision from Gateway memory or an external projection.

## Negative and failure matrix

| Schedule/failure | Required outcome | Forbidden outcome |
|---|---|---|
| Queue at capacity | Typed 429/overload with bounded metadata | Append unbounded waiter or silent drop |
| Required C0 mutation port method missing/unknown | Readiness and request fail closed with `UNKNOWN_METHOD` | Fall back to direct store write or provider call |
| Waiter context cancels before grant | Remove waiter and never call provider | Consume a permit or dispatch later |
| Grant races cancellation | Exactly one grant/release outcome | Permit leak or duplicate dispatch |
| Streaming wrapper around streaming provider | Capability preserved and permit held until terminal | 502 due to wrapper type loss |
| Status-reporting provider behind wrapper | Typed cooldown status and retry-after preserved | Generic 503 caused by type loss |
| Wrapper around non-stream/non-status provider | Unsupported interfaces remain absent | Invented interface/capability |
| Billable route without reservation | Fail closed before dispatch | In-memory check or provider call |
| Intent contains caller-selected scope list or maximum cost | Reject unsupported authority input; C0 derives both | Honor a reduced scope set or under-reserve |
| Unauthorized local caller or foreign tenant/project | Reject before reservation/exemption | Trust process locality or caller claims |
| Missing/additional/stale governance scope generation | Derive and compare all receipt-required scopes; typed reject | Omit agreement/customer scope or spend against stale policy |
| Provider/model/catalog/pricing/token proposal mismatch | C0 validates selection and computes conservative maximum | Reserve caller-proposed lower amount |
| Existing but hierarchy/policy-disallowed or expensive route | C0 denies final admission | Trust Gateway proposal because model exists |
| Gateway deadline exceeds caller/policy maximum | C0 clamps or denies final admission | Persist arbitrary long provider deadline |
| Port idempotency key replay with different intent digest | Typed conflict | Return the prior reservation for new intent |
| Non-billable exemption | C0-issued exemption, caller identity, governance generation, and intent digest agree | Caller-defined free route |
| Intent/reservation/admission digest cycle | Reject vector; reservation binds intent and final admission binds reservation | Require a not-yet-constructible digest |
| Reservation or exemption rebound to another final admission | Typed conflict | Torn phase or cross-request authority reuse |
| Final admission has both/neither reservation and exemption | Reject before dispatch | Ambiguous budget authority |
| Pre-dispatch disposition races dispatch receipt | Exactly one CAS winner; release only when disposition wins | Provider call after release or leaked reservation |
| Provider I/O before fresh `BEGIN_DISPATCH` commit ACK | Test barrier proves zero provider calls | Send headers/body on request submission or readback |
| `BEGIN_DISPATCH` commits but ACK times out | Readback cannot authorize I/O; quarantine as ambiguous | Treat replay as permission to call provider |
| Budget scope generation/window mismatch | Reject reservation operation | Spend against stale scope |
| Client disconnect before dispatch | No provider call; reservation released | Billable attempt or retry |
| Client disconnect after dispatch | Cancel upstream, persist terminal/ambiguous outcome, reconcile incurred cost | Full refund or blind retry |
| Provider deadline before headers | Outcome depends on dispatch acknowledgement; unknown is ambiguous | Assume non-billable |
| Provider 429 before execution | Definitive reject only when adapter contract proves it | Cross-provider retry from status code alone |
| SSE ends without terminal usage | Quarantine conservative reservation | Record zero cost |
| Duplicate dispatch/outcome/usage operation, same payload | Idempotent original result | Second state change or charge |
| Duplicate operation ID, different payload | Typed digest conflict | Last-write-wins mutation |
| Reservation transition mutates creation event | Append immutable transition and atomically advance aggregate | Update/delete prior event row |
| Coupled record/transition failpoint before commit | Entire C0 transaction rolls back | Partial disposition/release, outcome/quarantine, or usage/reconcile |
| Crash after commit before outbox/projection publication | Pending outbox and canonical aggregate recover | Cross-store compensation or lost event |
| Terminal outcome with stale predecessor or before dispatch | CAS reject | Out-of-order terminal append |
| Second terminal outcome | CAS reject after first terminal state | Rewrite `COMPLETED` as `AMBIGUOUS` or vice versa |
| Invalid terminal state/reason pair | Closed matrix rejects record | `COMPLETED` with timeout/reject reason |
| Free-text authority reason or oversized diagnostic | Reject schema; accept only enum plus 32-byte digest | Persist unbounded text in authority decision |
| Exempt usage with reservation or wrong exemption | Reject usage | Bill or launder authority across routes |
| Billable usage without reservation | Reject usage and quarantine attempt | Append unbound cost/effect |
| Gateway restart after dispatch | Rust bridge remains fail closed on `provider_in_flight` | Repeat provider call |
| Restart after completion commit | Recover one usage and one action claim | Lose or duplicate effect |
| Catalog/capability digest drift | Readiness/routing fail closed | Mutate capability map silently |
| Budget store unavailable | Reject cost-bearing request | In-memory fail-open |
| Concurrent near-limit requests | Atomic reservations keep total within ceiling | All requests pass stale balance |
| Fractional/NaN/overflow cost | Canonical checked micro-USD integer rejects input | Float coercion or wraparound |
| Cross-record digest type/version confusion | Domain-separated digest mismatch | Accept identical payload bytes under another record type/version |
| Omitted optional versus CBOR null | Omitted form only; null rejects | Hash both as equivalent |
| Reordered/duplicate scopes | Reject non-canonical array | Compute a language-dependent digest |
| Non-NFC Unicode, uint64 overflow, unknown/duplicate field, or float | Both codecs reject identically | Normalize silently, wrap, ignore, or coerce |
| Outbox publish then crash | Duplicate delivery absorbed by stable operation ID | Duplicate usage/effect |
| Engine cache eviction/restart | Recompute prompt/KV only | Change durable request/effect identity |
| Provider/model removed | Token-free readiness or catalog validation fails | Send to uncataloged model |

## M0 classification and owner routing

| Finding | Class | Rationale | Owner |
|---|---|---|---|
| Concurrent budget check/record can bypass cost ceiling | `BLOCKS_M0` routed to #695 | #695 explicitly requires cost ceiling under concurrency | Materialized #695 C0 addendum |
| Productive queue wrapper drops streaming and status interfaces | `M0_HARDENING` | Compatibility streaming breaks and Claude-Code typed cooldown status is hidden | #773, tests #769 |
| Go waiter queue has no capacity bound | `M0_HARDENING` | External/internal pressure can exhaust memory; no current runtime evidence was taken | #764 policy, #773 |
| Stream terminal usage/cost absent | `M0_HARDENING` | Streaming can incur cost outside the single sink | #773 plus #695/#732 |
| Blind post-dispatch failover must remain forbidden | `M0_HARDENING` | Prevents future target drift from creating duplicate charge | #695/#732 |
| Capability map not bound to catalog digest | `M0_HARDENING` | Mutable source map can disagree with productive wrapper capability | #732 schema delta plus #773 |
| vLLM/SGLang/llama.cpp engine selection | `POST_M0` | Requires target hardware, security, dependency, and rollback evidence | #705/#656 decision gate |
| Prefix/KV reuse, Multi-LoRA, speculative decode, grammar engine | `POST_M0` | Performance/engine mechanisms do not block product semantics | #705 and later engine owner |
| `sentinel-inference` prototype disposition | `POST_M0` | No productive path; audit before retain/rewrite/remove | #705 |
| Provider-independent catalog/tiering | `M0_HARDENING`, delivered | Existing verified contract should remain unchanged | #395 history, #650 |
| Durable request/effect/usage completion | `BLOCKS_M0`, implemented | Load-bearing no-duplicate authority; regression protection continues | #732/#733/#695 |

ORC approved the twelve decisions and this classification/owner split. Live
materialization routes S0 to #732, C0 to #695, the sole uncovered Go
implementation to #773, and narrow release consumption to #696. This approval
does not authorize implementation outside those issue contracts.

## Approved implementation-owner contracts

ORC approved materialization without a new coordination epic or duplicate
durable-budget child. Existing owners received precise deltas; #773 is the only
new issue because G1 is the only genuinely uncovered Go implementation work.

```text
#732 schema/append delta S0
  +-> #695 final-admission/cost/attempt delta C0
  +-> #773 G1 Go Gateway edge/proposal implementation

#733 consumes S0 events and owns outbox/consumer outcomes.
#764 supplies pressure policy to G1.
#769 supplies deterministic Go schedules to G1.

C0 and G1 implementation may proceed in parallel after S0.
Billable G1 activation depends on the authoritative C0 port being live.
```

This graph is acyclic. S0 is codec/schema authority, C0 is final-admission and
durable budget/attempt/transaction authority, and G1 is edge queue plus
route/deadline proposer. G1 may be built against fake S0/C0 ports in parallel,
but its billable producer flag cannot activate before C0. Cross-language
behavior is bound by versioned vectors, not shared implementation.

### Existing-owner delta S0: #732 schemas, validators, and append

**Owned write scope:** the exact shared schema, append validation, fixtures, and
schema-only Go mirrors assigned by #732; no queue, provider, budget policy,
bridge orchestration, outbox, or projection implementation.
**Dependencies:** #732 canonical envelope/append authority; #705 only if a new
dependency is proposed. S0 blocks C0/G1 production of V1 records.
**Deliverables:** versioned `AdmissionIntentV1`, `BudgetReservationV1`,
`BudgetReservationTransitionV1`, `BudgetExemptionV1`,
`InferenceAdmissionV1`, `AdmissionDispositionV1`,
`InferenceAuthorityPortV1`, `ProviderDispatchReceiptV1`,
`ProviderAttemptOutcomeV1`, `UsageOutcomeV1`, and `ProviderCapabilitiesV1`;
strict deterministic-CBOR Rust/Go codecs, canonical-byte/digest golden vectors,
invalid fixtures, unknown-version/field policy, and schema digest.

**Acceptance:**

1. Rust and Go produce byte-identical deterministic CBOR and SHA-256 for every
   record/transition/port golden vector and reject every invalid vector with a
   typed reason.
2. The phase order is acyclic: intent has no reservation, reservation or
   exemption binds intent, and final admission binds exactly one authority
   record.
3. Request, governance receipt, hierarchy/routing policy, catalog, pricing,
   provider, model, admission, stable attempt, reservation/exemption,
   transition, port, and usage identities are bound and validated.
4. Every V1 record requires `version`; every time is typed as Unix milliseconds.
5. Pre-dispatch disposition, dispatch, and terminal outcome are separate durable
   operations with canonical payload digests, stable idempotency identities, and
   legal-predecessor CAS; disposition and dispatch compete for the same final-
   admission predecessor.
6. Reservation creation is immutable; every status change is an append-only
   `BudgetReservationTransitionV1` with a closed legal edge.
7. Authority reasons are closed enums plus optional fixed 32-byte evidence/
   diagnostic digests; terminal state/reason uses the closed legal matrix.
8. `AMBIGUOUS` cannot be transformed to a retry-safe state.
9. Capability digest includes stream usage and definitive-rejection semantics.
10. CI path routing runs both language validators for schema/vector changes.

**Negative tests:** unknown/missing version, enum, or field; untyped/non-Unix-ms
time; missing digest; cyclic intent/reservation/admission vector; torn phase;
reservation or exemption rebound to another request/admission; both/neither
reservation and exemption; admission, stable attempt, transition payload,
governance, routing policy, pricing, catalog, or capability digest mismatch;
cross-record/type/version confusion; omitted versus null; reordered or duplicate
scopes; non-NFC Unicode; unknown/duplicate/case-variant field; integer overflow,
negative/bignum/tag/float/NaN value; self-digest inclusion; duplicate operation
with same payload and with different payload; stale predecessor; outcome before
dispatch; disposition/dispatch race; second/out-of-order terminal outcome;
exempt usage with a reservation or wrong exemption; billable usage without
reservation; mutable reservation event; illegal reservation edge; invalid
terminal state/reason; free-text authority reason; invented wrapper capability;
ambiguous marked non-billable.

**Runtime target block:** `NONE`; deploy, read-only, and benchmark targets none;
`.155`, `.240`, `.241`, `.242`, providers, and Proxmox are forbidden. Local
deterministic fixtures only. Rollback owner is the S0 implementer by PR revert.
**Benchmark:** structural vector count and schema-size ceiling only; no timing.
**Rollout/rollback:** readers accept old plus V1 before producers emit V1;
rollback stops V1 production without deleting records.
**Evidence:** exact vector hashes, validator outputs, CI paths, one-authority
matrix.
**TOGAF target delta:** the main-session owner immediately adds the approved
versioned admission/attempt/usage authority target to both language copies.

### Materialized child G1: #773 Go Gateway bounded admission and streaming

**Owned write scope:** `cmd/cortex-gateway` queue, provider wrappers, stream
adapter, Gateway metrics/config/tests only. No Rust, event store, projection, or
TOGAF file.
**Parent/dependencies:** native child #773 under #650; research #714; S0 schema port;
#764 pressure policy; #769 deterministic Go schedules. Implementation may run in
parallel with C0, but billable activation depends on C0's live authoritative
inference-authority port.
**Deliverables:** finite queue capacity and class limits; typed overload and
retry-after metadata; cancellation-safe grant; exact optional-interface wrapper
matrix; stream terminal parser/outcome; untrusted `AdmissionIntentV1` proposal
and authenticated, digest/idempotency/predecessor-bound
`InferenceAuthorityPortV1` client for all six methods; final admission and
provider-I/O authorization only from C0 commit ACKs.

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
9. Go supplies no authoritative scope list, maximum cost, route, token ceiling,
   or deadline. It propagates the #695 governance receipt and treats provider,
   model, catalog/capability/pricing generations, token limits, and deadline as
   proposals that C0 independently resolves, validates, and clamps.
10. The port authenticates the Gateway service identity and never treats local
    process access as authorization.
11. G1 calls reserve/exempt, finalize, and exactly one disposition-or-dispatch
    mutation in order. It performs zero provider I/O until a fresh
    `BEGIN_DISPATCH` commit ACK authorizes it; replay readback never authorizes
    I/O.
12. Provider response is followed by outcome commit and, for `COMPLETED`, usage
    reconciliation. Crash/timeout after dispatch commit is ambiguous,
    quarantined, and never auto-retried.

**Negative tests:** full ingress and full waiter cap; cancel-before-grant;
grant/cancel race; timeout while waiting; all optional-interface combinations;
status reporter with typed 429/503 and retry-after; duplicate terminal SSE;
missing usage; disconnect after headers; provider 429 with and without definitive
non-billable contract; queue-config overflow; C0 deny/timeout/unavailable;
unrecognized exemption; caller attempts to submit scopes or maximum cost;
missing/stale/foreign governance receipt; unauthorized local caller; port
idempotency key replay with a different intent digest; reservation/exemption
rebound during finalization; missing/unknown port method/version; stale
predecessor; provider call barrier before dispatch commit ACK; replay readback
attempts provider call; timeout/crash after dispatch commit; disallowed/expensive
route or overlong deadline proposal; readiness attempts provider generation. Use Go
`testing/synctest`, race detector, and deterministic barriers under #769; no
sleeps as proof.

**Runtime target block:** `SINGLE_NODE`; deploy and benchmark target `.240`;
read-only target `.240`; forbidden `.155`, `.241`, and `.242`; no real provider call.
Create an issue-specific `.240` snapshot before deployment. Use token-free
`local-loop` and fake HTTP providers for queue, stream, status, cancellation,
all authority-port methods, durable-before-I/O ordering, and restart probes.

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
**TOGAF target delta:** the main-session owner immediately adds the approved
bounded admission, deterministic route/deadline proposal, status/retry-after,
and stream-terminal targets to both language copies; C0 remains final admission
authority.

### Existing-owner delta C0: #695 durable budget and attempt reconciliation

**Owned write scope:** daemon bridge and the exact persistence/projection files
assigned by #695, using #732 append schemas and #733 delivery outcomes. No Go
queue/provider implementation and no parallel budget owner.
**Ownership/dependencies:** append this precise delta to active #695. If #695
finishes before the delta can land, create an explicit successor linked after
#695 rather than a parallel child. S0 precedes V1 production; #733 remains the
outbox/consumer authority. C0 and G1 implementation may run in parallel after
S0, but C0 blocks billable G1 activation.
**Deliverables:** authenticated, versioned, replay/predecessor-bound
`InferenceAuthorityPortV1` with all six methods; governance- and policy-derived
atomic budget reservation; immutable reservation plus append-only transitions;
stable attempt binding plus separate disposition/dispatch/terminal persistence;
one fenced SQLite/EventStore transaction gateway for every coupled mutation;
idempotent terminal usage reconciliation; failpoints, cancellation/restart
recovery, and typed bridge responses.

**Acceptance:**

1. Concurrent reservations cannot exceed an accepted budget scope.
2. C0 authenticates and authorizes caller/service identity. Idempotency binds
   service, method, key, record type/ID, payload digest, and expected
   predecessor; unknown method/version and replay rebinding reject.
3. C0 validates the versioned governance receipt and derives every mandatory
   tenant/project/work-item/agreement/customer/provider scope and generation.
4. C0 computes the conservative integer micro-USD maximum from validated
   provider/model, catalog/pricing digests, and effective policy token ceilings;
   no caller amount or scope list is authoritative.
5. C0 independently validates the exact hierarchy/policy route, allowed
   provider/model and catalog/capability/pricing generations, effective token
   ceilings, and policy-clamped deadline. Gateway values are proposals only.
6. Reservation is durable before final admission/provider dispatch and binds the
   intent without a cyclic final-admission dependency.
7. Every scope kind, generation, and time window is part of one atomic integer
   micro-USD comparison/reservation.
8. Reservation creation is immutable; status history consists only of legal
   append-only `BudgetReservationTransitionV1` records.
9. Pre-dispatch disposition, dispatch receipt, and terminal outcome use separate
   stable operation IDs, canonical payload digests, and legal-predecessor CAS;
   disposition and dispatch cannot both win.
10. Every coupled record, transition, authoritative aggregate update, event, and
    outbox insert commits in one fenced C0 SQLite/EventStore transaction. S0 and
    external projections are not transaction owners.
11. Only a fresh durable `BEGIN_DISPATCH` commit ACK authorizes provider I/O.
    Commit readback, timeout, or recovery never authorizes a second call.
12. Definitive completion reconciles exactly once to provider-reported or catalog
   cost; exempt local-loop/fake usage binds only its C0 exemption and costs zero.
13. Pre-dispatch cancellation releases exactly once; post-dispatch cancellation
   retains incurred input or conservative cost.
14. Ambiguous dispatch remains quarantined across restart and cannot retry,
    reconcile usage, or recover effects without authoritative evidence.
15. Completion recovery produces exactly one usage event and one action claim.
16. Outbox redelivery is absorbed by stable operation IDs and same-payload
    replay; different-payload reuse is rejected.
17. Port/store/governance authority unavailable fails closed for every billable
    route.

**Negative tests:** concurrent last-budget race; crash before/after reservation;
crash between intent/reservation/finalization; crash before/after dispatch;
timeout with unknown provider state; missing/additional/stale scope generation;
foreign tenant/project, mismatched work item/agreement/customer, old governance
receipt generation; caller-supplied wrong maximum; unauthorized local caller;
same idempotency key with different intent digest; duplicate same-payload
disposition/dispatch/outcome/usage; duplicate different-payload operation;
missing/unknown port method/version; disposition/dispatch race; provider call
before fresh dispatch commit ACK; dispatch commit ACK timeout/readback; stale
predecessor; out-of-order or second terminal outcome; invalid terminal
state/reason; mismatched request/intent/admission/
attempt digest; exempt usage with a reservation or wrong exemption; billable
usage without reservation; mutable reservation row/event; illegal reservation
edge; failpoints before/after append, aggregate update, commit, outbox publish,
and external projection apply; duplicate outbox; projection restart; expired
ambiguous reservation; disallowed/expensive route, stale routing generation, or
overlong deadline; float, negative, overflow, or malformed micro-USD; provider
cost outside catalog sanity bounds; free-text authority reason or oversized
evidence digest.

**Runtime target block:** `SINGLE_NODE`; deploy and benchmark target `.240`;
read-only target `.240`; forbidden `.155`, `.241`, and `.242`; no real provider call.
Create an issue-specific snapshot, deploy the complete affected daemon/Gateway/
store/projection set, and use local-loop/fake-provider journeys for concurrent
reserve, cancel, crash, restart, outbox replay, and projection readback.

**Benchmark contract:** on `.240`, report p50/p95/max authority-port and
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
**TOGAF target delta:** the main-session owner immediately adds the approved
durable integer-unit multi-scope reservation, final hierarchy/policy route and
deadline admission, complete mutation-port ordering, append-only attempt/
reservation transitions, and safe-failover targets to both language copies.

### Materialized existing-owner deltas

- **#695:** own C0 governance receipt validation, derived-scope/conservative-cost
  immutable `BudgetReservationV1`, append-only
  `BudgetReservationTransitionV1`, complete authenticated
  `InferenceAuthorityPortV1`, final route/deadline admission, fenced
  SQLite/EventStore transaction gateway, stable attempt binding, transition CAS,
  and canonical reconciliation as its existing provider/project cost-ceiling
  implementation; require schema-validated actions and add concurrent reserve,
  caller authorization, receipt/routing-generation, all-method port,
  durable-before-provider-I/O, transaction-failpoint, idempotency-replay,
  cancellation, restart, and ambiguous-dispatch negative ACs. If timing requires
  follow-up, create a successor after #695 rather than parallel authority.
- **#696:** consume exact S0/C0/#773 release evidence and preserve model/catalog/
  request lineage in delivery records.
- **#705:** decide retain/rewrite/remove for `sentinel-inference`; separately
  decide any vLLM/SGLang/llama.cpp adapter dependency with license, security,
  image, CVE, owner, update, migration, and rollback evidence.
- **#656:** only after #705 accepts a dependency, own update cadence and
  compatibility matrices.
- **#732:** own S0 canonical intent/reservation/exemption/final-admission,
  reservation-transition, pre-dispatch disposition, authority-port,
  dispatch/terminal/usage schema, deterministic-CBOR codec/golden vectors, and
  append/CAS validation, not provider or budget policy.
- **#733:** own durable delivery, retry outcome, and consumer idempotency for new
  events, not provider retry.
- **#758:** consume bounded queue/attempt/reservation counters and causal IDs;
  never become a second business-state store.
- **#764:** define pressure tiers and admission policy inputs consumed by #773.
- **#769:** add wrapper inventory/stream/status combinations, ingress/waiter
  capacity, grant/cancel, timeout, retry-after, and disconnect schedules. It
  remains test ownership, not production implementation.

Closed #395 remains unchanged; its historical body and status are not rewritten.

### Live materialization readback

| Issue | Materialized role | Body SHA-256 | Fresh Quality run |
|---|---|---|---|
| [#714](https://github.com/silentspike/project-sentinel/issues/714) | Approved research decisions, graph, and evidence | `e3f23fc8d6fc74f76b857b8a8cf8c4f55636877d171dfbd3188b4c647f7b86c2` | `30472084574` PASS |
| [#732](https://github.com/silentspike/project-sentinel/issues/732) | S0 schema/codec/vector/append addendum | `84a2ca63ee470368518738edf463c1c9b0bea2935ea4b4196c8e6b6f2a167cdc` | `30471794097` PASS |
| [#695](https://github.com/silentspike/project-sentinel/issues/695) | C0 final-admission/budget/attempt addendum | `67e161fa68207c3b6a9a90e351cd5e58cf91ace90554442c5a45cb6886445f79` | `30471808296` PASS |
| [#696](https://github.com/silentspike/project-sentinel/issues/696) | Narrow QA/release lineage consumer addendum | `766be5692c219a0bc672d65119a02dee67bf2ff94cd05555d7b74a01f6d9d100` | `30471805866` PASS |
| [#773](https://github.com/silentspike/project-sentinel/issues/773) | Sole new G1 child, natively under #650 | `aa1990dd381d02b4d7e6d7fa138feea14ba06e888f0c72353926c3a2d397c2cc` | `30471703481` PASS |

Every row has `quality:ready`. Reciprocal non-authority routes are live on
[#650](https://github.com/silentspike/project-sentinel/issues/650#issuecomment-5120799531),
[#705](https://github.com/silentspike/project-sentinel/issues/705#issuecomment-5120799820),
[#656](https://github.com/silentspike/project-sentinel/issues/656#issuecomment-5120800112),
[#733](https://github.com/silentspike/project-sentinel/issues/733#issuecomment-5120800430),
[#758](https://github.com/silentspike/project-sentinel/issues/758#issuecomment-5120800688),
[#764](https://github.com/silentspike/project-sentinel/issues/764#issuecomment-5120800966),
and [#769](https://github.com/silentspike/project-sentinel/issues/769#issuecomment-5120801253).

## Rollout, rollback, and benchmark decision gates

### M0 hardening rollout

1. Hand the approved target delta to the main-session owner for both TOGAF
   language copies; do not wait for implementation evidence.
2. Land S0 readers, deterministic-CBOR golden vectors, acyclic intent/reserve/
   finalize vectors, immutable reservation-transition and port-method/CAS invalid
   fixtures, append validation, and CI paths.
3. Build C0 and G1 in parallel behind independent producer flags. G1 may exercise
   only non-billable local-loop/fake-provider paths until C0 is live.
4. Compare old scope derivation, reservation totals, and usage totals with C0 V1
   governance-bound reconciliation using local deterministic fakes; any
   unexplained difference blocks enforcement.
5. Snapshot `.240`, deploy complete affected sets, and pass C0/G1 live,
   restart, rollback, p50/p95/max, and resource/cardinality contracts.
6. Enable C0 authoritative governance/route/deadline validation, scope/cost
   derivation, full mutation port, and fenced transaction gateway after existing
   pending completions reconcile.
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
| AC-5 explicit decision per mechanism | `PASS` | ORC approved all twelve executive decisions and the S0/C0/G1 authority model |
| AC-6 accepted gap has live quality owner | `PASS` | #732 S0, #695 C0, #696 consumer, and sole new #773 G1 contracts are live and quality-ready |
| AC-7 M0 classification and acknowledgement | `PASS` | Every finding is classified; ORC-approved routes and reciprocal live links preserve one authority |
| AC-8 public-safe study | `PASS` | One English ASCII document, no secrets, provider calls, copied code, or runtime data; final gates are recorded below |
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
python3 <private-verifier> --all --doc \
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
- a live materialization claim without exact issue, hash, quality-run, and
  reciprocal-link readback;
- a new coordination epic or parallel durable-budget owner beside #650/#695;
- a cyclic S0/C0/G1 owner graph or billable G1 activation before C0;
- an intent/reservation/final-admission digest cycle, rebound authority record,
  or torn phase;
- a Gateway-supplied authoritative scope set or maximum cost;
- a Gateway-owned final provider/model/deadline decision rather than a C0-
  validated proposal;
- missing/stale/foreign governance scope generation, unauthorized caller, or
  idempotency-key replay with a different intent digest;
- a missing authority-port method, unknown version/method acceptance, or a port
  request not bound to caller, method, key, record/payload digest, and
  predecessor;
- provider I/O before a fresh durable `BEGIN_DISPATCH` commit ACK, or provider
  I/O authorized by replay readback;
- mutable reservation status inside `BudgetReservationV1`, event mutation, or
  an illegal/rebound `BudgetReservationTransitionV1`;
- a coupled disposition/release, outcome/quarantine, or usage/reconcile claim
  outside one C0 SQLite/EventStore transaction;
- an attempt digest that includes mutable outcome state, or a dispatch/outcome/
  usage transition without stable operation ID, payload digest, and predecessor
  CAS;
- duplicate same/different-payload, stale predecessor, out-of-order terminal,
  exempt-usage-with-reservation, or billable-usage-without-reservation vectors;
- a V1 record without `version` or a time field not typed as Unix milliseconds;
- a digest codec without record/version domain separation, deterministic CBOR,
  absent-only optionals, NFC rule, sorted scopes, strict `uint64`, unknown-field
  rejection, or a no-float rule;
- free-text authority reasons, oversized diagnostics, or an invalid terminal
  state/reason pair;
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
- The approved TOGAF target delta is a main-session handoff; this worker did not
  edit either language copy.
