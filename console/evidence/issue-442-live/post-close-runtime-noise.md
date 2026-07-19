# Issue #442 post-close runtime-noise correction

Date: 2026-07-19 (UTC)

## Scope and acceptance gap

This is corrective evidence after the original Issue #442 deployment. The earlier
acceptance checked service activity, `NRestarts`, and Gaia request paths, but did not
scan the daemon warning stream or the daemon's short-lived `systemctl` children. That
was insufficient: the daemon remained active with `NRestarts=0` while continuously
emitting Zenoh warnings on both lab nodes and launching failed restart commands on
`.242`.

Production simulation node `.240` was not contacted. All runtime work below used only
`.241` and `.242`. No Claude request was made and no token was consumed.

## Before: reproduced faults

Ten-minute counts immediately before deployment:

```console
$ for node in 241 242; do ssh ubuntu@10.0.0.$node 'sudo journalctl -u sentinel-daemon --since "10 min ago" --no-pager | grep -c "Unable to connect to any locator of scouted peer" || true; sudo journalctl _COMM=systemctl --since "10 min ago" --no-pager | grep -Ec "Failed to (restart|reset failed) sentinel-(judge|projection)" || true'; done
.241 zenoh_warnings=75 restart_failures=0
.242 zenoh_warnings=75 restart_failures=11
```

The nodes discovered each other's loopback-only Zenoh listener over LAN multicast and
retried every eight seconds:

```console
$ ssh ubuntu@10.0.0.241 'sudo journalctl -u sentinel-daemon --since "2026-07-19 07:00:00 UTC" --until "2026-07-19 07:11:20 UTC" --no-pager | grep "Unable to connect to any locator of scouted peer" | head -2'
Jul 19 07:00:00 sentinel-test-node-0 sentinel-daemon[69325]: 2026-07-19T07:00:00.618140Z  WARN zenoh::net::runtime::orchestrator: Unable to connect to any locator of scouted peer 337d181568ca17869c5becdcdf5073eb: [tcp/127.0.0.1:33819]
Jul 19 07:00:08 sentinel-test-node-0 sentinel-daemon[69325]: 2026-07-19T07:00:08.618779Z  WARN zenoh::net::runtime::orchestrator: Unable to connect to any locator of scouted peer 337d181568ca17869c5becdcdf5073eb: [tcp/127.0.0.1:33819]

$ ssh ubuntu@10.0.0.242 'sudo journalctl -u sentinel-daemon --since "2026-07-19 07:00:00 UTC" --until "2026-07-19 07:10:20 UTC" --no-pager | grep "Unable to connect to any locator of scouted peer" | head -2'
Jul 19 07:00:00 sentinel-test-node-1 sentinel-daemon[84474]: 2026-07-19T07:00:00.617448Z  WARN zenoh::net::runtime::orchestrator: Unable to connect to any locator of scouted peer 164a9a7ab40fafffdbd2682068b6bdaf: [tcp/127.0.0.1:38171]
Jul 19 07:00:08 sentinel-test-node-1 sentinel-daemon[84474]: 2026-07-19T07:00:08.618325Z  WARN zenoh::net::runtime::orchestrator: Unable to connect to any locator of scouted peer 164a9a7ab40fafffdbd2682068b6bdaf: [tcp/127.0.0.1:38171]
```

The provisioned `.242` member inherited default monitored services that are not
installed on a daemon-only member:

