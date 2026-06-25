# #495 Chunk 4c — Live Cross-Node Verification (2026-06-25)

**Cluster:** test-node-0 (VM 1071, 10.0.0.241, GenesisSeed) + test-node-1 (VM 1072, 10.0.0.242, Member), both i5-1235U on Proxmox .69. Binary built from `main 03d1e32` + fix `fix/provisionnode-member-bootstrap` (PR #566). VM 1069 (prod) never touched.

## AC-by-AC (Command → Result)

| AC | Result | Evidence |
|----|--------|----------|
| AC-B1 | PASS | node-1 pre-provision: `sentinel-bin: ABSENT`, `/opt/sentinel ABSENT`, daemon inactive, no allow-llm |
| AC-B2 | PASS | `POST /operator/provision {pending_target_id,...}` → `202 accepted`; type exposes no host/user field (V14) |
| AC-B3 | PASS | node-1 received binary (838MB) + daemon.toml + systemd unit + 2 token-gate drop-ins (all installed) |
| AC-B4/AC-6 | PASS | node-0 journal `membership heartbeat ingested node_id=77c1eadf(node-1) outcome=Updated`; node-1 `ingested node_id=5016f6e4(node-0)` — both 1Hz |
| AC-B5 | PASS | re-run (idem-demo) convergent; node-1 stays `active` |
| AC-B6 | PASS | run #1 RenderingConfig failed → `ProvisionNode fehlgeschlagen (Target quarantined)`; node-1 had binary but no config/active service (no alive half-node) |
| AC-B7 | PASS | node-1: gateway=inactive judge=inactive, `/etc/sentinel/allow-llm` absent, token-gate drop-ins carry `ConditionPathExists=/etc/sentinel/allow-llm` |
| AC-S1 | PASS | seed pins node-1 host key in `known_hosts`, `StrictHostKeyChecking=yes` (no TOFU). NOTE: qemu-guest-agent not running → host key read out-of-band by operator (G3 prod channel = guest agent, documented precondition) |
| AC-S2 | PASS | 2nd POST same key → `ProvisionNode: bereits abgeschlossen, no-op (AC-S2)` |
| AC-S3 | PASS | only binary+config+unit+token-gates copied; no `.env`/LLM keys; allow-llm absent |
| AC-S4 | PASS | half-deploy (run #1) → quarantine, node-1 not alive (= AC-B6) |
| AC-S5 | by-construction | worker mints fresh `NodeId::new()` per op (no collision path) |
| AC-S6 | PASS | seed (daemon-as-root) pubkey in node-1 `~ubuntu/.ssh/authorized_keys`, NOPASSWD sudo; daemon ssh as `ubuntu`. NOTE: in prod cloud-init injects the seed pubkey; here set up by operator |
| AC-GEN-1 | PASS | node-0 from fresh OS, no prod state |
| AC-GEN-2 | PASS | `role=Seed lifecycle=GenesisSeed node_id=5016f6e4 cluster_id=039e153d` |
| AC-GEN-4 | PASS | node-0 provisioned node-1 (`Knoten provisioniert ... duration_ms=31786`) |
| AC-GEN-5 | by-design | `seed=true` set once per cluster_id (not destructively re-tested) |
| AC-2 | PASS | both nodes daemon sha256 `c8b4c758…` (seed pushes its own binary) |
| AC-3 | PASS | node-0→node-1 reachable, RTT avg 0.43ms |
| AC-4 | PASS | both daemons `active`; gateway/judge inactive |
| AC-7 | PASS | token-gate keeps gateway/judge inactive without allow-llm (both nodes) |
| AC-DX | NOTE | both same-CPU-class (i5-1235U) + identical binary/toolchain (#494). A meaningful STRICT/CORE hash compare needs agents+ticks (0-agent bring-up here); migration = state-transfer not replay, so cross-class is correct regardless |
| NodeProvisioned event | PASS | events.db id=518 `node_provisioned` aggregate=cluster payload{node_id,alias,pending_target_id,target_ip=10.0.0.242,duration_ms=31786} |

## Bugs found + fixed (PR #566)
1. RenderingConfig scp'd config/unit/drop-ins directly to root-owned paths as ubuntu → Permission denied → now staged to /tmp + `sudo install`.
2. `config/agents` not created (daemon read_dir fatal) → now created.
3. `/opt/sentinel/fs` not created (unit ReadWritePaths + ProtectSystem=strict, status=226/NAMESPACE) → now created.

## Honest scope / follow-up notes
- ProvisionOp durability is in-memory → idempotency lost across a seed restart (ADR-3 redb `PROVISION_OPS` = #496 follow-up).
- Member rendered config is minimal → `platform_controlplane.llm_enabled` defaults true; harmless here (gateway token-gated, 0 tokens reachable) but the member config should disable it (minor follow-up).
- `/ram/{sentinel,agents}` tmpfs + `sentinel.target` are base-image requirements (pre-staged), like /opt itself.
- qemu-guest-agent not installed on the test VMs → snapshots disk-only (no fs-freeze); host-key out-of-band channel was operator-read.

## Net baseline (Tier-1, → BENCHMARK-REGISTER.md)
iperf3 node-0→node-1: 1KB 4.52 / 64KB 23.4 / 1MB 29.6 Gbit/s (0 retransmits); RTT 0.43ms. Bootstrap ~31s (838MB debug-binary scp dominated).
