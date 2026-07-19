# Issue #442 control-plane security hardening

Date: 2026-07-19 (UTC)

## Scope

This is corrective evidence for the four control-plane findings raised against PR
#614:

- idempotency is scoped by authenticated peer, RPC method, and key, and binds that
  scope to one request digest;
- ProvisionNode reserves its assigned NodeId in a durable operation journal and
  reuses it across seed restarts;
- every failed bootstrap phase enters compensating cleanup plus durable quarantine;
- owner mutations require the configured chef, and holder advertisements may describe
  only the authenticated sender.

Production simulation node `.240` was not contacted. Rust commands ran only through
`cargo remote -c --` on `.155`; deployment and live checks used only `.241` and
`.242`. No Claude request was made and token cost was USD 0.

After root-only backups, `.242` was destructively reset as the allowlisted bare target.
A QUIC-only partition forced a real post-health join failure, proving target quarantine
and seed-side failed state. Removing the partition and retrying the same operation
proved NodeId reuse, successful authenticated join, marker removal, and completion. A
seed restart followed by the same request proved the durable completed no-op.

## Remote build and tests on `.155`

The checked-in toolchain pin, not the build account's unrelated global default, is
effective in the isolated Cargo Remote project:

```console
$ ssh root@10.0.0.155 'cd /tmp/builds/issue-442-security/378810692033878248 && cat rust-toolchain.toml && rustc --version && cargo --version && rustup show active-toolchain'
[toolchain]
channel = "1.97.1"
components = ["rustfmt", "clippy"]
rustc 1.97.1 (8bab26f4f 2026-07-14)
cargo 1.97.1 (c980f4866 2026-06-30)
1.97.1-x86_64-unknown-linux-gnu (overridden by '/tmp/builds/issue-442-security/378810692033878248/rust-toolchain.toml')
```

```console
$ cargo remote -c -- test -p sentinel-common -p sentinel-cluster-control
sentinel-cluster-control unit: 28 passed; 0 failed
block_pull_roundtrip: 2 passed; 0 failed
control_roundtrip: 4 passed; 0 failed
sentinel-common unit: 116 passed; 0 failed
acceptance: 2 passed; acceptance_agents: 5 passed; acceptance_rooms: 5 passed
all remaining integration and doc tests: ok; 0 failed

$ cargo remote -c -- test -p sentinel-daemon --lib provision_exec::tests
running 14 tests
test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 305 filtered out

$ cargo remote -c -- test -p sentinel-daemon
sentinel-daemon library: 319 passed; 0 failed
replay-spike binary: 3 passed; 0 failed
nano_runtime_registry integration: 1 passed; 0 failed
all remaining binary and doc tests: ok; 0 failed

$ cargo remote -c -- clippy -p sentinel-cluster-control -p sentinel-common -p sentinel-daemon --all-targets -- -D warnings
Finished `dev` profile [unoptimized + debuginfo] target(s) in 3m 54s
exit=0

$ cargo remote -c -- build -p sentinel-daemon --release --bins
Finished `release` profile [optimized] target(s) in 12m 19s
exit=0

$ sha256sum target/release/sentinel-daemon target/release/control_security_probe
48cf40dcb8957ee950bbb11239939c162574dd828611d1a1c05f2916b2d23ae7  target/release/sentinel-daemon
849d5c2eadb1dc9985998301b306e919c0d1670d8569f0445a27841f86fd627a  target/release/control_security_probe

$ cargo fmt --all -- --check
[exit 0]

$ git diff --check
[exit 0]
```

The provisioning tests include journal reopen with the same NodeId and operation ID,
completed-operation no-op after reopen, key/target rebinding rejection, mode `0600`,
binary-push failure quarantine, local-hash failure quarantine, and refusal to complete
when a stale target quarantine marker cannot be cleared after authenticated join.

## Initial candidate configuration and deployment

Both candidates parsed successfully before installation. The member candidate adds
the seed as its explicit chef authority.

```console
$ /tmp/sentinel-daemon.issue442-security-final --config /opt/sentinel/config/daemon.toml --dry-run
.241 role=Seed lifecycle=GenesisSeed Dry-Run abgeschlossen total_agents=0 current_shift=1 active_agents=0
.242 role=Member lifecycle=Joining Dry-Run abgeschlossen total_agents=0 current_shift=1 active_agents=0

$ grep -n chef_node_id /tmp/daemon.toml.issue442-security  # .242
11:chef_node_id = "5016f6e4-3e5c-483b-ae5f-24feeaf39b02"
```