```console
$ ssh ubuntu@10.0.0.242 'sudo journalctl _COMM=systemctl --since "2026-07-19 07:00:00 UTC" --until "2026-07-19 07:10:20 UTC" --no-pager | grep -E "Failed to (restart|reset failed) sentinel-(judge|projection)" | head -4'
Jul 19 07:00:14 sentinel-test-node-1 systemctl[370602]: Failed to restart sentinel-judge.service: Unit sentinel-judge.service not found.
Jul 19 07:01:14 sentinel-test-node-1 systemctl[370608]: Failed to restart sentinel-judge.service: Unit sentinel-judge.service not found.
Jul 19 07:01:14 sentinel-test-node-1 systemctl[370610]: Failed to restart sentinel-projection.service: Unit sentinel-projection.service not found.
Jul 19 07:02:14 sentinel-test-node-1 systemctl[370616]: Failed to restart sentinel-judge.service: Unit sentinel-judge.service not found.

$ ssh ubuntu@10.0.0.241 'sudo grep -nE "^\[daemon.platform_controlplane\]|^monitored_services" /opt/sentinel/config/daemon.toml'
65:[daemon.platform_controlplane]
79:monitored_services = []

$ ssh ubuntu@10.0.0.242 'sudo grep -nE "^\[daemon.platform_controlplane\]|^monitored_services" /opt/sentinel/config/daemon.toml'
17:[daemon.platform_controlplane]
```

## Source correction

- `crates/sentinel-zenoh/src/lib.rs`: keep the existing loopback listener and also set
  `scouting/multicast/enabled=false` in both the shared-memory and network-fallback
  configuration paths. Cross-node communication remains QUIC ClusterControl, not Zenoh.
- `services/sentinel-daemon/src/provision_exec.rs`: render
  `monitored_services = []` for a daemon-only provisioned member, next to the existing
  `llm_enabled = false` member override.
- The active Rust/DEV-010 contract was advanced from 1.95.0 to the current stable
  1.97.1 release in `rust-toolchain.toml`, every CI job and effective-version assertion,
  the deploy preflight, and the in-repo DEV-010 deviation record.
- Unit tests read back the effective Zenoh JSON setting and parse the rendered member
  TOML into `DaemonConfigFile` to verify the semantic values.

## Remote verification on build server `.155`

```console
$ cargo remote -c -- test -p sentinel-zenoh --lib
stable-x86_64-unknown-linux-gnu unchanged - rustc 1.97.1 (8bab26f4f 2026-07-14)
test tests::loopback_transport_disables_multicast_scouting ... ok
test result: ok. 41 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ cargo remote -c -- test -p sentinel-daemon provision_exec::tests::render_daemon_toml_is_member_config -- --exact --nocapture
test provision_exec::tests::render_daemon_toml_is_member_config ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 310 filtered out

$ cargo remote -c -- test -p sentinel-zenoh -p sentinel-daemon
stable-x86_64-unknown-linux-gnu unchanged - rustc 1.97.1 (8bab26f4f 2026-07-14)
Finished `test` profile [unoptimized + debuginfo] target(s) in 4m 31s
test result: ok. 311 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 41 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ cargo remote -c -- clippy --workspace --all-targets -- -D warnings
stable-x86_64-unknown-linux-gnu unchanged - rustc 1.97.1 (8bab26f4f 2026-07-14)
Finished `dev` profile [unoptimized + debuginfo] target(s) in 2m 29s

$ cargo remote -c -- build -p sentinel-daemon --release
stable-x86_64-unknown-linux-gnu unchanged - rustc 1.97.1 (8bab26f4f 2026-07-14)
Finished `release` profile [optimized] target(s) in 6m 54s

$ sha256sum target/release/sentinel-daemon
69236a3ab62699d7af303eae25e194d78bb06d33cc94b3c6c042a351c703b54d  target/release/sentinel-daemon
```

The final remote run, repository pin, CI assertions, DEV-010 record, and deployed
artifact all use Rust 1.97.1. An earlier mixed-cache artifact was discarded; the hash
above is from the clean final 1.97.1 rebuild and is the only hash used for the final
soak.

## Deployment and immediate readback

The previous daemon binary and `daemon.toml` were backed up on each node before the
root-owned replacement was installed. `.242` received the explicit empty service list;
`.241` already had that setting.

