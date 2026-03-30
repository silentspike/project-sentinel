#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_DIR="$(mktemp -d)"
FAKE_PORT="${FAKE_ANTHROPIC_PORT:-19876}"
GATEWAY_PORT="${MITM_GATEWAY_PORT:-18080}"
CONTROL_PORT="${MITM_CONTROL_PORT:-18081}"
CLAUDE_PROMPT="${CLAUDE_PROMPT:-Antworte exakt mit HALLO.}"

cleanup() {
  local exit_code=$?
  if [[ -n "${GATEWAY_PID:-}" ]]; then
    kill "${GATEWAY_PID}" 2>/dev/null || true
    wait "${GATEWAY_PID}" 2>/dev/null || true
  fi
  if [[ -n "${FAKE_PID:-}" ]]; then
    kill "${FAKE_PID}" 2>/dev/null || true
    wait "${FAKE_PID}" 2>/dev/null || true
  fi
  if [[ "${KEEP_MITM_TMP:-0}" == "1" ]]; then
    echo "Temporary MITM smoke artifacts kept in ${TMP_DIR}" >&2
  else
    rm -rf "${TMP_DIR}"
  fi
  exit "${exit_code}"
}
trap cleanup EXIT

for bin in go python3 claude curl; do
  if ! command -v "${bin}" >/dev/null 2>&1; then
    echo "missing required binary: ${bin}" >&2
    exit 1
  fi
done

pushd "${ROOT}" >/dev/null

echo "[1/5] Building cortex-gateway"
go build -o "${TMP_DIR}/cortex-gateway" ./cmd/cortex-gateway

echo "[2/5] Starting fake Anthropic upstream on :${FAKE_PORT}"
FAKE_ANTHROPIC_PORT="${FAKE_PORT}" \
FAKE_ANTHROPIC_TEXT="HALLO." \
python3 -u "${ROOT}/test-fake-api.py" >"${TMP_DIR}/fake.log" 2>&1 &
FAKE_PID=$!

echo "[3/5] Starting local gateway on :${GATEWAY_PORT}"
CORTEX_PORT="${GATEWAY_PORT}" \
CORTEX_CONTROL_PORT="${CONTROL_PORT}" \
ANTHROPIC_BASE_URL="http://127.0.0.1:${FAKE_PORT}" \
"${TMP_DIR}/cortex-gateway" >"${TMP_DIR}/gateway.log" 2>&1 &
GATEWAY_PID=$!

for _ in {1..40}; do
  if curl -fsS "http://127.0.0.1:${GATEWAY_PORT}/health" >/dev/null 2>&1; then
    break
  fi
  sleep 0.5
done
curl -fsS "http://127.0.0.1:${GATEWAY_PORT}/health" >/dev/null

echo "[4/5] Running claude -p against gateway"
CLAUDE_OUTPUT="$(
  ANTHROPIC_BASE_URL="http://127.0.0.1:${GATEWAY_PORT}" \
  NO_PROXY="127.0.0.1,localhost" \
  claude -p "${CLAUDE_PROMPT}" --output-format json
)"
echo "${CLAUDE_OUTPUT}" >"${TMP_DIR}/claude.json"

echo "[5/5] Verifying MITM path"
grep -q "POST /v1/messages" "${TMP_DIR}/fake.log"
grep -Eq 'provider.?anthropic-direct|anthropic-direct' "${TMP_DIR}/gateway.log"
grep -q 'HALLO' "${TMP_DIR}/claude.json"

echo "MITM smoke test passed"
if [[ "${KEEP_MITM_TMP:-0}" == "1" ]]; then
  echo "Logs:"
  echo "  fake:    ${TMP_DIR}/fake.log"
  echo "  gateway: ${TMP_DIR}/gateway.log"
  echo "  claude:  ${TMP_DIR}/claude.json"
else
  echo "Set KEEP_MITM_TMP=1 to keep fake/gateway/claude logs."
fi

popd >/dev/null
