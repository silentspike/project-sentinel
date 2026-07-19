# Issue #618 control-plane revocation and metastore fail-closed evidence

Date: 2026-07-19 (UTC)

## Scope

This evidence covers the corrective security contract tracked by issue #618:

- revoking a certificate-bound peer closes its already-established QUIC control and
  block-pull sessions;
- the revoked certificate cannot open another stream or reconnect until it is
  explicitly authorized again;
- an unavailable cluster metastore returns a typed rejection for owner mutations
  while authenticated membership remains available;
- the final release artifact is deployed on both lab nodes without reintroducing
  Zenoh discovery warnings, multicast sockets, or restart loops.

Rust commands ran only through `cargo remote -c --` on `.155`. Deployment and live
checks used only `.241` and `.242`. Production simulation node `.240` was not
contacted. No Claude request was made and token cost was USD 0.

## Remote verification on `.155`

```console
$ cargo remote -c -- test -p sentinel-cluster-control
stable-x86_64-unknown-linux-gnu unchanged - rustc 1.97.1 (8bab26f4f 2026-07-14)
sentinel-cluster-control unit: 29 passed; 0 failed
block_pull_roundtrip: 3 passed; 0 failed
control_roundtrip: 5 passed; 0 failed
doc-tests: 0 failed

$ cargo remote -c -- test -p sentinel-daemon --lib
running 320 tests
test result: ok. 320 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ cargo remote -c -- clippy -p sentinel-cluster-control -p sentinel-daemon --all-targets -- -D warnings
stable-x86_64-unknown-linux-gnu unchanged - rustc 1.97.1 (8bab26f4f 2026-07-14)
Finished `dev` profile [unoptimized + debuginfo] target(s) in 28.50s
exit=0

$ cargo remote -c -- build -p sentinel-daemon --release --bins
stable-x86_64-unknown-linux-gnu unchanged - rustc 1.97.1 (8bab26f4f 2026-07-14)
Finished `release` profile [optimized] target(s) in 1m 40s
exit=0

$ sha256sum target/release/sentinel-daemon target/release/cluster_fail_closed_probe
4e18c66915f1c2f692d0355c9da238cad688634d8bd70db62e72361c6095e5ae  target/release/sentinel-daemon
a4774268e1870e37b08d009ffda3e2c6ddf521db561f1c4b8f614917fa2ae6ac  target/release/cluster_fail_closed_probe
```

The real-QUIC integration tests retain one authenticated connection across multiple
streams. They prove an initial request succeeds, `PeerRegistry::revoke` closes the
live connection, a subsequent stream and reconnect fail, and explicit authorization
restores service. Separate tests exercise both control RPC and block pull.

## Deployment provenance

Before deployment, both running services used the same previous binary and had no
automatic restarts. Their unit configuration identified the exact install path.

```console
$ systemctl show sentinel-daemon -p ExecStart -p ActiveState -p SubState -p NRestarts
.241 ExecStart=/opt/sentinel/bin/sentinel-daemon --config /opt/sentinel/config/daemon.toml active/running NRestarts=0
.242 ExecStart=/opt/sentinel/bin/sentinel-daemon --config /opt/sentinel/config/daemon.toml active/running NRestarts=0

$ sha256sum /opt/sentinel/bin/sentinel-daemon
.241 48cf40dcb8957ee950bbb11239939c162574dd828611d1a1c05f2916b2d23ae7
.242 48cf40dcb8957ee950bbb11239939c162574dd828611d1a1c05f2916b2d23ae7
```

The release daemon was staged through `/tmp`, installed root-owned mode `0755`, and
the service was restarted on each lab node.

```console
$ sha256sum /opt/sentinel/bin/sentinel-daemon /tmp/cluster_fail_closed_probe
.241 4e18c66915f1c2f692d0355c9da238cad688634d8bd70db62e72361c6095e5ae  /opt/sentinel/bin/sentinel-daemon
.241 a4774268e1870e37b08d009ffda3e2c6ddf521db561f1c4b8f614917fa2ae6ac  /tmp/cluster_fail_closed_probe
.242 4e18c66915f1c2f692d0355c9da238cad688634d8bd70db62e72361c6095e5ae  /opt/sentinel/bin/sentinel-daemon
.242 a4774268e1870e37b08d009ffda3e2c6ddf521db561f1c4b8f614917fa2ae6ac  /tmp/cluster_fail_closed_probe

$ stat -c '%U:%G %a %n' /opt/sentinel/bin/sentinel-daemon
.241 root:root 755 /opt/sentinel/bin/sentinel-daemon
.242 root:root 755 /opt/sentinel/bin/sentinel-daemon
```

## Release-binary security probe

The probe uses loopback QUIC with generated server/client certificates, the production
`PeerRegistry`, control server, block-pull server, membership wrapper, chef gate, and
fail-closed terminal handler. It does not fake the transport or bypass authentication.