```console
$ sha256sum /opt/sentinel/bin/sentinel-daemon   # run on both nodes after install
.241 69236a3ab62699d7af303eae25e194d78bb06d33cc94b3c6c042a351c703b54d
.242 69236a3ab62699d7af303eae25e194d78bb06d33cc94b3c6c042a351c703b54d

$ systemctl is-active sentinel-daemon sentinel-gaia-loop sentinel-dashboard-backend
active
active
active

$ systemctl show sentinel-daemon -p NRestarts
NRestarts=0

$ sudo grep -nE '^\[daemon.platform_controlplane\]|^monitored_services' /opt/sentinel/config/daemon.toml
.241 65:[daemon.platform_controlplane]
.241 79:monitored_services = []
.242 17:[daemon.platform_controlplane]
.242 18:monitored_services = []

$ sudo ss -lunp | grep -c '224.0.0.224:7446' || true
.241 multicast_socket=0
.242 multicast_socket=0

$ sudo journalctl -u sentinel-daemon --since '<node restart timestamp>' --no-pager | grep -c 'Unable to connect to any locator of scouted peer' || true
.241 zenoh_warnings=0
.242 zenoh_warnings=0

$ sudo journalctl _COMM=systemctl --since '<node restart timestamp>' --no-pager | grep -Ec 'Failed to (restart|reset failed) sentinel-(judge|projection)' || true
.241 restart_failures=0
.242 restart_failures=0
```

The daemon operator health endpoint also reported `ecs_tick_loop.running=true`,
`service_health.running=true`, both worker restart counts `0`,
`projection_drift_detected=false`, and `last_repair_error=null` on both nodes.

## Thirty-minute soak

Window: `2026-07-19 07:44:51 UTC` through `2026-07-19 08:14:51 UTC`.

The same command was run on `.241` and `.242`, changing only the SSH target:

```console
$ ssh ubuntu@10.0.0.<node> 'bash -s' <<'REMOTE'
start='2026-07-19 07:44:51 UTC'; end='2026-07-19 08:14:51 UTC'
sha256sum /opt/sentinel/bin/sentinel-daemon
systemctl show sentinel-daemon sentinel-gaia-loop sentinel-dashboard-backend -p Id -p ActiveState -p NRestarts
printf 'zenoh_warnings='; sudo journalctl -u sentinel-daemon --since "$start" --until "$end" --no-pager | grep -c 'Unable to connect to any locator of scouted peer' || true
printf 'restart_failures='; sudo journalctl _COMM=systemctl --since "$start" --until "$end" --no-pager | grep -Ec 'Failed to (restart|reset failed) sentinel-(judge|projection)' || true
printf 'all_service_warning_error_entries='; sudo journalctl -u sentinel-daemon -u sentinel-gaia-loop -u sentinel-dashboard-backend --since "$start" --until "$end" -p warning --no-pager | grep -vc '^-- No entries --$' || true
printf 'panic_fatal_entries='; sudo journalctl -u sentinel-daemon -u sentinel-gaia-loop -u sentinel-dashboard-backend --since "$start" --until "$end" --no-pager | grep -Eic 'panic|fatal' || true
printf 'zenoh_multicast_sockets='; sudo ss -lunp | grep -c '224.0.0.224:7446' || true
printf 'claude_processes='; pgrep -xc claude || true
sudo ss -ltnp | grep sentinel-daemon | grep -vE ':8084|:9090'
curl -fsS localhost:8084/operator/runtime-health | python3 -c 'import json,sys; d=json.load(sys.stdin); print("projection_drift_detected="+str(d["projection_drift_detected"]).lower()); print("last_repair_error="+str(d["last_repair_error"])); print("ecs_tick_loop="+str(d["worker_states"]["ecs_tick_loop"]["running"]).lower()+" restarts="+str(d["worker_states"]["ecs_tick_loop"]["restart_count"])); print("service_health="+str(d["worker_states"]["service_health"]["running"]).lower()+" restarts="+str(d["worker_states"]["service_health"]["restart_count"]))'
REMOTE

.241 output:
69236a3ab62699d7af303eae25e194d78bb06d33cc94b3c6c042a351c703b54d  /opt/sentinel/bin/sentinel-daemon
NRestarts=0
Id=sentinel-daemon.service
ActiveState=active
NRestarts=0
Id=sentinel-gaia-loop.service
ActiveState=active
NRestarts=0
Id=sentinel-dashboard-backend.service
ActiveState=active
zenoh_warnings=0
restart_failures=0
all_service_warning_error_entries=0
panic_fatal_entries=0
zenoh_multicast_sockets=0
claude_processes=0
LISTEN 0 1024 127.0.0.1:46729 0.0.0.0:* users:(("sentinel-daemon",pid=177026,fd=37))
projection_drift_detected=false
last_repair_error=None
ecs_tick_loop=true restarts=0
service_health=true restarts=0

.242 output:
69236a3ab62699d7af303eae25e194d78bb06d33cc94b3c6c042a351c703b54d  /opt/sentinel/bin/sentinel-daemon
NRestarts=0
Id=sentinel-daemon.service
ActiveState=active
NRestarts=0
Id=sentinel-gaia-loop.service
ActiveState=active
NRestarts=0
Id=sentinel-dashboard-backend.service
ActiveState=active
zenoh_warnings=0
restart_failures=0
all_service_warning_error_entries=0
panic_fatal_entries=0
zenoh_multicast_sockets=0
claude_processes=0
LISTEN 0 1024 127.0.0.1:34759 0.0.0.0:* users:(("sentinel-daemon",pid=372377,fd=34))
projection_drift_detected=false
last_repair_error=None
ecs_tick_loop=true restarts=0
service_health=true restarts=0
```

