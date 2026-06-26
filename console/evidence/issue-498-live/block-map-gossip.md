# #498 PR1 (4a) — 2-VM live evidence: distributed block-map gossip

**Scope:** PR1 (4a) is the distributed-CAS **metadata layer** — `BlockRef`, the block
map (V8 locator), leveled anti-entropy (V25), the holder-gossip wire, and the CAS holder
source. Pull-by-hash (AC-2/AC-3), durable publish (AC-5) and the `BlockResolver` over all
read paths (AC-6) are PR2 (4b) / PR3 (4c).

**Transport note (architecture correction):** the issue specified Zenoh gossip, but Zenoh
is loopback-pinned since #525 (`sentinel-zenoh/src/lib.rs` — "cross-node = QUIC
ClusterControl/#569, not Zenoh"). 4a rides the existing **#569 QUIC control stream**
instead. AC-4 ("no bytes over Zenoh") therefore holds *stronger*: Zenoh is not used at all;
metadata travels over the QUIC control stream, blob bytes will travel over the QUIC pull
stream in 4b.

## Cluster

Two dedicated test VMs (NOT the prod sim VM 1069):
- node-0 `test-node-0` = 10.0.0.241 (seed), `control_bind 0.0.0.0:8085`, peer test-node-1
- node-1 `test-node-1` = 10.0.0.242, peer test-node-0
- pinned cert fingerprints exchanged out-of-band (V10) from the #496/#569 setup.

#498 binary `sha256 5adde2ebffeb767a0e0ed644bcff47ff38675d2a50c08257b314323ace3b4c0c`
built `cargo remote -c -- build --release -p sentinel-daemon`, scp'd to both VMs
(sha256 verified == on both), old binary backed up to `/tmp/sentinel-daemon-pre498.bak`.

## AC-1 — the block map knows the holder(s) for a BlockRef (2-VM)

A controlled blob was placed ONLY in node-0's CAS (node-1 has none):

```
content   = "ps497-498-ac1-distributed-cas-holder-gossip-evidence"
sha256    = 14d5192458faa6c090858dbe62846b604202f0c3a6b69da0476bc9d5982481ec
node-0 CAS blobs = 1 ; node-1 CAS blobs = 0
```

Both daemons restarted on the #498 binary; the gossip republish loop (15 s) runs only in
cluster mode (control_bind set). After ~2 rounds:

```
# node-0 (.241) — sender:
INFO sentinel_daemon::cluster_control: Cluster 12: control stream started bind_addr=0.0.0.0:8085 ... peers=1
INFO sentinel_daemon::orchestrator:    Cluster 12: #498 CAS block-map gossip republish gestartet
INFO sentinel_daemon::cluster_control: #498 block-map gossip delivered (peer newly applied) peer=test-node-1 applied=1
   (repeats every 15 s: 20:56:19, 20:56:34, 20:56:49, 20:57:04 ...)

# node-1 (.242) — receiver (THE block map now knows the holder):
INFO sentinel_daemon::cluster_control:       Cluster 12: control stream started ... peers=1
INFO sentinel_cluster_control::handler:      #498: merged holder gossip into the block map applied=1 block_count=1
   (repeats every 15 s: 20:56:19, 20:56:34, 20:56:49, 20:57:04 ...)
```

**Result:** node-1's block map resolves `BlockRef(blob, sha256:14d5…81ec) -> {test-node-0}`
(`block_count=1`, `applied=1`) — a node that lacks the blob now knows which peer holds it.
**AC-1 PASS** across two machines over the #569 QUIC control stream.

## AC-4 — metadata only, no bytes over Zenoh

The control RPC carries `Vec<HolderAdvertisement>` (BlockRef digest + NodeId + boot/incarnation/
generation + action) — no blob bytes. Zenoh is loopback-pinned and not used for #498 at all
(verified in code + the gossip flows entirely over the QUIC control stream above). **PASS.**

## Gossip overhead (4a layer)

Metadata only: one `HolderAdvertisement` per held block per republish round, batched into
bounded pages (V25, `page_limit = 256`), each a single QUIC control RPC on a 15 s cadence.
The conflict-free merge (V16) makes a re-broadcast a no-op on the receiver once applied
(here `applied=1` each round because a fresh per-round idempotency key forces re-evaluation;
the underlying merge is idempotent). No pull-latency / dedup / chunk-sweep benchmark here —
that is the byte path (4b). Register line: `sentinel-fs-distributed-cas (#498 4a)`.

## Cleanup / state

Both test VMs now run the #498 binary (the build under review); the controlled test blob
remains in node-0's CAS (harmless, uniquely named). Rollback binary at
`/tmp/sentinel-daemon-pre498.bak` on each VM. VM 1069 (prod sim) was never touched.
