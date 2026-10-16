# Isolated CI Lab

The golden image is an immutable, sanitized Ubuntu 24.04 OS/kernel/runtime
underlay. Install source-bound qualified product artifacts **after cloning**;
neither a source change nor a new candidate requires rebuilding the base.
Never copy production identity, databases, company history, provider credentials,
SSH private keys, or Git-hosting credentials into the template or guests.

`prepare-guest.sh` is restricted to a fresh guest carrying the lab owner marker.
It installs the measured runtime prerequisite versions and common repository
helpers, leaving product services disabled and product state empty. The pinned
older prerequisite versions deliberately match the acceptance reference; updating
them is a separate parity decision, not an unattended upgrade.

`verify-guest.py` checks the exact kernel, Bubblewrap binary, prerequisite
packages, real user namespaces and Bubblewrap isolation, cgroup v2/controllers,
AppArmor, FUSE/BTF, hot-tier tmpfs, and unit hardening. Its fingerprint excludes
machine identity. Identity hashes are reported separately to prove clone
isolation. Successful underlay probes do not prove product or customer acceptance.

## Site Binding

Keep the site JSON, jump host, public-key enrollment, storage topology, pool
registration and network setup private. The existing project controller owns
deployment credentials; test archives receive none. Run `control.py` **on the
registered Proxmox lab host** with explicit private config and durable state:

```sh
python3 control.py --config /work/lab/site.json --state /work/lab/state.json telemetry
python3 control.py --config /work/lab/site.json --state /work/lab/state.json lease --vmid 8601 --holder qualification-1 --seconds 1800
python3 control.py --config /work/lab/site.json --state /work/lab/state.json start --vmid 8601 --holder qualification-1 --memory-mib 2048
python3 control.py --config /work/lab/site.json --state /work/lab/state.json resize --vmid 8601 --holder qualification-1 --memory-mib 3072
```

The config requires `owner=sentinel-ci-v1`, `runtime_target_class=LAB`, `node`,
`pool`, `storage`, `bridge`, private IPv4 `network`, `gateway`, disjoint `template`
and `slots`, an `addresses` map keyed by VM ID, `reserve_mib>=6144`, and
`max_active<=3`. `managed_growth` binds named `builder` and `agent` guest IDs
to provisional allowances of 1024 and 512 MiB respectively. `idle_seconds`
defaults to 900. Private manifests contain the complete runtime-target declaration.
Starts also require a private `network_check` argument vector that verifies the
complete live owned firewall graph without modifying it. Saved configuration
and partial table presence are never equivalent to live isolation.

Each effect checks VM ID, node, pool membership, owner marker, bridge, storage
and a bounded matching lease. Start checks configured address reservations and
three ARP duplicate-address probes immediately before boot. Failed ping is not
an IP allocation check. An owned stopped reservation is recognized only at its
exact node/VM config path with matching structured PVE owner and IP values;
PVE's encoded description comments are not parsed as configuration keys.
Pool and network provisioning also require a fresh
cluster-wide configuration/interface/route inventory before initial allocation.
Provisioning, decommission, lifecycle and maintenance share one durable state
lock. Decommission fences admission before destruction and retains that fence
across a partial failure. A new lease generation cannot take over a running
expired/released job; stop it first. A running-start retry succeeds only for
the same recorded lease generation and VM identity.

The bounded lifecycle transition table below uses `starting` for a retained,
possibly disconnected start effect, even while PVE still reports stopped.
An in-flight task or VM lock blocks transfer, release and duplicate boot.
Completed tasks plus unlocked, repeated stopped readback allow recovery; expiry
alone never clears its target promise. Fresh owned status overrides stale
cluster hints in both directions, including just-started guests and guard stops.

| Guest Phase | Lease | Start | Release | New Holder |
|---|---|---|---|---|
| stopped | active | fresh admission | allowed | refused |
| stopped | expired | refused | refused | fresh generation |
| stopped | invalidated | refused | cleanup only | after cleanup |
| starting | active | task-fenced retry | task-fenced | refused |
| starting | expired | refused | refused | quiescence proof first |
| starting | invalidated | refused | quiescence proof first | after cleanup |
| running | active | same-generation reconnect | refused | refused |
| running | expired | refused | refused | stop first |
| running | invalidated | refused | refused | stop and cleanup first |

