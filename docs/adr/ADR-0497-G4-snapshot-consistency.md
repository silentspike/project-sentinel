# ADR-0497: Per-container snapshot consistency (G4)

- **Gate:** G4 (blocks #497; the uid/gid+path part V24 also blocks #500a)
- **Status:** Proposed
- **Primary issue:** #497 (per-container snapshot/restore)
- **Related issues / gates:** G0 (sub-worlds), G-ROUTE (AgentLocator), V11/V12/V23/V24/V27/V30
- **Supersedes / Superseded by:** —

> **N-node-native rule:** all schemas/messages/APIs MUST be `NodeId`-keyed, never a hard
> source/target pair. Two nodes are the first test, not the ceiling.

## Context

A consistent per-container snapshot is more than a frozen ECS marker. The codebase has
**no `Frozen` marker** today; the agent-mutating ECS systems run unconditionally —
verified: `bio_system` (`crates/sentinel-ecs/src/systems.rs:822`), `transit_system:1084`,
`work_context_system:1177`, `chaos_system:1425`, `mood_system:1553`,
`perception_system:1599`, `encounter_system:2125`, `persist_system:2269`,
`task_progress_system:786`, `autonomy_system` (`autonomy.rs:33`), `decision_system`
(`decision.rs:31`). Snapshots write sequentially across stores with no cross-store
atomicity; the Limbo save is the SSOT and the FS pin is a reconcilable post-hoc
best-effort.

## Problem

What makes a per-container snapshot torn-free and restorable without corrupting the
other agents or the migrated agent's references?

## Decision

**A `Frozen` marker over a fully-classified mutating-system set, a `SnapshotCut` across
stores, reference integrity on restore, and explicit exclusions for inbound /
side-effects / scheduled work.**

- **`Frozen` marker (V11):** a new `Frozen` component (`sentinel-common/components.rs`);
  the agent-mutating systems get `Without<Frozen>` (query filter / run condition).
  Sequence: `freeze → filtered-snapshot → filtered-restore → unfreeze`. The global tick
  keeps running (AC-3).
- **AC-0 mandatory mutating-system matrix:** **every** system above is classified —
  `Without<Frozen>`-guarded / route-queued / "never mutates per-agent" / "excluded from
  the migration class". A forgotten mutating system → a frozen-but-indirectly-changed
  agent = torn snapshot. A per-system-group test ("frozen agent bit-identical over N
  ticks") + a negative test that catches an unguarded system.
- **Reference integrity (V12), via AgentLocator (G-ROUTE):** restore is despawn+respawn
  of only the one agent (`despawn_agent_from_world` + `spawn_agent`, not global). ACs:
  `agent_id`/`container_id` identical; global registries point at the **new** entity;
  other agents can still reference the migrated one (resolved via `RouteRegistry` by
  `node_id`+`owner_term`, never a stale local `EntityId`); no despawned entity lingers
  in relationship/perception/scheduler structures.
- **Cross-store boundary, not 2PC (V27):** each multi-store op carries a class — **K1**
  single-store (guard suffices) / **K2** idempotent saga with `op_id` (restart-reconcile,
  like the existing post-hoc pin) / **K3** rejected/queued in the migration window.
  Migration additionally uses a `SnapshotCut { owner_term, event_cursor, redb_gen,
  fs_dump, cas_pin_set, inbound_cursor }` — all parts exist in `WorldSnapshot`.
- **uid/gid + path safety (V24):** never restore raw host uid/gid → sandbox identity
  mapping. Restore ACs validate symlink targets before creation, no absolute path, no
  `..`, no symlink escape, no device file unless explicitly allowed.
- **Inbound policy = EXCLUDE (V11, G0):** a container with active inbound cross-agent
  traffic is `NotMigratable` (typed reason) in Track A — **no** queue/no-drop/no-dup
  claim. The durable inbound queue is Track E/H.
- **Scheduled work + side-effects excluded (V23/V30):** active timers/tasks/delayed
  effects → the container is rejected/waited; active external side-effects (LLM-bridge,
  `PlatformSideEffect`) are excluded from the Track-A bounded class.
- **Sub-world (G0/G-6):** "per-node sub-world" is **not** a new container — it is the
  node's spawned agents in the existing global World. Migration despawns the subject at
  the source (this filtered-restore path) and the target spawns it.

## Non-Goals

- The durable inbound queue, full side-effect outbox, per-agent time-travel after move
  (G-EVENTHIST → `NotSupportedForMigratedContainer`) — all Track E/H.
- Cross-store 2PC (TOGAF: no external coordinator).

## Data Types

`Frozen` (new component), `NanoContainerSnapshot { ecs, redb_rows, fs_meta_subtree,
blob_hashes }`, `SnapshotCut { … }`. `StateTransferScope::NanoContainer` (`types.rs:666`)
implemented. `dump_agent_tables(agent_id)` (redb u16-keyed + FS `(agent_id,…)`-keyed
filtered).

## State Machine / Protocol

`freeze(agent) → snapshot_agent(ecs+redb+fs subtree under cut) → transfer → restore
(despawn+respawn only the agent) → unfreeze`.

## Failure Modes

- **Forgotten mutating system:** caught by the AC-0 matrix + negative test.
- **Torn cross-store write:** K2 saga `op_id` reconcile on restart.
- **Stale reference after move:** AgentLocator resolves, never a stale `EntityId`.

## Tests

AC-0 matrix; AC-1 complete+consistent snapshot, no foreign state (runtime-state only);
AC-2 restore without despawning others; AC-3 per-container fence (global tick runs,
frozen agent stable) + inbound=EXCLUDE; AC-3b reference integrity (V12); AC-5 (2-VM)
container-scoped state-hash B==A for a **resting** container (not whole-world, G0).

## Benchmarks

Snapshot/restore latency p50/p95/**p99/max** + bytes/agent (KB thesis, 1:n);
bug-finder completeness/consistency; sweep agent-memory small/med/large. Register:
`sentinel-daemon-per-container-transfer (#497)`.

## Backward Compatibility

`Frozen` + new types via `#[serde(default)]`; existing snapshots/restore unaffected
(single-node never sets `Frozen`). No `events` migration.

## Security

Restore uid/gid via sandbox mapping (no raw host ids); path-safety ACs prevent
symlink/`..`/device escape.

## Public Claim Boundary

- May claim after #497: consistent per-container snapshot/restore for the resting
  bounded class.
- **May NOT claim:** migration of actively-interacting containers, per-agent
  time-travel after move, or exactly-once inbound — all deferred.

## Open Follow-ups

- Durable inbound queue + side-effect outbox + per-agent EventHistory continuity (Track
  E/H).
