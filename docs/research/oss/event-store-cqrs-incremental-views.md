# Durable Event Truth and Generation-Safe CQRS

**Issue:** [#709](https://github.com/silentspike/project-sentinel/issues/709)
**Parent:** [#659](https://github.com/silentspike/project-sentinel/issues/659)
**Baseline:** `589f2bba0ca5b481aa7c3ac8212d48cf3903b770`
**Runtime target:** `NONE`
**Status:** Comparative source study and implementation contract

## Executive Verdict

Project Sentinel should retain SQLite through `rusqlite` as its local event
ledger and retain NATS JetStream as its asynchronous delivery substrate. It
should not add Materialize, EventStoreDB/Kurrent, Flink, RisingWave, Kafka, or a
second workflow platform to solve the current event and projection defects.

The missing component is a small Sentinel-owned event-truth layer around the
engines already in the product:

1. one versioned caller proposal, one store-sealed event envelope, and one
   authoritative append gateway;
2. explicit acknowledgement and durability classes;
3. stream-revision compare-and-append for concurrent writers;
4. a durable outbox that records JetStream broker acceptance through `PubAck`
   and evaluates transport durability against the effective server policy;
5. durable consumer inbox and outcome receipts for side effects;
6. independent, local projection frontiers committed with each read-model
   transaction;
7. generation-based rebuild and atomic projection activation instead of
   clearing the live database;
8. poison-event quarantine instead of advancing past malformed relevant data;
9. a mandatory consumer catalog that controls retention and initial cursor
   policy;
10. an immutable event-truth generation descriptor, a monotonic event-truth
    head, and sealed cut receipts referenced one-way by the storage generation
    owned by [#728](https://github.com/silentspike/project-sentinel/issues/728)
    and the active recovery owners #751/#753/#755. Closed
    [research #708](https://github.com/silentspike/project-sentinel/issues/708)
    remains input only and owns no implementation or activation step.

SQLite WAL provides the required local transaction primitive. JetStream
provides broker acceptance, at-least-once redelivery, and bounded producer
deduplication under its configured storage, replication, and sync policy. A
`PubAck` alone does not prove stable-media survival under OS or power loss.
Neither engine alone provides end-to-end exactly-once business effects. A local
effect can produce one durable business outcome through an atomic claim and
mutation. An external effect can do so only when the external system honors a
bound idempotency key or exposes an authoritative outcome probe. Otherwise an
indeterminate attempt is quarantined for manual resolution and is never
automatically retried.

The highest-priority defect is the current episode producer. It advances its
source cursor before the episode is durably written. A failed Hippocampus write
can therefore permanently remove an event from an agent's memory. This is a
`BLOCKS_M0` defect because an apparently successful company action can vanish
from the agent's durable experience after restart.

## Plain-Language Model

The event ledger is the company's notarized history. The outbox is the dispatch
desk. Projections are purpose-built reports. Agent memory is one of those
reports. NATS is the delivery network.

A notarized fact must not become less true because a report was rebuilt, a
message was delivered twice, or a process crashed between two instructions.
The target contract is therefore:

- write a fact once under a stable operation identity;
- acknowledge it only at the declared durability boundary;
- deliver it one or more times using the same identity;
- let every local consumer prove whether its effect already happened and keep
  unverifiable external uncertainty quarantined;
- advance each consumer's frontier only in the transaction that made the
  effect durable;
- rebuild a new report beside the old report, validate it, then switch readers
  atomically;
- retain history whenever a required consumer, backup, snapshot, effect, or
  recovery cut is uncertain.

"Exactly once" is an outcome property, not a transport checkbox. JetStream may
redeliver. A local second delivery observes the first atomic durable outcome.
An external second delivery may avoid another call only when a bound upstream
idempotency key or authoritative outcome probe proves the prior result;
otherwise uncertainty remains quarantined rather than being guessed away.

## Method

This issue performed no deployment, runtime mutation, Rust build, or
performance benchmark. The study used four evidence layers:

1. a source inventory of Sentinel's event schema, append paths, outboxes,
   consumers, projections, cursors, retention, backup, and restore paths;
2. live issue ownership and sequencing readback;
3. a pinned source and test review of SQLite, Turso, Materialize,
   EventStoreDB/Kurrent, and NATS Server;
4. mechanism-level comparison and an explicit decision for every mechanism.

Upstream benchmark results are not Sentinel evidence. Source from projects
with restrictive licenses was used only to understand public mechanisms. No
third-party implementation code was copied.

## Sentinel Baseline

### Engine and Process Topology

The crate name `sentinel-limbo` is historical. The implementation resolves
`rusqlite 0.38.0` and `libsqlite3-sys 0.36.0` with the `bundled` feature
because the evaluated Limbo version did not provide the required PRAGMA
surface. Those package versions do not prove the runtime SQLite engine
version; implementation evidence must also read `sqlite_version()` from the
deployed binary. The event database is opened with:

- WAL journal mode;
- `synchronous=NORMAL`;
- a 256 MiB mmap hint;
- an 8 KiB page-size request;
- a five-second busy timeout;
- one `Arc<Mutex<Connection>>` in the Rust event store.

The Rust-opened database contains:

- append-only domain events;
- event snapshots;
- the general NATS outbox;
- the durable LLM completion outbox and reservations;
- projection offsets;
- simulation metadata and dead-branch records.

The Go event-store package independently creates and migrates the overlapping
`events` and `outbox` tables through `modernc.org/sqlite`. The Go NATS bridge
calls this writable `Open()` path even though its business role is delivery.
Rust and Go are therefore two migration authorities, two SQLite bindings, and
two connection-policy implementations for one file.

### Event Envelope

`DomainEvent` currently carries a UUIDv4 event ID, string event type,
aggregate ID, JSON payload, correlation ID, causation ID, operation ID, tick,
wall time, schema version, and compensation type.

The constructor creates a random event ID and, by default, a second random
operation ID. The architecture guide already describes UUIDv7, a stream ID,
and deterministic operation IDs, but those target fields are not yet enforced
by the code. The SQLite row ID is currently also used as a global-looking
consumer cursor even though it is local to one database generation.

### Append Paths

Sentinel has two materially different append APIs:

- `append_event()` inserts only the event;
- `append_with_outbox()` inserts the event and delivery row in one SQLite
  transaction and uses operation-ID uniqueness for replay deduplication.

Both are used by production callsites. The current source scan finds eight
direct `append_event()` callsites in `orchestrator.rs`. It finds eleven
`append_with_outbox()` callsites across runtime, operator, orchestrator,
platform-control, resource-manager, and Nightrun production code. Direct
appends can therefore create facts that never enter the delivery path.

The LLM completion path is stronger. It reserves a stable request before the
provider call, persists the provider result, appends usage with retries, and
claims downstream action. This is the correct Sentinel pattern to generalize
only where the provider binds that request identity or supports an
authoritative result probe: intent, stable external request identity, durable
result, and idempotent effect claim. Without either upstream capability,
timeout remains indeterminate and cannot be converted into an automatic retry.

### Outbox and JetStream Bridge

The Rust outbox publisher and the Go bridge both implement publish-then-mark.
A crash after publication and before the mark causes redelivery, which is
correct at-least-once behavior.

The Go bridge sets `Nats-Msg-Id`, but publishes through core NATS and marks the
row published without requiring a JetStream publish acknowledgement. It can
therefore record success before the stream proves broker acceptance. Even a
future `PubAck` would establish stream acceptance and sequence assignment, not
by itself stable-media survival under the declared crash model. After five
failures it marks a row `failed`; the row then leaves the normal pending path
without a durable poison-lane workflow, operator resolution, or readiness
effect.

JetStream's message-ID deduplication window is ten minutes in the current
stream configuration. That bounds duplicate producer storage; it is not a
permanent idempotency record.

### Consumers and Side Effects

The Judge uses a durable explicit-ack consumer with `MaxDeliver=3`. It applies
in-memory and heuristic effects before acknowledging the message. There is no
durable consumer inbox or effect receipt. A crash after the effect and before
the acknowledgement can repeat the effect.

Malformed or incomplete messages are acknowledged and dropped. This protects
the consumer from a poison loop but converts a contract violation into silent
loss.

### Projection Worker

The projection worker maintains agent, room, KPI, task, cost, and hierarchy
views in a separate SQLite database.

The main projection path commits view changes in `projection.db`, then writes
the source offset to the event database in a second transaction. A crash
between those commits causes replay. Most handlers tolerate replay through
source-row identity, but not all do; KPI aggregation explicitly permits a
small divergence.

The hierarchy projection has a local projection watermark and rejects malformed
known v2 cost events. This is the strongest existing projection pattern and
should become the common contract.

The general projection loop silently skips unknown and malformed payloads and
then advances its watermark to the last row in the batch. A relevant malformed
event can therefore disappear permanently from that read model.

Full rebuild calls `clear_all()` on the active projection database, resets
offsets, and rebuilds in place. Readers can observe an empty or partially
rebuilt company. Startup migrations also ignore every `ALTER TABLE` error
rather than only the expected duplicate-column case.

### Episode and Hippocampus Projection

`EpisodeProducer` is an event consumer that builds an agent's episodic memory.
Its current cursor contract is unsafe:

1. first start initializes to the current maximum event row and skips all
   earlier history;
2. each tick stores the last event row before decoding and before writing the
   episode;
3. malformed events are skipped;
4. a failed Hippocampus write leaves the source cursor advanced;
5. process-local episode IDs restart at one.

A crash or write failure can therefore lose an episode permanently or reuse an
episode identity. This is not only a reporting defect: agent planning and
reflection consume this memory.

### Retention

Event pruning uses the minimum offset among rows that currently exist in
`projection_offsets` plus pending outbox state. There is no mandatory consumer
catalog.

Consequences:

- a required consumer with no offset does not protect history;
- an abandoned consumer row can block retention indefinitely;
- there is no typed start policy such as `Beginning`, `RecoveryCut`, or `Now`;
- outbox rows in all terminal states can be deleted without an independent
  outcome-retention contract;
- backup, inbox, effect, and projection-generation frontiers are not part of
  the delete proof.

### Snapshot and Restore Boundary

Event rows, projection offsets, redb state, ECS state, filesystem metadata,
and CAS reachability are not cut in one transaction. Restore writes stores in
sequence and can reset projection offsets to the current maximum event row.
That can declare events consumed without rebuilding their effects.

The world snapshot is itself stored as a row in `events.db`; it is not an
independent backup of that database or its WAL. `sentinel-db-maint
compact-events` copies selected event, snapshot, offset, metadata, and outbox
rows into a new SQLite file and checkpoints it, but it is an offline
maintenance path, not a generation-sealed online backup. Current restore
seeding clears the active projection tables, rebuilds them from snapshot state,
sets projection watermarks and event-store offsets to the current event
maximum, then appends `snapshot_restored`. The persisted
`restore_generation`/`dead_ranges` side table prevents discarded future rows
from re-entering normal reads, but it does not prove cross-store atomicity.

The cross-store generation design is owned by
[#728](https://github.com/silentspike/project-sentinel/issues/728). This study
extends it with an event-truth manifest and projection generations rather than
creating a competing restore protocol.

### Stored Incident and Issue Evidence

No runtime host was accessed for this study. A reproducible GitHub issue search
for `event store`, `outbox`, `projection restore`, `EpisodeProducer`, and
`JetStream durability` found these source-relevant historical incidents:

| Issue | Stored evidence | Architectural consequence |
| --- | --- | --- |
| [#475](https://github.com/silentspike/project-sentinel/issues/475) | Invalid NATS subjects left 4.06 million pending outbox rows and blocked event pruning; the issue records a 4.07 GB event store at peak. | Poison publication must quarantine visibly and must not hold the entire retention frontier forever. |
| [#259](https://github.com/silentspike/project-sentinel/issues/259) | An initial 5.55 million-row prune overloaded the tick-loop path; later readback identified millions of retained published outbox rows. | Retention and compaction need bounded work, independent outcome retention, and offline recovery tooling. |
| [#481](https://github.com/silentspike/project-sentinel/issues/481) | Multiple stores grew without complete retention, including evolution and Hippocampus data. | A mandatory catalog must cover every durable consumer/store, not only rows already present in `projection_offsets`. |
| [#488](https://github.com/silentspike/project-sentinel/issues/488) | Restore projection seeding used the wrong table name and omitted tasks/watermarks before repair. | Restore schema, projection generation, and validation must be versioned and fail closed. |
| [#487](https://github.com/silentspike/project-sentinel/issues/487) | Restore left process/runtime ownership inconsistent before repair. | Event/projection recovery cannot claim whole-product recovery; it composes with #728 and the active recovery owners. |

Closed status records historical repair, not proof that the broader #709
contract is implemented. Current source still proves E-01 through E-13 below.

### Productive Path Inventory

The inventory command scans Rust before each file's `#[cfg(test)]` section and
non-test Go files. Benchmarks and test directories are excluded.

| Plane | Productive source and count | Current transaction/publication cut | Owner |
| --- | --- | --- | --- |
| Event-only append | `services/sentinel-daemon/src/orchestrator.rs`: 8 callsites | SQLite event insert only; no delivery intent | #732 |
| Event plus outbox | `sentinel-runtime`: 1; operator API: 1; orchestrator: 2; platform control: 2; resource manager: 1; Nightrun: 4 | Event and one outbox row share a SQLite transaction | #732, then #733 |
| LLM request/effect state | `event_store.rs` reservation, provider-result, usage retry, action claim, terminal compaction; callers in `llm_bridge.rs` | Stronger stable request identity, but still a bespoke state machine | #695/#773 consume; #732 schema and #733 outcome substrate |
| Rust outbox | `outbox_publisher.rs` | Transport success then separate SQLite mark; crash can redeliver | #733 |
| Go event bridge | `sentinel-nats-bridge/main.go` plus writable `pkg/sentinel-go/eventstore` | Core NATS socket publish then mark; no JetStream `PubAck`; five failures become hidden `failed` | #733, NATS boundary #679 |
| Judge consumer/effects | `sentinel-judge/internal/service/stream.go` and `alerter.go` | Explicit-ack durable consumer; metrics/evolution/alerts run before ack without durable inbox outcome | #733 |
| Daemon Judge-alert consumer | `services/sentinel-daemon/src/nats_consumer.rs` | Explicit ack, bounded channel handoff; no canonical effect receipt | #733 |
| Main projection | `sentinel-projection/src/worker.rs` | View and local watermark commit together; event-store offset commits afterward | #734 |
| Hierarchy projection | same worker, independent local watermark and event-store offset | Rejects malformed known v2 cost payload before advancing; still split across databases | #734 |
| Dashboard/Gaia consumers | dashboard uses an ack-none `DeliverNew` event feed; Gaia readiness uses explicit ack | Observation/readiness lanes are not business-effect authorities | #733/#758 |
| Episode projection | `episode_producer.rs` plus Hippocampus append | Source offset commits before decode and redb append | #735 with #729 primitive |
| Retention | `can_prune`, `prune_batch`, snapshot manager | Minimum existing projection offset plus live pending outbox; no mandatory catalog | #736 |
| Snapshot/restore | `snapshot.rs`, orchestrator restore commit/rollback, redb/fs dumps | Sequential multi-store writes; active projection seed and offset jump | #728/#736 and active recovery owners |
| Offline maintenance | `sentinel-db-maint.rs` | Copy-to-new-file, integrity checks, WAL checkpoint; operator-controlled offline replacement | #732 schema, #736 retention/recovery |

### Sentinel Source Map

Line numbers were re-read at the report baseline. Implementation must repeat
the inventory against its own final main.

| Contract | Baseline source |
| --- | --- |
| SQLite dependency and historical Limbo boundary | [`crates/sentinel-limbo/Cargo.toml:1-22`](../../../crates/sentinel-limbo/Cargo.toml#L1-L22); [`Cargo.lock`](../../../Cargo.lock) resolves `rusqlite 0.38.0` and `libsqlite3-sys 0.36.0` |
| Event, outbox, completion, snapshot, offset, and restore-generation schema | [`event_store.rs:34-148`](../../../crates/sentinel-limbo/src/event_store.rs#L34-L148) |
| WAL, synchronous, mmap, page, busy timeout, and schema application | [`event_store.rs:341-418`](../../../crates/sentinel-limbo/src/event_store.rs#L341-L418) |
| Read-only database open | [`event_store.rs:458-477`](../../../crates/sentinel-limbo/src/event_store.rs#L458-L477) |
| Event-only append | [`event_store.rs:484-518`](../../../crates/sentinel-limbo/src/event_store.rs#L484-L518) |
| LLM request/outcome state machine | [`event_store.rs:523-919`](../../../crates/sentinel-limbo/src/event_store.rs#L523-L919) |
| Transactional event plus outbox append | [`event_store.rs:993-1112`](../../../crates/sentinel-limbo/src/event_store.rs#L993-L1112) |
| Owner-fenced event transaction and commit recheck | [`event_store.rs:2109-2158`](../../../crates/sentinel-limbo/src/event_store.rs#L2109-L2158) |
| Event envelope and default random identities | [`events.rs:24-109`](../../../crates/sentinel-common/src/events.rs#L24-L109) |
| Production direct event appends | [`orchestrator.rs:450`](../../../services/sentinel-daemon/src/orchestrator.rs#L450), `2055`, `2531`, `2568`, `2631`, `4705`, `7260`, `7372` |
| Production transactional-outbox appends | [`sentinel-runtime/src/lib.rs:614`](../../../crates/sentinel-runtime/src/lib.rs#L614); [`operator_api.rs:3429`](../../../services/sentinel-daemon/src/operator_api.rs#L3429); [`orchestrator.rs:303`](../../../services/sentinel-daemon/src/orchestrator.rs#L303), `4079`; [`platform_controlplane/mod.rs:633`](../../../services/sentinel-daemon/src/platform_controlplane/mod.rs#L633), `712`; [`resource_manager.rs:237`](../../../services/sentinel-daemon/src/resource_manager.rs#L237); [`runner.rs:486`](../../../services/sentinel-nightrun/src/runner.rs#L486), `528`, `556`, `581` |
| Rust outbox publish-then-mark loop | [`outbox_publisher.rs:62-201`](../../../crates/sentinel-limbo/src/outbox_publisher.rs#L62-L201) |
| Go schema/migration and writable-open authority | [`store.go:46-176`](../../../pkg/sentinel-go/eventstore/store.go#L46-L176), [`310-327`](../../../pkg/sentinel-go/eventstore/store.go#L310-L327) |
| Go core-NATS publish, retry, and terminal-failed path | [`sentinel-nats-bridge/main.go:174-247`](../../../services/sentinel-nats-bridge/main.go#L174-L247) |
| Stream retention, duplicate window, and replica configuration | [`streams.go:18-79`](../../../pkg/sentinel-go/messaging/streams.go#L18-L79) |
| Judge durable consumer, parse drops, effects, and final ack | [`stream.go:76-180`](../../../services/sentinel-judge/internal/service/stream.go#L76-L180), [`181-317`](../../../services/sentinel-judge/internal/service/stream.go#L181-L317) |
| Main and hierarchy projection loops | [`worker.rs:54-323`](../../../crates/sentinel-projection/src/worker.rs#L54-L323) |
| General projection skip and local frontier advance | [`worker.rs:361-415`](../../../crates/sentinel-projection/src/worker.rs#L361-L415) |
| Projection migration and startup repair | [`store.rs:191-265`](../../../crates/sentinel-projection/src/store.rs#L191-L265) |
| Destructive active projection clear | [`store.rs:344-368`](../../../crates/sentinel-projection/src/store.rs#L344-L368) |
| KPI non-idempotent replay caveat | [`store.rs:1081-1090`](../../../crates/sentinel-projection/src/store.rs#L1081-L1090) |
| Episode cursor and write ordering | [`episode_producer.rs:41-155`](../../../services/sentinel-daemon/src/episode_producer.rs#L41-L155) |
| Hippocampus load-extend-overwrite | [`store.rs:93-103`](../../../crates/sentinel-hippocampus/src/store.rs#L93-L103) |
| Retention eligibility, event/outbox deletion, and offsets | [`event_store.rs:1689-1907`](../../../crates/sentinel-limbo/src/event_store.rs#L1689-L1907) |
| Snapshot captures redb/ECS/fs metadata and projection offsets into `events.db` | [`snapshot.rs:135-220`](../../../services/sentinel-daemon/src/snapshot.rs#L135-L220) |
| Projection restore seed and offset jump | [`orchestrator.rs:3632-3786`](../../../services/sentinel-daemon/src/orchestrator.rs#L3632-L3786), [`4010-4050`](../../../services/sentinel-daemon/src/orchestrator.rs#L4010-L4050) |
| Sequential restore, rollback, and failure points | [`orchestrator.rs:3945-4170`](../../../services/sentinel-daemon/src/orchestrator.rs#L3945-L4170) |
| Offline event-store compaction/checkpoint | [`sentinel-db-maint.rs:101-218`](../../../services/sentinel-daemon/src/bin/sentinel-db-maint.rs#L101-L218), [`327-329`](../../../services/sentinel-daemon/src/bin/sentinel-db-maint.rs#L327-L329) |

Current tests prove narrower properties than the target contract:

| Test source | What it proves | What it does not prove |
| --- | --- | --- |
| [`sentinel-limbo/tests/acceptance.rs:162-425`](../../../crates/sentinel-limbo/tests/acceptance.rs#L162-L425) and [`event_store.rs:2569-3139`](../../../crates/sentinel-limbo/src/event_store.rs#L2569-L3139) | Append-only API shape, event+outbox transaction, operation-ID replay, monotonic offsets, WAL mode, and local outbox flow | Proposal/sealed-envelope field ownership, replay-before-revision, structured authority context/rebinding, conflicting replay digest, power-loss durable ack, PubAck, or permanent effects |
| [`outbox_publisher.rs:245-418`](../../../crates/sentinel-limbo/src/outbox_publisher.rs#L245-L418) | Transport failure remains pending and later retries; batching and shutdown drain | Store-issued claim generation/token CAS, stale-worker fencing, broker acceptance, effective storage/replica/sync policy, indeterminate PubAck, poison lane, concurrent publishers |
| [`sentinel-nats-bridge/bridge_test.go:1-78`](../../../services/sentinel-nats-bridge/bridge_test.go#L1-L78) | Stable operation ID maps to stable `Nats-Msg-Id` | JetStream publish API, PubAck, power-loss boundary, crash between publish/mark, permanent business idempotency |
| [`sentinel-projection/tests/acceptance.rs:113-738`](../../../crates/sentinel-projection/tests/acceptance.rs#L113-L738) | Rebuild equivalence, restart continuation, several idempotent handlers, hierarchy catch-up, and rejection of a malformed known v2 cost event | Blue-green activation, all-handler replay safety, poison resolution, or catalog/local-frontier crash reconciliation |
| [`episode_producer.rs:373-588`](../../../services/sentinel-daemon/src/episode_producer.rs#L373-L588) | Event-to-episode mapping, scheduling, process-local ID increment, and agent registration | First-start policy, redb write failure, crash ordering, source receipt, atomic frontier, or concurrent producers |
| [`orchestrator.rs:8504-8760`](../../../services/sentinel-daemon/src/orchestrator.rs#L8504-L8760) | Selected restore validation, rollback, and projection-seed behavior | One sealed event/projection/redb/CAS generation or no-offset-jump recovery |

## Current Findings

| ID | Severity | Finding | Failure outcome | Classification |
| --- | --- | --- | --- | --- |
| E-01 | Critical | EpisodeProducer advances its event cursor before a durable Hippocampus write. | Agent memory permanently omits a relevant fact after write failure or restart. | `BLOCKS_M0` |
| E-02 | Critical | NATS bridge marks delivery without JetStream `PubAck`, and no effective transport-durability policy is recorded. | A row can be recorded published before broker acceptance; even accepted data has an unproven power-loss boundary. | `M0_HARDENING` |
| E-03 | Critical | Judge and other side-effect consumers lack durable inbox/outcome receipts. | Redelivery after crash can duplicate business effects. | `M0_HARDENING` |
| E-04 | High | General projections skip malformed relevant events and advance the frontier. | Read model silently diverges from event truth. | `M0_HARDENING` |
| E-05 | High | Projection rebuild clears the active database in place. | Users observe empty or partial company state during rebuild or crash. | `M0_HARDENING` |
| E-06 | High | Projection state and the catalog offset commit in different databases. | Replay is unavoidable and non-idempotent handlers can diverge. | `M0_HARDENING` |
| E-07 | High | Direct event appends bypass the delivery outbox. | The ledger and downstream consumers observe different fact sets. | `M0_HARDENING` |
| E-08 | High | The authoritative ledger uses WAL plus `synchronous=NORMAL` without typed durability classes. | An acknowledged recent commit may be lost after OS or power failure. | `M0_HARDENING` |
| E-09 | High | Rust and Go independently migrate overlapping SQLite schema. | Version drift can corrupt startup ownership or fail differently by process order. | `M0_HARDENING` |
| E-10 | High | Retention has no mandatory consumer catalog or generation-bound frontier proof. | Required history can be deleted before a new or failed consumer processes it. | `M0_HARDENING` |
| E-11 | High | Snapshot restore can reset offsets to the event maximum without replaying effects. | Restored projections or memories falsely claim to be current. | `M0_HARDENING` |
| E-12 | Medium | Message deduplication is treated too close to exactly-once despite a bounded window. | Duplicate effects reappear outside the transport window or after state loss. | `M0_HARDENING` |
| E-13 | Medium | Event payload schema/upcasting and poison resolution are not centrally governed. | Upgrade behavior depends on consumer-specific parsing and silent defaults. | `M0_HARDENING` |
| E-14 | Medium | One mutexed SQLite connection serializes all event-store work. | It may limit throughput, but replacement without an implementation-owned target-runtime benchmark is unjustified. | `POST_M0` |
| E-15 | Medium | Sentinel lacks shared incremental arrangements for complex derived views. | Duplicate computation may grow with future analytics, not current M0 correctness. | `POST_M0` |

### Deterministic Failure Matrix

Each schedule names the durable records that may exist after the injected stop.
An implementation test must control the boundary with an explicit barrier or
failpoint; sleeping and killing at a guessed time is not evidence.

| Schedule | Injected boundary | Allowed durable state after restart | Forbidden result | Owner |
| --- | --- | --- | --- | --- |
| F-01 append validation | Before proposal/context/schema/digest/authority validation | No event, outbox, or effect reservation; unknown historical context remains explicitly unknown | Caller-supplied store fields, invalid context, or invented historical authority is visible to any reader | #732 |
| F-02 replay before revision | Exact retry after the original append advanced the stream, or same scoped operation ID with a different request/context digest | After namespace authentication, exact digest match returns the prior sealed envelope/outcome without checking the new head or creating an effect; mismatch is `OperationConflict` | Legitimate retry returns `WrongExpectedRevision`, or cross-tenant/project/context rebinding is treated as replay | #732 |
| F-03 new-operation append | Replay lookup misses; two genuinely new operations read revision `r`, then one stops after event insert before intents | Only one new operation wins revision `r+1`; the loser gets typed `WrongExpectedRevision`; event plus intents commit or roll back together | Revision is checked before exact replay lookup, both updates claim `r+1`, or an event exists without required intents | #732 |
| F-04 durable ack | After SQLite commit record but before required sync/ack return | Caller receives no success; retry returns the prior operation outcome or a typed indeterminate result | Success is returned for data lost by the declared crash model | #732 |
| F-05 outbox claim/reclaim | Lease expires or shutdown starts after claim; another worker receives a higher claim generation/token | Only the exact active token may publish or transition; the old claimant rejects/no-ops and advances no frontier | Old claimant publishes, retries, quarantines, or completes after reclaim | #733 |
| F-06 PubAck gap and stale claimant | Broker accepted the message; publisher lost `PubAck`; lease expires and a new claimant wins before the old response arrives | Republish/probe keeps the same message ID; a late old-token `PubAck` rejects/no-ops; only the active token records acceptance | Late old claimant completes, a new operation ID is used, or acceptance is inferred from socket write | #733 |
| F-07 publish then mark | Active-token `PubAck` arrives before local completion commit or configured server sync boundary | Broker acceptance records only under active-token CAS; redelivery is permitted; transport durability matches the declared storage/replica/sync policy | Stale token records acceptance, `PubAck` proves stable media, or duplicate effect is accepted as transport behavior | #733/#679 |
| F-08 local consumer effect | Lease expires/reclaim races local view/effect mutation and inbox outcome | Exact active-token CAS, local mutation, inbox outcome, and frontier commit together or not at all; old claimant rejects/no-ops | Stale claimant commits state/outcome or advances a frontier | #733/#734 |
| F-09 external effect | External response arrives after lease expiry/reclaim or shutdown, or dispatch has no terminal response | Active token plus bound upstream idempotency/probe resolves the same request; stale-token response rejects/no-ops; without upstream capability the attempt stays indeterminate/quarantined/manual | Late old claimant commits outcome/frontier or any blind automatic retry | #733/#710 |
| F-10 malformed relevant event | Decode/upcast/handler validation fails | Source position and digest enter quarantine; frontier does not pass it | Ack/skip plus frontier advance | #733/#734 |
| F-11 projection commit | After view+local-frontier commit, before catalog offset commit | Restart reconciles catalog from the local frontier; replay is harmless | Catalog guesses completion beyond local frontier | #734 |
| F-12 projection rebuild | During build, catch-up, validation, or activation | Old generation remains live until one atomic validated pointer switch | Readers see empty, mixed, or partial generations | #734 |
| F-13 episode write | Before, during, or after Hippocampus transaction | Episode, source receipt, and per-agent frontier commit together or not at all | Source frontier advances without episode/receipt | #735/#729 |
| F-14 first episode start | Consumer catalog has no frontier | Required consumer blocks readiness/prune until an explicit start policy is committed | Implicit jump to current max row | #735/#736 |
| F-15 retention | Any required frontier, outcome, backup cut, or generation is missing/unknown | Delete is denied and uncertainty is observable | History below an inferred frontier is removed | #736 |
| F-16 online backup | Before/during SQLite cut, before cut-receipt seal, or after seal but before top-level manifest activation | Normal appends advance only `EventTruthHead`; an incomplete cut is discarded/quarantined; a sealed `EventTruthCutReceipt` is referenced one-way by the top-level recovery manifest | Receipt embeds the final parent-manifest digest, mixed constituents activate, or a normal append churns `StorageGeneration` | #728/#736 |
| F-17 restore | After each sequential redb/fs/ECS/projection/event step | Restore fence stays closed until rollback or full validated activation | Partial restore is declared ready or offsets jump over unapplied effects | #728/#736 and recovery owners |
| F-18 offline compaction | Before copy, before/after checkpoint, or before operator replacement | Original file remains authoritative; candidate fails validation or atomically replaces it while stopped | Live writer races the copy or partial output replaces authority | #732/#736 |
| F-19 disk full/I/O fault | SQLite write, WAL sync/checkpoint, projection write, outbox/outcome write | Typed failure; no success ack; readiness reflects unresolved authority | Error is logged while cursor/outcome advances | #732-#736 |
| F-20 process/OS loss | At every F-02 through F-19 boundary | Recovery matches the declared `EventDurability` class and generation manifest | Process-restart-only evidence is generalized to power-loss durability | #732/#736 |

The matrix proves safety properties, not exactly-once transport. A local
exactly-one business outcome requires the stable operation, inbox/outcome, and
local state to share an atomic authority. An external exactly-one outcome also
requires a bound idempotency key honored by the external system or an
authoritative outcome probe. Without that capability, uncertainty is
quarantined and manual; no exactly-once claim is made.

## Durability and Acknowledgement Contract

SQLite's WAL documentation and implementation distinguish logical commit from
stable-media synchronization. Under WAL plus `synchronous=NORMAL`, SQLite
preserves database consistency across crashes, but the most recent committed
transactions may be lost after an OS crash or power loss. Under `FULL`, the WAL
commit path synchronizes at the transaction boundary.

Sentinel therefore needs explicit classes:

```text
EventDurability {
    Authoritative,       // customer work, agreement, cost, authority, effect
    DurableOperational, // repair, migration, audit, security outcome
    RebuildableTelemetry // high-rate observation with a named rebuild source
}
```

Rules:

- `Authoritative` and `DurableOperational` acknowledge only after the engine's
  durable-commit boundary.
- Rebuildable telemetry can batch or use deferred durability only when the
  producer names the durable source and startup rebuild gate.
- One connection-wide PRAGMA must not pretend to implement per-event classes.
  The implementation must either use a durable authoritative connection/store
  and a separate telemetry store or choose the strictest class for the shared
  ledger.
- A process exit test is insufficient. The fault matrix includes process crash,
  OS crash, lost unsynchronized WAL, torn logical log, disk-full, I/O error,
  and restart during checkpoint.

## Event Truth Contract

### Caller Proposal, Causal Authority, and Sealed EventEnvelopeV2

The following shape refines the accepted #718 contract without creating a
second schema owner; #732 owns the canonical field codec and bounds.

```text
AuthorityRefV1 {
    kind: Tenant | Company | Project | Workflow | WorkItem,
    id: BoundedAuthorityId,
    authority_generation: u64,
    authority_digest: Digest,
}

CausalContextV1 {
    schema_version: 1,
    tenant: AuthorityRefV1,
    company: AuthorityRefV1,
    project: AuthorityRefV1,
    workflow: Option<AuthorityRefV1>,
    work_item: Option<AuthorityRefV1>,
    request_id: RequestId,
    request_digest: Digest,
    correlation_id: CorrelationId,
    causation_event_id: Option<EventId>,
    operation_id: OperationId,
    attempt: u32,
    source_generation: GenerationId,
    source_digest: Digest,
    bounded_optional_lineage,
}

AppendProposalV2 {
    proposal_version: 2,
    requested_event_id: Option<UuidV7>,
    event_type: EventType,
    schema_version: u32,
    payload_codec: CodecId,
    payload_digest: Digest,
    payload: Bytes,
    causal_context: CausalContextV1,
    producer: ProducerId,
    owner_term: Option<OwnerTerm>,
    tick: Option<u64>,
    requested_durability: EventDurability,
    expected_stream_revision: Exact(u64) | NoStream,
    delivery_intents,
    effect_reservations,
}

EventEnvelopeV2 {
    event_id: UuidV7, // store-assigned or validated requested_event_id
    event_truth_generation: GenerationId,
    stream_namespace: AuthorityScopeDigest,
    stream_revision: u64,
    global_position: GenerationLocalPosition,
    event_type: EventType,
    schema_version: u32,
    payload_codec: CodecId,
    payload_digest: Digest,
    payload: Bytes,
    causal_context: CausalContextV1,
    producer: ProducerId,
    owner_term: Option<OwnerTerm>,
    tick: Option<u64>,
    appended_at_ms: i64,
    durability: EventDurability,
    canonical_request_digest: Digest,
    append_receipt_digest: Digest,
    sealed_envelope_digest: Digest,
}
```

The database position orders rows only within one event-truth generation. It
is not a portable event identity. Event IDs, operation IDs, structured causal
authority, and source provenance survive backup, restore, projection rebuild,
and cluster transfer.

The caller authors `AppendProposalV2`, never a persisted envelope. It must
supply the stable operation/request/correlation identities, direct causation
for non-root work, bounded payload/provenance, producer identity, requested
durability, and expected revision. It may request an event ID only under a
registered deterministic producer contract; otherwise the store assigns
UUIDv7. The gateway authenticates the caller/service and validates every caller
ID, digest, owner term, context bound, authority hierarchy, and generation. For
a replay lookup, validation proves the namespace structure and the caller's
permission to query that scope; only a replay miss performs current write
authorization, so an exact retry can retrieve its prior outcome after authority
or stream-head state has advanced.

The caller cannot supply `event_truth_generation`, `stream_namespace`, exact
stream revision, generation-local position, authoritative append time,
canonical request digest, receipt digest, or sealed-envelope digest. The store
derives or assigns those fields inside the append transaction. #732 remains the
sole schema/codec owner for `AuthorityRefV1`, `CausalContextV1`, proposal, and
envelope.

`CausalContextV1` is versioned and size-bounded, and its tenant/company/project
hierarchy plus applicable workflow/work-item authority generation/digest is
validated before append or effect. The derived authority-scope digest is part
of the stream namespace and canonical request/operation key. The exact context
or its canonical digest is preserved in delivery/effect intents, inbox/outcome
keys, quarantine evidence, replay lookup, backup cuts, and projection
authorization. Historical decode records missing context as unknown; it never
invents a tenant, project, generation, or authority digest, and unknown
historical authority cannot cross a mutating boundary without explicit repair.

### Append Gateway

All production writers use one typed gateway:

```text
append(AppendRequest {
    authenticated_caller,
    proposal: AppendProposalV2,
}) -> AppendOutcome
```

The transaction performs:

1. decode and canonicalize the proposal, authenticate the caller/service and
   structured authority namespace for scoped lookup, validate
   bounds/schema/payload/context integrity, and compute the complete canonical
   request digest without yet requiring current write authority;
2. look up `(authority_scope_digest, operation_id)` before reading the current
   stream head. Exact digest match returns the prior sealed envelope/outcome
   without a new event, intent, or effect even when the stream advanced;
3. reject the same scoped operation ID with a different complete proposal,
   context, expected revision, delivery/effect intent, or digest as
   `OperationConflict`;
4. only for a genuinely new operation, validate current write authority and
   compare the expected stream revision;
5. assign current event-truth generation, stream namespace/revision,
   generation-local position, event ID when not validly requested,
   authoritative append time, and store-owned receipt fields;
6. insert the sealed event and all delivery/effect intents carrying the same
   authority/context digest;
7. commit at the requested durability boundary and return the sealed envelope,
   sealed-envelope digest, append-receipt digest, and outcome digest.

`AppendOutcome` is closed and typed: `Appended`, `ReplayOfPriorOperation`,
`WrongExpectedRevision`, `OperationConflict`, `UnauthorizedProducer`,
`UnknownSchema`, `PayloadDigestMismatch`, `StaleOwnerTerm`, or
`IndeterminateDurability`. `Appended` and `ReplayOfPriorOperation` both bind the
same prior sealed envelope and outcome digest. A reused scoped operation ID
with a different canonical request digest is a conflict, never a replay. A
caller cannot change authority context, stream namespace, expected revision,
payload, durability, delivery intents, or effect reservations while retaining
the operation ID. Cross-tenant/project replay and context rebinding are typed
authorization/conflict failures, never deduplication hits.

An event-only append is allowed only for an explicitly typed local event that
has no delivery contract. It must not be an alternate unreviewed API.

## Delivery and Effect Contract

### Producer Outbox

```text
Pending
  -> Claimed { boot_id, attempt, lease_until, claim_generation, claim_token }
  -> BrokerAccepted { stream, sequence, duplicate, acceptance_digest,
                      claim_generation, claim_token_digest }
  -> Completed
  | Retryable { next_attempt, error_class }
  | Quarantined { reason, evidence }
```

The bridge uses JetStream publish and requires `PubAck`. An indeterminate
publish keeps the same message ID, probes when possible, or republishes. It
never marks completion from a successful socket write alone. Exhausted retry
enters a visible quarantine with operator resolve/retry/discard policy; it does
not disappear into a terminal `failed` row.

The store issues a monotonic `claim_generation` plus opaque claim token for
every claim or reclaim. Reclaim increments the generation and atomically
invalidates the prior token. Publish, broker acceptance, retry, quarantine,
completion, lease extension, and shutdown release each compare-and-swap the
exact active `(claim_generation, claim_token)`; a stale transition rejects or
is an idempotent no-op and cannot advance an outbox or retention frontier.

`PubAck` creates a typed acceptance record, not a universal durability proof:

```text
BrokerAcceptance {
    message_id,
    stream,
    sequence,
    duplicate,
    accepted_at_ms,
    acceptance_digest,
}

TransportDurability {
    acceptance_digest,
    storage_kind: Memory | File,
    replica_count,
    sync_always,
    sync_interval_ms,
    server_config_digest,
    declared_crash_model: Process | Server | OS | PowerLoss,
    assessment: Accepted | StableWithinDeclaredModel | Indeterminate,
}
```

The publisher reads and verifies the effective stream/server configuration,
not an intended configuration file. NATS FileStore defaults to asynchronous
background sync; the pinned implementation sets
`defaultSyncInterval = 2 * time.Minute` unless configuration overrides it, and
`SyncAlways` changes the write boundary. Therefore a `PubAck` proves broker
acceptance and sequence assignment. Stable-media or power-loss durability is
claimed only when the recorded storage kind, replica count, effective sync
policy, and tested crash model support it. A policy mismatch keeps readiness
closed or marks transport durability indeterminate.

### Consumer Inbox and Outcome

Every consumer that changes durable state or triggers an external effect owns
an inbox keyed by
`(consumer_id, authority_scope_digest, event_id, effect_id)`:

```text
Unseen -> Executing { boot_id, attempt, request_digest,
                      claim_generation, claim_token }
       -> Succeeded { outcome_digest, receipt,
                      claim_generation, claim_token_digest }
       | Retryable
       | Quarantined
```

The inbox claim and local business state commit in one transaction whenever
they share an engine, and both CAS the exact active store-issued claim token.
Reclaim invalidates the old token before another worker can commit. Late local
work, `PubAck`, external response, retry, quarantine, completion, shutdown, or
lease extension from the stale claimant rejects/no-ops and advances no
frontier. For an external effect, Sentinel reserves a stable request identity
before the call and records one of two explicit capability contracts:

- `BoundIdempotencyKey`: the external system commits to treating that identity
  as the same operation across retries;
- `AuthoritativeOutcomeProbe`: the external system can return the terminal
  result for that exact identity without issuing the effect again.

Only those capabilities permit automated reconciliation and an exactly-one
business-outcome target. If neither is supported, Sentinel may dispatch once,
but timeout or connection loss is `IndeterminateExternalEffect`; it is
quarantined for operator resolution and never automatically retried. A stable
Sentinel request ID and local receipt alone cannot prevent the external system
from executing twice.

JetStream `Nats-Msg-Id`, duplicate detection, and durable consumer ack floors
are transport optimizations. The inbox is the business idempotency authority.

## Projection Contract

### Projection Catalog

```text
ProjectionDefinition {
    projection_id,
    code_version,
    schema_version,
    source_generation,
    authority_scope_policy_digest,
    start_policy: Beginning | RecoveryCut | ExplicitPosition,
    required_for_readiness,
    handler_contract_digest,
}

ProjectionGeneration {
    generation_id,
    projection_id,
    source_generation,
    authority_scope_policy_digest,
    local_frontier,
    status: Building | CatchingUp | Validated | Live | Retired | Quarantined,
    validation_digest,
}
```

Each read-model transaction commits its view changes and local frontier in the
same projection database transaction. The event-store catalog mirrors that
frontier for retention and observability, but after restart it reconciles from
the projection's own durable frontier rather than guessing that processing
happened. Projection handlers authorize the event's structured causal context
against the generation's scope-policy digest; replay cannot rebind a tenant,
project, workflow, or work item to another projection authority.

### Handler Outcomes

Every event handler returns exactly one typed result:

- `Applied`;
- `IgnoredByContract` with a versioned reason;
- `Quarantined` with source position and payload digest.

Unknown event handling is a projection-version policy. A malformed event that
the projection claims to understand never advances silently.

### Blue-Green Rebuild

Rebuild never clears the live database:

1. create a new projection generation in a private file/schema;
2. replay from the declared start frontier;
3. continue incremental catch-up while the old generation serves readers;
4. validate counts, digests, referential invariants, and lag;
5. atomically switch the active-generation pointer;
6. let existing readers drain their old generation;
7. retire and later reclaim the old generation conservatively.

This is the local CQRS equivalent of Materialize's frontiers and arrangements,
implemented with SQLite and Sentinel generation metadata rather than importing
a distributed dataflow engine.

### Episode Projection

Episode creation follows the same projection contract:

- source `event_id` plus agent ID defines the durable episode/effect identity;
- one Hippocampus redb transaction writes the episode, source receipt, and
  per-agent frontier;
- the external projection catalog reconciles from that local frontier;
- relevant malformed events enter quarantine;
- a first-start skip requires an explicit `RecoveryCut` approved and recorded
  in the generation, never an implicit jump to `MAX(id)`;
- no process-local counter is an episode identity.

## Frontiers, Retention, and Recovery

### Mandatory Consumer Catalog

Every retention-relevant consumer declares:

- stable consumer ID and version;
- required/optional status;
- start policy;
- current generation and frontier;
- maximum tolerated lag;
- poison/quarantine state;
- rebuild source;
- outcome-retention requirement.

A required consumer with no frontier blocks pruning and product readiness. An
optional retired consumer is removed by a durable catalog transition, not by
deleting its offset row.

### Event Truth Generation, Head, and Cut

```text
EventTruthGenerationDescriptor {
    generation_id,
    parent_generation_id,
    sqlite_engine_fingerprint,
    schema_manifest_digest,
    event_codec_digest,
    producer_catalog_digest,
    causal_context_schema_digest,
    lower_position,
    created_by_operation_id,
    descriptor_digest,
}

EventTruthHead {
    generation_id,
    head_version,
    durable_upper,
    previous_head_digest,
    head_digest,
}

EventTruthCutReceipt {
    capture_operation_id,
    generation_id,
    parent_storage_generation_id: Option<GenerationId>,
    event_truth_head_digest,
    authority_scope_catalog_digest,
    durable_upper,
    required_consumer_frontiers,
    projection_generations,
    outbox_frontier,
    inbox_outcome_frontier,
    snapshot_recovery_cut,
    cut_digest,
}
```

`EventTruthGenerationDescriptor` is immutable lineage for compatibility. A new
descriptor is created only when the SQLite engine contract, schema manifest,
event codec, or producer catalog requires a new compatibility generation.
Normal event appends do not create a descriptor and do not advance
`StorageGeneration`; they monotonically advance `EventTruthHead` within the
current generation.

Backup, restore, and migration capture a specific head and seal one immutable
`EventTruthCutReceipt`. The receipt binds its capture operation, generation,
durable upper, all mandatory consumer frontiers, active projection generations,
the authority-scope catalog, unresolved outbox/inbox outcomes, and the snapshot
recovery cut. It may name
the prior storage generation for lineage, but it MUST NOT contain the final
parent `StorageGeneration` or `RecoveryPoint` manifest digest.

The top-level #728 `StorageGeneration`/`RecoveryPoint` manifest references the
sealed event cut digest one-way together with redb, filesystem, and CAS
constituent receipts. One final canonical top-level digest/signature owns
validation and activation. There is no reciprocal digest, no two-store atomic
activation claim, and no per-event whole-product generation churn.

### Retention Rule

The delete frontier is the minimum proven-safe point across:

- every required consumer's durable local frontier;
- every live projection generation;
- unresolved outbox and inbox/effect outcomes;
- the oldest retained snapshot and backup recovery cut;
- audit and legal retention policy;
- migration or cluster transfer claims;
- unknown or quarantined state.

Uncertainty retains data. Compaction may rewrite physical representation but
does not advance logical retention.

## Schema and Migration Contract

Sentinel owns one ordered SQL migration manifest with immutable IDs and
checksums. Exactly one migration authority applies it. Other processes verify
the compatible schema range and refuse writes when incompatible.

Event payloads have a registry of `(event_type, schema_version, codec)` plus
deterministic upcasters. A migration never edits old event payloads in place.
Consumers either understand, explicitly ignore, or quarantine each version.

Before a destructive schema transition:

1. create and validate an online SQLite backup under the storage generation;
2. apply the migration in a staged generation when possible;
3. run old/new reader compatibility tests;
4. activate only after semantic validation;
5. retain the prior generation until rollback expiry.

## Upstream Landscape

### Reproducible Shortlist Rubric

The landscape scan was repeated on 2026-07-29 from the current #709 contract
using the mechanism terms `embedded WAL crash recovery`, `event store expected
revision`, `persistent subscription poison`, `incremental view frontier`,
`stream PubAck dedup`, and `CQRS projection rebuild`. A candidate receives
0 (absent/poor), 1 (partial), or 2 (strong) for:

1. direct mechanism relevance;
2. pinned implementation plus failure-test depth;
3. local-first operational and deterministic/1:n fit;
4. usable license plus a discoverable security-reporting boundary;
5. low dependency and integration cost.

A score of six qualifies for normal deep review. A lower-scoring system can be
a mechanism-reference exception only when it uniquely covers an accepted gap;
that exception cannot become a product-adoption recommendation.

| Candidate | Mechanism | Source/tests | Operations/1:n | License/security | Integration | Total | Result |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| SQLite | 2 | 2 | 2 | 2 | 2 | 10 | Deep review; keep |
| Turso | 2 | 2 | 1 | 2 | 1 | 8 | Deep review; observe/port tests |
| NATS JetStream | 2 | 2 | 2 | 2 | 2 | 10 | Deep review; wrap existing |
| Materialize | 2 | 2 | 1 | 0 | 0 | 5 | Mechanism exception for frontiers/arrangements |
| EventStoreDB/Kurrent | 2 | 2 | 1 | 0 | 0 | 5 | Mechanism exception for expected revision/parking |
| RisingWave | 1 | 1 | 0 | 1 | 0 | 3 | Reject: duplicate distributed runtime |
| Apache Flink | 1 | 2 | 0 | 2 | 0 | 5 | Reject: JVM cluster/checkpoint plane |
| Kafka/Pulsar | 1 | 2 | 0 | 2 | 0 | 5 | Reject: duplicates NATS/log operations |
| Debezium | 0 | 1 | 0 | 2 | 0 | 3 | Reject: CDC, not event-truth authority |

### Pinned Provenance

The commits were retrieved on 2026-07-21 and their objects, paths, and license
files were re-read on 2026-07-29.

| Project and immutable revision | Implementation and failure tests | License/security boundary | Operational harness | Decision |
| --- | --- | --- | --- | --- |
| [SQLite `ef893b1d`](https://github.com/sqlite/sqlite/tree/ef893b1d66fa281d30b6d2165c398e1f08ffc801) | WAL sync in [`src/wal.c`](https://github.com/sqlite/sqlite/blob/ef893b1d66fa281d30b6d2165c398e1f08ffc801/src/wal.c#L4195-L4213), NORMAL/FULL distinction in [`src/pager.c`](https://github.com/sqlite/sqlite/blob/ef893b1d66fa281d30b6d2165c398e1f08ffc801/src/pager.c#L604-L625), backup in [`src/backup.c`](https://github.com/sqlite/sqlite/blob/ef893b1d66fa281d30b6d2165c398e1f08ffc801/src/backup.c), crash/I/O tests in [`test/walcrash2.test`](https://github.com/sqlite/sqlite/blob/ef893b1d66fa281d30b6d2165c398e1f08ffc801/test/walcrash2.test) and [`test/backup_ioerr.test`](https://github.com/sqlite/sqlite/blob/ef893b1d66fa281d30b6d2165c398e1f08ffc801/test/backup_ioerr.test) | [Public-domain scope](https://github.com/sqlite/sqlite/blob/ef893b1d66fa281d30b6d2165c398e1f08ffc801/LICENSE.md); no root `SECURITY` file at the pin, so Sentinel keeps its own advisory/version gate | `speedtest1` and fault corpus are useful methods only; no number transfers | Keep bundled SQLite; formalize sync, backup, and fault contracts |
| [Turso `7ce8778f`](https://github.com/tursodatabase/turso/tree/7ce8778fe8befaa89fd50c9ec07b49cbc7f4925e) | Recovery cases in [`RECOVERY_SEMANTICS.md`](https://github.com/tursodatabase/turso/blob/7ce8778fe8befaa89fd50c9ec07b49cbc7f4925e/docs/internals/mvcc/RECOVERY_SEMANTICS.md#L39-L85), ordered checkpoint states in [`checkpoint_state_machine.rs`](https://github.com/tursodatabase/turso/blob/7ce8778fe8befaa89fd50c9ec07b49cbc7f4925e/core/mvcc/database/checkpoint_state_machine.rs#L53-L100), deterministic faults in [`testing/concurrent-simulator`](https://github.com/tursodatabase/turso/tree/7ce8778fe8befaa89fd50c9ec07b49cbc7f4925e/testing/concurrent-simulator) | [MIT](https://github.com/tursodatabase/turso/blob/7ce8778fe8befaa89fd50c9ec07b49cbc7f4925e/LICENSE.md); no root `SECURITY` file at the pin; upstream calls the engine production-used but pre-1.0 and recommends independent backups | MVCC recovery and throughput harnesses exist; methods are hypotheses only | Port recovery vocabulary/failpoints; no replacement |
| [Materialize `7f6c5277`](https://github.com/MaterializeInc/materialize/tree/7f6c52776d27c34cb24210bc07a926c4cb6a7d5f) | Idempotent compare-and-append in [`machine.rs`](https://github.com/MaterializeInc/materialize/blob/7f6c52776d27c34cb24210bc07a926c4cb6a7d5f/src/persist-client/src/internal/machine.rs#L321-L449), compaction/GC in [`compact.rs`](https://github.com/MaterializeInc/materialize/blob/7f6c52776d27c34cb24210bc07a926c4cb6a7d5f/src/persist-client/src/internal/compact.rs) and [`gc.rs`](https://github.com/MaterializeInc/materialize/blob/7f6c52776d27c34cb24210bc07a926c4cb6a7d5f/src/persist-client/src/internal/gc.rs), data-driven tests including [`caa_idempotent`](https://github.com/MaterializeInc/materialize/blob/7f6c52776d27c34cb24210bc07a926c4cb6a7d5f/src/persist-client/tests/machine/caa_idempotent) | [Business Source License 1.1](https://github.com/MaterializeInc/materialize/blob/7f6c52776d27c34cb24210bc07a926c4cb6a7d5f/LICENSE); no root `SECURITY` file at the pin; mechanism study only | Persist/compute benches and arrangements exist but require a distributed product topology | Independent Sentinel implementation of the documented upper/since/generation behavioral contract; no copied, transliterated, or structurally derived source |
| [EventStoreDB/Kurrent `be4eb435`](https://github.com/EventStore/EventStore/tree/be4eb435a73ba90e1cf1480d8c7995a2000d7137) | Expected-revision API/tests in [`WhenExpectingRevision.cs`](https://github.com/EventStore/EventStore/blob/be4eb435a73ba90e1cf1480d8c7995a2000d7137/src/KurrentDB.Api.V2.Tests/Modules/Streams/AppendRecords/WriteOnly/WhenExpectingRevision.cs), durable checkpoint/parker implementations in [`PersistentSubscriptionCheckpointWriter.cs`](https://github.com/EventStore/EventStore/blob/be4eb435a73ba90e1cf1480d8c7995a2000d7137/src/KurrentDB.Core/Services/PersistentSubscription/PersistentSubscriptionCheckpointWriter.cs) and [`PersistentSubscriptionMessageParker.cs`](https://github.com/EventStore/EventStore/blob/be4eb435a73ba90e1cf1480d8c7995a2000d7137/src/KurrentDB.Core/Services/PersistentSubscription/PersistentSubscriptionMessageParker.cs), tests in [`PersistentSubscriptionMessageParkerTests.cs`](https://github.com/EventStore/EventStore/blob/be4eb435a73ba90e1cf1480d8c7995a2000d7137/src/KurrentDB.Core.Tests/Services/PersistentSubscription/PersistentSubscriptionMessageParkerTests.cs) | [Kurrent License v1](https://github.com/EventStore/EventStore/blob/be4eb435a73ba90e1cf1480d8c7995a2000d7137/LICENSE.md); upstream [security-reporting pointer](https://github.com/EventStore/EventStore/blob/be4eb435a73ba90e1cf1480d8c7995a2000d7137/SECURITY.md); behavior study only | Scavenge, persistent-subscription, and crash tests show operations burden that Sentinel should not import | Independent Sentinel implementation of the documented expected-revision/parking behavioral contract; no copied, transliterated, or structurally derived source |
| [NATS Server `40359273`](https://github.com/nats-io/nats-server/tree/40359273926ae1b238b8b50270a867fa742bb13e) | Stream storage/duplicate handling in [`jetstream.go`](https://github.com/nats-io/nats-server/blob/40359273926ae1b238b8b50270a867fa742bb13e/server/jetstream.go), sync configuration and the two-minute default in [`filestore.go:61-69`](https://github.com/nats-io/nats-server/blob/40359273926ae1b238b8b50270a867fa742bb13e/server/filestore.go#L61-L69) and [`filestore.go:327-333`](https://github.com/nats-io/nats-server/blob/40359273926ae1b238b8b50270a867fa742bb13e/server/filestore.go#L327-L333), ack floor/redelivery in [`consumer.go`](https://github.com/nats-io/nats-server/blob/40359273926ae1b238b8b50270a867fa742bb13e/server/consumer.go), tests in [`jetstream_test.go`](https://github.com/nats-io/nats-server/blob/40359273926ae1b238b8b50270a867fa742bb13e/server/jetstream_test.go) and [`jetstream_consumer_test.go`](https://github.com/nats-io/nats-server/blob/40359273926ae1b238b8b50270a867fa742bb13e/server/jetstream_consumer_test.go) | [Apache-2.0](https://github.com/nats-io/nats-server/blob/40359273926ae1b238b8b50270a867fa742bb13e/LICENSE); no root `SECURITY` file at the pin; existing dependency remains behind Sentinel auth/config | JetStream benchmark tests exist; no upstream throughput/latency number is Sentinel evidence | Use JetStream `PubAck` as broker acceptance; separately verify effective transport durability and retain Sentinel outcome authority |

### Candidate Fit Matrix

Legend: `S` directly supports the mechanism, `P` offers a portable partial
lesson, and `N/A` is not that product's authority. A cell describes mechanism
support, not an adoption recommendation.

| Mechanism | SQLite | Turso | Materialize | Kurrent | NATS |
| --- | --- | --- | --- | --- | --- |
| Local authoritative ledger | S: embedded ACID/WAL | S: embedded SQL/WAL, pre-1.0 caveat | P: durable persist, distributed | S: dedicated event service | N/A: transport |
| Stable event/operation identity | P: constraints only | P: constraints only | P: idempotent command token | S: event/stream identity | P: message ID within dedup window |
| Expected-revision append | P: implement in transaction | P: implement in transaction | S: compare upper-and-append | S: expected stream revision | N/A |
| Typed durability/ack | S: sync/commit modes | S: explicit WAL/log phases | S: durable shard command | S: server write result | P: `PubAck` proves acceptance; stable-media boundary depends on storage/replicas/sync policy |
| Producer outbox | P: atomic local rows | P: atomic local rows | P: durable command log | P: subscription/connector patterns | S: publish target, not source transaction |
| Permanent effect outcome | P: inbox tables | P: inbox tables | P: durable state machine | P: persistent subscriptions | N/A: ack floor is not business outcome |
| Poison quarantine | P: local table/state | P: local table/state | P: error/state collections | S: parked messages | P: max-delivery/advisory, needs Sentinel policy |
| Projection local frontier | S: same-DB transaction | S: persistent cursor lesson | S: upper/since frontiers | S: subscription checkpoint | P: ack floor only |
| Blue-green projection | P: separate files + pointer | P: separate files + pointer | S: maintained versions/frontiers | P: new projection/checkpoint | N/A |
| Shared incremental view | P: indexes/triggers, local | P: indexes/local query | S: arrangements | P: projections/subscriptions | N/A |
| Schema/upcast authority | P: migration manifest | P: migration manifest | S: versioned persist schema | S: event metadata/upcasting patterns | N/A |
| Retention frontier | P: delete under catalog proof | P: checkpoint/log proof | S: since/compaction frontier | S: stream retention/scavenge | P: stream limits plus consumer state |
| Online backup/recovery | S: online backup API | P: independent backup required | S: persist restore machinery | S: service backup/restore | S: stream snapshots, separate authority |
| Deterministic fault tests | S: crash/I/O corpus | S: simulator/yield/fault injection | S: data-driven/proptest state machine | P: broad service/crash tests | S: file/cluster/consumer tests |
| Local-first 1:n fit | S: one embedded copy | S: one embedded copy | N/A: distributed runtime | N/A: separate service | S: existing one-to-many transport |

Cross-cutting differences:

| Candidate | Failure/determinism boundary | Security/maintenance boundary | Dependency/integration cost | Performance hypothesis only |
| --- | --- | --- | --- | --- |
| SQLite | Mature crash/I/O corpus; Sentinel still needs its own filesystem and power-loss matrix | Public domain; Sentinel owns advisory tracking and configuration hardening | Already bundled through rusqlite; no new service | `FULL` durability and one mutex may cost latency/throughput; measure only on implementation runtime target |
| Turso | Strong deterministic simulator and explicit recovery phases; pre-1.0 evolution increases format/API risk | MIT; no pinned root security policy; independent backups explicitly prudent | Engine replacement, file compatibility, SQL/PRAGMA and binding migration are high risk | Simulator and MVCC designs are hypotheses, not a reason to replace |
| Materialize | Indeterminate compare-and-append and frontier tests are strong; distributed failures differ from local SQLite | BSL restricts reuse; large, fast-moving distributed maintenance surface | New distributed dataflow/storage plane violates low-resource 1:n | Shared arrangements may reduce repeated computation only after a Sentinel workload proves need |
| Kurrent | Expected revision, parking, checkpoint, and scavenging are mature service patterns | Kurrent License limits product use; separate auth/TLS/service patching | Adds a database service, network boundary, operations and migration | Dedicated event service may scale writes, but M0 has no evidence that SQLite is limiting |
| NATS | File/cluster/consumer tests expose configurable transport-durability boundaries; FileStore defaults to asynchronous two-minute sync and redelivery is expected | Apache-2.0; existing server still needs auth, limits, advisories and bounded cardinality | Existing dependency; change is API/config plus Sentinel acceptance, durability, and outcome records | Async fan-out may reduce coupling; sync policy and replicas add cost that target tests must measure |

### Source-Level Mechanisms

#### SQLite

SQLite's WAL implementation makes the commit record and synchronization
boundary explicit. The fault corpus models WAL crash, I/O failure, backup
interruption, and checksum behavior. Sentinel should use the online backup API
and adopt the same failure-discipline around its existing engine.

#### Turso

Turso's recovery contract distinguishes durable WAL and logical-log artifacts,
fails closed on a committed WAL with a missing or torn logical-log header, and
orders truncation last. Its persistent cursor is committed with pager state.
The simulator explores concurrent schedules and injected failures.

The useful result is a test model and recovery vocabulary. The pinned README
describes production users but also a pre-1.0, rapidly evolving engine and
recommends independent backups. That maturity and migration boundary does not
justify replacing Sentinel's current SQLite binding.

#### Materialize

Materialize's persist layer uses an upper frontier and compare-and-append with
a retry token to resolve indeterminate outcomes. Since frontiers constrain
logical compaction. Arrangements share maintained indexed state rather than
recomputing the same view per reader.

Sentinel needs the frontier and generation concepts, not Materialize's
distributed compute engine.

#### EventStoreDB/Kurrent

Expected stream revision prevents concurrent writers from silently overwriting
one logical stream. Duplicate append attempts return an existing outcome.
Persistent subscriptions checkpoint progress and park repeatedly failing
messages for explicit resolution.

These contracts map directly to Sentinel's append gateway and poison lane.
The current Kurrent License requires an independent Sentinel implementation of
the documented behavioral contract, with no copied, transliterated, or
structurally derived source and no new service dependency.

#### NATS JetStream

JetStream stores producer message IDs for a configured duplicate window and
returns a `PubAck` with stream sequence. FileStore defaults to background sync
at a two-minute interval unless `SyncAlways` or another interval changes the
boundary. Redelivery remains normal. Sentinel should record broker acceptance
and separately evaluate transport durability from effective storage,
replication, sync policy, and crash model while retaining its own permanent
inbox/outcome authority.

## Mechanism Comparison and Decisions

| Mechanism | Sentinel today | Upstream lesson | Decision | Boundary and benefit |
| --- | --- | --- | --- | --- |
| Local event ledger | `rusqlite`, WAL, one connection | SQLite has the required ACID and fault primitives | `Keep Sentinel` | No new engine; formalize durability and backup |
| WAL recovery tests | Unit/restart tests, limited power-loss model | SQLite/Turso model torn and missing durable artifacts | `Port algorithm/contract` | Deterministic failpoints around current engine |
| Event identity | UUIDv4 plus often-random operation ID | Event stores separate event ID, stream revision, operation replay | `Reimplement minimal` | Stable replay, provenance, and concurrency contract |
| Concurrent append | Unique operation ID, no stream expected revision | EventStore compare expected revision | `Reimplement minimal` | Prevent lost logical updates per aggregate |
| Producer delivery | Publish then mark; no required PubAck | JetStream PubAck plus explicit storage/replica/sync policy | `Configure existing dependency` | Record broker acceptance; claim transport durability only for the verified crash model |
| Consumer idempotency | Ack/redelivery without durable inbox | Durable inbox/outcome and parked poison patterns | `Reimplement minimal` | Exactly-one local outcome; external outcome only with bound upstream idempotency/probe |
| Projection frontier | Split DB/offset commits | Materialize upper/since; Turso atomic persistent cursor | `Reimplement minimal` | Replay-safe local truth and retention proof |
| Projection rebuild | Clear active DB and replay | Generation/frontier activation | `Reimplement minimal` | Readers never see empty or partial rebuild |
| Poison events | Skip/ack or terminal failed row | Persistent message parking/quarantine | `Reimplement minimal` | No silent loss and explicit operator resolution |
| Incremental views | Handwritten per projection | Materialize arrangements share maintained indexes | `Keep Sentinel` for M0 | Add only targeted shared arrangements after measurement |
| Schema authority | Rust and Go DDL/migrations | Immutable ordered manifest and compatibility gates | `Reimplement minimal` | One writer, deterministic upgrades |
| Retention | Minimum existing offset plus outbox | Read/since frontiers and mandatory consumers | `Reimplement minimal` | No delete under missing-consumer uncertainty |
| Online backup | Cross-store sequential snapshot | SQLite backup plus a sealed one-way cut receipt | `Integrate` with #728 | Top-level recovery manifest owns one activation digest without per-append churn |
| Turso replacement | Historical incomplete Limbo evaluation | Improved recovery/simulator, current maturity caveat | `Reject` now | Observe through #705/#656; avoid migration risk |
| Materialize/EventStore service | Not present | Rich mechanisms but heavy/restrictive integration | `Reject` | Concepts only; preserve low-resource 1:n design |

In the consumer-idempotency row, an exactly-one local business outcome is the
target result of Sentinel's stable identity plus one atomic local
receipt/effect protocol. For an external effect, the same target is allowed
only with an upstream-bound idempotency key or authoritative outcome probe.
Otherwise indeterminate is quarantined/manual and never automatically retried.
No claim is made about NATS, SQLite, or the current code providing universal
exactly-once behavior.

## Dependency and Security Impact

- No new production dependency is accepted by this study.
- Existing `rusqlite`, SQLite, and NATS dependencies remain subject to
  [#705](https://github.com/silentspike/project-sentinel/issues/705) ownership
  and [#656](https://github.com/silentspike/project-sentinel/issues/656)
  upgrade conformance.
- Materialize and EventStoreDB/Kurrent were reviewed only for behavioral
  mechanisms. Any follow-up is an independent Sentinel implementation of the
  documented behavioral contract; no copied, transliterated, or structurally
  derived source is permitted.
- Payload digest, producer identity, owner term, schema version, and source
  generation are validated before a fact or effect becomes authoritative.
- Quarantine content is access-controlled and redacted; it can contain customer
  payloads or provider responses.
- Consumer replay cannot bypass current authorization. Authority is rechecked
  before each external or owner-scoped effect.
- Backup and restore authenticate the generation manifest before activating
  event or projection state.

## Existing Ownership and Integration Map

| Concern | Existing owner | Boundary after this study |
| --- | --- | --- |
| Cross-store storage generation and restore | [#728](https://github.com/silentspike/project-sentinel/issues/728), active recovery [#751](https://github.com/silentspike/project-sentinel/issues/751)/[#753](https://github.com/silentspike/project-sentinel/issues/753)/[#755](https://github.com/silentspike/project-sentinel/issues/755) | #728 owns the top-level generation/recovery manifest and its sole canonical activation digest; it references a sealed event cut receipt one-way. Recovery owners activate whole-product cuts; #709 adds no second restore authority |
| Hippocampus transaction/fault policy | [#729](https://github.com/silentspike/project-sentinel/issues/729) | Owns redb atomic write primitive; episode producer owns source receipt/frontier |
| NATS internals | [#679](https://github.com/silentspike/project-sentinel/issues/679) | Owns NATS topology and effective storage/replication/sync configuration; #733 verifies it and owns broker-acceptance, transport-durability, outbox, inbox, and outcome semantics |
| Durable company workflow | [#695](https://github.com/silentspike/project-sentinel/issues/695), [#696](https://github.com/silentspike/project-sentinel/issues/696), [#710](https://github.com/silentspike/project-sentinel/issues/710) | Consume the event/effect contract; do not invent another ledger |
| Cross-node inbound and side effects | [#552](https://github.com/silentspike/project-sentinel/issues/552) | Consumes the same envelope and inbox identity; owns cluster queuing |
| Projection host provisioning | [#644](https://github.com/silentspike/project-sentinel/issues/644) | Owns service materialization, not read-model correctness |
| Dependency necessity and upgrades | [#705](https://github.com/silentspike/project-sentinel/issues/705), [#656](https://github.com/silentspike/project-sentinel/issues/656) | Record keep/wrap decisions and run compatibility gates |

Live readback on 2026-07-29:

| Issue | State/quality | Body SHA-256 | Reconciliation |
| --- | --- | --- | --- |
| #731 | open, `status:blocked`, `quality:ready` | `50bb8d20ce1cd619dcfa04f053520628739e23e0da0a7f4fd1cf17cc8920fd9e` | Ordered epic and five children |
| #732 | open, `status:blocked`, `quality:ready` | `84a2ca63ee470368518738edf463c1c9b0bea2935ea4b4196c8e6b6f2a167cdc` | Envelope/append/schema/durability authority |
| #733 | open, `status:blocked`, `quality:ready` | `8b795485a6fa3647dbe0b4a5ab5041cda57e55b24cd01f698fc9fc00455ce0dd` | PubAck outbox and durable outcomes |
| #734 | open, `status:blocked`, `quality:ready` | `1fdb1cd2bf2b24fad37933ea83c0f95f216057833c94a218106cb32a16e3a674` | Projection catalog, poison lane, generations |
| #735 | open, `status:blocked`, `quality:ready` | `0a2e421c10d463ca749fc8aa060887820bc547d6ab47f973ed8a1e591cc6aec9` | EpisodeProducer durability; #729 primitive |
| #736 | open, `status:blocked`, `quality:ready` | `5f1443552affdc42e34af255f927c2a3e57a61e379fa406043303ade03af50c3` | Mandatory frontiers, retention, event generation/recovery |
| #728 | open, `status:blocked`, `quality:ready` | `119bb7e05bc2f7826566ba3b7a08bec754e479e2255c64677879087d23085628` | Current body names #722 as research only and #751/#753/#755 as active recovery owners |
| #729 | open, `status:blocked`, `quality:ready` | `fee8a82424fd9efe5f9d750ead3fbb3aacc383976ba3d4fc26bb0c4611396e6d` | Sole redb/SQLite/CAS integrity and fault-policy owner |
| #679/#552 | open, ready/quality-ready | `1848abd856ac2acc15df93c897af2a1de90ae0218a23282fa176a5bd66408ee3` / `2204b57142dadbed292bfefa638e57759f9a9e57689f6885cc26d87d8da2d431` | NATS research and cross-node transport remain non-authoritative consumers |
| #695/#696/#710 | open, in-progress/quality-ready | `67e161fa68207c3b6a9a90e351cd5e58cf91ace90554442c5a45cb6886445f79` / `766be5692c219a0bc672d65119a02dee67bf2ff94cd05555d7b74a01f6d9d100` / `69a150dc7bfc842dc4d48edc4df26fd371a569676ab806093f83048a7a578421` | Workflow/QA research and implementation consume the substrate |
| #705/#656 | open, blocked/backlog, quality-ready | `238c8f2bbf845e1f84f123d218adcdea49ec489c2d49b09df7dc5c4ad733715f` / `8bd019a4487c1100c902661a6daef575f165ecda45b41de2b1ff61182474e660` | Dependency necessity and approved upgrade ownership |

One live owner-text drift remains outside this worker's write authority:
issue #696 AC-19 still says closed research issue #722 performs recovery. Its
durability section otherwise consumes #709/#731/#736. ORC should replace that
single implementation-owner reference with active #751/#753/#755 while
retaining #722 only as research input. This does not create a new #709 owner.

## Implementation Slices

This study is implemented through the already materialized ordered epic
[#731](https://github.com/silentspike/project-sentinel/issues/731) with five
children.

### Slice 1: Event Envelope, Append Gateway, and Schema Authority

Implementation owner:
[#732](https://github.com/silentspike/project-sentinel/issues/732).

Runtime target class: `BOTH`.

Scope:

- `EventEnvelopeV2` and compatibility decoder;
- caller-authored `AppendProposalV2` separated from store-sealed
  `EventEnvelopeV2`, with exact caller/store field ownership;
- #732-owned versioned, bounded `CausalContextV1` authority references and
  cross-language canonical vectors;
- deterministic operation identity and generation-local global position;
- replay-before-revision lookup keyed by authority scope plus operation ID,
  followed by expected-revision compare-and-append only for new operations;
- one typed append gateway and audited callsite migration;
- authoritative versus rebuildable durability classes;
- canonical SQL migration manifest and one migration authority;
- event schema registry, upcasters, and compatibility tests;
- crash/OS-loss failpoints for WAL and commit acknowledgement;
- exact retry after advanced head, stale-head new operation, conflicting digest,
  cross-tenant/project replay, context rebinding, unknown-history, and
  caller-supplied-store-field negative tests.

Rollback retains v1 decoding and dual-read compatibility. A new v2 event is
never rewritten to v1.

### Slice 2: PubAck Outbox and Durable Consumer Outcomes

Implementation owner:
[#733](https://github.com/silentspike/project-sentinel/issues/733).

Runtime target class: `SINGLE_NODE` for M0. Cluster delivery consumes the same
contract through #552 later.

Scope:

- claim/retry/quarantine outbox state machine;
- store-issued monotonic claim generation plus opaque token CAS on every
  outbox/inbox transition, receipt, local effect, and frontier commit;
- JetStream publish API and required `PubAck` as `BrokerAcceptance`;
- `TransportDurability` bound to effective storage kind, replica count, server
  sync policy, configuration digest, and declared crash model;
- configuration/readiness and process/server/OS/power-loss tests proving that a
  `PubAck` alone is insufficient for stable-media claims;
- indeterminate publish reconciliation with stable message ID;
- durable inbox and outcome receipts for Judge and all effecting consumers;
- external-effect capability contract requiring a bound upstream idempotency
  key or authoritative outcome probe for automated reconciliation; otherwise
  indeterminate is quarantined/manual and never auto-retried;
- crash after publish, effect, outcome commit, and ack;
- deterministic old-claimant/new-claim races, late PubAck/external response,
  lease-expiry, reclaim, and shutdown fencing tests;
- operator quarantine resolution and readiness/metrics.

Rollback can return transport scheduling to the legacy loop only while the new
outcome tables remain authoritative. It cannot discard receipts.

### Slice 3: Projection Catalog and Blue-Green Generations

Implementation owner:
[#734](https://github.com/silentspike/project-sentinel/issues/734).

Runtime target class: `SINGLE_NODE`.

Scope:

- projection definitions, local frontiers, and generation states;
- handler result contract and poison quarantine;
- view mutation plus local frontier in one SQLite transaction;
- catalog reconciliation from local frontier;
- side-by-side rebuild, catch-up, validation, atomic activation, drain, retire;
- idempotency repair for KPI and every handler;
- dashboard/readiness exposure and rollback to retained generation.

### Slice 4: Durable Episode Projection

Implementation owner:
[#735](https://github.com/silentspike/project-sentinel/issues/735).

Runtime target class: `SINGLE_NODE`.

Scope:

- remove cursor-before-write and implicit first-start skip;
- source event identity as durable episode receipt;
- one Hippocampus transaction for episode, receipt, and per-agent frontier;
- reconciliation with projection catalog;
- malformed-event quarantine and operator resolution;
- process/disk fault injection at every write boundary;
- token-free proof on the issue's authorized `SINGLE_NODE` target that one
  source event produces one durable remembered outcome across restart.

This slice is the only `BLOCKS_M0` child and may proceed as the first compatible
part after the report contract is accepted.

### Slice 5: Retention, Event Truth Head/Cut, Backup, and Fault Matrix

Implementation owner:
[#736](https://github.com/silentspike/project-sentinel/issues/736).

Runtime target class: `BOTH`.

Scope:

- mandatory consumer catalog and explicit start/retire policies;
- conservative frontier-based pruning;
- immutable `EventTruthGenerationDescriptor`, monotonic `EventTruthHead`, and
  sealed `EventTruthCutReceipt`;
- one-way integration in which #728's top-level recovery manifest references
  the cut digest and owns the sole canonical activation digest/signature;
- negative fixtures for per-append `StorageGeneration` churn, reciprocal digest
  cycles, unsealed cuts, constituent mismatch, and stale-head capture;
- SQLite online backup and projection cut;
- restore without offset jumps;
- process, OS, disk-full, I/O, WAL, checkpoint, backup, and schema rollback
  failpoints;
- local and cluster evidence reported separately.

## Acceptance Mapping

| #709 AC | State | Evidence in this report |
| --- | --- | --- |
| AC-1 | PASS | Current source map, productive path inventory, runtime contracts, stored issue/incident evidence, TOGAF readback, and live ownership table |
| AC-2 | PASS | Nine-candidate landscape with reproducible five-factor rubric, scores, shortlist exceptions, and rejection reasons |
| AC-3 | PASS | Five immutable upstream commits with implementation, tests, failures, license/security, and operations evidence |
| AC-4 | PASS | Fifteen-row mechanism-by-five-candidate matrix, cross-cut matrix, Sentinel findings, and deterministic failure schedules |
| AC-5 | PASS | One exact action per mechanism; no upstream/build/CI number is Sentinel runtime evidence |
| AC-6 | PENDING ORC OWNER INTEGRATION | Live ordered epic #731 and disjoint children #732-#736 exist, but #732/#733/#736/#728 require the exact proposal/envelope, causal authority, fencing, acceptance/durability, and acyclic cut addenda from this corrected report |
| AC-7 | PASS | E-01 through E-15 classified as `BLOCKS_M0`, `M0_HARDENING`, or `POST_M0`; only source-proven E-01 blocks M0 |
| AC-8 | PASS | This sole public English/ASCII report passes local/external link, GFM, public-safety, typo, and diff gates at the frozen head |
| AC-9 | PENDING ORC INTEGRATION | The exact semantic delta below is ready; this worker is forbidden to edit either TOGAF language copy or claim issue completion |

Negative-criteria readback:

| Negative AC | Result and evidence |
| --- | --- |
| AC-N1 | PASS: no dependency, manifest, lockfile, workflow, or code change |
| AC-N2 | PASS: immutable provenance and license/security boundaries precede every port/wrap/reject decision; no upstream code copied |
| AC-N3 | PASS: current code and stored incident evidence, not labels/tests alone, determine findings |
| AC-N4 | PASS: runtime target `NONE`; no VM, provider, Rust/Cargo, build-server timing, or performance run |
| AC-N5 | PASS: each accepted gap maps to #732-#736 and existing #728/#729/#679/#552/#695/#696/#710 owners |
| AC-N6 | PASS: the report separates SQLite durability, JetStream broker acceptance, policy-bound transport durability, bounded dedup, local permanent outcomes, and capability-gated external outcomes |

## TOGAF Delta

AC-9 is intentionally pending. ORC owns both language-specific edits. The
current English guide contains four statements that the accepted target must
replace rather than merely supplement:

- Cluster 03/08 calls the store "Limbo" even though production uses bundled
  SQLite through rusqlite.
- Cluster 08 says NATS provides "exactly-once via Msg-ID"; the configured
  duplicate window is ten minutes and is not permanent effect authority.
- Cluster 10 models an append-only table but omits event-truth generation,
  expected stream revision, payload digest, producer, owner term, durability,
  and atomic delivery/effect intents.
- Cluster 11 says restore is only a pointer/offset change with no replay and
  seeds offsets to `max(event_id)`; that can skip unmaterialized outcomes and
  conflicts with generation-safe recovery.

Apply this exact semantic target as one coherent event-truth subsection:

1. **Engine and authority.** SQLite through rusqlite remains the one local
   event ledger. One ordered migration authority owns its schema. Go and other
   processes verify a compatible schema and do not run independent DDL.
2. **Proposal, authority, and envelope.** A caller submits bounded payload and
   provenance in `AppendProposalV2`, requested durability/expected revision,
   and #732-owned versioned `CausalContextV1`. Tenant/company/project and
   applicable workflow/work-item authority ID, generation, and digest form the
   authenticated stream namespace. The store alone seals `EventEnvelopeV2`
   with current event-truth generation, exact stream revision,
   generation-local position, authoritative append time, canonical request
   digest, and receipt/envelope digests. Caller IDs are explicitly allowlisted
   and validated; no caller supplies store-owned fields. Historical decode
   records unknown authority and never fabricates context.
3. **Append and replay.** Every authoritative writer uses one typed gateway.
   After canonicalization and namespace authentication, it looks up authority
   scope plus operation ID before checking current revision. Exact request
   digest returns the prior sealed outcome without a new effect even after the
   head advanced; different digest is `OperationConflict`. Only a new operation
   checks current write authority/expected revision, assigns store fields, and
   atomically appends event plus context-bound delivery/effect intents.
   Event-only append requires an explicit no-delivery event class.
4. **Durability.** Authoritative and durable-operational events acknowledge
   only at their declared SQLite durable-commit boundary. Rebuildable telemetry
   names its durable source and startup gate. `synchronous=NORMAL` is not
   described as power-loss durable.
5. **Delivery versus outcome.** A JetStream `PubAck` proves broker acceptance
   and sequence assignment, not universal stable-media survival.
   `BrokerAcceptance` is distinct from `TransportDurability`, which binds the
   effective storage kind, replica count, server sync policy/configuration
   digest, and declared process/server/OS/power-loss model. Message-ID dedup is
   bounded. A local durable inbox/outcome can own one atomic local business
   result. An external exactly-one outcome is claimed only when the external
   system honors a bound idempotency key or exposes an authoritative outcome
   probe; otherwise timeout/indeterminate is quarantined/manual and never
   automatically retried.
   Every outbox/inbox claim receives a store-issued monotonic claim generation
   and opaque token. All transitions, receipts, local effects, and frontier
   commits CAS the active token; reclaim invalidates old workers, and late
   `PubAck`/external responses reject/no-op without frontier movement.
6. **Poison and uncertainty.** Malformed relevant events, exhausted delivery,
   and ambiguous effects enter typed quarantine with evidence and operator
   resolution. No relevant cursor/frontier advances past unresolved authority.
7. **Projection.** Each projection commits view mutations and its local
   frontier in one transaction. Rebuild uses a side-by-side generation:
   build, catch up, validate, atomically activate, drain, retire. The active
   database is never cleared in place. Projection and replay authorization bind
   the exact causal authority context; no tenant/project context rebinding is
   permitted.
8. **Agent memory.** Episode production is a durable projection. Episode,
   source-event receipt, and per-agent frontier commit in one Hippocampus
   transaction. First-start position is an explicit catalog policy, not an
   implicit jump to the current maximum.
9. **Retention.** The delete frontier is the minimum proven-safe position
   across every mandatory consumer, live projection generation, unresolved
   delivery/effect outcome, backup/snapshot recovery cut, legal/audit rule,
   migration, and transfer. Missing or unknown proof retains data.
10. **Recovery composition.** An immutable
    `EventTruthGenerationDescriptor` defines engine/schema/codec/producer
    compatibility lineage. Normal appends advance a monotonic
    `EventTruthHead`, not `StorageGeneration`. Backup, restore, or migration
    seals an `EventTruthCutReceipt` over the captured head, durable upper,
    mandatory frontiers, authority-scope catalog, projection generations,
    outcomes, and recovery cut.
    The receipt never embeds the final parent-manifest digest. #728's top-level
    `StorageGeneration`/`RecoveryPoint` manifest references the cut digest
    one-way with redb/filesystem/CAS receipts and owns the sole canonical
    activation digest/signature. Recovery never infers completion by setting
    offsets to the current maximum.
11. **Source boundaries.** SQLite and NATS are retained. Turso contributes
    recovery/fault-test ideas under MIT but is not adopted. Materialize and
    Kurrent inform documented behavioral contracts only. Any Sentinel
    implementation is independent, with no copied, transliterated, or
    structurally derived source. No second event store, dataflow platform, or
    workflow authority is introduced.

The public English guide and canonical German guide describe the same target
architecture independently in their own language and preserve the numbered
semantics above. They must not be produced by copying one language file over
the other. The target vision is not a claim that #732-#736 are delivered.

## Benchmarks

N/A for #709. Runtime target class is `NONE`.

Implementation issues measure only on their declared runtime targets. Relevant
co-primary metrics are correctness first, then append CPU-seconds/event,
synchronized writes/event, p50/p95/p99 durable-ack latency, outbox and consumer
lag, duplicate-delivery-to-duplicate-effect ratio, projection catch-up rate,
rebuild cutover pause, retention scan cost, recovery time, and peak memory.

Build-server time, CI duration, and upstream benchmark numbers are never
runtime evidence.

## Limitations

- This study did not run crash or power-loss tests; it identifies the exact
  boundaries that implementation fault tests must exercise.
- Upstream source review does not transfer reliability or performance claims
  to Sentinel.
- SQLite `fsync` ultimately depends on filesystem, kernel, hypervisor, and
  storage-device behavior. Sentinel can enforce and test the declared OS
  contract but cannot promise stronger hardware semantics.
- Distributed consensus and event replication remain cluster architecture
  concerns. A locally correct ledger is not automatically replicated.
- Full durable workflow semantics remain in #710. This issue provides the fact
  and effect substrate rather than a second workflow orchestrator.

## Final Go/No-Go

**GO:** keep SQLite/rusqlite and JetStream, implement the five ordered
event-truth slices, and integrate them with the storage generation.

**NO-GO:** engine replacement, core-NATS publish recorded as durable delivery,
transport-window exactly-once claims, cursor advance before local effect,
silent malformed-event skip, active-database rebuild, dual migration authority,
or retention under missing-consumer uncertainty.

Until E-01 is fixed, the M0 agent-memory path must not claim crash-safe durable
experience. The other findings do not stop unrelated M0 implementation, but an
active path must not claim durable delivery, exact effects, rebuild-safe CQRS,
or generation-consistent recovery beyond the boundaries proven today.
