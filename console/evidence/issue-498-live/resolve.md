# #498 PR3 (4c) — 2-VM live evidence: BlockResolver resolves a remote-only blob via cas.read

**Scope:** PR3 (4c) is the **read path** — the `BlockResolver` (V9) wired over the CAS/artifact
read anchors (L1 `cas.read`, B1/B3 chunk reads), with single-flight + negative-cache + pull-pin.
AC-6 is the central #498 claim at the actual read API: a read of a blob this node does **not**
hold resolves on the miss (pull + verify + durable store + retry), and a second read is a local
hit with no extra pull. Builds on 4b's byte path (#601) and 4a's block map (#597).

## Cluster

Same two dedicated test VMs (NOT the prod sim VM 1069), now running the **#498 4c daemon**:
- node-0 `sentinel-test-node-0` = 10.0.0.241 (seed, holder), control `:8085`, block-pull `:8086`,
  control fingerprint `4026ff7a2f0ab2c6a4bd8b30b286e36ac23f9d918a29cee982ade65c056311b8` (unchanged
  across the redeploy — the cert is persisted, so the probe's pin still matches).
- node-1 `sentinel-test-node-1` = 10.0.0.242 (puller), presents its control cert (pinned by node-0).

#498 4c daemon `sha256 92024d0db08481a8b28e4e76ae80051af5810cb7de39c1a47e69715fe96d28ba`,
probe `sha256 d4f5261016cdb2b328da99415032f8eebd5ab721d39852ecec10a16d4d0ff478`, both scp'd +
sha256-verified on the VMs; old 4b binary backed up to `/tmp/sentinel-daemon-pre4c.bak` on each VM.

The 4c daemon wires the resolver into the read path **on a real deploy** (cluster mode only —
gated on the control stream + the fs layer; single-node prod is unchanged). node-0 log:

```
INFO sentinel_cluster_control::block_pull: #498 block-pull server listening local_addr=0.0.0.0:8086
INFO sentinel_daemon::orchestrator: Cluster 12: #498 CAS block-map gossip republish gestartet
INFO sentinel_daemon::orchestrator: Cluster 12: #498 4c blob resolver wired into the CAS read path
INFO sentinel_daemon::cluster_control: Cluster 12: control stream started bind_addr=0.0.0.0:8085 \
     fingerprint=4026ff7a…311b8 peers=1 pull_server=true
```

The blob exists ONLY on node-0 (placed in 4a): `sha256 14d5…81ec`, content size 52 (53 on disk =
1 prefix + 52 raw). node-1's resolve dest starts empty.

## AC-6 — cas.read resolves a remote-only blob through the wired BlockResolver (2-VM, cross-host)

`block_pull_probe resolve` on node-1 wires a real `BlockResolver` (V9) into a fresh empty
`CasStore`, then drives the **actual `CasStore::read` API**. The bridge is both the resolver's
local-existence `BlockStore` and its `RemotePull`: on a miss the puller `block_on`s the QUIC
block-pull on a non-runtime thread (this is exactly the daemon's FUSE-thread / `DaemonRemotePull`
design), verifies + durably stores, and the read retries against the now-local store.

```
# node-1 (.242), dest /tmp/probe-resolve-dest starts non-existent (guaranteed empty):
$ sudo block_pull_probe resolve 10.0.0.241:8086 4026ff7a…311b8 14d5…81ec 52 /opt/sentinel/data /tmp/probe-resolve-dest
RESOLVE ok: hash=14d5…81ec content_len=52 cached_before=false pulls=1
SECOND-READ content_len=52 pulls=1 (no extra pull)
# the resolved blob is durable on node-1:
/tmp/probe-resolve-dest/cas/14/d519…81ec   (53 bytes)
```

**Result:** `cas.read` of a blob node-1 lacked (`cached_before=false`) resolved on the miss —
pulled by hash from node-0, SHA-256-verified, durably published (fsync→rename→fsync, V28), and the
read returned the 52-byte content (`pulls=1`). The **second `cas.read` was a local hit with no
extra pull** (`pulls=1`) — local short-circuit + single-flight. This is the V9 read-path
resolution end-to-end over the #569 QUIC block-pull stack (cert-pinned, V10), driving the same
resolver code the daemon wires in (proven live by node-0's wiring log above). **AC-6 PASS.**

Negative/integrity paths (corrupt pull rejected; peer-miss → no publish) are covered by 4b's
`integrity` AC (#601) and the in-repo resolver unit tests (single-flight 8→1, negative-cache,
ttl-expiry, pull-pinned).

## State / cleanup

Both test VMs run the #498 4c daemon (under review); the resolved blob remains in node-1's
throwaway `/tmp/probe-resolve-dest` (not the daemon CAS). Rollback binary at
`/tmp/sentinel-daemon-pre4c.bak` on each VM. VM 1069 (prod sim) was never touched.
Register line: `sentinel-fs-distributed-cas-pull (#498 4c read-path resolution)`.
