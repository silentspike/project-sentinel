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
`max_active<=3`. Optional `growth_margin_mib` and `idle_seconds` default to 1024
and 900. Private manifests contain the complete runtime-target declaration.

Each effect checks VM ID, node, pool membership, owner marker, bridge, storage
and a bounded matching lease. Start checks configured address reservations and
three ARP duplicate-address probes immediately before boot. Failed ping is not
an IP allocation check. Pool and network provisioning also require a fresh
cluster-wide configuration/interface/route inventory before initial allocation.

Starts and balloon increases reserve their requested memory plus a conservative
foreign-growth margin. Lab automatic balloon growth is disabled; requests use
the 2-8 GiB envelope. `telemetry` reports actual balloon, configured minimum,
maximum, guest/host-used bytes, concurrent guests, host available memory, PSI and
load. Null telemetry is unknown, not zero. This is shared capacity, not a claim
that all foreign maximum memory commitments fit simultaneously.

Run `guard` and `reap` from a host-local timer, without an agent session.
Host reserve pressure gracefully stops only owned labs, durably invalidating
their leases, including active work; interrupted tests cannot count as PASS.
Released/expired idle slots shut down after the idle interval. `release` ends
only the caller's lease. Renewal uses the same holder and bounded lease API.

## Snapshots

`snapshot --name ci-...` stops the guest gracefully before a disk-only snapshot,
providing quiesced disks without merging or snapshotting external effects.
`rollback` requires a tracked ready snapshot and leaves the guest stopped.
`discard-snapshot` refuses untracked or preserved snapshots. `reap` expires only
tracked, unpreserved snapshots older than 24 hours when no active lease exists.
Set `--preserve` for active investigations or a working rollback. A snapshot is
not an independent backup and never reverses provider effects.

## Verification

```sh
python3 -m unittest discover -s deploy/ci-lab -p 'test_*.py' -v
bash -n deploy/ci-lab/prepare-guest.sh
```

These focused tests do not allocate VMs. Live two-guest identity, isolation,
balloon, restart/idle and snapshot restoration evidence is retained privately;
the aggregate candidate still needs independent exact-head review. Lab
measurements never substitute for final single-node product acceptance.
