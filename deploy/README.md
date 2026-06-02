# Deploy

Deployment-Artefakte fuer Project Sentinel auf der Ziel-VM.

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
