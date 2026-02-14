#!/usr/bin/env bash
set -euo pipefail

# Guest-side performance baseline for Sentinel workloads.
# Keeps settings conservative but latency-friendly.

if [[ "$(id -u)" -ne 0 ]]; then
  echo "error: run as root" >&2
  exit 1
fi

cat > /etc/sysctl.d/90-sentinel-perf.conf <<'EOF'
vm.swappiness=1
vm.vfs_cache_pressure=50
vm.dirty_background_bytes=67108864
vm.dirty_bytes=268435456
kernel.sched_autogroup_enabled=0
kernel.numa_balancing=0
fs.aio-max-nr=1048576
vm.max_map_count=1048576
EOF

sysctl -p /etc/sysctl.d/90-sentinel-perf.conf >/dev/null

if systemctl list-unit-files | grep -q '^irqbalance\.service'; then
  systemctl disable --now irqbalance >/dev/null 2>&1 || true
fi

echo "guest_perf_applied=yes"
sysctl vm.swappiness vm.vfs_cache_pressure vm.dirty_background_bytes vm.dirty_bytes kernel.sched_autogroup_enabled kernel.numa_balancing fs.aio-max-nr vm.max_map_count
