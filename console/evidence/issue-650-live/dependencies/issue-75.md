# Issue #75 Dependency Readback for M0

Status: `PARTIAL - LIVE NEGATIVE INJECTION OUTSTANDING`

Timestamp: 2026-07-20 UTC.

Target: canonical single-node VM. Cluster nodes were not contacted.

## Repository State

The full-cage implementation is present on the current `origin/main` lineage:

- agent bwrap configuration uses `--unshare-all` and does not emit `--share-net`;
- bwrap `--info-fd` yields the sandboxed child PID;
- the verifier compares the child network namespace to the daemon network
  namespace;
- `Isolated` continues, `ProbeError` warns without terminating, and
  `NotIsolated` records a degraded runtime state, emits `AgentIsolationFailed`
  where an event store is available, terminates the process, and tears down its
  sandbox resources; and
- the historical bridge/veth provisioning path is not used.

Remote package tests and Clippy with warnings denied passed during the #650
pre-deployment gate.

## Live Positive Readback

The existing deployment was inspected without restarting services:

| Check | Result |
| --- | --- |
| Resident `agent-runtime` processes | 26 |
| Distinct agent network namespaces | 26 |
| Agent namespace equal to daemon namespace | 0 |
| Representative namespace interfaces | Loopback only |
| Representative external TCP probe | Blocked with `Network is unreachable` |
| Host `veth-*` or `vp-*` links | 0 |
| Matching isolation/bridge error records in 30-minute daemon journal | 0 |

This proves the normal full-cage path and structural cross-agent isolation for the
currently resident shift. It does not by itself prove the `NotIsolated` operator
response.

## Remaining Gate

Issue #75 requires a target-runtime fault injection that forces the sandboxed child
onto the daemon network namespace and proves all of the following on the real daemon
path:

1. the un-caged child is detected using the child PID rather than the bwrap
   supervisor PID;
2. the child is terminated;
3. health/runtime state becomes visibly degraded;
4. the durable `AgentIsolationFailed` record is emitted on an event-owning path;
5. no un-caged runtime remains; and
6. the canonical binary and healthy full-cage shift are restored afterward.

The current production binary has no safe runtime injection control, and the merged
unit tests cover namespace classification but not the complete termination/event
path. Therefore #75 remains open and is not represented as verified.
