#!/usr/bin/env bash
# Materialize independent workflow principals without exposing credential values.
set -euo pipefail

DEFAULT_CREDENTIAL_DIR="/etc/sentinel/credentials"
CREDENTIAL_DIR="${SENTINEL_WORKFLOW_CREDENTIAL_DIR:-${DEFAULT_CREDENTIAL_DIR}}"
TEST_ROOT="${SENTINEL_WORKFLOW_AUTH_TEST_ROOT:-}"
NAMES=(
  workflow-customer
  workflow-sales
  workflow-project-manager
  workflow-technical-lead
  workflow-designer
  workflow-developer
  workflow-qa
  workflow-release-manager
)

fail() {
  printf 'ERROR: %s\n' "$1" >&2
  exit 1
}

case "${CREDENTIAL_DIR}" in /*) ;; *) fail "credential directory must be absolute" ;; esac
case "/${CREDENTIAL_DIR}/" in */../*|*/./*) fail "credential directory contains dot components" ;; esac

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

reject_symlink_components "${CREDENTIAL_DIR}"

if [ -n "${TEST_ROOT}" ]; then
  case "${TEST_ROOT}" in /*) ;; *) fail "test root must be absolute" ;; esac
  case "${CREDENTIAL_DIR}" in "${TEST_ROOT}"/*) ;; *) fail "credential directory escapes test root" ;; esac
  [ -d "${TEST_ROOT}" ] && [ ! -L "${TEST_ROOT}" ] || fail "test root is unsafe"
  [ "$(stat -c '%a' -- "${TEST_ROOT}")" = "700" ] || fail "test root mode must be 0700"
  TARGET_USER="$(id -un)"
  TARGET_GROUP="$(id -gn)"
else
  [ "$(id -u)" -eq 0 ] || fail "workflow credential initialization requires root"
  [ "${CREDENTIAL_DIR}" = "${DEFAULT_CREDENTIAL_DIR}" ] || fail "production directory override is forbidden"
  TARGET_USER=root
  TARGET_GROUP=root
fi

command -v openssl >/dev/null || fail "openssl is required"
install -d -o "${TARGET_USER}" -g "${TARGET_GROUP}" -m 0700 "${CREDENTIAL_DIR}"
[ ! -L "${CREDENTIAL_DIR}" ] || fail "credential directory must not be a symlink"

generated=0
existing=0
for name in "${NAMES[@]}"; do
  path="${CREDENTIAL_DIR}/${name}"
  [ ! -L "${path}" ] || fail "workflow credential must not be a symlink"
  if [ -e "${path}" ]; then
    [ -f "${path}" ] || fail "workflow credential must be a regular file"
    metadata="$(stat -c '%u:%g:%a:%h:%s' -- "${path}")"
    expected_uid="$(id -u "${TARGET_USER}")"
    expected_gid="$(id -g "${TARGET_GROUP}")"
    case "${metadata}" in "${expected_uid}:${expected_gid}:400:1:"*) ;; *) fail "workflow credential metadata is invalid" ;; esac
    size="$(stat -c '%s' -- "${path}")"
    [ "${size}" -eq 64 ] || fail "workflow credential size is invalid"
    LC_ALL=C grep -Eq '^[0-9a-f]{64}$' "${path}" || fail "workflow credential content is invalid"
    existing=$((existing + 1))
    continue
  fi
  temporary="$(mktemp "${CREDENTIAL_DIR}/.${name}.XXXXXX")"
  trap 'rm -f -- "${temporary:-}"' EXIT
  chmod 0600 "${temporary}"
  openssl rand -hex 32 >"${temporary}"
  # Credentials are read as exact UTF-8 strings; omit the openssl newline.
  truncate -s -1 "${temporary}"
  chown "${TARGET_USER}:${TARGET_GROUP}" "${temporary}"
  chmod 0400 "${temporary}"
  mv -f -- "${temporary}" "${path}"
  temporary=""
  generated=$((generated + 1))
done

printf 'workflow_credentials=%d existing=%d permissions=0400\n' "${generated}" "${existing}"
