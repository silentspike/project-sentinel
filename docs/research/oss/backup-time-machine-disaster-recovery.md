# OSS backup, Time Machine, and disaster recovery study

- Status: REVIEW_READY candidate
- Issue: [#722](https://github.com/silentspike/project-sentinel/issues/722)
- Parent: [#659](https://github.com/silentspike/project-sentinel/issues/659)
- Baseline: `55ace5371a64d4369dccf7aea13ceb32ae441891`
- Research cut: 2026-07-29
- Runtime evidence: none; this is a source and test audit, not a deployment or
  benchmark

## 1. Executive decision

Sentinel should not adopt a backup product as its recovery coordinator. Its state is
split across databases, immutable content, in-memory ECS state, future workflow
journals, derived projections, runtime processes, configuration, and credentials.
None of the reviewed systems can create a valid application-consistent cut across
those authorities without Sentinel first defining and enforcing the cut.

The recommended architecture is:

1. **Reimplement minimal:** add a Sentinel-owned `RecoveryPointManifestV1`, signed
   `RecoveryPointEnvelopeV1`, and durable multi-process recovery coordinator. The
   authenticated envelope is published last and is the only object that makes a
   multi-store capture restorable.
2. **Keep Sentinel:** retain the existing WorldSnapshot, event/outbox,
   projection-seed, owner-fence, CAS pin, and runtime-reconciliation mechanisms as
   components of the coordinator. Do not describe a WorldSnapshot alone as a
   whole-product backup.
3. **Integrate, subject to #705:** evaluate restic as an external, least-privilege
   encrypted transport for already sealed recovery bundles. Restic must never run
   against live mutable stores and must not define the cut.
4. **Port algorithm/contract:** use Litestream's contiguous transaction chain,
   restore-to-temporary-file, fsync, atomic publication, retention protection, and
   explicit gap/error model for each SQLite capture. Do not embed Litestream as a
   cross-store coordinator.
5. **Port algorithm/contract:** use Velero's explicit non-terminal, finalizing,
   partially-failed, hook, and ordered-restore concepts in Sentinel's coordinator.
   Do not add Kubernetes or Velero as a dependency.
6. **Reject as product requirements:** do not require OpenZFS, btrfs, or CRIU.
   Filesystem snapshots may be an optional operator defense-in-depth layer. Runtime
   recovery restores durable intent and reconciles processes; it does not restore
   PIDs, sockets, leases, credentials, or live provider connections.

This is `M0_HARDENING`, not a newly discovered `BLOCKS_M0` code defect. Production
acceptance of authoritative customer work needs a proved whole-product restore path,
but geo replication, cluster quorum recovery, and live process checkpointing remain
separate work.

## 2. Method and reproducibility

### 2.1 Evidence rules

- Current Sentinel claims are based on the baseline source and tests, not closed
  issue labels.
- Upstream claims are tied to immutable commits and load-bearing source or test
  paths. Documentation is used only for operator contracts or privilege boundaries.
- No upstream benchmark number is treated as Sentinel performance evidence.
- No upstream code was copied, vendored, built, or executed.
- Release popularity and feature lists did not decide the result.
- All recommendations distinguish a per-store checkpoint from a valid cross-store
  recovery point.

### 2.2 Reproduction commands

The upstream review used the following read-only method for every shortlist entry:

```text
git clone --filter=blob:none --branch <tag> <repository>
git -C <checkout> rev-parse HEAD
git -C <checkout> status --porcelain
rg -n '<mechanism or failure term>' <source-and-test-roots>
nl -ba <load-bearing-file> | sed -n '<range>p'
```

All five pinned worktrees were clean. The Sentinel baseline was checked with:

```text
git rev-parse HEAD
git status --short --branch
rg -n '<store, snapshot, restore, fence, credential, or path>' \
  crates services cmd deploy config docs
gh issue view <owner> --json number,title,state,labels,url
```

## 3. Current Sentinel baseline

### 3.1 Implemented recovery building blocks

| Building block | Current implementation and evidence | What it proves | Claim boundary |
|---|---|---|---|
| World snapshot envelope | [`WorldSnapshot`](../../../crates/sentinel-common/src/types.rs#L639-L654) binds schema, tick, event cursor, 12-table redb dump, ECS, projection offsets, and optional filesystem metadata. | A versioned simulation restore anchor exists. | It excludes CAS bytes, several product stores, config, credentials, and durable runtime intent outside the envelope. |
| Per-container cut | [`SnapshotCut`](../../../crates/sentinel-common/src/types.rs#L708-L731) records owner epoch, event cursor, CAS pins, and inbound cursor and explicitly models multi-store work as an idempotent saga rather than distributed 2PC. | A fenced and reconcilable cut contract exists for the bounded migration class. | It is not a whole-product disaster-recovery manifest. |
| Snapshot capture | [`create_and_store`](../../../services/sentinel-daemon/src/snapshot.rs#L131-L204) reads redb, ECS, FS metadata, offsets, and event cursor in sequence, saves the snapshot, then pins CAS references. | The Time Machine path captures and retains an in-process world anchor. | The reads are not one transaction. A crash can occur after snapshot save and before CAS pinning. |
| redb restore | [`restore_all_tables`](../../../crates/sentinel-redb/src/lib.rs#L777-L796) replaces all 12 tables in one redb write transaction. | Atomicity inside `state.redb`. | No atomicity with Limbo, FS metadata, ECS, or other redb files. |
| FS metadata restore | [`MetadataStore::restore_all_tables`](../../../crates/sentinel-fs/src/metadata.rs#L621-L660) restores inode, dirent, refcount, and trash tables in one fenced redb transaction. | Atomicity inside `metadata.redb`. | Blob bytes are separate and must already exist. |
| CAS | [`CasStore`](../../../crates/sentinel-fs/src/cas.rs#L28-L39) uses SHA-256 names and compressed immutable blobs; [`store`](../../../crates/sentinel-fs/src/cas.rs#L91-L116) writes a temporary file and renames it. | Content identity, deduplication, and local atomic visibility. | The shown write path does not fsync file and parent directory. Snapshot metadata carries references, not blob backup copies. |
| Event plus outbox | [`append_with_outbox`](../../../crates/sentinel-limbo/src/event_store.rs#L993-L1052) inserts an event and pending outbox row in one SQLite transaction with operation-id idempotency. | Durable event publication intent survives a publisher failure. | `SENTINEL_EVENTS` can be rebuilt from that authority, but current direct `SENTINEL_JUDGE` publication and durable consumer cursors can hold the only in-flight effect. NATS must be classified per stream/consumer rather than globally dismissed. |
| Limbo durability | [`EventStore::open`](../../../crates/sentinel-limbo/src/event_store.rs#L341-L418) enables WAL and `synchronous=NORMAL` and initializes events, outboxes, snapshots, offsets, and metadata. | A coherent single SQLite authority and schema bootstrap exist. | Copying only the main database file while live is unsafe; WAL-aware capture is mandatory. `NORMAL` also defines the crash-durability ceiling to document and test. |
| Retention and immutability | The database trigger and daemon guard protect recent world snapshots; [`delete_redundant`](../../../services/sentinel-daemon/src/snapshot.rs#L421-L478) releases CAS pins only after deletion. | Accidental or malicious young-snapshot deletion is constrained. | A pin leak is safe for integrity but can grow storage. Retention is not an offline backup. |
| Restore validation | [`execute_world_restore_transfer`](../../../services/sentinel-daemon/src/orchestrator.rs#L4170-L4311) validates referenced CAS blobs and projection schema before fencing, bounds replay, and records an owner epoch. | Missing content and unsupported projection schema fail before mutation. | It validates only stores represented by WorldSnapshot. |
| Ordered restore and rollback | [`commit_world_restore_stores`](../../../services/sentinel-daemon/src/orchestrator.rs#L3945-L4087) restores redb, FS metadata, ECS, bounded replay, projections, offsets, generation, dead branch, and the restore event. [`rollback_world_restore_stores`](../../../services/sentinel-daemon/src/orchestrator.rs#L4091-L4131) applies a pre-restore snapshot on failure. | Store ordering, projection reset, and fail-closed fencing have targeted tests. | Commit and rollback are sequential sagas. Neither is atomic across stores, and rollback itself can fail. |
| Runtime reconciliation | Runtime teardown starts only after the store commit, followed by per-agent respawn ([source](../../../services/sentinel-daemon/src/orchestrator.rs#L4366-L4395)). | Durable state is authoritative over runtime processes. | A crash between store commit and teardown/respawn requires restart reconciliation; live process state is not part of the transaction. |

Targeted current-main tests include CAS-missing validation, failure injection after
each restore phase, rollback-failure fencing, projection reseeding, runtime teardown,
snapshot immutability, event/outbox atomicity, CAS pinning, and FS metadata
round-trips:

- [`restore_validate_rejects_missing_cas_blob_before_commit`](../../../services/sentinel-daemon/src/orchestrator.rs#L8504)
- [`mid_commit_failures_roll_back_to_pre_snapshot_without_mixed_state`](../../../services/sentinel-daemon/src/orchestrator.rs#L8665)
- [`rollback_failure_keeps_restore_fence_active_and_reports_critical`](../../../services/sentinel-daemon/src/orchestrator.rs#L8677)
- [`projection_restore_seed_resets_future_views_and_seeds_snapshot_state`](../../../services/sentinel-daemon/src/orchestrator.rs#L8715)
- [`test_append_with_outbox_atomic`](../../../crates/sentinel-limbo/src/event_store.rs#L2569)
- [`test_snapshot_delete_blocked_within_7_days`](../../../crates/sentinel-limbo/src/event_store.rs#L3399)
- [`snapshot_pin_blocks_gc_then_unpin_frees`](../../../crates/sentinel-fs/src/metadata.rs#L1384)

These tests prove important local invariants. They do not prove that every product
authority is captured or that a machine-loss restore works.

### 3.2 Cross-store authority and coverage map

The recovery coordinator must classify data by authority, not by filename alone.
Paths below are logical deployment paths; a manifest must use logical store IDs
rather than accepting caller-supplied arbitrary paths.

The registry uses four classes: `authoritative` is needed to preserve accepted
facts, `authoritative_in_flight` is temporarily the sole durable copy of an
uncommitted effect, `derived` has a named authoritative rebuild source, and
`ephemeral` is intentionally discarded. `node_local_authority` is durable but must
be reconciled rather than cloned onto a disaster host.

| Logical plane | Current source and role | Class | Required capture, rebuild, or reconciliation receipt |
|---|---|---|---|
| `events.db` | Authoritative event log, outboxes, world snapshots, restore generations/dead ranges, and current projection offsets. | `authoritative` | #732/#733/#736 provide one WAL-aware `EventTruthGeneration` receipt with schema/codec fingerprint, durable upper position, outbox/inbox outcomes, mandatory consumer frontiers, and file digest. Never copy only the live main file. |
| `state.redb` | Twelve tables of simulation state and durable agent facts, also represented logically in WorldSnapshot. | `authoritative` | #728/#729 provide an engine-consistent `SealedStoreGenerationReceipt`; #722 records that receipt and does not choose the redb copy primitive. |
| ECS memory | Current simulation resources and components. `RoomPhysicsState` is explicitly absent from the existing WorldSnapshot ([source](../../../services/sentinel-daemon/src/orchestrator.rs#L3709-L3715)). | `authoritative` for registered world resources | #707 owns the deterministic freeze/schedule barrier and complete resource inventory. Capture a versioned ECS digest and tick under that barrier; any unclassified resource fails coverage. |
| `metadata.redb` | Namespace, CAS references, trash queue, and snapshot pins. | `authoritative` | #728 generation receipt bound to the exact CAS reachability root and WorldSnapshot ID. |
| SHA-256 CAS directory | Immutable artifact bytes referenced by metadata and snapshots. | `authoritative` | #726/#728 enumerate and verify the reachable set, sizes, decoded hashes, pins, and storage generation. The sealed bundle contains the bytes, not just references. |
| Runtime-home ArtifactPlane (`home.redb` plus segment packs) | BLAKE3 chunk metadata, ingest sessions, home objects, and append-only segment bytes ([source](../../../crates/sentinel-fs/src/artifact.rs#L151-L208)). | `authoritative` | #726/#728 bind metadata and segment generation, reject missing chunks, and resolve non-terminal ingest sessions before issuing one sealed generation receipt. |
| `controlplane.redb` | Control-plane analysis and policy state opened separately by the daemon ([source](../../../services/sentinel-daemon/src/orchestrator.rs#L1485-L1500)). | `authoritative` | #728/#729 redb generation receipt; restore before control-plane mutations reopen. |
| `hippocampus.redb` | Episodic/semantic memory and goals opened separately by the daemon ([source](../../../services/sentinel-daemon/src/orchestrator.rs#L1501-L1515)). | `authoritative` | #728/#729 receipt plus #735 event/source frontier; restore cannot advance the frontier past durable memory effects. |
| `evolution.db` | Judge personality-evolution results; the service opens a persistent SQLite store before consuming events ([source](../../../services/sentinel-judge/main.go#L60-L65)). | `authoritative` | #733 durable inbox/outcome plus #736 WAL-aware event-truth cut. A consumer ACK alone is insufficient. |
| `nightrun-jobs.db` | Pending, in-progress, completed, failed, and skipped consolidation work in SQLite WAL ([source](../../../services/sentinel-nightrun/src/job_queue.rs#L73-L104)). | `authoritative` | WAL-aware receipt with run/job counts and effect outcomes. In-progress rows become recoverable or manual-review states under #710; they are never blindly re-executed. |
| `observatory.db` | Optional Gateway SQLite store for run and observation records; enabling Observatory opens it at startup ([source](../../../cmd/cortex-gateway/main.go#L195-L210)) and loads persistent rows into a cache ([source](../../../cmd/cortex-gateway/internal/observatory/sqlite_store.go#L52-L83)). | `authoritative` when enabled | Register an Observatory participant and WAL-aware receipt with run/observation frontier and config hash. If enabled but unregistered, whole-product DR readiness is false. |
| `gaia_console_memory.redb` plus `gaia-memory.md` | The Gaia memory crate explicitly owns a bi-temporal graph and Markdown memory source ([source](../../../crates/sentinel-gaia-memory/src/lib.rs#L1-L17)); its existing bundle exports and restores both ([source](../../../crates/sentinel-gaia-memory/src/backup.rs#L42-L131)). | both `authoritative` | Bind graph and Markdown to one Gaia-memory generation and digest pair. Read-only wake-up/rehydrated views are `derived`; #728/#729 own the redb-consistent capture correction. |
| Gaia Loop `alerts.jsonl`, `state.json`, and session tree | The loop persists event scan state and alert dedupe data ([source](../../../services/sentinel-gaia-loop/src/storage.rs#L15-L83)); session index and per-session records live below the same private tree ([source](../../../services/sentinel-gaia-loop/src/config.rs#L110-L128)). | alerts/state `derived`; completed session audit records `authoritative`; active lock/process files `ephemeral` | Rebuild alerts/state from `events.db` and verify the cursor. Capture allowlisted completed session audit records encrypted and redact content policy; discard active locks. Unknown files fail registry validation rather than being copied recursively. |
| `projection.db` and dashboard event-log CAS plane | CQRS rows are rebuilt from events; the current worker updates the external event-store offset after view work ([source](../../../crates/sentinel-projection/src/worker.rs#L118-L147)). | `derived` | #734 owns blue-green projection generations and local atomic frontiers; #736 binds the active generation to `EventTruthGeneration`. Rebuild and validate before reads; do not restore the current file as authority. |
| `cluster_meta.redb` | Node/owner route, term, and fence state when cluster mode is enabled. | `node_local_authority` | #556 owns cluster recovery. Single-node manifests record it absent/local-only. Disaster restore never promotes stale terms, node identity, or certificates. |
| Company workflow journal | #695 owns customer/agreement/project/work state and an execution outbox; the final durable-execution boundary is refined by #710. | `authoritative` when enabled | #695/#710 expose one digest-bound workflow-generation receipt with authority generation, operation/event/outbox/evidence frontiers, and terminal/unknown effects. R1 consumes that port and creates no second journal. |
| Workbench/runtime intent | #694 owns invocation reservation, results, receipts, and restart recovery; #472/#701 own selected runtime/channel lifecycle. | durable intent `authoritative`; PIDs, pipes, cgroups, sockets, and leases `ephemeral` | Capture only invocation/effect receipts and desired intent. Kill old process trees, invalidate leases, and reconcile normally; never restore handles or process memory. |
| `provision-ops.json` and other node-local saga markers | The daemon opens a separate provisioning journal ([source](../../../services/sentinel-daemon/src/provision_exec.rs#L72-L101)). | `node_local_authority` | Record terminal audit digest and unresolved-operation count, but do not activate old node-local operations on a disaster host. Unresolved operations require outcome probing/manual resolution. |
| Configuration and policy | Agent definitions, topology, model catalog, work/tool profiles, service configuration, and feature enablement are outside WorldSnapshot. | `authoritative desired state` | Capture only allowlisted canonical config bytes with source commit, schema, and semantic digests. Unknown files or incompatible binary/config pairs fail closed. |
| Signed release set | Current deployment manifests provide SHA-256 integrity but the repository ADR says signed provenance is not yet implemented ([source](../../adr/ADR-0397-G9-binary-provenance.md#L15-L33)). | independent `authoritative` release source | #696 must publish binaries, SBOM, provenance, compatibility metadata, and an Ed25519-signed release manifest to an independently durable release registry. Recovery bundles store only the immutable release-set reference and digest. |
| Credentials and recovery trust | Caller/provider credentials, TLS keys, recovery signing keys, revocations, and owner truth live outside WorldSnapshot. | independent `authoritative` trust source | M0 uses one separately administered encrypted recovery escrow. Data bundles contain references only. Current revocation wins, all runtime credentials are reissued, and lost escrow/key/authority keeps restore fenced. |
| Logs, metrics, caches, active locks | Operational observations with no accepted-work authority. | `derived` or `ephemeral` | Recreate after restore. Any audit record required by policy must be promoted to a registered authoritative store rather than silently assumed to exist in logs. |

JetStream has three configured streams, not one undifferentiated cache
([source](../../../pkg/sentinel-go/messaging/streams.go#L11-L79)):

| Stream | Current durability and authority | Purge/recreate/replay contract |
|---|---|---|
| `SENTINEL_EVENTS` | File-backed seven-day mirror of authoritative `events.db`; bridge publication currently uses core `PublishMsg` and marks rows published without a JetStream `PubAck` ([source](../../../services/sentinel-nats-bridge/main.go#L174-L239)). It is `derived`, but its publication frontier is not yet trustworthy. | #733 changes publication to PubAck-backed outcomes. On restore, purge/recreate the exact versioned stream config, replay `EventEnvelopeV2` from the restored `EventTruthGeneration` with stable message IDs, and verify the stream frontier before consumers start. |
| `SENTINEL_JUDGE` | File-backed 30-day alerts/results stream. Judge publishes alerts directly, while the daemon holds a durable explicit-ACK consumer ([source](../../../services/sentinel-daemon/src/nats_consumer.rs#L44-L80)). Until #733 provides a producer outbox and consumer outcome, an unconsumed alert is `authoritative_in_flight`. | A restorable cut requires every message through the cut to have a durable producer intent and permanent consumer outcome or a captured engine-supported NATS generation. The selected M0 target is #733 outbox/inbox ownership; after it lands, purge/recreate and replay unresolved intents only. Before that, coverage fails closed. |
| `SENTINEL_EBPF` | Memory-backed one-day eBPF telemetry ([source](../../../pkg/sentinel-go/messaging/streams.go#L50-L64)). | `ephemeral`: purge/recreate empty, reset consumers, and repopulate from new runtime probes. It never gates data correctness. |

Durable or explicitly ephemeral consumer state is also registered:

| Consumer | Current state | Recovery classification and frontier |
|---|---|---|
| Judge on `SENTINEL_EVENTS` | Durable explicit-ACK consumer; writes `evolution.db` ([source](../../../services/sentinel-judge/internal/service/stream.go#L76-L120)). | Delivery cursor is `derived`; the permanent effect frontier must come from #733 inbox/outcome state in the same generation as `evolution.db`. Recreate at that local durable frontier, never from the server ACK alone. |
| Daemon on `SENTINEL_JUDGE` | Durable explicit-ACK `sentinel-daemon` consumer. | `authoritative_in_flight` today; #733 must bind each alert to one daemon outcome/event before ACK. Restore recreates at the durable outcome frontier. |
| Gaia Loop on `SENTINEL_EVENTS` | Durable `sentinel-gaia-loop` consumer, but startup first scans `events.db` and can continue with scheduled scans when NATS is unavailable ([source](../../../services/sentinel-gaia-loop/src/readiness.rs#L251-L315)). | `derived`; discard server cursor, rebuild local state from EventStore, then create the consumer at the verified local scan frontier. |
| Dashboard on `SENTINEL_EVENTS` | Explicitly ephemeral, `DeliverPolicy::New`, no ACK; current state comes from projections/connect snapshot ([source](../../../services/sentinel-dashboard-backend/src/event_sub.rs#L200-L231)). | `ephemeral`; recreate only after projections are ready. |
| Judge on `SENTINEL_EBPF` | Durable explicit-ACK consumer feeding an in-memory map ([source](../../../services/sentinel-judge/internal/service/ebpf_consumer.go#L81-L123)). | Semantically `ephemeral`; delete/recreate with the stream and wait for fresh telemetry. |

At startup every enabled service must publish a versioned
`DurablePlaneDeclarationV1` containing its participant ID, logical store IDs,
engine, authority class, schema, and allowlisted path key. The coordinator compares
those declarations with the signed service/config catalog. An enabled store,
JetStream stream, durable consumer, or file writer without exactly one registry row
sets typed `RecoveryCoverageIncomplete`, rejects capture, and keeps DR readiness
closed. This is the completeness rule for future service-local stores; a recursive
directory scan is not a substitute. Schema migration, compaction, prune, retention,
and garbage-collection workers are admission classes of their owning participant:
they must be closed and drained too, even when they do not own another data file.

### 3.3 Known claim drift and target architecture

The TOGAF guide describes a complete simulation snapshot and a low-I/O pointer
restore, but the current source has a narrower envelope and sequential cross-store
commit. The guide also distinguishes bounded replay from replay-to-head and records
CAS pinning ([Time Machine section](../../architecture/togaf-architecture-guide.html#timemachine)).
The implementation study therefore treats "complete state" as a target statement,
not proof of whole-product backup.

The relevant ADRs are also target contracts:

- [ADR-0397 G-H2](../../adr/ADR-0397-G-H2-backup-restore.md) requires a cold,
  consistent cut, manifest/digests, stale-trust rejection, and CAS verification. It
  explicitly says the Track-A product cannot claim whole backup/restore yet.
- [ADR-0397 G6/G8](../../adr/ADR-0397-G6-G8-state-durability-recovery-point.md)
  defines RecoveryPoint durability/RPO classes and quorum acceptance for cluster
  failover. Single-node recovery remains `LocalOnly`; this study does not claim
  quorum durability.
- [ADR-0497 G4](../../adr/ADR-0497-G4-snapshot-consistency.md) defines the
  per-container fenced transfer and sealed restore permit. It is reusable
  vocabulary, not a replacement for a product-wide cut.

### 3.4 Incident and owner map

Live issue state was read on 2026-07-29.

| Owner | Live state | Existing responsibility | #722 use, not duplicate |
|---|---|---|---|
| [#250](https://github.com/silentspike/project-sentinel/issues/250) | Closed, verified | World-snapshot tiers, retention, point-in-time restore | Retain as Time Machine owner; add no whole-product claim. |
| [#264](https://github.com/silentspike/project-sentinel/issues/264) | Closed, verified | CAS trash/pins, immutable snapshots, ransomware recovery | Reuse immutability and CAS integrity; offline independent copies remain uncovered. |
| [#481](https://github.com/silentspike/project-sentinel/issues/481) | Closed, verified | System-wide retention and growth controls | Reuse retention bounds; recovery retention must protect in-flight and last-known-good points. |
| [#486](https://github.com/silentspike/project-sentinel/issues/486) | Closed, verified | Snapshot coverage documentation drift | Historical warning that a documented "complete" snapshot can omit state. |
| [#706](https://github.com/silentspike/project-sentinel/issues/706) | Open, in progress | Supervision, dependency-aware readiness, restart budgets, quarantine | Supplies participant crash/restart and readiness semantics; R1 does not create a second supervisor. |
| [#707](https://github.com/silentspike/project-sentinel/issues/707) | Open, in progress | ECS schedule, deterministic barriers, snapshot/replay ordering | Owns the ECS freeze barrier and registered-resource completeness used by the product cut. |
| [#708](https://github.com/silentspike/project-sentinel/issues/708) / [#726](https://github.com/silentspike/project-sentinel/issues/726) | Open, in progress / blocked | Accepted redb/CAS operating design and generation-safe storage epic | Supplies storage-generation vocabulary; #722 coordinates but does not redefine engine backup. |
| [#728](https://github.com/silentspike/project-sentinel/issues/728) | Open, blocked | Versioned metadata-plus-CAS generations, staging, backup/restore, activation | Sole owner of `SealedStoreGenerationReceipt`, engine-consistent storage generations, and activation. R1 consumes its receipt. |
| [#729](https://github.com/silentspike/project-sentinel/issues/729) | Open, blocked | redb policies, integrity, transactions, compaction, deterministic fault harness | Sole owner of redb mechanism choice and proof. Raw open-file copying remains forbidden unless this owner proves it. |
| [#709](https://github.com/silentspike/project-sentinel/issues/709) / [#731](https://github.com/silentspike/project-sentinel/issues/731) | Open, in progress / blocked | Accepted event truth, delivery, CQRS, and generation-safe epic | Supplies `EventTruthGeneration`; #722 consumes it as part of the whole-product manifest. |
| [#732](https://github.com/silentspike/project-sentinel/issues/732) | Open, blocked | Canonical event envelope, append gateway, durability, schema authority | Owns event identity, generation, and durability fields; R1 does not create another event envelope. |
| [#733](https://github.com/silentspike/project-sentinel/issues/733) | Open, blocked | JetStream PubAck outbox and permanent consumer inbox/outcomes | Resolves authoritative in-flight stream state and effect-idempotency gaps required before a rebuild-only NATS contract is valid. |
| [#734](https://github.com/silentspike/project-sentinel/issues/734) | Open, blocked | Projection catalog, poison lane, blue-green generations | Owns projection generation/rebuild/activation and readiness. |
| [#735](https://github.com/silentspike/project-sentinel/issues/735) | Open, blocked | Idempotent durable EpisodeProducer projection | Owns the event-to-Hippocampus effect frontier. |
| [#736](https://github.com/silentspike/project-sentinel/issues/736) | Open, blocked | Consumer catalog, retention frontiers, `EventTruthGeneration`, backup/recovery | Sole owner of WAL-aware event/projection cut and event-retention claims; R1 consumes the receipt. |
| [#710](https://github.com/silentspike/project-sentinel/issues/710) | Open, in progress | Cross-store durable execution, external-effect outcomes, workflow journaling | Owns durable-execution cut/effect semantics. R1 consumes the #695/#710 port and never becomes another workflow engine. |
| [#472](https://github.com/silentspike/project-sentinel/issues/472) / [#701](https://github.com/silentspike/project-sentinel/issues/701) / [#694](https://github.com/silentspike/project-sentinel/issues/694) | Open, review / blocked / in progress | Runtime selection, cancellable channel, durable Workbench intent and receipts | Restore consumes durable intent/outcomes, rejects raw handles, and reconciles through their production lifecycle. |
| [#556](https://github.com/silentspike/project-sentinel/issues/556) | Open, ready | Cluster GA, backup, stale identity/term rejection | Owns cluster cold recovery and N-node claims, not single-node M0 recovery. |
| [#650](https://github.com/silentspike/project-sentinel/issues/650) | Open, blocked | Single-node M0 product acceptance | Owns final runtime acceptance and must acknowledge the M0 recovery class. |
| [#693](https://github.com/silentspike/project-sentinel/issues/693) | Closed, verified | Work-execution contract | Supplies authority/idempotency vocabulary; no new implementation ownership. |
| [#696](https://github.com/silentspike/project-sentinel/issues/696) | Open, ready | QA, signed release/delivery lineage, and rollback | Owns creation and independent publication of the signed release set consumed by recovery. |
| [#695](https://github.com/silentspike/project-sentinel/issues/695) | Open, in progress | Company workflow and journal | Must expose a recovery capture/restore port; this study does not modify its parked work. |
| [#705](https://github.com/silentspike/project-sentinel/issues/705) | Open, blocked | Dependency necessity/ownership | Mandatory gate before restic or any other dependency is introduced. |
| [#656](https://github.com/silentspike/project-sentinel/issues/656) | Open, backlog | Upgrade ownership | Owns future version/pin/update policy for accepted dependencies. |

The ownership split is binding for the proposed work below: R1 owns only product
coordination, coverage, participant receipts, and manifest/envelope authority.
Engine-consistent redb/CAS generations come from #728/#729; event, NATS, consumer,
projection, and frontier generations come from #732-#736; workflow/effect
generations come from #695/#710; runtime intent comes from #472/#701/#694.

## 4. OSS landscape and shortlist

### 4.1 Scoring rubric

Each criterion is scored `0` (poor/absent), `1` (conditional), or `2` (strong):

1. mechanism fit;
2. production maturity;
3. maintenance activity;
4. license fit;
5. documented security posture;
6. language/runtime boundary fit;
7. deterministic and verifiable output;
8. bounded resource model;
9. operational recovery evidence;
10. Sentinel integration cost.

Scores shortlist candidates; they do not select a winner. The shortlist additionally
requires coverage of backup transport, SQLite continuity, orchestration, filesystem
checkpointing, and process checkpointing. Redundant candidates can score well and
still be rejected from deep review.

### 4.2 Candidate inventory

| Candidate | Pin | Score / 20 | Shortlist result | Reproducible reason |
|---|---|---:|---|---|
| restic | `v0.19.1`, `6aa3a516ce654808a1f28f9fa21e9b7c8e6e90bf` | 19 | Deep review | Best boundary fit for encrypted, content-addressed, backend-neutral export and verification after Sentinel seals a bundle. |
| BorgBackup | `1.4.5`, `1b7d3271d2e59e27c61815ff36a29e06a9767e13` | 17 | Landscape rejection | Strong dedup/encryption/check/repair, but overlaps restic and has a less attractive Python/C deployment boundary for this product. Keep as a fallback if #705 rejects restic. |
| Kopia | `v0.23.1`, `72ec08fd8edb86c67ed27099bf1b955e1f308ffa` | 18 | Landscape rejection | Strong snapshot, policy, encryption, and server modes, but overlaps restic and introduces a broader service/policy surface than required for sealed-bundle transport. |
| Litestream | `v0.5.15`, `4e3f0c0f98a8808788c721b3637b41e7f9ce4a9c` | 17 | Deep review | Directly tests SQLite WAL, transaction-chain, gap, retention, disk-full, and atomic restore behavior. |
| Velero | `v1.18.2`, `c253c7fe37d78c9b7e55c68544f7c5b2608712d8` | 14 | Deep review | Kubernetes is not a fit, but its lifecycle, hooks, ordered restore, partial-failure, and finalization contracts are valuable. |
| OpenZFS | `zfs-2.4.3`, `83020cf8259d057d4cc9102010c05f07ffdfc136` | 13 | Deep review | Strong atomic filesystem snapshots, send/receive, checksums, raw encryption streams, and scrub. Platform and license coupling prevent a product requirement. |
| btrfs-progs | `v7.1`, `4ab0e80be9e3bb1db2e6038e6d4316d35fb7ba8b` | 12 | Landscape rejection | Useful snapshots and send/receive but the same substrate limitation as ZFS, with less relevant built-in encryption/integrity coverage for this study. |
| CRIU | `v4.2.1`, `9539417f3e3cfa4eb84c319cd71f4d52f1f08645` | 9 | Deep review as negative control | Only credible candidate here for full Linux process-tree checkpoint/restore; reviewed to decide whether runtime handles belong in DR. |

The exact score vectors below follow the criterion order in section 4.1, so the
totals can be reproduced rather than inferred from the prose:

| Candidate | Fit | Maturity | Maintenance | License | Security | Boundary | Determinism | Resources | Operations | Integration | Total |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| restic | 2 | 2 | 2 | 2 | 1 | 2 | 2 | 2 | 2 | 2 | 19 |
| BorgBackup | 2 | 2 | 2 | 2 | 1 | 1 | 2 | 2 | 2 | 1 | 17 |
| Kopia | 2 | 2 | 2 | 2 | 2 | 2 | 2 | 1 | 2 | 1 | 18 |
| Litestream | 2 | 2 | 2 | 2 | 2 | 1 | 2 | 2 | 1 | 1 | 17 |
| Velero | 1 | 2 | 2 | 2 | 2 | 0 | 1 | 1 | 2 | 1 | 14 |
| OpenZFS | 1 | 2 | 2 | 1 | 1 | 0 | 2 | 2 | 2 | 0 | 13 |
| btrfs-progs | 1 | 2 | 2 | 1 | 1 | 0 | 2 | 2 | 1 | 0 | 12 |
| CRIU | 1 | 2 | 2 | 1 | 0 | 0 | 0 | 1 | 2 | 0 | 9 |

Shortlisting five instead of only the highest-scoring tools prevents a false
comparison of five backup repositories while leaving checkpoint coordination and
runtime state unanswered.

## 5. Pinned upstream deep reviews

### 5.1 restic: sealed-bundle transport, not cut authority

**Provenance and license.** Repository
[`restic/restic`](https://github.com/restic/restic/tree/6aa3a516ce654808a1f28f9fa21e9b7c8e6e90bf)
at `v0.19.1`, commit `6aa3a516ce654808a1f28f9fa21e9b7c8e6e90bf`,
[BSD-2-Clause](https://github.com/restic/restic/blob/6aa3a516ce654808a1f28f9fa21e9b7c8e6e90bf/LICENSE).
The pinned tree has no `SECURITY.md`; vulnerability reporting and supported-version
expectations therefore require an explicit downstream operations policy.

**Source and test findings.**

- [`PlanPrune`](https://github.com/restic/restic/blob/6aa3a516ce654808a1f28f9fa21e9b7c8e6e90bf/internal/repository/prune.go#L95-L135)
  first derives used blobs and a plan. Its
  [`Execute`](https://github.com/restic/restic/blob/6aa3a516ce654808a1f28f9fa21e9b7c8e6e90bf/internal/repository/prune.go#L564-L640)
  separates dry-run, unreferenced deletion, repack, index rewrite, and final deletion.
- [`CheckPack`](https://github.com/restic/restic/blob/6aa3a516ce654808a1f28f9fa21e9b7c8e6e90bf/internal/repository/check.go#L38-L110)
  detects index gaps/overlaps, hashes the pack while reading, decrypts blobs, and
  retries once to distinguish a transient read.
- [`VerifyFiles`](https://github.com/restic/restic/blob/6aa3a516ce654808a1f28f9fa21e9b7c8e6e90bf/internal/restorer/restorer.go#L617-L680)
  verifies restored regular files with bounded workers and stops on error.
- The crypto layer authenticates ciphertext before decryption
  ([source](https://github.com/restic/restic/blob/6aa3a516ce654808a1f28f9fa21e9b7c8e6e90bf/internal/crypto/crypto.go#L275-L315)).
- Integration tests expect a damaged repository check to fail and exercise full
  and subset data verification
  ([test](https://github.com/restic/restic/blob/6aa3a516ce654808a1f28f9fa21e9b7c8e6e90bf/cmd/restic/cmd_check_integration_test.go#L12-L100)).

**Failure, security, and operations.** Destructive maintenance needs an exclusive
repository lock. Prune is multi-phase and can retain excess data safely, but backend
loss, key loss, credential compromise, or an unverified restore still defeats
recovery. Repository encryption protects data from an untrusted storage backend; it
does not protect a live host that holds both repository credentials and deletion
authority. The operating contract must use separate credentials for append/backup
and prune/delete where the backend supports it, offline or object-locked copies,
scheduled `check --read-data` sampling, and restore-to-staging verification.

**Sentinel decision.** `Integrate`, only after #705. Feed restic immutable,
manifest-complete bundles, never live SQLite/redb/CAS paths. Keep repository keys
outside the bundle and outside normal daemon authority.

### 5.2 Litestream: port the SQLite continuity contract

**Provenance and license.** Repository
[`benbjohnson/litestream`](https://github.com/benbjohnson/litestream/tree/4e3f0c0f98a8808788c721b3637b41e7f9ce4a9c)
at `v0.5.15`, commit `4e3f0c0f98a8808788c721b3637b41e7f9ce4a9c`,
[Apache-2.0](https://github.com/benbjohnson/litestream/blob/4e3f0c0f98a8808788c721b3637b41e7f9ce4a9c/LICENSE).
The pinned
[security policy](https://github.com/benbjohnson/litestream/blob/4e3f0c0f98a8808788c721b3637b41e7f9ce4a9c/SECURITY.md)
provides private reporting but no fixed response SLA.

**Source and test findings.**

- Initialization forces WAL, disables SQLite auto-checkpoint, persists the WAL, and
  holds a read transaction to control checkpointing
  ([source](https://github.com/benbjohnson/litestream/blob/4e3f0c0f98a8808788c721b3637b41e7f9ce4a9c/db.go#L997-L1113)).
- Checkpointing serializes with sync, skips safely during a snapshot, copies the WAL
  before checkpoint, and tracks errors and duration
  ([source](https://github.com/benbjohnson/litestream/blob/4e3f0c0f98a8808788c721b3637b41e7f9ce4a9c/db.go#L2358-L2451)).
- Restore computes a contiguous transaction plan, rejects unavailable targets, writes
  a temporary output, syncs it, and only then publishes the database
  ([source](https://github.com/benbjohnson/litestream/blob/4e3f0c0f98a8808788c721b3637b41e7f9ce4a9c/replica.go#L655-L750)).
- The restore planner rejects non-contiguous transaction histories
  ([source](https://github.com/benbjohnson/litestream/blob/4e3f0c0f98a8808788c721b3637b41e7f9ce4a9c/replica.go#L1497-L1610)).
- Incremental follow mode takes an exclusive file lock before applying pages and
  fsyncs before truncation and completion
  ([source](https://github.com/benbjohnson/litestream/blob/4e3f0c0f98a8808788c721b3637b41e7f9ce4a9c/replica.go#L925-L987)).
- Tests inject `ENOSPC` at staging open/write/sync/close, check error metrics and
  retry recovery
  ([test](https://github.com/benbjohnson/litestream/blob/4e3f0c0f98a8808788c721b3637b41e7f9ce4a9c/db_internal_test.go#L1220-L1305)).
  A fuzz test deletes a compacted file and verifies restored SQLite integrity
  ([test](https://github.com/benbjohnson/litestream/blob/4e3f0c0f98a8808788c721b3637b41e7f9ce4a9c/restore_fuzz_test.go#L17-L134)).

**Failure, security, and operations.** A valid LTX chain protects one SQLite
database. It cannot make multiple databases, redb, ECS, or CAS mutually consistent.
Remote backend credentials and encryption remain deployment concerns. Long readers,
disk full, a missing transaction range, incompatible history, retention racing an
in-flight restore, and shutdown sync timeout are explicit operational cases.

**Sentinel decision.** `Port algorithm/contract`. Implement engine-native,
coordinator-controlled SQLite captures with contiguous cursor receipts, temporary
restore, fsync, integrity check, and atomic publication. A direct Litestream
dependency or one daemon per SQLite file is not justified by this study.

### 5.3 Velero: port lifecycle and restore-order contracts

**Provenance and license.** Repository
[`vmware-tanzu/velero`](https://github.com/vmware-tanzu/velero/tree/c253c7fe37d78c9b7e55c68544f7c5b2608712d8)
at `v1.18.2`, commit `c253c7fe37d78c9b7e55c68544f7c5b2608712d8`,
[Apache-2.0](https://github.com/vmware-tanzu/velero/blob/c253c7fe37d78c9b7e55c68544f7c5b2608712d8/LICENSE).
Its pinned
[security policy](https://github.com/vmware-tanzu/velero/blob/c253c7fe37d78c9b7e55c68544f7c5b2608712d8/SECURITY.md)
supports private reporting, documents latest-version support, and explicitly warns
that operators must harden defaults.

**Source and test findings.**

- Backup and restore expose explicit `WaitingForPluginOperations`,
  `Finalizing`, `Completed`, `PartiallyFailed`, and `Failed` phases; a finalizing
  object is not yet usable
  ([backup](https://github.com/vmware-tanzu/velero/blob/c253c7fe37d78c9b7e55c68544f7c5b2608712d8/pkg/apis/velero/v1/backup_types.go#L289-L354),
  [restore](https://github.com/vmware-tanzu/velero/blob/c253c7fe37d78c9b7e55c68544f7c5b2608712d8/pkg/apis/velero/v1/restore_types.go#L254-L307)).
- Hook errors have explicit continue/fail policy, while both outcomes remain visible
  as partial failure
  ([source](https://github.com/vmware-tanzu/velero/blob/c253c7fe37d78c9b7e55c68544f7c5b2608712d8/pkg/apis/velero/v1/backup_types.go#L265-L286)).
- Backup item blocks execute pre-hooks, capture items/volumes, wait for volume
  terminal states, and execute post-hooks
  ([source](https://github.com/vmware-tanzu/velero/blob/c253c7fe37d78c9b7e55c68544f7c5b2608712d8/pkg/backup/backup.go#L785-L910)).
- The default restore order puts definitions, namespaces, storage, authority, secrets,
  config, workloads, and controllers in an explicit sequence
  ([source](https://github.com/vmware-tanzu/velero/blob/c253c7fe37d78c9b7e55c68544f7c5b2608712d8/pkg/cmd/server/config/config.go#L90-L135)).
- Tests record API creation order and reject out-of-order restore
  ([test](https://github.com/vmware-tanzu/velero/blob/c253c7fe37d78c9b7e55c68544f7c5b2608712d8/pkg/restore/restore_test.go#L867-L915)).
- If operation metadata becomes unavailable, the controller persists
  `FinalizingPartiallyFailed` rather than claiming success
  ([source](https://github.com/vmware-tanzu/velero/blob/c253c7fe37d78c9b7e55c68544f7c5b2608712d8/pkg/controller/restore_operations_controller.go#L127-L195)).

**Failure, security, and operations.** Velero demonstrates that a backup object is a
durable state machine, not a tar command. It also demonstrates plugin, RBAC, backend,
hook-command, and partially-failed complexity. Its Kubernetes CRDs, controllers,
plugins, object store assumptions, and cluster resource model do not fit Sentinel's
single-node M0 runtime.

**Sentinel decision.** `Port algorithm/contract`. Use explicit phases, bounded hooks,
ordered resources, finalization receipts, partial failure, and resumable
reconciliation. Do not add Velero or Kubernetes.

### 5.4 OpenZFS: optional substrate, not product contract

**Provenance and license.** Repository
[`openzfs/zfs`](https://github.com/openzfs/zfs/tree/83020cf8259d057d4cc9102010c05f07ffdfc136)
at `zfs-2.4.3`, commit `83020cf8259d057d4cc9102010c05f07ffdfc136`,
[CDDL-1.0 with file-level exceptions](https://github.com/openzfs/zfs/blob/83020cf8259d057d4cc9102010c05f07ffdfc136/LICENSE).
The pinned release tree has no `SECURITY.md`; downstream kernel/package advisories
and distributor support therefore form part of the operations burden.

**Source and test findings.**

- Multi-dataset snapshots in one pool are explicitly all-or-nothing and execute
  check plus sync in one sync task
  ([source](https://github.com/openzfs/zfs/blob/83020cf8259d057d4cc9102010c05f07ffdfc136/module/zfs/dsl_dataset.c#L1905-L1994)).
- Snapshot checks reject duplicate names, inconsistent receive datasets, limit
  violations, and insufficient reserved space
  ([source](https://github.com/openzfs/zfs/blob/83020cf8259d057d4cc9102010c05f07ffdfc136/module/zfs/dsl_dataset.c#L1522-L1578)).
- Receive verifies checksums and returns `ECKSUM`; resumable receive waits for a
  transaction-group sync before preserving resume state
  ([checksum](https://github.com/openzfs/zfs/blob/83020cf8259d057d4cc9102010c05f07ffdfc136/module/zfs/dmu_recv.c#L2840-L2857),
  [resume](https://github.com/openzfs/zfs/blob/83020cf8259d057d4cc9102010c05f07ffdfc136/module/zfs/dmu_recv.c#L2743-L2773)).
- Functional tests corrupt a compressed stream, require a checksum failure, resume,
  and verify final content
  ([test](https://github.com/openzfs/zfs/blob/83020cf8259d057d4cc9102010c05f07ffdfc136/tests/zfs-tests/tests/functional/rsend/send-c_resume.ksh#L22-L50)).
- Raw encrypted send/receive tests keep keys unavailable on the target until an
  explicit load and verify content checksums after mount
  ([test](https://github.com/openzfs/zfs/blob/83020cf8259d057d4cc9102010c05f07ffdfc136/tests/zfs-tests/tests/functional/cli_root/zfs_receive/zfs_receive_raw.ksh#L24-L93)).

**Failure, security, and operations.** ZFS gives a strong atomic filesystem cut only
for datasets in one pool. It does not quiesce application transactions, drain
outboxes, validate Sentinel authority, or define replay. It adds kernel/filesystem
qualification, pool capacity, scrub, snapshot-hold, key, send/receive, privilege,
and disaster-host compatibility duties. A storage snapshot on the same pool is not
an independent backup.

**Sentinel decision.** `Reject` as a required dependency or deployment substrate.
Permit an optional operator hook after the application fence and before release,
label it defense-in-depth, and never make a valid RecoveryPoint depend on ZFS.

### 5.5 CRIU: reject live runtime state as recovery authority

**Provenance and license.** Repository
[`checkpoint-restore/criu`](https://github.com/checkpoint-restore/criu/tree/9539417f3e3cfa4eb84c319cd71f4d52f1f08645)
at `v4.2.1`, commit `9539417f3e3cfa4eb84c319cd71f4d52f1f08645`.
Most code is
[GPL-2.0 and `lib/` is LGPL-2.1](https://github.com/checkpoint-restore/criu/blob/9539417f3e3cfa4eb84c319cd71f4d52f1f08645/COPYING).
The pinned tree has no `SECURITY.md`.

**Source and test findings.**

- Restore validates image inventory, image version, LSM options, CPU compatibility,
  VDSO, TTY, process tree, and other host-sensitive resources before completion
  ([source](https://github.com/checkpoint-restore/criu/blob/9539417f3e3cfa4eb84c319cd71f4d52f1f08645/criu/cr-restore.c#L2368-L2405)).
- Inventory rejects old or corrupt formats and requires compatible dump options
  ([source](https://github.com/checkpoint-restore/criu/blob/9539417f3e3cfa4eb84c319cd71f4d52f1f08645/criu/image.c#L38-L100)).
- A failed late restore kills the created process tree; the source warns that failure
  after network unlock may already have lost connection data
  ([source](https://github.com/checkpoint-restore/criu/blob/9539417f3e3cfa4eb84c319cd71f4d52f1f08645/criu/cr-restore.c#L2230-L2332)).
- External mounts, devices, files, TTYs, and Unix sockets require explicit dump and
  restore handling
  ([operator contract](https://github.com/checkpoint-restore/criu/blob/9539417f3e3cfa4eb84c319cd71f4d52f1f08645/Documentation/criu.txt#L193-L223)).
- Non-root mode still requires `CAP_CHECKPOINT_RESTORE` or broader capabilities and
  has kernel/namespace limitations
  ([security boundary](https://github.com/checkpoint-restore/criu/blob/9539417f3e3cfa4eb84c319cd71f4d52f1f08645/Documentation/criu.txt#L929-L963)).
- The test harness exposes CRIU and preload fault injection
  ([source](https://github.com/checkpoint-restore/criu/blob/9539417f3e3cfa4eb84c319cd71f4d52f1f08645/criu/fault-injection.c#L1-L34),
  [test harness](https://github.com/checkpoint-restore/criu/blob/9539417f3e3cfa4eb84c319cd71f4d52f1f08645/test/zdtm.py#L2790-L2800)).

**Failure, security, and operations.** CRIU couples a recovery point to kernel
features, CPU, LSM, namespaces, cgroups, open files, sockets, devices, and external
resource injection. Restoring provider connections, credentials in process memory,
or an old authority lease is unsafe. The capability boundary conflicts with
Sentinel's least-privilege sandbox posture.

**Sentinel decision.** `Reject`. Persist execution intent, request digests,
idempotency markers, and terminal receipts; kill old process trees; reconstruct
runtime handles through `NanoRuntime` reconciliation. Process memory is never a
disaster-recovery authority.

## 6. Comparison matrices

### 6.1 Mechanism matrix

| Mechanism | Sentinel today | restic | Litestream | Velero | OpenZFS | CRIU |
|---|---|---|---|---|---|---|
| Immutable snapshots, manifests, retention, verification, pruning | WorldSnapshot plus CAS pins and seven-day deletion guard; no product manifest or off-host copy | Strong encrypted content-addressed repository, retention/prune, pack check, restore verify; input consistency is caller-owned | LTX snapshots/levels and protected restore plans for one SQLite DB | Durable backup objects and item status; backend/plugin owns bytes | Immutable dataset snapshots, holds/send, checksums/scrub within a pool | Versioned image inventory; not an immutable backup repository |
| Cross-store quiescence and checkpoint cut | Owner fence and sequential saga for represented world stores; no product-wide writer registry | None; reads supplied paths | Controls WAL/checkpoint for one SQLite DB | Hooks and lifecycle coordinate heterogeneous Kubernetes items, not atomic cross-store commit | Atomic only for selected datasets in one pool; application quiescence external | Freezes a process tree but external stores/resources remain separate |
| Multi-process prepare/drain/receipt protocol | No complete participant registry, admission-close protocol, or durable product-wide receipt exists | None | One SQLite process only | Controller/item phases and hooks demonstrate durable asynchronous preparation, but Kubernetes is the authority | Application processes must be quiesced separately | Freezes one process tree, not independent services or stores |
| Streams, durable consumers, and effect frontiers | Three JetStream streams have different authority classes; PubAck/inbox/outcome and generation-safe frontier work is owned by #733/#736 | Does not model queues | WAL chain is source state, not a message/effect protocol | Backup-item status can preserve lifecycle, not Sentinel consumer outcomes | Dataset snapshot captures broker files only if correctly quiesced | Open sockets/external messages are unsafe restore resources |
| Incremental/deduplicated encrypted remote/offline backup | CAS dedup is local; no complete encrypted remote bundle | Strongest fit after bundle seal; encryption and many backends | Incremental SQLite transaction chain; backend transport varies | Object/PV backup through plugins | Incremental send, raw encrypted send; tied to ZFS | Iterative memory pre-dump; not durable business-data backup |
| Restore order, compatibility, projections, runtime, rollback | Explicit sequential store order, schema checks, projection seed/reset, pre-snapshot rollback, runtime respawn | Restores files and verifies content; application order external | Transaction-order plan, gap rejection, temp output, fsync, atomic rename | Strong explicit priorities, finalization, partial failure, hooks | Receive validates stream and supports resumable transfer; app order external | Extensive host/image compatibility; can restore process tree, but unsafe authority semantics |
| Release, credential, and anti-rollback trust | Current deployment manifest has binary hashes but no signed provenance; credentials/revocation are outside Time Machine | Repository authenticity does not prove application release provenance or current trust | None | Kubernetes identity/RBAC is environment-specific | Encryption keys and pool history do not supply current product revocation truth | Restores sensitive memory and stale credentials, the opposite of the required boundary |
| Drills, corruption injection, RPO/RTO evidence, runbooks | Targeted unit/failure tests; no complete machine-loss drill or measured whole-product RPO/RTO | Repository corruption/check and restore verification tests; operator schedules drills | Disk-full, missing file, chain gap, shutdown, fuzz, and integrity tests | Controller, hook, plugin, ordering, and partial-failure tests | Large functional suite for corrupt/resumable send, raw encryption, scrub | Broad process/resource and fault-injection harness; high environment cost |

### 6.2 Non-functional and integration matrix

| System | Main benefit | Main cost/failure semantics | 1:n and determinism | Security | Maintenance/dependency impact | Expected boundary |
|---|---|---|---|---|---|---|
| Sentinel | Domain authority, owner fencing, ECS semantics, projection reconstruction | Sequential saga can leave intermediate state; coverage and multi-process preparation are incomplete | One coordinator can enumerate N participants/stores; canonical manifests and receipts can be deterministic | Can bind principals, terms, signed releases, secret references, and revocation freshness | New coordinator/protocol and drills, but no mandatory platform dependency | Dedicated recovery coordinator plus versioned participant protocol and owner-supplied generation receipts |
| restic | Encrypted deduplicated remote repository and independent verification | Repository/key/backend loss; prune and credential misuse; no application cut | N sealed bundles to one or more repositories; content IDs deterministic | Authenticated encryption; host-held delete key remains a risk | External binary, backend and upgrade policy; #705/#656 required | Spawn a pinned executable with read-only bundle input and scoped credentials |
| Litestream | Mature WAL/LTX continuity and restore publication contracts | One DB only; gaps, disk full, long readers, retention, shutdown timeouts | One replicator per SQLite DB; ordered TXIDs/checksums | Transport/backend credentials vary; no whole-product trust model | Go service or algorithm port; direct multi-process adoption is costly | Port invariants into a `SqliteCheckpointPort`, do not expose live files to a generic copier |
| Velero | Durable orchestration states, hooks, order, partial failure | Kubernetes/plugin/RBAC complexity and asynchronous partial outcomes | N resources coordinated declaratively; controller replay is idempotent, not byte-deterministic | Powerful hooks/plugins widen attack surface; latest-version support | Adopting it would add an entire platform | Port state-machine vocabulary only |
| OpenZFS | Atomic same-pool filesystem snapshots, integrity, send/receive, scrub | Platform, pool, key, privilege, capacity, and compatibility burden | N datasets in one pool can share a transaction group; cross-pool/app state is external | Native encryption/raw sends; root/storage authority is powerful | Kernel/filesystem operational ownership and CDDL review | Optional operator hook, never product authority |
| CRIU | Full Linux process-tree image and host compatibility validation | External resources, network side effects, privilege, kernel/CPU/LSM coupling; failed restore kills tree | N processes in one tree; image outcome depends on host/kernel state | High capabilities and sensitive memory images | Large C/GPL runtime dependency and kernel qualification matrix | Reject; use durable intent plus runtime reconciliation |

## 7. One decision per Sentinel mechanism

Each mechanism below has exactly one primary decision. Alternatives are recorded to
avoid silently combining incompatible approaches.

| ID | Sentinel mechanism | Decision | Rationale | Rejected alternatives |
|---|---|---|---|---|
| D1 | Whole-product application-consistent cut and validity | **Reimplement minimal** | R1 owns the coordinator, signed coverage/participant registry, prepare/drain receipts, and publish-last envelope. It consumes owner-supplied generations rather than implementing engines. | restic/Velero/ZFS as cut authority; one in-process mutex; filesystem copy while live |
| D2 | Existing world Time Machine and bounded replay | **Keep Sentinel** | It already models domain state, dead branches, projection seed, and runtime reconciliation. | Replace with generic file rollback or CRIU |
| D3 | SQLite event/projection generation | **Port algorithm/contract** | #736 owns the WAL-aware `EventTruthGeneration`; it may port Litestream continuity, gap, temp-file, fsync, and failure invariants. R1 consumes the sealed receipt and cannot create a competing SQLite activation authority. | Direct live-file copy; one Litestream daemon per DB; R1-owned SQLite adapter |
| D4 | redb/CAS store generation | **Keep Sentinel** | #728/#729 exclusively decide and prove the redb mechanism and emit `SealedStoreGenerationReceipt`. R1 records the receipt. Raw copying of an open redb file is forbidden unless the pinned redb contract proves that exact operation. | A vague "short transaction" promise; R1-owned redb adapter; filesystem snapshot as logical consistency |
| D5 | Sealed bundle remote/offline transport | **Integrate** | restic has the best external boundary for encryption, dedup, check, retention, and backend diversity. #705 must approve the dependency and privilege model first. | Embed restic code; make Borg/Kopia simultaneous dependencies |
| D6 | Recovery lifecycle, finalization, partial failure, and ordered resources | **Port algorithm/contract** | Velero's durable phase and ordering model maps well without Kubernetes. | Boolean success flag; unbounded shell hooks; Velero dependency |
| D7 | Host filesystem snapshots | **Reject** | They are neither portable nor application-consistent and cannot be an M0 prerequisite. | Require OpenZFS/btrfs; call a VM snapshot a product backup |
| D8 | Runtime process checkpoint | **Reject** | Old PIDs, sockets, leases, process memory, and credentials must not regain authority after disaster restore. | CRIU restore or microVM-memory restore as canonical state |
| D9 | Projections and NATS delivery state | **Keep Sentinel** | #733/#734/#736 own PubAck, inbox/outcome, consumer, projection-generation, replay, and frontier contracts. R1/R3 consume their receipts, recreate streams, rebuild projections, and verify watermarks. | Restore `projection.db` or broker ACK cursors as authority; skip to `MAX(id)` |
| D10 | Release, credentials, and recovery trust | **Reimplement minimal** | Use one independent trust plane: an encrypted dual-control recovery escrow plus an independently durable Ed25519-signed release registry. The data bundle contains immutable references only; current revocation and anti-rollback catalog win. | Secret bytes in the data bundle; unsigned binaries by digest alone; stale backed-up trust; optional "secret bundle or references" fork |
| D11 | Recovery drills and evidence | **Reimplement minimal** | Sentinel-specific cut, corruption, restore, restart, and business invariants need a first-party harness. | Claim RPO/RTO from upstream or build-server timings |

No dependency is authorized by this decision table. D5 routes through #705 and any
accepted version/update contract routes through #656. D1, D3, D4, and D9 are
composition decisions: #722 must not duplicate the activation or engine authority
already accepted in #728/#729 and #732-#736.

## 8. Whole-product recovery contract

### 8.1 Signed manifest and anti-rollback envelope

The manifest payload uses deterministic CBOR and an explicit schema. A recovery
point is invalid until every owner receipt is present, the bundle is durable, and a
signed envelope is published last.

```text
RecoveryPointManifestV1 {
  schema_version
  recovery_point_id
  recovery_sequence
  scope                         // single_node | cluster
  created_at_utc
  coverage_catalog_digest
  participant_catalog_digest
  organization_generation
  owner_term_or_local_epoch
  fence_generation
  restore_generation
  world_snapshot_id

  release_set {
    registry_id
    release_id
    signed_release_manifest_sha256
    product_commit
    binary_roles[]
    sbom_sha256
    provenance_sha256
    compatibility_profile
  }

  config_set {
    source_commit
    canonical_bundle_sha256
    semantic_digest
    schema_versions[]
  }

  participant_receipts[] {
    participant_id
    protocol_version
    request_digest
    fence_generation
    prepared_state_digest
    source_cursor
    build_digest
    config_digest
    receipt_issuer
    receipt_sha256
  }

  store_generation_receipts[] {
    owner_contract             // #728/#729 or another registered owner
    generation_id
    logical_store_ids[]
    authority_classes[]
    engine_and_schema_digest
    source_cursors[]
    allowlisted_bundle_entries[]
    logical_bytes
    stored_bytes
    content_root
    integrity_receipt_sha256
  }

  event_truth_generation {
    owner_contract             // #736
    generation_id
    durable_upper_position
    outbox_inbox_outcome_cut
    consumer_catalog_digest
    required_consumer_frontiers[]
    projection_generations[]
    receipt_sha256
  }

  workflow_generation {
    owner_contract             // #695/#710
    generation_id
    event_operation_frontier
    execution_effect_frontier
    authority_generation
    receipt_sha256
  }

  cas {
    storage_generation_id
    manifest_sha256
    blob_count
    logical_bytes
    stored_bytes
    every_blob_verified
  }

  runtime_intent {
    owner_contract             // #472/#701/#694
    schema_version
    invocation_effect_frontier
    digest
    no_live_handles
  }

  trust {
    escrow_catalog_id
    escrow_reference_ids[]
    recovery_authority_generation
    revocation_catalog_digest
    secret_bytes_absent
  }

  bundle_sha256
}

RecoveryPointEnvelopeV1 {
  domain_separator             // "project-sentinel/recovery-point/v1"
  canonical_payload_sha256
  payload_length
  anti_rollback {
    catalog_id
    recovery_sequence
    previous_envelope_sha256
    minimum_release_generation
  }
  authentication {
    algorithm                  // Ed25519
    signer_key_id
    authority_generation
    signed_at_utc
    signature
  }
}

RecoveryCopyReceiptV1 {
  domain_separator             // "project-sentinel/recovery-copy-receipt/v1"
  schema_version
  receipt_id
  recovery_point_id
  recovery_envelope_sha256
  repository_id
  backend_type
  object_id
  object_version
  encryption_key_reference
  uploader_principal
  upload_outcome
  uploaded_at_utc
  verifier_principal
  verification_outcome
  verified_at_utc
  logical_bytes
  stored_bytes
  content_root
  retention_policy_id
  immutability_evidence
  previous_receipt_sha256
  supersedes_receipt_id
  authority_generation
  signer_key_id
  signature
}
```

The Ed25519 signature covers the domain separator, payload schema/version, canonical
payload digest and length, catalog ID, recovery sequence, prior-envelope digest,
minimum release generation, and authority generation. Bundle encryption is a
separate confidentiality layer and does not replace this application signature.

The independently administered encrypted recovery escrow contains no product data.
It provides the current `RecoveryAuthorityCatalog`: trusted public keys, revoked
keys/principals, authority generation, highest accepted release generation,
minimum permitted recovery sequence per scope, and accepted envelope-chain heads.
Restore rejects an unknown/revoked signer, stale authority generation, lower
sequence, invalid predecessor, or release below the catalog floor. Selecting an
older point requires an authenticated dual-control catalog transition with an audit
receipt; replacing both a bundle and its plain digest cannot roll the product back.
Loss of the escrow, its unlock material, or the authority catalog is a fail-closed
disaster requiring trust-owner recovery, never a fallback to backed-up credentials.

The signed release registry is the only binary source. A restore fetches the
referenced release set and verifies its Ed25519 manifest, product commit, artifact
hashes, SBOM, provenance, and compatibility profile. Binaries are not assumed
available merely because their digests occur in a data manifest.

`RecoveryPointManifestV1` and `RecoveryPointEnvelopeV1` form one immutable local
seal. They never contain mutable transport state and are never rewritten after
publication. R2 appends independently authenticated `RecoveryCopyReceiptV1`
children. Each receipt signature covers every field above, including the parent
envelope digest, object/version, key reference, principals, outcomes, byte counts,
content root, retention evidence, authority generation, and receipt chain.
Conflicting receipts or an invalid supersession chain are rejected.

Recovery class is a derived view over immutable evidence and current policy:
`SealedLocal` requires a valid local envelope; `VerifiedOffsite` additionally
requires a current successful copy receipt whose repository, key, retention,
immutability, verifier separation, and authority generation satisfy policy. A later
loss/tombstone receipt removes the offsite class and offsite readiness/RPO claim but
does not change the local envelope or its cryptographic integrity. Numeric RPO/RTO,
offline-copy requirements, and drill cadence remain unapproved #650 policy inputs.

### 8.2 Bootstrap journal and durable coordinator state

The recovery coordinator exclusively owns
`/var/lib/sentinel-recovery/control/recovery-journal.sqlite`. The path is compiled
into the service unit/allowlist, not supplied by a capture or restore request. Its
parent is mode `0700`, the file is mode `0600`, and the journal uses versioned
SQLite schema, WAL, `synchronous=FULL`, explicit checkpoint on terminal transitions,
file sync, and parent-directory sync on first creation.

The journal is outside every replaceable product data generation and is never
included recursively in a RecoveryPoint. It stores only operation metadata:
incident/capture ID, request digest, target generation, phase, participant
declarations/receipts, staged entry digests, envelope digest, attempts, deadlines,
typed failures, operator resolutions, and release/abort acknowledgements. Customer
payloads and secret values are prohibited.

Journal schema version 1 has exactly three logical tables: `operations` holds one
row per capture/restore request and its current state, `participant_receipts` is
keyed by operation/participant/fence generation and stores the immutable request
and receipt digests, and `transitions` is an append-only sequence of state,
decision, error, and acknowledgement records. A `journal_metadata` singleton binds
schema version, coordinator identity, and highest accepted fence generation.
Foreign keys and uniqueness constraints reject a receipt from another operation or
generation. Every state transition and receipt insertion is one transaction; a
terminal checkpoint plus file and directory sync precedes any external
Release/Abort acknowledgement.

```text
Idle
  -> PrepareRequested
  -> PreparingParticipants
  -> Draining
  -> Prepared
  -> CapturingOwnerGenerations
  -> Validating
  -> Sealing
  -> SealedLocal
  -> ReleaseDecided
  -> ValidationOnly
  -> ReleaseAcknowledged
  -> ReadinessCASCommitted
  -> Ready

Any pre-seal phase -> Aborting -> Aborted | ManualRecoveryRequired
Any post-seal validation failure -> Quarantined
Any missing/conflicting release acknowledgement -> ManualRecoveryRequired

CopyIdle
  -> Uploading
  -> Uploaded
  -> IndependentlyVerifying
  -> CopyReceiptAppended
  -> CopyClassRecomputed

Any copy failure/loss -> CopyFailureReceiptAppended -> CopyClassRecomputed
```

On total host loss, an authenticated recovery operator may create a new empty
journal at the same path from the binary's embedded journal schema. The first row is
`BootstrapRestore`, bound to the verified envelope digest, recovery sequence,
release-set identity, authority catalog, and incident authorization. The journal
does not need its lost predecessor to validate a sealed point; it needs the
independently surviving signed envelope and authority catalog. Unsealed staging from
the lost host is never inferred as complete.

### 8.3 Versioned multi-process participant protocol

The product fence is a protocol, not a shared mutex:

```text
PrepareCaptureV1 {
  capture_id, request_digest, fence_generation, candidate_cut, deadline
}
DrainV1 {
  capture_id, request_digest, fence_generation, target_frontiers
}
PreparedReceiptV1 {
  participant_id, protocol_version, request_digest, fence_generation,
  admission_closed_at, source_cursor, in_flight_counts,
  unresolved_outcomes, local_generation, build_digest, config_digest,
  prepared_state_digest, issuer, receipt_digest
}
ReleaseV1 | AbortV1 {
  capture_id, request_digest, fence_generation, coordinator_decision_digest
}
```

Each participant durably records the highest accepted fence generation and local
prepare state before returning `PreparedReceiptV1`. Messages use mutually
authenticated service identities; the coordinator verifies participant identity
against the signed catalog and stores the receipt. A repeated request with the same
digest returns the same outcome. The same capture ID with another digest, a stale
generation, unknown participant, or mismatched build/config digest is rejected.

Prepare closes admission first, then drains already admitted work. There is no TTL
that silently reopens writes: after coordinator or participant crash, startup
readiness remains closed until the same coordinator generation resumes or durably
aborts. A timeout moves the coordinator to `Aborting`; failure to collect every
abort acknowledgement becomes `ManualRecoveryRequired`. Only the coordinator may
issue Release/Abort, and participants reject decisions from any other or older
generation.

The required participant and dependency registry is:

| Order and participant | Current writer/source evidence | Prepare and drain responsibility | Prepared receipt dependency |
|---|---|---|---|
| 0. `recovery-coordinator` | New R1 owner; no product-wide coordinator exists today. | Persist request, validate signed catalogs, assign generation, collect receipts, authorize capture, and alone release/abort. | Journal durable before any participant message. |
| 1. `gateway-admission` | Provider calls start in the Gateway pipeline ([source](../../../cmd/cortex-gateway/internal/proxy/pipeline.go#L685-L700)); in-flight tracking is configured separately ([source](../../../cmd/cortex-gateway/main.go#L244-L255)). | Reject new billable/provider work, drain admitted calls to durable result/outcome, and freeze Observatory writes. | Coordinator prepared; receipt includes active call count, outcome frontier, and Observatory cursor. |
| 1. `workflow-api-dispatcher` | #695 owns customer/governance mutations and execution outbox; #710 owns cross-store effects. | Reject new mutations/dispatch, finish current local transaction, and durably classify pending/unknown effects. | Coordinator prepared; #695/#710 generation available. |
| 1. `workbench-runtime-dispatcher` | #694 owns invocation reservation/results; #472/#701 own runtime launch/channel. | Stop new launches, cancel or durably classify executing invocations, preserve receipts, and expose no raw handle. | Workflow admission closed; durable intent/effect frontier available. |
| 1. `nightrun` | Nightrun opens EventStore and its job queue, then resumes incomplete runs ([source](../../../services/sentinel-nightrun/src/main.rs#L224-L257)). | Stop scheduling/new jobs; finish or classify each in-progress job without repeating an unknown effect. | Gateway/workflow/workbench admission closed. |
| 1. `daemon-ecs` | The daemon owns the tick/world and separately opened state, control-plane, hippocampus, CAS, and FS stores ([source](../../../services/sentinel-daemon/src/orchestrator.rs#L1296-L1321)). | Finish the admitted tick, close mutation channels, apply #707 freeze barrier, and record tick/world/resource digest. | External admissions closed; nightrun prepared. |
| 2. `eventstore-outbox` | Limbo commits event plus outbox atomically. | Close event append after upstream producers prepare; expose candidate `EventTruthGeneration` and pending intents. | Daemon/workflow/nightrun producers prepared. |
| 2. `nats-bridge` | Bridge polls outbox and publishes to JetStream ([source](../../../services/sentinel-nats-bridge/main.go#L177-L239)). | Under #733, publish through target frontier to PubAck or quarantine; accept no later outbox row. | EventStore candidate frontier fixed. |
| 2. `judge-events` | Judge durable consumer writes evolution and publishes alerts. | Consume through `SENTINEL_EVENTS` target, commit inbox/evolution/outbox outcome, and expose local outcome frontier. | NATS bridge PubAck frontier fixed. |
| 2. `daemon-judge` | Daemon durable consumer receives Judge alerts. | Consume through Judge target and bind each effect to durable inbox/event outcome. | Judge producer intent frontier fixed. |
| 2. `gaia-events` | Gaia scans EventStore and also uses a durable NATS consumer. | Scan through event target, persist local cursor/alerts, and expose matching rebuild frontier. | EventStore target fixed. |
| 2. `dashboard-events` | Dashboard uses an ephemeral new-only consumer. | Disconnect and discard ephemeral state; it cannot block the cut. | Admission closed; no authoritative receipt fields. |
| 2. `judge-ebpf` | eBPF stream feeds an in-memory Judge map. | Stop consumer and discard ephemeral cursor/map. | No authoritative dependency. |
| 3. `projection` | Projection worker reads EventStore and writes read models before updating the external offset. | Stop rebuild/prune, catch the candidate event frontier under #734, validate candidate generation, and expose local frontier. | Event and all required effect outcomes stable. |
| 3. `enabled-store:<logical_id>` | One participant per registered redb, SQLite, CAS/ArtifactPlane, Gaia, workflow, config, release-ref, or node-local store in section 3.2. | Stop its own writer, emit the owner-defined sealed/rebuild/reconcile receipt, and reject unregistered files. | Owning writer participant prepared; #728/#729, #736, or #695/#710 contract as applicable. |

Event consumers can create new local events/effects, so order 2 is a bounded
fixed-point drain: after one bridge/consumer pass, the coordinator rereads event,
outbox, inbox, outcome, and consumer frontiers. If any frontier moved, it repeats
the wave under closed admission. Success requires two identical consecutive
frontier vectors and zero unclassified in-flight effects. The configured deadline
and maximum waves bound the process; non-convergence aborts rather than producing a
cut.

Release is dependency-reversed after `SealedLocal` or a durable abort. Store
adapters and projections may enter only `ValidationOnly`: read-only opens, integrity
checks, generation probes, and projection comparison are allowed under the same
fence, but normal writers remain closed. No tick, store mutation, outbox publish,
consumer effect, runtime launch, workflow mutation, customer request, or provider
call reopens in stages.

The coordinator first commits one durable `ReleaseV1` decision for the fence
generation. Every required participant must persist and acknowledge that exact
decision digest while remaining fenced. After all generation/probe results and ACKs
match, the coordinator performs one final compare-and-swap from
`ReleaseAcknowledged` to `ReadinessCASCommitted`. That CAS is the sole permission
for participants to reopen normal admission. A crash resumes the same fenced
decision. Missing, stale, or conflicting ACKs remain fail-closed and move to
`ManualRecoveryRequired`; no partial release is inferred from process liveness.

### 8.4 Application-consistent capture

The cut is a bounded fenced saga, not distributed 2PC:

1. Persist `PrepareRequested` in the bootstrap journal and verify the signed
   coverage, participant, release, and recovery-authority catalogs.
2. Reject the request unless every enabled durable plane and participant from
   section 3.2 has exactly one owner and a compatible protocol.
3. Run `PrepareCaptureV1` in dependency order. Every participant closes admission
   at its real process boundary and durably records the fence generation.
4. Run the fixed-point `DrainV1` waves. Pending work is allowed only when a durable
   owner receipt proves its idempotent continuation or manual-resolution state.
   No direct provider call, Judge alert, workbench effect, NATS message, or claimed
   action may remain the sole copy of an outcome.
5. Require `PreparedReceiptV1` from every required participant. Bind their build,
   config, authority, event, workflow, runtime, and store frontier vector into the
   journal.
6. Create WorldSnapshot under the #707 ECS barrier and verify its CAS pins before
   using its ID.
7. Ask #728/#729 owners for sealed storage-generation receipts and #736 for the
   WAL-aware `EventTruthGeneration` receipt. R1 never reads an open redb/SQLite file
   as a shortcut.
8. Ask #695/#710 for the workflow/effect generation and #694 for durable runtime
   intent. Verify no external effect can be replayed merely by restoring local state.
9. Enumerate and hash every reachable SHA-256 CAS blob and ArtifactPlane chunk/pack
   from the storage receipts. Pin the exact set and reject missing, corrupt, mixed
   profile, or extra executable content.
10. Capture both Gaia authoritative files in one generation; capture registered
    Observatory, evolution, nightrun, control-plane, hippocampus, completed Gaia
    session audit, and allowlisted config receipts. Record node-local journals only
    as reconcile/manual-resolution state.
11. Bind the independently signed release-set reference and recovery-escrow
    catalog/revocation digests. No binary or secret availability is inferred from a
    public hash.
12. Fsync every staged file and directory, run owner integrity/open checks, verify
    all cross-store cursors/references, and compute the immutable bundle digest.
13. Encode `RecoveryPointManifestV1`, then write/sign
    `RecoveryPointEnvelopeV1` through temporary file, file sync, rename, and parent
    sync. The authenticated envelope is the publish-last marker.
14. Record the immutable local seal, commit one dependency-reversed `ReleaseV1`,
    admit only `ValidationOnly`, collect every matching durable ACK, validate all
    generations/probes, and commit the final readiness CAS before any normal writer
    reopens.
15. R2 may later export only the immutable sealed directory. Upload and independent
    verification append `RecoveryCopyReceiptV1`; they never edit the local manifest.
    Recovery class and offsite readiness are recomputed from valid receipts and
    current policy.

If any pre-seal step fails, no recovery point is advertised. Immutable partial
artifacts are journaled for later bounded cleanup. A pin leak or undeleted staging
tree is visible maintenance debt, never permission to claim success.

### 8.5 Restore order and rollback

A disaster restore never mutates the only remaining source bundle:

1. Authenticate a recovery incident through the independent escrow/catalog and
   bootstrap the fixed recovery journal if the host was lost.
2. Fetch the selected point into quarantine. Verify the envelope signature,
   authority generation, revocations, recovery sequence, predecessor chain, release
   floor, payload digest/length, and bundle digest before any product mutation.
3. Fetch the exact signed release set from the independent release registry. Verify
   Ed25519 provenance, product commit, every binary, SBOM, compatibility profile,
   and config schema. Registry/escrow loss keeps the system fenced.
4. Start all product services in restore-only mode with admission disabled. Validate
   the participant and coverage catalogs against the manifest.
5. Restore and verify CAS/ArtifactPlane bytes into an inactive storage generation,
   then stage the #728 redb/CAS generation. Restore `events.db` through #736's
   WAL-aware generation contract; never reset a frontier to `MAX(id)`.
6. Stage all other authoritative stores: control plane, hippocampus, evolution,
   nightrun, Observatory when enabled, both Gaia memory files, completed Gaia audit
   records, and #695/#710 workflow/effect state. Apply only owner-approved migrations.
7. Verify organization/owner/assignment generations, workflow and workbench
   outcomes, event/outbox/inbox frontiers, CAS ownership, WorldSnapshot, Gaia pair,
   config semantics, and every participant receipt. External facts reconcile
   forward from receipts/probes; Time Machine never rewinds them.
8. Activate the one #728 storage generation under the restore journal while
   retaining the old local generation. Node-local terms, provision operations,
   locks, sessions, and runtime handles remain invalid and require reconciliation.
9. Purge/recreate the versioned JetStream definitions. Replay
   `SENTINEL_EVENTS` from `EventTruthGeneration` with stable IDs and PubAck; replay
   only unresolved durable Judge intents after #733 outcome checks; recreate eBPF
   empty. Recreate durable consumers at their local outcome frontiers.
10. Build and validate #734 projection generations beside the old ones, verify every
    required consumer/projection watermark and poison lane, then atomically activate
    the read generation.
11. Kill any surviving old process tree. Reconcile desired runtime intent through
    #472/#701/#694, reissue all service/provider/TLS credentials from current trust,
    and reject stale leases or caller tokens.
12. Enter `ValidationOnly` for staged stores and projections while every normal
    writer, tick, publisher, effect consumer, runtime, workflow, customer, and
    provider admission remains fenced. Positive and negative probes must match the
    manifest generation without producing normal effects.
13. Record and sign a restore receipt with envelope/release IDs, activated
    generations, consumer/projection frontiers, credential generation, unresolved
    manual work, and invariant results. Commit one durable release decision, collect
    the same decision ACK from every participant, and then perform one final
    readiness CAS. Only that CAS reopens normal admission. Advance the independent
    anti-rollback catalog only after this verification.

On failure, stop and keep the fence. If the old local generation is intact, the
journal may switch back only to that complete verified generation and rerun all
checks. Otherwise select another envelope allowed by the current anti-rollback
catalog. A mixed old/new store set, unsigned binary, stale trust source, missing
participant receipt, or unavailable outcome authority always remains fenced.

## 9. RPO, RTO, drills, and failure injection

The following numbers are proposal inputs for measurement and a separate product
policy decision. They are not approved SLOs and must not become release gates until
the named owner accepts them:

| Class | Purpose | Required recovery point | Proposed RPO | Proposed RTO | Approval state and M0 class |
|---|---|---|---|---|---|
| `TM_LOCAL` | Operator Time Machine rollback on an intact host | Valid WorldSnapshot and CAS pins | Snapshot interval; no disk-loss claim | Existing in-process target only after current-head measurement | Unapproved measurement target; `M0_HARDENING` mechanics owned by #250/#264 |
| `M0_SINGLE_NODE_DR` | Loss/corruption of the product data directory holding authoritative customer work | Immutable signed local envelope plus a current valid `RecoveryCopyReceiptV1` and independent release/trust inputs | At most 15 minutes | At most 60 minutes | Unapproved policy proposal; #650/product owner must decide separately from D1-D11 |
| `OFFLINE_SECURITY` | Ransomware/operator credential compromise | Independently administered immutable/offline copy plus surviving escrow/catalog | At most 24 hours | At most 4 hours | Unapproved policy proposal; `M0_HARDENING` mechanism for production customer work |
| `CLUSTER_RECOVERY` | Node loss, quorum recovery, geo recovery | Quorum-accepted RecoveryPoint and stale-term/cert rejection | Defined by #556 | Defined by #556 | Not decided here; `POST_M0` for the single-node product |

Approving D1-D11 does not approve `15 min`, `60 min`, or any schedule above. A
lower RPO increases capture frequency, remote bandwidth, retained objects, signing
operations, and restore-plan complexity.

Proposed drill inputs for separate #650/product-owner approval:

- every RecoveryPoint: canonical manifest, all file hashes, all CAS references,
  engine open/integrity checks, cursor coherence;
- daily: restore a sampled SQLite/redb/CAS subset to staging and verify no production
  path is touched;
- weekly: full isolated whole-product restore with mock/local-loop external services,
  projection rebuild, runtime reconciliation, negative authorization tests, and
  business invariant readback;
- before M0 production acceptance and after schema/key/topology changes: destructive
  single-node disaster drill on the explicitly authorized target with issue-specific
  rollback protection;
- quarterly after M0: offline-copy restore by an operator who did not create the
  backup, including lost-primary-key and revoked-principal scenarios.

Failure-injection matrix:

| Injection | Required result |
|---|---|
| Crash after fence, before first capture | Restart finds `PreparingParticipants` or `Prepared`, aborts or resumes deterministically, and releases no false recovery point. |
| Coordinator crash after one participant prepares | Prepared participants remain admission-closed, reject stale generations, and return the same receipt after restart; no TTL reopens them. |
| Participant crash or timeout during drain | Participant restarts closed and reconciles its local marker; coordinator aborts on deadline and requires every abort ACK or enters manual recovery. |
| Consumer drain creates another event/effect | Fixed-point vector changes and another bounded wave runs; non-convergence aborts. |
| Crash after any store capture | Completed receipt is reused only if request digest and source cursor match; otherwise quarantine. |
| Crash after WorldSnapshot save, before CAS pin | Recovery journal completes pins or invalidates the point; no restorable manifest exists. |
| SQLite WAL changes or chain gap | Capture/restore fails with typed cursor/gap error. |
| Raw copy attempted on an open redb store | Owner adapter rejects it unless #728/#729's pinned engine contract explicitly proves that operation. |
| Disk full on write, fsync, rename, or manifest publish | No sealed point; old points remain untouched; health reports the phase. |
| Missing/corrupt CAS blob | Seal and restore both fail before mutation/readiness. |
| redb/SQLite schema incompatibility | Fail before store replacement unless a pinned crash-safe migration exists. |
| Projection rebuild crash | Resume idempotently from authoritative cursor; mutation remains fenced until watermarks pass. |
| JetStream stream/consumer data lost | Recreate from `EventTruthGeneration` and permanent outcomes; if an authoritative in-flight Judge effect lacks them, the point is not restorable. |
| Workflow evidence/assignment authority mismatch | Item remains blocked; no completion or duplicate action. |
| Runtime crash during restore | Old process tree is killed/reaped; desired intent is reconciled once by idempotency key. |
| Crash after durable Release but before all participant ACKs | Restart remains fenced, resumes the same decision digest, and cannot reopen any normal writer. |
| One participant ACKs another Release digest or generation | Coordinator enters `ManualRecoveryRequired`; every participant remains fenced and the final readiness CAS fails. |
| Crash immediately before or after final readiness CAS | CAS is idempotent and generation-bound; before it no normal admission opens, after it all participants observe the same committed generation. |
| Envelope/digest pair replaced with an older valid bundle | Independent catalog rejects recovery sequence, predecessor, authority, or release generation rollback. |
| Copy receipt forged, replayed, cross-parent, or signed by a stale authority | Derived offsite class remains false; the immutable local envelope is unchanged. |
| Remote object is lost after a valid copy receipt | An authenticated append-only loss/supersession receipt removes offsite readiness/RPO without rewriting or invalidating the local seal. |
| Release registry unavailable, artifact unsigned, or SBOM/provenance mismatch | Restore stays fenced; a digest-only local binary is not accepted. |
| Recovery escrow missing, unlock material lost, or backed-up principal revoked | Restore stays fenced; trust owner must recover the independent escrow/catalog and reissue credentials. |
| Remote repository unavailable | `SealedLocal` can remain valid locally but does not satisfy an offsite RPO class. |
| Backup/prune credential compromise | Immutable/offline copy survives; delete capability cannot silently erase every recovery generation. |

Issue-specific implementation benchmarks must measure capture pause, total seal,
export, verify, restore, projection rebuild, and readiness latency with sidecars on
the declared runtime target. This research defines no benchmark result.

## 10. M0 classification

| Finding | Class | Evidence and owner |
|---|---|---|
| WorldSnapshot is not a whole-product recovery point | `M0_HARDENING` | Cross-store table above; proposed coordinator contract, final acceptance #650 |
| CAS references are retained but blob bytes have no complete sealed/offsite bundle | `M0_HARDENING` | `FsMetadataDump` contains references only; #264 plus proposed bundle contract |
| Control-plane, memory, evolution, nightrun, Observatory, Gaia Loop audit, config, and trust are outside Time Machine | `M0_HARDENING` | Complete section 3.2 registry; R0/R1 plus existing engine owners |
| `SENTINEL_JUDGE` can hold the only in-flight effect and current delivery lacks permanent PubAck/inbox outcomes | `M0_HARDENING` | Current bridge/consumer source; #733/#736 |
| No executable multi-process prepare/drain/release protocol exists | `M0_HARDENING` | Current writers span Gateway, daemon, projection, NATS, Judge, Nightrun, and future workflow; R1 composes #706/#707/#710 |
| Workflow journal/effect cut needs recovery integration | `M0_HARDENING` | #695 owns the journal and #710 owns cross-store effects; R1 consumes their port instead of creating a journal |
| Engine-consistent redb/CAS/SQLite generations are not R1 work | `M0_HARDENING` | Accepted #726/#728/#729 and #731/#736 contracts; R1 waits for sealed receipts |
| Projection file should remain derived and generation-bound | `M0_HARDENING` | Current worker offset boundary; #734/#736 own local frontier and activation |
| Manifest authentication, release availability, and anti-rollback need independent authorities | `M0_HARDENING` | Current binary manifest is digest-only; D10/R0 define signed registry plus encrypted recovery escrow |
| Stale runtime handles and credentials must not be restored | `M0_HARDENING` | Existing teardown/respawn and CRIU negative review; #472/#701/#694 and D10 |
| Numeric `15 min`/`60 min` targets | `M0_HARDENING` policy proposal, not approved | Measurement proposal only; #650/product-owner decision is separate from mechanism approval |
| Mandatory ZFS/btrfs substrate | `POST_M0` | Optional defense only; product portability wins |
| CRIU/live process memory restore | `POST_M0` and rejected | Security and compatibility costs exceed value |
| Multi-node/quorum/geo recovery | `POST_M0` for single-node acceptance | #556 and G6/G8 own cluster durability |

No finding is marked `BLOCKS_M0` merely because an upstream has a feature Sentinel
lacks. The listed hardening contracts must be owned and proved before a whole-product
DR claim. #650 must separately decide whether the proposed
`M0_SINGLE_NODE_DR` numbers become binding before production acceptance.

## 11. Ordered implementation issue contracts

ORC approved D1-D11 and authorized one acyclic M0 whole-product-recovery epic.
The implementation order is strict:

```text
R0 independent recovery authority and signed-release retrieval
  -> R1 local RecoveryPoint coordinator and immutable seal
  -> R2 immutable offsite copy transport and verification
  -> R3 whole-product restore runner and drills
  -> R4 coverage, retention, and health
  -> #650 product acceptance and separate policy decisions
```

R0-R4 are the only new implementation children. Existing engine, event, workflow,
runtime, release-production, retention, and supervision owners receive narrow
reciprocal deltas instead of parallel authorities. #650 is downstream acceptance
and numeric-policy ownership, never a prerequisite for implementing R0-R4.

### R0. Independent recovery authority and signed-release retrieval

**Class and target:** `M0_HARDENING`; code/fake gates use `NONE`, final integration
and benchmarks use `SINGLE_NODE` only after separately authorized runtime work.

**Depends on:** #696's versioned signed-release publication port and an assigned
security/operations authority. R0 has no dependency on R1-R4, #650 acceptance, a
product-data bundle, or the restic decision.

**Scope:** `RecoveryAuthorityPortV1`, current signer/revocation/anti-rollback
catalog, dual-control escrow, key ceremony, break-glass/audit, and
`SignedReleaseRetrievalPortV1`. #696 produces signed releases; R0 retrieves and
validates them. The versioned port breaks any #696/#722 staging cycle.

**Negative/failure contract:** unknown/revoked/stale signer, rollback catalog,
missing escrow quorum, lost key/authority, unsigned artifact, wrong SBOM/provenance,
or incompatible release remains fail-closed. Restart between catalog/escrow/release
steps cannot weaken trust. Fakes are deterministic and hold no production secret.

**Delivery:** default off; token-free conformance first; rollout to read-only trust
verification before restore authority. Rollback disables retrieval without trusting
backed-up credentials. Benchmarks measure catalog/release verification on the
declared target, never build time. TOGAF target delta defines the independent
recovery authority and release source, but this issue does not edit TOGAF.

### R1. Local RecoveryPoint coordinator and immutable seal

**Class and target:** `M0_HARDENING`; `SINGLE_NODE` for final capture/protocol
validation and pause/seal benchmarks.

**Depends on:** R0 interfaces; #706 supervision; #707 ECS barrier; #728/#729 storage
generation; #732-#736 event/delivery/projection generation; #695/#710
workflow/effect receipt; and #472/#701/#694 runtime intent. It does not depend on R2
transport or #650 acceptance.

**Scope:** coverage/participant catalogs, fixed bootstrap journal,
`PrepareCaptureV1`/`DrainV1`/`PreparedReceiptV1`, immutable
`RecoveryPointManifestV1`/`RecoveryPointEnvelopeV1`, one durable Release/Abort
decision, `ValidationOnly`, participant ACK collection, and final readiness CAS.
R1 consumes owner ports and never selects engine copy/activation mechanisms.

**Negative/failure contract:** uncovered plane, arbitrary path, secret byte, stale
generation, receipt mismatch, one-process mutex, fixed-point non-convergence,
disk/fsync/rename failure, coordinator/participant crash, missing/conflicting
Release ACK, or restart during release stays fenced. No normal writer, tick, effect,
runtime, workflow, customer, or provider admission opens before the final CAS.

**Delivery:** default off -> catalog observation -> shadow prepare/abort -> local
seal -> authorized scheduling. Rollback disables new captures and durably
aborts/releases the active generation while retaining prior points. Benchmarks
measure pause/seal, participant drain, bytes, and CPU/IO sidecars. TOGAF target delta
defines WorldSnapshot versus owner generations, product cut, local envelope, and
readiness CAS.

### R2. Immutable offsite copy transport and verification

**Class and target:** `M0_HARDENING`; `SINGLE_NODE` for final copy/verification
integration and declared-target measurements.

**Depends on:** R1 immutable local envelope; #705 exact restic dependency/privilege
decision; #656 version/update ownership; and R0 authority/key interfaces. It does
not mutate R1 artifacts.

**Scope:** read-only sealed-directory upload, independent verification, separated
upload/delete/verifier principals, append-only signed `RecoveryCopyReceiptV1`,
retention/immutability evidence, supersession/tombstone chain, and derived recovery
class.

**Negative/failure contract:** live-store input, manifest rewrite, forged/replayed
receipt, wrong parent envelope, backend/object/key mismatch, same uploader/verifier
where policy forbids it, corrupt pack, rollback, retention race, lost remote object,
or delete-credential compromise cannot yield `VerifiedOffsite`. Remote loss removes
offsite readiness/RPO only; local cryptographic integrity remains valid.

**Delivery:** local mock -> read-only shadow upload -> independent verify -> policy
gating only after #650 policy approval. Rollback removes transport while preserving
local envelopes and append-only loss receipts. Benchmarks measure upload, verify,
retrieve, dedup, bytes, and sidecars. TOGAF target delta separates immutable local
seal, immutable copy receipts, and derived class.

### R3. Whole-product restore runner and disaster drills

**Class and target:** `M0_HARDENING`; `SINGLE_NODE` on an explicitly authorized
isolated target for destructive restore and drill evidence.

**Depends on:** R0, R1, and R2 plus complete owner-generation ports from #728/#729,
#732-#736, #695/#710, #472/#701/#694, and signed releases from #696 through R0.

**Scope:** quarantine, envelope/copy/release verification, staged generation
restore, JetStream recreation, projection rebuild, credential reissue, runtime
reconciliation, `ValidationOnly` probes, one Release decision, all ACKs, final
readiness CAS, signed restore receipt, and drills.

**Negative/failure contract:** in-place first mutation, mixed generation, invalid or
lost copy, stale trust, unsigned/incompatible release, missing CAS/evidence,
frontier rewind, stale runtime handle, duplicate effect, crash at every transition,
or missing/conflicting ACK remains fenced or `ManualRecoveryRequired`.

**Delivery:** dry validation -> staged restore -> authorized full drill. Rollback
switches only to an intact verified generation and reruns the same release protocol.
Benchmarks measure quarantine-to-ready, projection rebuild, reconciliation, and
p50/p95/max with sidecars; proposed RPO/RTO values are not pass thresholds until
#650 approves them. TOGAF target delta defines restore order and final readiness.

### R4. Recovery coverage, retention, and health

**Class and target:** `M0_HARDENING`; `SINGLE_NODE` for final health/retention soak.

**Depends on:** R0-R3 as applicable and owner deltas in #250/#264/#481/#736.
#650 supplies downstream policy values but does not block structural health work.

**Scope:** exact durable-plane coverage, local-seal/copy-receipt/drill age,
participant/frontier health, retention and pin protection, repository/release/escrow
health, derived recovery class, typed operator status, and alerts.

**Negative/failure contract:** transport success alone, stale/forged receipt,
unregistered plane, unknown consumer, missing trust/release authority, lost remote
copy, pin leak, disk pressure, or missed proposed target cannot report green. A
policy change recomputes class/readiness without rewriting evidence.

**Delivery:** observe-only -> alert -> readiness policy after owner acceptance.
Rollback returns to observe-only without deleting immutable evidence. Benchmarks
measure health cost, retention/prune, disk growth, and outage detection on the
declared target. TOGAF target delta defines coverage, receipt-derived class,
retention frontier, and policy status.

### Approved owner-resolution and materialization rule

1. Create one recovery epic with exactly R0-R4 as ordered children.
2. Add reciprocal, versioned-port deltas to #706/#707, #728/#729, #732-#736,
   #695/#710, #472/#701/#694, #696, #250/#264/#481, #705/#656, and #650.
3. Stage #696 release production and R0 retrieval through
   `SignedReleaseRetrievalPortV1`; neither implementation waits on the other.
4. Keep engine generation/activation in #728/#729/#736, workflow/effects in
   #695/#710, runtime lifecycle in #472/#701/#694, and release production in #696.
5. Require runtime target, ACs, negative/failure criteria, benchmarks, rollout,
   rollback, claim boundary, TOGAF target delta, reciprocal links, final labels, and
   a fresh Issue Quality Gate PASS for every changed/new owner.
6. #650 records proposed numeric RPO/RTO/offline/drill policy without approving it.

### Live materialization readback

ORC authorized materialization in review
`522e0a77-b99e-4f24-9438-2d549d150468`. The native GitHub sub-issue readback for
epic [#751](https://github.com/silentspike/project-sentinel/issues/751) is exactly
`[#752, #753, #754, #755, #756]`. Every issue is `quality:ready`; the new epic and
children are `status:blocked` because specification is complete but implementation
and runtime evidence are not.

| Node | Ordered dependency | Live body SHA-256 | Fresh Issue Quality Gate |
|---|---|---|---|
| [#751 epic](https://github.com/silentspike/project-sentinel/issues/751) | Exactly R0-R4 | `930a0da9ff867e16a17f88b74598e7463b9459e114ea2b3aba1e0823bb9cd862` | [30430032015 PASS](https://github.com/silentspike/project-sentinel/actions/runs/30430032015) |
| [#752 R0](https://github.com/silentspike/project-sentinel/issues/752) | None; versioned #696 port | `27d00b6d1066c81f0f42a0562310171fcbc76751d05a14b2767a33abada2563a` | [30430032328 PASS](https://github.com/silentspike/project-sentinel/actions/runs/30430032328) |
| [#753 R1](https://github.com/silentspike/project-sentinel/issues/753) | #752 plus owner ports; not R2/#650 | `73cfa0fddee9603380f29eaf45a4ef5701fc6e63ccfc26d346c565b9ff912dcf` | [30430032499 PASS](https://github.com/silentspike/project-sentinel/actions/runs/30430032499) |
| [#754 R2](https://github.com/silentspike/project-sentinel/issues/754) | #752, #753, #705, #656 | `bfe0fd6b521b89223740722e94d90d99a2f94ecd7a6a59b48a8bf4a858ba86d2` | [30430031810 PASS](https://github.com/silentspike/project-sentinel/actions/runs/30430031810) |
| [#755 R3](https://github.com/silentspike/project-sentinel/issues/755) | #752-#754 plus owner ports | `30bce4b97a0f8167928a389876a42554914617c0d7d5e0dfa5f8e0a9bd8f0609` | [30430031953 PASS](https://github.com/silentspike/project-sentinel/actions/runs/30430031953) |
| [#756 R4](https://github.com/silentspike/project-sentinel/issues/756) | #752-#755 plus retention owners | `5a9ea398ca698d30db9355b15af09bff27183cb94d6bb24945f9aa05ee8ac779` | [30430013292 PASS](https://github.com/silentspike/project-sentinel/actions/runs/30430013292) |

Existing-owner bodies carry one `<!-- issue-722-recovery-delta -->` section. The
body digest is over the exact UTF-8 GitHub body, without an added newline:

| Existing owner | Accepted delta | Live body SHA-256 | Fresh Issue Quality Gate |
|---|---|---|---|
| [#706](https://github.com/silentspike/project-sentinel/issues/706) | Supervision, restart fencing, ACK/readiness CAS | `64bf4b7fd6f31393c8f83fc9ccecd21d1c4e430b4f0f033c1ba745f0bc7813ea` | [30430211270 PASS](https://github.com/silentspike/project-sentinel/actions/runs/30430211270) |
| [#707](https://github.com/silentspike/project-sentinel/issues/707) | ECS barrier/resource receipt/no early tick | `cafd874ee81a1f87b027132c26665b8148e9ae0212b5360c6c474b125fcb60a9` | [30430213877 PASS](https://github.com/silentspike/project-sentinel/actions/runs/30430213877) |
| [#728](https://github.com/silentspike/project-sentinel/issues/728) | Storage-generation receipt/stage/activation | `4306acce3e8a689febaf45df95a0527196b9ce4197ee495bb659d56865759ad0` | [30430217003 PASS](https://github.com/silentspike/project-sentinel/actions/runs/30430217003) |
| [#729](https://github.com/silentspike/project-sentinel/issues/729) | Exact redb mechanism/failpoints/raw-copy rejection | `4ce70811047117e57bdac6dc5d13e1d513781443359aa760a6c2791303f8fdc1` | [30430217916 PASS](https://github.com/silentspike/project-sentinel/actions/runs/30430217916) |
| [#732](https://github.com/silentspike/project-sentinel/issues/732) | Event envelope/generation/replay identity | `b9c606ffe2b0e4e18c26c88605bfe812857810e32a02b424df56d730c2be1474` | [30430218501 PASS](https://github.com/silentspike/project-sentinel/actions/runs/30430218501) |
| [#733](https://github.com/silentspike/project-sentinel/issues/733) | PubAck/inbox/outcome/fixed-point drain | `b2cb1e157dc577dbae4b27ece4900c33718ce0c406967058e7244a9df063342d` | [30430219641 PASS](https://github.com/silentspike/project-sentinel/actions/runs/30430219641) |
| [#734](https://github.com/silentspike/project-sentinel/issues/734) | Projection generation/ValidationOnly/CAS gate | `3e0b65ab9363237b2f9f249485c508104acc7ad83381e277cc52adccbae6d381` | [30430220995 PASS](https://github.com/silentspike/project-sentinel/actions/runs/30430220995) |
| [#735](https://github.com/silentspike/project-sentinel/issues/735) | Episode durable effect frontier | `0a2e421c10d463ca749fc8aa060887820bc547d6ab47f973ed8a1e591cc6aec9` | [30430222504 PASS](https://github.com/silentspike/project-sentinel/actions/runs/30430222504) |
| [#736](https://github.com/silentspike/project-sentinel/issues/736) | EventTruthGeneration/consumer/retention frontier | `5f1443552affdc42e34af255f927c2a3e57a61e379fa406043303ade03af50c3` | [30430225735 PASS](https://github.com/silentspike/project-sentinel/actions/runs/30430225735) |
| [#695](https://github.com/silentspike/project-sentinel/issues/695) | WorkflowRecoveryPort/fence/authority | `bbdd9e49b3d8225b3cda50365e93f15fbad4c23081dfd5f58727994cd6a20a4a` | [30430226209 PASS](https://github.com/silentspike/project-sentinel/actions/runs/30430226209) |
| [#710](https://github.com/silentspike/project-sentinel/issues/710) | Durable operation/effect generation | `ab8475f1936b70320e00e23984390d35224b23d0ec80c7e39bfc34ac27e2e7a4` | [30430227725 PASS](https://github.com/silentspike/project-sentinel/actions/runs/30430227725) |
| [#472](https://github.com/silentspike/project-sentinel/issues/472) | Runtime profile as intent/no raw handles | `af919cd219d44c1364ebb460e61a1fc4bca173913f8c0b34e1c9ab5465036813` | [30430228494 PASS](https://github.com/silentspike/project-sentinel/actions/runs/30430228494) |
| [#701](https://github.com/silentspike/project-sentinel/issues/701) | Channel/process-tree cancel and fence | `4694d2bf3a6ffb5b9479181b913b203608adf78cc7dd821af35f32bc9f3fc963` | [30430232251 PASS](https://github.com/silentspike/project-sentinel/actions/runs/30430232251) |
| [#694](https://github.com/silentspike/project-sentinel/issues/694) | Workbench intent/outcome recovery port | `eeac3beb5fe2f627e858e5c4eacfe597b40baecb572772278e7f975fb9af7698` | [30430232308 PASS](https://github.com/silentspike/project-sentinel/actions/runs/30430232308) |
| [#696](https://github.com/silentspike/project-sentinel/issues/696) | Versioned signed-release publication port | `960f21cfb227f7800bcf312c0e4f5b2cf691357ff77d195f9ea5a39a07fa8362` | [30430234672 PASS](https://github.com/silentspike/project-sentinel/actions/runs/30430234672) |
| [#250](https://github.com/silentspike/project-sentinel/issues/250) | WorldSnapshot local-only receipt boundary | `34feb020dabd54fabfe5d5fe1e4b731200c5ccf8161134e2ebf520c5f90955a2` | [30430234832 PASS](https://github.com/silentspike/project-sentinel/actions/runs/30430234832) |
| [#264](https://github.com/silentspike/project-sentinel/issues/264) | CAS pin/immutability/local-vs-offsite boundary | `3b4dabd31b7fa81aad4d6e7017071f803b894d9281efe18979674db489e67044` | [30430461168 PASS](https://github.com/silentspike/project-sentinel/actions/runs/30430461168) |
| [#481](https://github.com/silentspike/project-sentinel/issues/481) | Product retention/copy/restore/frontier protection | `bf3966feaa022a128ee3a0bc0c3dab88fdcfe35a844c4969e0d6bf4de981ebae` | [30430238604 PASS](https://github.com/silentspike/project-sentinel/actions/runs/30430238604) |
| [#705](https://github.com/silentspike/project-sentinel/issues/705) | Exact restic/privilege decision before R2 | `08f262749881793e061dfccfd3ba2c678b6fea60e7d4eb20674036a08601c7a5` | [30430240530 PASS](https://github.com/silentspike/project-sentinel/actions/runs/30430240530) |
| [#656](https://github.com/silentspike/project-sentinel/issues/656) | Accepted dependency upgrade/rollback ownership | `2f3bab8e3c7f6f4d83b6750e2e15c6a3b35328a50b08e10841cb51fdf6b8c117` | [30430241574 PASS](https://github.com/silentspike/project-sentinel/actions/runs/30430241574) |
| [#650](https://github.com/silentspike/project-sentinel/issues/650) | Downstream acceptance; policy explicitly unapproved | `06116f06accb2808afedb6666ff207927a331931e9a65d830e1de732986fff17` | [30430243639 PASS](https://github.com/silentspike/project-sentinel/actions/runs/30430243639) |

## 12. Acceptance-criteria readback

| Criterion | Study evidence | Status at REVIEW_READY |
|---|---|---|
| AC-1 | Sections 3.1-3.4 map source, tests, runtime contracts, claim drift, incidents, and live owners. | PASS |
| AC-2 | Section 4 evaluates eight candidates with a 10-factor rubric and explicit shortlist/rejection reasons. | PASS |
| AC-3 | Section 5 reviews five pinned systems through source, tests, failures, security, license, and operations. | PASS |
| AC-4 | Section 6 covers every listed mechanism and all five shortlisted systems; the non-functional matrix covers correctness boundaries, failures, 1:n/determinism, security, maintenance, dependency cost, and integration. | PASS |
| AC-5 | Section 7 assigns exactly one decision to each Sentinel mechanism and records rejected alternatives. ORC approved D1-D11 in review `522e0a77-b99e-4f24-9438-2d549d150468`. | PASS |
| AC-6 | Section 11 records the acyclic R0-R4 epic, native sub-issue graph, exact existing-owner deltas, live body digests, final labels, and fresh quality runs. | PASS |
| AC-7 | Section 10 classifies every finding and identifies the M0 acknowledgement owner. | PASS |
| AC-8 | This file is the sole repository change and is English/ASCII/public-safe. URL, link, typo, sanitization, and diff gates passed before the review head was frozen and are recorded in the PR evidence. | PASS |
| AC-N1 | No dependency is added; restic is gated by #705. | PASS |
| AC-N2 | Every deep review records provenance, license, security, maintenance, and boundary. No code is copied. | PASS |
| AC-N3 | Current tests are treated as local invariant evidence, not optimality or whole-product proof. | PASS |
| AC-N4 | No runtime, VM, provider, Rust build, or performance benchmark is used. | PASS |
| AC-N5 | Every gap has an explicit existing or new implementation owner and reciprocal contract; all remain open/blocked until implementation evidence exists. No gap is declared implemented. | PASS |

## 13. Limitations and pending policy

- This source audit did not run upstream test suites. The reviewed tests demonstrate
  upstream intent and adversarial coverage, not compatibility with Sentinel.
- This issue did not mutate a runtime and therefore has no current machine-loss,
  pause-time, RPO, or RTO measurement.
- The architecture selects an independently administered encrypted recovery escrow,
  an independently durable signed release registry, and current revocation as
  authoritative. A security/operations owner still must approve the concrete escrow
  provider, key ceremony, quorum, custody, audit, and break-glass runbook before
  implementation.
- The final #695 workflow schema is not on this baseline. Its live
  `WorkflowRecoveryPortV1` delta is an implementation contract, not a claim that the
  port already exists.
- D5 selects restic behind the immutable sealed-bundle boundary, but does not
  authorize a dependency. #705 must either approve that exact integration and
  privilege/update contract or reject D5 and return the transport choice to ORC; it
  must not silently substitute another backend.
- D1-D11 and the owner-resolution procedure are approved and materialized. This
  does not approve any implementation, runtime result, release, restore, or M0
  acceptance claim.

The remaining policy decisions belong only to #650/product ownership:

1. whether RPO <= 15 minutes and RTO <= 60 minutes become accepted thresholds;
2. whether an offline/immutable copy is mandatory before accepting customer work;
3. which drill cadence becomes mandatory;
4. which security/operations owner and exact escrow provider/key ceremony satisfy
   R0;
5. when complete R0-R4 implementation/runtime evidence is sufficient for product
   acceptance.

Until those decisions and implementation evidence exist, the new issues remain
blocked and every numeric/offline/drill value remains a proposal, not a readiness
gate or achieved result.
