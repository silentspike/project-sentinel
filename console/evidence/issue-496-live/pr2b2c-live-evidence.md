# #496 PR2b-2c — Live 2-VM Handoff Evidence

Cooperative cross-node ownership handoff, verified on the live 2-node test cluster.
Ground-truth via the `/operator/owner-check` diagnostic (a real `OwnerRegistry::validate`
probe per node, not a cached value) + cross-node `/operator/handoff` + journal timestamps.

**Cluster:** node-0 = `10.0.0.241` / VM 1071 (seed, alias `test-node-0`, node_id
`5016f6e4-…`, control fp `4026ff7a…`); node-1 = `10.0.0.242` / VM 1072 (member, alias
`bare-node-1`/peer-alias `test-node-1`, node_id `6435ca03-…`, control fp `72d2f8b3…`).
Binary md5 `5083db99…` on both. Control stream QUIC/UDP :8085, peers pinned both ways.
**VM 1069 (prod) untouched.** Build only via `cargo remote -c --`; pre-snapshots
`pre_pr2b2c_*` on both VMs.

## Self-checks (Control-requested, before the ACs)

**Check #1 — `block_in_place(block_on(cc.rpc))` does not deadlock under the axum worker.**
The handoff peer step uses the identical pattern as the existing `/operator/control/query`;
RefQuery returns cleanly cross-host both directions:
```
$ curl -sX POST :8084/operator/control/query -d '{"peer_alias":"test-node-1","kind":"ref","block_ref":"cas-blob:v1:sha256:ab"}'   # on node-0
{"peer":"test-node-1","response":{"RefQueryResult":{"block_ref":"cas-blob:v1:sha256:ab","referenced":false}}}
$ # node-1 -> test-node-0: {"peer":"test-node-0","response":{"RefQueryResult":{...,"referenced":false}}}
```

**Check #3 — the owner-check probe runs a real `validate()`, not a proxy.** It returns the
actual `StaleEpochError` string from `validate` (see AC-2). Confirmed ground-truth.

## Bug found + fixed by the live verification (init-order OnceLock race)

Before the fix, node-0 (the seed) reported `this_node = 00000000-…-0` (the nil default) —
an early fenced write locked the process-global `OwnerRegistry` to its nil single-node
default *before* `init_single_node(node_id)` ran (it then became a silent no-op). This
breaks handing ownership **back** to the seed (it would not recognise a scope it owns).
Fix: `init_single_node` now runs at the very top of `run()`, before any store opens.
```
# after fix, node-0:
$ curl -sX POST :8084/operator/owner-check -d '{"scope":"nano:AGENT-07"}'
{"...,"is_owner":true,"owner_node":"5016f6e4-3e5c-483b-ae5f-24feeaf39b02","this_node":"5016f6e4-3e5c-483b-ae5f-24feeaf39b02"}
```

## AC — seed-only gate (Chef-SPOF precondition): PASS

A member must reject `/operator/handoff` even though it has a control stream + meta store.
```
$ curl -sX POST :8084/operator/handoff -d '{...}'    # on node-1 (non-seed)
{"error":"handoff is seed-only (this node is not the chef)"} [HTTP 503]
```

## Baseline (pre-handoff), scope `nano:AGENT-07`

```
node-0: {"epoch":1,"is_owner":true,"local_retired":false,"own_write_validates":true,"owner_node":"5016f6e4…","this_node":"5016f6e4…"}
node-1: {"epoch":1,"is_owner":true,"local_retired":false,"own_write_validates":true,"owner_node":"6435ca03…","this_node":"6435ca03…"}
```
(Each node is its own single-node seed until a cross-node term commits — the committed
1-owner invariant becomes meaningful after the handoff.)

## Handoff A — node-0 → node-1 (local source) — AC-1 + AC-2

```
$ curl -sX POST :8084/operator/handoff -d '{"scope":"nano:AGENT-07","source_alias":"test-node-0","target_alias":"test-node-1","target_node_id":"6435ca03-…","idempotency_key":"hA-1"}'   # on node-0
{"outcome":"Committed { new_epoch: 2 }"} [HTTP 200]

# post-A owner-check:
node-0: {"epoch":2,"is_owner":false,"local_retired":true,"own_write_validates":false,"owner_node":"6435ca03…","reject_reason":"stale owner term for NanoContainer(\"AGENT-07\"): guard epoch 2 < current committed epoch 2",...}
node-1: {"epoch":2,"is_owner":true,"local_retired":false,"own_write_validates":true,"owner_node":"6435ca03…",...}
```

