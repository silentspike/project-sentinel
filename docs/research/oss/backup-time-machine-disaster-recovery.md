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
| Company workflow journal | #695 owns customer/agreement/project/work state and an execution outbox; the final durable-execution boundary is refined by #710. | `authoritative` when enabled | #695/#710 expose one digest-bound workflow-generation receipt with authority generation, operation/event/outbox/evidence frontiers, and terminal/unknown effects. P4 is an integration contract, not a second journal. |
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
| [#706](https://github.com/silentspike/project-sentinel/issues/706) | Open, in progress | Supervision, dependency-aware readiness, restart budgets, quarantine | Supplies participant crash/restart and readiness semantics; P1 does not create a second supervisor. |
| [#707](https://github.com/silentspike/project-sentinel/issues/707) | Open, in progress | ECS schedule, deterministic barriers, snapshot/replay ordering | Owns the ECS freeze barrier and registered-resource completeness used by the product cut. |
| [#708](https://github.com/silentspike/project-sentinel/issues/708) / [#726](https://github.com/silentspike/project-sentinel/issues/726) | Open, in progress / blocked | Accepted redb/CAS operating design and generation-safe storage epic | Supplies storage-generation vocabulary; #722 coordinates but does not redefine engine backup. |
| [#728](https://github.com/silentspike/project-sentinel/issues/728) | Open, blocked | Versioned metadata-plus-CAS generations, staging, backup/restore, activation | Sole owner of `SealedStoreGenerationReceipt`, engine-consistent storage generations, and activation. P1 consumes its receipt. |
| [#729](https://github.com/silentspike/project-sentinel/issues/729) | Open, blocked | redb policies, integrity, transactions, compaction, deterministic fault harness | Sole owner of redb mechanism choice and proof. Raw open-file copying remains forbidden unless this owner proves it. |
| [#709](https://github.com/silentspike/project-sentinel/issues/709) / [#731](https://github.com/silentspike/project-sentinel/issues/731) | Open, in progress / blocked | Accepted event truth, delivery, CQRS, and generation-safe epic | Supplies `EventTruthGeneration`; #722 consumes it as part of the whole-product manifest. |
| [#732](https://github.com/silentspike/project-sentinel/issues/732) | Open, blocked | Canonical event envelope, append gateway, durability, schema authority | Owns event identity, generation, and durability fields; P1 does not create another event envelope. |
| [#733](https://github.com/silentspike/project-sentinel/issues/733) | Open, blocked | JetStream PubAck outbox and permanent consumer inbox/outcomes | Resolves authoritative in-flight stream state and effect-idempotency gaps required before a rebuild-only NATS contract is valid. |
| [#734](https://github.com/silentspike/project-sentinel/issues/734) | Open, blocked | Projection catalog, poison lane, blue-green generations | Owns projection generation/rebuild/activation and readiness. |
| [#735](https://github.com/silentspike/project-sentinel/issues/735) | Open, blocked | Idempotent durable EpisodeProducer projection | Owns the event-to-Hippocampus effect frontier. |
| [#736](https://github.com/silentspike/project-sentinel/issues/736) | Open, blocked | Consumer catalog, retention frontiers, `EventTruthGeneration`, backup/recovery | Sole owner of WAL-aware event/projection cut and event-retention claims; P1 consumes the receipt. |
| [#710](https://github.com/silentspike/project-sentinel/issues/710) | Open, in progress | Cross-store durable execution, external-effect outcomes, workflow journaling | Owns durable-execution cut/effect semantics. P4 refines #695 through this contract and never becomes another workflow engine. |
| [#472](https://github.com/silentspike/project-sentinel/issues/472) / [#701](https://github.com/silentspike/project-sentinel/issues/701) / [#694](https://github.com/silentspike/project-sentinel/issues/694) | Open, review / blocked / in progress | Runtime selection, cancellable channel, durable Workbench intent and receipts | Restore consumes durable intent/outcomes, rejects raw handles, and reconciles through their production lifecycle. |
| [#556](https://github.com/silentspike/project-sentinel/issues/556) | Open, ready | Cluster GA, backup, stale identity/term rejection | Owns cluster cold recovery and N-node claims, not single-node M0 recovery. |
| [#650](https://github.com/silentspike/project-sentinel/issues/650) | Open, blocked | Single-node M0 product acceptance | Owns final runtime acceptance and must acknowledge the M0 recovery class. |
| [#693](https://github.com/silentspike/project-sentinel/issues/693) | Closed, verified | Work-execution contract | Supplies authority/idempotency vocabulary; no new implementation ownership. |
| [#696](https://github.com/silentspike/project-sentinel/issues/696) | Open, ready | QA, signed release/delivery lineage, and rollback | Owns creation and independent publication of the signed release set consumed by recovery. |
| [#695](https://github.com/silentspike/project-sentinel/issues/695) | Open, in progress | Company workflow and journal | Must expose a recovery capture/restore port; this study does not modify its parked work. |
| [#705](https://github.com/silentspike/project-sentinel/issues/705) | Open, blocked | Dependency necessity/ownership | Mandatory gate before restic or any other dependency is introduced. |
| [#656](https://github.com/silentspike/project-sentinel/issues/656) | Open, backlog | Upgrade ownership | Owns future version/pin/update policy for accepted dependencies. |

The ownership split is binding for the proposed work below: P1 owns only product
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
| D1 | Whole-product application-consistent cut and validity | **Reimplement minimal** | P1 owns the coordinator, signed coverage/participant registry, prepare/drain receipts, and publish-last envelope. It consumes owner-supplied generations rather than implementing engines. | restic/Velero/ZFS as cut authority; one in-process mutex; filesystem copy while live |
| D2 | Existing world Time Machine and bounded replay | **Keep Sentinel** | It already models domain state, dead branches, projection seed, and runtime reconciliation. | Replace with generic file rollback or CRIU |
| D3 | SQLite event/projection generation | **Port algorithm/contract** | #736 owns the WAL-aware `EventTruthGeneration`; it may port Litestream continuity, gap, temp-file, fsync, and failure invariants. P1 consumes the sealed receipt and cannot create a competing SQLite activation authority. | Direct live-file copy; one Litestream daemon per DB; P1-owned SQLite adapter |
| D4 | redb/CAS store generation | **Keep Sentinel** | #728/#729 exclusively decide and prove the redb mechanism and emit `SealedStoreGenerationReceipt`. P1 records the receipt. Raw copying of an open redb file is forbidden unless the pinned redb contract proves that exact operation. | A vague "short transaction" promise; P1-owned redb adapter; filesystem snapshot as logical consistency |
| D5 | Sealed bundle remote/offline transport | **Integrate** | restic has the best external boundary for encryption, dedup, check, retention, and backend diversity. #705 must approve the dependency and privilege model first. | Embed restic code; make Borg/Kopia simultaneous dependencies |
| D6 | Recovery lifecycle, finalization, partial failure, and ordered resources | **Port algorithm/contract** | Velero's durable phase and ordering model maps well without Kubernetes. | Boolean success flag; unbounded shell hooks; Velero dependency |
| D7 | Host filesystem snapshots | **Reject** | They are neither portable nor application-consistent and cannot be an M0 prerequisite. | Require OpenZFS/btrfs; call a VM snapshot a product backup |
| D8 | Runtime process checkpoint | **Reject** | Old PIDs, sockets, leases, process memory, and credentials must not regain authority after disaster restore. | CRIU restore or microVM-memory restore as canonical state |
| D9 | Projections and NATS delivery state | **Keep Sentinel** | #733/#734/#736 own PubAck, inbox/outcome, consumer, projection-generation, replay, and frontier contracts. P1 restores their receipts, recreates streams, rebuilds projections, and verifies watermarks. | Restore `projection.db` or broker ACK cursors as authority; skip to `MAX(id)` |
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
  state                         // sealed_local | verified_offsite
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

  transport {
    bundle_format
    repository_id
    encryption_key_reference
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
  -> Releasing
  -> Exporting
  -> VerifyingOffsite
  -> Restorable

Any pre-seal phase -> Aborting -> Aborted | ManualRecoveryRequired
Any post-seal validation failure -> Quarantined
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
| 0. `recovery-coordinator` | New P1 owner; no product-wide coordinator exists today. | Persist request, validate signed catalogs, assign generation, collect receipts, authorize capture, and alone release/abort. | Journal durable before any participant message. |
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
adapters and projections reopen first for validation, then consumers/publishers,
daemon/runtime dispatchers, workflow, and finally customer/provider admission.
Every participant must acknowledge the same release digest before product readiness
turns green.

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
   WAL-aware `EventTruthGeneration` receipt. P1 never reads an open redb/SQLite file
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
14. Record `SealedLocal`, issue dependency-reversed `ReleaseV1`, and require all
    acknowledgements before readiness. Export only the immutable sealed directory.
15. Verify transport repository data and a staged restore before setting
    `VerifiedOffsite`; transport success alone cannot upgrade recovery validity.

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
12. Run participant readiness in dependency order: stores, projections, consumers,
    publishers, daemon/runtime, workflow, then customer/provider admission. Positive
    and negative business probes must match the manifest generation.
13. Record and sign a restore receipt with envelope/release IDs, activated
    generations, consumer/projection frontiers, credential generation, unresolved
    manual work, and invariant results. Advance the independent anti-rollback
    catalog only after verification, then release admission.

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
| `M0_SINGLE_NODE_DR` | Loss/corruption of the product data directory holding authoritative customer work | `VerifiedOffsite` signed whole-product envelope plus independent release/trust inputs | At most 15 minutes | At most 60 minutes | Unapproved policy proposal; #650/product owner must decide separately from D1-D11 |
| `OFFLINE_SECURITY` | Ransomware/operator credential compromise | Independently administered immutable/offline copy plus surviving escrow/catalog | At most 24 hours | At most 4 hours | Unapproved policy proposal; `M0_HARDENING` mechanism for production customer work |
| `CLUSTER_RECOVERY` | Node loss, quorum recovery, geo recovery | Quorum-accepted RecoveryPoint and stale-term/cert rejection | Defined by #556 | Defined by #556 | Not decided here; `POST_M0` for the single-node product |

Approving D1-D11 does not approve `15 min`, `60 min`, or any schedule above. A
lower RPO increases capture frequency, remote bandwidth, retained objects, signing
operations, and restore-plan complexity.

Required drills:

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
| Envelope/digest pair replaced with an older valid bundle | Independent catalog rejects recovery sequence, predecessor, authority, or release generation rollback. |
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
| Control-plane, memory, evolution, nightrun, Observatory, Gaia Loop audit, config, and trust are outside Time Machine | `M0_HARDENING` | Complete section 3.2 registry; P1/P2 plus existing engine owners |
| `SENTINEL_JUDGE` can hold the only in-flight effect and current delivery lacks permanent PubAck/inbox outcomes | `M0_HARDENING` | Current bridge/consumer source; #733/#736 |
| No executable multi-process prepare/drain/release protocol exists | `M0_HARDENING` | Current writers span Gateway, daemon, projection, NATS, Judge, Nightrun, and future workflow; P1 composes #706/#707/#710 |
| Workflow journal/effect cut needs recovery integration | `M0_HARDENING` | #695 owns the journal and #710 owns cross-store effects; P4 refines them instead of creating a journal |
| Engine-consistent redb/CAS/SQLite generations are not P1 work | `M0_HARDENING` | Accepted #726/#728/#729 and #731/#736 contracts; P1 waits for sealed receipts |
| Projection file should remain derived and generation-bound | `M0_HARDENING` | Current worker offset boundary; #734/#736 own local frontier and activation |
| Manifest authentication, release availability, and anti-rollback need independent authorities | `M0_HARDENING` | Current binary manifest is digest-only; D10/P2 define signed registry plus encrypted recovery escrow |
| Stale runtime handles and credentials must not be restored | `M0_HARDENING` | Existing teardown/respawn and CRIU negative review; #472/#701/#694 and D10 |
| Numeric `15 min`/`60 min` targets | `M0_HARDENING` policy proposal, not approved | Measurement proposal only; #650/product-owner decision is separate from mechanism approval |
| Mandatory ZFS/btrfs substrate | `POST_M0` | Optional defense only; product portability wins |
| CRIU/live process memory restore | `POST_M0` and rejected | Security and compatibility costs exceed value |
| Multi-node/quorum/geo recovery | `POST_M0` for single-node acceptance | #556 and G6/G8 own cluster durability |

No finding is marked `BLOCKS_M0` merely because an upstream has a feature Sentinel
lacks. The listed hardening contracts must be owned and proved before a whole-product
DR claim. #650 must separately decide whether the proposed
`M0_SINGLE_NODE_DR` numbers become binding before production acceptance.

## 11. Proposed implementation issue contracts

Per the research issue, these contracts are proposals for ORC review. They are not
live issues and must not be materialized before the synthesis is approved.

### P1. M0 RecoveryPoint coordinator and coverage registry

**Class:** `M0_HARDENING`

**Depends on:** #706 supervision/readiness protocol; #707 ECS barrier; #728/#729
storage-generation receipts; #732-#736 event/delivery/projection receipts; #695/#710
workflow/effect receipt; #472/#701/#694 runtime intent; #696 signed release set; P2
recovery trust/transport; #650 product acceptance.

**Scope:** only whole-product coordination: signed coverage/participant catalogs,
fixed bootstrap journal, `PrepareCaptureV1`/`DrainV1`/`PreparedReceiptV1`/
`ReleaseV1`, receipt composition, manifest/envelope authority, publish-last seal,
and readiness. P1 does not implement redb/SQLite/CAS adapters, event/projection
activation, workflow journaling, runtime lifecycle, release production, or a secret
store.

**Acceptance contract:**

- every enabled service, store, stream, and consumer is registered exactly once;
  unknown/uncovered declarations fail capture and DR readiness;
- participant protocol closes the real process admission boundary, performs bounded
  fixed-point drain, rejects stale/digest-conflicting generations, and releases only
  by coordinator authority;
- coordinator and participant crash, timeout, missing abort ACK, and restart at
  every transition resume, abort, or enter manual recovery without a false point or
  automatic admission reopen;
- the fixed bootstrap journal meets path/schema/permissions/WAL/FULL/fsync,
  non-recursive backup, and total-host-loss bootstrap contracts from section 8.2;
- #728/#729, #736, #695/#710, and #694 receipts are consumed through narrow
  conformance ports and cannot be replaced by direct file access;
- `RecoveryPointEnvelopeV1` Ed25519 golden vectors bind payload, sequence, chain,
  release floor, and authority generation; tamper, signer revocation, catalog
  rollback, and bundle-plus-digest replacement fail;
- request-id/digest retries are idempotent and conflicts are typed;
- no arbitrary paths, secret bytes, derived projection authority, or live runtime
  handles enter the manifest;
- automated tests cover participant order, fixed-point non-convergence, owner
  receipt mismatch, CAS pin window, disk full, fsync/rename, schema mismatch, and
  signed-envelope publication;
- target-runtime tests and benchmarks are token-free and issue-specific;
- rollout is default-off and observe-only until every required owner receipt exists,
  then shadow prepare/abort, local seal-only, and finally offsite-required;
- rollback disables new capture while preserving old verified points;
- TOGAF delta distinguishes WorldSnapshot, owner generations, participant cut,
  signed RecoveryPoint, and independently surviving recovery authorities.

### P2. Independent bundle transport, recovery escrow, and release retrieval

**Class:** `M0_HARDENING`

**Depends on:** P1; #696 signed release publication; a #705 transport/crypto
dependency decision; #656 upgrades; an ORC-assigned security/operations owner for
the encrypted recovery escrow.

**Scope:** external sealed-bundle transport, evaluated restic integration,
independent immutable/offline copy, separately administered encrypted recovery
escrow/catalog, signed-release registry retrieval, privilege separation,
verification, retention, prune safety, and restore-to-quarantine. It does not sign
releases for #696 or store product secrets in the data repository.

**Acceptance contract:**

- transport receives only a read-only sealed directory and expected manifest digest;
- backup and prune/delete principals are separated where the backend permits;
- recovery escrow uses dual control, separate keys/principals/retention/audit, and
  provides current revocation plus anti-rollback catalog; no escrow secret appears
  in a RecoveryPoint;
- lost data key, lost escrow unlock material, lost authority catalog, revoked signer,
  compromised delete credential, backend rollback, corrupt pack, and retention race
  all have fail-closed runbooks and tests;
- signed release retrieval verifies Ed25519 manifest, product commit, artifacts,
  SBOM, provenance, and compatibility; a digest-only binary is rejected;
- a second operator restores and verifies the bundle, release, and trust inputs
  without access to the creator's live credentials;
- benchmarks measure product bundle sizes and target runtime, not upstream claims;
- rollback removes the transport adapter while preserving local signed envelopes;
  it cannot weaken escrow, signature, or anti-rollback policy.

### P3. Whole-product restore runner and disaster drills

**Class:** `M0_HARDENING`

**Depends on:** P1, P2, #728/#729 complete storage generations, #732-#736 complete
event/delivery/projection generations, #695/#710 workflow/effect generation,
#472/#701/#694 durable runtime intent, and #696 signed release/delivery lineage.

**Scope:** quarantine, compatibility validation, staged restore, generation swap,
JetStream recreation, projection rebuild, trust refresh/rotation, runtime
reconciliation, business probes, signed restore receipt, and scheduled drills.

**Acceptance contract:**

- exact restore order in section 8.5 is encoded as a versioned state machine;
- failpoints before/after every store replacement and projection/runtime transition
  prove restart recovery;
- wrong tenant/owner generation, revoked signer/principal, rollback sequence,
  missing release/SBOM/provenance, missing evidence artifact/CAS blob, stale runtime
  lease, corrupt envelope, incompatible schema, and missing owner receipt fail closed;
- JetStream definitions are recreated from `EventTruthGeneration`, durable consumers
  start from local outcome frontiers, eBPF starts empty, and no unbacked Judge effect
  can disappear or repeat;
- successful drill proves customer agreement/project/work state, event/outbox
  PubAck/inbox/outcome idempotency, artifact ownership, Gaia pair, Observatory,
  projections, runtime restart, and no duplicate external action through
  deterministic fakes;
- rollout runs first in an isolated authorized target; rollback retains the prior
  local generation and verified source point;
- RPO/RTO measurements include sidecars and exact envelope/release/generation
  identities but do not claim the proposed values are approved.

### P4. Workflow and durable-execution recovery integration

**Class:** `M0_HARDENING`

**Owner:** refine #695 and #710; do not create a second workflow-journal issue.

**Scope:** a narrow `WorkflowRecoveryPort` from the existing owner returning schema,
transaction/event/operation cursor, authority generation, execution/effect frontier,
outbox/evidence state, digest, and restore/reopen receipt.

**Acceptance contract:**

- capture is invoked only under the product fence and never accepts caller authority;
- completion-evidence requests, authority conflicts, claimed actions, and pending
  outbox rows remain recoverable without replaying a completed external effect;
- restore rejects cross-tenant/project/assignment receipt replay and stale
  organization generation;
- unknown external outcomes are durably blocked or probed, never inferred/retried;
- migration crash points, #695 restart tests, and #710 cross-store cut/effect tests
  remain valid after bundle restore;
- P4 emits a receipt and owns no activation, backup scheduler, or second journal.

### P5. Recovery coverage, retention, and operator health

**Class:** `M0_HARDENING`

**Owner overlap:** refine #250, #264, #481, #650, and #736; materialize a new owner
only for uncovered whole-product health aggregation.

**Scope:** coverage inventory, last-sealed/last-offsite age, restore-test age,
participant health, consumer/frontier blockers, retention protection, pin leaks,
repository/release/escrow health, typed operator status, and alerts.

**Acceptance contract:**

- health names the exact uncovered/stale participant, store, stream, consumer,
  receipt, release, or trust catalog without paths/secrets;
- retention never removes the last verified point for each required RPO class or a
  point referenced by an in-progress drill/restore;
- pin leaks, disk pressure, repository check failures, and missed RPO targets are
  observable and bounded;
- #736's minimum safe event frontier includes every retained RecoveryPoint;
- negative tests prove health cannot be forged by transport success, stale
  participant receipts, digest-only binaries, or absent escrow/catalog.

### Owner-resolution rule

After ORC approves the synthesis:

1. ask #706, #707, #708/#726/#728/#729, #709/#731/#732-#736, #710, #472/#701/#694,
   #695, #696, #705, #656, #250, #264, #481, and #650 owners to accept the exact
   deltas above;
2. update an existing issue when it owns the complete acceptance contract;
3. bind P4 to #695/#710, signed release production to #696, engine generations to
   #728/#729/#736, and runtime intent to #472/#701/#694;
4. materialize only genuinely uncovered P1 coordinator, P2 external recovery
   authorities/transport, P3 restore runner, or P5 aggregator contracts in dependency
   order;
5. require reciprocal links, `quality:ready`, target runtime, benchmarks, rollout,
   rollback, negative criteria, and TOGAF delta before implementation;
6. keep AC-6 and AC-N5 open until that live owner readback and fresh quality gate
   exist.

The following delivery envelope is mandatory if ORC materializes or merges any of
P1-P5 into an existing owner:

| Contract | Dependencies | Negative criteria | Target-runtime tests and benchmarks | Rollout | Rollback | TOGAF delta |
|---|---|---|---|---|---|---|
| P1 coordinator | All owner receipts above; P2 trust; #650 acceptance | No uncovered plane, one-process mutex claim, arbitrary path, false seal, unsigned envelope, stale generation, secret byte, live handle, or competing engine activation | Authorized single-node target: participant prepare/drain/release, pause/seal duration, bytes, CPU/IO sidecars, crash/timeout/fixed-point matrix, exact signed-envelope readback | Default off -> observe catalogs -> shadow prepare/abort -> local seal -> scheduled seal | Disable coordinator; participants durably abort/release; old WorldSnapshot and prior verified points remain readable | Define WorldSnapshot, owner generations, participant cut, signed RecoveryPoint, and bootstrap journal |
| P2 independent recovery inputs | P1, #696, #705, #656, security/operations owner | No live-store input, shared data/escrow key, unsigned release, all-powerful credential, rollback catalog bypass, or prune of last-known-good | Local mock stores first, then authorized independent storage/escrow/registry: upload/verify/retrieve/restore latency, dedup, lost-key and revoked-signer drills | Read-only shadow export -> independent verification -> offsite-required policy | Remove transport while preserving local signed envelopes; never weaken escrow/signature policy | Define independent data, release, and trust authorities |
| P3 restore/drills | P1/P2 plus #728/#729, #732-#736, #695/#710, #472/#701/#694, #696 | No in-place first mutation, mixed generations, broker ACK authority, stale trust, unsigned binary, raw handle restore, effect rewind, or readiness before frontiers/probes | Authorized isolated single-node restore: quarantine through readiness, p50/p95/max over issue fixtures, sidecars, stream/consumer corruption and restart matrix | Dry validation -> staged restore -> scheduled full drill -> incident runbook | Journal switches only to intact verified generation/allowed envelope; fence remains on failure | Encode release/trust verification, store/event activation, stream replay, projection rebuild, runtime reconciliation, and signed receipt |
| P4 workflow integration | #695 and #710, then P1 conformance | No caller authority, second journal, cross-tenant receipt, duplicate effect, unknown-outcome retry, busy loop, or non-terminal evidence loss | Deterministic fake plus final single-node integration: receipt latency/size, restart, migration, unknown-effect and bundle-restore failpoints | Existing workflow remains default-off until owner receipt and coordinator catalog accept its schema | Disable DR registration; workflow may run only while DR readiness reports uncovered authority | Add workflow/effect generation receipt to RecoveryPoint without moving journal authority |
| P5 health/retention | P1-P3, #250/#264/#481/#736, #650 policy | No green health from transport-only success, stale receipt/drill, unknown consumer, pin leak, missed proposed target, unsigned release, or missing trust authority | Authorized single-node soak: health cost, retention/prune duration, disk growth, participant outage, missed schedule, repository/release/escrow failures | Observe-only -> alert -> readiness gate after stable evidence and approved SLO | Return to observe-only; never weaken immutable retention or required frontiers | Add coverage, participant, offsite, release/trust, drill, and policy-approval status |

## 12. Acceptance-criteria readback

| Criterion | Study evidence | Status at REVIEW_READY |
|---|---|---|
| AC-1 | Sections 3.1-3.4 map source, tests, runtime contracts, claim drift, incidents, and live owners. | PASS |
| AC-2 | Section 4 evaluates eight candidates with a 10-factor rubric and explicit shortlist/rejection reasons. | PASS |
| AC-3 | Section 5 reviews five pinned systems through source, tests, failures, security, license, and operations. | PASS |
| AC-4 | Section 6 covers every listed mechanism and all five shortlisted systems; the non-functional matrix covers correctness boundaries, failures, 1:n/determinism, security, maintenance, dependency cost, and integration. | PASS |
| AC-5 | Section 7 assigns exactly one decision to each Sentinel mechanism and records rejected alternatives. Maintainer approval remains an ORC action. | REVIEW_PENDING |
| AC-6 | Section 11 supplies implementation-ready proposed contracts without violating the no-materialization instruction. Live quality-ready issues intentionally await ORC synthesis approval. | REVIEW_PENDING |
| AC-7 | Section 10 classifies every finding and identifies the M0 acknowledgement owner. | PASS |
| AC-8 | This file is the sole repository change and is English/ASCII/public-safe. URL, link, typo, sanitization, and diff gates passed before the review head was frozen and are recorded in the PR evidence. | PASS |
| AC-N1 | No dependency is added; restic is gated by #705. | PASS |
| AC-N2 | Every deep review records provenance, license, security, maintenance, and boundary. No code is copied. | PASS |
| AC-N3 | Current tests are treated as local invariant evidence, not optimality or whole-product proof. | PASS |
| AC-N4 | No runtime, VM, provider, Rust build, or performance benchmark is used. | PASS |
| AC-N5 | No gap is declared closed. Proposed owners and the post-approval resolution procedure are explicit. | REVIEW_PENDING: live owner acceptance and reciprocal issue contracts remain required |

## 13. Limitations and review questions

- This source audit did not run upstream test suites. The reviewed tests demonstrate
  upstream intent and adversarial coverage, not compatibility with Sentinel.
- This issue did not mutate a runtime and therefore has no current machine-loss,
  pause-time, RPO, or RTO measurement.
- The architecture selects an independently administered encrypted recovery escrow,
  an independently durable signed release registry, and current revocation as
  authoritative. A security/operations owner still must approve the concrete escrow
  provider, key ceremony, quorum, custody, audit, and break-glass runbook before
  implementation.
- The final #695 workflow schema is not on this baseline. P4 is a required adapter
  contract, not a claim about code that does not exist here.
- D5 selects restic behind the immutable sealed-bundle boundary, but does not
  authorize a dependency. #705 must either approve that exact integration and
  privilege/update contract or reject D5 and return the transport choice to ORC; it
  must not silently substitute another backend.
- ORC must decide whether to approve D1-D11, independently decide the proposed
  numeric recovery policy, and approve the owner-resolution procedure. Until then
  AC-5, AC-6, and AC-N5 are not final.

Review should answer:

1. Does ORC approve D1-D11 as the mechanism decisions, independently of any numeric
   RPO/RTO target?
2. Does ORC approve the owner-resolution procedure: update the named existing
   owners first, bind P4 to #695/#710, and materialize only uncovered P1, P2, P3,
   or P5 contracts?
3. Which security/operations owner accepts the independent recovery escrow,
   authority catalog, key ceremony, dual-control, revocation, anti-rollback, and
   disaster-access contract?
4. Does #696 accept production and independent publication of the signed release
   set, including artifacts, SBOM, provenance, compatibility, and release manifest?
5. Do #706/#707, #728/#729, #732-#736, #695/#710, and #472/#701/#694 accept their
   participant/generation receipt deltas and the product-fence lifecycle?
6. Separately, does #650/product ownership approve the proposed RPO <= 15 minutes,
   RTO <= 60 minutes, offline-copy requirement, and drill cadence?

The exact remaining AC-6 action is therefore: after ORC approves this synthesis,
obtain the named owners' live acceptance, update their issue bodies with the exact
receipt/dependency deltas, materialize only still-unowned P1/P2/P3/P5 contracts,
add reciprocal links and required runtime/rollback/negative criteria, and run a
fresh quality gate for every changed or new issue. No implementation starts before
that readback.
