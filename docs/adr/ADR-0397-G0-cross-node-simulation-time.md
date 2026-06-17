# ADR-0397: Cross-node simulation time & interaction consistency (G0)

- **Gate:** G0 (top-level gate #0 — blocks the whole Cluster-12 GA roadmap)
- **Status:** Proposed
- **Primary issue:** #397 (Nano-Container Platform v23 / Cluster 12 epic)
- **Related issues / gates:** #494 (determinism scope), #497 (per-container snapshot), #501 (cross-node move), Track E/H
- **Supersedes / Superseded by:** —

> **N-node-native rule:** Even though the foundation is verified on a 2-node cluster
> first, all schemas, messages and APIs MUST be N-node-native (`NodeId`-keyed
> sets/maps, never a hard source/target pair as the cluster model). Two nodes are the
> first test, not the ceiling.

## Context

The simulation runs as **one global whole-world tick**: a single loop drives every
agent each tick (`services/sentinel-daemon/src/orchestrator.rs:4247`, `tick_start`
captured immediately after). Inter-agent interaction is strictly in-process within
one `bevy_ecs` World — a room chat buffer plus an in-process action receiver, never a
network boundary. The codebase declares cross-node transfer out of scope itself:
`crates/sentinel-common/src/types.rs:759` — *"LOKAL (gleicher Host); cross-node ist
out-of-scope (Multi-Node-gated)."*

The central correctness metric of the whole Cluster-12 program is *"the migrated
container's state hash on node B equals its state hash on node A."* That equality is
only well-defined for a **resting, non-interacting** agent. The moment agent A (on
node 1) interacts with agent B (on node 0) "in the same tick", we are forced to
choose how simulation time relates across nodes.

TOGAF is **silent** on cross-node simulation time, but its Cluster-12 direction is
explicitly *"Beyond Kubernetes"* with a control center placing agents by interaction
graph and node load — i.e. it assumes agents can live on different nodes and still
interact. So this ADR fills a TOGAF gap and is **TOGAF-amendment-pending** (main
session writes it into the SSOT after the mechanism is built and verified).

## Problem

How does simulation time advance across nodes, and what determinism does the program
guarantee cross-node? Two options were genuinely on the table:

- **(a) Global barrier tick** — every node synchronizes once per tick so the whole
  cluster shares one logical clock. Gives global same-tick determinism, but
  re-introduces exactly the global serialization point that *"Beyond Kubernetes"*,
  N-node scaling and low latency are meant to remove. The barrier's wall-clock cost
  is the slowest node every tick.
- **(b) Per-node sub-worlds + accepted relaxed cross-node determinism** — each node
  ticks only the agents it owns; cross-node interaction is asynchronous with bounded
  delay. Keeps scaling and latency, but the global "same agent, same tick everywhere"
  determinism is deliberately relaxed.

This is a classic parallel-discrete-event-simulation (PDES) fork: conservative
(Chandy–Misra) vs. optimistic (Time-Warp) vs. relaxed.

## Decision

**Option (b): per-node sub-worlds + asynchronous causal messaging + per-node
determinism. No global barrier tick.**

Concretely:

1. **Each node ticks its own sub-world** (only the agents it owns). There is **no
   barrier tick** across nodes. A "sub-world" is **not** a new container or datatype
   in Track A — it is simply the set of agents a node has spawned inside the existing
   global World (`orchestrator.rs:4247`). Migration *despawns* the subject on the
   source node and *spawns* it on the target node (this is exactly the per-container
   filtered restore path of #497); no separate sub-world struct is introduced in
   Track A.
2. **Determinism scope = per-node / per-owned-scope + a causal event cut**, *not*
   global same-tick. #494 (the determinism profile) and the DEV-010 deviation are
   scoped accordingly: identical STRICT/CORE state hashes are guaranteed for a given
   owned scope on same-CPU-class hardware, not across the whole cluster in lock-step.
3. **Cross-node interaction is asynchronous with bounded delay.** A message from A on
   node 1 to B on node 0 is delivered in B's **next local tick**. This makes a
   **durable inbound queue a precondition, not an optional extra** — see the bounded
   class below and Track E/H.
4. **Shared state is node-local, bound to its owner.** A room is node-local; agents of
   a room are co-located by soft affinity (TOGAF control-center cut-cost direction).
   The `RELATIONSHIPS` table is `TableDefinition<u32, &[u8]>` keyed by `agent_id`
   (`crates/sentinel-redb/src/lib.rs:16`) → one row per agent holding that agent's
   `Relationships { affinity: Vec<(AgentId, f32)> }`
   (`crates/sentinel-common/src/components.rs:165`). A's row (A's affinity list)
   travels **with A** as part of A's snapshot. B's view of A lives in B's **own** row
   on B's node and is resolved through the route registry (see ADR-2 /
   `AgentLocator`), never through a stale local `EntityId`. `ROOM_STATE`
   (`TableDefinition<u16, &[u8]>`, room-keyed, `redb/lib.rs:18`) is **not** part of an
   agent snapshot: the room stays with the room owner; a migrated agent gets a
   node-local-resolved room reference.

