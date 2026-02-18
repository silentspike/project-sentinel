#!/usr/bin/env bash
# init-tmpfs.sh — Mount tmpfs for Sentinel hot-tier storage
# Usage: sudo bash init-tmpfs.sh
set -euo pipefail

TMPFS_SIZE="${SENTINEL_TMPFS_SIZE:-4G}"
SENTINEL_USER="${SENTINEL_USER:-ubuntu}"

echo "[init-tmpfs] Mounting tmpfs at /ram/sentinel (size=${TMPFS_SIZE})..."

if mountpoint -q /ram/sentinel; then
    echo "[init-tmpfs] Already mounted, skipping."
else
    mkdir -p /ram/sentinel
    mount -t tmpfs -o size="${TMPFS_SIZE}",mode=0755,uid="$(id -u "${SENTINEL_USER}")",gid="$(id -g "${SENTINEL_USER}")" tmpfs /ram/sentinel
fi

# Ensure subdirectories exist after mount
mkdir -p /ram/sentinel/{ecs,sessions,zenoh,bench}
chown -R "${SENTINEL_USER}:${SENTINEL_USER}" /ram/sentinel

echo "[init-tmpfs] Done. tmpfs mounted at /ram/sentinel (${TMPFS_SIZE})."
