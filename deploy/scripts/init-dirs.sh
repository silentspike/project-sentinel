#!/usr/bin/env bash
# init-dirs.sh — Create Sentinel directory structure with correct permissions
# Usage: sudo bash init-dirs.sh
set -euo pipefail

SENTINEL_USER="${SENTINEL_USER:-ubuntu}"

echo "[init-dirs] Creating Sentinel directory structure..."

# Main directories
mkdir -p /opt/sentinel/{bin,config,data,logs}

# RAM-backed directories (created here, mounted by init-tmpfs.sh)
mkdir -p /ram/sentinel/{ecs,sessions,zenoh,bench}

# Set ownership
chown -R "${SENTINEL_USER}:${SENTINEL_USER}" /opt/sentinel
chown -R "${SENTINEL_USER}:${SENTINEL_USER}" /ram/sentinel

echo "[init-dirs] Done. Directories created for user ${SENTINEL_USER}."