Result: the original eight-second Zenoh warning loop and `.242` one-minute failed
service-restart loop were both absent for the full soak. The wider service warning
scan was also empty. No runtime write or service action was performed on `.240`.

## Cross-node membership correction

The first noise correction disabled multicast on a loopback-only Zenoh session but
left cluster membership on that session. That silenced the warnings by disconnecting
the two membership views. The final correction moves liveness heartbeats to the
existing explicit, cert-pinned QUIC control peers and keeps Zenoh daemon-local.

The new `MembershipHeartbeat` control request is observational rather than effectful:
it bypasses the control RPC idempotency response cache, so every arrival refreshes
receiver-local monotonic time and does not grow the cache. The daemon validates the
cluster id, rejects a heartbeat claiming its own node id, preserves boot/incarnation
ABA handling, sends to peers concurrently with a 750 ms per-peer timeout, and retains
the existing liveness-only `Alive -> Suspect -> Dead` semantics. The unused
`seed_endpoint` Zenoh setting was removed; provisioned nodes require out-of-band QUIC
certificate/pin installation and do not fall back to LAN discovery.

### Remote build verification on `.155`

```console
$ cargo remote -c -- test -p sentinel-cluster-control -p sentinel-daemon
stable-x86_64-unknown-linux-gnu unchanged - rustc 1.97.1 (8bab26f4f 2026-07-14)
sentinel-cluster-control: 21 unit tests passed; 2 block-pull tests passed; 4 control-roundtrip tests passed
sentinel-daemon: 311 unit tests passed; 3 replay tests passed; 1 integration test passed
all test results: ok; 0 failed

$ cargo remote -c -- clippy --workspace --all-targets -- -D warnings
stable-x86_64-unknown-linux-gnu unchanged - rustc 1.97.1 (8bab26f4f 2026-07-14)
Finished `dev` profile [unoptimized + debuginfo] target(s) in 33.21s

$ cargo remote -c -- build -p sentinel-daemon --release
stable-x86_64-unknown-linux-gnu unchanged - rustc 1.97.1 (8bab26f4f 2026-07-14)
Finished `release` profile [optimized] target(s) in 2m 36s

$ sha256sum target/release/sentinel-daemon
3714008fe4a39bf59eb7c3ef2f131e626d7aa26635b864461f48e2ffaefd4e10  target/release/sentinel-daemon

$ cargo fmt --all --check && git diff --check && bash scripts/check-determinism-profile.sh
PASS: rust-toolchain.toml pins channel 1.97.1
PASS: effective rustc 1.97.1 matches the pin
PASS: no fast-math / FMA / target-feature override in RUSTFLAGS or cargo config
DEV-010 determinism profile: OK
```

