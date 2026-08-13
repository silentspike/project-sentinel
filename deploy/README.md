# Deploy

Deployment-Artefakte fuer Project Sentinel auf der Ziel-VM.

## Single-node M0 release provisioning

`provision-m0-single-node.sh` installs one complete, stopped M0 release from a
schema-v1 release manifest. The manifest is the sole artifact inventory: the
provisioner requires the exact canonical set, including all 60 agent configs,
the health-monitor unit/timer/script, the web project and authoring profiles,
the product-acceptance contract, and the separately packaged `nats-server`
binary. Missing, extra, duplicate, source-swapped, type-swapped, or hash-mismatched
entries fail before target files are changed.

The operator supplies both the approved Git SHA and the SHA-256 of the raw
manifest. Sources must be regular, single-link, owner-controlled files below
the source root. All required Sentinel units must be stopped. Production
staging is restricted to `/work/tmp/project-sentinel/`; `/tmp` is never used.

```bash
sudo bash deploy/provision-m0-single-node.sh \
  --manifest /work/tmp/project-sentinel/release/release-manifest.json \
  --expected-manifest-sha256 "${APPROVED_MANIFEST_SHA256}" \
  --expected-git-sha "${APPROVED_GIT_SHA}" \
  --source-root /work/tmp/project-sentinel/release \
  --stage-root /work/tmp/project-sentinel/m0-provision
```

Installed ownership is `root:root`; binaries and scripts use mode `0755`, while
configs and systemd units use `0644`. Existing files are captured in the
owner-only staging area before replacement. A partial installation rolls back
the changed files and emits `provision-receipt.json` with a bounded, public-safe
status. The provisioner never copies secrets, identity, databases, or runtime
state, and it never starts, enables, reloads, or restarts a service. Service
activation and runtime acceptance are separate, explicitly authorized steps.

## Ziel-VM

- Host: `ubuntu@<deploy-vm>` (Proxmox VM)
- OS: Ubuntu, KVM/q35, CPU host-passthrough
- Tuning: isolcpus, tmpfs, cgroups v2, io_uring

## Verzeichnisstruktur

| Verzeichnis/Datei | Zweck |
|-------------------|-------|
| `systemd/` | systemd Service-Units (sentinel-ecs, cortex, console backend) |
| `vm-config/` | Host/Guest-Tuning Scripts |
| `bench/` | Benchmark-Harness + Runner-Scripts |
| `proxmox-vm.conf` | Proxmox VM-Konfiguration (CPU, RAM, Storage) |
| `kernel-params.conf` | Guest Kernel-Parameter (mitigations, cstate, THP) |
| `cgroups-setup.sh` | cgroups v2 Limits fuer Agent-Prozesse |
| `tmpfs-setup.sh` | tmpfs `/ram/sentinel` Mount (4G, huge=within_size) |

## VM-Tuning Scripts

```bash
# Host-Profil anwenden (isolcpus, irqaffinity, ZFS ARC, KSM)
deploy/vm-config/apply-host-profile.sh

# Guest I/O-Profil (io_uring, Scheduler)
deploy/vm-config/apply-vm-io-profile.sh

# Host-Profil verifizieren
deploy/vm-config/verify-host-profile.sh

# Boot-Guard installieren (Kernel-Params persistent)
deploy/vm-config/install-boot-guard.sh
```

## Benchmarks

```bash
# Stack-Suite auf VM ausfuehren (ECS, redb, Limbo, Zenoh, bwrap, wasmtime)
ssh ubuntu@<deploy-vm> 'bash -s' < deploy/bench/run-stack-suite-guest.sh

# P0-Gate-Tests (Zenoh SHM, Persist, Circuit Breaker)
ssh ubuntu@<deploy-vm> 'bash -s' < deploy/bench/run-p0-gates-guest.sh
```

## Guest-Setup

```bash
# tmpfs einrichten
sudo bash deploy/tmpfs-setup.sh

# cgroups konfigurieren
sudo bash deploy/cgroups-setup.sh

# Kernel-Parameter anwenden
sudo cp deploy/kernel-params.conf /etc/sysctl.d/99-sentinel.conf
sudo sysctl --system
```
