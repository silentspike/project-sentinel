#!/usr/bin/env bash
# Install an already downloaded native Codex CLI release as a root-owned pinned artifact.
set -euo pipefail

PINNED_VERSION="0.151.0"
PINNED_SHA256="9739cbc928b9c573be83256acd46668f5dd4f119d2d09e05246895ca2aaf0c9a"

if [ "$#" -ne 1 ]; then
  echo "Usage: $0 <native-binary>" >&2
  exit 1
fi
if [ "$(id -u)" -ne 0 ]; then
  echo "ERROR: install-native-codex.sh must run as root" >&2
  exit 1
fi

SOURCE="$1"
VERSION="${PINNED_VERSION}"
EXPECTED_SHA256="${PINNED_SHA256}"
DEST_DIR="/opt/sentinel/bin"
DEST="${DEST_DIR}/codex-${VERSION}"

[[ "${VERSION}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "ERROR: invalid version" >&2; exit 1; }
[[ "${EXPECTED_SHA256}" =~ ^[0-9a-f]{64}$ ]] || { echo "ERROR: invalid sha256" >&2; exit 1; }
[ -f "${SOURCE}" ] || { echo "ERROR: binary not found: ${SOURCE}" >&2; exit 1; }
file "${SOURCE}" | grep -q 'ELF.*executable' || { echo "ERROR: source is not a native ELF executable" >&2; exit 1; }
ACTUAL_SHA256="$(sha256sum "${SOURCE}" | awk '{print $1}')"
[ "${ACTUAL_SHA256}" = "${EXPECTED_SHA256}" ] || { echo "ERROR: sha256 mismatch" >&2; exit 1; }
"${SOURCE}" --version 2>&1 | grep -Fq "codex-cli ${VERSION}" || { echo "ERROR: version output mismatch" >&2; exit 1; }

install -d -o root -g root -m 0755 "${DEST_DIR}"
install -o root -g root -m 0755 "${SOURCE}" "${DEST}.tmp"
mv -f "${DEST}.tmp" "${DEST}"
ln -sfn "$(basename "${DEST}")" "${DEST_DIR}/codex"
chown -h root:root "${DEST_DIR}/codex"
echo "codex_version=${VERSION} sha256=${ACTUAL_SHA256} owner=root:root mode=0755"
