# OSS concurrency model checking and fault-injection study

- Status: REVIEW_READY candidate
- Issue: [#719](https://github.com/silentspike/project-sentinel/issues/719)
- Parent: [#659](https://github.com/silentspike/project-sentinel/issues/659)
- Sentinel baseline: `cbd7c25d2bb57df99462d4a180aae5ab00eaf651`
- Research cut: 2026-07-29
- Runtime evidence: none; this is a source and test audit, not a deployment or
  performance benchmark

## Executive decision

Sentinel should use a layered test architecture rather than select one model
checker or chaos product:

1. **Configure the existing Loom dependency** for small Rust synchronization
   primitives whose correctness depends on weak-memory ordering. The current
   OwnerRegistry test is the right mechanism but is optional and isolated.
2. **Adopt Shuttle as a test-only dependency, subject to #705**, for scalable,
   reproducible task, channel, cancellation, retry, and timeout interleavings
   that are too large for exhaustive Loom exploration.
3. **Adopt Stateright as a test-only dependency, subject to #705**, for explicit
   protocol state machines, safety/reachability properties, shortest
   counterexample paths, and bounded linearizability checks.
4. **Adopt Turmoil as a test-only dependency, subject to #705**, for deterministic
   Tokio virtual time, network delay/drop/partition, host crash/bounce, source
   barriers, and its test-only filesystem durability model. Its filesystem and
   barrier features are explicitly unstable, so #729 must pin a reviewed version
   and keep an adapter boundary.
5. **Port the Jepsen contracts, not its Clojure runtime**, for black-box operation
   histories, invoke/ok/fail/info outcomes, nemesis repair phases, and
   post-recovery reads. #556 remains the owner of real N-node chaos. Stateright
   supplies bounded in-repository linearizability checks; a production-cluster
   history is never called correct merely because a simulator passed.
6. **Keep Kani for sequential data invariants and reject it for concurrency
   exploration.** Upstream explicitly says concurrent verification is not
   supported. The delivered #393 proofs remain useful but do not overlap Loom,
   Shuttle, or Stateright.
7. **Reject an Apalache/TLA+ dependency for the first implementation slice.**
   A small, hand-maintained specification may be reconsidered after the Rust
   Stateright models expose stable state/action vocabularies. Maintaining two
   independently encoded authorities now would create specification drift.
8. **Reimplement only the thin Sentinel-owned control plane:** a versioned
   `FailureScheduleV1`, `OperationHistoryV1`, outcome taxonomy, bounded CI
   profiles, deterministic seed/schedule retention, and adapters to the tools
   above. These records describe tests; they never become runtime authority.

No current source-backed defect is newly classified `BLOCKS_M0` by this study.
The missing systematic coverage around customer-work concurrency, outbox
publication, cancellation, projection frontiers, and restart boundaries is
`M0_HARDENING`. Broad cluster exploration, production-trace-derived schedules,
and additional formal specifications are `POST_M0`, except where an existing
owner already requires them for a declared gate.

The decision package is intentionally not materialized in GitHub yet. AC-5,
AC-6, and the owner-acknowledgement part of AC-7 remain pending ORC approval, as
required by the issue contract.

## Method and decision rules

### Evidence standard

- Sentinel claims come from the pinned baseline's source and tests. Closed issue
  labels are history, not proof that all current interleavings are correct.
- Upstream claims are tied to immutable commits and load-bearing source or test
  paths. README text is used only when the implementation and tests support it.
- No upstream source was copied, vendored, built, or executed.
- No Cargo, Rust, runtime host, deployment, or performance benchmark was used.
- Upstream benchmark methods inform hypotheses only. They are not Sentinel
  measurements.
- A passing bounded or randomized search means only that no failure was found
  inside its declared state, step, seed, and fault bounds.
- Every adopted test dependency is development-only, default-off outside its
  dedicated gates, reviewed through [#705](https://github.com/silentspike/project-sentinel/issues/705),
  and upgrade-governed by [#656](https://github.com/silentspike/project-sentinel/issues/656).

### Screening rubric

Each candidate was scored 0 to 3 on ten factors, for a maximum of 30:

| Factor | 0 | 1 | 2 | 3 |
|---|---|---|---|---|
| Mechanism fit | No listed mechanism | Peripheral | One strong mechanism | Several strong mechanisms |
| Correctness model | Unstated | Heuristic only | Explicit bounded/probabilistic model | Explicit exhaustive or checked properties |
| Determinism | Not reproducible | Best effort | Seeded | Exact schedule/path replay |
| Failure semantics | None | Generic errors | Named faults/outcomes | Named faults plus recovery checks |
| Sentinel boundary | Incompatible | Heavy adapter | Test-only adapter | Direct Rust/test fit |
| State-space control | None | Timeout only | Steps/seeds | Steps, bounds, reduction, replay |
| Maturity | Experimental snapshot | Sparse tests | Maintained tests/releases | Mature tests and operating use |
| Maintenance | Dormant/archived | Irregular | Active | Active with releases and review |
| License/security | Incompatible/unknown | Review burden | Compatible, no policy | Compatible plus explicit policy |
| Dependency/operations | Production service | Heavy toolchain | Test runtime | Small test-only library |

Scores select deep reviews; they do not select dependencies automatically.

## Sentinel baseline

### Current concurrency and failure mechanisms

All source links in this section refer to the exact Sentinel baseline above.

| Surface | Current source and tests | What exists | Unproved boundary |
|---|---|---|---|
| Owner publication and fencing | [`OwnerRegistry`](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/crates/sentinel-common/src/fencing.rs#L575-L607) publishes terms under an `RwLock` and then flips the mode with Release ordering ([source](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/crates/sentinel-common/src/fencing.rs#L755-L799)). Guard issue and validation recheck readiness, term, owner, epoch, and generation ([source](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/crates/sentinel-common/src/fencing.rs#L964-L1023)). | One focused Loom test exhaustively checks the publish ordering ([test](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/crates/sentinel-common/tests/loom_owner_ordering.rs#L1-L57)). | The optional `loom-test` feature is not a complete owner-state or saga model. Snapshot rebuild, readiness close/open, saga overlays, tick barrier, and concurrent guard validation are not jointly explored. |
| Cluster RPC idempotency | The cache scopes by authenticated peer, method, operator key, and request digest, with TTL/capacity ([source](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/crates/sentinel-cluster-control/src/idempotency.rs#L1-L79)). | Threaded tests cover single computation, digest conflict, TTL, and capacity. | Process-local reply dedup is not durable exactly-once execution. Crash after effect and before reply/cache publication still belongs to each durable operation owner. |
| Event plus outbox | `append_with_outbox` inserts event and publication intent in one fenced SQLite transaction with operation-ID uniqueness ([source](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/crates/sentinel-limbo/src/event_store.rs#L997-L1052)); its atomic and idempotent cases are tested ([tests](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/crates/sentinel-limbo/src/event_store.rs#L2569-L2647)). | Store-local transaction atomicity and retry identity exist. | The publisher sends before `mark_published` ([source](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/crates/sentinel-limbo/src/outbox_publisher.rs#L140-L184)), so crash/timeout at that cut is intentionally at-least-once and requires consumer dedup/outcome evidence. No scheduler systematically explores shutdown, retry, and mark failures together. |
| Projection frontier | The worker reads from its event offset, applies each event to the projection, and then advances the EventStore offset ([source](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/crates/sentinel-projection/src/worker.rs#L82-L151)). | Replay and monotonic-offset checks exist. | Projection mutation and external offset advancement are separate store actions. Crash cuts can repeat projection application; every handler must be idempotent and generation-aware. |
| Restore saga | Restore has named failure points after redb, FS, ECS, and projection phases ([source](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/services/sentinel-daemon/src/orchestrator.rs#L3554-L3574)); tests run all four and verify rollback and fail-closed fencing ([tests](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/services/sentinel-daemon/src/orchestrator.rs#L8529-L8711)). | Deterministic phase injection for one in-process restore path. | It is an enumerated unit harness, not a reusable crash-schedule engine. Process death, partial filesystem durability, queue concurrency, and restart between every visibility step are outside it. |
| Runtime reconciliation | Runtime control uses synchronous command/reply channels and explicit reconcile, panic, stall, pause, resume, and despawn requests ([source](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/services/sentinel-daemon/src/runtime_control.rs#L54-L94)). Respawn has deterministic exponential backoff and a blocked terminal state ([source](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/services/sentinel-daemon/src/runtime_control.rs#L178-L238)). | Targeted retry, panic, stall, and reconciliation tests. | Sender drop, late reply, concurrent operator/periodic reconcile, cancellation, and daemon death are not generated from one replayable schedule. |
| CAS single-flight | `BlockResolver` serializes same-key pulls, caches failures, and protects recent pulls from GC ([source](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/crates/sentinel-fs/src/block_resolver.rs#L75-L180)). An eight-thread barrier test proves one pull for one fixture ([test](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/crates/sentinel-fs/src/block_resolver.rs#L295-L318)). | Concrete parallel smoke coverage. | Ordinary threads do not enumerate gate creation/removal, negative-cache expiry, GC observation, panic, and failed pull orderings. |
| Gateway queues and sequencing | The forward queue protects active count and FIFO waiters with a mutex and handles cancellation after a grant ([source](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/cmd/cortex-gateway/internal/forwardqueue/manager.go#L37-L97)). Room sequencing closes a channel under a mutex and waits without holding it ([source](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/cmd/cortex-gateway/internal/sequencing/queue.go#L31-L195)). | Go unit tests cover selected concurrent and cancellation cases. | Go's production scheduler and wall-clock timers do not yield a portable schedule artifact. Cancellation/grant, complete/timeout, hot reload, and shutdown combinations remain probabilistic. |
| ECS determinism and replay | The repository has deterministic state hashing, bounded replay, snapshot restore, and a pinned determinism profile. | Same-input replay and restore tests detect state divergence. | Replay controls domain inputs, not all OS thread, task, channel, network, and storage interleavings. Bit-identical ECS replay does not prove outbox or external-effect linearizability. |
| Kani proofs | Closed [#393](https://github.com/silentspike/project-sentinel/issues/393) delivered bounded proofs for sequential data invariants. | Useful proof history for pure functions and codecs. | Kani upstream explicitly excludes concurrent verification; these proofs cannot establish race freedom or scheduler correctness. |

### Spawn, lock, channel, store, and crash impact map

| Primitive or boundary | Main owners | Local failure | Butterfly effect |
|---|---|---|---|
| `AtomicBool`/`AtomicU8` plus `RwLock` owner view | `sentinel-common`, daemon cluster control | Mode/readiness visible before the corresponding maps or durable marker | A stale guard can pass, a valid owner can be rejected, or two nodes can act on inconsistent route/owner state. |
| Daemon `mpsc` command/reply channels | operator API, orchestrator, runtime control, replay, LLM bridge | Sender drop, receiver shutdown, reply timeout, late result, full bounded queue | Operator retries can duplicate an action; a completed runtime effect can be reported as failed or abandoned. |
| Tokio tasks and `select!` | daemon services, outbox publisher, QUIC control, dashboard | Cancellation at any await, task panic, biased readiness, shutdown race | Publication, control reply, health state, or cleanup may lag durable state and trigger a duplicate or premature recovery. |
| Go goroutines, channels, mutexes, timers | Gateway, NATS bridge, Judge | Grant/cancel race, close/send race, timeout versus completion, goroutine exit | An accepted request can be reordered, lose context, exceed concurrency policy, or be retried without the intended receipt. |
| SQLite transaction plus process-local mutex | Limbo, projection, Gateway observatory, Judge, Nightrun | Crash before/after commit; lock poisoning is Rust-local while process death loses volatile decisions | Event, outbox, projection, job, and consumer frontiers can describe different generations. |
| redb writer transactions and filesystem publication | state, FS metadata, ArtifactPlane, cluster metadata, memory stores | Sync/rename/directory durability failure, process death, mixed generation | A row or CAS reference can become visible without durable bytes, or recovery can select a stale owner/store generation. |
| NATS/Zenoh/QUIC boundaries | bridge, Judge, fanout, cluster control, block pull | ACK/reply loss, redelivery, partition, delayed message, peer restart | Consumers repeat effects, membership changes, or block/snapshot operations continue after authority changed. |
| ECS tick and snapshot barriers | daemon orchestrator, ECS, restore | Pause after input acceptance, before event append, during snapshot/restore, or before projection | Domain state, event cursor, projections, runtime residency, and external effects can no longer describe one point. |
| External provider/tool/release/delivery effect | Gateway, future Workbench/workflow/QA owners | Effect succeeds but receipt or workflow completion does not persist | Automatic retry can duplicate a billable or irreversible effect; guessing success can lose customer work. |

The map is deliberately causal. A scheduler test is useful only when its final
oracle checks the authoritative cross-boundary outcome, not merely that all tasks
joined.

### Existing owner map and non-overlap

Live issue state was read on 2026-07-29.

| Owner | Live state | Existing boundary | #719 delta after approval |
|---|---|---|---|
| [#393](https://github.com/silentspike/project-sentinel/issues/393) | Closed, verified history | Kani proofs for sequential invariants | Keep unchanged. Explicitly reject using those proofs as concurrency evidence. |
| [#498](https://github.com/silentspike/project-sentinel/issues/498) | Closed, verified history | Distributed CAS, single-flight, durable publication, QUIC pull | Historical evidence only. Any regression schedule routes to active storage/cluster owners, not a reopened issue. |
| [#501](https://github.com/silentspike/project-sentinel/issues/501) | Open | Bounded stop-and-copy saga, owner/route transitions, failure injection | Add the executable migration schedules in this study and require reproducible model/simulator counterexamples. |
| [#556](https://github.com/silentspike/project-sentinel/issues/556) | Open, ready | Cluster-GA safety invariants, chaos, N-node matrix | Own real-cluster Jepsen-style histories and Stateright/Turmoil protocol models; no Clojure production dependency. |
| [#653](https://github.com/silentspike/project-sentinel/issues/653) | Open, backlog | ReplicaGroup ordering, promotion, side-effect authority ADR | Consume model counterexamples and define the canonical state/action vocabulary before broad cluster implementation. |
| [#693](https://github.com/silentspike/project-sentinel/issues/693) | Closed, verified history | M0 work-execution contract and conformance matrix | Keep unchanged; the proposed new harness must map its tests to this contract without becoming workflow authority. |
| [#710](https://github.com/silentspike/project-sentinel/issues/710) | Open, in progress | Durable execution and cross-store crash semantics | Own business/effect failure cuts and expected terminal outcomes. #719 supplies schedule/history mechanics only. |
| [#729](https://github.com/silentspike/project-sentinel/issues/729) | Open, blocked | redb policy, integrity, compaction, deterministic storage fault harness | Evaluate Turmoil `unstable-fs` behind an adapter instead of duplicating a general virtual filesystem; retain engine-specific semantic oracles. |
| [#705](https://github.com/silentspike/project-sentinel/issues/705) | Open dependency authority | New dependency decisions | Must approve Shuttle, Stateright, and Turmoil before manifests change. |
| [#656](https://github.com/silentspike/project-sentinel/issues/656) | Open upgrade authority | Upgrade and compatibility policy | Own pin/review/replay requirements for every accepted test dependency. |

### Target-architecture constraints and delta

The target architecture already requires:

- append-only events as the source of truth and deterministic operation identity
  ([TOGAF lines 2088-2117](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/docs/architecture/togaf-architecture-guide.html#L2088-L2117));
- bounded replay from an anchor to a target and explicit snapshot coverage
  ([lines 2518-2520](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/docs/architecture/togaf-architecture-guide.html#L2518-L2520));
- one primary tick and side-effect authority, with replicated information rather
  than transient caches
  ([lines 2658-2665](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/docs/architecture/togaf-architecture-guide.html#L2658-L2665));
- fail-closed ownership and fencing instead of membership inference
  ([lines 2731-2734](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/docs/architecture/togaf-architecture-guide.html#L2731-L2734));
- a versioned homogeneous determinism profile
  ([lines 2738-2740](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/docs/architecture/togaf-architecture-guide.html#L2738-L2740)).

The target delta proposed after ORC approval is narrow:

1. Define the layered verification taxonomy: memory-order model, task schedule,
   protocol model, simulated fault schedule, and real-cluster history are
   complementary evidence and never interchangeable.
2. Require every critical failure test to emit a versioned, bounded,
   replayable schedule/history artifact and an authoritative final-state oracle.
3. State that passing a bounded model is not production proof and that real
   cluster claims still require the owning issue's declared runtime.
4. Preserve one authority and 1:n: models and histories reference event, owner,
   generation, operation, artifact, and receipt IDs; they never copy or replace
   those stores.

No TOGAF file is changed by this worker.

## OSS landscape

### Reproducible inventory

| Candidate | Pin and release | License | Security policy | Score | Deep review | Result |
|---|---|---|---|---:|---|---|
| [Loom](https://github.com/tokio-rs/loom/tree/948c8cc78b178ede6eeff3afc7d97f2f4ea08559) | `948c8cc78b178ede6eeff3afc7d97f2f4ea08559`; crate 0.7.2 | MIT | No repository policy found | 24 | Yes | Configure existing dependency for small weak-memory and lock primitives. |
| [Shuttle](https://github.com/awslabs/shuttle/tree/cd57cf9d04c3056eb82a6fd7bd272d264b5c290c) | `cd57cf9d04c3056eb82a6fd7bd272d264b5c290c`; post-0.9.0 main | Apache-2.0 | No repository policy found; AWS Labs contribution process | 25 | Yes | Adopt test-only for scalable deterministic task/channel schedules. |
| [Stateright](https://github.com/stateright/stateright/tree/ab8c8be9341505e0f71edbe5dd88ed275bd976a4) | `ab8c8be9341505e0f71edbe5dd88ed275bd976a4`; 0.31.0 | MIT | No repository policy found | 25 | Yes | Adopt test-only for protocol models and bounded linearizability. |
| [Jepsen](https://github.com/jepsen-io/jepsen/tree/58b4c48629fb31a333d7101ad7554c6d59c9ad61) | `58b4c48629fb31a333d7101ad7554c6d59c9ad61`; v0.3.12 line | EPL-1.0 for Jepsen module | No repository policy found | 20 | Yes | Port contracts; reject Clojure runtime as a Sentinel dependency. |
| [Turmoil](https://github.com/tokio-rs/turmoil/tree/684acc1a8eea3a9cf2c6959dc47b69dba981cac1) | `684acc1a8eea3a9cf2c6959dc47b69dba981cac1`; 0.7.2 line | MIT | No repository policy found | 25 | Yes | Adopt test-only behind adapters for virtual network/time/crash/storage faults. |
| [Kani](https://github.com/model-checking/kani/tree/2f56cf3503ef6e495394a820013c89610f8f550a) | `2f56cf3503ef6e495394a820013c89610f8f550a`; post-0.67.0 main | Apache-2.0 OR MIT | [Policy](https://github.com/model-checking/kani/blob/2f56cf3503ef6e495394a820013c89610f8f550a/.github/SECURITY.md) | 18 | Rejection check | Keep for sequential proofs; reject for concurrency. |
| [Apalache](https://github.com/apalache-mc/apalache/tree/ac61ee884068a927c9861ee4cb4a4516fa2f2ac6) | `ac61ee884068a927c9861ee4cb4a4516fa2f2ac6`; post-v0.58.3 main | Apache-2.0 | No repository policy found | 18 | Rejection check | Reject first-slice dependency; revisit after stable protocol vocabulary. |
| FoundationDB simulation | Landscape reference only | Apache-2.0 | Project policy varies by repository | 13 | No | Reject: C++-coupled whole-system simulation is not a reusable Sentinel component. |

The inventory intentionally includes eight candidates across exhaustive
weak-memory exploration, randomized schedulers, explicit state models, black-box
histories, network/filesystem simulation, bounded verification, symbolic formal
specification, and whole-system simulation.

### Shortlist rationale

- Loom is already present and is uniquely strong for Rust weak-memory ordering.
- Shuttle complements Loom by trading exhaustive soundness for larger
  reproducible schedules, including scheduler and nondeterministic data choices.
- Stateright directly models Sentinel's owner, migration, replica, and outbox
  state machines and includes a bounded linearizability tester.
- Turmoil directly fits Tokio network, time, host restart, and storage failure
  boundaries without requiring real hosts.
- Jepsen supplies the strongest reviewed black-box history/nemesis discipline,
  even though its language and operational footprint make direct adoption a poor
  fit.

Kani and Apalache were still read at pinned source level to prevent false
substitution. Neither replaces the selected runtime-concurrency mechanisms.

## Pinned deep reviews

### 1. Loom

**Mechanism.** `model::Builder` bounds threads, branches, permutations, duration,
preemptions, and optional checkpoints
([source](https://github.com/tokio-rs/loom/blob/948c8cc78b178ede6eeff3afc7d97f2f4ea08559/src/model.rs#L11-L72)).
`check` repeatedly runs one closure under the scheduler, checks leaks, and steps
to the next execution until exhausted or bounded
([source](https://github.com/tokio-rs/loom/blob/948c8cc78b178ede6eeff3afc7d97f2f4ea08559/src/model.rs#L136-L218)).
The runtime models synchronization and atomic orderings rather than merely
randomizing OS threads.

**Tests and failures.** The two-lock test expects Loom to find a deadlock
([test](https://github.com/tokio-rs/loom/blob/948c8cc78b178ede6eeff3afc7d97f2f4ea08559/tests/deadlock.rs#L1-L35)).
Upstream's memory-order suite also documents limits: a Relaxed example is
ignored because Loom cannot fully model it, and a SeqCst example is ignored for
an upstream-known illegal permutation
([test](https://github.com/tokio-rs/loom/blob/948c8cc78b178ede6eeff3afc7d97f2f4ea08559/tests/spec.rs#L1-L12),
[test](https://github.com/tokio-rs/loom/blob/948c8cc78b178ede6eeff3afc7d97f2f4ea08559/tests/spec.rs#L103-L118)).
Those are material claim limits, not reasons to discard the tool.

**Security and operations.** Loom is a test library, not a production service.
It needs cfg-separated synchronization imports and can explode state space.
Bounds and checkpoints must be committed test policy, not developer-local
environment guesses. The MIT license is compatible; no repository security
policy was found at the pin.

**Sentinel fit.** Strong for OwnerRegistry, idempotency cache, single-flight,
atomic counters, and short lock/channel primitives. Poor for SQLite, redb,
network, process crash, Go goroutines, and end-to-end workflow effects.

### 2. Shuttle

**Mechanism.** Upstream explicitly chooses randomized scalability over
exhaustive soundness and records failing schedules for deterministic replay
([source](https://github.com/awslabs/shuttle/blob/cd57cf9d04c3056eb82a6fd7bd272d264b5c290c/shuttle/src/lib.rs#L3-L9),
[source](https://github.com/awslabs/shuttle/blob/cd57cf9d04c3056eb82a6fd7bd272d264b5c290c/shuttle/src/lib.rs#L36-L64)).
It provides random, PCT, DFS, replay, and other schedulers. PCT accepts an exact
seed, bug depth, and iteration bound
([source](https://github.com/awslabs/shuttle/blob/cd57cf9d04c3056eb82a6fd7bd272d264b5c290c/shuttle-schedulers/src/pct.rs#L12-L64)).

**Replay and failure behavior.** `ReplayScheduler` consumes an encoded or file
schedule and rejects impossible or truncated schedules unless explicitly
allowed
([source](https://github.com/awslabs/shuttle/blob/cd57cf9d04c3056eb82a6fd7bd272d264b5c290c/shuttle-schedulers/src/replay.rs#L11-L59),
[source](https://github.com/awslabs/shuttle/blob/cd57cf9d04c3056eb82a6fd7bd272d264b5c290c/shuttle-schedulers/src/replay.rs#L69-L140)).
It can also restrict replay to the vector-clock ancestry of a failure, useful
for minimization without pretending unrelated events caused it.

**Tests and limits.** Shuttle replaces synchronization, thread, future, and
randomness surfaces with controlled equivalents. Passing random/PCT iterations
is probabilistic, not proof. Code that reaches uncontrolled OS I/O, SQLite,
redb, Tokio network, or process APIs escapes its scheduler and must be separated
behind test ports.

**Security and operations.** Apache-2.0 is compatible. The test process receives
the same privileges as the test, so fixtures must use isolated temp roots and
fake effect ports. No production credentials or external network are allowed.
No repository security policy was found at the pin.

**Sentinel fit.** Strong for daemon task/channel cancellation, retries,
shutdown, local state machines, and larger concurrent structures. It
complements, not replaces, Loom: Shuttle does not make a passing run exhaustive
or a substitute for weak-memory modeling.

### 3. Stateright

**Mechanism.** `Model` separates initial states, deterministic action
enumeration, state transitions, boundaries, and properties
([source](https://github.com/stateright/stateright/blob/ab8c8be9341505e0f71edbe5dd88ed275bd976a4/src/lib.rs#L152-L259)).
`always`, `eventually`, and `sometimes` express safety, path-liveness, and
reachability, while paths provide counterexamples
([source](https://github.com/stateright/stateright/blob/ab8c8be9341505e0f71edbe5dd88ed275bd976a4/src/lib.rs#L262-L330)).
The checker supports BFS/DFS/simulation, depth/state/timeout bounds, and
symmetry reduction
([source](https://github.com/stateright/stateright/blob/ab8c8be9341505e0f71edbe5dd88ed275bd976a4/src/checker.rs#L55-L87),
[source](https://github.com/stateright/stateright/blob/ab8c8be9341505e0f71edbe5dd88ed275bd976a4/src/checker.rs#L153-L288)).

**History checking.** `LinearizabilityTester` records invoke/return histories,
enforces cross-thread happens-before constraints, and searches for a valid
serialization against a sequential specification
([source](https://github.com/stateright/stateright/blob/ab8c8be9341505e0f71edbe5dd88ed275bd976a4/src/semantics/linearizability.rs#L14-L62),
[source](https://github.com/stateright/stateright/blob/ab8c8be9341505e0f71edbe5dd88ed275bd976a4/src/semantics/linearizability.rs#L159-L280)).
The upstream linearizable-register example combines unordered delivery with a linearizability
property and checks both BFS and DFS
([test](https://github.com/stateright/stateright/blob/ab8c8be9341505e0f71edbe5dd88ed275bd976a4/examples/linearizable-register.rs#L220-L299)).

**Failure and liveness limits.** A model is only as faithful as its abstraction.
`eventually` has documented restrictions on cyclic state graphs
([source](https://github.com/stateright/stateright/blob/ab8c8be9341505e0f71edbe5dd88ed275bd976a4/src/lib.rs#L286-L304)).
State hashing, symmetry, and boundaries can accidentally merge meaningful
owner generation, receipt, or failure states; Sentinel models must include
negative mutation tests proving each load-bearing field matters.

**Security and operations.** MIT is compatible. The optional explorer is a web
surface and is not part of CI or production. CI uses non-serving checker APIs
only. No repository security policy was found at the pin.

**Sentinel fit.** Strongest fit for OwnerTerm/readiness, MigrationOp,
ReplicaGroup, projection frontier, outbox consumer outcomes, and bounded
linearizability. It does not execute production Tokio/redb/SQLite code unless a
separate adapter or trace connects the model.

### 4. Turmoil

**Mechanism.** Turmoil runs multiple async hosts deterministically in one thread,
with controlled virtual time and network behavior
([source](https://github.com/tokio-rs/turmoil/blob/684acc1a8eea3a9cf2c6959dc47b69dba981cac1/crates/turmoil/src/lib.rs#L1-L14)).
It exposes hold/release, bidirectional and one-way partition/repair
([source](https://github.com/tokio-rs/turmoil/blob/684acc1a8eea3a9cf2c6959dc47b69dba981cac1/crates/turmoil/src/lib.rs#L319-L361)),
plus host crash/bounce and bounded `run`/`step` control
([source](https://github.com/tokio-rs/turmoil/blob/684acc1a8eea3a9cf2c6959dc47b69dba981cac1/crates/turmoil/src/sim.rs#L153-L185),
[source](https://github.com/tokio-rs/turmoil/blob/684acc1a8eea3a9cf2c6959dc47b69dba981cac1/crates/turmoil/src/sim.rs#L389-L445)).

**Storage and barriers.** The current pin has unstable filesystem shims whose
pending writes survive only according to explicit sync semantics; `crash`
discards unsynced state
([source](https://github.com/tokio-rs/turmoil/blob/684acc1a8eea3a9cf2c6959dc47b69dba981cac1/crates/turmoil/src/lib.rs#L60-L99)).
Tests distinguish synced data, unsynced data, and directory-entry durability
([tests](https://github.com/tokio-rs/turmoil/blob/684acc1a8eea3a9cf2c6959dc47b69dba981cac1/crates/turmoil/tests/fs/durability.rs#L12-L113),
[tests](https://github.com/tokio-rs/turmoil/blob/684acc1a8eea3a9cf2c6959dc47b69dba981cac1/crates/turmoil/tests/fs/durability.rs#L174-L284)).
Unstable barriers suspend code at typed events for deterministic cut placement.

**Failures and limits.** The network is simulated, not QUIC/NATS/Zenoh itself.
Application code must accept transport, time, filesystem, and barrier ports.
The new filesystem/barrier APIs are unstable and may change between patch
reviews. A simulated sync outcome cannot replace engine-specific recovery tests
or real-cluster evidence.

**Security and operations.** MIT is compatible. Tests run in-process with fake
hosts and must not bind real sockets or access production paths. No repository
security policy was found at the pin.

**Sentinel fit.** Strong for cluster-control protocol adapters, publisher/consumer
network cuts, runtime heartbeat and restart, and #729 storage fault schedules.
It should not force production transport or filesystem types into domain APIs.

### 5. Jepsen

**Mechanism.** Jepsen separates operation generation, clients, nemeses, and
checkers. A checker returns structured validity and can be composed
([source](https://github.com/jepsen-io/jepsen/blob/58b4c48629fb31a333d7101ad7554c6d59c9ad61/jepsen/src/jepsen/checker.clj#L59-L129)).
Its linearizability checker delegates histories to Knossos and preserves
unknown/incomplete operation semantics
([source](https://github.com/jepsen-io/jepsen/blob/58b4c48629fb31a333d7101ad7554c6d59c9ad61/jepsen/src/jepsen/checker.clj#L273-L322)).

**Fault and recovery discipline.** Nemeses implement setup, invoke, teardown, and
finalization and include partitions and clock disruption
([source](https://github.com/jepsen-io/jepsen/blob/58b4c48629fb31a333d7101ad7554c6d59c9ad61/jepsen/src/jepsen/nemesis.clj#L41-L89),
[source](https://github.com/jepsen-io/jepsen/blob/58b4c48629fb31a333d7101ad7554c6d59c9ad61/jepsen/src/jepsen/nemesis.clj#L162-L209)).
Generators can mix clients with break/repair phases, wait for recovery, and
finish with per-thread reads
([source](https://github.com/jepsen-io/jepsen/blob/58b4c48629fb31a333d7101ad7554c6d59c9ad61/generator/src/jepsen/generator.clj#L209-L243)).
Checker tests distinguish acknowledged, recovered, lost, duplicated, and
unexpected queue elements
([test](https://github.com/jepsen-io/jepsen/blob/58b4c48629fb31a333d7101ad7554c6d59c9ad61/jepsen/test/jepsen/checker_test.clj#L154-L209)).

**Failures and limits.** Histories are only as correct as the client adapter's
invoke/completion boundaries. An `info` or unknown outcome must not be silently
converted to fail/success. Cluster setup, nemesis privileges, teardown, and
recovery reads are operationally heavy. Shrinking and checking can be expensive,
and a finite history cannot prove all future executions.

**Security and operations.** The Jepsen module is EPL-1.0. Direct integration
would add a JVM/Clojure/SSH-style toolchain and high-privilege fault operator.
No repository security policy was found at the pin. Sentinel should retain its
own least-privilege cluster-lab tooling and port only the history/outcome/repair
contracts.

**Sentinel fit.** Strong external validation method for #556 and future cluster
GA, weak as an in-workspace dependency. It is the independent black-box layer
above Stateright/Turmoil, not a replacement for either.

## Rejection checks

### Kani

Kani's current documentation says Rust concurrency is unsupported
([source](https://github.com/model-checking/kani/blob/2f56cf3503ef6e495394a820013c89610f8f550a/docs/src/getting-started.md#L17-L24))
and warns that atomic-intrinsic verification should not be trusted as
concurrent evidence
([source](https://github.com/model-checking/kani/blob/2f56cf3503ef6e495394a820013c89610f8f550a/docs/src/rust-feature-support/intrinsics.md#L245-L252)).
Decision: keep delivered pure-function/codec proofs; reject Kani for the
mechanisms in this study.

### Apalache and TLA+

Apalache symbolically checks bounded TLA+ executions rather than executing Rust
([source](https://github.com/apalache-mc/apalache/blob/ac61ee884068a927c9861ee4cb4a4516fa2f2ac6/docs/src/apalache/theory.md#L1-L25)).
Its own overview states that only finite bounded executions are analyzed and
that it does not support the full TLC language
([source](https://github.com/apalache-mc/apalache/blob/ac61ee884068a927c9861ee4cb4a4516fa2f2ac6/docs/src/apalache/index.md#L1-L33)).
Decision: reject a first-slice dependency and duplicate specification. Revisit
only when #653 or #556 has a stable action/state vocabulary and an owner accepts
the maintenance cost of a second formal model.

### FoundationDB simulation

FoundationDB's simulation architecture is credible evidence that deterministic
whole-system simulation can find storage and distributed failures. Its
mechanisms are tightly coupled to FoundationDB's C++ runtime, transport, disk,
and process abstractions. Decision: reject adoption, vendoring, or porting; keep
the design lesson that every nondeterministic boundary must be injectable and
every failure must be replayable.

## Mechanism comparison

| Mechanism | Sentinel today | Loom | Shuttle | Stateright | Turmoil | Jepsen |
|---|---|---|---|---|---|---|
| Weak-memory and lock interleavings | One optional owner-ordering model | Exhaustive bounded Rust synchronization and atomics | Task/thread schedule focus, not Loom's memory model | Abstract actions only | Single-thread async simulation | Black-box only |
| Deterministic async/task scheduling | Ordinary Tokio/threads and targeted fakes | Limited future support, state-space cost | Seeded random, PCT, DFS, exact replay | Abstract protocol steps | Deterministic Tokio host/runtime and virtual time | Real client/process schedules |
| Pause/restart/fault cuts | Named restore phases and selected panic/stall hooks | Branch/yield only | Controlled task choices and data nondeterminism | Explicit crash/recover actions | Barriers, partition, crash/bounce, unstable FS | Nemesis break/repair/final recovery |
| Safety/liveness state model | State machines exist in code/issues, no shared checker | Assertions in executable model | Assertions in scheduled test | Always/eventually/sometimes with paths | Test assertions over simulation | Checker over finite external history |
| Linearizability/serializability | Idempotency and monotonicity tests, no general checker | Possible only as custom small model | Possible custom oracle | Built-in bounded linearizability/sequential consistency | Requires custom history oracle | Knossos-backed linearizability and transaction checkers |
| History and shrinking | Logs/evidence are domain-specific | Checkpoint/execution path | Encoded schedule and causal replay filtering | Counterexample path; BFS shortest path | Seeded trace and controlled steps | Invoke/ok/fail/info histories and analysis/shrinking ecosystem |
| Network/storage realism | Real integration tests and runtime labs | None | Uncontrolled I/O excluded | Abstract network/storage | Simulated TCP/UDP/time and unstable crash FS | Real hosts/processes/network faults |
| 1:n and authority fit | One source of truth by design | Tests shared-memory code only | Tests adapters, no new authority | Models IDs/terms/refs without copying data | Simulated hosts refer to authority IDs | Histories reference operations and outcomes |
| Security | Production boundaries vary | Test-only, no new service | Test-only; fake effect ports required | Non-serving CI API only | In-process fake hosts/paths | High-privilege lab operator; isolate carefully |
| Maintenance/dependency | Loom already optional | Existing 0.7 dep | New Rust dev dep | New Rust dev dep | New Rust dev dep, unstable features isolated | No dependency; contract port only |

### Dependency, resource, and maintenance matrix

| Candidate | Production binary impact | CI resource hypothesis | Upgrade risk | Required boundary |
|---|---|---|---|---|
| Loom | None; optional dev feature | Exponential in threads/branches; use tiny exhaustive models | Memory-model semantics and cfg shims | Small pure concurrency kernel, no I/O |
| Shuttle | None; test-only | Iteration/depth budget; parallel portfolio optional | Scheduler serialization and wrapper API | Task/channel/random ports, fake effects |
| Stateright | None; test-only | State count, depth, memory, and property budget | Model/hash/symmetry semantics | Pure canonical state/action adapter |
| Turmoil | None; test-only | Virtual steps/hosts/messages and optional FS state | Unstable FS/barrier APIs | Transport/time/FS/barrier traits; no production type leak |
| Jepsen contracts | None | Real cluster tests are owner-target-specific | History schema and checker semantics | Sentinel lab driver emits canonical history |
| Kani | Existing verification tooling only | Unwind/state bound | Compiler/CBMC coupling | Pure sequential harness only |
| Apalache | Rejected | JVM/SMT and spec-state bound | Tool/language/spec drift | None until a later explicit decision |

## Decisions

Every row is one mechanism and has exactly one decision.

| ID | Mechanism | Decision | Rationale | Rejected alternatives |
|---|---|---|---|---|
| D1 | Rust weak-memory publication, atomics, and short lock protocols | **Configure existing dependency**: Loom | Already present, exhaustive within explicit bounds, directly proved one load-bearing owner ordering. | Reject ordinary thread stress as proof; reject Kani concurrency; reject replacing Loom with Shuttle. |
| D2 | Scalable task/channel/cancellation/retry schedule exploration | **Adopt dependency**: Shuttle, test-only, after #705 | Exact schedule replay plus random/PCT/DFS covers larger async structures than Loom. | Reject wall-clock sleeps and repeated Tokio tests; reject claiming probabilistic pass as proof. |
| D3 | Explicit owner/migration/outbox protocol safety and bounded liveness | **Adopt dependency**: Stateright, test-only, after #705 | Native Rust model/action/property boundary, bounded BFS/DFS/simulation, path evidence. | Reject a new hand-written checker; reject duplicate TLA+ as the first model. |
| D4 | Bounded operation-history linearizability | **Integrate** Stateright `LinearizabilityTester` through `OperationHistoryV1` | Reuses reviewed search semantics while Sentinel owns domain operations and authority IDs. | Reject implementing a new linearizability algorithm; reject embedding Jepsen/Knossos. |
| D5 | Virtual time, network partitions, host crash, source barriers, and storage durability cuts | **Adopt dependency**: Turmoil, test-only behind adapters, after #705 | Direct Tokio fit and current crash-FS/barrier tests; no runtime host required. | Reject production transport replacement; reject building another general virtual filesystem in #729. |
| D6 | Real-cluster nemesis and post-recovery history discipline | **Port algorithm/contract** from Jepsen into #556 lab tooling | Preserves invoke/outcome/repair/final-read rigor without JVM/Clojure or generic SSH authority. | Reject Jepsen as production/workspace dependency; reject simulator-only Cluster GA evidence. |
| D7 | Reproducible schedule/history artifact, budgets, and tiered CI | **Reimplement minimal** Sentinel-owned schema and runner policy | Tools need one public-safe envelope, common bounds, exact replay command, and owner oracle. | Reject tool-specific ad hoc logs; reject unbounded always-on exploration. |
| D8 | Sequential bounded invariants | **Keep Sentinel** Kani scope from #393 | Proven useful for pure invariants and explicitly distinct from concurrency. | Reject extending Kani claims to threads/atomics. |
| D9 | Symbolic protocol specification | **Reject** for the first implementation slice | A second state/action encoding would drift before #653/#556 stabilizes the vocabulary. | Reject premature Apalache/TLA+ dependency; permit a later decision gate. |
| D10 | Production-trace-to-schedule replay | **Reimplement minimal**, POST_M0 | Map redacted causal operation events to existing schedule actions; never replay raw payloads or effects. | Reject production payload capture, unsanitized task IDs, or automatic effect execution. |

## Failure schedule contract

### `FailureScheduleV1`

The proposed public schema contains:

```text
schema_version
schedule_id
owner_issue
model_or_adapter
tool_and_version
source_commit
seed_or_path_digest
bounds { threads, tasks, steps, states, depth, iterations, virtual_time }
initial_state_digest
actions[] { index, actor, operation_id, authority_generation, kind, args_digest }
faults[] { action_index, kind, target_ref, mode }
expected_terminal
invariants[]
observed_history_digest
counterexample_digest
```

Payloads, prompts, secrets, filesystem paths, host identities, and customer data
are forbidden. References are stable IDs plus content digests. A replay refuses
tool/version, schema, source commit, initial digest, or bounds mismatch.

### `OperationHistoryV1`

Each record is one of:

```text
invoke | ok | fail | info | blocked | quarantined | manual_recovery
```

It binds actor, operation ID, attempt, direct causation, authority generation,
request digest, response/effect receipt digest, logical sequence, and optional
model step. `info` means unknown outcome and is never coerced to `fail`.

### Executable schedule catalog

These are implementation-ready schedules, not tests executed by this research
issue.

| Schedule | Actions and injected cut | Required terminal oracle | Owner/class |
|---|---|---|---|
| S1 Owner publish | Concurrent term insert/mode publish, readiness close/open, snapshot rebuild, guard issue/validate, tick barrier; preempt at every atomic/lock edge. | No guard succeeds under stale owner/epoch/generation; cluster readiness implies complete visible maps. | New harness plus #501/#556; `M0_HARDENING` |
| S2 Idempotency compute | Two peers/methods/keys/digests race compute, capacity eviction, TTL expiry, panic before response publication, process restart. | One effect per durable owner; digest conflict typed; volatile cache loss never implies effect loss/success. | #501/#556 and operation owners; `M0_HARDENING` |
| S3 Event/outbox | Accept command, begin transaction, insert event, insert outbox, commit, poll, publish, ACK loss, mark, shutdown/drain, restart. | Event plus intent atomic; duplicate delivery visible in history; consumer receipt/dedup prevents duplicate authority effect. | #710/event owners; `M0_HARDENING` |
| S4 Projection frontier | Read batch, apply rows, crash before/after each row, offset update, generation flip, restart, duplicate event. | Derived view equals rebuild at declared generation; offset never authorizes work and never advances past unapplied source. | Event/projection owners; `M0_HARDENING` |
| S5 Forward queue | Acquire/cancel/grant/release/resize/shutdown across N waiters. | Active never exceeds limit; no permit leak; surviving waiters preserve FIFO; canceled waiter never executes unless grant won and is returned. | New harness/Gateway owner; `M0_HARDENING` |
| S6 Room sequencing | Register P1, multiple P3 waits, complete, timeout, hot disable, duplicate completion, shutdown. | Each P3 has one terminal context/no-context outcome; no close/send panic; no stale response attached to another request. | New harness/Gateway owner; `M0_HARDENING` |
| S7 Runtime cancellation | Reserve invocation, enqueue, launch, cancel, child exits, receipt persists, reply drops, daemon restarts, reconcile. | One durable invocation/effect outcome; unknown outcome blocks automatic repeat; no orphan gains authority. | #710 plus Workbench/runtime owners; `M0_HARDENING` |
| S8 Restore | Admission close, per-store phase, fsync/rename, process death, rollback phase, projection rebuild, runtime reconcile, admission open. | One committed generation or durable blocked/quarantine/manual state; never mixed writable stores. | #722/#729; `M0_HARDENING` |
| S9 CAS resolve/GC | Parallel miss, holder failure, pull, verify, file sync, rename, dir sync, advertise, pin, GC, restart. | At most one transfer per key per node; no false content; referenced/pinned bytes survive; incomplete publication reconciles. | #729/#556; `POST_M0` unless single-node artifact path |
| S10 Migration | Every #501 saga step, lost reply, coordinator/source/target restart, partition, duplicate move, stale generation, route flip. | Exactly one owner/routable target; resume, permitted rollback, or manual recovery according to durable commit point. | #501; `POST_M0` |
| S11 Replica promotion | Input/tick/side-effect claim, follower lag, quorum loss, fence proof, promotion, old-owner message, route publish. | No stale writer or duplicate side effect; promotion requires complete RecoveryPoint and authority proof. | #653/#556; `POST_M0` |
| S12 Storage durability | Write, sync data, sync file, rename, sync directory, commit marker, truncate/fail sync, kill, reopen/integrity/reconcile. | Engine-specific semantic state matches the last declared durable boundary; unknown/corrupt state blocks readiness. | #729; `M0_HARDENING` |
| S13 Customer work effect | Every #710 cut from acceptance through delivery, including effect-before-receipt, cancellation, approval, version change, and missing CAS. | Resumed, idempotently replayed, compensated, durably blocked, quarantined, or manual recovery; never silent abandonment or blind repeat. | #710 and M0 owners; `M0_HARDENING` |
| S14 Real cluster history | Generate concurrent owner/CAS/migration operations, partition/repair/restart/revoke, then final authoritative reads. | History linearizable to declared spec or produces a minimized counterexample; all nemesis changes repaired. | #556; `POST_M0` |

### State-space and CI policy

| Tier | Trigger | Bound and evidence | Failure handling |
|---|---|---|---|
| T0 exact | Every relevant PR | Small Loom and Stateright models with committed thread/state/depth bounds; exact path on failure | Any counterexample fails CI. Bound exhaustion without completion is a distinct failure unless explicitly approved. |
| T1 seeded | Every relevant PR | Fixed Shuttle/Turmoil regression seeds plus a small deterministic portfolio; schedule artifact retained | New failure seed becomes a permanent regression. |
| T2 rotating | Nightly/weekly | Rotating Shuttle PCT/random seeds and larger Stateright simulation/Turmoil scenarios under fixed resource budgets | Failure opens evidence; pass is search coverage, not proof. |
| T3 owner target | Issue-specific | Real process/VM/cluster histories only on the owner's declared target, with sidecars and repair receipts | Required for runtime claims; never substituted by build-host timing or simulation. |
| T4 trace-derived | POST_M0 | Public-safe redacted operation/action references converted to a bounded offline schedule | No raw payload/effect replay; human approval before a new regression joins T0/T1. |

Resource gates record explored permutations/states/steps/iterations and bound
exhaustion. Wall-clock duration is diagnostic only and never a product
performance benchmark.

## M0 classification and owner routing

| Finding | Classification | Evidence and owner |
|---|---|---|
| Critical Rust owner ordering has only one optional Loom test | `M0_HARDENING` | Directly protects authority but no current failing execution was found; new harness plus #501/#556. |
| Task/channel cancellation and retry lack reproducible schedules | `M0_HARDENING` | Relevant to accepted work and unknown outcomes; #710 plus new harness. |
| Event publish/mark and projection/apply offset cuts are tested locally but not systematically composed | `M0_HARDENING` | At-least-once and cross-store cuts are real; event/projection owners and #710. |
| Gateway grant/cancel and room complete/timeout have no portable schedule artifact | `M0_HARDENING` | Current mutex/channel logic is concrete but ordinary Go scheduling is probabilistic; new harness/Gateway owner. |
| Deterministic storage crash harness is not implemented | `M0_HARDENING` | #729 already owns authoritative-store durability and readiness. |
| Bounded migration and replica protocol models | `POST_M0` | Cluster capability, already owned by #501/#653/#556. |
| Real N-node Jepsen-style history validation | `POST_M0` | Cluster GA gate in #556, not M0 single-node acceptance. |
| Apalache/TLA+ second specification | `POST_M0` | Decision-gated after stable #653/#556 vocabulary. |
| Production-trace-derived replay | `POST_M0` | Useful expansion, no current defect, privacy and effect-safety work required. |
| Kani concurrency extension | Rejected | Upstream explicitly does not support concurrent verification. |

No row is newly `BLOCKS_M0`: this audit found coverage gaps, not a concrete
source execution that demonstrably loses or duplicates customer work. If an
accepted schedule later exposes such a path, the owning implementation issue
must reclassify that defect based on the counterexample.

## Proposed implementation-owner contracts

These contracts are proposals only. No issue body, label, comment, or child
issue is changed until ORC approves the complete package.

### Contract A: Sentinel deterministic concurrency and failure-schedule harness

**Proposed parent:** #659.
**Proposed components:** runtime, daemon, cortex, testing.
**Classification:** `M0_HARDENING`.

**Scope**

- Add `FailureScheduleV1` and `OperationHistoryV1` as test/evidence schemas.
- Configure the existing Loom feature for a bounded always-run critical lane.
- After #705 approval, add Shuttle and Stateright as test-only dependencies and
  add adapters that do not leak their types into production APIs.
- Add deterministic barrier/fake-effect ports for the Limbo outbox,
  projection frontier, daemon command/reply lifecycle, Gateway forward queue,
  and room sequencing.
- Implement schedules S1-S7 and S13 with exact replay commands and
  authoritative final-state oracles.
- Add T0/T1 CI routing and machine-readable coverage/bound reports.
- Register tool pins and upgrade/replay requirements with #656.

**Dependencies**

- #705 before either new dependency enters a manifest.
- #656 for pin and upgrade policy.
- #710 for canonical external-effect outcomes.
- Existing event/projection, Gateway, Workbench, and runtime owners for their
  domain adapters.

**Acceptance criteria**

1. Every schedule file validates schema, pin, commit, bounds, action order, and
   expected terminal state fail-closed.
2. Loom exhaustively checks the declared owner/readiness/single-flight kernels
   and reports completed permutations, not only test PASS.
3. Shuttle replays an injected task/channel failure from the exact encoded
   schedule after a fresh process start.
4. Stateright produces and then replays a shortest bounded counterexample for a
   deliberately broken owner/outbox model; the fixed model satisfies declared
   properties within committed bounds.
5. `OperationHistoryV1` preserves unknown outcomes and rejects missing,
   conflicting, stale-generation, or out-of-order records.
6. Gateway grant/cancel and complete/timeout schedules prove no permit leak,
   duplicate terminal, stale context, or concurrency-limit violation.
7. Event/outbox/projection/runtime schedules end only in the #710 terminal
   taxonomy and never infer an external effect from task completion.
8. Always-run CI includes T0 and fixed T1 regression seeds. Rotating exploration
   is separate and cannot make the required context flaky.
9. Evidence contains structural counts, exact seeds/paths/digests, bounds,
   counterexamples, and replay outputs. It contains no private paths, hosts,
   payloads, prompts, or credentials.

**Negative criteria**

- No production binary dependency or runtime scheduler replacement.
- No wall-clock sleep as a synchronization oracle.
- No PASS when bounds exhaust without the policy-declared completion mode.
- No model/history row becomes business, owner, artifact, or effect authority.
- No unknown outcome becomes success/failure by omission.
- No test reaches a real provider, customer, Git remote, runtime VM, or
  production data path.
- No dependency/fork/version change outside #705/#656.

**Target tests and benchmarks**

- Rust gates only through `cargo remote -c --` on the implementation issue.
- Go tests run under the repository's approved CI path.
- Runtime target class `NONE` for T0/T1.
- Structural measurements: permutations, unique states, maximum depth, schedule
  steps, iterations, history operations, shrink/path length, and bound
  exhaustion.
- No build-server timing is product evidence. Any later runtime claim uses the
  owning issue's declared product/cluster target with sidecars.

**Rollout and rollback**

1. Land schemas and validators with negative fixtures.
2. Add existing Loom lane.
3. Add dependency-approved adapters and deliberately failing proofs.
4. Add fixed regression schedules before rotating exploration.
5. Enable required CI only after deterministic replay is stable.
6. Roll back CI routing and test-only adapters together; production behavior is
   unchanged. Retain discovered regression schedules.

**TOGAF delta**

Add the layered evidence taxonomy and schedule/history artifact contract from
the target-delta section. Do not name a simulator as runtime architecture.

### Existing-owner delta B: #501 migration

- Model the canonical 17-step saga as explicit state/actions including
  coordinator, source, target, authority, route, staged snapshot, acknowledgments,
  and durable participant outcomes.
- Implement S10 with Stateright for safety and Turmoil for deterministic
  transport/restart cuts.
- Safety properties: at most one writable owner, no routable target before the
  durable gates, no stale generation, no duplicate finalization, and no lost
  staged digest after its declared commit point.
- Liveness is bounded and terminal: resume, permitted rollback, or
  `ManualRecoveryRequired`; no silent wait.
- Negative tests mutate one load-bearing term, transition sequence, certificate
  identity, digest, route gate, or acknowledgement and require a counterexample.
- Real two-node evidence and pause/resource benchmarks remain exactly #501's
  declared target responsibility; simulation timing is not substituted.
- Rollback removes test-only adapters, never weakens the migration gates.

### Existing-owner delta C: #556 and #653 cluster safety

- #653 defines the canonical `ReplicaGroup` state/action vocabulary and safety
  properties for owner terms, RecoveryPoints, input/tick order, lag, promotion,
  side-effect claims, inbound/outbox positions, and route visibility.
- #556 implements S11 and S14: Stateright bounded models, Turmoil protocol
  simulation, then real-cluster Jepsen-contract histories.
- Every real history records invoke/ok/fail/info, nemesis action, authority
  generation, and post-repair reads. `info` remains unknown.
- Negative tests cover stale writers, revocation on an open connection,
  partitioned old owner, lost owner commit reply, duplicate block advertisement,
  lagging follower, missing RecoveryPoint, and route publication before
  activation.
- Target tests and N-node performance remain #556's runtime/benchmark contract.
  T0/T1 run without VMs; T3 alone supports Cluster GA claims.
- Rollback disables experimental cluster test adapters and restores the lab,
  while preserving histories and counterexamples.
- TOGAF delta links the model vocabulary and real-history gate; no model checker
  becomes the cluster authority.

### Existing-owner delta D: #729 storage faults

- Evaluate pinned Turmoil `unstable-fs` and barriers behind a Sentinel
  `FaultStorage` test adapter before implementing custom filesystem mechanics.
- Implement S12 for each redb/SQLite/CAS construction class with engine-specific
  semantic integrity and readiness oracles.
- Required faults: unsynced data loss, unsynced directory-entry loss, truncate,
  short/error write, sync error, rename error, process death before/after each
  visibility boundary, and crash during compaction/backup/restore.
- A schedule must replay the same fault position and outcome digest. Random sync
  probability alone is insufficient evidence.
- Negative tests prove that synced file data without a synced directory entry
  can still disappear and that generic file survival cannot substitute for
  engine semantic integrity.
- #729 retains its declared target recovery/compaction benchmarks. Simulation
  reports only steps/faults/state counts, never durability latency.
- Rollback pins or removes the unstable adapter without changing production
  durability policy; retain engine fixtures and schedule schema.
- TOGAF delta names deterministic storage fault testing as evidence, not as a
  storage engine or backup primitive.

### Existing-owner delta E: #710 durable execution

- Express every required #710 failure-matrix cut as S13 actions and terminal
  oracles over references to workflow, invocation, event, artifact, QA, release,
  delivery, policy, credential, and owner generations.
- The schedule engine supplies barriers and history; #710 remains the sole owner
  of durable-execution state/effect semantics.
- Negative tests cover effect-before-receipt, receipt-before-workflow, unknown
  provider outcome, cancellation/re-arm, in-flight schema/profile upgrade, stale
  authority, missing CAS, and admission before restored generations agree.
- Target tests and issue-specific benchmarks remain with #710 and its
  implementation owners. Fake-effect T0/T1 tests use no token, provider, runtime
  VM, or customer endpoint.
- Rollback removes adapters without deleting authoritative receipts or weakening
  fail-closed unknown-outcome handling.

### POST_M0 decision gate: symbolic specifications and trace-derived schedules

After #653/#556 stabilizes its state/action vocabulary, compare the maintenance
and counterexample value of a small TLA+/Apalache spec against the accepted
Stateright models. Adoption requires:

- one generated or mechanically checked mapping between canonical actions and
  the spec;
- a demonstrated invariant found beyond the Rust model;
- bounded resource and upgrade ownership;
- #705/#656 review for any tool dependency;
- no duplicate runtime authority.

Production-trace-derived schedules require public-safe field allowlists,
one-way payload digests, redaction tests, effect-free offline replay, retention,
and explicit operator promotion into regression fixtures.

## Acceptance-criteria mapping

| Criterion | State at REVIEW_READY | Evidence |
|---|---|---|
| AC-1 Sentinel baseline | PASS | Current source/test map, spawn/lock/channel/store/crash map, TOGAF targets, and live owner readback. |
| AC-2 landscape | PASS | Eight candidates, ten-factor rubric, scores, shortlist, and rejection reasons. |
| AC-3 pinned deep reviews | PASS | Five immutable commits with implementation, tests, failures, security/license, and operations. |
| AC-4 mechanism matrix | PASS | Mechanism and dependency/resource matrices cover correctness, failures, determinism, 1:n, security, maintenance, dependency, and boundary. |
| AC-5 one decision per mechanism | PENDING ORC APPROVAL | D1-D10 each has one decision and rejected alternatives; no upstream timing claim. |
| AC-6 implementation owners | PENDING ORC APPROVAL | Complete proposed Contract A and deltas B-E exist, but no GitHub mutation is permitted before approval. |
| AC-7 classification | PENDING OWNER ACKNOWLEDGEMENT | Every finding is classified; owner routing is proposed but not yet acknowledged/materialized. |
| AC-8 public study | PASS candidate | One English/ASCII file; focused public-safety, link, render, typo, and diff gates are listed below. |
| AC-N1 dependency popularity | PASS | Adoption is mechanism-specific and dependency-gated by #705. |
| AC-N2 provenance/security/maintenance | PASS | Every deep review records pin, license, policy state, failure limits, and maintenance boundary. |
| AC-N3 old status as proof | PASS | Closed #393/#498/#693 are explicitly historical; current source boundaries remain stated. |
| AC-N4 runtime/timing | PASS | No runtime, VM, Rust/Cargo, deployment, or benchmark action occurred. |
| AC-N5 accepted gap owner | PENDING ORC APPROVAL | No accepted gap is claimed closed; complete owner contracts await materialization approval. |

## Reproduction and verification

### Upstream provenance

The read-only source review used:

```text
git clone --depth 1 --filter=blob:none <repository> <name>
git -C <name> rev-parse HEAD
git -C <name> log -1 --format='%cI %s'
find <name> -maxdepth 3 -type f -iname 'LICENSE*'
find <name> -maxdepth 3 -type f -iname 'SECURITY.md'
rg -n '<mechanism or failure term>' <source-and-test-roots>
nl -ba <load-bearing-file> | sed -n '<range>p'
```

Pins:

```text
loom       948c8cc78b178ede6eeff3afc7d97f2f4ea08559
shuttle    cd57cf9d04c3056eb82a6fd7bd272d264b5c290c
stateright ab8c8be9341505e0f71edbe5dd88ed275bd976a4
jepsen     58b4c48629fb31a333d7101ad7554c6d59c9ad61
turmoil    684acc1a8eea3a9cf2c6959dc47b69dba981cac1
kani       2f56cf3503ef6e495394a820013c89610f8f550a
apalache   ac61ee884068a927c9861ee4cb4a4516fa2f2ac6
```

### Focused final checks

The final delivery must record exact outputs for:

```text
python3 <private-work-root>/verify/check-pinned-sources.py
python3 <private-work-root>/verify/check-structure.py
python3 <private-work-root>/verify/check-links.py
python3 <private-work-root>/verify/check-gfm.py
python3 <private-work-root>/verify/check-public.py
python3 <private-work-root>/verify/run-negative-tests.py
typos docs/research/oss/concurrency-model-checking-fault-injection.md
git diff --check
git diff --name-only <delivery-base>...HEAD
```

Negative fixtures must reject:

- an unpinned upstream link or wrong commit;
- a missing deep-review source, test, failure, license, security, or operations
  field;
- a missing decision or two decisions for one mechanism;
- a missing M0 class, owner dependency, negative criterion, target test,
  benchmark, rollout, rollback, or TOGAF delta;
- non-ASCII text, private infrastructure, absolute paths, host/user fields,
  copied source blocks, or a closing keyword.

## Known limits

- No upstream project was built or executed. Source and test inspection supports
  mechanism decisions, not runtime compatibility.
- The current main branch does not yet contain final #694/#695/#696 business
  implementations. Their live contracts and #710 define schedule inputs; this
  study does not claim those paths are implemented.
- No schedule in this document was run. They are implementation-ready contracts
  awaiting owner approval.
- Turmoil filesystem and barrier APIs are unstable at the reviewed pin.
- Stateright liveness properties require careful bounded/cycle semantics.
- Shuttle passes are probabilistic unless an exhaustive scheduler completes
  within declared bounds.
- Loom has documented memory-model limitations and state-space growth.
- Jepsen-style histories require trustworthy client boundaries and full nemesis
  repair; a malformed history can produce a false conclusion.
- No maintainer decision, owner acknowledgement, dependency approval, TOGAF
  mutation, or implementation issue is implied by REVIEW_READY.
