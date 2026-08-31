# AC-1 single-node readiness

Result: PASS on 2026-08-31.

- Runtime target class: `SINGLE_NODE`; no cluster section or peer is required.
- Exact deployed revision: `6c90ae73742ab678daa9868f5d07a4a68643d163`.
- Manifest: 129 artifacts; SHA-256
  `b0f928aa41474614d79ac587bef6c20622242fc8f90393ccaf807b18ad8e0acb`.
- Eight required services and two timer outcomes passed the fail-closed
  preflight; 10 required listeners had the expected owners.
- Daemon, Gateway, Judge, Projection, NATS bridge, NATS server, Dashboard
  backend, and Gaia loop were active. Service results were successful and
  restart counters were zero.
- Fourteen loopback readiness/health endpoints passed.
- NATS bridge readiness was HTTP 200 with `ready=true`.
- Company workflow readiness was `ready`, with execution, completion, gate,
  and delivery-publication queues all zero.

Final preflight result digest:
`e778bd5765e9371b2bdfeb002e89f1261b8cdecdad718a4825c2ff949f6f4e39`.
