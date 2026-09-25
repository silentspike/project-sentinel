# Durable Execution and Workflow Recovery Study

- Issue: [#710](https://github.com/silentspike/project-sentinel/issues/710)
- Parent research program: [#659](https://github.com/silentspike/project-sentinel/issues/659)
- Snapshot date: 2026-07-22
- Runtime target: `NONE`

## Executive Decision

Project Sentinel should not add Restate, Temporal, DBOS, Dapr Workflow, or
another durable-execution server to the production architecture at this time.
Each candidate contains useful mechanisms, but every complete engine would add
a second scheduler, journal, persistence plane, deployment surface, and upgrade
contract beside Sentinel's canonical EventStore, deterministic Bevy world,
Workbench, CAS, and Time Machine.

The target is a **minimal Sentinel-native durable-execution kernel**. It keeps
one authoritative event truth and ports five narrow contracts from the reviewed
systems:

1. A stored command or outcome is acknowledged before replay can continue.
2. A persisted activity result is replayed instead of re-executed.
3. A write timeout after a possible effect becomes `UnknownOutcome`, never a
   blind retry.
4. Durable timers, signals, and workflow-version markers are generation-fenced.
5. Recovery is admitted only from one committed, publish-last application cut.

The kernel composes existing owners rather than creating a new platform:

- [#709](https://github.com/silentspike/project-sentinel/issues/709) and
  [#731](https://github.com/silentspike/project-sentinel/issues/731) own the
  canonical event envelope, append gateway, outbox, outcomes, projections, and
  `EventTruthGeneration`.
- [#695](https://github.com/silentspike/project-sentinel/issues/695) owns the
  company workflow and Work DAG.
- [#694](https://github.com/silentspike/project-sentinel/issues/694) owns
  Workbench reservations, execution, probes, and immutable receipts.
- [#696](https://github.com/silentspike/project-sentinel/issues/696) owns
  independent QA, release, delivery, and acceptance lineage.
- [#722](https://github.com/silentspike/project-sentinel/issues/722) owns the
  cross-store cut and restore modes.
- [#708](https://github.com/silentspike/project-sentinel/issues/708) and
  [#726](https://github.com/silentspike/project-sentinel/issues/726) own the redb
  and CAS program; its child
  [#728](https://github.com/silentspike/project-sentinel/issues/728) owns storage
  generations, backup/restore activation, and schema compatibility.
- [#719](https://github.com/silentspike/project-sentinel/issues/719) owns the
  concurrency and crash-state model tests.
- [#650](https://github.com/silentspike/project-sentinel/issues/650) owns final
  single-node M0 acceptance.

This decision adds no dependency. Any later adoption or source port must pass
[#705](https://github.com/silentspike/project-sentinel/issues/705), and every
upgrade-sensitive contract remains governed by
[#656](https://github.com/silentspike/project-sentinel/issues/656).

## Claim Boundary

This is a source-level architecture study. It does not deploy a workflow engine,
mutate a runtime, or claim performance. Upstream tests and benchmarks identify
mechanisms and test shapes only. They are not Sentinel evidence.

The reviewed active PRs are implementation evidence, not merged architecture:

| Work | Reviewed head | Status at the cut |
|---|---|---|
| Workbench, PR [#704](https://github.com/silentspike/project-sentinel/pull/704) | `e30c8fd9608d4637973bcafb1cee4646e9e922fd` | Draft; merge state dirty |
| Company workflow, PR [#725](https://github.com/silentspike/project-sentinel/pull/725) | `8af76b0874d03bfdfe516bb8cd3040389a177309` | Draft; behind main |

Findings against these heads are required contract refinements. They are not a
claim that unmerged code is present on `main`.

## Reproduction Method

The landscape and deep review are reproducible from public GitHub data:

```bash
gh api repos/OWNER/REPO --jq \
  '[.full_name,.default_branch,.license.spdx_id,.archived,.language,.pushed_at]'
gh api repos/OWNER/REPO/commits/BRANCH --jq .sha
gh api repos/OWNER/REPO/releases/latest --jq .tag_name
gh api 'repos/OWNER/REPO/git/trees/SHA?recursive=1' --jq '.tree[].path'
gh api 'repos/OWNER/REPO/contents/PATH?ref=SHA' --jq .content | base64 -d
```

Selection used six equally weighted dimensions scored from 0 to 2:

- durable correctness contract;
- failure-injection and recovery evidence;
- fit with Sentinel's event/ECS/Workbench architecture;
- operational and resource fit for the 1:n principle;
- license and repository security-policy clarity;
- integration and future-upgrade cost, where 2 means low cost.

Scores select complementary mechanism families, not a vendor winner. A high
score cannot override an authority conflict or a second-control-plane cost.

## Sentinel Baseline

### Existing Product Contract

The merged M0 contract already defines the correct business boundary:

- accepted work moves through agreement, project creation, assignment,
  Workbench execution, independent QA, release, delivery, acceptance, and
  closeout;
- room chat and agent presence are not authoritative work state;
- replay of a matching completed Workbench request returns its durable outcome;
- tool-bearing work is admitted only through the secured Workbench;
- external effects follow reserve, persist intent, execute or probe, persist
  receipt, and reconcile workflow state.

See [Virtual Company Work Execution Contract](../../virtual-company-work-execution.md).

### Current World Snapshot Boundary

The merged `WorldSnapshot` captures schema, tick, simulation hour, timestamp,
tier, EventStore cursor, redb dump, ECS snapshot, projection offsets, and
filesystem metadata
([source](../../../crates/sentinel-common/src/types.rs#L641-L654)). It does not
capture workflow operations, execution-outbox state, Workbench invocation
frontiers, terminal or unknown receipts, authority/profile generations, or the
external-fact frontier.

Snapshot creation reads redb, ECS, filesystem metadata, projection offsets, and
the latest event ID in sequence before serializing the snapshot and pinning CAS
references
([source](../../../services/sentinel-daemon/src/snapshot.rs#L131-L220)). There
is no shared admission close, in-flight effect drain, or publish-last manifest
across those stores in the current function.

World restore stages stores sequentially and has failure points plus rollback,
which is valuable local protection. It restores redb, filesystem state, ECS,
and projections before advancing restore-generation metadata
([source](../../../services/sentinel-daemon/src/orchestrator.rs#L3945-L4167)).
The transfer path uses an in-memory restore fence and validates CAS and
projection inputs
([source](../../../services/sentinel-daemon/src/orchestrator.rs#L4171-L4355)).
It still cannot prove that workflow, Workbench, queue, and external-effect
truth describe the same moment.

Within redb, `restore_all_tables` correctly restores all 12 tables in one redb
write transaction
([source](../../../crates/sentinel-redb/src/lib.rs#L777-L796)). That local
atomicity must remain, but it is not cross-store atomicity.

The EventStore keeps projection offsets, the latest event row, restore
generation, and dead ranges in SQLite, with several independently committed
operations
([source](../../../crates/sentinel-limbo/src/event_store.rs#L1579-L1673)).
[#731](https://github.com/silentspike/project-sentinel/issues/731) is therefore
the required event-generation owner for the future cut.

### Active Workflow and Workbench Evidence

PR #725 introduces a separate SQLite workflow store with entities, operations,
workflow events, entity history, project projections, an execution outbox, and
a projection checkpoint
([source](https://github.com/silentspike/project-sentinel/blob/8af76b0874d03bfdfe516bb8cd3040389a177309/crates/sentinel-workflow/src/store.rs#L18-L96)).
It uses WAL plus `synchronous=FULL`, and a local transaction can bind command
idempotency, entity changes, events, outbox rows, and projections
([source](https://github.com/silentspike/project-sentinel/blob/8af76b0874d03bfdfe516bb8cd3040389a177309/crates/sentinel-workflow/src/store.rs#L216-L367)).
It also has verified standalone SQLite backup/restore
([source](https://github.com/silentspike/project-sentinel/blob/8af76b0874d03bfdfe516bb8cd3040389a177309/crates/sentinel-workflow/src/store.rs#L473-L559)).

Those are useful local contracts, but two integration gaps remain:

1. `workflow_events` and `workflow_execution_outbox` cannot become a second
   authoritative event/outbox plane beside the canonical #731 append gateway.
2. Dispatch reserves execution through an external port and then records the
   receipt and state in a later workflow-store transaction
   ([source](https://github.com/silentspike/project-sentinel/blob/8af76b0874d03bfdfe516bb8cd3040389a177309/crates/sentinel-workflow/src/engine.rs#L175-L264)).
   A crash after reservation but before the local commit needs a typed outcome
   probe or `UnknownOutcome`; reserve idempotency alone is insufficient.

PR #704 introduces digest-bound Workbench records with `Reserved`, `Executing`,
and terminal states, plus recovery actions and immutable completion receipts
([source](https://github.com/silentspike/project-sentinel/blob/e30c8fd9608d4637973bcafb1cee4646e9e922fd/services/sentinel-daemon/src/workbench.rs#L233-L367)).
It records `Executing` before runtime exchange, rechecks authority before and
after execution, and preserves ambiguous transport failures for recovery
([source](https://github.com/silentspike/project-sentinel/blob/e30c8fd9608d4637973bcafb1cee4646e9e922fd/services/sentinel-daemon/src/workbench.rs#L915-L1110)).
These receipts are the correct external-effect recovery source, but neither
`WorldSnapshot` nor the current restore path includes their frontier or root.

### Existing Incident and Regression Evidence

| Evidence | Durable-execution lesson | Reused contract |
|---|---|---|
| Issue #395 provider result versus usage-append failure injection | A provider effect can complete before the local business/event record; retaining only retry intent can duplicate a paid call | Stable request reservation, retained result, bounded outcome resolution |
| Issues #491/#493 Time Machine replay and dead-future handling | Rewound local history must not leak discarded events back into reads, outbox publication, or replay | Restore generation, dead-range exclusion, mode-specific replay |
| Issue #492 snapshot CAS pinning | A logically valid snapshot becomes unreadable when referenced immutable bytes are collected | Recovery cuts own explicit CAS manifests and durable pins |
| Current restore rollback tests | Per-store failure injection and a closed fence prevent some mixed local states | Preserve local rollback, add a cross-store committed-manifest boundary |
| PR #704 restart recovery tests | Executing state plus immutable runtime receipt can survive a process loss without blind execution | Standardize probe, unknown outcome, and receipt replay across all effects |
| PR #725 standalone backup tests | A consistent workflow SQLite image is useful but does not synchronize other product planes | Include the image/frontier only as one member of `DurableExecutionCut` |

These rows are architecture evidence, not a claim that every active PR or
historical issue already satisfies the final contract.

### State-Plane Callsite and Persistence Map

| Plane | Current write/read entry | Transaction or publication boundary | Recovery implication |
|---|---|---|---|
| Limbo events | `EventStore::append_event` ([source](../../../crates/sentinel-limbo/src/event_store.rs#L484-L518)) | One fenced SQLite transaction; `operation_id` is unique | Event row alone does not prove an effect or delivery |
| Limbo event plus outbox | `append_with_outbox` and batch variant ([source](../../../crates/sentinel-limbo/src/event_store.rs#L993-L1058)) | Event and local outbox intent commit in one fenced SQLite transaction | This is the base #731 must generalize for workflow event plus outcome/dispatch intent |
| Limbo outbox delivery | `poll_outbox` then `mark_published` ([source](../../../crates/sentinel-limbo/src/event_store.rs#L1517-L1577)) | Poll/read and published marker are separate transactions | Redelivery is expected; a socket send is not a business outcome |
| LLM request | `reserve_llm_request` starts at [event_store.rs](../../../crates/sentinel-limbo/src/event_store.rs#L520) | Stable request reservation precedes provider execution | Existing narrow pattern should converge on the common external-effect state machine |
| redb world/domain | `dump_all_tables` and `restore_all_tables` ([source](../../../crates/sentinel-redb/src/lib.rs#L760-L796)) | Each read or restore is internally consistent within one redb transaction | Cut records exact generation/digest; no cross-store claim |
| Bevy ECS | `SnapshotManager::create_and_store` ([source](../../../services/sentinel-daemon/src/snapshot.rs#L131-L220)) | ECS capture is one step in a sequential snapshot function | Tick barrier and schedule/schema fingerprint must surround the global cut |
| CAS ingest | `commit_ingest` ([source](../../../crates/sentinel-fs/src/ingest.rs#L98-L195)) | Segment bytes append before an atomic redb index/manifest transaction; orphan bytes are reclaimable | Cut references only committed manifest roots and pins them before validity |
| Sandbox home manifest | `HomeManifest` and `release_manifest` ([source](../../../crates/sentinel-fs/src/home_manifest.rs#L153-L178)) | Deterministic hash references; local object IDs own refcounts | Recovery manifest stores portable roots, while local pin ownership is rebuilt |
| NATS bridge | `pollLoop` ([source](../../../services/sentinel-nats-bridge/main.go#L177-L247)) | Current bridge uses core `PublishMsg`, then marks a batch published | #731 must require JetStream `PubAck` and durable consumer outcomes; current send is insufficient |
| Projection service | `ProjectionWorker` construction/rebuild/run ([source](../../../services/sentinel-projection/src/main.rs#L62-L103)) | Separate projection database and poll loop | Projection is derived, rebuilt into a generation, and never authorizes execution |
| Workflow draft | PR #725 `WorkflowStore::execute` ([source](https://github.com/silentspike/project-sentinel/blob/8af76b0874d03bfdfe516bb8cd3040389a177309/crates/sentinel-workflow/src/store.rs#L277-L367)) | One workflow SQLite transaction can bind local entity/event/outbox/projection changes | Must use or derive from #731 event authority and expose cut frontiers |
| Workbench draft | PR #704 coordinator ([source](https://github.com/silentspike/project-sentinel/blob/e30c8fd9608d4637973bcafb1cee4646e9e922fd/services/sentinel-daemon/src/workbench.rs#L915-L1110)) | Durable local CAS transitions around a separate runtime exchange | Executing ambiguity is resolved by receipt/probe, not blind execution |
| Runtime credentials/policy | Daemon composition root reads required credentials; active work binds profile and authority generations | Secret source is external to snapshot stores | Cut stores reference/generation only; restore rebinds current valid secret |
| Runtime handles and caches | Daemon maps, process handles, routes, owner caches, projection caches | Process-local and intentionally lossy | Startup rebuilds from durable owners and opens admission last |

## State Ownership Matrix

Every row has one authority and one recovery source. A projection, cache, or
transport acknowledgment never becomes authority.

| Datum | Class | Sole authority | Durability rule | Recovery and retention rule |
|---|---|---|---|---|
| Customer agreement, project, Work DAG, assignment, QA, release, delivery | Authoritative business state | Canonical EventStore aggregate stream through #731 append gateway | Event plus operation outcome and dispatch intent commit atomically | Replay aggregate from committed event generation; retain through product/audit policy |
| Command operation ID and response | Authoritative idempotency outcome | #731 append gateway outcome table | Same namespace and digest replays; conflict rejects | Restore with `EventTruthGeneration`; never regenerate from a projection |
| Workflow execution intent | Authoritative pending effect | Canonical dispatch outbox attached to workflow event | Intent commits with the state transition that requires it | Reconcile from outbox plus Workbench reservation/receipt |
| Workbench invocation | Authoritative mutable execution state | #694 Workbench store | Digest-bound CAS transitions; attempt and authority generations fenced | Restore from cut; probe `Executing` records before retry |
| Completion receipt | Immutable artifact/external-fact evidence | Agent runtime/Workbench receipt root, accepted by daemon | Publish content and fsync before terminal acceptance | CAS-pinned by work, release, audit, and recovery cut |
| Provider, email, Git, deploy, billing, delivery effect | External fact | Downstream system plus Sentinel receipt/probe record | At-least-once request with downstream idempotency where available | Probe; otherwise `UnknownOutcome` or manual recovery, never rewind |
| Event delivery | Transport state | JetStream stream and Sentinel durable inbox/outcome | `PubAck` proves stream acceptance only | Redeliver until durable consumer outcome; generation binds frontier |
| Projection rows | Derived projection | No authority | View mutation and local frontier commit together | Rebuild from canonical event generation; quarantine poison input |
| Episode and organizational memory | Derived durable projection with provenance | Canonical events plus memory projection contract | Source event and memory outcome frontier advance atomically | Rebuild only when source/provenance retained |
| redb agent/domain state | Authoritative mutable simulation/domain state | redb tables under fenced writer | Per-database transaction and owner generation | Restore selected redb generation/savepoint and validate digest |
| Bevy world | Deterministic runtime state | Active ECS world for current tick; not business authority | Tick barrier defines cut; side effects remain outside ECS | Snapshot plus deterministic input tail for Simulation Replay only |
| CAS bytes and manifests | Immutable artifact | CAS digest and manifest | Bytes fsync and digest verify before reference publication | Pin from live work, receipts, releases, and committed cuts; uncertainty blocks GC |
| Configuration and work/tool profiles | Versioned policy input | Repository/deployed checksummed profile catalog | Activation records exact digest and generation | Restore requires compatible version; no silent substitution |
| Credentials | External secret state | Credential provider/systemd credential store | Store only references and generations in cut | Rebind current valid credential; never copy secret into snapshot |
| Organization, assignment, policy, owner generations | Authority metadata | Respective canonical control record | Checked at reservation, runtime I/O, result acceptance, QA, release, delivery | Rebuild caches, then compare exact generations before admission |
| Timer, signal, approval wait | Authoritative workflow wait | Canonical workflow event stream | Token/generation/deadline and delivery ID persist before wait | Stale tokens ignored; matching event resumes once |
| Runtime handles, leases, in-memory queues | Transient cache/execution lease | No business authority | May accelerate only; loss is expected | Rebuild from durable intent, reservation, receipts, and runtime probe |
| Recovery cut | Recovery metadata | #722 committed `DurableExecutionCut` manifest | Prepared content fsynced; valid marker published last | Retain manifest, all roots, predecessor, and compatibility metadata |

## Landscape Inventory

All commits and releases below were read live on 2026-07-22. `No tree policy`
means no repository-local `SECURITY.md` was found at the pinned commit; it does
not assert that the organization has no private disclosure channel.

| Candidate | Pin | License/policy at pin | Score | Disposition |
|---|---|---|---:|---|
| Restate | [`a8d7ac4`](https://github.com/restatedev/restate/tree/a8d7ac49d4d8a941bd4e52a0a806d94d445cc778), v1.7.2 | BSL 1.1, Apache-2.0 after four years; no tree policy | 7/12 | Deep review: journal ACK and idempotent outcome contracts |
| Temporal | [`9559480`](https://github.com/temporalio/temporal/tree/955948007cc6d9d94fa8ef484225954bd9328451), v1.31.2 | MIT; no tree policy | 7/12 | Deep review: history state, possibly-succeeded writes, fault injection |
| DBOS Python | [`50234b2`](https://github.com/dbos-inc/dbos-transact-py/tree/50234b2220111a47ca1681cd789071328c2e0151), 2.28.0 | MIT; no tree policy | 9/12 | Deep review: minimal recorded-result and recovery fence |
| Durable Task Go + Dapr Workflow | [`9c9e2d6`](https://github.com/microsoft/durabletask-go/tree/9c9e2d6d4cc3609c28bc2cc660ab5311f0217593) + [`a934df1`](https://github.com/dapr/dapr/tree/a934df1dd333f16075d3849c464e25fb3d3414bc), Dapr v1.18.1 | Apache-2.0; both have `SECURITY.md` | 7/12 | Deep review: replay, timers, version stalls, chaos saves |
| Rivet Gasoline | [`9a852ca`](https://github.com/rivet-dev/rivet/tree/9a852ca75b1cfb8e1c59899b437730caef3a5a18), v2.3.5 | Apache-2.0; no tree policy | 7/12 | Deep review: Rust history/version type shapes and a cross-store warning |
| Cadence | [`6230d76`](https://github.com/cadence-workflow/cadence/tree/6230d76b7f2e88c0298f8c986e3db7237f75faea), v1.4.1 | Apache-2.0 | 6/12 | Reject deep review: mechanism overlap with Temporal, higher integration cost |
| Conductor OSS | [`54f8369`](https://github.com/conductor-oss/conductor/tree/54f8369fa8875a2bad4ed5baa8a66f89720b1594), v3.31.0 | Apache-2.0 | 5/12 | Reject: task/DAG model useful, Java platform duplicates Sentinel services |
| Inngest | [`3e51f5b`](https://github.com/inngest/inngest/tree/3e51f5bbbc442874ab7b0aa820dfdd9c7bbf9574), v1.38.1 | Non-standard source license at pin | 4/12 | Reject: license and hosted-platform fit do not beat alternatives |
| Hatchet | [`5558601`](https://github.com/hatchet-dev/hatchet/tree/55586012312c90ad7a3a6b4f14436e4365677898), v0.94.10 | MIT | 6/12 | Reject deep review: queue/DAG overlap; no unique mechanism gap |
| Trigger.dev | [`95307ba`](https://github.com/triggerdotdev/trigger.dev/tree/95307ba33c80226fa596bda3b003ab90afa6361b), v4.5.6 | Apache-2.0 | 5/12 | Reject: TypeScript worker platform and operational footprint |

The shortlist deliberately includes five distinct implementation styles. DBOS
scores highest on integration simplicity, while Temporal and Dapr are retained
despite lower integration scores because their failure semantics and chaos tests
cover risks that a minimal result table does not.

Score order is `(correctness, failure evidence, Sentinel fit, operations,
license/security clarity, integration cost)`. The totals above derive from:

| Candidate | Score vector | Replay and failure semantics | Effects and authority | Determinism and 1:n fit | Security, maintenance, dependency, and upgrade impact |
|---|---|---|---|---|---|
| Restate | `2,2,2,1,0,0` | Stored-command ACK frontier, durable journal/outcome replay, stale-timer rejection | Strong invocation and outbox ownership; would conflict with Sentinel event authority if adopted | Deterministic journal model is useful; replicated runtime is heavier than the native 1:n target | Active Rust project; BSL 1.1 and no tree policy at pin; large server/SDK upgrade surface |
| Temporal | `2,2,1,0,2,0` | Mature history replay, mutable-state checksums, explicit possibly-succeeded failure injection | Strong workflow/activity split and operator recovery; requires its own persistence/task authority | Deterministic workflow code but not Sentinel ECS determinism; high service and database footprint | Active MIT project; no tree policy at pin; very large Go control-plane and schema upgrade contract |
| DBOS | `2,1,2,1,2,1` | Compact operation-result replay and nondeterminism check; less broad chaos evidence | Clear function/result authority and executor fence; cross-database atomicity explicitly limited | Good minimal pattern, but Python/PostgreSQL runtime is not needed for Sentinel | Active MIT project; no tree policy at pin; moderate SDK/database coupling and polyglot upgrades |
| Durable Task + Dapr | `2,2,1,0,2,0` | History replay, deterministic actions/children, generation timers, save/cache chaos tests | Actor state and leased tasks provide strong ownership but duplicate Sentinel workflow and queue planes | Replay contracts fit; sidecar, actor store, reminders, and host increase resource multiplication | Active Apache-2.0 projects with tree policies; two repositories, APIs, sidecars, and state backends to upgrade |
| Rivet Gasoline | `1,1,2,0,2,1` | Typed history/version divergence; joined independent writes expose an atomicity risk | Activity/workflow split is useful, but full platform authority is unsuitable | Rust types fit directly; broader platform/database footprint does not fit 1:n | Active Apache-2.0 project; no tree policy at pin; narrower mechanism port is maintainable |
| Cadence | `2,1,1,0,2,0` | Mature replay model, but mechanism coverage overlaps the more current Temporal review | Full history/task-list authority conflicts with Sentinel | Deterministic workflow model, high operational footprint | Active Apache-2.0 project; separate large Go platform with little unique benefit after Temporal |
| Conductor OSS | `1,1,1,0,2,0` | Durable task/DAG state, but weaker unique replay/fault evidence for this study | Server owns workflows, tasks, queues, indexing, and operator state | DAG semantics fit company work; Java service stack does not fit low-overhead native kernel | Active Apache-2.0 project; substantial service, datastore, and JVM upgrade burden |
| Inngest | `1,1,1,1,0,0` | Event-driven function recovery is relevant, but no unique source mechanism beat the shortlist | Adds event/function execution authority | Developer model is convenient; hosted/self-hosted platform duplicates state and queues | Active non-standard source license at pin; licensing and platform upgrades are unacceptable here |
| Hatchet | `1,1,1,1,2,0` | Queue/DAG retry and worker mechanisms are credible but overlap selected references | Adds workflow, worker, queue, and control-plane authority | Some lightweight worker patterns; still duplicates existing planes | Active MIT project; new server/database/API upgrade surface without a unique correctness gain |
| Trigger.dev | `1,1,1,1,1,0` | Durable task execution is relevant, but TypeScript platform mechanisms overlap | Adds hosted/self-hosted orchestration authority | JavaScript worker ecosystem and service footprint do not improve deterministic ECS integration | Active Apache-2.0 project; broad Node/service/deployment upgrade contract |

The scoring is comparative, not a safety proof. The deep-review decision also
requires complementary mechanism coverage and source-level evidence.

## Deep Source Review

### Restate

**Mechanisms read**

- `JournalTracker` waits until proposed commands and notifications have storage
  acknowledgments before retry is safe
  ([source](https://github.com/restatedev/restate/blob/a8d7ac49d4d8a941bd4e52a0a806d94d445cc778/crates/invoker-impl/src/invocation_state_machine.rs#L72-L141)).
- Retry progress resets retry state, and timer keys reject stale timer firings
  ([source](https://github.com/restatedev/restate/blob/a8d7ac49d4d8a941bd4e52a0a806d94d445cc778/crates/invoker-impl/src/invocation_state_machine.rs#L373-L539)).
- A call command enqueues the service invocation into the state-machine outbox
  before exposing its completion
  ([source](https://github.com/restatedev/restate/blob/a8d7ac49d4d8a941bd4e52a0a806d94d445cc778/crates/worker/src/partition/state_machine/entries/call_commands.rs#L89-L152)).
- Idempotency tests prove that a completed invocation result is retained and
  returned rather than rerun
  ([test](https://github.com/restatedev/restate/blob/a8d7ac49d4d8a941bd4e52a0a806d94d445cc778/crates/worker/src/partition/state_machine/tests/idempotency.rs)).
- The repository has dedicated throughput benchmark crates
  ([harness](https://github.com/restatedev/restate/tree/a8d7ac49d4d8a941bd4e52a0a806d94d445cc778/benchmarks)).

**Sentinel use:** port the stored-command ACK frontier, stale-timer token, and
durable completed-result replay contracts. Do not copy source or adopt the
runtime. BSL licensing, a replicated partition/control plane, and a second
journal make adoption incompatible with the current dependency and 1:n goals.

### Temporal

**Mechanisms read**

- Mutable workflow state generates mutation or snapshot records carrying
  events, tasks, versions, conditions, and checksums
  ([source](https://github.com/temporalio/temporal/blob/955948007cc6d9d94fa8ef484225954bd9328451/service/history/workflow/mutable_state_impl.go#L7449-L7577)).
- Workflow completion validates scheduled/started identity and appends the
  result transition
  ([source](https://github.com/temporalio/temporal/blob/955948007cc6d9d94fa8ef484225954bd9328451/service/history/workflow/mutable_state_impl.go#L4467-L4510)).
- Persistence distinguishes an operation that may have succeeded
  ([source](https://github.com/temporalio/temporal/blob/955948007cc6d9d94fa8ef484225954bd9328451/common/persistence/error_type.go#L5-L21)).
- Fault injection explicitly executes a persistence write and then returns a
  timeout, modeling the dangerous effect-before-ACK window
  ([source](https://github.com/temporalio/temporal/blob/955948007cc6d9d94fa8ef484225954bd9328451/common/persistence/faultinjection/fault.go#L12-L71)).
- The generated persistence fault-injection package is an extensive reusable
  test shape
  ([tests](https://github.com/temporalio/temporal/tree/955948007cc6d9d94fa8ef484225954bd9328451/common/persistence/faultinjection)).

**Sentinel use:** port `OperationPossiblySucceeded` as the typed
`UnknownOutcome` state and mirror execute-then-timeout tests at every external
effect boundary. Retain history/checksum/version concepts. Reject adoption:
Temporal would duplicate history, matching, task queues, visibility, database
schema, deployment, and operational ownership.

### DBOS Python

**Mechanisms read**

- The system schema stores workflow status and one operation output per
  `(workflow_uuid, function_id)`
  ([source](https://github.com/dbos-inc/dbos-transact-py/blob/50234b2220111a47ca1681cd789071328c2e0151/dbos/_schemas/system_database.py#L30-L181)).
- Recorded output is loaded on replay; a changed function identity is treated
  as nondeterminism rather than silently executed
  ([source](https://github.com/dbos-inc/dbos-transact-py/blob/50234b2220111a47ca1681cd789071328c2e0151/dbos/_sys_db.py#L2626-L2705)).
- Recovery is fenced by executor identity
  ([source](https://github.com/dbos-inc/dbos-transact-py/blob/50234b2220111a47ca1681cd789071328c2e0151/dbos/_sys_db.py#L4043-L4075)).
- The implementation documents that caller transaction composition works only
  when application and system state share the same database
  ([source](https://github.com/dbos-inc/dbos-transact-py/blob/50234b2220111a47ca1681cd789071328c2e0151/dbos/_sys_db.py#L4333-L4357)).

**Sentinel use:** port the minimal operation-output table shape, function/tool
profile identity check, and executor-generation fence. The same-database caveat
directly confirms why Sentinel needs a publish-last cut rather than claiming a
transaction across SQLite, redb, ECS, CAS, and external systems. Reject the
Python/PostgreSQL/SQLAlchemy runtime as a production dependency.

### Durable Task Go and Dapr Workflow

**Mechanisms read**

- Durable Task reconstructs orchestration context from old and new history and
  emits deterministic actions
  ([source](https://github.com/microsoft/durabletask-go/blob/9c9e2d6d4cc3609c28bc2cc660ab5311f0217593/task/orchestrator.go#L23-L235)).
- Runtime state appends history while deriving pending timers, tasks, messages,
  and deterministic child IDs
  ([source](https://github.com/microsoft/durabletask-go/blob/9c9e2d6d4cc3609c28bc2cc660ab5311f0217593/backend/runtimestate.go#L101-L225)).
- Its SQLite backend separates instances, history, new events, and leased tasks
  ([schema](https://github.com/microsoft/durabletask-go/blob/9c9e2d6d4cc3609c28bc2cc660ab5311f0217593/backend/sqlite/schema.sql#L1-L55)).
- Dapr verifies persisted orchestration state, signs it, and saves multi-key
  changes transactionally while invalidating stale cache on conflict
  ([source](https://github.com/dapr/dapr/blob/a934df1dd333f16075d3849c464e25fb3d3414bc/pkg/actors/targets/workflow/orchestrator/state.go#L40-L244)).
- Generation-fenced reminders, stale-cache reload, retention, and termination
  retries are explicit
  ([source](https://github.com/dapr/dapr/blob/a934df1dd333f16075d3849c464e25fb3d3414bc/pkg/actors/targets/workflow/orchestrator/run.go#L40-L145)).
- Workflow-version mismatch becomes a durable stall instead of an unsafe replay
  ([source](https://github.com/dapr/dapr/blob/a934df1dd333f16075d3849c464e25fb3d3414bc/pkg/actors/targets/workflow/orchestrator/versioning.go#L31-L105)).
- Chaos tests cover save failure, cache invalidation, and ordering around inbox
  and reminders
  ([test](https://github.com/dapr/dapr/blob/a934df1dd333f16075d3849c464e25fb3d3414bc/tests/integration/suite/daprd/workflow/chaos/savefail.go#L42-L166)).

**Sentinel use:** port replay cursors, deterministic child/delegation IDs,
generation-fenced wait tokens, durable version stalls, cache invalidation, and
the chaos-test matrix. Reject Dapr sidecars and the Durable Task engine as a
production layer because they add actor state, queues, hosting, and a second
workflow authority.

### Rivet Gasoline

**Mechanisms read**

- The Rust workflow context binds name, history, cursor, version, state, and
  wake deadline
  ([source](https://github.com/rivet-dev/rivet/blob/9a852ca75b1cfb8e1c59899b437730caef3a5a18/engine/packages/gasoline/src/ctx/workflow.rs#L46-L123)).
- A version regression is a typed `HistoryDiverged` failure
  ([source](https://github.com/rivet-dev/rivet/blob/9a852ca75b1cfb8e1c59899b437730caef3a5a18/engine/packages/gasoline/src/ctx/workflow.rs#L130-L145)).
- History events are typed and versioned for activities, signals, child
  workflows, loops, sleep, branches, and version checks
  ([source](https://github.com/rivet-dev/rivet/blob/9a852ca75b1cfb8e1c59899b437730caef3a5a18/engine/packages/gasoline/src/history/event.rs#L10-L218)).
- Successful activity handling uses two asynchronously joined database writes
  for the event and mutable state
  ([source](https://github.com/rivet-dev/rivet/blob/9a852ca75b1cfb8e1c59899b437730caef3a5a18/engine/packages/gasoline/src/ctx/workflow.rs#L265-L337)).

**Sentinel use:** port the Rust type shapes for history coordinates, version
checks, and typed divergence. Treat the joined independent writes as a warning,
not a pattern: concurrency is not atomicity. Reject the larger Rivet platform
and database stack.

### Failure, Operations, and Benchmark Harness Inventory

| Candidate | Failure/recovery evidence reviewed | Operations or benchmark evidence reviewed | Study limitation |
|---|---|---|---|
| Restate | Partition-state-machine idempotency tests and invocation retry/ACK state | [`benchmarks/`](https://github.com/restatedev/restate/tree/a8d7ac49d4d8a941bd4e52a0a806d94d445cc778/benchmarks) parallel/sequential throughput harness | Upstream throughput is not a Sentinel resource claim |
| Temporal | Generated persistence fault injection, including execute-then-timeout | History-service persistence and test-server tooling | No upstream latency number is imported; only fault semantics are used |
| DBOS | System-database workflow/recovery tests and recorded-output code paths | No dedicated benchmark directory found at the pin | Python/PostgreSQL behavior does not prove SQLite/redb behavior |
| Durable Task + Dapr | Durable Task backend tests plus Dapr workflow chaos save/dedup/version tests | Dapr [`tests/runner/loadtest`](https://github.com/dapr/dapr/tree/a934df1dd333f16075d3849c464e25fb3d3414bc/tests/runner/loadtest) | Sidecar/store topology differs from Sentinel; test shapes only |
| Rivet Gasoline | Typed history, retry, sleep/signal/version code; no equally strong workflow chaos suite found | Only broader example benchmark scripts, not a workflow durability harness | The missing atomic commit between activity event and mutable state is a caution, not proof of failure in every backend |

Security-policy pins used in the review are
[`durabletask-go/SECURITY.md`](https://github.com/microsoft/durabletask-go/blob/9c9e2d6d4cc3609c28bc2cc660ab5311f0217593/SECURITY.md)
and
[`dapr/SECURITY.md`](https://github.com/dapr/dapr/blob/a934df1dd333f16075d3849c464e25fb3d3414bc/SECURITY.md).
The repository-tree observation for the other deep candidates is recorded in
the landscape table and must be refreshed before any future adoption decision.

## Mechanism Comparison and Decisions

| Mechanism | Best source reference | Sentinel today | Decision | Exact boundary |
|---|---|---|---|---|
| Journaled progress | Restate, Temporal | EventStore plus separate active workflow draft | `Reimplement minimal` | Typed workflow events in #731 canonical stream; no second journal authority |
| Result replay | Restate, DBOS | Workbench receipts in PR #704 | `Port algorithm/contract` | Lookup invocation + digest before runtime I/O; replay receipt |
| Possibly succeeded effect | Temporal | Some call paths retain executing state, but no universal type | `Port algorithm/contract` | `UnknownOutcome` plus required probe/manual resolution |
| Workflow/activity split | All deep candidates | #695 `WorkExecutionPort` and #694 Workbench | `Keep Sentinel` | Workflow decides business state; Workbench owns bounded effects |
| Timers/signals/approvals | Dapr/Durable Task, Restate | Bounded timers are contractual but not one durable wait model | `Reimplement minimal` | Canonical wait event with token, generation, deadline, source cursor |
| Attempt fencing | DBOS, Dapr | Invocation digest and authority checks exist in PRs | `Integrate` | Add executor/attempt generation to every claim, probe, and result accept |
| Code/version evolution | Dapr, Rivet, DBOS | Schema/profile versions exist, workflow replay marker incomplete | `Port algorithm/contract` | Durable patch/version marker; mismatch stalls and quarantines |
| Child workflow/delegation | Durable Task, Rivet | Work DAG and handoffs in #695 | `Keep Sentinel` | Stable child/work-item ID derived from parent operation and graph version |
| Cancellation/compensation | Temporal, Dapr | Individual state machines exist; cross-stage contract incomplete | `Reimplement minimal` | Persist cancellation intent, fence effects, await/probe outcome, then compensate |
| History compaction | Temporal/continue-as-new family | Event retention and snapshots are separate | `Integrate` | New workflow generation links prior root and retained cut; never delete required receipts |
| Application cut | No engine spans all Sentinel planes | WorldSnapshot is incomplete | `Reimplement minimal` | #722 `DurableExecutionCut` links #731 `EventTruthGeneration` and #728 `StorageGeneration` |
| External engine | Restate/Temporal/Dapr/DBOS/Rivet | No engine dependency | `Reject` | Reconsider only if measured correctness/operations beat native kernel through #705 |

### Performance Hypotheses and Required Future Measurements

No hypothesis below is a result. Each is verified only in the implementation
owner on its declared product target, never on the Rust build server.

| Change | Hypothesis | Correctness co-primary metric | Resource/performance metrics | Owner |
|---|---|---|---|---|
| Canonical workflow event/outcome/dispatch transaction | One transaction avoids extra synchronization and removes reconciliation between two event authorities | Zero lost accepted transitions and zero duplicate committed outcomes | durable append p50/p95/p99, fsyncs/transition, CPU-seconds/1k transitions | #731/#695 |
| Workbench result replay | Receipt lookup is cheaper and safer than relaunch | One effect per downstream idempotency contract | lookup latency, receipt bytes/invocation, recovery CPU/time | #694 |
| Durable waits | Event-driven resume removes tick polling and retry storms | One matching wake; stale tokens never advance | idle CPU/wait, wake latency, rows/open wait | #695 |
| `DurableExecutionCut` | Publish-last manifest adds bounded checkpoint cost while eliminating mixed recovery | Every injected cut/restore crash resolves to old or new committed cut | admission pause, drain time, fsync count, manifest bytes, cut/restore p95 | #722 |
| History version markers | Small storage growth prevents unsafe interpretation after upgrades | All incompatible replays durably block | bytes/transition, replay CPU/event, blocked-workflow recovery time | #695/#719 |
| Native kernel versus external engine | Avoided network/service/database hops should reduce idle and per-transition overhead | Semantics remain equivalent to accepted contracts | RSS/service, CPU-seconds/transition, disk bytes/transition, operational components | #705 if revisited |

### Operator and Observability Contract

The native kernel requires one correlated view keyed by customer request,
agreement, project, work item, workflow generation, operation, invocation,
receipt, artifact, release, delivery, and recovery cut. Required operator
queries and metrics are:

- current durable state and expected next transition;
- open waits, deadline, token generation, and authorized wake source;
- current attempt owner, lease, heartbeat, and authority generations;
- pending dispatch, executing, unknown, probing, quarantined, and manual states;
- receipt/probe provenance without private payload disclosure;
- oldest unknown outcome and oldest blocked accepted work;
- replay count versus effect-execution count;
- cut preparation phase, active committed cut, predecessor, and validation;
- projection/consumer lag against the cut frontiers;
- authenticated probe, re-arm, compensate, cancel, quarantine, and release
  actions with before/after state and reason.

Search and dashboards are derived views. Operator commands still pass through
the canonical append gateway and authority checks; a dashboard edit cannot
mutate an execution record directly.

### Why Not Adopt an Engine

| Criterion | Native kernel | External engine |
|---|---|---|
| Event authority | Reuses one Sentinel append gateway | Introduces or bridges a second history authority |
| ECS determinism | Cut directly binds tick and schedule profile | Requires custom synchronization outside engine transaction |
| Workbench receipts | Native invocation and CAS roots | Must translate activity outcomes across process/runtime boundary |
| Time Machine | One generation manifest | Engine history cannot rewind external reality or redb/CAS by itself |
| Resource model | Narrow tables and reconciliation tasks | Additional server, workers, database schema, queues, metrics, upgrades |
| Polyglot cost | Existing Rust/Go boundary | New SDK/runtime contracts and failure modes |
| License | Existing dependencies | Restate BSL; others permissive but still operationally costly |
| 1:n principle | Move event identities, digests, and pointers | Duplicates state and transport to another control plane |

The rejection is not permanent. A later engine must demonstrate lower total
correctness and operations cost on Sentinel workloads, then pass #705 and #656.

## Sentinel-Native Durable Execution Contract

### Execution State Machine

```text
Accepted
  -> IntentCommitted
  -> Reserved
  -> Dispatched
  -> Executing
  -> EffectCommitted
  -> ReceiptCommitted
  -> BusinessOutcomeCommitted
  -> QaBound
  -> Released
  -> Delivered
  -> CustomerAccepted
```

Every arrow is a durable compare-and-set transition with operation ID, canonical
request digest, expected aggregate version, authority generations, and an audit
event. No later state is inferred from an earlier state.

External-effect outcome is a nested state machine:

```text
Reserved -> Dispatched -> Committed
                    |--> Failed
                    |--> UnknownOutcome -> Probing -> Committed | Failed
                                              |------> ManualRecovery
```

Rules:

1. `Reserved` is durable before the first effect.
2. A downstream idempotency key is stable across retries.
3. A timeout after dispatch is `UnknownOutcome` unless the downstream system
   proves no effect occurred.
4. Only a trusted receipt or authoritative probe can move `UnknownOutcome`.
5. Re-arm is an authenticated operator command with a new attempt generation.
6. Business completion follows receipt persistence; it never precedes it.
7. QA, release, delivery, and customer acceptance remain independent states and
   cannot be self-attested by the executing agent or runtime.

### Durable Wait Contract

Timers, signals, approvals, and blocked work use one record shape:

```text
DurableWait {
  wait_id,
  workflow_id,
  workflow_generation,
  wait_kind,
  expected_event_or_actor,
  token,
  created_event_position,
  deadline_ms,
  policy_generation,
  authority_generation,
  state,
}
```

The wait is persisted before yielding. A wake event consumes the matching token
once. Old timers, duplicate signals, wrong actors, and stale generations are
recorded or ignored without advancing the workflow.

### Workflow Evolution

In-flight work pins:

- workflow definition/version and patch markers;
- schema and upcaster generation;
- work and tool profile digests;
- policy, organization, assignment, credential, and owner generations;
- runtime and artifact-format profiles.

Compatible code records an explicit patch/version event. Incompatible code
places the workflow in `VersionBlocked` and requires migration or operator
resolution. It never reinterprets old history silently. History compaction or
continue-as-new creates a linked generation with the prior history-root digest,
open waits, outcomes, artifact roots, and recovery-cut reference.

## DurableExecutionCut

`WorldSnapshot` remains a simulation-state anchor. It should not be overloaded
with business workflow and external-effect semantics. #722 should introduce a
linked, versioned `DurableExecutionCut` that participates in the broader
`StorageGeneration`/`RecoveryPoint` contract.

```text
DurableExecutionCut {
  schema_version,
  cut_id,
  parent_cut_id,
  state: Prepared | Committed | Superseded,
  created_at_ms,

  tick,
  sim_time,
  ecs_snapshot_id,
  ecs_schema_digest,
  schedule_profile_digest,

  event_truth_generation,
  event_high_watermark,
  append_outcome_frontier,
  dispatch_outbox_frontier,
  consumer_outcome_frontier,
  projection_generation,

  workflow_generation,
  workflow_event_frontier,
  workflow_operation_frontier,
  workflow_execution_frontier,
  workflow_projection_frontier,
  open_wait_set_digest,

  redb_storage_generation,
  redb_schema_digest,
  redb_snapshot_digest,

  workbench_generation,
  workbench_invocation_frontier,
  completion_receipt_root,
  unknown_outcome_set_digest,

  cas_manifest_roots,
  cas_reachability_digest,
  cas_pin_generation,
  gc_generation,

  nats_stream_frontier,
  nats_consumer_frontiers,

  code_digest,
  schema_generation,
  work_profile_digest,
  tool_profile_digest,
  policy_generation,
  organization_generation,
  assignment_generation,
  credential_generation,
  owner_generation,

  external_fact_set_digest,
  manifest_digest,
}
```

Secret values never enter the cut. It records only the credential identity and
generation required for safe rebind.

### Prepare and Publish Protocol

Cross-store atomicity is achieved as a saga with one publish-last validity
point, not by pretending the stores share a transaction:

1. Close admission for new customer mutations and external effects.
2. Close agent-scoped effect admission and take the Bevy tick/world barrier.
3. Drain in-flight local writes. Resolve external executions to a durable
   receipt, `UnknownOutcome`, or explicit blocked state.
4. Flush canonical event append outcomes and dispatch-outbox intents.
5. Capture the `EventTruthGeneration`, event high watermark, outcome frontiers,
   projection generation, and required consumer frontiers.
6. Capture workflow aggregate/operation/execution frontiers and open waits.
7. Capture a redb read generation or verified snapshot digest.
8. Capture the ECS snapshot and deterministic schedule/schema profile.
9. Capture Workbench invocation frontier, immutable receipt root, and unknown
   outcome-set digest.
10. Materialize CAS manifests and add durable pins for every live work,
    receipt, release, audit, and recovery reference.
11. Capture JetStream stream and required consumer frontiers without treating
    them as business outcomes.
12. Validate schema compatibility, generations, digests, and complete CAS
    reachability.
13. Fsync every staged store image, manifest, and containing directory according
    to its engine contract.
14. Write the full `Prepared` cut manifest and fsync it.
15. Re-read and verify the manifest plus all referenced roots.
16. Publish the `Committed` marker or active-cut pointer last and fsync it.
17. Rebuild any caches invalidated by the barrier and reopen admission last.

A crash before step 16 leaves only ignored staging. A crash after step 16
reconciles forward from the committed cut. The predecessor stays retained until
post-activation validation succeeds.

### Restore Protocol

1. Keep all customer, agent, timer, queue, and external-effect admission closed.
2. Load only a `Committed` manifest and verify its digest, predecessor chain,
   code/schema/profile compatibility, and authority requirements.
3. Verify and pin CAS artifact, receipt, and manifest roots before restoring
   mutable references.
4. Restore the canonical EventStore generation, append outcomes, dispatch
   intents, consumer outcomes, and poison/quarantine state.
5. Restore/rebuild workflow aggregates, operations, execution state, waits, and
   projections from the canonical event truth.
6. Restore redb under the selected `StorageGeneration` and verify its digest.
7. Restore Workbench records and reconcile runtime/receipt roots.
8. Restore ECS at the bound tick and schedule profile.
9. Rebuild all derived projections into inactive generations and validate them.
10. Reconcile JetStream redelivery against durable inbox/outcome receipts.
11. Probe every `UnknownOutcome`; keep unresolved effects blocked or quarantined.
12. Rebind current credentials and rebuild policy, organization, assignment,
    owner, runtime, route, and cache state.
13. Run semantic invariants: one accepted lineage, no stale authority, complete
    CAS reachability, no unowned execution, no required poisoned projection.
14. Activate the new generation/pointer.
15. Open normal effect and customer admission last.

Failure at any step keeps admission closed. Rollback activates only the verified
predecessor generation; it never deletes receipts or rewinds an external fact.

## Restore Modes

| Mode | Purpose | Permitted replay | External effects | Credentials/authority | Output |
|---|---|---|---|---|---|
| Simulation Replay | Reproduce historical deterministic world behavior | ECS plus canonical deterministic input tail | Disabled; prior outcomes are read-only placeholders | No effect authority | Historical simulation state/hash |
| Disaster Recovery | Resume current business operation after loss | Restore newest committed cut, then reconcile forward | Probe receipts/outcomes; never repeat solely due to rewind | Rebind current valid generations before admission | Active current system |
| Audit View | Inspect a historical business and runtime lineage | Read-only event/projection reconstruction | Disabled | No runtime credentials or mutation authority | Isolated evidence view |

No API may silently switch modes. Mode is explicit in the restore request,
manifest validation, credentials, network policy, and resulting service state.

## Required Failure Matrix

| Boundary/fault | Required result | Primary owner | Class |
|---|---|---|---|
| Acceptance received before workflow commit | Retry command by operation ID; no project unless agreement commit exists | #695/#731 | `BLOCKS_M0` |
| Workflow commit before dispatch | Outbox remains pending and resumes exactly once | #695/#731 | `BLOCKS_M0` |
| Reservation succeeds before workflow receipt commit | Probe by invocation/digest; resolve or `UnknownOutcome` | #695/#694 | `BLOCKS_M0` |
| Runtime launch before `Executing` commit | Probe process and immutable receipt; no blind second launch | #694 | `BLOCKS_M0` |
| LLM/tool/provider effect before receipt | Downstream probe/idempotency or `UnknownOutcome`/manual recovery | #694/#696 | `BLOCKS_M0` |
| Receipt before workflow completion | Replay receipt and commit business outcome | #694/#695 | `BLOCKS_M0` |
| Artifact publication before QA | Artifact remains pinned; workflow resumes at QA | #695/#696 | `BLOCKS_M0` |
| QA result before release authorization | Replay QA receipt bound to exact candidate digest | #696 | `BLOCKS_M0` |
| Release effect before delivery event | Probe release receipt; never create a second release | #696 | `BLOCKS_M0` |
| Delivery before customer receipt publication | Probe delivery; remain unknown/manual if downstream cannot prove | #696 | `BLOCKS_M0` |
| Timer pending across restart | Reload token/deadline; one matching wake advances | #695/#719 | `M0_HARDENING` |
| Human approval pending across restart | Reload actor/scope/version; stale or wrong approver rejects | #695/#719 | `M0_HARDENING` |
| Cancellation races completion | Attempt generation chooses one durable outcome; compensate only after probe | #694/#695/#719 | `M0_HARDENING` |
| Workflow code/profile changes in flight | Compatible marker replays; mismatch becomes `VersionBlocked` | #695/#719 | `M0_HARDENING` |
| Stale organization/assignment/policy/credential/owner generation | Reject before I/O and again before outcome acceptance | #694/#695/#696 | `BLOCKS_M0` |
| Cut crash before committed marker | Ignore prepared staging; active cut unchanged | #722/#719 | `M0_HARDENING` |
| Cut crash after committed marker | Reconcile forward from committed manifest | #722/#719 | `M0_HARDENING` |
| Restore fails at any store | Admission remains closed; activate only verified predecessor | #722/#719 | `BLOCKS_M0` |
| Restore completes before cache/projection rebuild | Readiness remains closed | #722/#731 | `BLOCKS_M0` |
| CAS root missing/corrupt/collected | Fail closed; no partial activation | #722/#728 | `BLOCKS_M0` |
| Required consumer is missing/poisoned/behind | Retention and readiness remain closed for affected capability | #731 | `M0_HARDENING` |
| Node/process loss after M0 single-node cut | Current single-node RPO/RTO contract applies; no HA claim | #722/#650 | `M0_HARDENING` |
| Cross-node worker or replicated workflow | Separate cluster durability and ownership contract | Cluster program | `POST_M0` |

Every test must assert one typed outcome: resumed, replayed, compensated,
blocked, quarantined, or manual recovery. Busy retry and silent abandonment fail.

## M0 Findings and Ownership

| Finding | Evidence | Classification | Ownership action |
|---|---|---|---|
| Workflow reservation can cross a transaction boundary without a universal unknown/probe state | PR #725 engine dispatch sequence | `BLOCKS_M0` | Refine #695 and #694 before their evidence can satisfy M0 |
| Active workflow draft has its own event/outbox authority beside #731 | PR #725 schema | `BLOCKS_M0` | Refine #695 to use or strictly derive from #731 canonical append/outcome contract |
| Workbench receipt/invocation frontiers are absent from product recovery cuts | Main `WorldSnapshot` versus PR #704 | `BLOCKS_M0` | #694 exposes the frontier and root; #722 binds them in `DurableExecutionCut` |
| QA, release, delivery, and customer acceptance need independent receipts and unknown-outcome handling | Product contract and external-effect matrix | `BLOCKS_M0` | #696 owns state/receipt/probe contracts |
| Current restore can activate simulation stores without workflow/Workbench truth | Main restore call sequence | `BLOCKS_M0` | #722/#731/#728 implement and test publish-last global cut |
| Durable timers, waits, version markers, cancellation races need a shared model | OSS comparison and M0 workflow | `M0_HARDENING` | #695 plus #719 model/fault tests |
| Operator recovery needs visible `UnknownOutcome`, quarantine, probe, and re-arm APIs | External-effect contract | `M0_HARDENING` | Reuse #694/#695/#696 operator surfaces; no new issue yet |
| Workflow history compaction and long retention | No immediate M0 loss if full history retained | `POST_M0` | #731 retention plus future measured optimization |
| External workflow engine or cross-node workflow workers | No current single-node correctness need | `POST_M0` | Revisit only through #705/#656 and cluster owners |

No new implementation issue is required by this study. Every accepted gap has
an existing owner. Creating another epic would duplicate active work. The owner
refinements are:

1. **#695:** use one canonical event/outcome authority; add
   `UnknownOutcome`, probe result, wait/version markers, and attempt generation.
2. **#694:** expose invocation frontier, completion-receipt root, unknown set,
   and authoritative probe/re-arm semantics to #722.
3. **#696:** make QA, release, delivery, and customer receipt independent,
   digest-bound durable outcomes with unknown handling.
4. **#731:** provide workflow aggregate/event/outcome identities and frontiers
   through the one append gateway and `EventTruthGeneration`.
5. **#722:** own `DurableExecutionCut`, the publish-last protocol, the three
   restore modes, and the final admission gate.
6. **#708/#726/#728:** provide redb/CAS generation, manifest, reachability, pin,
   schema-compatibility, and GC inputs to the cut.
7. **#719:** model all crash points, races, stale tokens, and version changes.
8. **#650:** run the full customer-to-acceptance crash/restart journey on the
   declared single-node runtime target.

## Security, Dependency, and Upgrade Impact

- No production dependency is added or removed by the accepted decision.
- No reviewed source is copied. Only behavior, state-machine, and test contracts
  are referenced under public provenance.
- Restate is BSL 1.1 at the pin and explicitly not open source until its change
  date; this reinforces the no-copy/no-adopt decision.
- MIT and Apache-2.0 candidates remain provenance references, not bundled code.
- If a future source port exceeds a trivial independently written contract, #705
  must record the source, license, transformed boundary, maintenance owner, and
  replacement test.
- #656 must watch upstream semantic changes in retries, history versioning,
  unknown outcomes, and persistence fault behavior only if Sentinel continues
  to use those projects as normative references.
- Security review must treat workflow history and receipts as sensitive audit
  data. Public events carry redacted metadata and digests, not customer content,
  credentials, raw prompts, or private tool output.
- Credential generations can be restored as references, but secret material is
  always reissued or rebound from the active credential authority.

## Exact TOGAF Target Delta

The TOGAF guide is the target vision, not an implementation diary. The following
content belongs in the virtual-company execution and Time Machine architecture
sections regardless of current delivery status.

### Product-language insertion

> A customer order does not live inside one process or one task row. Sentinel
> carries it as a durable chain from agreement through delegated work,
> Workbench artifacts, independent QA, release, delivery, and acceptance. After
> a crash, the company continues from the last proved boundary. A completed
> provider, tool, Git, deployment, billing, or delivery effect is recovered from
> its receipt or verified at its source; uncertainty stops the affected step
> instead of repeating it. Time Machine can replay the simulated world, but it
> never rewinds external reality.

### Technical architecture insertion

> Durable Execution is a Sentinel-native kernel over the canonical EventStore,
> workflow aggregates, Workbench receipts, redb, deterministic Bevy snapshots,
> CAS manifests, projection generations, and queue outcomes. There is one event
> truth and one operation/outcome authority, not a second workflow platform.
> `DurableExecutionCut` links `EventTruthGeneration`, `StorageGeneration`, the
> ECS tick/profile, workflow and Workbench frontiers, immutable receipt roots,
> CAS reachability, required consumer frontiers, and all code/schema/profile/
> policy/credential/authority generations. Preparation closes admission,
> drains or classifies effects, stages and fsyncs each local plane, validates
> digests and reachability, and publishes the committed manifest last. Restore
> stages one complete generation, reconciles unknown effects forward, rebuilds
> projections and caches, rechecks authority, and opens admission last.

### Three-mode insertion

> `Simulation Replay` replays deterministic world history with all external
> effects disabled. `Disaster Recovery` restores the newest committed cut and
> reconciles receipts and probes forward. `Audit View` is an isolated read-only
> historical view with no runtime credentials or mutation authority. These
> modes never share an implicit API or authority token.

### OSS source rationale insertion

> The design ports mechanisms rather than platforms: Restate contributes the
> stored-journal ACK and durable result-replay contract; Temporal contributes
> the explicit possibly-succeeded outcome and execute-then-timeout fault model;
> DBOS contributes compact operation-result replay and executor fencing; Dapr
> Workflow and Durable Task contribute generation-fenced timers, deterministic
> history, version stalls, and chaos-save tests; Rivet contributes Rust-native
> typed history coordinates and divergence detection. Their full control planes
> are not adopted because they would duplicate Sentinel's event truth,
> scheduler, stores, queues, and operational surface.

The exact integration anchors in the current guide are:

- Cluster 11 `StorageGeneration (der anwendungsweit konsistente Schnitt)` for
  the linked `DurableExecutionCut`, prepare/publish protocol, restore modes, and
  external-reality rule.
- The virtual-company/work-execution section for agreement-to-acceptance durable
  progress, Workbench receipts, independent QA/release, and typed unknown
  outcomes.
- The source-reference section for the five pinned mechanism families and the
  explicit no-external-engine decision.

## Verification and Evidence

This issue intentionally has no runtime benchmark:

```text
Runtime target class: NONE
Deploy targets: none
Read-only runtime targets: none
Benchmark targets: N/A
Rollback: revert the documentation commit
```

Required document checks:

```bash
git diff --check
typos docs/research/oss/durable-execution-workflows.md CHANGELOG.md
LC_ALL=C grep -nP '[^\x00-\x7F]' \
  docs/research/oss/durable-execution-workflows.md
python3 scripts/dependency-reachability-audit.py check-public-evidence \
  docs/research/oss/durable-execution-workflows.md
```

The final check is a manual rendered-link and claim review. No upstream timing,
build-server duration, or vendor exactly-once claim is accepted as evidence.

### Acceptance-Criteria Mapping

| AC | Evidence in this report | Result |
|---|---|---|
| AC-1 | Sentinel Baseline; State-Plane Callsite and Persistence Map; live PR heads | Covered |
| AC-2 | State Ownership Matrix with one class, authority, durability, and recovery rule per row | Covered |
| AC-3 | Landscape Inventory; five pinned Deep Source Reviews; Failure/Operations inventory | Covered |
| AC-4 | Candidate comparison matrix and Mechanism Comparison and Decisions | Covered |
| AC-5 | `DurableExecutionCut`; Prepare and Publish Protocol; Restore Protocol | Covered |
| AC-6 | Restore Modes and external-reality rules | Covered |
| AC-7 | Required Failure Matrix with typed resolution and owner | Covered |
| AC-8 | Executive Decision; per-mechanism decisions; dependency/upgrade impact | Covered |
| AC-9 | M0 Findings and Ownership; no duplicate issue required | Covered locally; reciprocal GitHub comments follow the published PR |
| AC-10 | M0 Findings and Ownership classifications | Covered; maintainer acknowledgement remains a PR/issue review action |
| AC-11 | Required public path, ASCII report, provenance, and verification commands | Covered locally; commit/PR readback follows verification |
| AC-12 | Exact TOGAF Target Delta in product and technical language | Covered; TOGAF integration remains with its architecture owner |

### Negative-Criteria Readback

- No dependency or external engine is adopted from reputation or advertising.
- No exactly-once external-effect claim is made.
- No local store snapshot is called an application recovery point.
- Admission opens last and only after every required plane agrees.
- Agents, runtimes, ACKs, projections, and caches cannot attest business
  completion, QA, release, delivery, or customer acceptance.
- Only source-backed lost-work, duplicate-effect, stale-authority, and unsafe
  restore defects are classified `BLOCKS_M0`.
- No VM, Rust build server, deployment, or performance benchmark is used.
- No new implementation owner duplicates an existing issue.

## Conclusion

Sentinel needs durable execution, but it does not need another workflow
platform. The correct architecture is smaller and stricter: one event truth,
one durable operation/outcome path, Workbench-owned effects and receipts, typed
unknown outcomes, generation-fenced waits and versions, and one publish-last
cut that makes every state plane agree before admission opens. This preserves
the deterministic ECS core, the 1:n resource principle, independent business
authority, and Time Machine without pretending external reality can be rewound.
