#!/bin/bash
# Idempotent network isolation setup for sentinel agents.
#
# Run as root on the deployment VM (10.0.0.240):
#   sudo bash scripts/vm-setup-network-isolation.sh
#
# This creates the br-sentinel bridge and ensures nftables/ip tools
# are available so that sentinel can create per-agent network namespaces.
set -euo pipefail

BRIDGE_NAME="br-sentinel"
BRIDGE_IP="10.42.0.1"
BRIDGE_PREFIX="16"

echo "=== Sentinel Network Isolation Setup ==="

# 1. Verify required tools
for tool in ip nft nsenter; do
    if ! command -v "$tool" &>/dev/null; then
        echo "ERROR: '$tool' not found. Install: apt install iproute2 nftables util-linux"
        exit 1
    fi
done
echo "Required tools (ip, nft, nsenter): OK"

# 2. Create bridge (idempotent)
if ip link show "$BRIDGE_NAME" &>/dev/null; then
    echo "Bridge $BRIDGE_NAME: already exists"
else
    ip link add "$BRIDGE_NAME" type bridge
    ip addr add "${BRIDGE_IP}/${BRIDGE_PREFIX}" dev "$BRIDGE_NAME"
    ip link set "$BRIDGE_NAME" up
    echo "Bridge $BRIDGE_NAME: created (${BRIDGE_IP}/${BRIDGE_PREFIX})"
fi

# 3. Enable IP forwarding (needed for bridge traffic)
if [ "$(cat /proc/sys/net/ipv4/ip_forward)" -eq 1 ]; then
    echo "IP forwarding: already enabled"
else
    sysctl -w net.ipv4.ip_forward=1
    echo "net.ipv4.ip_forward = 1" >> /etc/sysctl.d/99-sentinel.conf
    echo "IP forwarding: enabled"
fi

# 4. Verify CAP_NET_ADMIN (dummy link probe)
PROBE_NAME="sentinel-probe"
if ip link add "$PROBE_NAME" type dummy 2>/dev/null; then
    ip link del "$PROBE_NAME"
    echo "CAP_NET_ADMIN: OK"
else
    echo "ERROR: CAP_NET_ADMIN not available. Run as root."
    exit 1
fi

# 5. Verification
echo ""
echo "=== Verification ==="
echo "Bridge:"
ip addr show "$BRIDGE_NAME" | head -3
echo ""
echo "nft version: $(nft --version)"
echo ""
echo "Network isolation setup: OK"
echo ""
echo "NOTE: Per-agent veth pairs and nftables rules are created dynamically"
echo "      by sentinel-sandbox at agent spawn time."
