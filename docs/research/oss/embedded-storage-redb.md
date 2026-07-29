# Embedded Storage and redb Operating Contract

**Issue:** [#708](https://github.com/silentspike/project-sentinel/issues/708)
**Parent:** [#659](https://github.com/silentspike/project-sentinel/issues/659)
**Baseline:** `5471d844d219874f06eb5f8c9d13d797464530dd`
**Runtime target:** `NONE`
**Status:** Comparative source study and implementation contract

## Executive Verdict

Project Sentinel should keep redb 3.1 as its embedded transactional metadata
engine. Replacing it with LMDB, RocksDB, Fjall, sled, or another embedded
engine would add operational and dependency cost without fixing Sentinel's
actual correctness gaps.

The important finding is at the boundary around redb:

- redb atomically commits metadata inside one database;
- the Artifact Plane stores compressed chunk bytes in segment files outside
  redb;
- the blob CAS stores SHA-256-addressed files outside redb;
- snapshots, pins, migrations, projections, and the event cursor are committed
  by different stores;
- current reference counts are treated too much like deletion authority even
  though they are derived state.

This means a successful redb transaction does not by itself prove that every
referenced byte is durable, that a backup is a consistent application cut, or
that garbage collection cannot race a new reference. The required design is a
small Sentinel-owned operating layer around the existing engines, not a new
storage engine.

The accepted direction is:

1. Keep and wrap redb for transactional metadata.
2. Publish bytes durably before making metadata reachable.
3. Treat reference counts as rebuildable accelerators, never as the sole delete
   authority.
4. Introduce typed reachability claims and a monotonic storage generation for
   ingest, readers, snapshots, backup, restore, migration, and GC.
5. Reconcile incomplete operations at startup and fail readiness when a
   committed reference points to missing or corrupt bytes.
6. Add deterministic filesystem-fault and process-crash tests based on the
   compact mechanisms used by SQLite and RocksDB.
7. Use redb's integrity, quick-repair, savepoint, and compaction facilities
   through an explicit per-store policy.

No engine replacement is justified by this study. Any future dependency change
still requires [#705](https://github.com/silentspike/project-sentinel/issues/705)
and the upgrade contract in
[#656](https://github.com/silentspike/project-sentinel/issues/656).

## Plain-Language Model

redb is the ledger that says which content exists and who references it. The
CAS and segment files contain the actual content. A ledger row is not the
content itself.

Sentinel is correct only when these statements are simultaneously true:

- every reachable ledger entry points to durable bytes with the expected size
  and digest;
- every deletion proves that no committed manifest, snapshot, active reader,
  ingest, backup, restore, migration, or uncertain remote node still needs the
  bytes;
- a restart can distinguish a harmless orphan file from a fatal dangling
  reference;
- a backup identifies one generation of metadata and exactly the content set
  reachable from that generation.

redb copy-on-write is not content deduplication. Copy-on-write preserves old
database pages for transactional readers and crash recovery. Sentinel's
deduplication recognizes equal application content and stores that content once.
Long-lived redb readers or savepoints can therefore increase database file size
even while CAS deduplication is working perfectly.

## Method

This issue made no runtime mutation and collected no performance numbers. The
study used four evidence layers:

1. A source-level inventory of every Sentinel redb database, content-addressed
   byte store, reference table, pin table, trash queue, snapshot path, restore
   path, delete path, and load-bearing storage test at the exact baseline
   commit.
2. Live issue readback for existing ownership and sequencing.
3. A pinned source and test review of seven upstream projects.
4. Mechanism-level comparison. An upstream benchmark is never treated as a
   Sentinel result.

The candidate search considered redb, heed/LMDB, SQLite, Fjall, RocksDB, sled,
and the SQLite-derived Turso/Limbo line. Turso is not shortlisted here because
Sentinel already uses SQLite through `rusqlite`, and the detailed event-store
comparison belongs to [#709](https://github.com/silentspike/project-sentinel/issues/709).

## Sentinel Storage Baseline

### Engine Inventory

| Store | Engine and role | Authoritative data | External byte dependency |
| --- | --- | --- | --- |
| `StateStore` | redb, fenced agent hot state | Twelve per-agent state tables | None for the rows themselves |
| `ClusterMetaStore` | redb, control-plane authority | Owner terms, local roles, saga state, snapshot metadata | Cluster RPC outcomes and participant state |
| `ArtifactPlane` | redb metadata plus segment files | Object metadata, manifests, chunk locations, refcounts, sessions, trash | Compressed BLAKE3-128 chunks in segment files |
| Filesystem metadata | redb | Inodes, directory entries, CAS refcounts, snapshot pins, trash | SHA-256 blob files in `CasStore` |
| Hippocampus | redb | Goals, episodes, facts, archive, metadata | None |
| Gaia graph/control state | redb | Graph and control-plane state | Gaia backup files |
| Event store and projections | SQLite through `rusqlite` | Event history, outbox, offsets, projections | WAL/database files managed by SQLite |

The workspace pins `redb = "3.1"`; the baseline lockfile resolves redb 3.1.1.
SQLite 0.38 with the bundled engine is used by Limbo fallback, projection,
Nightrun, dashboard, and Gaia-memory paths.

### redb StateStore

`crates/sentinel-redb/src/lib.rs` defines twelve agent-state tables and opens
them in one database. `dump_agent_tables()` and `restore_agent_tables()` cover
all twelve tables; restore uses one redb write transaction. Fenced writes
recheck owner authority at commit.

This is a sound local transaction boundary. It does not cover ECS state,
filesystem metadata, CAS bytes, projection offsets, or the event cursor.

### Cluster Metadata

`crates/sentinel-redb/src/cluster_meta.rs` stores global owner terms separately
from local owner role and local saga markers. That separation is required for
cluster recovery, but it also means backup, repair, and migration code must not
interpret a byte-for-byte database copy as global authority without validating
node identity and generation.

### Artifact Plane: BLAKE3 Chunk Store

The Artifact Plane defines:

- `FS_OBJECTS`
- `FS_MANIFESTS`
- `FS_CHUNKS`
- `FS_CHUNK_REFCOUNT`
- `FS_TRASH_QUEUE`
- `FS_OBJECT_REFS`
- `FS_INGEST_SESSIONS`

`FS_CHUNKS` stores only `ChunkLocation`; compressed bytes live in append-only
segment files. Ingest performs a read-only dedup precheck, appends new bytes to
segments outside redb, then commits locations, refcount deltas, the manifest,
object metadata, and session removal in one redb transaction.

That transaction is metadata-atomic, but the segment writer is not synchronized
before the redb commit. A power loss can therefore preserve a manifest and
location whose bytes never reached stable storage.

Concurrent ingests of the same new chunk can both pass the read precheck and
append duplicate bytes. The later redb write can replace the location, leaving
dead segment bytes. This is space leakage rather than content corruption when
both copies are valid, but it must be detectable and bounded.

### Blob CAS: SHA-256 File Store

Filesystem metadata is a separate redb database from the Artifact Plane. Its
load-bearing tables are `FS_INODES`, `FS_DIRENTS`, `CAS_REFCOUNT`,
`FS_TRASH_QUEUE`, and `FS_SNAPSHOT_BLOB_REFS`. `CAS_REFCOUNT` and the trash
queue are keyed by the full 32-byte SHA-256 blob digest; snapshot pins use
`(snapshot_id, sha256_digest)`. The Artifact Plane has a different
`FS_TRASH_QUEUE`, keyed by the 16-byte BLAKE3 chunk hash. Equal table names do
not create a shared transaction or a shared deletion authority across these
two databases.

`CasStore::store()` hashes bytes, writes a temporary file, and renames it into
the canonical path. It does not synchronize the file or containing directory.
The distributed pull path is stronger: it verifies content, synchronizes the
file, renames, and attempts to synchronize the parent directory, although parent
open/sync failures are currently ignored.

The same logical CAS therefore has two different publication guarantees.

### Self-Describing Content Identity

`crates/sentinel-common/src/block_ref.rs` correctly separates the two content
namespaces:

- BLAKE3-128 for trusted-cluster Artifact Plane chunks;
- SHA-256 for whole blob integrity and remote transport boundaries.

`BlockRef` carries namespace, algorithm, digest, size, optional chunk profile,
and version. This is the right foundation and must be retained. The current
chunk profile is the versioned `gear-v1` family; changing boundaries creates a
new chunk namespace in practice and must never silently reinterpret existing
hashes.

### Sentinel Source Map

Line numbers below were revalidated against the exact report baseline. They are
navigation aids; implementation must re-run the inventory against its final
main.

| Contract | Current-baseline source |
| --- | --- |
| StateStore table set | `crates/sentinel-redb/src/lib.rs:21-40` |
| StateStore open and fenced write entry | `crates/sentinel-redb/src/lib.rs:72-107` |
| Agent/all-table dump and atomic redb restore | `crates/sentinel-redb/src/lib.rs:723-795` |
| Owner-fence commit recheck | `crates/sentinel-redb/src/lib.rs:946-986` |
| Cluster owner, local role/saga, and term-snapshot metadata | `crates/sentinel-redb/src/cluster_meta.rs:1-70` |
| Artifact object/manifest/chunk/refcount/trash/ref/session tables | `crates/sentinel-fs/src/artifact.rs:41-65` |
| Artifact dedup precheck, segment append, and metadata commit | `crates/sentinel-fs/src/ingest.rs:121-193` |
| Batch segment append and metadata commit | `crates/sentinel-fs/src/ingest.rs:269-330` |
| Segment append and unused sync primitive | `crates/sentinel-fs/src/segment.rs:95-116`, `269-275` |
| Peer-pulled chunk verification/publication | `crates/sentinel-fs/src/artifact.rs:348-387` |
| Artifact trash selection/deletion and object release | `crates/sentinel-fs/src/gc.rs:20-159` |
| Normal SHA-256 CAS publication and physical GC | `crates/sentinel-fs/src/cas.rs:96-176` |
| Peer-pulled blob durable publication and temp cleanup | `crates/sentinel-fs/src/cas.rs:268-345` |
| Filesystem inode, dirent, SHA-256 refcount, trash, and snapshot-pin tables | `crates/sentinel-fs/src/metadata.rs:20-39` |
| Blob refcount, trash, and snapshot pin operations | `crates/sentinel-fs/src/metadata.rs:371-540` |
| Metadata dump/restore and destructive GC | `crates/sentinel-fs/src/metadata.rs:621-723` |
| Snapshot cut, persistence, then separate blob pin | `services/sentinel-daemon/src/snapshot.rs:131-204` |
| Restore preflight and refcount-derived blob validation | `services/sentinel-daemon/src/orchestrator.rs:3604-3629` |
| Sequential redb/FS/ECS/projection restore and rollback | `services/sentinel-daemon/src/orchestrator.rs:3944-4167` |
| Existing process-local restore failure points | `services/sentinel-daemon/src/orchestrator.rs:8665-8673` |
| Gaia live-file backup and write/rename restore | `crates/sentinel-gaia-memory/src/backup.rs:42-52`, `134-199` |
| Hippocampus load-modify-store append paths | `crates/sentinel-hippocampus/src/store.rs:98-103`, `219-248`, `283-292` |
| Versioned/namespaced `BlockRef` contract | `crates/sentinel-common/src/block_ref.rs:1-17`, `121-227` |
| StateStore basic/MVCC acceptance tests | `crates/sentinel-redb/tests/acceptance.rs:14-110` |
| StateStore API-pattern dump/restore regression | `crates/sentinel-redb/src/lib.rs:1397-1426` |
| Artifact dedup, GC, and multi-format integration tests | `crates/sentinel-fs/tests/integration.rs:20-611` |
| Restore preflight and injected mid-commit rollback tests | `services/sentinel-daemon/src/orchestrator.rs:8482-8710` |
| Gaia backup/restore roundtrip and refusal tests | `crates/sentinel-gaia-memory/src/backup.rs:256-307` |

These tests prove their named process-local behaviors. They do not prove a
power-loss barrier, cross-database atomicity, or race-free deletion. The open
implementation owners below carry those missing proofs; issue state is not used
as a substitute for source evidence.

The current target-architecture baseline describes the 1:n CAS model and redb
pointer shorthand at
`docs/architecture/togaf-architecture-guide.html:229-243`, selects redb and
sentinel-fs at
`docs/architecture/togaf-architecture-guide.html:716-719`, describes dedup and
metadata-only hits at
`docs/architecture/togaf-architecture-guide.html:856-890`, and keeps the exact
RAM existence index subordinate to the durable store at
`docs/architecture/togaf-architecture-guide.html:923-930`. The storage choices
are repeated in
`docs/architecture/togaf-architecture-guide.html:1375-1377`. Those are target
statements, not proof that every publication, backup, restore, and GC boundary
already satisfies the target.

Live issue readback established the active ownership graph in this report.
Runtime incident or host evidence was neither required nor authorized for this
runtime-target `NONE` study, so no correctness decision relies on an uninspected
incident feed. The source-proven failure windows are routed to implementation
owners with deterministic failure tests instead.

The inventory also found two explicit `Durability::None` uses in
`crates/sentinel-fs/src/artifact.rs:233-238` and
`crates/sentinel-fs/src/metadata.rs:940-944`. These are not automatically bugs:
the implementation issue must prove that each table is rebuildable from a
durable authority and that startup remains closed until rebuild completes.

## Current Correctness Findings

| ID | Severity | Finding | Failure outcome | Classification |
| --- | --- | --- | --- | --- |
| S-01 | High | Segment bytes are appended but not synchronized before the manifest/location redb commit. | A committed manifest can point to truncated or absent bytes after power loss. | `M0_HARDENING` |
| S-02 | High | Normal SHA-256 CAS publication renames without file and directory synchronization. | A committed filesystem reference can survive while its blob does not. | `M0_HARDENING` |
| S-03 | Critical | Artifact trash deletion does not recheck current refcount and ingest does not cancel a stale trash entry. | A re-referenced chunk can be removed from the index. | `M0_HARDENING`; promote to `BLOCKS_M0` if the active M0 workflow exercises artifact GC |
| S-04 | Critical | Metadata GC checks refs and pins before unlink, performs physical delete outside redb, then rechecks only refs. | A new snapshot pin or reference can race physical deletion. | `M0_HARDENING`; promote to `BLOCKS_M0` when destructive GC is enabled |
| S-05 | High | Snapshot persistence and snapshot-blob pinning are separate operations. | A retained snapshot can be visible but unprotected from GC after a crash. | `M0_HARDENING` |
| S-06 | High | Restore validates from stored refcounts and commits redb, FS metadata, ECS, and projection in a process-local saga. | A process crash can leave a mixed-generation application state. | `M0_HARDENING` |
| S-07 | High | Gaia backup reads a live redb file directly; atomic restore uses write plus rename without durable file/directory sync. | Backup may not represent an engine-consistent cut; acknowledged restore may disappear after power loss. | `M0_HARDENING` |
| S-08 | Medium | Sentinel has no redb integrity, quick-repair, savepoint-lifetime, or compaction operating policy. | Slow recovery, unbounded file growth, or corruption discovered after readiness. | `M0_HARDENING` |
| S-09 | High | Hippocampus append paths load in one transaction and overwrite in a later transaction. | Concurrent updates for the same agent can be lost. | `M0_HARDENING` |
| S-10 | Medium | Schema/version envelopes are inconsistent across raw JSON rows. | Upgrades can decode incompatible rows ambiguously or fail late. | `M0_HARDENING` |
| S-11 | High | Startup cleanup removes temporary files but does not reconcile all orphan bytes, dangling refs, impossible refcounts, incomplete sessions, or mixed profiles. | Readiness can report healthy while the store is internally inconsistent. | `M0_HARDENING` |

None of these findings proves that redb itself violated ACID. They are Sentinel
composition defects or missing operating contracts.

## Deduplication and Copy-on-Write Contract

### Separate Mechanisms

| Mechanism | Identity | What it saves | What it cannot prove |
| --- | --- | --- | --- |
| redb copy-on-write | Database pages and transaction roots | Crash-safe metadata updates and MVCC readers | That external CAS/segment bytes are durable or unique |
| Artifact Plane dedup | `BlockRef` chunk namespace, BLAKE3-128, chunk profile | Repeated content-defined chunks | Whole-object SHA-256 integrity or remote hostile-input security |
| Blob CAS dedup | SHA-256 whole-blob namespace | Repeated complete blobs/files | Manifest reachability, snapshot retention, or chunk-level reuse |
| In-memory existence index proposed by #629 | Exact cache of durable chunk identities | Avoids redb lookup contention | Durability or delete authority; it must rebuild from committed metadata |

### Unified Invariants

The implementation must make these invariants executable:

1. **Durable reachability:** every committed manifest references only bytes that
   have passed size and digest verification and have completed file plus
   directory durability barriers.
2. **Orphans over dangling refs:** a crash may leave unreferenced bytes. It must
   never leave a reachable reference to missing or unverifiable bytes.
3. **One publication order:**
   `temp -> stream/hash -> verify -> sync file -> rename -> sync directory ->
   metadata/reachability commit -> holder advertisement`.
4. **Derived refcounts:** reference counts can accelerate decisions, but the
   canonical count is rebuildable from committed manifests, object refs,
   snapshot refs, and durable active claims.
5. **Conservative deletion:** uncertainty means retain. Delete requires a
   generation-bound claim proving absence from every local reachability root and,
   for cluster data, the distributed guard owned by #499.
6. **Typed active claims:** readers, ingests, snapshots, backups, restores,
   migrations, and pending advertisements hold claims that GC can observe.
7. **Versioned identity:** namespace, codec, hash algorithm, chunk profile, and
   schema version are part of content identity or the generation manifest.
8. **Generation-safe backup:** metadata and bytes belong to one storage
   generation. Restore rejects a missing, duplicate, mixed, or unsupported
   generation.
9. **Fail-closed readiness:** a dangling reachable reference, digest mismatch,
   incompatible schema, or unresolved interrupted delete blocks the owning
   capability from readiness.
10. **Idempotent reconciliation:** restarting reconciliation cannot create a
    reference, decrement twice, delete a live block, or reinterpret content under
    a new profile.

### Required State Model

The exact table names may be refined by implementation, but the state semantics
must not be weakened:

```text
StorageGeneration {
    generation_id,
    schema_set,
    block_ref_version,
    chunk_profiles,
    created_at,
    parent_generation,
    state: Building | Committed | Retiring | Rejected
}

ReachabilityClaim {
    block_ref,
    claimant_kind: Manifest | Snapshot | Reader | Ingest | Backup |
                   Restore | Migration | RemoteUncertainty,
    claimant_id,
    generation_id,
    state: Active | Released,
    expires_at_if_lease_based,
    digest
}

DeleteClaim {
    block_ref,
    observed_generation,
    observed_reachability_digest,
    state: Claimed | Unlinked | Finalized | Cancelled,
    attempt,
    boot_id
}
```

Normal committed manifests and snapshot references are not leases and do not
expire. Only genuinely ephemeral claims may use an expiry, and recovery must
distinguish a dead process from a slow live holder.

## Crash and Concurrency Matrix

| Boundary or race | Allowed post-crash state | Required recovery | Forbidden outcome |
| --- | --- | --- | --- |
| Before temp creation | No metadata and no bytes | No-op | Reachable metadata |
| During temp write/hash | Partial temp only | Remove or resume only with verified journal identity | Publish partial bytes |
| After verification, before file sync | Verified but non-durable temp | Repeat sync and publish | Metadata commit |
| After file sync, before rename | Durable temp | Idempotent rename | Duplicate reachability |
| After rename, before directory sync | Canonical name may disappear on power loss | Repeat or quarantine based on durable journal | Metadata commit before name durability |
| After directory sync, before metadata commit | Durable orphan block | Reconcile and either attach to the same operation or collect after grace | Treat orphan as corruption |
| During metadata/ref/manifest transaction | Old or new complete redb state | redb recovery selects a valid commit | Partial manifest/refcount transaction |
| After metadata commit, before advertisement | Locally complete object, no remote holder claim | Re-advertise from durable inventory | Remote claims bytes before local durability |
| Concurrent identical ingest | At least one canonical durable block; duplicate dead bytes allowed temporarily | Single-flight when practical; reconcile dead segment bytes | Two incompatible locations for one identity |
| Ingest versus artifact GC | Ingest claim or committed manifest prevents deletion | Cancel stale trash and retry GC | Delete bytes needed by the ingest/manifest |
| New ref versus blob unlink | Generation or delete claim serializes the race | New ref cancels delete before unlink, or waits and republishes verified bytes | Commit a ref after irreversible unlink without bytes |
| Snapshot save versus GC | Snapshot reference/pin is committed in the same cut or via an outcome-probed saga | Complete pin before exposing snapshot as retained | Visible unpinned snapshot |
| Backup during ingest | Backup cut includes only fully committed generation; active ingest is excluded or completed | Retry at a later generation | Bundle contains metadata for uncommitted bytes |
| Restore into non-empty store | Target generation staged and validated separately | Atomic generation switch or recoverable forward saga | Merge incompatible profiles or retain stale target rows |
| Crash after redb restore only | Activation remains closed | Resume forward or roll back from a durable pre-restore generation | Serve mixed redb/FS/ECS/projection state |
| Refcount rebuild during writes | Rebuild uses a stable generation and applies later deltas | Compare reachability digest before publish | Replace current counts with stale rebuild |
| Missing reachable block | Capability not ready | Pull from verified holder when allowed, restore from backup, or manual recovery | Silently drop reference or synthesize content |
| Corrupt reachable block | Quarantine block and owning object | Verify alternate replica/backup by digest | Return corrupt bytes |
| Long-lived redb reader | Old pages retained; compaction blocked or deferred | Expose age and owner, enforce read lease policy at caller boundary | Force-close memory safety or compact under reader |
| Mixed chunk profiles | Both profiles remain explicit and resolvable | Migrate through new manifests; never re-label old hashes | Alias same digest under incompatible profile semantics |
| Trash claim versus migration pin | Pin or remote uncertainty cancels/blocks delete | Retry after migration finalization | Delete in-transit state |
| Crash after unlink, before delete finalization | Durable delete claim remains | Probe filesystem and finalize idempotently | Recreate a stale refcount without bytes |

The fault harness must inject process termination and filesystem persistence
loss, not only return ordinary Rust errors. It must model unsynchronized file
data and directory entries independently.

## Backup, Restore, and Schema Contract

### Generation Manifest

A backup bundle must contain a canonical, versioned manifest with at least:

```text
BackupGenerationManifest {
    format_version,
    generation_id,
    source_node_identity_class,
    created_at,
    redb_stores: [{ logical_name, schema_version, digest, integrity_status }],
    sqlite_stores: [{ logical_name, schema_version, wal_cut, digest }],
    block_roots: [{ namespace, algorithm, profile, reachability_digest }],
    event_cursor,
    projection_generation,
    required_block_count,
    required_block_bytes,
    manifest_digest
}
```

The source node identity class distinguishes portable business/application state
from node-local authority such as cluster roles, supervisor budgets, private
keys, and boot identity. A restore must never clone node-local authority merely
because it was present in a host backup.

### Backup Cut

1. Close or generation-fence new mutation admission for the selected scope.
2. Drain or journal admitted mutations.
3. Establish redb/SQLite read snapshots and the application event cursor.
4. Materialize the complete reachability set from manifests and durable pins.
5. Hold a non-expiring backup claim for that set while bytes are exported.
6. Verify every byte by its `BlockRef` and every database by its engine-specific
   integrity mechanism.
7. Write and synchronize the generation manifest last.
8. Expose the backup only after the manifest directory entry is durable.

Copying an open redb file with `fs::read` is not this contract. The
implementation must either use an engine-supported stable snapshot/savepoint or
quiesce and copy under an explicit lifecycle gate.

### Restore

1. Parse and authenticate the generation manifest before writing target state.
2. Reject unsupported schemas, hash algorithms, profiles, duplicate logical
   stores, missing blocks, digest mismatch, and prohibited node-local state.
3. Stage databases and bytes under a new generation.
4. Run redb integrity checks, SQLite integrity checks, table-level semantic
   checks, and complete reachability reconstruction.
5. Switch the active generation only after every store passes.
6. Rebuild process-local caches before opening mutation admission.
7. Keep the old generation until post-activation validation completes.

Schema migrations run against a staged generation or under a durable migration
journal. They must be restartable, retain backward decode for the declared
window, and never silently coerce an unknown JSON row into a default authority
state.

### Compatibility Matrix

| Input condition | Decision | Required action |
| --- | --- | --- |
| Same generation format, schemas, `BlockRef` version, and chunk profile | Accept after full integrity/reachability verification | Stage and activate normally |
| Known older schema inside declared backward-decode window | Accept only into migration staging | Migrate idempotently, verify, emit a new generation |
| Unknown/newer schema or authority enum | Reject | Preserve bundle unchanged; require compatible binary/migration |
| Same content under two supported chunk profiles | Coexist as distinct profile-bound identities | Resolve using manifest profile; optional explicit re-chunk migration |
| Unknown/missing chunk profile | Reject before byte publication | Never infer current default |
| BLAKE3-128 identity from an untrusted boundary | Reject as sole authenticity proof | Require transport-authenticated source plus full security-boundary digest |
| Missing, duplicate, extra-reachable, or digest-mismatched required block | Reject activation | Repair from a verified source or mark manual recovery |
| Extra verified orphan not referenced by the generation | Quarantine/retain outside active reachability | Reconcile after activation; do not fail a valid content cut solely for an orphan |
| Event cursor and projection generation disagree | Reject activation | Rebuild projection from the declared event cut or provide matching generation |
| Portable bundle contains node identity, private keys, local owner/saga state, or supervisor authority | Reject by default | Require explicit authenticated node-bound restore/rebind flow |
| Complete generation but unsupported storage-policy version | Reject | Upgrade through #656 compatibility path |
| Generation manifest missing or its canonical digest fails | Reject before target mutation | No legacy best-effort import into active stores |

## Upstream Landscape

### Reproducible Candidate Rubric

Each candidate was scored qualitatively against the same criteria:

- transactional mechanism fit;
- crash and power-loss semantics;
- snapshot, backup, repair, and compaction support;
- deterministic testability;
- Rust/FFI and runtime boundary;
- write, memory, and operational resource model;
- maintenance and on-disk compatibility;
- license and repository security material;
- integration and upgrade cost;
- fit with Sentinel's 1:n content model.

### Pinned Provenance

| Project | Reviewed revision | License material | Repository security file at revision | Role in study |
| --- | --- | --- | --- | --- |
| [redb](https://github.com/cberner/redb/tree/fc2b084dc0c8c261693b544942b1c1aa0bc75967) | `fc2b084` (3.1.1) | MIT OR Apache-2.0 | Not present | Current engine, deep review |
| [heed](https://github.com/meilisearch/heed/tree/86cd1f681953cd5f6870706f6139b851e975975e) | `86cd1f6` (0.22.1) | MIT | Not present | Safe Rust LMDB wrapper, deep review |
| [LMDB](https://github.com/LMDB/lmdb/tree/389e1009a86c37f9d48564c58f8dbfc2858c3a44) | `389e100` | OpenLDAP Public License 2.8 | Not present | mmap B-tree reference, deep review |
| [SQLite](https://github.com/sqlite/sqlite/tree/b524d66cd24e8baef29618b77de126feefa14e57) | `b524d66` | Public domain dedication; GitHub is an official mirror, Fossil is canonical | Not present | Backup and crash-test reference, deep review |
| [Fjall](https://github.com/fjall-rs/fjall/tree/6debe706dbc53d6d0eb666aae5057671d5c1370f) | `6debe70` (3.1.8) | MIT OR Apache-2.0 | Not present | Rust LSM and GC watermark reference, deep review |
| [RocksDB](https://github.com/facebook/rocksdb/tree/3b446089141659fad25328c5ea3e7ed283df46e4) | `3b44608` (11.1.2) | Apache-2.0 plus LevelDB BSD portions | Not present | Checkpoint and fault-model reference, deep review |
| [sled](https://github.com/spacejam/sled/tree/d81865d07f07910133877915b57abf0c52d5756b) | `d81865d` | MIT OR Apache-2.0 | Present | Rejected candidate |

An absent repository `SECURITY.md` is not proof of insecurity. It means the
reviewed revision does not expose that standard reporting artifact and the
project must be evaluated through normal dependency governance.

The following immutable paths are the reproducible source/test/license/security
inventory. Every link resolves inside the reviewed commit; `Absent` means a
recursive tree scan found no repository `SECURITY.md` at that revision.

| Project | Implementation evidence | Test/failure evidence | License and security evidence |
| --- | --- | --- | --- |
| redb | [`src/transactions.rs`](https://github.com/cberner/redb/blob/fc2b084dc0c8c261693b544942b1c1aa0bc75967/src/transactions.rs), [`src/db.rs`](https://github.com/cberner/redb/blob/fc2b084dc0c8c261693b544942b1c1aa0bc75967/src/db.rs) | [`tests/integration_tests.rs`](https://github.com/cberner/redb/blob/fc2b084dc0c8c261693b544942b1c1aa0bc75967/tests/integration_tests.rs), [`tests/multithreading_tests.rs`](https://github.com/cberner/redb/blob/fc2b084dc0c8c261693b544942b1c1aa0bc75967/tests/multithreading_tests.rs) | [`LICENSE-MIT`](https://github.com/cberner/redb/blob/fc2b084dc0c8c261693b544942b1c1aa0bc75967/LICENSE-MIT), [`LICENSE-APACHE`](https://github.com/cberner/redb/blob/fc2b084dc0c8c261693b544942b1c1aa0bc75967/LICENSE-APACHE); security file: Absent |
| heed/LMDB | [`heed/src/envs/env_open_options.rs`](https://github.com/meilisearch/heed/blob/86cd1f681953cd5f6870706f6139b851e975975e/heed/src/envs/env_open_options.rs), [`heed/src/envs/env.rs`](https://github.com/meilisearch/heed/blob/86cd1f681953cd5f6870706f6139b851e975975e/heed/src/envs/env.rs), [`lmdb.h`](https://github.com/LMDB/lmdb/blob/389e1009a86c37f9d48564c58f8dbfc2858c3a44/libraries/liblmdb/lmdb.h) | heed inline environment tests in [`env.rs`](https://github.com/meilisearch/heed/blob/86cd1f681953cd5f6870706f6139b851e975975e/heed/src/envs/env.rs#L755-L1075), LMDB [`mtest.c`](https://github.com/LMDB/lmdb/blob/389e1009a86c37f9d48564c58f8dbfc2858c3a44/libraries/liblmdb/mtest.c) | heed [`LICENSE`](https://github.com/meilisearch/heed/blob/86cd1f681953cd5f6870706f6139b851e975975e/LICENSE), LMDB [`LICENSE`](https://github.com/LMDB/lmdb/blob/389e1009a86c37f9d48564c58f8dbfc2858c3a44/libraries/liblmdb/LICENSE); security files: Absent |
| SQLite | [`src/backup.c`](https://github.com/sqlite/sqlite/blob/b524d66cd24e8baef29618b77de126feefa14e57/src/backup.c) | [`test/walcrash.test`](https://github.com/sqlite/sqlite/blob/b524d66cd24e8baef29618b77de126feefa14e57/test/walcrash.test), [`test/backup_ioerr.test`](https://github.com/sqlite/sqlite/blob/b524d66cd24e8baef29618b77de126feefa14e57/test/backup_ioerr.test) | [`LICENSE.md`](https://github.com/sqlite/sqlite/blob/b524d66cd24e8baef29618b77de126feefa14e57/LICENSE.md); security file: Absent |
| Fjall | [`src/journal/writer.rs`](https://github.com/fjall-rs/fjall/blob/6debe706dbc53d6d0eb666aae5057671d5c1370f/src/journal/writer.rs), [`src/snapshot_tracker.rs`](https://github.com/fjall-rs/fjall/blob/6debe706dbc53d6d0eb666aae5057671d5c1370f/src/snapshot_tracker.rs) | [`tests/batch_recovery.rs`](https://github.com/fjall-rs/fjall/blob/6debe706dbc53d6d0eb666aae5057671d5c1370f/tests/batch_recovery.rs), [`tests/recovery_journal_mac.rs`](https://github.com/fjall-rs/fjall/blob/6debe706dbc53d6d0eb666aae5057671d5c1370f/tests/recovery_journal_mac.rs) | [`LICENSE-MIT`](https://github.com/fjall-rs/fjall/blob/6debe706dbc53d6d0eb666aae5057671d5c1370f/LICENSE-MIT), [`LICENSE-APACHE`](https://github.com/fjall-rs/fjall/blob/6debe706dbc53d6d0eb666aae5057671d5c1370f/LICENSE-APACHE); security file: Absent |
| RocksDB | [`checkpoint_impl.cc`](https://github.com/facebook/rocksdb/blob/3b446089141659fad25328c5ea3e7ed283df46e4/utilities/checkpoint/checkpoint_impl.cc), [`fault_injection_fs.cc`](https://github.com/facebook/rocksdb/blob/3b446089141659fad25328c5ea3e7ed283df46e4/utilities/fault_injection_fs.cc) | [`checkpoint_test.cc`](https://github.com/facebook/rocksdb/blob/3b446089141659fad25328c5ea3e7ed283df46e4/utilities/checkpoint/checkpoint_test.cc), [`db/fault_injection_test.cc`](https://github.com/facebook/rocksdb/blob/3b446089141659fad25328c5ea3e7ed283df46e4/db/fault_injection_test.cc) | [`LICENSE.Apache`](https://github.com/facebook/rocksdb/blob/3b446089141659fad25328c5ea3e7ed283df46e4/LICENSE.Apache), [`LICENSE.leveldb`](https://github.com/facebook/rocksdb/blob/3b446089141659fad25328c5ea3e7ed283df46e4/LICENSE.leveldb); security file: Absent |
| sled | [`README.md`](https://github.com/spacejam/sled/blob/d81865d07f07910133877915b57abf0c52d5756b/README.md) | [`tests/test_crash_recovery.rs`](https://github.com/spacejam/sled/blob/d81865d07f07910133877915b57abf0c52d5756b/tests/test_crash_recovery.rs), [`tests/test_tree_failpoints.rs`](https://github.com/spacejam/sled/blob/d81865d07f07910133877915b57abf0c52d5756b/tests/test_tree_failpoints.rs) | [`LICENSE-MIT`](https://github.com/spacejam/sled/blob/d81865d07f07910133877915b57abf0c52d5756b/LICENSE-MIT), [`LICENSE-APACHE`](https://github.com/spacejam/sled/blob/d81865d07f07910133877915b57abf0c52d5756b/LICENSE-APACHE), [`SECURITY.md`](https://github.com/spacejam/sled/blob/d81865d07f07910133877915b57abf0c52d5756b/SECURITY.md) |

### Candidate Fit Matrix

| Candidate | Failure and recovery semantics | Determinism and 1:n fit | Security/runtime boundary | Maintenance and integration cost | Sentinel performance hypothesis | Decision |
| --- | --- | --- | --- | --- | --- | --- |
| redb 3.1.1 | CoW commit slots, checksums, optional 2PC/quick-repair, integrity repair, savepoints, compaction | Deterministic local metadata engine; content remains outside, so no conflict with 1:n CAS | Pure Rust; non-cryptographic internal checksums require trusted local-file boundary | Already shipped; lowest migration and upgrade cost | Policy tuning can trade commit cost for crash-open time; must measure per store | Configure and wrap |
| heed/LMDB | Mature single-writer MVCC, environment sync/copy/compact, explicit unsafe durability flags | Good local KV behavior but does not solve content identity or cross-store generation | C FFI, unsafe environment-open contract, mmap/map-size operations | New engine, data migration, C toolchain/license/update surface | Point reads may be competitive, but no source-backed reason to improve Sentinel's end-to-end path | Reject dependency; port reader/maintenance contracts |
| SQLite | WAL/rollback journals, online backup read cut, extensive crash/fault corpus | Deterministic transactional SQL and already used; 1:n content still belongs to CAS | Bundled C boundary already accepted for event/projection stores | No new dependency, but replacing redb would duplicate schema/query semantics | Strong for event/range/query workloads, not proven better for typed hot KV | Keep current split; port backup/fault contracts |
| Fjall | Journal persistence modes, poison on persist failure, snapshots and GC sequence watermark | Watermark model fits reader-safe reclamation; LSM layout is not application dedup | Pure Rust but adds background storage machinery and newer operational surface | New engine and migration; separate compaction/write-amplification model | Could favor write-heavy workloads but risks extra IO/space on constrained nodes | Reject engine; port poison/watermark contracts |
| RocksDB | WAL/manifest/checkpoints, mature fault injection, repair and compaction ecosystem | Strong testability; large LSM/background model conflicts with minimal embedded 1:n footprint | C++ FFI, many threads/options, broad native attack/update surface | Highest binary, build, tuning, and upgrade cost | May win large write-heavy datasets, but current Sentinel workload does not justify cost | Reject dependency; port checkpoint/fault model |
| sled | Log/page-cache design with transactions, but upstream labels reliability and format as immature | Interesting concurrency model; unstable format is incompatible with deterministic upgrades | Pure Rust, but maturity is the primary safety boundary | Manual migrations and uncertain maintenance path | No admissible basis for improvement over current engines | Reject |

No candidate receives an `Adopt dependency` decision. This is a positive result:
the study extracts narrowly testable mechanisms while keeping Sentinel's shipped
dependency and operational surface stable.

### Source-Level Mechanisms

#### redb

redb already exposes the mechanisms Sentinel needs:

- `Durability::Immediate` guarantees persistence when commit returns, while
  `Durability::None` explicitly defers persistence;
- persistent savepoints retain old pages and therefore require bounded lifetime;
- ephemeral savepoints support transaction-local rollback;
- optional two-phase commit separates commit-slot persistence from activation;
- quick-repair persists allocator state and enables two-phase commit to reduce
  crash-open repair time;
- `check_integrity()` verifies and repairs the database when possible;
- `compact()` refuses active transactions/savepoints and uses protected commits
  while moving and shrinking pages.

Pinned source:
[durability](https://github.com/cberner/redb/blob/fc2b084dc0c8c261693b544942b1c1aa0bc75967/src/transactions.rs#L360-L369),
[savepoints](https://github.com/cberner/redb/blob/fc2b084dc0c8c261693b544942b1c1aa0bc75967/src/transactions.rs#L941-L975),
[commit and quick-repair](https://github.com/cberner/redb/blob/fc2b084dc0c8c261693b544942b1c1aa0bc75967/src/transactions.rs#L1175-L1257),
[integrity and compaction](https://github.com/cberner/redb/blob/fc2b084dc0c8c261693b544942b1c1aa0bc75967/src/db.rs#L553-L668).

**Decision:** `Configure existing dependency` plus `Wrap`. Keep redb. Introduce
per-store policy for durability, quick-repair, integrity/readiness, savepoint
lifetime, and maintenance compaction.

#### heed and LMDB

heed makes LMDB usable from Rust but opening an environment remains unsafe
because map sizing, flags, and filesystem assumptions are caller contracts.
LMDB documents durability-relaxing flags such as `MDB_NOSYNC`,
`MDB_NOMETASYNC`, and `MDB_MAPASYNC`; it also provides environment copy/compact
operations. heed exposes force-sync, compact-copy, stale-reader cleanup, and
warns against unnecessarily long read transactions.

Pinned source:
[heed open contract](https://github.com/meilisearch/heed/blob/86cd1f681953cd5f6870706f6139b851e975975e/heed/src/envs/env_open_options.rs#L150-L255),
[heed readers and maintenance](https://github.com/meilisearch/heed/blob/86cd1f681953cd5f6870706f6139b851e975975e/heed/src/envs/env.rs#L417-L632),
[LMDB flags and copy API](https://github.com/LMDB/lmdb/blob/389e1009a86c37f9d48564c58f8dbfc2858c3a44/libraries/liblmdb/lmdb.h#L354-L804).

**Decision:** `Reject` as a dependency replacement; `Port algorithm/contract`
for bounded reader lifetimes, file-growth observability, compact-copy
operations, and explicit durability modes. The C FFI, mmap environment sizing,
additional license/update boundary, and migration cost are not justified.

#### SQLite

SQLite's online backup implementation establishes a consistent read transaction
and copies pages incrementally while coordinating schema and destination state.
Its test suite repeatedly models WAL crashes, I/O faults, and integrity checks.
Sentinel already ships SQLite and should reuse its own engine-level contracts for
event/projection stores rather than emulating them with redb.

Pinned source:
[online backup](https://github.com/sqlite/sqlite/blob/b524d66cd24e8baef29618b77de126feefa14e57/src/backup.c),
[WAL crash tests](https://github.com/sqlite/sqlite/blob/b524d66cd24e8baef29618b77de126feefa14e57/test/walcrash.test),
[fault simulation](https://github.com/sqlite/sqlite/tree/b524d66cd24e8baef29618b77de126feefa14e57/test).

**Decision:** `Keep Sentinel` engine split; `Port algorithm/contract` for a
deterministic crash harness, explicit online backup cuts, and integrity gates.
Do not replace redb with SQLite and do not replace SQLite event stores with
redb under this issue.

#### Fjall

Fjall makes persistence intent explicit through buffered, data-sync, and
full-sync modes. Its journal path poisons the keyspace after persistence failure
instead of continuing to expose uncertain state. Its snapshot tracker computes a
sequence watermark below which reclamation is safe.

Pinned source:
[journal persistence](https://github.com/fjall-rs/fjall/blob/6debe706dbc53d6d0eb666aae5057671d5c1370f/src/journal/writer.rs),
[persistence failure handling](https://github.com/fjall-rs/fjall/blob/6debe706dbc53d6d0eb666aae5057671d5c1370f/src/keyspace/mod.rs#L240-L300),
[snapshot GC watermark](https://github.com/fjall-rs/fjall/blob/6debe706dbc53d6d0eb666aae5057671d5c1370f/src/snapshot_tracker.rs#L64-L179).

**Decision:** `Reject` as an engine replacement; `Port algorithm/contract` for
poison-on-persistence-failure and generation/reader watermarks. An LSM would add
write amplification, background compaction, and a second storage operating
model without solving Sentinel's cross-store boundary.

#### RocksDB

RocksDB creates checkpoints in a private temporary directory, disables file
deletion while collecting checkpoint files, renames the completed staging
directory, and synchronizes the resulting directory. Its fault-injection
filesystem can drop unsynchronized file data, randomly truncate unsynchronized
data, and delete names created after the last directory sync.

Pinned source:
[checkpoint staging](https://github.com/facebook/rocksdb/blob/3b446089141659fad25328c5ea3e7ed283df46e4/utilities/checkpoint/checkpoint_impl.cc#L92-L195),
[unsynchronized-data fault model](https://github.com/facebook/rocksdb/blob/3b446089141659fad25328c5ea3e7ed283df46e4/utilities/fault_injection_fs.cc#L1340-L1398).

**Decision:** `Reject` as a dependency; `Port algorithm/contract` for checkpoint
staging and the compact filesystem-fault model. RocksDB's C++/FFI surface,
background threads, compaction model, binary size, and upgrade burden violate
the minimal 1:n fit for Sentinel's current workload.

#### sled

sled's own reviewed README calls the engine beta, recommends SQLite when
reliability is primary, notes excess space use, and warns that its on-disk format
requires manual migration before 1.0.

Pinned source:
[known issues](https://github.com/spacejam/sled/blob/d81865d07f07910133877915b57abf0c52d5756b/README.md#L145-L160).

**Decision:** `Reject`. It does not improve the reliability or upgrade contract.

## Mechanism Comparison

| Mechanism | Sentinel today | Best upstream lesson | Decision and integration boundary |
| --- | --- | --- | --- |
| Metadata transactions | redb transactions per logical store | redb already provides the needed ACID boundary | Keep redb; expose a Sentinel policy wrapper |
| External byte publication | Inconsistent between segment, local CAS, and pull CAS | Fjall sync modes; RocksDB staged checkpoint | Implement one durable publish primitive in `sentinel-fs` |
| MVCC reader lifetime | No application lease/age policy | heed reader guidance; Fjall sequence watermark | Add metrics, owner tags, and bounded read claims |
| Crash-open recovery | redb default recovery only | redb quick-repair and integrity APIs | Configure by store criticality and size; gate readiness |
| Savepoints | Not used as an operating primitive | redb persistent/ephemeral savepoints | Use only for scoped redb rollback; never claim cross-store atomicity |
| Compaction | No policy | redb compaction preconditions; LMDB compact copy | Maintenance API with no active readers/savepoints and before/after integrity |
| Online backup | Live file read in Gaia; sequential app snapshots | SQLite consistent read cut; RocksDB staging | Build generation manifest and scoped admission barrier |
| Reference counting | Mutable count used in GC decisions | Fjall reader watermark | Keep as derived index; rebuild and compare digest |
| GC | Grace queue but incomplete rechecks/claims | Snapshot watermark plus generation claim | Add local delete claim and compose with #499 cluster guard |
| Startup reconciliation | Temporary-file cleanup only | Journal recovery patterns | Scan every metadata-byte invariant before readiness |
| Schema evolution | Mixed explicit and implicit JSON schemas | Versioned engine/application manifests | Add per-row/envelope version and staged migration generation |
| Fault injection | Function-error injections in selected paths | SQLite crash corpus; RocksDB fault filesystem | Build deterministic file-data/dir-entry/process-crash harness |
| Content identity | Versioned `BlockRef` is sound | OCI/Git-style self-describing identity already adopted | Keep; enforce profile compatibility on every import/restore |
| Engine replacement | No demonstrated engine defect | All alternatives add a different cost model | Reject replacement; revisit only on measured, unrecoverable limitation |

## Exact Decisions

| Decision area | Decision | Rejected alternative |
| --- | --- | --- |
| redb dependency | Configure and wrap existing redb 3.1 | Replace with LMDB, Fjall, RocksDB, sled, or SQLite |
| Authoritative redb writes | `Durability::Immediate`; consider quick-repair per store after benchmark | Global `Durability::None` for authoritative state |
| Rebuildable caches | `Durability::None` only with explicit rebuild source, generation, and fail-closed startup rule | Treat eventual cache state as authority |
| CAS publication | One Sentinel durable publish API with mandatory error propagation | Per-callsite ad hoc write/rename logic |
| Refcounts | Derived, rebuildable, generation-tagged accelerator | Sole deletion authority |
| GC | Durable delete claims plus all local/cluster reachability guards | Read-check, unlink, then partial recheck |
| Savepoints | redb-local rollback and stable read cuts only | Cross-store atomicity claim |
| Backup | Versioned metadata-plus-CAS generation | Copy live files independently |
| Restore | Stage, verify, generation switch, cache rebuild, then readiness | Sequential overwrite of active stores |
| Compaction | Controlled maintenance with read/savepoint exclusion and integrity checks | Background/unbounded compaction without ownership |
| Crash testing | Minimal Sentinel fault filesystem inspired by SQLite/RocksDB | Add RocksDB as a test dependency or rely on ordinary error returns |
| Engine benchmarks | Measure only implementation candidates on declared runtime targets | Use upstream numbers or build-server timings |

## Operating Policy for redb Stores

Every redb database must declare a policy at construction rather than inherit
implicit defaults:

```text
RedbStorePolicy {
    logical_name,
    authority_class: Authoritative | DerivedRebuildable | NodeLocalAuthority,
    durability: Immediate | DeferredUntilCheckpoint,
    quick_repair: Enabled | DisabledWithReason,
    two_phase_commit: Enabled | EngineDefault,
    schema_version,
    max_read_lease,
    integrity_on_start: Always | AfterUncleanShutdown | OperatorOnly,
    compaction_trigger,
    backup_class: Portable | NodeBound | Excluded,
}
```

Rules:

- `DeferredUntilCheckpoint` is legal only for a derived store with a named
  authoritative rebuild source and a tested startup rebuild.
- Node-local authority is included in host disaster recovery but cannot be
  restored to a different node identity without an authenticated rebind.
- Integrity failure blocks the owning capability. Automatic repair is recorded
  as an auditable event and followed by semantic validation.
- Compaction never runs while the store has active read transactions,
  savepoints, backup claims, or a restore/migration activation.
- Persistent savepoints have an owner, purpose, age limit, and cleanup path.
- Every database reports file size, allocated/fragmented bytes when available,
  oldest reader/savepoint age, last clean shutdown, last integrity result, last
  repair, last compaction, and schema version.

## Existing Issue Ownership

| Concern | Existing owner | Boundary after this study |
| --- | --- | --- |
| Distributed block pull and durable remote receive | [#498](https://github.com/silentspike/project-sentinel/issues/498) | Delivered transport foundation; new local durable-publication regressions need a follow-up owner |
| Cluster uncertainty and destructive delete guard | [#499](https://github.com/silentspike/project-sentinel/issues/499) | Owns remote reference/pin uncertainty; must consume the local delete-claim contract |
| Migration staging and in-transit pins | [#501](https://github.com/silentspike/project-sentinel/issues/501) | Owns migration claims; must block local and cluster GC |
| Chunk algorithm and ingest performance | [#620](https://github.com/silentspike/project-sentinel/issues/620), [#627-#630](https://github.com/silentspike/project-sentinel/issues/627) | Own performance/profile rollout, not durability or delete correctness |
| Backup and disaster recovery research history | [#722](https://github.com/silentspike/project-sentinel/issues/722) | Closed and verified research input; it is not an active implementation owner |
| Whole-product recovery implementation | [#751](https://github.com/silentspike/project-sentinel/issues/751), especially [#753](https://github.com/silentspike/project-sentinel/issues/753) and [#755](https://github.com/silentspike/project-sentinel/issues/755) | Consume the generation manifest for recovery-point sealing and restore; do not duplicate storage publication, reachability, or redb policy ownership |
| Dependency ownership | [#705](https://github.com/silentspike/project-sentinel/issues/705) | Records the keep-and-wrap redb decision |
| Dependency upgrades | [#656](https://github.com/silentspike/project-sentinel/issues/656) | Must test redb on-disk compatibility and policy APIs on update |

This study identifies an uncovered local implementation domain: durable
publication, generation-bound reachability, conservative local deletion,
startup reconciliation, and deterministic cross-store fault testing. It is
owned by [#726](https://github.com/silentspike/project-sentinel/issues/726) and
its ordered children rather than being appended invisibly to a closed
distributed-CAS issue or a performance issue.

## Implementation Slices

### Slice 1: Durable Publication and Reconciliation

Implementation owner:
[#730](https://github.com/silentspike/project-sentinel/issues/730).

Runtime target class: `BOTH`.

Scope:

- one `DurableBlockPublisher` for segment chunks and SHA-256 blobs;
- mandatory file and directory synchronization with propagated failures;
- segment synchronization before manifest reachability;
- digest/size verification at both write and startup boundaries;
- per-operation ingest journal/claim;
- concurrent identical-ingest single-flight or deterministic loser cleanup;
- startup inventory for temp files, orphan bytes, dangling locations, truncated
  segments, digest mismatch, and incomplete sessions;
- fail-closed readiness for reachable corruption;
- local evidence first, then distributed pull/advertisement regression evidence.

Rollback: retain legacy reads, disable new writer only before any new-format
generation is committed; after new generation publication, rollback must retain
the compatibility decoder and reconciliation path.

### Slice 2: Reachability Ledger and GC Delete Claims

Implementation owner:
[#727](https://github.com/silentspike/project-sentinel/issues/727).

Runtime target class: `BOTH`.

Scope:

- reconstruct canonical reachability from manifests, object refs, snapshot refs,
  and durable active claims;
- compare and repair derived refcounts by generation;
- atomically cancel trash on new references;
- durable, CAS-style `DeleteClaim` around eligibility, unlink, and finalization;
- reader/ingest/snapshot/backup/restore/migration claims;
- compose local proof with #499 remote uncertainty and unknown-node retention;
- recover crash before unlink, after unlink, and before finalization;
- prove zero false delete under ingest, snapshot, backup, restore, and migration
  races.

Rollback: disable destructive deletion and retain trash. No rollback procedure
may reconstruct missing content from refcounts.

### Slice 3: Storage Generation, Backup, Restore, and Schema

Implementation owner:
[#728](https://github.com/silentspike/project-sentinel/issues/728).

Runtime target class: `BOTH`.

Scope:

- canonical `StorageGeneration` and backup manifest;
- classify portable state versus node-bound authority;
- consistent redb/SQLite/event/projection/CAS cut;
- staged restore into a non-empty target;
- complete reachability and digest validation before generation activation;
- crash recovery after every store layer;
- schema envelope/version registry and mixed-profile compatibility;
- integration with the #751 recovery epic, especially #753/#755, Time Machine,
  and #501 migration without duplicating their orchestration.

Rollback: retain the old active generation until post-activation validation;
forward-complete or switch back only while old generation and all referenced
bytes remain intact.

### Slice 4: redb Operations and Storage Fault Harness

Implementation owner:
[#729](https://github.com/silentspike/project-sentinel/issues/729).

Runtime target class: `BOTH` for operating behavior; deterministic unit/model
tests run without VM timing claims.

Scope:

- `RedbStorePolicy` on every construction site;
- explicit durability classification for current `Durability::None` callsites;
- quick-repair and two-phase-commit decision by store;
- integrity/readiness and semantic validation;
- read/savepoint age observability;
- controlled compaction and file-growth limits;
- safe online backup primitive for Gaia rather than live `fs::read`;
- fix Hippocampus load-modify-store updates inside one writer transaction;
- deterministic filesystem fault model for unsynced data and directory entries;
- process-crash matrix across the three preceding slices.

Rollback: policies can return to prior engine defaults only after proving no new
schema/generation dependency; integrity and reconciliation gates are not
silently disabled.

## Acceptance Mapping

| #708 AC | Evidence in this report |
| --- | --- |
| AC-1 | Sentinel engine/table/path inventory, current findings, issue ownership, TOGAF delta |
| AC-2 | Seven-candidate landscape and reproducible rubric |
| AC-3 | Pinned deep review of redb, heed/LMDB, SQLite, Fjall, and RocksDB; sled rejection evidence |
| AC-4 | Mechanism comparison including failure, dependency, maintenance, and integration boundaries |
| AC-5 | Exact decision table; no upstream number is used as Sentinel evidence |
| AC-6 | Ordered implementation epic #726 with quality-gated children #727-#730 |
| AC-7 | S-01 through S-11 classification with explicit promotion conditions |
| AC-8 | This public-safe English/ASCII report; repository validation remains required before merge |
| AC-9 | Unified redb/CAS/dedup invariants and table/blob inventory |
| AC-10 | Crash and concurrency matrix with allowed, recovery, and forbidden outcomes |
| AC-11 | Versioned backup/restore generation and schema/compatibility contract |

Negative criteria are explicit:

| Negative AC | Enforcement in this report |
| --- | --- |
| AC-N1 | No dependency is added; every engine alternative is rejected or retained at its existing boundary |
| AC-N2 | Every reviewed mechanism has pinned source/test/license/security and maintenance analysis; no source is copied |
| AC-N3 | Current tests are scoped to what they prove; issue labels and prior completion are not correctness evidence |
| AC-N4 | Runtime target is `NONE`; no VM, provider, Cargo/Rust, benchmark, or build-server timing was used |
| AC-N5 | Every accepted uncovered gap routes to the quality-ready #726 epic and #727-#730 children, with #751/#753/#755 as recovery consumers |

## TOGAF Delta

This worker PR does not edit either TOGAF copy. The following exact semantic
target delta is an ORC-owned handoff for separate, language-specific integration
after architecture review. The target vision should retain redb and the 1:n
content-addressed design while making the composition contract explicit:

1. redb is the transactional metadata authority; it is not the content-dedup
   mechanism and does not make external bytes durable.
2. The storage plane publishes verified bytes durably before metadata makes them
   reachable and advertises only after both are complete.
3. Refcounts and RAM existence indexes are derived accelerators. Deletion is
   authorized by complete generation-bound reachability plus local and cluster
   uncertainty guards.
4. Time Machine, migration, backup, and restore share a versioned storage cut
   covering redb metadata, SQLite/event cursor, projections, and the CAS set.
5. Every redb store declares durability, recovery, integrity, compaction,
   schema, backup, and node-portability policy.
6. The statement that a redb read is simply a memory-mapped pointer with no
   syscall must be reworded as a measured warm point-read property, not an API or
   zero-copy guarantee.
7. The final vision includes deterministic power-loss testing of file data and
   directory entries, not only function-level fault returns.

The current English guide's shorthand that a redb read is a pointer with no
syscall and its `metadata update` storage descriptions are target claims, not
proof of the cross-store ordering above. ORC must integrate the approved delta
into the public English guide and the separately maintained German SSOT without
copying one language file over the other. Neither target document is evidence
that the implementation slices are complete.

## Security and Supply-Chain Impact

- Keeping redb avoids a new C/C++ FFI boundary and a second on-disk engine.
- No upstream source is copied or vendored by this study.
- The small contracts borrowed from other projects are mechanisms, not code:
  staged publication, poison-on-persist-failure, reader watermarks, and a fault
  model.
- Digest verification is mandatory at every untrusted or cross-node byte
  boundary. BLAKE3-128 remains a trusted-cluster dedup identity, not an
  adversarial authenticity primitive.
- Repair and restore never promote unknown defaults into authority state.
- Backup manifests must be authenticated before restoring node-bound state.
- Any future dependency change is routed through #705 and #656 with license,
  advisory, compatibility, and reproducible-build checks.

## Benchmarks

N/A for #708. This is a runtime-target `NONE` research issue.

Implementation issues must benchmark only on their declared runtime targets:

- local ingest, recovery, integrity, compaction, backup, and restore on the
  single-node product target;
- distributed holder, migration, and destructive-GC behavior on the cluster
  test target;
- shared contracts reported as two separate result sets.

Required co-primary metrics for implementation work include correctness first,
then CPU-seconds/GB, synchronized writes/object, syscalls/object, write
amplification, recovery time, database/segment growth, and peak memory. Build
server and CI duration are never runtime evidence.

## Limitations

- This study did not run power-cut tests; it identifies source-proven boundaries
  that implementation tests must exercise.
- Upstream source review does not transfer upstream reliability claims to
  Sentinel.
- Filesystem and storage-device firmware can weaken `fsync` guarantees. Sentinel
  can enforce and test the OS contract but cannot promise more than the deployed
  hardware reports.
- Whole-product recovery, retention policy, and operator restore UX are
  implemented under #751, with recovery-point and restore work in #753/#755;
  #722 remains the closed research history.
- Cluster consensus, replication factor, and forced failover remain separate
  cluster architecture concerns. A safe local store does not create replicas.

## Final Go/No-Go

**GO:** keep redb and implement the four ordered operating-contract slices.

**NO-GO:** engine replacement, refcount-only deletion, live-file backup,
metadata-before-byte publication, silent eventual durability for authority, or
destructive GC under uncertainty.

Until the implementation slices are delivered, destructive GC and restore
claims must remain scoped to their currently proven conditions. The findings do
not require pausing unrelated Project Sentinel M0 functionality, but active M0
paths must not enable a proven unsafe delete or advertise cross-store atomicity
that the code does not provide.
