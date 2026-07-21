# Issue #75 Full-Cage Live Evidence

Issue: `#75`
Deployment VM: `ubuntu@10.0.0.240` (`sentinel-ubuntu-2404`)
Release binary SHA-256: `cffa9026d878bc81b848e4436a6be0b01045be09c5286ba6b5f967a5832a88c5`

All Rust compilation and tests were run through `cargo remote -c --`. Runtime
checks and performance measurements were run on the deployment VM, never on the
build server.

## Rollback Boundary

Before deployment, Proxmox VM 1069 received the issue-owned snapshot
`pre-75-20260720T230840Z`. Existing snapshots were not changed. The snapshot is
deleted only after the PR is merged, every AC is re-read on the merged artifact,
and Issue #75 is closed with `status:verified`.

## AC-1 / AC-3: One Full Cage Per Agent

The deployed daemon runs 26 agent-runtime processes. The check below uses the
sandboxed child PIDs, not the outer bwrap supervisors.

```text
daemon_pid=595603 daemon_netns=net:[4026531833] agent_count=26
unique_agent_netns=26 shared_with_daemon=0 loopback_only_agents=26
```

All 26 network namespace inode values are pairwise distinct. Every namespace
contains only `lo`; none shares the daemon namespace. The obsolete, memberless
`br-sentinel` artifact had no configuration reference or attached link and was
removed. Its `10.42.0.0/16` route disappeared with it; no veth or `vp-*` links
remain.

The 60-cage VM stress test exercises the same `BwrapConfig::for_agent` plus
`--info-fd` path concurrently:

```text
test ac_75_vm_60_concurrent_full_cages_are_distinct ... ok
test result: ok. 4 passed; 0 failed; 0 ignored
```

The four ignored VM acceptance tests cover a distinct cage, loopback-only
interfaces, deliberate shared-net classification, and 60 concurrent cages.
They completed in `0.52s` and left no issue-owned process or cgroup.

## AC-2: No External Network

```text
external_probe_pid=595637 rc=1
bash: connect: Network is unreachable
bash: /dev/tcp/1.1.1.1/443: Network is unreachable
```

The probe ran with `nsenter -t 582215 -n` inside a real agent-runtime network
namespace. No external route exists.

## AC-4: Detection, Durable Signal, And Termination

The daemon test starts the real `/usr/bin/agent-runtime` with deliberate
`share_net=true`, supplies that process's actual bwrap child PID to the
production enforcement function, and verifies process termination, sandbox
teardown, degraded health state, and one durable `AgentIsolationFailed` event.
The complementary acceptance test independently verifies the same shared-net
classification at the sandbox boundary.

```text
[landlock-wrapper] Landlock enforced
agent-runtime: started (pid=2)
test tests::netns_not_isolated_enforcement_terminates_and_records_failure ... ok
test result: ok. 1 passed; 0 failed
```

`netns_probe_error_preserves_runtime_and_emits_no_failure` separately proves that
an unreadable `/proc` probe neither terminates the runtime nor emits a false cage
breach. Every spawn, restart, reconcile, and restore call site now supplies the
EventStore, so a confirmed breach has the same durable outcome on every path.

The normal deployed path emitted no isolation failure:

```text
agent_isolation_events_since_deploy=0
```

## AC-5: Thirty-Minute Soak

Final output from `/tmp/issue75/soak-final-cffa9026`, executed against release
SHA-256 `cffa9026d878bc81b848e4436a6be0b01045be09c5286ba6b5f967a5832a88c5`:

```text
t=   1s expected=26 runtime=26 projection=26 cgroups=26 stale=0 orphans=0 zombies=0 drift=false failed=none agents=26 unique_netns=26 shared_with_daemon=0 services=healthy restarts=0
...
t=1800s expected=26 runtime=26 projection=26 cgroups=26 stale=0 orphans=0 zombies=0 drift=false failed=none agents=26 unique_netns=26 shared_with_daemon=0 services=healthy restarts=0
agent_count=26 unique_agent_netns=26 shared_with_daemon=0 loopback_only_agents=26 external_probe_rc=1 legacy_links=0 failed=none
FINAL legacy_errors=0 isolation_log_failures=0 isolation_events=0 panic_fatal=0 elapsed_s=1801
SOAK_PASS out_dir=/tmp/issue75/soak-final-cffa9026
```

The soak performed 31 runtime-health reads at one-minute intervals, polled all
five services with `NRestarts=0`, and recorded `vmstat`, `mpstat`, `iostat -x`,
and `ss` sidecars. The tracked `normal-path-soak.txt` has SHA-256
`a50628b651c8240396542a484fa7e3dc4f795cd9aa22a2391b4fb64d46ed4092`.

## AC-6: Regression Gates And Runtime Health

Remote Rust gates on Rust 1.97.1:

```text
$ cargo remote -c -- fmt --all -- --check
PASS

$ cargo remote -c -- test -p sentinel-sandbox -p sentinel-daemon -j1
sentinel-daemon: 339 passed; 0 failed; 1 ignored
sentinel-sandbox lib: 46 passed; 0 failed; 3 ignored
sentinel-sandbox acceptance: 10 passed; 0 failed; 4 ignored

$ cargo remote -c -- clippy -p sentinel-sandbox -p sentinel-daemon --lib --tests -- -D warnings
PASS

$ cargo remote -c -- build -p sentinel-daemon --release -j1
PASS
release SHA-256: cffa9026d878bc81b848e4436a6be0b01045be09c5286ba6b5f967a5832a88c5
```

Initial deployed health:

```json
{"expected_active_agents":26,"runtime_agents":26,"projection_agents":26,"live_cgroup_dirs":26,"stale_runtime_entries":0,"orphan_cgroups":0,"zombie_tracked_pids":0,"projection_drift_detected":false}
```

`sentinel-daemon`, `sentinel-gateway`, `sentinel-projection`,
`sentinel-nats-bridge`, and `nats-server` were all active with `NRestarts=0`.

## Deployment-VM Benchmark

Command:

```bash
sudo /tmp/issue75/vm-issue-75-bench.py \
  --agent-pid 582215 --spawn-samples 1000 --verify-samples 10000
```

Results on `.240` with bubblewrap 0.9.0:

| Operation | Samples | p50 | p95 | max |
|---|---:|---:|---:|---:|
| Full-cage bwrap spawn + `true` + reap | 1,000 | 9.523 ms | 20.977 ms | 53.836 ms |
| Two-read netns isolation verifier | 10,000 | 6.625 us | 15.381 us | 75.043 us |

The spawn figure includes process creation, namespace setup, the command, and
reaping. The dead bridge/veth path has no valid historical deploy-VM baseline,
so no fabricated delta is reported. The VM reported 10.58% mean steal time
during this deliberately uncorrected live-load measurement; the observed tail
is reported as-is. Sidecars are in
`/tmp/issue75/benchmark-20260721T000859Z`.

## Architecture Readback

Both language editions already describe the accepted target correctly:

- German SSOT: `/home/jan/togaf-llm-architecture-guide.html`
- English repository edition: `docs/architecture/togaf-architecture-guide.html`

Each specifies `--unshare-all`, loopback-only agent cages, no `--share-net`, and
daemon-to-gateway proxying. No TOGAF edit was needed for this closeout. Stale
public README, demo, governance, workshop, limitations, and example-policy text
was aligned with that target, and the obsolete bridge/veth/nftables VM setup
script was removed.
