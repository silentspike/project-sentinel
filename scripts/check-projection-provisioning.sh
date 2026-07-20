#!/usr/bin/env bash
# Static deploy-contract gate for the sentinel-projection worker.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

MANIFEST_GENERATOR="${REPO_ROOT}/deploy/generate-manifest.sh"
PROVISIONER="${REPO_ROOT}/deploy/provision-projection.sh"
UNIT="${REPO_ROOT}/deploy/systemd/sentinel-projection.service"
TARGET_UNIT="${REPO_ROOT}/deploy/systemd/sentinel.target"
INIT_DIRS="${REPO_ROOT}/deploy/scripts/init-dirs.sh"

require_text() {
  local path="$1"
  local expected="$2"
  if ! grep -Fq -- "${expected}" "${path}"; then
    echo "FAIL: ${path} is missing required contract text: ${expected}" >&2
    exit 1
  fi
}

require_once() {
  local path="$1"
  local expected="$2"
  local count
  count="$(grep -Fc -- "${expected}" "${path}")"
  if [ "${count}" -ne 1 ]; then
    echo "FAIL: ${path} must contain exactly one contract entry: ${expected}" >&2
    exit 1
  fi
}

bash -n "${MANIFEST_GENERATOR}" "${PROVISIONER}" "${INIT_DIRS}"

require_once "${MANIFEST_GENERATOR}" \
  'target/release/sentinel-projection|/opt/sentinel/bin/sentinel-projection|binary'
require_once "${MANIFEST_GENERATOR}" \
  'deploy/systemd/sentinel-projection.service|/etc/systemd/system/sentinel-projection.service|systemd'

require_text "${UNIT}" 'User=root'
require_text "${UNIT}" 'Group=root'
require_text "${UNIT}" 'ExecStart=/opt/sentinel/bin/sentinel-projection'
require_text "${UNIT}" '--event-store /opt/sentinel/data/events.db'
require_text "${UNIT}" '--projection-db /opt/sentinel/data/projection.db'
require_text "${UNIT}" 'ReadWritePaths=/opt/sentinel/data'
require_text "${UNIT}" 'WantedBy=sentinel.target'
require_text "${TARGET_UNIT}" 'sentinel-projection.service'

require_text "${PROVISIONER}" 'target/release/sentinel-projection'
require_text "${PROVISIONER}" 'deploy/systemd/sentinel-projection.service'
require_text "${PROVISIONER}" 'deploy/systemd/sentinel.target'
require_text "${PROVISIONER}" 'deploy/scripts/init-dirs.sh'
require_text "${PROVISIONER}" 'readable append-only EventStore is required at /opt/sentinel/data/events.db'
require_text "${PROVISIONER}" 'Projection artifacts installed; services remain stopped'

if grep -Eq '(^|[[:space:]])(touch|truncate|sqlite3)[[:space:]].*projection\.db' "${PROVISIONER}"; then
  echo "FAIL: the provisioner must not synthesize projection.db" >&2
  exit 1
fi

if grep -Eq 'systemctl[[:space:]]+(start|restart|enable)' "${PROVISIONER}"; then
  echo "FAIL: the provisioner must leave services stopped" >&2
  exit 1
fi

init_dirs_flat="$(tr '\n' ' ' < "${INIT_DIRS}")"
if ! grep -Eq 'install -d -o "\$\{SENTINEL_USER\}" -g "\$\{SENTINEL_USER\}" -m 0750[[:space:]]+\\[[:space:]]+/opt/sentinel/data /opt/sentinel/logs' <<< "${init_dirs_flat}"; then
  echo "FAIL: init-dirs.sh must provision /opt/sentinel/data as 0750 for SENTINEL_USER" >&2
  exit 1
fi

echo "Projection provisioning contract: OK"