**AC-1 (1 owner / container): PASS** — exactly one node can write the scope after the
handoff: node-1 `own_write_validates=true`, node-0 `false`. Ownership moved cross-node to
node-1 @ epoch 2.

**AC-2 (stale-reject, cross-node): PASS** — node-0 (the old source) is rejected with a real
`StaleEpochError` (V19 owner mismatch + V4 local retirement). Its writes to the handed-off
scope no longer validate.

## AC-7 — restart-durability: PASS

node-0 restarted; it re-establishes both the committed term **and** the local retirement
from the durable `CLUSTER_OWNER` / `LOCAL_OWNER` tables — no state loss.
```
# node-0 journal after restart:
Cluster 12: Owner-Term aus Meta-Store re-etabliert
Cluster 12: lokale Retirements aus Meta-Store re-etabliert (V4) count=1
# owner-check node-0 after restart:
{"epoch":2,"is_owner":false,"local_retired":true,"own_write_validates":false,"owner_node":"6435ca03…","reject_reason":"stale owner term … guard epoch 2 < current committed epoch 2",...}
```

## Handoff B — node-1 → node-0 (peer source) — AC-3 (V1 on the wire): PASS

The chef (node-0) drives; the source is now a **peer** (node-1), so `PrepareHandoff` is a
real cross-node RPC: node-1 durably retires + acks **before** the `OwnerCommit`.
```
$ curl -sX POST :8084/operator/handoff -d '{"scope":"nano:AGENT-07","source_alias":"test-node-1","target_alias":"test-node-0","target_node_id":"5016f6e4-…","idempotency_key":"hB-1"}'   # on node-0
{"outcome":"Committed { new_epoch: 3 }"} [HTTP 200]

# V1 ordering, timestamps from both VMs:
node-1  2026-06-26T10:41:16.002751Z  owner_handler: PrepareHandoff: scope durably retired (source side, V4) scope="nano:AGENT-07" epoch=2
node-0  2026-06-26T10:41:16.015403Z  handoff: Handoff committed: ownership moved to target (V1) scope=nano:AGENT-07 source=test-node-1 target=test-node-0 new_epoch=3
```
The durable retire on node-1 (`…002751Z`) precedes node-0's commit (`…015403Z`) by ~12.6 ms
— the target never serves before the durable SourceRetiredAck (V1).

Post-B (ownership back at node-0 @ epoch 3):
```
node-0: {"epoch":3,"is_owner":true,"local_retired":true,"own_write_validates":true,"owner_node":"5016f6e4…",...}
node-1: {"epoch":2,"is_owner":true,"local_retired":true,"own_write_validates":false,"owner_node":"6435ca03…","reject_reason":"stale owner term … guard epoch 2 < current committed epoch 3",...}
```
Note (V4 demonstrated): node-1's *committed-term view* is stale (`is_owner=true` @2 — the
`OwnerCommit` went to the target, not the old owner), **but** `own_write_validates=false`
because the V4 local retirement still fences it. The real 1-owner guarantee is
`own_write_validates` (only node-0), not the committed-term view — exactly the
partition-safe property: a retired source cannot write even if it never learns the new owner.

## Remaining ACs (careful ground-truth phase — Control: "mit voller Sorgfalt")

- **AC-4 (V2 partition):** cut the .241↔.242 link, trigger a handoff needing the peer → abort;
  during the partition neither side steals, a retired scope stays fenced, a non-retired one
  is writable; after healing exactly one owner. (network manipulation + ground-truth timing)
- **AC-5 (TOCTOU cross-node):** the V19 commit-recheck catches a guard that went stale between
  begin and commit across the node boundary.
- **AC-6 (Chef-SPOF):** kill the seed mid-saga → no new ownership, existing owners keep
  writing; chef restart recovers exactly one owner from durable `LOCAL_OWNER`/`CLUSTER_OWNER`.

Status: 5/7 ACs verified with ground-truth (+ seed-only gate, + the init-order bug fix);
the remaining 3 split-brain/partition ACs are the deliberate final phase.
