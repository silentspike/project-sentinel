#!/usr/bin/env bash
set -euo pipefail

# Guest-side helper to create reusable cgroup-v2 limits for Sentinel services.
# Safe to run multiple times.

if [[ "$(id -u)" -ne 0 ]]; then
  echo "error: run as root" >&2
  exit 1
fi

install -d -m 0755 /etc/systemd/system/sentinel-agent@.service.d

ROOT_SRC="$(findmnt -n -o SOURCE / || true)"
ROOT_DEV="/dev/$(lsblk -n -o PKNAME "${ROOT_SRC}" 2>/dev/null || true)"
if [[ -z "${ROOT_DEV}" || "${ROOT_DEV}" == "/dev/" ]]; then
  ROOT_DEV="/dev/sda"
fi

cat > /etc/systemd/system/sentinel-agent@.service.d/limits.conf <<EOF
[Service]
CPUQuota=100%
MemoryMax=256M
IOReadIOPSMax=${ROOT_DEV} 300
IOWriteIOPSMax=${ROOT_DEV} 300
IOReadBandwidthMax=${ROOT_DEV} 10M
IOWriteBandwidthMax=${ROOT_DEV} 10M
EOF

systemctl daemon-reload
echo "cgroups_limits_installed=yes"