The member was upgraded first, then the seed. Root-owned backups were created before
each install; each install script restored its backup if the service restart failed.

```console
$ systemctl show sentinel-daemon -p ActiveEnterTimestamp -p NRestarts
.242 ActiveEnterTimestamp=Sun 2026-07-19 13:30:48 UTC NRestarts=0
.241 ActiveEnterTimestamp=Sun 2026-07-19 13:31:03 UTC NRestarts=0

$ sha256sum /opt/sentinel/bin/sentinel-daemon
.241 48cf40dcb8957ee950bbb11239939c162574dd828611d1a1c05f2916b2d23ae7
.242 48cf40dcb8957ee950bbb11239939c162574dd828611d1a1c05f2916b2d23ae7

$ grep -n chef_node_id /opt/sentinel/config/daemon.toml  # .242
11:chef_node_id = "5016f6e4-3e5c-483b-ae5f-24feeaf39b02"

$ stat -c '%n %U:%G %a' /opt/sentinel/config/daemon.toml /opt/sentinel/bin/sentinel-daemon
/opt/sentinel/config/daemon.toml root:root 644
/opt/sentinel/bin/sentinel-daemon root:root 755
```

After both restarts, each receiver accepted its configured, certificate-bound peer:

```console
$ journalctl -u sentinel-daemon --since '2026-07-19 13:30:45 UTC' --no-pager | grep 'membership peer became Alive over QUIC'
.241 node_id=6435ca03-1306-461b-a667-2f21711a176b alias=bare-node-1 previous=None outcome=Joined state=Alive
.242 node_id=5016f6e4-3e5c-483b-ae5f-24feeaf39b02 alias=test-node-0 previous=None outcome=Joined state=Alive
```

## Destructive live ProvisionNode failure, recovery, and restart

The pre-drill seed and target state was archived root-only before mutation:

```console
$ sudo sha256sum /opt/sentinel/backups/issue442-live-provision-20260719T140002Z/node-*-before.tgz
.241 8164f6d5f1c2869c5877d7dea3a8e92d49bab5fa52978a748293f22a2a01a870
.242 c37b7e19cd43fcd5a8f1f66de7ce5bccef67e71ac2e156e909dfe052df7694f2
```

The expired seed allowlist entry was renewed, its old static `.242` peer pin was
removed, and the seed restarted with the durable ProvisionNode worker. `.242`'s daemon
unit, config, binary, and control identity were removed after backup. Only UDP/8085
between the two nodes was blocked; the pinned SSH bootstrap path remained available.

```console
$ sudo systemctl disable --now sentinel-daemon.service  # .242
$ sudo rm -f /opt/sentinel/config/daemon.toml /opt/sentinel/data/control-node-{cert,key}.der /opt/sentinel/bin/sentinel-daemon /etc/systemd/system/sentinel-daemon.service
$ sudo iptables -I INPUT 1 -p udp -s 10.0.0.241 --dport 8085 -m comment --comment issue-442-live-provision-join-fail -j DROP
$ sudo iptables -I OUTPUT 1 -p udp -d 10.0.0.241 --dport 8085 -m comment --comment issue-442-live-provision-join-fail -j DROP
TARGET_RESET_AT=2026-07-19T14:01:23Z
DAEMON_ACTIVE=inactive

$ curl -i -X POST localhost:8084/operator/provision -H 'Content-Type: application/json' -d '{"pending_target_id":"bare-node-1","requested_alias":"bare-node-1","idempotency_key":"issue442-live-provision-20260719T140002Z"}'
HTTP/1.1 202 Accepted
{"accepted":true,"message":"ProvisionNode-Bootstrap angestossen (Seed-getrieben)"}
```

The target reached `active` with the reserved NodeId before the blocked QUIC join
timed out. The real compensation then disabled the service, removed staging, revoked
the dynamic peer, persisted `Failed`, and wrote the root-only marker.

