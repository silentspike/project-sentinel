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

## Single-node regression smoke (after the init-order fix): PASS

Both nodes' `cluster_meta.redb` wiped → fresh **single-node mode** on the 2c binary; the
fix did not scratch the single-node fast-path guarantee:
```
node-0: {"epoch":1,"is_owner":true,"local_retired":false,"own_write_validates":true,"owner_node":"5016f6e4…","this_node":"5016f6e4…"}
node-1: {"epoch":1,"is_owner":true,"local_retired":false,"own_write_validates":true,"owner_node":"6435ca03…","this_node":"6435ca03…"}
# journal: 0 StaleEpoch / panic, no "Cluster-Mode re-etabliert"; tick advancing (88959 / 88060)
```

## AC-4 — V2 partition (no steal): PASS

Setup: AGENT-08 handed node-0 → node-1 (node-0 retired @1, node-1 owns @2); AGENT-09 left
non-retired on node-0. Then the .241↔.242 link is cut (`iptables -I … 10.0.0.242 DROP`,
both directions; RefQuery → `HTTP 000`).
```
# handoff AGENT-08 back (source = test-node-1, now unreachable), during the partition:
$ curl … /operator/handoff -d '{"scope":"nano:AGENT-08","source_alias":"test-node-1",…}'    # 10:52:09 → 10:52:40
{"outcome":"AbortedSourceUnreachable"} [HTTP 200]
node-0 journal: Handoff aborted: source unreachable/refused — no ownership steal (V2) scope=nano:AGENT-08 source=test-node-1 error=timed out

# during the partition (ground-truth, both nodes):
node-0 AGENT-08: is_owner=false, local_retired=true, own_write_validates=false, owner_node=6435ca03…   # NOT stolen, retired scope stays fenced
node-0 AGENT-09: is_owner=true,  local_retired=false, own_write_validates=true                          # non-retired scope still writable
node-1 AGENT-08: epoch=2, is_owner=true, own_write_validates=true                                       # isolated, keeps its ownership

# after healing (iptables -D …):
RefQuery node-0 → node-1: RefQueryResult        # control stream restored
node-0 AGENT-08 own_write_validates=false ; node-1 AGENT-08 epoch=2 is_owner=true own_write_validates=true   # exactly ONE owner @ E+1
```
The abort is deterministic (the RPC `timed out` → logged `AbortedSourceUnreachable` with a
timestamp), distinguishable from "didn't happen to race": no side stole, the retired scope
stayed fenced while the non-retired one was writable, and after healing exactly one writer
remained (node-1 @ 2). No split-brain.

## AC-5 — TOCTOU cross-node (V19 commit-recheck): PASS

A guard minted at one epoch is caught stale after a cross-node handoff advanced the
committed epoch (the optional `guard_epoch` re-validates a previously-issued guard):
```
# (1) at issue — guard @ epoch 1 is valid:
node-0 AGENT-10: {"epoch":1,"guard_epoch":1,"is_owner":true,"own_write_validates":true}
# (2) cross-node handoff AGENT-10 node-0 → node-1:
{"outcome":"Committed { new_epoch: 2 }"}
# (3) re-check the earlier guard (guard_epoch=1) — now rejected at commit:
node-0 AGENT-10: {"epoch":2,"guard_epoch":1,"own_write_validates":false,"reject_reason":"stale owner term for NanoContainer(\"AGENT-10\"): guard epoch 1 < current committed epoch 2",…}
```
The guard that was valid when issued (epoch 1) is rejected after the cross-node handoff
committed epoch 2 — the V19 begin+commit recheck (the in-process `commit_rechecks_owner_term_and_rejects_stale`
test proves the same validate runs at `FencedTxn::commit`).

## AC-6 — Chef-SPOF: PASS

```
# before: node-1 owns AGENT-08 @ 2 (existing owner)
# kill the seed (chef):
$ systemctl kill -s SIGKILL sentinel-daemon    # on node-0
# while the seed is down:
node-1 AGENT-08 own_write_validates=true                                   # existing owner keeps writing
node-1 /operator/handoff → {"error":"handoff is seed-only …"} [HTTP 503]   # no new ownership without the chef
# after the chef restarts:
node-0 journal: Owner-Term aus Meta-Store re-etabliert ; lokale Retirements aus Meta-Store re-etabliert (V4) count=1
node-0 AGENT-08: is_owner=false, local_retired=true, own_write_validates=false, owner_node=6435ca03…   # exactly one owner (node-1 @ 2), recovered
```
Chef death = no new ownership, existing owners keep writing; the chef restart recovers
exactly one owner per scope from the durable `CLUSTER_OWNER` + `LOCAL_OWNER` tables (no
double-owner from the crash).

## Status: 7/7 ACs verified with ground-truth

AC-1 (1 owner), AC-2 (stale-reject), AC-3 (V1 on-wire), AC-4 (V2 partition), AC-5 (TOCTOU),
AC-6 (Chef-SPOF), AC-7 (restart-durability) — all PASS, plus the seed-only gate and the
single-node regression smoke. Two real bugs were found and fixed by this live verification:
the init-order `OnceLock` race (seed identity nil) and the missing seed-only handoff gate.
VM 1069 (prod) untouched throughout.

## Handoff latency benchmark (#496 plan requirement) — added after a self-audit

The plan requires `Handoff p50/p95/p99/max` + a `0 double-writes` bug-finder for #496;
this measurement was missing when the functional ACs were verified, so it was run after a
self-audit on the same live 2-node cluster (on node-0's loopback, never `cargo remote`).

**Latency — N=50 handoffs, ping-pong `nano:BENCH-1` node-0 ↔ node-1 (each iteration drives
exactly one cross-node #569 RPC):**
```
handoff latency: N=50, ok=50, failures=0
min=9.3  p50=11.5  p95=14.2  p99=25.5  max=25.5  mean=11.9  ms
```
All in the ms class (p99 = 25.5 ms ≪ the 1 Hz tick budget); the tail is dominated by the
QUIC connect-per-RPC + the durable redb owner-term write (consistent with #569 RefQuery
p50 ≈ 7.2 ms). Connection-reuse would cut it further (Track G); 0-RTT stays off (V18).

**0-double-writes bug-finder — 12 handoffs, owner-check both nodes after each:**
```
i=1  Committed -> node-0:false node-1:true  (writers=1) OK
i=2  Committed -> node-0:true  node-1:false (writers=1) OK
… (i=3..12, alternating) …
Result: 0 double-owner violations / 12 handoffs
```
Across rapid alternating cross-node handoffs there is never a moment with two writers —
exactly one node has `own_write_validates=true` at all times (the other fenced by the V19
term + the V4 retirement). The partition steal variant is covered by AC-4 above.

**Sidecar (`vmstat 1` during the run, both VMs; `ss -s` before/after):** node-0
(chef + sim) us 7–13% / sy 11–23% / id 53–77% — not saturated; node-1 (target, commit RPC
only) ~93–98% idle; `ss -s` 161/159 sockets unchanged before==after → no socket leak.

**Heartbeat / lease-TTL sweep (plan line): n/a** — the cooperative handoff is RPC-driven
(`PrepareHandoff → durable SourceRetiredAck → OwnerCommit`); there is no lease/heartbeat
timeout in the handoff path (the #495 membership heartbeats are liveness-only, V2/V38, and
do not drive the handoff). Lease/forced-failover is Track D (G-D0).
