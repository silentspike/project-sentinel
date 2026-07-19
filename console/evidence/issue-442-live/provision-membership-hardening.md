# Issue #442 ProvisionNode and membership hardening

Date: 2026-07-19 (UTC)

## Scope

This is corrective evidence for the review findings on PR #614:

- `ProvisionNode` must configure reciprocal QUIC peers and complete only after the
  assigned `NodeId` is observed as `Alive`.
- A pinned certificate must be bound to exactly one `NodeId`; a peer may not claim a
  different identity in a membership heartbeat.
- Control RPC idempotency must be atomic for concurrent duplicate requests, and the
  active ADR must describe the actual QUIC client and process-local durability limit.

Production simulation node `.240` was not contacted. Builds ran only through
`cargo remote -c --` on `.155`; deployment and live checks used only `.241` and
`.242`. No Claude request was made and token cost was USD 0.

The active `.242` node was not destructively reprovisioned. The target-local identity
helper and every membership security/liveness boundary were exercised live. The full
ProvisionNode transport sequence, reciprocal configuration, dynamic trust
persistence, exact-NodeId join gate, and rollback are covered by remote tests.

## Remote verification on `.155`

```console
$ cargo remote -c -- test -p sentinel-cluster-control -p sentinel-daemon
stable-x86_64-unknown-linux-gnu unchanged - rustc 1.97.1 (8bab26f4f 2026-07-14)
sentinel-cluster-control unit: 24 passed; 0 failed
block_pull_roundtrip: 2 passed; 0 failed
control_roundtrip: 4 passed; 0 failed
sentinel-daemon unit: 313 passed; 0 failed
all binary, integration, and doc-test results: ok; 0 failed

$ cargo remote -c -- test -p sentinel-daemon provision_exec::tests
stable-x86_64-unknown-linux-gnu unchanged - rustc 1.97.1 (8bab26f4f 2026-07-14)
running 9 tests
test provision_exec::tests::happy_path_drives_saga_to_completed_in_order ... ok
test provision_exec::tests::active_process_without_membership_join_never_completes ... ok
test provision_exec::tests::transport_error_quarantines_the_op ... ok
test provision_exec::tests::unhealthy_target_fails_after_polls ... ok
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 304 filtered out

$ cargo remote -c -- clippy --workspace --all-targets -- -D warnings
stable-x86_64-unknown-linux-gnu unchanged - rustc 1.97.1 (8bab26f4f 2026-07-14)
Finished `dev` profile [unoptimized + debuginfo]
exit=0

$ cargo remote -c -- build -p sentinel-daemon --release --bins
stable-x86_64-unknown-linux-gnu unchanged - rustc 1.97.1 (8bab26f4f 2026-07-14)
Finished `release` profile [optimized] target(s) in 1m 47s

$ sha256sum target/release/sentinel-daemon target/release/membership_spoof_probe
b6f3cb0411abfad8ea5718712b69b2756858da93d2a861cc399ad359794fd04c  target/release/sentinel-daemon
1dd60f65328c51b40d34e4a1956a88eeea16a169328ef0f8a34bb2ac7c0ee2bc  target/release/membership_spoof_probe

$ cargo fmt --all -- --check
[exit 0]

$ git diff --check
[exit 0]
```

The focused ProvisionNode suite includes the fail-closed case where the target
process is active but its exact assigned `NodeId` never joins membership. The control
crate suite includes concurrent same-key idempotency and one-to-one
certificate/NodeId binding tests.

## Deployment configuration and provenance

Both candidate configurations were parsed with the staged release binary before the
running services were changed. Root-owned backups were created with suffix
`pre-issue442-20260719T1027Z`.