```console
$ sudo cat /opt/sentinel/data/provision-ops.json  # .241
op_id=57db14c3-288f-4d65-b744-511541e35f0b
node_id=8c79a2e0-8d79-4e88-a155-613c6c1f3470
state=Failed
failure_reason="target did not join authenticated membership"

$ systemctl show sentinel-daemon -p ActiveState -p SubState -p UnitFileState -p NRestarts  # .242
ActiveState=inactive
SubState=dead
UnitFileState=disabled
NRestarts=0

$ sudo stat -c '%n %U:%G %a' /opt/sentinel/data/provision-quarantine.json
/opt/sentinel/data/provision-quarantine.json root:root 600
$ sudo cat /opt/sentinel/data/provision-quarantine.json
node_id=8c79a2e0-8d79-4e88-a155-613c6c1f3470
op_id=57db14c3-288f-4d65-b744-511541e35f0b
state=quarantined
reason="target did not join authenticated membership"

$ sudo cat /opt/sentinel/data/control-peers.json  # .241
[]
$ find /tmp -maxdepth 1 -name 'sentinel-stage-*' -o -name 'sentinel-daemon.new' | wc -l
0
```

The two firewall rules were removed (`RESIDUAL_RULES=0`) and the exact same request
was sent again. The operation and NodeId were reused, the target joined, and the
quarantine marker disappeared before `Completed` was persisted.

```console
RULES_REMOVED_AT=2026-07-19T14:03:28Z
RESIDUAL_RULES=0
RETRY_AT=2026-07-19T14:03:29Z

$ journalctl -u sentinel-daemon --since '2026-07-19 14:03:20 UTC' --no-pager | grep -E 'ProvisionNode|membership peer became Alive'
14:03:29 Bootstrap gestartet node_id=8c79a2e0-8d79-4e88-a155-613c6c1f3470
14:03:36 membership peer became Alive over QUIC node_id=8c79a2e0-8d79-4e88-a155-613c6c1f3470
14:03:38 ProvisionNode: Knoten provisioniert node_id=8c79a2e0-8d79-4e88-a155-613c6c1f3470 duration_ms=9038

$ sudo cat /opt/sentinel/data/provision-ops.json  # .241
op_id=57db14c3-288f-4d65-b744-511541e35f0b
node_id=8c79a2e0-8d79-4e88-a155-613c6c1f3470
state=Completed

$ systemctl show sentinel-daemon -p ActiveState -p UnitFileState -p NRestarts  # .242
ActiveState=active
UnitFileState=enabled
NRestarts=0
$ test ! -e /opt/sentinel/data/provision-quarantine.json && echo QUARANTINE_MARKER=absent
QUARANTINE_MARKER=absent
```

Finally, the seed was restarted and the same request was sent a third time. The worker
loaded the completed journal entry and did not touch the target process.

```console
$ sudo systemctl restart sentinel-daemon  # .241
SEED_ACTIVE_ENTER=Sun 2026-07-19 14:04:18 UTC
$ curl -X POST localhost:8084/operator/provision ...same body...
HTTP/1.1 202 Accepted
14:04:33 ProvisionNode: durably completed, no-op (AC-S2) node_id=Some(NodeId(8c79a2e0-8d79-4e88-a155-613c6c1f3470))
TARGET_ACTIVE_BEFORE=Sun 2026-07-19 14:03:36 UTC
TARGET_ACTIVE_AFTER=Sun 2026-07-19 14:03:36 UTC
```

## Live negative security probe

The probe ran from `.242` with `.242`'s existing pinned certificate against `.241`.
Its first operation was a read-only `RefQuery`. It exited zero only if all three
expected typed rejections occurred.

```console
$ sudo /tmp/control_security_probe.issue442-security-final /opt/sentinel/config/daemon.toml test-node-0
IDEMPOTENCY_CONFLICT_REJECTED method=ref_query
NON_CHEF_OWNER_MUTATION_REJECTED reason=owner_commit requires the configured chef node
FOREIGN_HOLDER_NODE_REJECTED certificate_node=8c79a2e0-8d79-4e88-a155-613c6c1f3470 claimed_node=723fb150-0380-420b-8487-fd93cd1eaa9b reason=holder advertisement node_id must match authenticated peer 8c79a2e0-8d79-4e88-a155-613c6c1f3470
```

The digest-conflict case reused one peer/method/key tuple with a different body. The
owner case used a probe-only invalid scope and was stopped at the chef authorization
boundary. The holder case was rejected before the shared block map was locked or
mutated.

## Partition, failure detection, and healing

A 15-second UDP/8085 partition was applied only between `.242` and `.241`. An `EXIT`
trap removed both rules, and the residual rule count was checked explicitly.

