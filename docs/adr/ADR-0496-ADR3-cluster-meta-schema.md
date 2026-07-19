# ADR-0496: Cluster authority and operation persistence (ADR-3)

- **Gate:** ADR-3 (blocks #615 and #501)
- **Status:** Accepted
- **Primary issues:** #496, #615, and #501
- **Related gates:** G1, ADR-2, G4, G5, G-D0

> Even though the foundation is verified on a two-node cluster first, all schemas,
> messages, and APIs are N-node-native and keyed by `NodeId`. Two nodes are the first
> test, not the cluster model.

## Context

The current `ClusterMetaStore` has separate transactions for `CLUSTER_OWNER` and
`LOCAL_OWNER`. It cannot atomically install complete global authority, recipient-local
activation, and an installation marker. The current schema also mixes stable local
roles with handoff transition roles and lacks durable migration operation/participant
journals.

An owner-term replacement is a control-plane authority operation. It cannot be
authorized by the old `OwnerWriteGuard` whose authority it is replacing.

## Problem

How does Sentinel persist complete owner snapshots, local transition overlays, and
recoverable migration steps atomically without weakening normal data-plane fencing?

## Decision

Use a dedicated redb cluster metadata database with explicit atomic control-plane APIs.
Normal simulation-state stores continue to require `OwnerWriteGuard`; authority and
saga transitions require authenticated actor context, expected terms/sequences,
request digests, and CAS semantics.

## Owner authority schema

`OwnerTerm` is:

`OwnerTerm { scope, owner_node, epoch, coordinator_generation }`

Track A installs coordinator generation 1. Generation 0 is legacy. Epochs are
monotonic per scope and can never decrease during installation.

The global envelope is:

`OwnerTermSnapshot { schema_version, coordinator_generation, term_snapshot_revision, sorted_terms, checksum }`

The recipient-bound envelope is:

`LocalOwnerStateSnapshot { schema_version, recipient_node, coordinator_generation, term_snapshot_revision, sorted_base_states, checksum }`

`term_snapshot_revision` belongs only to these envelopes, never to `OwnerTerm`.
Every term in a global snapshot must carry the envelope's coordinator generation, and
the install marker stores both the global and recipient-local checksums.

Canonical codecs use fixed field order, big-endian integers, length prefixes, and
sorted scope/row lists. SHA-256 covers the canonical payload without the checksum
field. Golden vectors prevent serializer drift.

## Owner tables

- `CLUSTER_OWNER`: complete global terms.
- `LOCAL_OWNER`: recipient-bound stable base states.
- `LOCAL_OWNER_SAGA`: one scope-keyed active overlay for legacy reconciliation,
  handoff, or migration.
- `OWNER_TERM_SNAPSHOT_META`: installed generation/revision, both checksums, and
  install/conflict status.

`ActivationState` is `LegacyUnknown|NotRoutable|Routable`; legacy decoding defaults to
`LegacyUnknown`. `LocalOwnerBaseState` contains scope, recipient, complete term, base
role `Owner|Follower`, and activation. `LocalOwnerSagaState` contains scope, operation
kind, optional operation id, complete term, transition role, and transition sequence.

Effective local state is the overlay when present, otherwise the base state. A general
snapshot install never changes `LOCAL_OWNER_SAGA`. Only the matching active operation
may CAS-replace or complete it. Competing active operations for one scope require
manual recovery.

The coordinator derives each recipient snapshot deterministically from global terms,
the durable `MigrationOp`, and participant outcomes. A stable current owner is
`Owner/Routable`, a non-owner is `Follower/NotRoutable`, and a migration target before
`TargetRoutable` is `Owner/NotRoutable`. After `TargetRoutableAck`, the persisted
recipient-local routable state remains the truth and later snapshots derive the same
value from the advanced operation. Recipients never invent roles or activation from
volatile caches.

## Atomic full-snapshot installation

The only public bootstrap/replication API is:

`install_owner_snapshot(global, local) -> InstallOutcome`

In one redb transaction it:

1. validates schema, recipient, generation, revision, canonical checksums, and
   non-decreasing epochs;
2. fully replaces `CLUSTER_OWNER`;
3. fully replaces this recipient's `LOCAL_OWNER` base rows;
4. deletes legacy term/base rows absent from the incoming full snapshot;
5. writes `OWNER_TERM_SNAPSHOT_META`;
6. leaves `LOCAL_OWNER_SAGA` untouched.

Outcomes are deterministic:

- no marker or installed generation 0: install;
- different non-legacy generation: `GenerationMismatch`, no authority mutation;
- lower same-generation revision: `StaleSnapshot`, no mutation;
- equal revision and equal checksums: `AlreadyInstalled`, no mutation;
- equal revision and different checksum: persist `SnapshotConflict`, close readiness,
  and require manual recovery;
- higher same-generation revision: install atomically.

Individual owner/local put methods do not implement bootstrap readiness or snapshot
replication.

The seed materializes the initial scope set from `World` and every configured agent
scope in the boot roster. A dynamically created scope is first materialized with a
new term and incremented snapshot revision in one coordinator transaction, replicated
successfully, and only then spawned or published. Unknown scopes are never synthesized
as self-owned.

## Legacy reconciliation

Before the first full install, legacy `Retiring|Retired|PreparedTarget` rows become
scope-keyed `LegacyReconciliation` overlays with no operation id. Legacy owner/follower
base rows are replaced by the seed snapshot. Generation-0 terms are not retained as
authority. The existing #496 handoff moves to the same overlay contract.

## Migration operation schema

The coordinator owns:

- `MIGRATION_OPS`: durable `MigrationOp` rows retained after completion;
- `ACTIVE_MIGRATION_BY_SCOPE`: one active operation per scope;
- snapshot revision metadata used by the authority commit.

Claim creates the operation and active-scope index atomically.
`transition_migration(expected_state, expected_seq, next)` is monotonic CAS.

`commit_migration_owner_authority` performs one coordinator transaction containing:

1. the authority-commit step claim;
2. `CLUSTER_OWNER` target term E+1;
3. incremented owner snapshot revision and metadata;
4. the operation transition to `OwnerAuthorityCommitted`.

The active-scope index is removed only after terminal completion/recovery. Audit rows
remain.

## Participant journal

Each participant stores:

`MIGRATION_PARTICIPANT_STEPS(op_id, step, peer, request_digest, boot_id, attempt, status, outcome)`

Status is `Executing|Succeeded|DigestConflict`. A mutation atomically claims its step,
executes or probes the deterministic outcome, and CAS-completes the row. A crash after
effect but before completion is resolved by the probe; the effect is never blindly
replayed.

## Other cluster tables

Existing provision, node registry, pin, recovery-point, and voting metadata remain
versioned dedicated cluster tables. Their later quorum/replication contracts remain
Track D. Agent state, CAS bytes, and append-only events do not move into this database.

## Authorization boundary

- Data-plane EventStore/redb/FS state writes require complete V19
  `OwnerWriteGuard` validation.
- Owner snapshot install, handoff overlay CAS, migration claim/transition, and authority
  commit are authenticated control-plane operations with explicit expected state,
  complete term, actor, digest, and sequence checks.
- No raw public table mutation may bypass those APIs.

## Tests and evidence

- Canonical codec golden vectors and checksum mismatch tests.
- Legacy generation-0, stale, equal-idempotent, equal-conflicting, newer, generation
  mismatch, full replacement/deletion, and overlay-preservation tests.
- Atomic operation/scope claim, monotonic transition, and authority-commit crash tests.
- Participant claim/effect/probe/complete and digest-conflict tests.
- Restart tests rebuild only from a valid marker and durable rows.
- Two-node replication evidence proves identical global authority and recipient-correct
  local activation without deleting overlays.

## Consequences

- Bootstrap and replication gain one atomic authority installation point.
- Authority transitions no longer misuse the data-plane guard they replace.
- Durable operation/participant journals provide recoverability and effect idempotency.
- Track D can later replicate a defined subset without changing agent-state stores.

## Public claim boundary

After #615, Sentinel may claim atomic complete owner snapshot installation and durable
local activation state. After #501, it may claim a durable migration operation and
participant journal. It may not claim quorum authority, coordinator replacement, or
replicated RPO before Track D.