### Deploy and explicit peer readback

Both old daemons were stopped before either replacement was started, avoiding a mixed
wire-protocol interval. The same root-owned mode-0755 artifact was installed on each
node. The active configs have the same cluster id and point at each other's QUIC
control listener with different pinned certificate fingerprints:

```console
$ sha256sum /opt/sentinel/bin/sentinel-daemon
.241 3714008fe4a39bf59eb7c3ef2f131e626d7aa26635b864461f48e2ffaefd4e10
.242 3714008fe4a39bf59eb7c3ef2f131e626d7aa26635b864461f48e2ffaefd4e10

$ grep -nE '^\[daemon\.cluster\]|^control_bind|^node_id|^cluster_id|^\[\[daemon\.cluster\.control_peers\]\]|^alias|^addr|^cert_fingerprint' /opt/sentinel/config/daemon.toml
.241 control_bind = "0.0.0.0:8085"
.241 node_id = "5016f6e4-3e5c-483b-ae5f-24feeaf39b02"
.241 cluster_id = "039e153d-6551-4cf1-9e12-6a1ed9769175"
.241 peer addr = "10.0.0.242:8085"
.241 peer cert_fingerprint = "72d2f8b3baefb425f3ec50c34419aeef8767f9db7853be9b969dcdb1776130b6"
.242 control_bind = "0.0.0.0:8085"
.242 node_id = "6435ca03-1306-461b-a667-2f21711a176b"
.242 cluster_id = "039e153d-6551-4cf1-9e12-6a1ed9769175"
.242 peer addr = "10.0.0.241:8085"
.242 peer cert_fingerprint = "4026ff7a2f0ab2c6a4bd8b30b286e36ac23f9d918a29cee982ade65c056311b8"
```

### Live AC: Alive, partition, Dead, heal

Initial cross-node discovery used only the configured QUIC peer addresses:

```console
$ journalctl -u sentinel-daemon --since '2026-07-19 09:06:40 UTC' --no-pager | grep 'membership peer became Alive over QUIC'
.241 09:06:42 node_id=6435ca03-1306-461b-a667-2f21711a176b alias=bare-node-1 previous=None outcome=Joined state=Alive
.242 09:06:41 node_id=5016f6e4-3e5c-483b-ae5f-24feeaf39b02 alias=test-node-0 previous=None outcome=Joined state=Alive
```

At `09:07:28 UTC`, `.242` received two temporary, uniquely commented iptables rules
dropping UDP/8085 to and from `.241`. A 45-second transient systemd cleanup timer was
armed before waiting; the rules were then removed explicitly after both TTL stages.

```console
$ sudo iptables -I INPUT 1 -p udp -s 10.0.0.241 --dport 8085 -m comment --comment issue-442-membership-ac -j DROP
$ sudo iptables -I OUTPUT 1 -p udp -d 10.0.0.241 --dport 8085 -m comment --comment issue-442-membership-ac -j DROP
PARTITION_START=2026-07-19T09:07:28Z

$ journalctl -u sentinel-daemon --since '2026-07-19 09:07:28 UTC' --no-pager | grep 'membership peer state changed'
.241 09:07:31 previous=Alive current=Suspect peer=.242
.241 09:07:37 previous=Suspect current=Dead peer=.242
.242 09:07:32 previous=Alive current=Suspect peer=.241
.242 09:07:38 previous=Suspect current=Dead peer=.241

$ sudo iptables -D INPUT  -p udp -s 10.0.0.241 --dport 8085 -m comment --comment issue-442-membership-ac -j DROP
$ sudo iptables -D OUTPUT -p udp -d 10.0.0.241 --dport 8085 -m comment --comment issue-442-membership-ac -j DROP
HEAL_START=2026-07-19T09:08:12Z
INPUT_RULE_ABSENT
OUTPUT_RULE_ABSENT

$ journalctl -u sentinel-daemon --since '2026-07-19 09:08:12 UTC' --no-pager | grep 'membership peer became Alive over QUIC'
.241 09:08:13 peer=.242 previous=Some(Dead) outcome=Updated state=Alive
.242 09:08:13 peer=.241 previous=Some(Dead) outcome=Updated state=Alive

$ sudo iptables -S INPUT | grep -c issue-442-membership-ac; sudo iptables -S OUTPUT | grep -c issue-442-membership-ac
firewall_input_rule=0
firewall_output_rule=0
$ systemctl list-units --all 'issue442-membership-cleanup.*' --no-legend | wc -l
cleanup_units=0
```

