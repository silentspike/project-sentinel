# #498 PR2 (4b) — 2-VM live evidence: pull-by-hash + durable publish + integrity

**Scope:** PR2 (4b) is the **byte path** — the QUIC block-pull server/client, the durable
publish (V28), and the daemon `resolve_block` primitive. The full `BlockResolver` over all
read paths + single-flight/negative-cache is PR3 (4c). Builds on 4a's block map (#597).

## Cluster

Two dedicated test VMs (NOT the prod sim VM 1069), stacked-4b daemon:
- node-0 `test-node-0` = 10.0.0.241 (seed), control `:8085`, **block-pull `:8086`**,
  control fingerprint `4026ff7a2f0ab2c6a4bd8b30b286e36ac23f9d918a29cee982ade65c056311b8`
- node-1 `test-node-1` = 10.0.0.242, presents its control cert (pinned by node-0).

#498 4b daemon `sha256 68ad2fcac8604f0c4b7b92e083055f6f856d326a3e0bd965bdca72a6bfc48f1b`,
probe `sha256 e36d4d8417b58cb46899535b18cbd64287edadcecc2c0ecab52e781bb003e1b0`, both scp'd +
sha256-verified on the VMs; old binary backed up to `/tmp/sentinel-daemon-pre4b.bak`.
node-0 log: `#498 block-pull server listening local_addr=0.0.0.0:8086` + `pull_server=true`.

The blob exists ONLY on node-0 (placed in 4a): `sha256 14d5…81ec`, content size 52 (53 on
disk = 1 prefix + 52 raw).

## AC-2 — pull-by-hash from a peer, verify, cache (2-VM, cross-host)

`block_pull_probe pull` on node-1 pulls the blob by hash from node-0's `:8086` over QUIC
(presenting node-1's pinned control cert, pinning node-0's fingerprint):

```
# node-1 (.242):
$ block_pull_probe pull 10.0.0.241:8086 4026ff7a…311b8 14d5…81ec 52 /opt/sentinel/data /tmp/probe-dest
PULL ok: hash=14d5…81ec wire_bytes=53 verified+durable=true cached_before=false
SECOND-READ local_hit=true (no pull needed)
# the pulled blob is durable on node-1:
/tmp/probe-dest/cas/14/d519…81ec   (53 bytes)
```

**Result:** node-1 fetched the blob it lacked, by hash, from the peer node-0 holds it on;
the content was SHA-256-verified and durably published; a second read is a local cache hit
(no pull). **AC-2 PASS** cross-host over the #569 QUIC stack (cert-pinned, V10).

## AC-3 — a tampered pulled block is rejected, never published (2-VM)

`block_pull_probe integrity` pulls the real bytes, flips one content byte, then verifies:

```
$ block_pull_probe integrity 10.0.0.241:8086 4026ff7a…311b8 14d5…81ec 52 /opt/sentinel/data
INTEGRITY ok: corrupt blob rejected, not published: pulled blob digest mismatch:
  got efabea8f…5090d9 want 14d5…81ec — rejected, not published
```

**Result:** the digest verify caught the tampered content; the blob was **rejected, never
published**. **AC-3 PASS.**

## AC-5 — durable publish + pull-latency benchmark (2-VM)

Durable publish is shown by AC-2 (`verified+durable=true`, the blob lands on disk via
fsync(file) → atomic rename → fsync(dir)). Pull-by-hash latency, 200 cross-host iters:

```
$ block_pull_probe bench 10.0.0.241:8086 4026ff7a…311b8 14d5…81ec 52 /opt/sentinel/data 200
BENCH pull-by-hash size=52 wire_bytes=53 iters=200: p50=1597 p95=1682 p99=1919 max=3047 us
```

~1.6 ms p50 per pull, **including a fresh QUIC connection + handshake on every call** (the
probe connects per pull). Steady-state with a pooled connection (as `resolve_block` reuses
the client endpoint) is lower; this is the honest per-pull cross-VM cost with setup. The
sweep over block size / connection reuse + the integrity/peer-loss bug-finder are noted for
the 4c/perf pass. Register line: `sentinel-fs-distributed-cas-pull (#498 4b)`.

## State / cleanup

Both test VMs run the #498 4b daemon (under review); the pulled blob remains in node-1's
`/tmp/probe-dest` (a throwaway dest, not the daemon CAS). Rollback binary at
`/tmp/sentinel-daemon-pre4b.bak` on each VM. VM 1069 (prod sim) was never touched.
