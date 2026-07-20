#!/usr/bin/env bash

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

mkdir -p "$repo_root/target"
scratch_dir="$(mktemp -d "$repo_root/target/compose-auth-smoke.XXXXXX")"
export TMPDIR="$scratch_dir"
credential_dir="$scratch_dir/credentials"
mkdir "$credential_dir"
export SENTINEL_DEMO_CREDENTIAL_DIR="$credential_dir"
umask 077
for role in agent-runtime platform evolution judge; do
    printf 'demo-%s-%s\n' "$role" "$(openssl rand -hex 32)" >"$credential_dir/caller-$role"
done

compose=(
    docker compose
    -f docker-compose.demo.yml
    -f deploy/docker/docker-compose.auth-smoke.yml
)

cleanup() {
    local status=$?
    if [ "$status" -ne 0 ]; then
        "${compose[@]}" ps >&2 || true
        "${compose[@]}" logs --no-color --tail 10 daemon >&2 || true
        "${compose[@]}" logs --no-color --tail 40 gateway >&2 || true
    fi
    "${compose[@]}" down --remove-orphans --volumes >/dev/null 2>&1 || true
    rm -rf -- "$scratch_dir"
    trap - EXIT
    exit "$status"
}
trap cleanup EXIT

"${compose[@]}" config --quiet
printf '[compose-auth-smoke] build image\n'
"${compose[@]}" build
# Local Docker Compose bind-mounts secret files and ignores uid/gid/mode. Use
# the freshly built image only to apply the same owner-only contract to the
# temporary host files before the non-root service containers start.
docker run --rm --user 0:0 \
    --entrypoint sh \
    --volume "$credential_dir:/credentials" \
    sentinel-demo:local \
    -eu -c 'chown 65001:65001 /credentials/caller-* && chmod 0400 /credentials/caller-*'
printf '[compose-auth-smoke] start daemon and gateway\n'
"${compose[@]}" up -d --wait daemon gateway

printf '[compose-auth-smoke] reject invalid credential\n'
# The quoted script expands inside the daemon container, not in this shell.
# shellcheck disable=SC2016
"${compose[@]}" exec -T daemon sh -eu -c '
    test "$CORTEX_GATEWAY_URL" = "http://gateway:8080"
    invalid_token="$(printf "%s-%s" "invalid" "smoke-token")"
    status="$(curl -sS -o /dev/null -w "%{http_code}" \
        --oauth2-bearer "$invalid_token" \
        -H "Content-Type: application/json" \
        -d "{\"messages\":[{\"role\":\"user\",\"content\":\"auth smoke\"}],\"metadata\":{\"agent_id\":\"1\",\"agent_role\":\"CEO\",\"hierarchy_tier\":\"1\",\"tick\":\"1\"}}" \
        "$CORTEX_GATEWAY_URL/internal/agent-runtime")"
    test "$status" = "401"
'

printf '[compose-auth-smoke] execute authenticated local-loop request\n'
# The quoted script expands inside the daemon container, not in this shell.
# shellcheck disable=SC2016
response="$("${compose[@]}" exec -T daemon sh -eu -c '
    token="$(cat /run/secrets/caller-agent-runtime)"
    curl -fsS \
        -H "Authorization: Bearer $token" \
        -H "X-Request-ID: compose-auth-smoke-001" \
        -H "Content-Type: application/json" \
        -d "{\"messages\":[{\"role\":\"user\",\"content\":\"auth smoke\"}],\"metadata\":{\"agent_id\":\"1\",\"agent_name\":\"AGENT-01\",\"agent_role\":\"CEO\",\"hierarchy_tier\":\"1\",\"tick\":\"1\"}}" \
        "$CORTEX_GATEWAY_URL/internal/agent-runtime"
')"

inspector="$("${compose[@]}" exec -T gateway \
    curl -fsS http://127.0.0.1:8081/control/traffic-responses)"

printf '[compose-auth-smoke] verify response contract and secret redaction\n'
python3 - "$response" "$inspector" <<'PY'
import json
import sys

response = json.loads(sys.argv[1])
entries = json.loads(sys.argv[2])

assert response["request_id"] == "compose-auth-smoke-001", response
assert response["effective_model"] == "local-loop-tier1", response
assert response["hierarchy_tier"] == 1, response

entry = next(item for item in entries if item["request_id"] == "compose-auth-smoke-001")
assert entry["caller_role"] == "agent_runtime", entry
assert entry["effective_model"] == "local-loop-tier1", entry
assert entry["hierarchy_tier"] == 1, entry
assert entry["cost_source"] == "non_provider_zero", entry
PY

# Compare the live inspector payload with all mounted credentials without
# emitting the credential values or a reusable fingerprint.
# shellcheck disable=SC2016
"${compose[@]}" exec -T gateway sh -eu -c '
    inspector="$(curl -fsS http://127.0.0.1:8081/control/traffic-responses)"
    for credential in /run/secrets/caller-*; do
        value="$(cat "$credential")"
        case "$inspector" in
            *"$value"*) exit 1 ;;
        esac
    done
'

printf '[compose-auth-smoke] PASS\n'