```console
$ sudo iptables -I INPUT 1 -p udp -s 10.0.0.241 --dport 8085 -m comment --comment issue-442-live-provision-final-membership -j DROP
$ sudo iptables -I OUTPUT 1 -p udp -d 10.0.0.241 --dport 8085 -m comment --comment issue-442-live-provision-final-membership -j DROP
PARTITION_START=2026-07-19T14:04:55Z
PARTITION_END=2026-07-19T14:05:10Z
RESIDUAL_RULES=0

$ journalctl -u sentinel-daemon --since '2026-07-19 14:04:50 UTC' --no-pager | grep -E 'membership peer (state changed|became Alive)'
.241 14:04:58 peer=.242 previous=Alive current=Suspect
.241 14:05:04 peer=.242 previous=Suspect current=Dead
.241 14:05:10 peer=.242 previous=Some(Dead) outcome=Updated state=Alive
.242 14:04:58 peer=.241 previous=Alive current=Suspect
.242 14:05:04 peer=.241 previous=Suspect current=Dead
.242 14:05:11 peer=.241 previous=Some(Dead) outcome=Updated state=Alive
```

## Post-heal runtime stability

Checked at `2026-07-19T14:07:37Z`, more than two minutes after the recovered target
and seed restart and more than two minutes after healing.

```console
$ systemctl show sentinel-daemon sentinel-gaia-loop sentinel-dashboard-backend -p Id -p ActiveState -p SubState -p NRestarts
.241 sentinel-daemon=active/running NRestarts=0
.241 sentinel-gaia-loop=active/running NRestarts=0
.241 sentinel-dashboard-backend=active/running NRestarts=0
.242 sentinel-daemon=active/running NRestarts=0
.242 sentinel-gaia-loop=active/running NRestarts=0
.242 sentinel-dashboard-backend=active/running NRestarts=0

$ journalctl -u sentinel-daemon --since '<final node deploy timestamp>' --no-pager | grep -E ' WARN | ERROR ' | grep -Ec 'zenoh::|sentinel_zenoh|Unable to connect to any locator of scouted peer' || true
.241 zenoh_warning_error_count=0
.242 zenoh_warning_error_count=0

$ journalctl _COMM=systemctl --since '<final node deploy timestamp>' --no-pager | grep -Ec 'Failed to (restart|reset failed) sentinel-(judge|projection)|start request repeated' || true
.241 restart_failure_count=0
.242 restart_failure_count=0

$ journalctl -u sentinel-daemon --since '<final node deploy timestamp>' --no-pager | grep -Eic 'panic|fatal' || true
.241 panic_fatal_count=0
.242 panic_fatal_count=0

$ sudo ss -lunp | grep -c '224.0.0.224:7446' || true
.241 zenoh_multicast_sockets=0
.242 zenoh_multicast_sockets=0

$ sudo ss -lunp | grep -Ec '0\.0\.0\.0:8085|0\.0\.0\.0:8086'
.241 quic_listeners=2
.242 quic_listeners=2

$ sudo iptables-save | grep -c 'issue-442' || true
.241 issue442_firewall_rules=0
.242 issue442_firewall_rules=0

$ sha256sum /opt/sentinel/bin/sentinel-daemon
.241 48cf40dcb8957ee950bbb11239939c162574dd828611d1a1c05f2916b2d23ae7
.242 48cf40dcb8957ee950bbb11239939c162574dd828611d1a1c05f2916b2d23ae7

$ sudo stat -c '%n %U:%G %a' /opt/sentinel/data/provision-ops.json  # .241
/opt/sentinel/data/provision-ops.json root:root 600

$ curl -fsS localhost:8084/operator/runtime-health
.241 projection_drift_detected=false last_repair_error=None ecs_tick_loop=true/restarts=0 service_health=true/restarts=0
.242 projection_drift_detected=false last_repair_error=None ecs_tick_loop=true/restarts=0 service_health=true/restarts=0
```

## Result

- A key cannot conflate peers or methods, and reusing one peer/method/key scope with
  another payload returns `IdempotencyConflict`. The bounded cache has TTL and
  capacity eviction; paged CAS gossip uses a page- and digest-specific key.
- ProvisionNode's operation, assigned NodeId, target, and terminal status survive a
  seed restart. The destructive two-node drill proved service disable, staging
  cleanup, durable target quarantine, and trust revocation after a real post-health
  join failure. A successful retry reused the operation and NodeId, cleared the target
  marker only after authenticated join, and remained a no-op after seed restart.
- Certificate identity is now an authorization input: only the configured chef can
  mutate owner state, and a holder advertisement must name its authenticated sender.
- Bidirectional membership, partition detection, healing, runtime health, and the
  original Zenoh/restart-noise regressions are clean on both lab nodes.