```console
$ timeout 15 /tmp/cluster_fail_closed_probe
.241:
MEMBERSHIP_AVAILABLE response=MembershipAccepted
METASTORE_FAIL_CLOSED response=Rejected reason=cluster_metastore_unavailable
LIVE_SESSIONS_REVOKED count=2
POST_REVOKE_DENIED control=true block_pull=true reconnect=true
EXPLICIT_REAUTH_RECOVERED control=true block_pull=true

.242:
MEMBERSHIP_AVAILABLE response=MembershipAccepted
METASTORE_FAIL_CLOSED response=Rejected reason=cluster_metastore_unavailable
LIVE_SESSIONS_REVOKED count=2
POST_REVOKE_DENIED control=true block_pull=true reconnect=true
EXPLICIT_REAUTH_RECOVERED control=true block_pull=true
```

The count of two is the established control connection plus the established block-pull
connection. Probe exit status was zero on both nodes only after every assertion passed.

## Real two-node health

After deployment, each running daemon accepted the other node's certificate-bound
heartbeat over the production QUIC control transport.

```console
$ journalctl -u sentinel-daemon --since '<service ActiveEnterTimestamp>' --no-pager | grep 'membership peer became Alive over QUIC'
.241 node_id=8c79a2e0-8d79-4e88-a155-613c6c1f3470 alias=bare-node-1 previous=None outcome=Joined state=Alive
.242 node_id=5016f6e4-3e5c-483b-ae5f-24feeaf39b02 alias=test-node-0 previous=None outcome=Joined state=Alive
```

The post-deploy stability readback was taken from each daemon's exact
`ActiveEnterTimestamp` (`15:14:46 UTC` on `.241`, `15:14:47 UTC` on `.242`).

```console
$ systemctl show sentinel-daemon sentinel-gaia-loop sentinel-dashboard-backend -p Id -p ActiveState -p SubState -p NRestarts
.241 sentinel-daemon=active/running NRestarts=0
.241 sentinel-gaia-loop=active/running NRestarts=0
.241 sentinel-dashboard-backend=active/running NRestarts=0
.242 sentinel-daemon=active/running NRestarts=0
.242 sentinel-gaia-loop=active/running NRestarts=0
.242 sentinel-dashboard-backend=active/running NRestarts=0

$ start=$(systemctl show sentinel-daemon -p ActiveEnterTimestamp --value)
$ journalctl -u sentinel-daemon --since "$start" --no-pager | grep -E '( WARN | ERROR ).*(zenoh::|sentinel_zenoh)|Unable to connect to any locator of scouted peer' | wc -l
$ journalctl -u sentinel-daemon --since "$start" --no-pager | grep -Ei 'restart.*fail|failed.*restart|start request repeated' | wc -l
$ journalctl -u sentinel-daemon --since "$start" --no-pager | grep -Ei 'panic|fatal' | wc -l
$ journalctl -u sentinel-daemon --since "$start" --no-pager | grep -c 'membership peer became Alive over QUIC'
$ ss -uapn | grep -Ec '(^|[[:space:]])(224\.|239\.|ff0[0-9a-f]:)' || true
.241 zenoh_warning_error_count=0 restart_failure_count=0 panic_fatal_count=0 membership_alive_count=1 multicast_udp_socket_count=0
.242 zenoh_warning_error_count=0 restart_failure_count=0 panic_fatal_count=0 membership_alive_count=1 multicast_udp_socket_count=0

$ ss -ulnp | grep -E '(:8085|:8086)'
.241 0.0.0.0:8085 sentinel-daemon; 0.0.0.0:8086 sentinel-daemon
.242 0.0.0.0:8085 sentinel-daemon; 0.0.0.0:8086 sentinel-daemon
```

The Zenoh counter includes only `WARN`/`ERROR` entries from the Zenoh modules plus the
historical locator warning text. Normal Zenoh `INFO` startup and shutdown lines are not
misreported as warnings.

## Explicit boundaries

- The live nodes' healthy metastore files were not corrupted or removed. The exact
  missing-metastore handler composition is exercised by the release probe and daemon
  regression test; destructive production-data corruption is not required.
- The active cluster peers were not revoked from the real two-node registry because
  doing so would intentionally take membership and block transfer offline. Real QUIC
  revocation behavior is exercised by the release binary on both target kernels.
- General certificate-authority lifecycle and certificate rotation remain outside
  issue #618. This change enforces the existing pinned-certificate revocation boundary.
- `.240`, TOGAF HTML, and unrelated services were not modified.

## ORC final correction and re-verification

The ORC review found that the first correction tracked only server-accepted sessions
and that `ProvisionNode` could persist `Completed` before its audit event. The final
revision therefore also registers outbound client sessions, removes remotely closed
sessions from the registry immediately, and makes the idempotent `NodeProvisioned`
append a prerequisite for `Completed`. An injected append failure persists `Failed`,
revokes the peer, and quarantines the target.