An unsafe guard sample stops actual running owned guests regardless of lease
state. It never discards an uncertain stopped start promise. Missing process
telemetry on a running owned guest remains a bounded capacity wait, not zero
residency or a successful run. Invalidated job identities never resume or PASS.

Starts and balloon increases reserve their request, the named 1536-MiB growth
allowance, and **owned** outstanding current/pending targets minus measured
residency. Accepted stopped starting intents also remain promises after a crash.
Pending balloon requests remain committed until actual convergence; residency
is not counted twice. All foreign potential headroom is reported as external
risk, not subtracted as an unbounded blanket reservation. This policy does not
guarantee capacity against uncontrolled foreign bursts. Lab automatic balloon
growth is disabled; requests use the 2-8 GiB envelope.
Admission's external-risk estimate uses the fresh cluster maximum/residency
view, explicitly not current balloon targets. Detailed foreign status queries
remain in read-only telemetry, outside the startup critical path. Maintenance
skips already-stopped idle slots without clearing their retained starting intents.

The existing read-only `telemetry` command returns schema 2: Unix-second
`timestamp`, `host_mem_bytes` (including cache and availability), raw cumulative
`host_cpu_stat`/`host_diskstats`, `host_psi` for CPU/memory/I/O, and `guests` for
concurrent load. Each QEMU entry includes PVE byte fields `balloon` (actual),
`balloon_min` (configured target), `maxmem`, `memhost`, CPU and I/O counters, plus
`process` with actual `/proc` `rss_bytes`, `pss_bytes`, `swap_bytes`, `pid` and
`start_ticks`. Owned entries include `lease`, generation-bound `run` and
`pending_target_mib`. Unknown process measurements are null with an explicit
error, never fabricated zeros. The private site interface publishes the exact
SSH command. It does not allocate a measurement lease.

Maintenance samples actual managed QEMU RSS/PSS with PID-reuse protection.
Admission requires a preceding sample at most 45 seconds old and stable process
identity. An active lab window retains its initial managed-RSS baseline;
cumulative builder growth above 1024 MiB or agent growth above 512 MiB invalidates
it. Memory PSI avg10 `some>=1.0` or `full>=0.1`, unavailable telemetry and host
availability below reserve+256 MiB also trip the guard. These are provisional
safety thresholds, not empirically proven maximum growth or performance SLAs.

Run `guard` and `reap` from a host-local timer, without an agent session.
An unsafe sample gracefully stops only owned labs, durably invalidating
their leases, including active work; interrupted tests cannot count as PASS.
Released/expired idle slots shut down after the idle interval. `release` ends
only the stopped caller's lease. Renewal uses the same holder and bounded lease API.
`defer-start` records a bounded, deduplicated request rather than bypassing
capacity. Host-local maintenance automatically retries only capacity/transport
waits with fresh admission. Semantic failures stay explicitly blocked, not PASS.
Raw-log expiry after seven days is suspended while any active lease, pending
request, running intent or preserved investigation snapshot exists. Explicit
`.keep` files and symlinks remain protected even when the lab is idle.

## Snapshots

`snapshot --name ci-...` stops the guest gracefully before a disk-only snapshot,
providing quiesced disks without merging or snapshotting external effects.
`rollback` requires a tracked ready snapshot and leaves the guest stopped.
`discard-snapshot` refuses untracked or preserved snapshots. `reap` expires only
tracked, unpreserved snapshots older than 24 hours when no active lease exists.
Set `--preserve` for active investigations or a working rollback. A snapshot is
not an independent backup and never reverses provider effects.
Creating/deleting intents reconcile the external operation owner, snapshot
presence, VM lock and active task after reconnect. A completed effect is not
issued again, and a missing or foreign snapshot cannot satisfy restoration.

## Verification

```sh
python3 -m unittest discover -s deploy/ci-lab -p 'test_*.py' -v
bash -n deploy/ci-lab/prepare-guest.sh
```

These focused tests do not allocate VMs. Live two-guest identity, isolation,
balloon, restart/idle and snapshot restoration evidence is retained privately;
the aggregate candidate still needs independent exact-head review. Lab
measurements never substitute for final single-node product acceptance.
