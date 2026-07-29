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
8. **Use Go 1.26.5 `testing/synctest`, the race detector, and explicit
   implementation barriers for the Gateway lane.** `synctest` provides fake time
   and quiescence for Go code; it does not enumerate schedules. A Rust protocol
   model is specification evidence only until both languages consume and emit the
   same versioned test vectors and traces.
9. **Reimplement only the thin Sentinel-owned control plane:** a versioned
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

### Reproducible spawn and task-boundary inventory

The productive-source scan is deliberately broader than `tokio::spawn`:

```text
rg -n --glob '**/src/**/*.rs' --glob '**/src/*.rs' \
  '(tokio::spawn|tokio::task::spawn|spawn_blocking|std::thread::spawn|thread::spawn|\.spawn\(async|JoinSet|\.spawn\(move \|\|)' \
  crates services
rg -n --glob '*.go' --glob '!**/*_test.go' \
  '^[[:space:]]*go[[:space:]]+(func|[A-Za-z_(])|\.Go\(func' cmd services
```

The Rust classification removes doc examples, test modules, the test-only
Firecracker fixture at `firecracker.rs:213`, three Bevy entity spawns at
`world.rs:2143,2259,2260`, and the `JoinSet` declaration at
`cluster_control.rs:484`; it adds the four `thread::Builder::spawn` calls not
matched by the first alternatives. Result: **49 productive Rust task/thread
starts in 14 files**. The Go scan returns **15 productive goroutine starts in
six files**. The line lists below account for every result.

