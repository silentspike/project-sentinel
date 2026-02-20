#!/usr/bin/env bash
# deploy-preflight.sh — Verifies SHA-256 hash parity between release manifest and target VM.
# Hard-aborts deploy (exit 1) on any mismatch.
# Usage: bash deploy/deploy-preflight.sh <SSH_TARGET> [MANIFEST_PATH]
#   SSH_TARGET:    e.g. ubuntu@10.0.0.240
#   MANIFEST_PATH: optional, defaults to deploy/release-manifest.json
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

if [ $# -lt 1 ]; then
  echo "Usage: $0 <SSH_TARGET> [MANIFEST_PATH]" >&2
  echo "  SSH_TARGET:    e.g. ubuntu@10.0.0.240" >&2
  echo "  MANIFEST_PATH: path to manifest JSON (default: deploy/release-manifest.json)" >&2
  exit 1
fi

SSH_TARGET="$1"
MANIFEST="${2:-${REPO_ROOT}/deploy/release-manifest.json}"

if [ ! -f "${MANIFEST}" ]; then
  echo "ERROR: Manifest not found: ${MANIFEST}" >&2
  echo "Run deploy/generate-manifest.sh first." >&2
  exit 1
fi

# Parse manifest using python3 (standard in Ubuntu, no jq dependency)
ARTIFACT_JSON="$(python3 -c "
import json
with open('${MANIFEST}') as f:
    manifest = json.load(f)
for a in manifest['artifacts']:
    print(a['path'] + '\t' + a['sha256'])
")"

echo "Deploy Preflight: ${SSH_TARGET}"
echo "Manifest:         ${MANIFEST}"
echo ""

# Header
printf "%-60s %-14s %-14s %s\n" "Artifact" "Expected" "Actual" "Status"
printf "%s\n" "$(printf '%.0s-' {1..105})"

PASS=0
FAIL=0
MISSING=0

while IFS=$'\t' read -r path expected_hash; do
  # Get hash from remote; file may not exist yet
  actual_output="$(ssh "${SSH_TARGET}" "sha256sum '${path}' 2>/dev/null || echo MISSING" 2>/dev/null)"

  if echo "${actual_output}" | grep -q "^MISSING$"; then
    printf "%-60s %-14s %-14s %s\n" \
      "${path}" \
      "${expected_hash:0:12}..." \
      "NOT FOUND" \
      "MISSING"
    MISSING=$((MISSING + 1))
  else
    actual_hash="$(echo "${actual_output}" | awk '{print $1}')"
    exp_short="${expected_hash:0:12}..."
    act_short="${actual_hash:0:12}..."

    if [ "${expected_hash}" = "${actual_hash}" ]; then
      printf "%-60s %-14s %-14s %s\n" \
        "${path}" "${exp_short}" "${act_short}" "MATCH"
      PASS=$((PASS + 1))
    else
      printf "%-60s %-14s %-14s %s\n" \
        "${path}" "${exp_short}" "${act_short}" "MISMATCH" >&2
      FAIL=$((FAIL + 1))
    fi
  fi
done <<< "${ARTIFACT_JSON}"

echo ""
echo "Results: ${PASS} MATCH, ${FAIL} MISMATCH, ${MISSING} MISSING"

if [ "${FAIL}" -gt 0 ] || [ "${MISSING}" -gt 0 ]; then
  echo "" >&2
  echo "PREFLIGHT FAILED: ${FAIL} mismatch(es), ${MISSING} missing artifact(s)." >&2
  echo "Deploy aborted. Verify artifacts are deployed correctly before retrying." >&2
  exit 1
fi

echo "PREFLIGHT PASSED: All ${PASS} artifacts verified."
exit 0
