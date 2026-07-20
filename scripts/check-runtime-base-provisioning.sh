#!/usr/bin/env bash
# Static contract gate for the common Sentinel runtime-base provisioner.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
PROVISIONER="${REPO_ROOT}/deploy/provision-runtime-base.sh"
CONTRACT="${REPO_ROOT}/deploy/runtime-base.env"
MANIFEST="${REPO_ROOT}/deploy/generate-manifest.sh"
UNIT="${REPO_ROOT}/deploy/systemd/sentinel-daemon.service"
SYSCTL="${REPO_ROOT}/deploy/vm-config/99-sentinel-bwrap.conf"

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

bash -n "${PROVISIONER}"

require_text "${CONTRACT}" 'SENTINEL_BUBBLEWRAP_VERSION=0.9.0-1ubuntu0.1'
require_text "${CONTRACT}" 'SENTINEL_BUBBLEWRAP_BINARY_SHA256=52231e1caf55bcbc667b269f49c63599a6f7db4767ae6a039580d0ff853db712'
require_text "${SYSCTL}" 'kernel.apparmor_restrict_unprivileged_userns = 0'
require_text "${SYSCTL}" 'kernel.unprivileged_userns_clone = 1'
require_text "${UNIT}" 'User=root'
require_text "${UNIT}" 'Group=root'
require_text "${UNIT}" 'EnvironmentFile=-/etc/sentinel/env'
require_text "${UNIT}" 'NoNewPrivileges=true'
require_text "${UNIT}" 'AmbientCapabilities=CAP_SYS_PTRACE'

require_once "${MANIFEST}" 'target/release/agent-runtime|/usr/bin/agent-runtime|binary'
require_once "${MANIFEST}" 'target/release/landlock-wrapper|/opt/sentinel/bin/landlock-wrapper|binary'
require_once "${MANIFEST}" 'deploy/runtime-base.env|/opt/sentinel/share/runtime-base.env|config'
require_once "${MANIFEST}" 'deploy/apt/sentinel-runtime.pref|/etc/apt/preferences.d/sentinel-runtime|config'
require_once "${MANIFEST}" 'deploy/vm-config/99-sentinel-bwrap.conf|/etc/sysctl.d/99-sentinel-bwrap.conf|config'

# These are deliberately literal fragments from the remote heredoc.
# shellcheck disable=SC2016
for text in \
  'require_inactive sentinel-daemon.service' \
  'require_inactive sentinel-projection.service' \
  'bubblewrap=${bwrap_version}' \
  '--no-new-privs' \
  '/sentinel/${probe_name}' \
  '[landlock-wrapper] Landlock enforced' \
  'protected config, data, credential, or machine identity bytes changed' \
  'Runtime base installed and functionally verified'; do
  require_text "${PROVISIONER}" "${text}"
done

if grep -Eq '(/opt/sentinel/config|/opt/sentinel/data|/etc/sentinel).*(cp|install)|(^|[[:space:]])(cp|install).*(/opt/sentinel/config|/opt/sentinel/data|/etc/sentinel)' "${PROVISIONER}"; then
  echo "FAIL: runtime-base provisioner must not install protected node state" >&2
  exit 1
fi
if grep -Eq 'systemctl[[:space:]]+(start|restart|enable)' "${PROVISIONER}"; then
  echo "FAIL: runtime-base provisioner must leave services stopped" >&2
  exit 1
fi

echo "Runtime-base provisioning contract: OK"
