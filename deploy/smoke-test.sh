#!/usr/bin/env bash
# smoke-test.sh — Post-deploy smoke test: verifies service health within timeout.
# Usage: bash deploy/smoke-test.sh <SSH_TARGET> [TIMEOUT_SEC]
#   SSH_TARGET:  e.g. ubuntu@192.0.2.240
#   TIMEOUT_SEC: max seconds to wait for healthy services (default: 30)
set -uo pipefail

if [ $# -lt 1 ]; then
  echo "Usage: $0 <SSH_TARGET> [TIMEOUT_SEC]" >&2
  exit 1
fi

SSH_TARGET="$1"
TIMEOUT="${2:-30}"

echo "Smoke Test: ${SSH_TARGET} (timeout: ${TIMEOUT}s)"
echo ""

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Single SSH call with base64-encoded python script (avoids shell escaping issues)
REMOTE_SCRIPT="$(base64 -w0 "${SCRIPT_DIR}/smoke-test-remote.py")"

START=$(date +%s)

ssh -n -o ConnectTimeout=5 "${SSH_TARGET}" "echo '${REMOTE_SCRIPT}' | base64 -d | python3 - ${TIMEOUT}"
EXIT_CODE=$?

ELAPSED=$(( $(date +%s) - START ))
echo ""
echo "(Total wall time including SSH: ${ELAPSED}s)"

exit ${EXIT_CODE}
