# #569 Phase 3a0 — QUIC Control Stream, Live 2-Node Verification (2026-06-25)

**Cluster:** node-0 (10.0.0.241, `4026ff7a…`) + node-1 (10.0.0.242, `72d2f8b3…`), control stream on `:8085` (QUIC/UDP). Each node persists its control cert (stable fingerprint across restarts). Pins exchanged out-of-band into each node's `[daemon.cluster].control_peers`.

## AC-by-AC (Operator API `POST /operator/control/query` → cross-host QUIC RPC)

| AC | Result | Evidence |
|----|--------|----------|
| AC-5 RefQuery cross-host | PASS | node-0→node-1 `{"response":{"RefQueryResult":{"block_ref":"blk-ref-1","referenced":false}}}` |
| AC-5 PinQuery cross-host | PASS | node-0→node-1 (distinct key) `{"PinQueryResult":{"block_ref":"blk-pin-1","pinned":false}}` |
| AC-5 reverse direction | PASS | node-1→node-0 RefQuery `{"RefQueryResult":{"block_ref":"x","referenced":false}}` |
| AC-2 idempotency (live, over the wire) | PASS | re-sent `idempotency_key="ac-ref"` with kind=pin + block_ref=DIFFERENT → returned the **cached** `RefQueryResult{blk-ref-1}` (handler did not re-run) |
| AC-4 cert-pin reject (live) | PASS | mis-pinned node-1 (`0000…`) → `{"error":"server cert 72d2f8b3… does not match pin 0000…"}`; correct pin restored → RPC works again |
| AC-6 0-RTT off | PASS (code) | `tls.rs quic_server_config` sets `max_early_data_size=0`, TLS 1.3 only |
| cert persistence | PASS | fingerprints `4026ff7a`/`72d2f8b3` stable across the multiple restarts in this run |
| unknown peer reject | PASS | unknown `peer_alias` → `{"error":"unknown control peer …"}` |

## Bench (→ /work/company/BENCHMARK-REGISTER.md)
Cross-host RefQuery, 20 runs, distinct keys: **p50=7.2ms, p95=7.4ms, max=7.4ms** (incl. operator HTTP + fresh QUIC connection per RPC; connection-reuse = Track-G optimization).

## Notes
- In-process integration test (part 2) covers the wrong-cert reject in BOTH directions rigorously; this run confirms it live cross-host plus the happy path + idempotency.
- Peer-pin distribution is manual/config here (out-of-band, like the SSH host-key pin); automatic cert distribution via membership is Track-D2.
- Handlers are the `StubHandler` (referenced/pinned=false); the real owner/GC answers come from #496/#499.
