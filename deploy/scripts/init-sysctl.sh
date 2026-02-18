#!/usr/bin/env bash
# init-sysctl.sh — Apply kernel parameters for Sentinel VM
# Usage: sudo bash init-sysctl.sh
set -euo pipefail

echo "[init-sysctl] Applying Sentinel kernel parameters..."

# Minimize swapping (but keep swap available for emergency)
sysctl -w vm.swappiness=1

# THP: madvise mode (NOT always — anti-pattern for memory-mapped B-Trees like redb)
echo madvise > /sys/kernel/mm/transparent_hugepage/enabled

# Increase dirty page limits for write batching
sysctl -w vm.dirty_ratio=20
sysctl -w vm.dirty_background_ratio=5

# Increase max memory map areas (redb + Zenoh SHM)
sysctl -w vm.max_map_count=262144

# Network tuning for Zenoh
sysctl -w net.core.rmem_max=16777216
sysctl -w net.core.wmem_max=16777216

echo "[init-sysctl] Done. Kernel parameters applied."