```console
$ /tmp/sentinel-daemon.issue442 --config /tmp/daemon.toml.issue442 --dry-run
.241: Dry-Run abgeschlossen total_agents=0 current_shift=1 active_agents=0
.242: Dry-Run abgeschlossen total_agents=0 current_shift=1 active_agents=0

$ grep -nE '^control_|^node_id|^alias|^addr|^cert_fingerprint' /tmp/daemon.toml.issue442
.241:
15:control_bind = "0.0.0.0:8085"
16:control_advertise = "10.0.0.241:8085"
17:node_id = "5016f6e4-3e5c-483b-ae5f-24feeaf39b02"
20:alias = "test-node-0"
85:node_id = "6435ca03-1306-461b-a667-2f21711a176b"
86:alias = "test-node-1"
87:addr = "10.0.0.242:8085"
88:cert_fingerprint = "72d2f8b3baefb425f3ec50c34419aeef8767f9db7853be9b969dcdb1776130b6"

.242:
6:control_bind = "0.0.0.0:8085"
7:node_id = "6435ca03-1306-461b-a667-2f21711a176b"
10:alias = "bare-node-1"
13:node_id = "5016f6e4-3e5c-483b-ae5f-24feeaf39b02"
14:alias = "test-node-0"
15:addr = "10.0.0.241:8085"
16:cert_fingerprint = "4026ff7a2f0ab2c6a4bd8b30b286e36ac23f9d918a29cee982ade65c056311b8"

$ sudo sha256sum /opt/sentinel/bin/sentinel-daemon
.241 b6f3cb0411abfad8ea5718712b69b2756858da93d2a861cc399ad359794fd04c
.242 b6f3cb0411abfad8ea5718712b69b2756858da93d2a861cc399ad359794fd04c
```

The seed readback proves that the ProvisionNode worker now starts only with the live
QUIC trust registry, shared membership view, and reachable advertised seed endpoint:

```console
$ journalctl -u sentinel-daemon --since '2026-07-19 10:32:00' --no-pager | grep -E 'control stream started|QUIC membership service spawned|ProvisionNode worker'
.241 Cluster 12: control stream started bind_addr=0.0.0.0:8085 fingerprint=4026ff7a... peers=1 pull_server=true
.241 Cluster 12: QUIC membership service spawned node_id=5016f6e4-3e5c-483b-ae5f-24feeaf39b02 peers=1
.241 Cluster 12: ProvisionNode worker spawned with QUIC join verification targets=1
```

## Target-local control identity

The staged verified daemon generated the private key on `.242`; the second invocation
loaded the same identity. Only the fingerprint was printed, and the key remained
root-owned mode `0600`.

```console
$ sudo /tmp/sentinel-daemon.issue442 generate-control-identity --alias issue442-probe --cert /tmp/issue442-identity/cert.der --key /tmp/issue442-identity/key.der
fingerprint_first=c0b10f57c9839431d594203b5108c9c21c47aa397839dd54664bb89a4bf9628b
fingerprint_second=c0b10f57c9839431d594203b5108c9c21c47aa397839dd54664bb89a4bf9628b

$ sudo stat -c '%U:%G %a %n' /tmp/issue442-identity /tmp/issue442-identity/cert.der /tmp/issue442-identity/key.der
root:root 700 /tmp/issue442-identity
root:root 644 /tmp/issue442-identity/cert.der
root:root 600 /tmp/issue442-identity/key.der
```

The temporary identity directory was removed after the readback.

## Bidirectional authenticated membership

Each receiver observed the exact NodeId bound to the connecting certificate:

```console
$ journalctl -u sentinel-daemon --since '2026-07-19 10:32:00' --no-pager | grep 'membership peer became Alive over QUIC'
.241 node_id=6435ca03-1306-461b-a667-2f21711a176b alias=bare-node-1 previous=None outcome=Joined state=Alive
.242 node_id=5016f6e4-3e5c-483b-ae5f-24feeaf39b02 alias=test-node-0 previous=None outcome=Joined state=Alive
```

## Certificate-to-NodeId spoof rejection

The probe on `.242` presented `.242`'s real, pinned control certificate to `.241` but
claimed a different NodeId in the heartbeat. The server rejected it with a typed
identity mismatch; probe exit status was zero only because that rejection occurred.

```console
$ sudo /tmp/membership_spoof_probe.issue442 /opt/sentinel/config/daemon.toml test-node-0 aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa
SPOOF_REJECTED certificate_node=6435ca03-1306-461b-a667-2f21711a176b claimed_node=aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa reason=membership node_id aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa does not match authenticated peer 6435ca03-1306-461b-a667-2f21711a176b
```

## Partition, failure detection, and healing

A 15-second `.241`/`.242` UDP partition was applied on `.242`. The shell installed an
`EXIT/INT/TERM` trap before inserting the rules and removed both rules at the end.