## Non-Goals

- The durable inbound queue itself (message-id + dedup) is **not** built here — it is
  the Track-E/H build. G0 only *decides* that async causal messaging is the model and
  that the queue is therefore required before active cross-node interaction is
  claimed.
- Optimistic/Time-Warp rollback of speculatively-executed cross-node events is **out
  of scope** (we chose relaxed/conservative, not optimistic).
- The cross-node messaging layer (per-node sub-worlds + bounded-delay delivery) is a
  large later build (Track E/H), tracked as a top program risk; G0 only fixes the
  model.

## Data Types

No new persisted type in Track A. The decision binds the **migration eligibility
class**: a container is migratable in Track A only if it is *resting and not actively
interacting cross-node*. The eligibility predicate is enforced in #497/#501 and uses
the existing `StateTransferScope { World, NanoContainer(String) }`
(`crates/sentinel-common/src/types.rs:666`).

## State Machine / Protocol

Local tick per node is unchanged (`orchestrator.rs:4247`). Cross-node delivery is:
`A emits @ node1 tick T1` → enqueued → `delivered to B @ node0 tick T_next` (B's next
local tick after receipt). Ordering guarantee: causal (a message is never delivered
into B's *past*); no total order across nodes.

## Failure Modes

- **Clock skew between nodes** (node-0 @ tick 5000, node-1 @ tick 5003): a message
  must not "land in B's past". Async bounded-delay delivery into B's *next* tick is
  the protection; the durable queue (Track E/H) carries the message across the skew.
- **Active cross-node interaction during a Track-A move:** excluded by the migration
  eligibility class (resting-only). A container with active inbound cross-agent
  traffic is `NotMigratable` (typed reason), not silently moved.

## Tests

- #494: two identical bio+ECS tick sequences → identical STRICT/CORE hash, as a
  **per-node** correctness gate (catches intra-run nondeterminism; cross-platform f32
  is a documented homogeneous-only boundary, not test-catchable on one machine).
- #497/#501: container-scoped (not whole-world) state-hash equality for a **resting**
  container A→B; a negative test that an actively-interacting container is rejected as
  `NotMigratable`.

## Benchmarks

`n/a` for G0 itself (it is an architecture decision, not a measurable path). The
downstream moves (#501) measure pause time per runtime type; #494 is a correctness
gate (`n/a`).

## Backward Compatibility

No change to existing single-node snapshots/events/configs. Single-node operation is
the degenerate one-node case of per-node sub-worlds and behaves identically.

## Security

Out of scope for G0 (covered by ADR-2 transport auth and G-D0/G2 trust model).
Single trust domain per cluster (see G2/Track-A security model).

## Public Claim Boundary

- May claim today: per-node sub-world model decided; single-node behavior unchanged.
- **May NOT claim:** global same-tick determinism, exactly-once cross-node delivery,
  or migration of actively-interacting containers — those depend on the Track-E/H
  inbound-queue build and are not yet built.

## Open Follow-ups

- Durable inbound queue + message-id + dedup (Track E/H) — the cross-node messaging
  layer is the program's largest deferred build, larger than the distributed CAS.
- TOGAF SSOT amendment recording the per-node-sub-world decision (main session, after
  the mechanism is built and live-verified).
