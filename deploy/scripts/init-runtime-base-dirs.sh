#!/usr/bin/env bash
# Create only the common runtime-base directories. Node configuration, runtime
# data, credentials, and identity are separate overlays and are never touched.
set -euo pipefail
export LC_ALL=C

ROOT_PREFIX="${SENTINEL_ROOT_PREFIX:-}"
BASE_USER="${SENTINEL_BASE_USER:-root}"
BASE_GROUP="${SENTINEL_BASE_GROUP:-root}"
DATA_USER="${SENTINEL_DATA_USER:-ubuntu}"
DATA_GROUP="${SENTINEL_DATA_GROUP:-ubuntu}"

canonical_dir() {
  local path="$1"
  local owner="$2"
  local group="$3"
  local mode="$4"
  local target="${ROOT_PREFIX}${path}"
  local owner_uid
  local group_gid

  owner_uid="$(id -u "${owner}")"
  group_gid="$(getent group "${group}" | cut -d: -f3)"

  if [ -L "${target}" ] || { [ -e "${target}" ] && [ ! -d "${target}" ]; }; then
    echo "ERROR: runtime-base path is not a real directory: ${path}" >&2
    exit 1
  fi

  install -d -o "${owner}" -g "${group}" -m "${mode}" "${target}"
  if [ "$(stat -c '%u:%g:%a:%F' "${target}")" != "${owner_uid}:${group_gid}:${mode#0}:directory" ]; then
    echo "ERROR: runtime-base directory contract mismatch: ${path}" >&2
    exit 1
  fi
}

preserve_or_create_bind_root() {
  local path="$1"
  local target="${ROOT_PREFIX}${path}"

  if [ -L "${target}" ] || { [ -e "${target}" ] && [ ! -d "${target}" ]; }; then
    echo "ERROR: runtime-base bind root is not a real directory: ${path}" >&2
    exit 1
  fi
  if [ -d "${target}" ]; then
    return
  fi
  canonical_dir "${path}" "${BASE_USER}" "${BASE_GROUP}" 0755
}

# Root-owned executable, structural, filesystem, and read-only bind roots.
for path in \
  /opt/sentinel \
  /opt/sentinel/bin \
  /opt/sentinel/scripts \
  /opt/sentinel/share \
  /opt/sentinel/fs \
  /ram \
  /ram/agents; do
  canonical_dir "${path}" "${BASE_USER}" "${BASE_GROUP}" 0755
done

# Existing company trees are node-local data. Create only missing empty bind
# roots; never normalize metadata on a populated or pre-provisioned tree.
preserve_or_create_bind_root /work
preserve_or_create_bind_root /work/company

# Daemon runtime scratch is separate from protected persistent data.
for path in \
  /ram/sentinel \
  /ram/sentinel/ecs \
  /ram/sentinel/sessions \
  /ram/sentinel/zenoh \
  /ram/sentinel/bench; do
  canonical_dir "${path}" "${DATA_USER}" "${DATA_GROUP}" 0755
done

echo "[init-runtime-base-dirs] Canonical runtime-base directories verified."