```console
$ sudo iptables -I INPUT 1 -p udp -s 10.0.0.241 -j DROP
$ sudo iptables -I OUTPUT 1 -p udp -d 10.0.0.241 -j DROP
PARTITION_ACTIVE 2026-07-19T10:33:21Z

$ journalctl -u sentinel-daemon --since '2026-07-19 10:33:20' --no-pager | grep 'membership peer state changed'
.241 node_id=6435ca03-1306-461b-a667-2f21711a176b previous=Alive current=Suspect
.241 node_id=6435ca03-1306-461b-a667-2f21711a176b previous=Suspect current=Dead
.242 node_id=5016f6e4-3e5c-483b-ae5f-24feeaf39b02 previous=Alive current=Suspect
.242 node_id=5016f6e4-3e5c-483b-ae5f-24feeaf39b02 previous=Suspect current=Dead

PARTITION_RULES_REMOVED 2026-07-19T10:33:36Z

$ journalctl -u sentinel-daemon --since '2026-07-19 10:33:36' --no-pager | grep 'membership peer became Alive over QUIC'
.241 node_id=6435ca03-1306-461b-a667-2f21711a176b previous=Some(Dead) outcome=Updated state=Alive
.242 node_id=5016f6e4-3e5c-483b-ae5f-24feeaf39b02 previous=Some(Dead) outcome=Updated state=Alive

$ sudo iptables -C INPUT ...; sudo iptables -C OUTPUT ...
.241 partition_rule_present_input=no partition_rule_present_output=no
.242 partition_rule_present_input=no partition_rule_present_output=no
```

## Post-heal stability

```console
$ systemctl is-active sentinel-daemon sentinel-gaia-loop sentinel-dashboard-backend
.241 sentinel-daemon=active sentinel-gaia-loop=active sentinel-dashboard-backend=active
.242 sentinel-daemon=active sentinel-gaia-loop=active sentinel-dashboard-backend=active

$ systemctl show sentinel-daemon sentinel-gaia-loop sentinel-dashboard-backend -p Id -p NRestarts
.241 sentinel-daemon=0 sentinel-gaia-loop=0 sentinel-dashboard-backend=0
.242 sentinel-daemon=0 sentinel-gaia-loop=0 sentinel-dashboard-backend=0

$ journalctl -u sentinel-daemon --since '2026-07-19 10:32:00' --no-pager | grep -E ' WARN | ERROR ' | grep -Ec 'zenoh::|sentinel_zenoh'
.241 zenoh_subsystem_warning_error_count=0
.242 zenoh_subsystem_warning_error_count=0

$ journalctl -u sentinel-daemon --since '2026-07-19 10:32:00' --no-pager | grep -Eic 'panic|fatal'
.241 panic_fatal_count=0
.242 panic_fatal_count=0

$ journalctl -u sentinel-daemon --since '2026-07-19 10:32:00' --no-pager | grep -Eic 'restart.*fail|failed.*restart|start request repeated'
.241 restart_failure_count=0
.242 restart_failure_count=0

$ sudo ss -uapn | grep -Ec '(^|[[:space:]])(224\.|239\.|ff0[0-9a-f]:)'
.241 multicast_udp_socket_count=0
.242 multicast_udp_socket_count=0

$ sudo ss -ulnp | grep -E '(:8085|:8086)'
.241 0.0.0.0:8085 sentinel-daemon; 0.0.0.0:8086 sentinel-daemon
.242 0.0.0.0:8085 sentinel-daemon; 0.0.0.0:8086 sentinel-daemon
```

The broader warning stream still contains pre-existing host/configuration warnings
such as the disabled NATS bridge on `.242`; those are not Zenoh transport warnings and
are not represented as zero here. The reviewed regression conditions are zero:
Zenoh subsystem warnings/errors, multicast UDP sockets, restart failures, panics, and
fatals.

## Result

- ProvisionNode renders `control_bind` plus the seed peer, generates the target key on
  the target, authorizes and durably stores the target peer on the seed, and cannot
  complete without observing the exact assigned NodeId as authenticated `Alive`.
- Every inbound control request carries the NodeId resolved from its certificate;
  membership rejects a mismatched claimed NodeId before ingestion.
- Same-key concurrent control requests compute their effect once per daemon process;
  ADR-2 explicitly does not claim crash-durable exactly-once behavior.
- Bidirectional membership, partition detection, healing, Zenoh isolation, and clean
  service restart counters are verified on `.241` and `.242`.
