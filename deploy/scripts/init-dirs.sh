#!/usr/bin/env bash
# init-dirs.sh — Create Sentinel directory structure with correct permissions
# Usage: sudo bash init-dirs.sh
set -euo pipefail

SENTINEL_USER="${SENTINEL_USER:-ubuntu}"
SENTINEL_HOME="${SENTINEL_HOME:-/home/${SENTINEL_USER}}"

echo "[init-dirs] Creating Sentinel directory structure..."

# Executables and config stay root-owned; runtime data belongs to the service user.
install -d -o root -g root -m 0755 \
  /opt/sentinel /opt/sentinel/bin /opt/sentinel/config /opt/sentinel/scripts
install -d -o "${SENTINEL_USER}" -g "${SENTINEL_USER}" -m 0750 \
  /opt/sentinel/data /opt/sentinel/logs
install -d -o root -g root -m 0700 /opt/sentinel/data/company-delivery
install -d -o "${SENTINEL_USER}" -g "${SENTINEL_USER}" -m 0700 \
	/opt/sentinel/data/gaia-console /opt/sentinel/data/gaia-console/sessions \
	/opt/sentinel/data/codex-provider "${SENTINEL_HOME}/.codex"

# RAM-backed directories (created here, mounted by init-tmpfs.sh)
mkdir -p /ram/sentinel/{ecs,sessions,zenoh,bench}

# Tighten existing Gaia prompt/session material without changing other data planes.
chown -R "${SENTINEL_USER}:${SENTINEL_USER}" /opt/sentinel/data/gaia-console
find /opt/sentinel/data/gaia-console -type d -exec chmod 0700 {} +
find /opt/sentinel/data/gaia-console -type f -exec chmod 0600 {} +
chown -R "${SENTINEL_USER}:${SENTINEL_USER}" /ram/sentinel

echo "[init-dirs] Done. Directories created for user ${SENTINEL_USER}."
