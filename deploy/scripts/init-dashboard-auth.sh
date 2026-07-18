#!/usr/bin/env bash
# Create the required dashboard operator credential without exposing it in output.
set -euo pipefail

ENV_FILE="${SENTINEL_DASHBOARD_ENV_FILE:-/opt/sentinel/config/dashboard-backend.env}"
ENV_DIR="$(dirname "${ENV_FILE}")"

if [ "$(id -u)" -ne 0 ]; then
  echo "ERROR: init-dashboard-auth.sh must run as root" >&2
  exit 1
fi
command -v openssl >/dev/null || { echo "ERROR: openssl is required" >&2; exit 1; }

install -d -o root -g root -m 0755 "${ENV_DIR}"
if [ -f "${ENV_FILE}" ] && grep -Eq '^SENTINEL_DASHBOARD_API_KEY=.+$' "${ENV_FILE}"; then
  chown root:root "${ENV_FILE}"
  chmod 0600 "${ENV_FILE}"
  echo "dashboard_auth=existing permissions=0600 owner=root:root"
  exit 0
fi

secret="$(openssl rand -hex 32)"
tmp="$(mktemp "${ENV_DIR}/.dashboard-backend.env.XXXXXX")"
trap 'rm -f "${tmp}"' EXIT
chmod 0600 "${tmp}"
if [ -f "${ENV_FILE}" ]; then
  grep -Ev '^SENTINEL_DASHBOARD_API_KEY=' "${ENV_FILE}" > "${tmp}" || true
fi
printf 'SENTINEL_DASHBOARD_API_KEY=%s\n' "${secret}" >> "${tmp}"
install -o root -g root -m 0600 "${tmp}" "${ENV_FILE}"
echo "dashboard_auth=generated permissions=0600 owner=root:root"
