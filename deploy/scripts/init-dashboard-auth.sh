#!/usr/bin/env bash
# Materialize independent dashboard-login and daemon-proxy credentials without exposing values.
set -euo pipefail

DEFAULT_ENV_FILE="/opt/sentinel/config/dashboard-backend.env"
DEFAULT_CREDENTIAL_FILE="/etc/sentinel/credentials/operator-api"
ENV_FILE="${SENTINEL_DASHBOARD_ENV_FILE:-${DEFAULT_ENV_FILE}}"
CREDENTIAL_FILE="${SENTINEL_OPERATOR_CREDENTIAL_FILE:-${DEFAULT_CREDENTIAL_FILE}}"
TEST_ROOT="${SENTINEL_AUTH_TEST_ROOT:-}"

fail() {
  printf 'ERROR: %s\n' "$1" >&2
  exit 1
}

require_absolute_plain_path() {
  case "$1" in
    /*) ;;
    *) fail "credential paths must be absolute" ;;
  esac
  case "/$1/" in
    */../*|*/./*) fail "credential paths must not contain dot components" ;;
  esac
}

reject_symlink_components() {
  local path="$1"
  local current=""
  local component
  local -a components
  IFS='/' read -r -a components <<< "${path#/}"
  for component in "${components[@]}"; do
    [ -n "${component}" ] || continue
    current="${current}/${component}"
    [ ! -L "${current}" ] || fail "managed credential path contains a symlink"
  done
}

require_absolute_plain_path "${ENV_FILE}"
require_absolute_plain_path "${CREDENTIAL_FILE}"
reject_symlink_components "${ENV_FILE}"
reject_symlink_components "${CREDENTIAL_FILE}"

if [ -n "${TEST_ROOT}" ]; then
  require_absolute_plain_path "${TEST_ROOT}"
  case "${ENV_FILE}" in "${TEST_ROOT}"/*) ;; *) fail "test env path escapes test root" ;; esac
  case "${CREDENTIAL_FILE}" in "${TEST_ROOT}"/*) ;; *) fail "test credential path escapes test root" ;; esac
  [ ! -L "${TEST_ROOT}" ] || fail "test root must not be a symlink"
  [ -d "${TEST_ROOT}" ] || fail "test root must exist"
  [ "$(stat -c '%a' -- "${TEST_ROOT}")" = "700" ] || fail "test root mode must be 0700"
  TARGET_USER="$(id -un)"
  TARGET_GROUP="$(id -gn)"
else
  [ "$(id -u)" -eq 0 ] || fail "init-dashboard-auth.sh must run as root"
  [ "${ENV_FILE}" = "${DEFAULT_ENV_FILE}" ] || fail "production env path override is forbidden"
  [ "${CREDENTIAL_FILE}" = "${DEFAULT_CREDENTIAL_FILE}" ] || fail "production credential path override is forbidden"
  TARGET_USER=root
  TARGET_GROUP=root
fi

command -v openssl >/dev/null || fail "openssl is required"
command -v stat >/dev/null || fail "stat is required"
command -v python3 >/dev/null || fail "python3 is required"

ENV_DIR="$(dirname -- "${ENV_FILE}")"
CREDENTIAL_DIR="$(dirname -- "${CREDENTIAL_FILE}")"
install -d -o "${TARGET_USER}" -g "${TARGET_GROUP}" -m 0755 "${ENV_DIR}"
install -d -o "${TARGET_USER}" -g "${TARGET_GROUP}" -m 0700 "${CREDENTIAL_DIR}"

for path in "${ENV_FILE}" "${CREDENTIAL_FILE}"; do
  [ ! -L "${path}" ] || fail "managed credential path must not be a symlink"
done
if [ -e "${CREDENTIAL_FILE}" ]; then
  [ -f "${CREDENTIAL_FILE}" ] || fail "operator credential must be a regular file"
  expected_uid="$(id -u "${TARGET_USER}")"
  expected_gid="$(id -g "${TARGET_GROUP}")"
  credential_metadata="$(stat -c '%u:%g:%a:%h:%s' -- "${CREDENTIAL_FILE}")"
  credential_identity="$(stat -c '%d:%i:%u:%g:%a:%h:%s:%y:%z' -- "${CREDENTIAL_FILE}")"
  case "${credential_metadata}" in
    "${expected_uid}:${expected_gid}:400:1:"*) ;;
    *) fail "existing operator credential metadata is invalid" ;;
  esac
  if ! python3 - "${CREDENTIAL_FILE}" <<'PY'
import pathlib
import sys
import unicodedata

data = pathlib.Path(sys.argv[1]).read_bytes()
if not 32 <= len(data) <= 4096:
    raise SystemExit(1)
try:
    value = data.decode("utf-8")
except UnicodeDecodeError:
    raise SystemExit(1)
if value.strip() != value or any(unicodedata.category(char).startswith("C") for char in value):
    raise SystemExit(1)
PY
  then
    fail "existing operator credential content is invalid"
  fi
  [ "$(stat -c '%d:%i:%u:%g:%a:%h:%s:%y:%z' -- "${CREDENTIAL_FILE}")" = "${credential_identity}" ] \
    || fail "existing operator credential identity changed"
fi

read_unique_env_value() {
  local key="$1"
  local count=0
  local value=""
  if [ -f "${ENV_FILE}" ]; then
    count="$(grep -Ec "^${key}=" "${ENV_FILE}" || true)"
    [ "${count}" -le 1 ] || fail "duplicate managed credential entry"
    if [ "${count}" -eq 1 ]; then
      value="$(grep -E "^${key}=" "${ENV_FILE}" | sed "s/^${key}=//")"
    fi
  fi
  printf '%s' "${value}"
}

validate_secret() {
  local value="$1"
  local length
  length="$(LC_ALL=C printf '%s' "${value}" | wc -c)"
  if [ "${length}" -lt 32 ] || [ "${length}" -gt 4096 ]; then
    fail "managed credential length is invalid"
  fi
  [ "${value}" = "${value#"${value%%[![:space:]]*}"}" ] || fail "managed credential has surrounding whitespace"
  [ "${value}" = "${value%"${value##*[![:space:]]}"}" ] || fail "managed credential has surrounding whitespace"
  if LC_ALL=C printf '%s' "${value}" | grep -q '[[:cntrl:]]'; then
    fail "managed credential contains control data"
  fi
}

dashboard_secret="$(read_unique_env_value SENTINEL_DASHBOARD_API_KEY)"
legacy_operator_secret="$(read_unique_env_value SENTINEL_OPERATOR_API_KEY)"
file_operator_secret=""
credential_exists=false
if [ -f "${CREDENTIAL_FILE}" ]; then
  credential_exists=true
  file_operator_secret="$(cat -- "${CREDENTIAL_FILE}")"
  [ "$(stat -c '%d:%i:%u:%g:%a:%h:%s:%y:%z' -- "${CREDENTIAL_FILE}")" = "${credential_identity}" ] \
    || fail "existing operator credential identity changed"
  validate_secret "${file_operator_secret}"
fi

if [ -n "${legacy_operator_secret}" ]; then
  validate_secret "${legacy_operator_secret}"
fi
if [ -n "${legacy_operator_secret}" ] && [ -n "${file_operator_secret}" ] \
  && [ "${legacy_operator_secret}" != "${file_operator_secret}" ]; then
  fail "legacy and file operator credentials conflict"
fi

operator_state=existing
operator_secret="${file_operator_secret:-${legacy_operator_secret}}"
if [ -z "${operator_secret}" ]; then
  operator_secret="$(openssl rand -hex 32)"
  operator_state=generated
elif [ -z "${file_operator_secret}" ]; then
  operator_state=migrated
fi
validate_secret "${operator_secret}"

dashboard_state=existing
if [ -z "${dashboard_secret}" ]; then
  dashboard_secret="$(openssl rand -hex 32)"
  dashboard_state=generated
fi
validate_secret "${dashboard_secret}"

credential_tmp=""
env_tmp="$(mktemp "${ENV_DIR}/.dashboard-backend.env.XXXXXX")"
cleanup() {
  [ -z "${credential_tmp}" ] || rm -f -- "${credential_tmp}"
  [ -z "${env_tmp}" ] || rm -f -- "${env_tmp}"
}
trap cleanup EXIT

chmod 0600 "${env_tmp}"
if [ "${credential_exists}" = false ]; then
  credential_tmp="$(mktemp "${CREDENTIAL_DIR}/.operator-api.XXXXXX")"
  chmod 0600 "${credential_tmp}"
  printf '%s' "${operator_secret}" > "${credential_tmp}"
fi
if [ -f "${ENV_FILE}" ]; then
  grep -Ev '^(SENTINEL_DASHBOARD_API_KEY|SENTINEL_OPERATOR_API_KEY)=' "${ENV_FILE}" > "${env_tmp}" || true
fi
printf 'SENTINEL_DASHBOARD_API_KEY=%s\n' "${dashboard_secret}" >> "${env_tmp}"

chown "${TARGET_USER}:${TARGET_GROUP}" "${env_tmp}"
chmod 0600 "${env_tmp}"
if [ "${credential_exists}" = false ]; then
  chown "${TARGET_USER}:${TARGET_GROUP}" "${credential_tmp}"
  chmod 0400 "${credential_tmp}"
  mv -f -- "${credential_tmp}" "${CREDENTIAL_FILE}"
  credential_tmp=""
fi
mv -f -- "${env_tmp}" "${ENV_FILE}"
env_tmp=""

printf 'dashboard_auth=%s operator_auth=%s env_permissions=0600 credential_permissions=0400\n' \
  "${dashboard_state}" "${operator_state}"
