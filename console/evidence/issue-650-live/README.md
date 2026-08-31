# Single-Node Product Ready evidence

Date: 2026-08-31

Claim boundary: the `web-project-v1` virtual-company journey is ready on the
canonical single-node deployment. This evidence does not claim Cluster GA,
N-node availability, single-survivor continuity, or native bare-metal boot.

## Release authority

- Deployed Git revision: `6c90ae73742ab678daa9868f5d07a4a68643d163`.
- Release manifest SHA-256:
  `b0f928aa41474614d79ac587bef6c20622242fc8f90393ccaf807b18ad8e0acb`.
- Release artifacts: 129.
- Final token-free preflight: PASS, result digest
  `e778bd5765e9371b2bdfeb002e89f1261b8cdecdad718a4825c2ff949f6f4e39`.
- Raw preflight SHA-256:
  `2a5e77960434f9f20d2c1fad875cc738cff87454690989385ff132ef58c84ad8`.
- Raw preflight: `final-preflight.json`.

## Acceptance summary

| AC | Result | Evidence |
|---|---|---|
| AC-1 | PASS | `ac-01-readiness.md` |
| AC-2 | PASS | `ac-02-identity-parity.md` |
| AC-3 through AC-10 | PASS | Issue #695/#696 evidence linked by the M0 contract |
| AC-11 | PASS | `ac-11-provider-cost.md` |
| AC-12 | PASS | `ac-12-recovery.md` |
| AC-13 | PASS | Issue #696 `console-lineage.md` |
| AC-14 | PASS | Issue #696 `release.md` |
| AC-15 | PASS | `ac-15-soak.md` |
| AC-16 | PASS after final issue closeout | Snapshot creation and exact cleanup receipt |
| AC-17 | PASS | This evidence uses the exact name `Single-Node Product Ready` |

The authoritative token-free journey completed all 28 steps and replayed all
28 stable operations. Its final record-chain tip is
`e64a2eb3053dd46f067fcc084d99462fcbb1fe1c397c16ca822e5106fd489094`.
The public-safe replay result is retained as `journey-replay.txt`.
The live delivery lineage contains 29 nodes, 29 edges, zero blockers, explicit
customer acceptance, and durable closeout memory.
