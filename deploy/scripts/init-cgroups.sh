#!/usr/bin/env bash
# init-cgroups.sh — Create cgroups v2 hierarchy for Sentinel agents
# Usage: sudo bash init-cgroups.sh
set -euo pipefail

CGROUP_ROOT="/sys/fs/cgroup"
SENTINEL_CG="${CGROUP_ROOT}/sentinel"

echo "[init-cgroups] Creating Sentinel cgroup hierarchy..."

# Create sentinel parent cgroup
mkdir -p "${SENTINEL_CG}"

# Enable controllers in parent
echo "+cpu +memory +io +pids" > "${CGROUP_ROOT}/cgroup.subtree_control" 2>/dev/null || true
echo "+cpu +memory +io +pids" > "${SENTINEL_CG}/cgroup.subtree_control" 2>/dev/null || true

# Create agent and nightrun sub-cgroups
mkdir -p "${SENTINEL_CG}/agents"
mkdir -p "${SENTINEL_CG}/nightrun"

echo "+cpu +memory +io +pids" > "${SENTINEL_CG}/agents/cgroup.subtree_control" 2>/dev/null || true

echo "[init-cgroups] Done. Hierarchy: ${SENTINEL_CG}/{agents,nightrun}"
