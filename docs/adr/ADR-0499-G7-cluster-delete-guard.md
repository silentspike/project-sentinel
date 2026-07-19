# ADR-0499: Two-stage ClusterDeleteGuard enablement (G7)

- **Gate:** G7 / G-DELETE
- **Status:** Accepted
- **Primary issues:** #499 (#499a dry-run) and #547 (#499b destructive)
- **Related gates:** G2, ADR-2, ADR-3, G4, G5

> Even though the foundation is verified on a two-node cluster first, all schemas,
> messages, and APIs are N-node-native and keyed by `NodeId`. Two nodes are the first
> test, not the cluster model.

## Context

Sentinel has multiple event, snapshot, CAS, outbox, retention, and derived-view delete
paths. Cross-node visibility is incomplete, and the real migration/GC contention race
does not exist until #501. Treating a non-destructive query as proof of safe deletion
would create a false safety claim.

## Problem

How does Sentinel introduce cluster-wide reference decisions early while preventing
any destructive enablement before the real migration race and commit-time authority
checks are proven?

## Decision

G7 has two explicit acceptance stages.

### Stage A: #499a query, pin, and dry-run

Stage A may be built before or in parallel with #501. It provides:

- a complete repository-derived deletion inventory;
- `DeleteKind` classification as canonical/non-rebuildable or derived/rebuildable;
- a CI registration gate for new delete paths;
- authenticated remote reference and pin queries;
- local snapshot and #497 in-transit pin visibility;
- conservative dry-run decisions and keep-reason metrics.

Stage A cannot invoke deletion. `AllowedClusterSafe` means only that the dry-run
decision would permit a future guarded delete under the observed inputs. It is not a
destructive authorization.

Uncertainty, timeout, unknown member, incomplete reply, stale generation/term, or
conflicting authority always returns a typed keep decision.

### Stage B: #547/#499b destructive guard

Stage B begins only after #501 is merged and live-verified. Every destructive path must
route through `ClusterDeleteGuard`. At destructive commit time it revalidates:

- registered `DeleteKind` and canonical/derived class;
- current local refs, manifests, and pins;
- current in-transit pins;
- authenticated remote query completeness;
- current owner/coordinator generation and any required migration state;
- absence of uncertainty.

Only Stage B may execute deletion. Its acceptance must include the real #501
migration-versus-GC race and prove that no registered path bypasses the guard.

## Inventory contract

The inventory is generated from a fresh repository search and checked into an
auditable artifact. It includes every direct delete/prune/remove/GC path, not a fixed
historical count. Each path maps to exactly one `DeleteKind` and classification.

Canonical/non-rebuildable data requires the cluster decision. Derived/rebuildable
data may be explicitly local-only only when its reconstruction contract is documented.
An unregistered new path fails CI.

## Query contract

The block map is a locator, never liveness or consensus. The dry-run decision combines:

1. local refs, manifests, and pins;
2. local in-transit pins;
3. authenticated remote reference/pin replies;
4. membership completeness and authority generation.

The in-memory control cache is reply deduplication only. Query correlation and
authentication use the ADR-2 handler context and request digest.

## Observability

Stage A records, at minimum:

- candidate decision by `DeleteKind`;
- blocked-by-remote-reference count;
- blocked-by-uncertainty count;
- blocked-by-unknown-node count;
- blocked-by-remote-timeout count;
- oldest blocked age;
- query retries/failures and response completeness.

Dry-run evidence includes before/after object counts or hashes proving no object was
removed.

## Failure modes

- **Remote node unavailable:** conservative keep.
- **Conflicting authority/generation:** conservative keep and alert.
- **In-transit migration pin:** conservative keep.
- **New unregistered delete path:** CI failure.
- **Stage A code attempts deletion:** test/static gate failure.
- **Stage B authority changes before commit:** commit recheck rejects and keeps.

## Tests and evidence

### Stage A acceptance

- Fresh inventory and per-path classification.
- Deliberately unregistered path fails the CI gate.
- Two-node remote reference/pin and timeout/unknown-member keep evidence.
- In-transit pin visibility.
- Dry-run output plus byte/object immutability proof.
- Under CAS-pull/tick contention: zero false-positive dry-run decisions.

Stage A makes no zero-false-delete claim because it deletes nothing.

### Stage B acceptance

- Every registered destructive path passes through the guard.
- Commit-time authority/reference/pin rechecks.
- Real #501 migration in parallel with destructive GC.
- No destructive action under uncertainty or stale authority.
- Only after these tests may evidence state a false-delete result.

## Benchmarks

Stage A reports cross-node query and dry-run scan p50/p95/p99/max plus retries/failures
and system sidecars. Stage B separately measures guarded delete latency and the real
migration contention scenario. Benchmarks run on test VMs, never the Rust build server.

## Consequences

- Useful query/pin infrastructure does not wait for the migration implementation.
- Destructive authority cannot accidentally arrive with the dry-run phase.
- Public evidence distinguishes decision correctness from deletion correctness.

## Public claim boundary

After #499a, Sentinel may claim conservative cross-node reference/pin queries and
non-destructive dry-run decisions. It may not claim cluster-safe deletion or zero false
deletes. Those claims remain blocked until #547/#499b completes after #501.
