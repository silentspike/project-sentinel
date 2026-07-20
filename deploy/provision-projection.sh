#!/usr/bin/env bash
# Install the canonical projection artifacts on one stopped Sentinel node.
# The service is deliberately not started here: projection.db must be created
# by the normal worker startup from the existing append-only EventStore.
set -euo pipefail

if [ "$#" -ne 1 ]; then
  echo "Usage: $0 <ssh-target>" >&2
  exit 2
fi

SSH_TARGET="$1"
SENTINEL_USER="${SENTINEL_USER:-ubuntu}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

if [[ ! "${SENTINEL_USER}" =~ ^[a-z_][a-z0-9_-]*$ ]]; then
  echo "ERROR: SENTINEL_USER is not a valid service account name" >&2
  exit 2
fi

BINARY="${REPO_ROOT}/target/release/sentinel-projection"
UNIT="${REPO_ROOT}/deploy/systemd/sentinel-projection.service"
TARGET_UNIT="${REPO_ROOT}/deploy/systemd/sentinel.target"
INIT_DIRS="${REPO_ROOT}/deploy/scripts/init-dirs.sh"

for source in "${BINARY}" "${UNIT}" "${TARGET_UNIT}" "${INIT_DIRS}"; do
  if [ ! -f "${source}" ]; then
    echo "ERROR: required projection artifact is missing: ${source}" >&2
    exit 1
  fi
done

if [ ! -x "${BINARY}" ]; then
  echo "ERROR: projection binary is not executable: ${BINARY}" >&2
  exit 1
fi

BINARY_SHA="$(sha256sum "${BINARY}" | awk '{print $1}')"
UNIT_SHA="$(sha256sum "${UNIT}" | awk '{print $1}')"
TARGET_SHA="$(sha256sum "${TARGET_UNIT}" | awk '{print $1}')"
INIT_DIRS_SHA="$(sha256sum "${INIT_DIRS}" | awk '{print $1}')"

REMOTE_DIR="$(ssh -o BatchMode=yes -o ConnectTimeout=5 "${SSH_TARGET}" \
  'mktemp -d /tmp/sentinel-projection-provision.XXXXXX')"

case "${REMOTE_DIR}" in
  /tmp/sentinel-projection-provision.*) ;;
  *)
    echo "ERROR: remote staging path is outside the expected prefix" >&2
    exit 1
    ;;
esac

cleanup() {
  ssh -n -o BatchMode=yes -o ConnectTimeout=5 "${SSH_TARGET}" \
    "rm -rf -- '${REMOTE_DIR}'" >/dev/null 2>&1 || true
}
trap cleanup EXIT HUP INT TERM

scp -q \
  "${BINARY}" \
  "${UNIT}" \
  "${TARGET_UNIT}" \
  "${INIT_DIRS}" \
  "${SSH_TARGET}:${REMOTE_DIR}/"

ssh -o BatchMode=yes -o ConnectTimeout=5 "${SSH_TARGET}" bash -s -- \
  "${REMOTE_DIR}" \
  "${SENTINEL_USER}" \
  "${BINARY_SHA}" \
  "${UNIT_SHA}" \
  "${TARGET_SHA}" \
  "${INIT_DIRS_SHA}" <<'REMOTE'
set -euo pipefail

staging_dir="$1"
sentinel_user="$2"
binary_sha="$3"
unit_sha="$4"
target_sha="$5"
init_dirs_sha="$6"

require_inactive() {
  local unit="$1"
  local state
  state="$(systemctl is-active "${unit}" 2>/dev/null || true)"
  if [ "${state}" != "inactive" ] && [ "${state}" != "unknown" ]; then
    echo "ERROR: ${unit} must be stopped before projection provisioning (state=${state})" >&2
    exit 1
  fi
}

verify_hash() {
  local expected="$1"
  local path="$2"
  local actual
  actual="$(sha256sum "${path}" | awk '{print $1}')"
  if [ "${actual}" != "${expected}" ]; then
    echo "ERROR: SHA-256 mismatch for ${path}" >&2
    exit 1
  fi
}

require_inactive sentinel-daemon.service
require_inactive sentinel-projection.service
getent passwd "${sentinel_user}" >/dev/null
sudo -n true

if [ ! -f /opt/sentinel/data/events.db ] || [ ! -r /opt/sentinel/data/events.db ]; then
  echo "ERROR: readable append-only EventStore is required at /opt/sentinel/data/events.db" >&2
  exit 1
fi

verify_hash "${binary_sha}" "${staging_dir}/sentinel-projection"
verify_hash "${unit_sha}" "${staging_dir}/sentinel-projection.service"
verify_hash "${target_sha}" "${staging_dir}/sentinel.target"
verify_hash "${init_dirs_sha}" "${staging_dir}/init-dirs.sh"

sudo install -d -o root -g root -m 0755 /opt/sentinel/bin /opt/sentinel/scripts
sudo install -o root -g root -m 0755 \
  "${staging_dir}/sentinel-projection" /opt/sentinel/bin/sentinel-projection
sudo install -o root -g root -m 0644 \
  "${staging_dir}/sentinel-projection.service" /etc/systemd/system/sentinel-projection.service
sudo install -o root -g root -m 0644 \
  "${staging_dir}/sentinel.target" /etc/systemd/system/sentinel.target
sudo install -o root -g root -m 0755 \
  "${staging_dir}/init-dirs.sh" /opt/sentinel/scripts/init-dirs.sh
sudo env SENTINEL_USER="${sentinel_user}" bash /opt/sentinel/scripts/init-dirs.sh
sudo systemctl daemon-reload

require_inactive sentinel-daemon.service
require_inactive sentinel-projection.service
verify_hash "${binary_sha}" /opt/sentinel/bin/sentinel-projection
verify_hash "${unit_sha}" /etc/systemd/system/sentinel-projection.service
verify_hash "${target_sha}" /etc/systemd/system/sentinel.target
verify_hash "${init_dirs_sha}" /opt/sentinel/scripts/init-dirs.sh

if [ -e /opt/sentinel/data/projection.db ]; then
  echo "INFO: existing projection.db was preserved; provisioning did not create or replace it"
else
  echo "INFO: projection.db is absent and will be created by sentinel-projection from events.db"
fi

echo "Projection artifacts installed; services remain stopped"
REMOTE
