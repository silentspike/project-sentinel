# TOGAF Handoff For Issue 473

No TOGAF HTML edit is included in this PR by task instruction.

Architecture note for the closing/control session:

- Issue #473 adds two dashboard TLS deployment modes for TOGAF Cluster 03
  infrastructure and Cluster 07 deployment.
- Zero-Config remains self-signed plus WebTransport certificate-hash pinning.
- Production mode loads deployer-provided PEM files, returns no cert hash, and
  relies on browser CA validation instead of pinning.
- Authentication and bind-address policy remain in #474 and are only
  cross-referenced from `docs/deployment/dashboard-tls.md`.