### Five-minute post-heal soak

Window: `2026-07-19 09:08:30 UTC` through `09:13:30 UTC`.

```console
$ systemctl show sentinel-daemon sentinel-gaia-loop sentinel-dashboard-backend -p Id -p ActiveState -p SubState -p NRestarts
.241 daemon=active/running/0 gaia=active/running/0 dashboard=active/running/0
.242 daemon=active/running/0 gaia=active/running/0 dashboard=active/running/0

$ START='2026-07-19 09:08:30 UTC'; END='2026-07-19 09:13:30 UTC'
$ printf 'post_heal_suspect_or_dead='; sudo journalctl -u sentinel-daemon --since '2026-07-19 09:08:14 UTC' --no-pager -o cat | grep -Ec 'current=(Suspect|Dead)' || true
$ printf 'zenoh_locator_warnings='; sudo journalctl -u sentinel-daemon --since "$START" --until "$END" --no-pager -o cat | grep -c 'Unable to connect to any locator of scouted peer' || true
$ printf 'membership_warning_error_entries='; sudo journalctl -u sentinel-daemon --since "$START" --until "$END" --no-pager -o cat | grep -Ei 'membership.*(warn|error)|warn.*membership|error.*membership' | wc -l
.241 post_heal_suspect_or_dead=0 zenoh_locator_warnings=0 membership_warning_error_entries=0
.242 post_heal_suspect_or_dead=0 zenoh_locator_warnings=0 membership_warning_error_entries=0

$ journalctl -u sentinel-daemon -u sentinel-gaia-loop -u sentinel-dashboard-backend --since "$START" --until "$END" -p warning --no-pager
.241 all_warning_error_entries=0 panic_fatal_entries=0
.242 all_warning_error_entries=0 panic_fatal_entries=0

$ ss -uapn | grep -Ec '224\.0\.0\.224:7446|:7446'; ss -lunp | grep -c ':8085 '
.241 zenoh_multicast_sockets=0 quic_8085_listeners=1
.242 zenoh_multicast_sockets=0 quic_8085_listeners=1

$ ps -C sentinel-daemon -o pid=,etimes=,%cpu=,rss=,vsz=,nlwp=,comm=
.241 179228 433 0.3 58132 1093148 12 sentinel-daemon
.242 374609 433 0.3 289428 884344 9 sentinel-daemon

$ curl -fsS localhost:8084/operator/runtime-health | python3 -c 'import json,sys; d=json.load(sys.stdin); print("projection_drift_detected="+str(d["projection_drift_detected"]).lower()); print("last_repair_error="+str(d["last_repair_error"])); print("ecs_tick_loop="+str(d["worker_states"]["ecs_tick_loop"]["running"]).lower()+" restarts="+str(d["worker_states"]["ecs_tick_loop"]["restart_count"])); print("service_health="+str(d["worker_states"]["service_health"]["running"]).lower()+" restarts="+str(d["worker_states"]["service_health"]["restart_count"]))'
.241 projection_drift_detected=false last_repair_error=None ecs_tick_loop=true/restarts=0 service_health=true/restarts=0
.242 projection_drift_detected=false last_repair_error=None ecs_tick_loop=true/restarts=0 service_health=true/restarts=0
```

The immediate process-start window contained the lab's pre-existing bwrap/NATS
configuration warnings and one simultaneous-start QUIC handshake abort; this evidence
does not relabel that window as warning-free. None recurred in the post-heal soak, and
the required Zenoh warning and multicast-socket counts stayed zero throughout. No
Claude request was made, token cost was USD 0, and `.240` was not contacted.