The first expanded release probe correctly rejected the candidate because closed
client wrappers were still counted in the local registry (`expected 2, got 4`). The
registry lifecycle was fixed and a regression test added before the final build and
deployment. This failed attempt was not accepted as evidence.

```console
$ cargo remote -c -- fmt --all -- --check
exit=0

$ cargo remote -c -- test -p sentinel-cluster-control
exit=0

$ cargo remote -c -- test -p sentinel-daemon provision_exec
exit=0

$ cargo remote -c -- test -p sentinel-daemon --lib
exit=0

$ cargo remote -c -- clippy --workspace --all-targets -- -D warnings
exit=0

$ cargo remote -c -- build -p sentinel-daemon --release --bins
exit=0

$ typos
exit=0

$ git diff --check
exit=0

$ sha256sum target/release/sentinel-daemon target/release/cluster_fail_closed_probe
815b6118441960f1eae13dea0dcf694b0f723cfe16ddc92628e46134886f2ba9  target/release/sentinel-daemon
e5ad876e2ea72ec8b08fe5c43db4e381bc3fac18d2a2ac9f28e50d724923e928  target/release/cluster_fail_closed_probe
```

The final release artifacts were installed identically on `.241` and `.242`. The
previous daemon binary remains on each node as
`/opt/sentinel/bin/sentinel-daemon.pre-issue-618` for rollback.

```console
$ sha256sum /opt/sentinel/bin/sentinel-daemon /tmp/cluster_fail_closed_probe.issue-618
.241 815b6118441960f1eae13dea0dcf694b0f723cfe16ddc92628e46134886f2ba9  /opt/sentinel/bin/sentinel-daemon
.241 e5ad876e2ea72ec8b08fe5c43db4e381bc3fac18d2a2ac9f28e50d724923e928  /tmp/cluster_fail_closed_probe.issue-618
.242 815b6118441960f1eae13dea0dcf694b0f723cfe16ddc92628e46134886f2ba9  /opt/sentinel/bin/sentinel-daemon
.242 e5ad876e2ea72ec8b08fe5c43db4e381bc3fac18d2a2ac9f28e50d724923e928  /tmp/cluster_fail_closed_probe.issue-618

$ timeout 20 /tmp/cluster_fail_closed_probe.issue-618
.241 and .242 (identical output, exit=0):
MEMBERSHIP_AVAILABLE response=MembershipAccepted
METASTORE_FAIL_CLOSED response=Rejected reason=cluster_metastore_unavailable
LIVE_SESSIONS_REVOKED count=2
POST_REVOKE_DENIED control=true block_pull=true reconnect=true
EXPLICIT_REAUTH_RECOVERED control=true block_pull=true
OUTBOUND_SESSIONS_REVOKED count=2
OUTBOUND_POST_REVOKE_DENIED control=true block_pull=true reconnect=true
OUTBOUND_REAUTH_RECOVERED control=true block_pull=true
```

Post-deploy runtime readback used the final daemon start time
`2026-07-19 17:03:51 UTC` on both nodes.

```console
$ systemctl show sentinel-daemon sentinel-gaia-loop sentinel-dashboard-backend -p Id -p ActiveState -p SubState -p NRestarts
.241 sentinel-daemon=active/running NRestarts=0
.241 sentinel-gaia-loop=active/running NRestarts=0
.241 sentinel-dashboard-backend=active/running NRestarts=0
.242 sentinel-daemon=active/running NRestarts=0
.242 sentinel-gaia-loop=active/running NRestarts=0
.242 sentinel-dashboard-backend=active/running NRestarts=0

$ journalctl -u sentinel-daemon --since '2026-07-19 17:03:50 UTC' --grep 'membership peer became Alive over QUIC'
.241 peer=.242 previous=None outcome=Joined state=Alive
.242 peer=.241 previous=None outcome=Joined state=Alive

$ post-deploy counters since 2026-07-19 17:03:50 UTC
.241 zenoh_warning_error=0 restart_failure=0 panic_fatal=0
.242 zenoh_warning_error=0 restart_failure=0 panic_fatal=0

$ sudo ss -ulnp | grep -E '(:8085|:8086)'
.241 0.0.0.0:8085 sentinel-daemon; 0.0.0.0:8086 sentinel-daemon
.242 0.0.0.0:8085 sentinel-daemon; 0.0.0.0:8086 sentinel-daemon
```

The active `.242` was not destructively reprovisioned again. The event-before-
`Completed` failure boundary is deterministic saga behavior covered by the injected
EventStore-failure test; the existing live provisioning/quarantine/retry evidence
remains in `console/evidence/issue-442-live/control-plane-security-hardening.md`.
