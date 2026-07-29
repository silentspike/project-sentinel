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

1. **Reimplement minimal:** add a Sentinel-owned `RecoveryPointManifestV1` and a
   durable, fenced recovery coordinator. The manifest is published last and is the
   only object that makes a multi-store capture restorable.
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
| Event plus outbox | [`append_with_outbox`](../../../crates/sentinel-limbo/src/event_store.rs#L993-L1052) inserts an event and pending outbox row in one SQLite transaction with operation-id idempotency. | Durable event publication intent survives a publisher failure. | Published NATS state is not an authority and does not need to be captured. |
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

| Logical store | Role | Current recovery coverage | Required whole-product contract |
|---|---|---|---|
| `events.db` | Authoritative event log, outboxes, world snapshots, restore generation/dead ranges, projection offsets | WorldSnapshot is stored inside it; the database itself is not exported by Time Machine. | WAL-aware sealed copy; SQLite integrity check; maximum event ID, restore generation, pending-outbox counts, schema digest, and file digest in the manifest. |
| `state.redb` | Authoritative simulation state and durable agent facts | Its 12 logical tables are embedded in WorldSnapshot and restored atomically within redb. | Engine-consistent file or logical export tied to the same global cut; redb integrity/open test and table inventory. |
| ECS memory | Current simulation resources/components | Versioned WorldSnapshot coverage exists, but `RoomPhysicsState` is explicitly absent and projection restore reconstructs only occupancy ([source](../../../services/sentinel-daemon/src/orchestrator.rs#L3709-L3715)). | Capture under the global mutation fence; schema/version digest; bounded replay inputs; an explicit include/rebuild/reject decision for every registered resource. |
| `metadata.redb` | Authoritative namespace, CAS references, trash, snapshot pins | Optional logical dump in WorldSnapshot. | Capture in the same cut and bind its exact CAS root/manifest digest. |
| CAS directory | Authoritative artifact bytes | Only references are pinned and pre-restore existence is checked. | Enumerated immutable blob set, size and digest verification, sealed export, missing/extra policy, and offline copy. |
| Runtime home ArtifactPlane (`home.redb` plus segment packs) | Authoritative bwrap home objects, BLAKE3 chunk references, ingest sessions, and append-only segment bytes | [`ArtifactPlane`](../../../crates/sentinel-fs/src/artifact.rs#L151-L208) is separate from the SHA-256 `CasStore` and is not in WorldSnapshot. | A dedicated capture adapter must bind the redb metadata and exact segment generation/chunk set. It must recover or reject non-terminal ingest sessions. |
| `controlplane.redb` | Durable control-plane analyses and policy state | Not in WorldSnapshot. | Capture as an authoritative redb store; restore before mutation APIs reopen. |
| `hippocampus.redb` | Durable episodic/semantic memory | Not in WorldSnapshot. | Capture and verify as authoritative; bind its event/source cursor where available. |
| `evolution.db` | Durable evolution proposals, judgments, and state | Not in WorldSnapshot. | WAL-aware SQLite capture and schema/integrity receipt. |
| `nightrun-jobs.db` | Durable scheduled-work queue | Not in WorldSnapshot. | Capture pending/running/terminal counts; on restore, convert stale running leases to recoverable pending or operator-review states. |
| `gaia_console_memory.redb` | Durable Gaia console graph/cache | Not in WorldSnapshot. | Declare whether authoritative or rebuildable before implementation. If rebuildable, store recipe/source cursor; otherwise capture as redb. |
| `projection.db` | Derived CQRS read model | Projection offsets are in WorldSnapshot; rows are reset and seeded on restore. | Do not make the file authoritative. Record schema and source cursor, rebuild/seed recipe, and verify watermarks before serving reads. |
| `cluster_meta.redb` | Cluster owner, route, term, and fencing metadata when cluster mode is enabled | Not in WorldSnapshot. | Separate cluster recovery owner [#556](https://github.com/silentspike/project-sentinel/issues/556); never restore stale terms/certificates as current authority. Single-node recovery records it as absent or local-only. |
| Company workflow journal | Future authoritative customer, agreement, project, work, evidence, and outbox state | No implementation or `company-workflow.sqlite` exists on this baseline; [#695](https://github.com/silentspike/project-sentinel/issues/695) owns it. | Its final schema must implement a coordinator capture port, event cursor, authority generation, pending evidence/outbox counts, migration version, and restore/open health receipt. |
| Runtime handles | PIDs, pipes, cgroups, microVM handles, leases, in-flight executions | Processes are torn down and reconstructed after World restore. | Never restore raw handles. Persist bounded durable intent and idempotency keys, terminate old trees, invalidate leases, and reconcile from the manifest. [#694](https://github.com/silentspike/project-sentinel/issues/694) owns work execution. |
| Configuration | Desired topology, agent definitions, model catalog, service config | Outside WorldSnapshot. | Versioned, allowlisted config bundle with source commit and semantic digests. Restore requires operator approval and binary/schema compatibility. |
| Credentials and trust | Caller credentials, provider tokens, TLS material, revocation/owner truth | Loaded separately from protected deployment credentials; outside WorldSnapshot. | Backup secrets in a separately encrypted domain or record only secret references. Current revocation and owner authority always override backed-up trust. Rotate credentials after disaster restore. |

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
| [#556](https://github.com/silentspike/project-sentinel/issues/556) | Open, ready | Cluster GA, backup, stale identity/term rejection | Owns cluster cold recovery and N-node claims, not single-node M0 recovery. |
| [#650](https://github.com/silentspike/project-sentinel/issues/650) | Open, blocked | Single-node M0 product acceptance | Owns final runtime acceptance and must acknowledge the M0 recovery class. |
| [#693](https://github.com/silentspike/project-sentinel/issues/693) | Closed, verified | Work-execution contract | Supplies authority/idempotency vocabulary; no new implementation ownership. |
| [#696](https://github.com/silentspike/project-sentinel/issues/696) | Open, ready | Delivery lineage and rollback | Recovery manifest must preserve delivery artifact lineage and rollback evidence. |
| [#695](https://github.com/silentspike/project-sentinel/issues/695) | Open, in progress | Company workflow and journal | Must expose a recovery capture/restore port; this study does not modify its parked work. |
| [#694](https://github.com/silentspike/project-sentinel/issues/694) | Open, in progress | Workbench durable execution/recovery | Owns durable execution intent; disaster restore invalidates runtime handles and reconciles. |
| [#705](https://github.com/silentspike/project-sentinel/issues/705) | Open, blocked | Dependency necessity/ownership | Mandatory gate before restic or any other dependency is introduced. |
| [#656](https://github.com/silentspike/project-sentinel/issues/656) | Open, backlog | Upgrade ownership | Owns future version/pin/update policy for accepted dependencies. |

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
| Incremental/deduplicated encrypted remote/offline backup | CAS dedup is local; no complete encrypted remote bundle | Strongest fit after bundle seal; encryption and many backends | Incremental SQLite transaction chain; backend transport varies | Object/PV backup through plugins | Incremental send, raw encrypted send; tied to ZFS | Iterative memory pre-dump; not durable business-data backup |
| Restore order, compatibility, projections, runtime, rollback | Explicit sequential store order, schema checks, projection seed/reset, pre-snapshot rollback, runtime respawn | Restores files and verifies content; application order external | Transaction-order plan, gap rejection, temp output, fsync, atomic rename | Strong explicit priorities, finalization, partial failure, hooks | Receive validates stream and supports resumable transfer; app order external | Extensive host/image compatibility; can restore process tree, but unsafe authority semantics |
| Drills, corruption injection, RPO/RTO evidence, runbooks | Targeted unit/failure tests; no complete machine-loss drill or measured whole-product RPO/RTO | Repository corruption/check and restore verification tests; operator schedules drills | Disk-full, missing file, chain gap, shutdown, fuzz, and integrity tests | Controller, hook, plugin, ordering, and partial-failure tests | Large functional suite for corrupt/resumable send, raw encryption, scrub | Broad process/resource and fault-injection harness; high environment cost |

### 6.2 Non-functional and integration matrix

| System | Main benefit | Main cost/failure semantics | 1:n and determinism | Security | Maintenance/dependency impact | Expected boundary |
|---|---|---|---|---|---|---|
| Sentinel | Domain authority, owner fencing, ECS semantics, projection reconstruction | Sequential saga can leave intermediate state; coverage is incomplete | One coordinator can enumerate N stores; canonical manifests can be deterministic | Can bind principals, terms, secret references, and revocation freshness | New product code and drills, but no mandatory platform dependency | In-process coordinator plus narrow store adapters |
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
| D1 | Whole-product application-consistent cut and validity | **Reimplement minimal** | Only Sentinel can enumerate its authorities, fence writers, bind authority generations, and publish a manifest last. | restic/Velero/ZFS as cut authority; filesystem copy while live |
| D2 | Existing world Time Machine and bounded replay | **Keep Sentinel** | It already models domain state, dead branches, projection seed, and runtime reconciliation. | Replace with generic file rollback or CRIU |
| D3 | SQLite checkpoint and restore adapter | **Port algorithm/contract** | Litestream supplies the load-bearing continuity, gap, temp-file, fsync, and failure principles without solving cross-store coordination. | Direct live-file copy; one Litestream daemon per DB |
| D4 | redb store capture adapter | **Reimplement minimal** | Use short read transactions or engine-supported consistent copies and verify reopen/table inventory under the global fence. | Treating filesystem snapshot alone as logical consistency |
| D5 | Sealed bundle remote/offline transport | **Integrate** | restic has the best external boundary for encryption, dedup, check, retention, and backend diversity. #705 must approve the dependency and privilege model first. | Embed restic code; make Borg/Kopia simultaneous dependencies |
| D6 | Recovery lifecycle, finalization, partial failure, and ordered resources | **Port algorithm/contract** | Velero's durable phase and ordering model maps well without Kubernetes. | Boolean success flag; unbounded shell hooks; Velero dependency |
| D7 | Host filesystem snapshots | **Reject** | They are neither portable nor application-consistent and cannot be an M0 prerequisite. | Require OpenZFS/btrfs; call a VM snapshot a product backup |
| D8 | Runtime process checkpoint | **Reject** | Old PIDs, sockets, leases, process memory, and credentials must not regain authority after disaster restore. | CRIU restore or microVM-memory restore as canonical state |
| D9 | Projections | **Keep Sentinel** | Rebuild/seed from authoritative state and verify watermarks; do not back up a derived database as authority. | Restore projection file and trust its offsets |
| D10 | Credentials and trust | **Reimplement minimal** | Store encrypted secret material separately or only immutable secret references; current revocation/owner truth wins. | Put plaintext credentials in the data bundle; restore stale certs/terms |
| D11 | Recovery drills and evidence | **Reimplement minimal** | Sentinel-specific cut, corruption, restore, restart, and business invariants need a first-party harness. | Claim RPO/RTO from upstream or build-server timings |

No dependency is authorized by this decision table. D5 routes through #705 and any
accepted version/update contract routes through #656.

## 8. Whole-product recovery contract

### 8.1 `RecoveryPointManifestV1`

The manifest must use a canonical, versioned encoding. A recovery point is invalid
until a complete manifest is durably published and its bundle digest is verified.

```text
RecoveryPointManifestV1 {
  schema_version
  recovery_point_id
  scope                    // single_node | cluster
  state                    // sealed_local | verified_offsite
  created_at_utc
  product_commit
  binary_digests[]
  config_bundle_digest
  organization_generation
  owner_term_or_local_epoch
  fence_generation
  event_cursor
  restore_generation
  world_snapshot_id
  stores[] {
    logical_store_id
    authority_class        // authoritative | derived | ephemeral | secret_ref
    engine
    schema_version
    capture_method
    logical_cursor
    artifact_path
    size_bytes
    sha256
    integrity_receipt
  }
  cas {
    manifest_sha256
    blob_count
    logical_bytes
    stored_bytes
    every_blob_verified
  }
  projection_recipes[] {
    projection_name
    schema_digest
    source_cursor
  }
  runtime_intent {
    schema_version
    digest
    no_live_handles
  }
  credential_set {
    reference_ids[]
    authority_generation
    secret_bytes_in_separate_domain
  }
  encryption {
    bundle_format
    repository_id
    key_reference
  }
  bundle_sha256
  prior_recovery_point_id
}
```

Security invariants:

- Logical store IDs map to compile-time or signed configuration allowlists; a caller
  cannot request arbitrary file capture or restore.
- The manifest contains no credential value, provider prompt, customer content, or
  private infrastructure address.
- A separately encrypted secret bundle, if approved, has a different key,
  retention policy, access principal, and audit trail from the data bundle.
- `organization_generation`, owner term, and credential authority are revalidated
  against current authority during restore. Backup contents cannot resurrect revoked
  principals.
- Canonical bytes are hashed and authenticated. A plain public SHA-256 is integrity
  evidence, not authorization.

### 8.2 Durable capture state machine

```text
Requested
  -> ValidatingCoverage
  -> FencingWriters
  -> DrainingOrFreezing
  -> CapturingAuthoritativeStores
  -> VerifyingReferences
  -> SealingLocal
  -> Exporting
  -> VerifyingOffsite
  -> Restorable
```

Any phase can enter `Aborted` before local seal or `Quarantined` after a corrupt,
incomplete, or policy-invalid artifact. `PartiallyFailed` is observable but never
restorable. The durable journal records phase, attempt, completed store receipts,
error class, retryability, and operator resolution. Retries use
`recovery_point_id + request_digest`; a digest conflict is non-retryable.

No user-visible "backup succeeded" result is returned before `SealedLocal`. An
offsite policy may require `VerifiedOffsite` before the point satisfies its RPO
class.

### 8.3 Application-consistent cut

The cut is a bounded fenced saga, not distributed 2PC:

1. Validate that every registered authoritative store has exactly one capture
   adapter and every derived/ephemeral store has a rebuild/reconcile contract.
2. Persist `Requested` and the canonical request digest outside the stores being
   replaced.
3. Acquire the world/product mutation fence. Block new customer mutations,
   governance commits, assignments, provider starts, runtime executions, snapshot
   pruning, and schema migrations with typed `RecoveryInProgress`.
4. Drain commits already admitted. Pending event, workflow, and completion outboxes
   may remain pending, but no row may be `provider_in_flight` or
   `action_claimed` without a durable recovery contract. Record counts and oldest
   age.
5. Capture the current organization/owner generation, product event cursor, restore
   generation, and runtime-intent digest.
6. Create the WorldSnapshot under the same fence. Persist and verify CAS pins before
   the point can proceed.
7. Capture each redb store in a short engine-consistent transaction. Capture each
   SQLite store with a WAL-aware backup/checkpoint adapter and an explicit
   transaction cursor. Never copy a live main file alone.
8. Enumerate all CAS hashes referenced by captured metadata and workflow/delivery
   manifests. Verify every blob by decoding and hashing; pin the final set.
9. Capture allowlisted config bytes and credential references. Do not place
   plaintext credentials in the data staging tree.
10. Fsync every staged file and directory. Run engine integrity/open checks and
    cross-store cursor/reference checks.
11. Write the canonical manifest to a temporary name, fsync it, rename it, fsync the
    parent, then record `SealedLocal`. The manifest is published last.
12. Release the mutation fence. Export the immutable sealed directory through the
    approved transport. Verify repository data and a staged restore before recording
    `VerifiedOffsite`.

If any step before 11 fails, no recovery point is advertised. Completed immutable
artifacts may be garbage-collected later by the recovery journal. Failure to unpin or
clean staging is a visible maintenance condition, not permission to claim success.

### 8.4 Restore order and rollback

A disaster restore never mutates the only remaining source bundle.

1. Authorize a recovery incident and enter a durable global restore fence.
2. Fetch to a quarantine/staging directory. Authenticate the manifest and verify
   every file, CAS object, schema, product commit, and supported migration path.
3. Select a compatible, verified binary. Validate configuration semantically.
   Resolve current credentials and revocation state from the independent trust
   authority; rotate service and provider credentials before reopening callers.
4. Restore CAS blobs first into a staged CAS and verify their plaintext SHA-256
   identities.
5. Restore staged authoritative databases: `events.db`, `state.redb`,
   `metadata.redb`, control plane, hippocampus, evolution, nightrun, Gaia memory if
   declared authoritative, and the company workflow journal. Run engine integrity
   checks after each restore.
6. Verify cross-store event cursors, owner/organization generations, workflow
   authority, outbox state, artifact ownership, CAS references, and snapshot IDs.
   Any mismatch keeps the system fenced.
7. Replace store generations under an offline restore journal. Keep the old
   generation until post-start verification. A crash resumes from the journal rather
   than guessing from filenames.
8. Rebuild/seed projections from authoritative stores and verify every watermark
   before read APIs become ready.
9. Invalidate all restored runtime handles, sessions, leases, in-flight network
   connections, and caller tokens. Reconcile desired runtime intent through the
   normal runtime registry. Do not restore process memory.
10. Start publishers, projections, daemon mutation paths, authenticated internal
    callers, APIs, and UI in dependency order. Run positive and negative probes.
11. Record a restore receipt containing the manifest ID, deployed binary digests,
    rebuilt watermarks, rotated credential generation, and business invariant
    results. Only then release the fence.

On failure, stop and keep the fence. If the old local generation is intact, the
restore journal can switch back to it and repeat all integrity checks. If it is not,
select another verified RecoveryPoint. Never partially continue on a mixture of old
and restored stores.

## 9. RPO, RTO, drills, and failure injection

The following are proposed product SLO classes, not measurements:

| Class | Purpose | Required recovery point | RPO target | RTO target | M0 class |
|---|---|---|---|---|---|
| `TM_LOCAL` | Operator Time Machine rollback on an intact host | Valid WorldSnapshot and CAS pins | Snapshot interval; no disk-loss claim | Existing in-process target only after current-head measurement | `M0_HARDENING` already owned by #250/#264 |
| `M0_SINGLE_NODE_DR` | Loss/corruption of the product data directory holding authoritative customer work | `VerifiedOffsite` whole-product manifest plus separate current trust material | At most 15 minutes | At most 60 minutes | `M0_HARDENING`; #650 acknowledgement required |
| `OFFLINE_SECURITY` | Ransomware/operator credential compromise | Independently administered immutable/offline copy | At most 24 hours | At most 4 hours | `M0_HARDENING` for production customer work |
| `CLUSTER_RECOVERY` | Node loss, quorum recovery, geo recovery | Quorum-accepted RecoveryPoint and stale-term/cert rejection | Defined by #556 | Defined by #556 | `POST_M0` for the single-node product |

Targets must be accepted by the product owner before implementation. A lower RPO is
not free: it increases capture frequency, remote bandwidth, retained objects, key
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
| Crash after fence, before first capture | Restart finds `FencingWriters`, aborts or resumes deterministically, and releases no false recovery point. |
| Crash after any store capture | Completed receipt is reused only if request digest and source cursor match; otherwise quarantine. |
| Crash after WorldSnapshot save, before CAS pin | Recovery journal completes pins or invalidates the point; no restorable manifest exists. |
| SQLite WAL changes or chain gap | Capture/restore fails with typed cursor/gap error. |
| Disk full on write, fsync, rename, or manifest publish | No sealed point; old points remain untouched; health reports the phase. |
| Missing/corrupt CAS blob | Seal and restore both fail before mutation/readiness. |
| redb/SQLite schema incompatibility | Fail before store replacement unless a pinned crash-safe migration exists. |
| Projection rebuild crash | Resume idempotently from authoritative cursor; mutation remains fenced until watermarks pass. |
| Workflow evidence/assignment authority mismatch | Item remains blocked; no completion or duplicate action. |
| Runtime crash during restore | Old process tree is killed/reaped; desired intent is reconciled once by idempotency key. |
| Credential bundle missing or backed-up principal revoked | Restore stays fenced; current authority must provision/rotate credentials. |
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
| Control-plane, memory, evolution, nightrun, config, and credentials are outside Time Machine | `M0_HARDENING` | Current path inventory; proposed coverage registry |
| Workflow journal is absent on the baseline and needs a recovery adapter | `M0_HARDENING` | #695 owns schema/journal; adapter contract below |
| Projection file should remain derived | `M0_HARDENING` | Existing projection seed/reset path; no new authority |
| Stale runtime handles and credentials must not be restored | `M0_HARDENING` | Existing teardown/respawn and CRIU negative review; #694/trust owners |
| Mandatory ZFS/btrfs substrate | `POST_M0` | Optional defense only; product portability wins |
| CRIU/live process memory restore | `POST_M0` and rejected | Security and compatibility costs exceed value |
| Multi-node/quorum/geo recovery | `POST_M0` for single-node acceptance | #556 and G6/G8 own cluster durability |

No finding is marked `BLOCKS_M0` merely because an upstream has a feature Sentinel
lacks. #650 must decide whether the proposed `M0_SINGLE_NODE_DR` targets are binding
before production acceptance.

## 11. Proposed implementation issue contracts

Per the research issue, these contracts are proposals for ORC review. They are not
live issues and must not be materialized before the synthesis is approved.

### P1. M0 RecoveryPoint coordinator and coverage registry

**Class:** `M0_HARDENING`

**Depends on:** #695 recovery adapter contract; #650 target/SLO approval

**Scope:** manifest schema, registered logical stores, global fence, durable capture
journal, redb/SQLite adapters, WorldSnapshot/CAS binding, config and credential
reference capture, seal-last publication, health.

**Acceptance contract:**

- every authoritative store is registered exactly once; unknown/uncovered stores
  fail readiness when DR is enabled;
- crash at every transition resumes or aborts without a false sealed point;
- request-id/digest retries are idempotent and conflicts are typed;
- no arbitrary paths, secret bytes, derived projection authority, or live runtime
  handles enter the manifest;
- current-main automated tests cover cut ordering, outbox states, CAS pin window,
  disk full, fsync/rename, schema mismatch, and manifest tamper;
- target-runtime tests and benchmarks are token-free and issue-specific;
- rollout is default-off, then local seal-only, then offsite-required;
- rollback disables new capture while preserving old verified points;
- TOGAF delta replaces "complete snapshot" ambiguity with WorldSnapshot versus
  RecoveryPoint terminology.

### P2. Sealed-bundle transport, independent copy, and key ownership

**Class:** `M0_HARDENING`

**Depends on:** P1 and a #705 decision; upgrades owned by #656

**Scope:** external transport port, evaluated restic integration, repository
credentials, immutable/offline policy, verification, retention, prune safety,
restore-to-staging.

**Acceptance contract:**

- transport receives only a read-only sealed directory and expected manifest digest;
- backup and prune/delete principals are separated where the backend permits;
- repository key is recoverable by an independently controlled break-glass process
  and is never in the repository;
- interrupted upload, duplicate upload, backend rollback, corrupt pack, lost key,
  compromised delete credential, and retention race have negative tests/runbooks;
- a second operator restores and verifies a complete bundle;
- benchmarks measure product bundle sizes and target runtime, not upstream claims;
- rollback removes the integration without changing the RecoveryPoint format.

### P3. Whole-product restore runner and disaster drills

**Class:** `M0_HARDENING`

**Depends on:** P1, P2, #694 durable runtime intent, #695 workflow journal, #696
delivery lineage

**Scope:** quarantine, compatibility validation, staged restore, generation swap,
projection rebuild, trust refresh/rotation, runtime reconciliation, business probes,
restore receipt, scheduled drills.

**Acceptance contract:**

- exact restore order in section 8.4 is encoded as a versioned state machine;
- failpoints before/after every store replacement and projection/runtime transition
  prove restart recovery;
- wrong tenant/owner generation, revoked principal, missing evidence artifact,
  missing CAS blob, stale runtime lease, corrupt manifest, and incompatible schema
  fail closed;
- successful drill proves customer agreement/project/work state, event/outbox
  idempotency, artifact ownership, projections, runtime restart, and no duplicate
  external action through deterministic fakes;
- rollout runs first in an isolated authorized target; rollback retains the prior
  local generation and verified source point;
- RPO/RTO results include sidecars and exact manifest/binary hashes.

### P4. Workflow-journal recovery adapter

**Class:** `M0_HARDENING`

**Owner overlap:** refine #695; do not create a duplicate if #695 accepts the delta

**Scope:** narrow `WorkflowRecoveryPort` returning schema, transaction/event cursor,
authority generation, digest, outbox/evidence state, integrity receipt, and
restore/reopen validation.

**Acceptance contract:**

- capture is invoked only under the product fence and never accepts caller authority;
- completion-evidence requests, authority conflicts, claimed actions, and pending
  outbox rows remain recoverable without replaying a completed action;
- restore rejects cross-tenant/project/assignment receipt replay and stale
  organization generation;
- migration crash points and restart tests remain valid after bundle restore.

### P5. Recovery coverage, retention, and operator health

**Class:** `M0_HARDENING`

**Owner overlap:** #250, #264, #481, and #650

**Scope:** coverage inventory, last-sealed/last-offsite age, restore-test age,
retention protection, pin leaks, repository health, typed operator status, alerts.

**Acceptance contract:**

- health names the exact uncovered or stale store without paths/secrets;
- retention never removes the last verified point for each required RPO class or a
  point referenced by an in-progress drill/restore;
- pin leaks, disk pressure, repository check failures, and missed RPO targets are
  observable and bounded;
- negative tests prove health cannot be forged by a transport success without
  manifest and restore receipts.

### Owner-resolution rule

After ORC approves the synthesis:

1. ask #650, #695, #696, #705, #656, #250, #264, and #481 owners to accept the
   deltas above;
2. update an existing issue when it owns the complete acceptance contract;
3. materialize only genuinely uncovered P1-P5 contracts in dependency order;
4. require reciprocal links, `quality:ready`, target runtime, benchmarks, rollout,
   rollback, negative criteria, and TOGAF delta before implementation;
5. keep AC-N5 open until that live owner readback exists.

The following delivery envelope is mandatory if ORC materializes or merges any of
P1-P5 into an existing owner:

| Contract | Dependencies | Negative criteria | Target-runtime tests and benchmarks | Rollout | Rollback | TOGAF delta |
|---|---|---|---|---|---|---|
| P1 coordinator | #650 target approval and #695 adapter | No uncovered authority, arbitrary path, false seal, secret byte, live handle, or derived projection authority | Authorized single-node target: pause/seal/export-ready duration, bytes, CPU/IO sidecars, crash matrix, and exact manifest readback | Default off -> local seal -> scheduled local seal | Disable capture; old WorldSnapshot behavior and prior verified points remain readable | Define WorldSnapshot versus whole-product RecoveryPoint and seal-last saga |
| P2 transport | P1, #705; upgrades #656 | No live-store input, shared data/key bundle, all-powerful unattended credential, or prune of last-known-good | Local mock backend first, then authorized independent storage: upload/verify/restore latency and dedup ratio using Sentinel bundles | Read-only shadow export -> sampled check -> offsite-required policy | Remove transport adapter while preserving canonical local bundles | Document external repository as transport, never authority or cut |
| P3 restore/drills | P1, P2, #694, #695, #696 | No in-place first mutation, mixed generations, stale trust, raw handle restore, or readiness before watermarks/business probes | Authorized isolated single-node restore: quarantine through readiness, p50/p95/max over issue-defined fixtures, sidecars, corruption/restart matrix | Dry validation -> staged restore drill -> scheduled full drill -> production incident runbook | Restore journal switches to intact prior generation or another verified point; fence remains on failure | Encode restore order, trust refresh, projection rebuild, runtime reconciliation, and receipts |
| P4 workflow adapter | #695 schema and authority contract | No caller authority, cross-tenant receipt, duplicate action, busy loop, or non-terminal evidence loss | Deterministic fake plus final single-node integration: capture/restore latency, journal size, restart and migration failpoints | Adapter default off until coordinator registry accepts its schema | Disable registration; workflow remains operational but DR readiness reports uncovered authority | Add workflow journal, evidence, outbox, and authority generation to RecoveryPoint coverage |
| P5 health/retention | P1-P3 and #250/#264/#481 policy | No green health from transport-only success, stale drill, pin leak, missed RPO, or unverified point | Authorized single-node soak: health evaluation cost, retention/prune duration, disk growth, missed-schedule and repository-failure probes | Observe-only -> alert -> readiness gate after stable evidence | Return to observe-only; never weaken immutable retention | Add coverage freshness, offsite verification, drill age, and RPO status |

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
| AC-N5 | No gap is declared closed. Proposed owners and the post-approval resolution procedure are explicit. | PASS at research boundary; live owner acceptance pending ORC |

## 13. Limitations and review questions

- This source audit did not run upstream test suites. The reviewed tests demonstrate
  upstream intent and adversarial coverage, not compatibility with Sentinel.
- This issue did not mutate a runtime and therefore has no current machine-loss,
  pause-time, RPO, or RTO measurement.
- Exact config and credential authority for a disaster host needs security-owner
  approval. The study intentionally rejects restoring stale trust from a data
  archive.
- The final #695 workflow schema is not on this baseline. P4 is a required adapter
  contract, not a claim about code that does not exist here.
- restic is a conditional recommendation. #705 may select Borg, Kopia, an object
  store SDK, or no new dependency after testing the same sealed-bundle boundary.
- ORC must decide whether to approve D1-D11 and whether P1-P5 should update existing
  owners or become new issues. Until then AC-5 and AC-6 are not final.

Review should answer:

1. Is `M0_SINGLE_NODE_DR` with proposed RPO <= 15 minutes and RTO <= 60 minutes the
   accepted production target?
2. Is a separately administered offline/immutable copy mandatory before customer
   work is accepted?
3. Which store is authoritative for Gaia console memory?
4. Will #695 implement P4 directly?
5. Does #705 authorize a restic proof of concept under the exact external boundary,
   or require a dependency-free transport first?
