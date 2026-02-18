#!/usr/bin/env bash
# init-hugepages.sh — Configure hugepages for Sentinel (optional, default 0)
# Usage: sudo bash init-hugepages.sh
# Env: SENTINEL_HUGEPAGES=0 (default, disabled) or SENTINEL_HUGEPAGES=1024 (2GB of 2MB pages)
set -euo pipefail

HUGEPAGES="${SENTINEL_HUGEPAGES:-0}"

echo "[init-hugepages] Configuring hugepages: ${HUGEPAGES} pages..."

if [ "${HUGEPAGES}" -eq 0 ]; then
    echo "[init-hugepages] Hugepages disabled (SENTINEL_HUGEPAGES=0). Skipping."
    exit 0
fi

# Set number of 2MB hugepages
echo "${HUGEPAGES}" > /proc/sys/vm/nr_hugepages
ACTUAL=$(cat /proc/sys/vm/nr_hugepages)

if [ "${ACTUAL}" -ne "${HUGEPAGES}" ]; then
    echo "[init-hugepages] WARNING: Requested ${HUGEPAGES} but got ${ACTUAL} (insufficient memory?)."
    exit 1
fi

echo "[init-hugepages] Done. ${ACTUAL} hugepages ($(( ACTUAL * 2 )) MB) allocated."
