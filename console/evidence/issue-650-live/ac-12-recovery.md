# AC-12 representative restart recovery

Result: PASS.

Exactly one representative daemon restart was injected at the persisted
`after_agreement_project` checkpoint. Additional workflow restart points were
not repeated because they use the same durable ledger/outbox mechanism.

- Journey: `single-node-web-company-v5`.
- Stable steps present: 28/28.
- Stable steps replay-verified: 28/28.
- Restart checkpoint count: 1.
- Restart result: `COMPLETE`.
- Authoritative replay verified: true.
- Final chain tip:
  `e64a2eb3053dd46f067fcc084d99462fcbb1fe1c397c16ca822e5106fd489094`.
- No duplicate provider call, action, artifact, release, acceptance, or memory
  effect was observed.
- Post-restart services returned success with restart counters zero.

The source correction and initial receipt are documented in issue #650 comment
`5468895682`; the final release repeated the complete stable-operation replay.