| Productive callsites and count | Class | Cancellation, shutdown/join, panic/error, retry/durable cut | Canonical owner |
|---|---|---|---|
| [`sentinel-limbo/src/lib.rs:116,167,211,248,300,323,357,396`](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/crates/sentinel-limbo/src/lib.rs#L110-L410) (8) | Request task (`spawn_blocking`) | Each handle is awaited and join/SQL errors return to the caller. Cancellation cannot stop an already running closure; the connection mutex and SQLite commit define visibility. | Limbo/event-store owner; #710 consumes effect outcomes. |
| [`block_pull.rs:202,206,253`](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/crates/sentinel-cluster-control/src/block_pull.rs#L190-L270) (3) | One deliberate fire-and-forget accept task plus two connection tasks | `BlockPullServer::close` closes the endpoint; connection/stream errors are logged and detached tasks are not joined. Per-peer semaphore and live pin recheck precede CAS read; no write authority is created. | Active cluster/storage owners #556 and #729; #498 is verified history. |
| [`server.rs:153,206,211,256`](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/crates/sentinel-cluster-control/src/server.rs#L140-L270) (4) | One connection-lifetime task, one deliberate fire-and-forget accept task, two connection tasks | Endpoint close cancels accepts/connections; per-stream failures are logged, not joined. Revocation is rechecked before dispatch; idempotency remains process-local and durable effects belong to handlers. | #501/#556/#653 plus each mutating operation owner. |
| [`sentinel-dashboard-backend/main.rs:52,61,62`](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/services/sentinel-dashboard-backend/src/main.rs#L40-L75) (3) | Deliberate fire-and-forget long-lived tasks | WebTransport, event subscriber, and log pusher handles are dropped. Process exit is shutdown; task errors are logged internally. Broadcast/projection state is derived and must reconcile after lag/restart. | Dashboard/read-model owner. |
| [`sentinel-dashboard-backend/wt.rs:51,145`](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/services/sentinel-dashboard-backend/src/wt.rs#L40-L155) (2) | Connection tasks | Connection close cancels work; errors are logged by the parent and tasks are not joined. Lag drops derived frames; EventLog CAS mutations remain behind their mutex/validator. | Dashboard/read-model owner. |
| [`cluster_control.rs:497`](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/services/sentinel-daemon/src/cluster_control.rs#L475-L525) (1) | Request task | All heartbeat tasks are consumed with `join_next`; timeout and join errors reduce the delivered count. No durable membership fact is inferred from task completion alone. | #556/#653 membership owner. |
| [`evolution_task.rs:75,110`](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/services/sentinel-daemon/src/evolution_task.rs#L65-L135) (2) | Deliberate fire-and-forget long-lived queue plus request task | Sender drop ends the queue; per-job semaphore bounds concurrency. Handles are dropped, failures become explicit `EvolutionResult`; downstream persistence remains authoritative. | Evolution/work-execution owner and #710 for outcomes. |
| [`llm_bridge.rs:681,706,870,994`](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/services/sentinel-daemon/src/llm_bridge.rs#L650-L1010) (4) | One long-lived supervised recovery task, one deliberate receiver thread, two request tasks | Recovery has a watch shutdown and is awaited at bridge exit; receiver ends on channel close. Request timeouts, semaphore permits, completion records, and retry queues distinguish unknown from completed provider effects. | #710 plus Gateway/runtime owners. |
| [`operator_api.rs:756,767,3301`](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/services/sentinel-daemon/src/operator_api.rs#L740-L780) (3; helper at [source](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/services/sentinel-daemon/src/operator_api.rs#L3288-L3310)) | One long-lived supervised child, one connection task, one deliberate fire-and-forget waiter | Server returns its parent handle; connection tasks log errors. The operator-only anomaly helper child is reaped by a detached thread. Command/reply channels and store receipts, not HTTP task completion, own outcomes. | Daemon/operator API; #710 for accepted work. |
| [`orchestrator.rs:1421,1593,1655,1688,1702,1727,1739,1882,1941,1952,1986,2005,2020,7273`](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/services/sentinel-daemon/src/orchestrator.rs#L1400-L2030) (14; config invalidation at [source](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/services/sentinel-daemon/src/orchestrator.rs#L7255-L7300)) | One long-lived supervised child (ECS), 12 deliberate long-lived tasks/threads, one deliberate fire-and-forget invalidation | ECS is joined at daemon shutdown; FUSE, provision, LLM, and the other Tokio handles are not joined. Errors are logged or reflected through service/readiness state. Event/outbox, cluster metadata, runtime receipts, and restore gates remain durable boundaries; DNA invalidation is explicitly best-effort. | Daemon composition; domain owners #501/#556/#710/#729/#751. |
| [`llm_analyzer.rs:108`](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/services/sentinel-daemon/src/platform_controlplane/llm_analyzer.rs#L95-L135) (1) | Deliberate fire-and-forget long-lived task | Bounded-channel close ends the loop; errors are logged and the handle is dropped. Event append and platform command dispatch define observable effects. | Platform-control/work-execution owner and #710. |
| [`service_health.rs:61`](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/services/sentinel-daemon/src/service_health.rs#L40-L90) (1) | Deliberate fire-and-forget long-lived thread with internal panic supervision | The handle is dropped; control/result-channel close ends polling. `catch_unwind` records and restarts a panicked worker in the same thread. Health snapshots are derived, never service authority. | Daemon supervision owner. |
| [`agent-runtime/main.rs:29`](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/services/agent-runtime/src/main.rs#L20-L52) (1) | Deliberate long-lived reader thread | EOF or `shutdown` clears the atomic flag; no handle is retained. Process exit and daemon reconciliation own lifecycle, not the heartbeat thread. | Runtime owner and #710. |
| [`block_pull_probe.rs:223,238`](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/services/sentinel-daemon/src/bin/block_pull_probe.rs#L210-L250) (2) | Request tasks | Both probe threads are joined and panic fails the diagnostic. CAS verification is read-only probe evidence, not production publication. | #729/#556 diagnostic owner. |
| [`apicp/observer.go:145,147`](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/cmd/cortex-gateway/internal/apicp/observer.go#L130-L155) (2) | Deliberate fire-and-forget long-lived goroutines | Shared `stopCh` stops loops; there is no join. Retry/load swaps are mutex-protected derived learning state and cannot authorize provider work. | Gateway control-plane owner. |
| [`proxy/claude_code.go:185`](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/cmd/cortex-gateway/internal/proxy/claude_code.go#L170-L205) (1) | Request task | The stderr reader is joined through `stderrDone`; `CommandContext` cancels the child. Parsed output plus process result still requires the caller's durable effect receipt. | Gateway provider owner and #710. |
| [`ticksync/buffer.go:66,95`](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/cmd/cortex-gateway/internal/ticksync/buffer.go#L53-L108) (2) | Deliberate fire-and-forget long-lived goroutines | Per-generation `stopCh` cancels loops, but no join proves the old loop exited before replacement. Pending responses are extracted under lock and flushed outside it. | Proposed Go child A2. |
| [`cortex-gateway/main.go:246,269,296,604,612`](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/cmd/cortex-gateway/main.go#L235-L310) (5; servers at [source](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/cmd/cortex-gateway/main.go#L590-L625)) | Two long-lived supervised HTTP children and three deliberate fire-and-forget ticker goroutines | Three ticker loops end only with process lifetime; two HTTP servers receive bounded `Shutdown`. Server errors call `os.Exit`; no join aggregates ticker failures. Config/queue/sequencing locks protect derived runtime policy, not effect completion. | Gateway composition; proposed Go child A2. |
| [`sentinel-judge/main.go:144,156,166`](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/services/sentinel-judge/main.go#L135-L180) (3) | Long-lived supervised children | Context cancel stops consumers and HTTP shutdown is bounded; handles are not joined. NATS/SQLite consumer frontiers own redelivery and alert publication. | Judge owner; cluster/runtime histories only where declared. |
| [`sentinel-nats-bridge/main.go:142,153`](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/services/sentinel-nats-bridge/main.go#L130-L165) (2) | Long-lived supervised children | Context cancellation, bounded HTTP shutdown, and NATS drain are explicit; no goroutine join. Poll/store/publish boundaries determine retry and duplicate exposure. | NATS bridge/event owner. |

`OutboxPublisher::run` is a separate audited boundary: it is one exported
long-lived loop with watch shutdown and a final drain
([source](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/crates/sentinel-limbo/src/outbox_publisher.rs#L94-L135)),
but the pinned productive tree has **zero composition callsites**; only tests
spawn it. Its send-before-`mark_published` cut remains load-bearing and must not
be represented as a currently supervised production task.

### Load-bearing lock, channel, and publication inventory

| Boundary and exact source | Count/owner | Publication or failure cut and required oracle |
|---|---|---|
| Owner tick barrier plus three `OwnerRegistry` maps ([source](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/crates/sentinel-common/src/fencing.rs#L21-L33), [source](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/crates/sentinel-common/src/fencing.rs#L575-L607)) | 1 global `Mutex`, 3 `RwLock`; #501/#556 | Map/term/base/saga publication must precede mode/readiness visibility; stale generation never validates. |
| Daemon composition channels ([source](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/services/sentinel-daemon/src/orchestrator.rs#L1520-L1548)) | 13 channel pairs: 1 bounded Tokio plus 12 standard `mpsc`; daemon and #710 | Send accepted, receiver drop, late reply, and daemon restart must map to a durable operation outcome, never task-liveness inference. |
| LLM bridge recovery and work queues ([source](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/services/sentinel-daemon/src/llm_bridge.rs#L650-L710)) | 1 watch, 1 bounded `mpsc`, semaphore, retry mutexes; #710 | Provider success before completion persistence is unknown/recoverable; shutdown drains committed completions without blind repeat. |
| Evolution and platform-analysis queues ([source](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/services/sentinel-daemon/src/evolution_task.rs#L70-L120), [source](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/services/sentinel-daemon/src/platform_controlplane/llm_analyzer.rs#L100-L130)) | 2 bounded Tokio queues and queue-state mutex; work-execution owners | Full/closed queue and canceled job produce typed terminal results; event append/dispatch cannot be inferred from dequeue. |
| QUIC peer registry and idempotency cache ([source](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/crates/sentinel-cluster-control/src/server.rs#L26-L74), [source](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/crates/sentinel-cluster-control/src/idempotency.rs#L45-L79)) | 1 `RwLock`, 1 scoped cache `Mutex`; #501/#556/#653 | Revocation must close/reject every stream; volatile reply dedup never proves a durable mutating effect. |
| Dashboard broadcast and EventLog CAS ([source](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/services/sentinel-dashboard-backend/src/lib.rs#L200-L258)) | 1 broadcast(256), 1 CAS mutex, 1 async rate-limit mutex; dashboard owner | Lag is explicit and reconciled from authoritative projections; CAS lock publication cannot become event authority. |
| Gateway forward queue, sequencing, and tick buffer ([source](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/cmd/cortex-gateway/internal/forwardqueue/manager.go#L37-L149), [source](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/cmd/cortex-gateway/internal/sequencing/queue.go#L31-L195), [source](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/cmd/cortex-gateway/internal/ticksync/buffer.go#L37-L180)) | 3 mutex-owned state machines and per-waiter/done/stop channels; proposed A2 | Grant/cancel, close/timeout, disable/flush, and old/new loop overlap require Go implementation oracles under barriers, `synctest`, and race detection. |
| Event plus outbox transaction and publication ([source](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/crates/sentinel-limbo/src/event_store.rs#L997-L1105), [source](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/crates/sentinel-limbo/src/outbox_publisher.rs#L136-L185)) | One SQLite mutex/transaction; event owner and #710 | Event+intent commit is atomic; publish succeeds before mark, so restart may redeliver and consumers must deduplicate by authority identity. |
| Projection apply/frontier ([source](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/crates/sentinel-projection/src/worker.rs#L82-L151), [source](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/crates/sentinel-projection/src/store.rs#L675-L705)) | Projection SQLite mutex/transaction plus EventStore offset; projection owner | Crash between derived-store commit and source offset may repeat apply; rebuild equivalence and generation-aware idempotency are the oracle. |
| redb/CAS/filesystem and whole-product recovery cuts | Engine-specific writers; #729 and #751/#753/#755 | Data sync, file sync, rename, directory sync, generation seal, restore activation, Release/ACK/readiness CAS must end in one committed generation or durable quarantine/manual state. |

The inventory is causal, not merely syntactic. A test is useful only when its
final oracle checks the authoritative cross-boundary outcome, not merely that
all tasks joined.

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
| [#751](https://github.com/silentspike/project-sentinel/issues/751) | Open, blocked epic | Ordered whole-product recovery program | Own recovery protocol models through its live children, not closed research #722. |
| [#753](https://github.com/silentspike/project-sentinel/issues/753) | Open, blocked | R1 local RecoveryPoint coordinator, immutable seal, Release/ACK/readiness CAS | Own S8 capture/seal crash schedules and Stateright/Turmoil protocol evidence. |
| [#755](https://github.com/silentspike/project-sentinel/issues/755) | Open, blocked | R3 quarantine, staged restore, restart, and destructive drills | Own S8 restore/recovery schedules and unknown-effect oracles; #729 retains engine/storage semantics. |
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
| [Go `testing/synctest`](https://github.com/golang/go/tree/c19862e5f8415b4f24b189d065ed739517c548ba/src/testing/synctest) | Go 1.26.5 tag commit `c19862e5f8415b4f24b189d065ed739517c548ba`; Gateway toolchain pin | BSD-3-Clause | [Go security policy](https://go.dev/security/policy) | 22 | Yes, language lane | Configure standard-library fake-time/quiescence tests plus `go test -race`; never call it exhaustive exploration. |

The inventory intentionally includes eight candidates across exhaustive
weak-memory exploration, randomized schedulers, explicit state models, black-box
histories, network/filesystem simulation, bounded verification, symbolic formal
specification, and Go-native virtual-time/quiescence testing.

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
- Go `testing/synctest` is the only reviewed mechanism here that executes the
  Gateway's actual Go goroutines and timers. It complements `go test -race` and
  explicit barriers; Rust models cannot replace it.

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

### Pinned Go language-lane review: `testing/synctest` and race detector

**Pin and mechanism.** The Gateway module pins `toolchain go1.26.5`
([Sentinel source](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/cmd/cortex-gateway/go.mod#L1-L6)).
The immutable Go tag commit is
`c19862e5f8415b4f24b189d065ed739517c548ba`. `synctest.Test` runs a
self-contained goroutine bubble, waits for all bubble goroutines to exit, and
fails on deadlock
([source](https://github.com/golang/go/blob/c19862e5f8415b4f24b189d065ed739517c548ba/src/testing/synctest/synctest.go#L274-L310)).
The fake clock advances only when bubble goroutines are durably blocked; channel,
`Cond`, `WaitGroup`, and timer waits qualify, while mutexes, network I/O, and
system calls do not
([source](https://github.com/golang/go/blob/c19862e5f8415b4f24b189d065ed739517c548ba/src/testing/synctest/synctest.go#L18-L115)).

**Tests, failures, and limits.** Upstream tests require deadlock panics for a blocked root,
child, and ticker
([tests](https://github.com/golang/go/blob/c19862e5f8415b4f24b189d065ed739517c548ba/src/internal/synctest/synctest_test.go#L512-L538)).
`T.Deadline`, `T.Parallel`, and nested `T.Run` panic inside the bubble
([tests](https://github.com/golang/go/blob/c19862e5f8415b4f24b189d065ed739517c548ba/src/testing/synctest/synctest_test.go#L122-L142)).
Network sockets, external processes, package-global wait groups, mutex blocking,
and system calls can prevent quiescence or escape the bubble. Most importantly,
`synctest` controls time and observes quiescence; it **does not enumerate every
goroutine schedule**. The race detector remains a separate dynamic detector.
Sentinel already runs the Gateway under `go test -race -count=1 ./...`
([CI source](https://github.com/silentspike/project-sentinel/blob/cbd7c25d2bb57df99462d4a180aae5ab00eaf651/.github/workflows/ci.yml#L318-L330)).

**Security, license, and operations.** Go uses a BSD-3-Clause-style repository
license ([license](https://github.com/golang/go/blob/c19862e5f8415b4f24b189d065ed739517c548ba/LICENSE))
and publishes an official vulnerability policy. This is standard-library test
code, not a new dependency or service. Tests must use in-memory fakes and
test-only deterministic barriers; they must not reach providers, runtime hosts,
real sockets, or production paths.

**Sentinel fit and proof boundary.** It directly fits Gateway grant/cancel,
complete/timeout, tick flush, observer retry, and shutdown tests. `go test -race`
detects observed unsynchronized memory access but neither tool proves all
interleavings. A Rust Stateright model is protocol-specification evidence only.
It supports a Go implementation claim only when Rust and Go validate the same
versioned vectors, Go emits a conforming `OperationHistoryV1`, and Go
implementation tests reproduce the relevant cut through named barriers.

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

### Unscored background: FoundationDB simulation

FoundationDB simulation was a discovery lead only. It was not pinned or reviewed
to this study's source/test/security standard, so it is removed from the scored
inventory and from all AC counts. No adoption, port, or rejection decision relies
on it. The general hypothesis that nondeterministic boundaries should be
injectable is tested against the pinned candidates above instead.

## Mechanism comparison

| Mechanism | Sentinel today | Loom/Shuttle | Stateright | Turmoil | Jepsen | Go `synctest`/race |
|---|---|---|---|---|---|---|
| Weak-memory and lock interleavings | One optional owner-ordering model | Loom exhaustively bounds Rust synchronization; Shuttle controls larger schedules | Abstract actions only | Single-thread async simulation | Black-box only | Race detects observed Go data races; neither race nor `synctest` enumerates schedules |
| Deterministic async/task scheduling | Ordinary Tokio/Go tasks and targeted fakes | Shuttle seed/PCT/DFS/replay; Loom smaller | Abstract protocol steps | Deterministic Tokio host/runtime and virtual time | Real client/process schedules | Actual Go goroutines with fake time/quiescence; explicit barriers required for exact cuts |
| Pause/restart/fault cuts | Named restore phases and selected panic/stall hooks | Controlled Rust task choices and data nondeterminism | Explicit crash/recover actions | Barriers, partition, crash/bounce, unstable FS | Nemesis break/repair/final recovery | Context/channel/timer/barrier cuts only; no external process or real-network control |
| Safety/liveness state model | State machines exist in code/issues, no shared checker | Assertions in executable model | Always/eventually/sometimes with paths | Test assertions over simulation | Checker over finite external history | Implementation assertions only; shared vectors can bind them to a model contract |
| Linearizability/serializability | Idempotency and monotonicity tests, no general checker | Custom oracle required | Built-in bounded linearizability/sequential consistency | Requires custom history oracle | Knossos-backed checkers | Emits canonical Go histories for the same bounded checker; not a checker itself |
| History and shrinking | Logs/evidence are domain-specific | Encoded Rust schedule/path | Counterexample path; BFS shortest path | Seeded trace and controlled steps | Invoke/ok/fail/info ecosystem | Shared versioned vectors/traces; no built-in schedule shrinking |
| Network/storage realism | Real integration tests and runtime labs | Uncontrolled I/O excluded | Abstract network/storage | Simulated TCP/UDP/time and unstable crash FS | Real hosts/processes/network faults | Fake in-memory I/O only; real socket/syscall waits are not durably blocked |
| 1:n and authority fit | One source of truth by design | Tests adapters, no new authority | Models IDs/terms/refs without copying data | Simulated hosts refer to authority IDs | Histories reference operations/outcomes | Test vectors reference operation/generation IDs; implementation remains authority |
| Security | Production boundaries vary | Test-only; fake effects | Non-serving CI API | In-process fake hosts/paths | High-privilege lab operator | Standard library/race tool, test-only barriers, no external I/O |
| Maintenance/dependency | Loom already optional | Existing Loom plus proposed Shuttle dev dependency | New Rust dev dependency | New Rust dev dependency, unstable features isolated | No dependency; contract port only | Existing pinned Go toolchain, no dependency |

### Dependency, resource, and maintenance matrix

| Candidate | Production binary impact | CI resource hypothesis | Upgrade risk | Required boundary |
|---|---|---|---|---|
| Loom | None; optional dev feature | Exponential in threads/branches; use tiny exhaustive models | Memory-model semantics and cfg shims | Small pure concurrency kernel, no I/O |
| Shuttle | None; test-only | Iteration/depth budget; parallel portfolio optional | Scheduler serialization and wrapper API | Task/channel/random ports, fake effects |
| Stateright | None; test-only | State count, depth, memory, and property budget | Model/hash/symmetry semantics | Pure canonical state/action adapter |
| Turmoil | None; test-only | Virtual steps/hosts/messages and optional FS state | Unstable FS/barrier APIs | Transport/time/FS/barrier traits; no production type leak |
| Jepsen contracts | None | Real cluster tests are owner-target-specific | History schema and checker semantics | Sentinel lab driver emits canonical history |
| Go `synctest`/race | None; standard toolchain tests only | Bubble count, virtual steps, goroutines, and race-observed executions | Toolchain semantics; no third-party API | Test-only clock, channels, contexts, deterministic barriers, and shared vectors |
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
| D11 | Go Gateway virtual time, quiescence, and observed race detection | **Configure existing toolchain**: `testing/synctest`, `go test -race`, and test-only deterministic barriers | Executes the actual Go implementation without a new dependency and makes timeout/quiescence cuts reproducible. | Reject Rust-only proof claims; reject calling `synctest` exhaustive; reject wall-clock sleeps or production hook exposure. |

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
| S5 Forward queue | In Go `synctest`, barrier before/after waiter selection and before grant-channel close; race acquire/cancel/grant/release/resize across N waiters. | Active never exceeds limit; no permit leak; surviving waiters preserve FIFO; canceled waiter either loses before grant or receives and returns exactly one grant. | Proposed A2 Go child; `M0_HARDENING` |
| S6 Room sequencing | In Go `synctest`, barriers before completion publish/channel close and immediately before virtual timeout; race P1 complete, duplicate complete, P3 waits, hot disable, and cleanup. | Each P3 has one terminal context/no-context outcome; no close/send panic; timeout cannot read a later content write; no stale response attaches to another request. | Proposed A2 Go child; `M0_HARDENING` |
| S7 Runtime cancellation | Reserve invocation, enqueue, launch, cancel, child exits, receipt persists, reply drops, daemon restarts, reconcile. | One durable invocation/effect outcome; unknown outcome blocks automatic repeat; no orphan gains authority. | #710 plus Workbench/runtime owners; `M0_HARDENING` |
| S8 Restore | Admission close, local RecoveryPoint phase, fsync/rename, process death, staged restore, projection rebuild, runtime reconcile, Release/ACK/readiness CAS. | One committed generation or durable blocked/quarantine/manual state; never mixed writable stores or admission before all matching generations. | #751, capture/seal #753, restore #755, engine semantics #729; `M0_HARDENING` |
| S9 CAS resolve/GC | Parallel miss, holder failure, pull, verify, file sync, rename, dir sync, advertise, pin, GC, restart. | At most one transfer per key per node; no false content; referenced/pinned bytes survive; incomplete publication reconciles. | #729/#556; `POST_M0` unless single-node artifact path |
| S10 Migration | Every #501 saga step, lost reply, coordinator/source/target restart, partition, duplicate move, stale generation, route flip. | Exactly one owner/routable target; resume, permitted rollback, or manual recovery according to durable commit point. | #501; `POST_M0` |
| S11 Replica promotion | Input/tick/side-effect claim, follower lag, quorum loss, fence proof, promotion, old-owner message, route publish. | No stale writer or duplicate side effect; promotion requires complete RecoveryPoint and authority proof. | #653/#556; `POST_M0` |
| S12 Storage durability | Write, sync data, sync file, rename, sync directory, commit marker, truncate/fail sync, kill, reopen/integrity/reconcile. | Engine-specific semantic state matches the last declared durable boundary; unknown/corrupt state blocks readiness. | #729; `M0_HARDENING` |
| S13 Customer work effect | Every #710 cut from acceptance through delivery, including effect-before-receipt, cancellation, approval, version change, and missing CAS. | Resumed, idempotently replayed, compensated, durably blocked, quarantined, or manual recovery; never silent abandonment or blind repeat. | #710 and M0 owners; `M0_HARDENING` |
| S14 Real cluster history | Generate concurrent owner/CAS/migration operations, partition/repair/restart/revoke, then final authoritative reads. | History linearizable to declared spec or produces a minimized counterexample; all nemesis changes repaired. | #556; `POST_M0` |

### State-space and CI policy

| Tier | Trigger | Bound and evidence | Failure handling |
|---|---|---|---|
| T0 exact | Every relevant PR | Small Loom and owner-local Stateright models with committed thread/state/depth bounds; Go `synctest` virtual-time/quiescence fixtures plus `go test -race`; exact path/vector on failure | Any counterexample, race, deadlock, or shared-vector mismatch fails CI. `synctest` PASS is not exhaustive; model bound exhaustion remains distinct. |
| T1 seeded | Every relevant PR | Fixed Shuttle/Turmoil owner regression seeds plus deterministic Go barrier vectors; schedule artifact retained | New failure seed/vector becomes a permanent regression. |
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
| Gateway grant/cancel and room complete/timeout have no portable schedule artifact | `M0_HARDENING` | Current Go implementation and `-race` coverage are concrete but not schedule proof; proposed A2 adds `synctest`, barriers, and shared vectors without claiming exhaustive exploration. |
| Rust protocol models cannot prove Go implementation behavior | `M0_HARDENING` proof-boundary correction | A0 owns shared versioned vectors; A2 must reproduce them in actual Go code. Without that link, Rust results remain specification evidence only. |
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

### Contract A: ordered deterministic-concurrency evidence epic

**Proposed parent:** #659. **Classification:** `M0_HARDENING`.

**Ordered scope and collision boundary**

```text
A0 shared test schemas, validators, vectors, and CI routing
A0 -> A1 Rust implementation schedules (Loom and Shuttle only)
A0 -> A2 Go implementation schedules (synctest, race, barriers)
A1 || A2 (parallel after A0)
```

A1 and A2 may proceed in parallel only after A0 is merged. Stateright and
Turmoil domain models do **not** belong to this epic: they route to
#501/#556/#653/#710/#729/#751 and their children. This keeps common schemas,
Rust primitives, Go Gateway code, storage, recovery, and cluster models in
disjoint write scopes.

**Epic dependencies and ACs.** #705 gates A1's Shuttle dependency; #656 owns all
pins/upgrades; #710 owns terminal effect outcomes. The epic completes only when
A0-A2 are merged, every shared vector runs in its declared language lane, the
required CI contexts are deterministic, and every domain schedule has a
reciprocal live owner. A Rust model without a matching versioned Go trace cannot
satisfy a Go implementation AC.

**Epic negatives.** No production scheduler, runtime authority, cross-child
source ownership, real provider/VM, copied upstream code, or tool-specific
business schema. No PASS from wall-clock sleeps, exhausted bounds, or a model
not linked to implementation evidence.

**Target/benchmarks.** Runtime target `NONE`. Structural counts only:
permutations, states, steps, goroutines, virtual-time advances, race executions,
history operations, counterexample length, and bound exhaustion. Rust commands
use `cargo remote -c --`; Go uses the repository CI toolchain. No build-host
duration is product evidence.

**Rollout/rollback and TOGAF.** Merge A0, then A1/A2, then enable required CI
after replay stability. Roll back the affected child and CI route without
changing production behavior; retain regression vectors. The eventual TOGAF
delta names the layered evidence taxonomy and language-specific proof boundary,
never a simulator as runtime architecture.

#### Child A0: shared schemas, validators, vectors, and CI routing

**Scope and dependencies.** Own only versioned `FailureScheduleV1`,
`OperationHistoryV1`, public-safe golden vectors, fail-closed validators, and
path-aware T0/T1 CI routing. Depend on #710's terminal taxonomy and #656's
versioning policy. A1 and A2 depend on A0; A0 does not depend on either language
implementation.

**Acceptance criteria**

1. Both schemas validate version, owner issue, source/tool pin, bounds, ordered
   actions, authority generation, outcome taxonomy, and expected terminal state.
2. Unknown, missing, duplicate, conflicting, stale-generation, reordered, or
   digest-mismatched records fail with typed diagnostics.
3. At least one shared grant/cancel vector and one complete/timeout vector have
   identical canonical digests in Rust and Go validator fixtures.
4. CI path routing runs schema/vector checks whenever schema, validator, Rust
   adapter, Go adapter, or relevant workflow files change and feeds the sole
   aggregate required context.
5. Evidence is deterministic and public-safe: no payload, prompt, credential,
   host, user, absolute path, or customer data.

**Negative criteria.** No runtime authority, model-checker type, production
dependency, generated private fixture, implicit schema default, or language-only
field. A vector cannot claim implementation coverage until its language adapter
emits the matching history.

**Target tests and benchmarks.** Runtime target `NONE`; schema golden,
round-trip, mutation, version-skew, digest-conflict, routing-positive, and
routing-negative tests. Report vector/field/mutation counts only.

**Rollout/rollback.** Land schemas and negative fixtures with CI advisory, prove
both validator lanes, then make routing required. Rollback disables the route
and reverts the schema version as one unit; retain rejected fixtures and never
rewrite published vectors.

**TOGAF delta.** Define the test-only schedule/history envelope, 1:n references,
public-safety boundary, and language-conformance link after implementation
evidence; do not add a runtime data plane.

#### Child A1: Rust Loom and Shuttle implementation schedules

**Scope and dependencies.** Own small Rust concurrency kernels, the existing
Loom lane, test-only Shuttle adapters, S1/S2 and Rust portions of S3/S4/S7/S13,
and exact replay evidence. Depend on A0, #705 before Shuttle manifest mutation,
#656 for pins, and #710 for effect outcomes. Stateright/Turmoil domain models
remain in their existing owner issues.

**Acceptance criteria**

1. Loom exhaustively completes declared owner/readiness/single-flight bounds and
   reports permutations; a deliberately broken publication ordering fails.
2. Shuttle replays a deliberately injected channel/cancellation failure from the
   exact encoded schedule after a fresh process start.
3. Every production task/channel under test is behind a minimal adapter whose
   production behavior is unchanged when test cfg/features are absent.
4. Rust histories conform byte-for-byte to A0 golden digests and preserve
   `info`/blocked/quarantined/manual outcomes.
5. Always-run T0 and fixed T1 seeds are bounded and non-flaky; exhaustion is a
   typed non-pass result.

**Negative criteria.** No Go claim, Stateright/Turmoil domain ownership,
production binary dependency, uncontrolled I/O, real effect, wall-clock sleep
oracle, swallowed task panic, or success inferred from task completion.

**Target tests and benchmarks.** Runtime target `NONE`; all Rust commands only
through `cargo remote -c --`. Run Loom feature tests, Shuttle exact replay,
schema conformance, deliberately broken negatives, and relevant remote
check/test/clippy gates. Report permutations, branches, steps, seeds, history
rows, and exhaustion; no duration claim.

**Rollout/rollback.** Enable existing Loom coverage first, add dependency-approved
Shuttle adapters, retain every discovered seed, then require T0/T1. Rollback
test-only cfg/adapters and CI routing together; never weaken production
synchronization or delete a regression schedule.

**TOGAF delta.** Record bounded Rust weak-memory versus scheduled-task evidence
and explicit limits; neither result becomes business/runtime authority.

#### Child A2: Go Gateway synctest, race, and barrier schedules

**Scope and dependencies.** Own only Gateway test hooks/barriers and tests for
S5/S6 plus tick-buffer enable/disable and shutdown. Use the existing Go 1.26.5
standard library and existing `go test -race`; depend on A0 vectors. No new
dependency and no Rust source ownership.

**Acceptance criteria**

1. `testing/synctest` tests the actual `forwardqueue.Manager`,
   `sequencing.Sequencer`, and `ticksync.Buffer` with fake time/quiescence; every
   test also passes under `go test -race -count=1`.
2. Test-only barriers stop immediately before and after waiter selection, grant
   publication, P1 content publication/channel close, virtual timeout, pending
   tick extraction, and loop generation replacement.
3. Grant-before-cancel and cancel-before-grant vectors each produce one typed
   outcome; active never exceeds the current limit, no permit leaks, and FIFO is
   preserved among surviving waiters.
4. Complete-before-timeout and timeout-before-complete vectors each produce one
   terminal result; duplicate completion cannot panic or attach stale content.
5. Disable/enable while an old flush loop is paused cannot double-write, strand,
   or reorder an entry; shutdown leaves zero unclassified pending entries.
6. Go emits A0 `OperationHistoryV1` digests matching the shared vectors. Any
   Stateright result is only specification evidence until this AC passes.
7. A deliberately removed lock, omitted grant flag, reordered content write, or
   disabled barrier produces a race, invariant failure, vector mismatch, or
   deadlock instead of a false PASS.

**Negative criteria.** Never call `synctest` exhaustive. No wall-clock sleeps,
real sockets/processes/providers, production-visible hook endpoint, build-tag
that ships test barriers, ignored race report, Rust-only proof substitution, or
completion inferred from goroutine exit.

**Target tests and benchmarks.** Runtime target `NONE`. Run focused Go package
tests under `go test -race -count=1`, full Gateway test/race CI, barrier/vector
mutation negatives, and static/lint gates. Report goroutine/vector/barrier,
virtual-time advance, and race-run counts only; no runtime or build timing.

**Rollout/rollback.** Add package-private test controls, land deterministic
vectors, run focused and full race lanes, then make path-aware CI required.
Rollback tests/hooks and routing together while retaining failing vectors; no
production behavior or toolchain dependency changes.

**TOGAF delta.** Record Go fake-time/quiescence plus observed race evidence,
test-only barrier policy, and the shared-vector boundary to Rust models. State
explicitly that neither mechanism enumerates all Go schedules.

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

### Existing-owner delta E: #751 recovery, with #753 and #755

- #751 retains ordered recovery-epic authority; closed research #722 receives no
  implementation work.
- #753 owns a Stateright model for Prepare, bounded Drain, immutable seal,
  Release/Abort, matching participant ACKs, and readiness CAS, plus Turmoil
  barriers around local file/directory durability where #729 supplies the
  engine-specific oracle.
- #755 owns staged restore, process restart at every transition, projection
  rebuild, runtime reconciliation, validation-only probes, unknown effects,
  Release/ACK/readiness CAS, and final restore history. #729 owns each
  redb/SQLite/CAS recovery semantic; #755 composes their receipts.
- Dependencies remain the ordered #751 graph. Test-only Stateright/Turmoil
  additions still require #705 and #656; this delta creates no reverse edge to
  the A epic.
- Negative models remove or corrupt one participant receipt, generation, seal,
  Release decision, ACK, readiness CAS, directory sync, or effect outcome and
  must reach blocked/quarantined/manual state, never writable mixed state.
- Runtime/destructive tests and benchmarks remain exactly #753/#755's declared
  target contracts. Model/simulator runs report states, steps, faults, and path
  length only; they do not prove independent durability or restore performance.
- Rollback removes test adapters without weakening recovery fencing, immutable
  evidence, or engine recovery. Preserve counterexamples and failpoint vectors.
- TOGAF delta links the recovery protocol model and implementation histories to
  their owner receipts; a simulator never becomes recovery authority.

### Existing-owner delta F: #710 durable execution

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
- TOGAF delta links schedule/history evidence to existing durable execution
  receipts and terminal outcomes without creating a second workflow/effect
  authority.

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
| AC-1 Sentinel baseline | PASS | Current source/test map; reproducible inventory of all 49 Rust and 15 Go productive task starts; load-bearing lock/channel/store cuts; TOGAF targets; and live owner readback. |
| AC-2 landscape | PASS | Eight candidates, ten-factor rubric, scores, shortlist, and rejection reasons. |
| AC-3 pinned deep reviews | PASS | Five shortlisted systems plus the required pinned Go 1.26.5 language-lane review cover implementation, tests, failures/limits, security/license, and operations. |
| AC-4 mechanism matrix | PASS | Mechanism and dependency/resource matrices cover correctness, failures, determinism, 1:n, security, maintenance, dependency, and boundary. |
| AC-5 one decision per mechanism | PENDING ORC APPROVAL | D1-D11 each has one decision and rejected alternatives; the Go proof boundary and absence of upstream timing claims are explicit. |
| AC-6 implementation owners | PENDING ORC APPROVAL | Collision-safe epic A with complete A0/A1/A2 children and deltas B-F exists, but no GitHub mutation is permitted before approval. |
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
go1.26.5   c19862e5f8415b4f24b189d065ed739517c548ba
```

### Focused final checks

The final delivery must record exact outputs for:

```text
python3 <private-work-root>/verify/check_pinned_sources.py
python3 <private-work-root>/verify/check_structure.py
python3 <private-work-root>/verify/check_task_inventory.py
python3 <private-work-root>/verify/check_links.py
python3 <private-work-root>/verify/check_gfm.py
python3 <private-work-root>/verify/check_public.py
python3 <private-work-root>/verify/run_negative_tests.py
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
- a Rust claim about Go without a matching shared vector and Go implementation
  history, an unclassified productive task start, or a missing cancellation,
  join/error, durable-cut, or owner field;
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
- Go `testing/synctest` neither enumerates schedules nor controls real
  network/system-call blocking; `go test -race` reports only races observed in
  executed tests. Shared vectors bind specification and implementation evidence
  but do not turn one into the other.
- No maintainer decision, owner acknowledgement, dependency approval, TOGAF
  mutation, or implementation issue is implied by REVIEW_READY.
