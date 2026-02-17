#!/bin/bash
# Idempotent IO controller delegation for sentinel cgroups v2.
#
# Run as root on the deployment VM (10.0.0.240):
#   sudo bash scripts/vm-setup-io-delegation.sh
#
# This enables the cgroup v2 IO controller so that sentinel can enforce
# per-agent IO limits (300 IOPS, 10 MB/s) via io.max.
set -euo pipefail

CGROUP_ROOT="/sys/fs/cgroup"
SENTINEL_CGROUP="${CGROUP_ROOT}/sentinel"
SENTINEL_USER="ubuntu"

echo "=== Sentinel IO Controller Delegation Setup ==="

# 1. Verify cgroups v2
if [ ! -f "${CGROUP_ROOT}/cgroup.controllers" ]; then
    echo "ERROR: cgroups v2 not available (no ${CGROUP_ROOT}/cgroup.controllers)"
    exit 1
fi

echo "cgroups v2: OK"

# 2. Enable IO controller at root level
if grep -qw "io" "${CGROUP_ROOT}/cgroup.subtree_control" 2>/dev/null; then
    echo "IO controller at root: already enabled"
else
    echo "+io" > "${CGROUP_ROOT}/cgroup.subtree_control" 2>/dev/null || {
        echo "ERROR: Cannot enable IO controller at root level."
        echo "Check: Are there processes directly in the root cgroup?"
        echo "Fix:   systemctl set-property -- '-.slice' IOAccounting=yes"
        exit 1
    }
    echo "IO controller at root: enabled"
fi

# 3. Create sentinel cgroup if missing
if [ ! -d "${SENTINEL_CGROUP}" ]; then
    mkdir -p "${SENTINEL_CGROUP}"
    echo "Sentinel cgroup: created"
else
    echo "Sentinel cgroup: exists"
fi

# 4. Enable IO controller at sentinel level
if grep -qw "io" "${SENTINEL_CGROUP}/cgroup.subtree_control" 2>/dev/null; then
    echo "IO controller at sentinel: already enabled"
else
    echo "+io" > "${SENTINEL_CGROUP}/cgroup.subtree_control"
    echo "IO controller at sentinel: enabled"
fi

# 5. Set ownership so ubuntu user can manage agent cgroups
chown -R "${SENTINEL_USER}:${SENTINEL_USER}" "${SENTINEL_CGROUP}"
echo "Ownership: ${SENTINEL_USER}"

# 6. Verify
echo ""
echo "=== Verification ==="
echo "Root subtree_control:     $(cat ${CGROUP_ROOT}/cgroup.subtree_control)"
echo "Sentinel subtree_control: $(cat ${SENTINEL_CGROUP}/cgroup.subtree_control)"
echo ""
echo "IO delegation: OK"
